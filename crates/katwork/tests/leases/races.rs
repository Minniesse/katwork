use super::*;

async fn waiting(pool: &PgPool, query: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1)")
            .bind(format!("{query}%")).fetch_one(pool).await.unwrap();
        if waiting {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "statement did not wait: {query}"
        );
        tokio::task::yield_now().await;
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn two_workers_claim_each_task_once(pool: PgPool) {
    setup(&pool).await;
    setup(&pool).await;
    let core = core(&pool);
    let barrier = Barrier::new(2);
    let (a, b) = tokio::join!(
        async {
            barrier.wait().await;
            claim(&core, "a").await.unwrap()
        },
        async {
            barrier.wait().await;
            claim(&core, "b").await.unwrap()
        }
    );
    assert_ne!(a.task_id, b.task_id);
    assert!(claim(&core, "c").await.is_none());
}
#[sqlx::test(migrations = "./migrations")]
async fn reclaimed_epoch_rejects_late_completion(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let old = start_run(&core, &a).await;
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    let new = start_run(&core, &b).await;
    let before = events(&pool, a.thread_id).await;
    assert!(matches!(
        lease::complete(&core, a.task_id, "a", a.lease_epoch, answered(old)).await,
        Err(KatError::LeaseLost)
    ));
    assert!(matches!(
        lease::heartbeat(&pool, a.task_id, "a", a.lease_epoch, TTL).await,
        Err(KatError::LeaseLost)
    ));
    assert_eq!(before, events(&pool, a.thread_id).await);
    lease::complete(&core, b.task_id, "b", b.lease_epoch, answered(new))
        .await
        .unwrap();
}
#[sqlx::test(migrations = "./migrations")]
async fn double_reclaim_closes_one_run_and_grants_one_epoch(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let old = start_run(&core, &a).await;
    tool(&core, &a, old, SideEffect::Read).await;
    expire(&pool, a.task_id).await;
    let barrier = Barrier::new(2);
    let (b, c) = tokio::join!(
        async {
            barrier.wait().await;
            claim(&core, "b").await
        },
        async {
            barrier.wait().await;
            claim(&core, "c").await
        }
    );
    assert_eq!(usize::from(b.is_some()) + usize::from(c.is_some()), 1);
    let current = b.or(c).unwrap();
    assert_eq!(current.lease_epoch.0, a.lease_epoch.0 + 1);
    assert_eq!(current.attempts, 2);
    assert_eq!(terminals(&pool, old).await, ["RUN_ERROR"]);
}
#[sqlx::test(migrations = "./migrations")]
async fn heartbeat_rechecks_expiry_after_waiting_for_lease_row(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let c = claim(&core, "a").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT task_id FROM task_leases WHERE task_id=$1 FOR NO KEY UPDATE")
        .bind(c.task_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let other = pool.clone();
    let task = c.task_id;
    let epoch = c.lease_epoch;
    let renewal =
        tokio::spawn(async move { lease::heartbeat(&other, task, "a", epoch, TTL).await });
    waiting(&pool, "SELECT task_id FROM task_leases").await;
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE task_id=$1",
    )
    .bind(task)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(renewal.await.unwrap(), Err(KatError::LeaseLost)));
}
#[sqlx::test(migrations = "./migrations")]
async fn complete_rechecks_expiry_after_thread_lock_wait(pool: PgPool) {
    setup(&pool).await;
    let mut core = core(&pool);
    Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(3);
    let core = Arc::new(core);
    let c = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &c).await;
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()+interval '1 second' WHERE task_id=$1",
    )
    .bind(c.task_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(c.thread_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let other = core.clone();
    let task = c.task_id;
    let epoch = c.lease_epoch;
    let completion =
        tokio::spawn(async move { lease::complete(&other, task, "a", epoch, answered(run)).await });
    waiting(&pool, "SELECT id FROM threads WHERE workspace_id").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let expired: bool = sqlx::query_scalar(
            "SELECT expires_at<=clock_timestamp() FROM task_leases WHERE task_id=$1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        if expired {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    tx.commit().await.unwrap();
    assert!(matches!(
        completion.await.unwrap(),
        Err(KatError::LeaseLost)
    ));
    assert_eq!(outcome(&pool, c.job_id).await, (None, None));
    assert!(terminals(&pool, run).await.is_empty());
}
#[sqlx::test(migrations = "./migrations")]
async fn wrong_owner_cannot_renew_complete_or_abandon(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let c = claim(&core, "owner").await.unwrap();
    let run = start_run(&core, &c).await;
    let before = events(&pool, c.thread_id).await;
    assert!(matches!(
        lease::heartbeat(&pool, c.task_id, "other", c.lease_epoch, TTL).await,
        Err(KatError::LeaseLost)
    ));
    assert!(matches!(
        lease::complete(&core, c.task_id, "other", c.lease_epoch, answered(run)).await,
        Err(KatError::LeaseLost)
    ));
    assert!(matches!(
        lease::abandon(&core, c.task_id, "other", c.lease_epoch).await,
        Err(KatError::LeaseLost)
    ));
    assert_eq!(events(&pool, c.thread_id).await, before);
    assert_eq!(task_state(&pool, c.task_id).await, "leased");
    lease::complete(&core, c.task_id, "owner", c.lease_epoch, answered(run))
        .await
        .unwrap();
}
#[sqlx::test(migrations = "./migrations")]
async fn claim_rejects_invalid_owner_and_ttl_without_writes(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    for (owner, ttl) in [
        ("", TTL),
        ("bad\0owner", TTL),
        ("a", Duration::ZERO),
        ("a", Duration::MAX),
    ] {
        assert!(matches!(
            lease::claim(&core, owner, ttl, &Limits::default()).await,
            Err(KatError::RequestInvalid)
        ));
    }
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "queued");
    assert!(events(&pool, ThreadId(f.thread)).await.is_empty());
}
#[sqlx::test(migrations = "./migrations")]
async fn policy_version_changed_while_queued_finishes_without_run(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    kat::update(
        &pool,
        &tenant(&pool, &f).await,
        KatId(f.kat),
        kat::KatPatch {
            tool_allowlist: Some(vec!["read_thread".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(claim(&core, "a").await.is_none());
    assert_eq!(
        outcome(&pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("policy_changed".into()))
    );
    assert!(
        events(&pool, ThreadId(f.thread))
            .await
            .iter()
            .all(|e| e["type"] != "RUN_STARTED")
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn job_deadline_includes_time_waiting_for_first_claim(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '11 minutes' WHERE id=$1")
        .bind(f.job)
        .execute(&pool)
        .await
        .unwrap();
    assert!(claim(&core, "a").await.is_none());
    assert_eq!(
        outcome(&pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("time_limit".into()))
    );
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "failed");
}

#[sqlx::test(migrations = "./migrations")]
async fn held_busy_queue_does_not_starve_another_workspace(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let active = claim(&core, "active").await.unwrap();
    let oldest = enqueue(&pool, &f, KatId(f.kat)).await;
    for _ in 1..16 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    assert!(claim(&core, "holds").await.is_none());
    let other = setup(&pool).await;
    assert_eq!(
        claim(&core, "other").await.unwrap().task_id,
        TaskId(other.task)
    );
    finish(&core, &active, "active").await;
    assert_eq!(claim(&core, "available").await.unwrap().task_id, oldest);
}
#[sqlx::test(migrations = "./migrations")]
async fn held_paused_workspace_does_not_starve_another_workspace(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    paused(&pool, &f, true).await;
    for _ in 1..16 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    assert!(claim(&core, "holds").await.is_none());
    let other = setup(&pool).await;
    assert_eq!(
        claim(&core, "other").await.unwrap().task_id,
        TaskId(other.task)
    );
    assert_eq!(
        events(&pool, ThreadId(f.thread))
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.job_status")
            .count(),
        16
    );
    paused(&pool, &f, false).await;
    assert_eq!(
        claim(&core, "available").await.unwrap().task_id,
        TaskId(f.task)
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn advisory_hold_filter_preserves_deadlines_and_eligibility_stops(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let active = claim(&core, "active").await.unwrap();
    let waiting = enqueue(&pool, &f, KatId(f.kat)).await;
    assert!(claim(&core, "holds").await.is_none());
    kat::update(
        &pool,
        &tenant(&pool, &f).await,
        KatId(f.kat),
        kat::KatPatch {
            status: Some(KatStatus::Paused),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(claim(&core, "stops").await.is_none());
    assert_eq!(task_state(&pool, waiting).await, "failed");
    let fresh = setup(&pool).await;
    paused(&pool, &fresh, true).await;
    assert!(claim(&core, "holds").await.is_none());
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '11 minutes' WHERE id=$1")
        .bind(fresh.job)
        .execute(&pool)
        .await
        .unwrap();
    assert!(claim(&core, "deadline").await.is_none());
    assert_eq!(task_state(&pool, TaskId(fresh.task)).await, "failed");
    assert_eq!(task_state(&pool, active.task_id).await, "leased");
}

#[sqlx::test(migrations = "./migrations")]
async fn heartbeat_cannot_revive_lease_that_expires_during_policy_read(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let c = claim(&core, "a").await.unwrap();
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()+interval '1 second' WHERE task_id=$1",
    )
    .bind(c.task_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    // A concurrent schema operation can block a plain SELECT after both row locks were acquired.
    sqlx::query("LOCK TABLE workspaces IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    let other = pool.clone();
    let task = c.task_id;
    let epoch = c.lease_epoch;
    let renewal =
        tokio::spawn(async move { lease::heartbeat(&other, task, "a", epoch, TTL).await });
    waiting(&pool, "SELECT w.paused").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let expired: bool = sqlx::query_scalar(
            "SELECT expires_at<=clock_timestamp() FROM task_leases WHERE task_id=$1",
        )
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
        if expired {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    tx.commit().await.unwrap();
    assert!(matches!(renewal.await.unwrap(), Err(KatError::LeaseLost)));
}
