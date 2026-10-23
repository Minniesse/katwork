#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;

pub(super) async fn waiting(pool: &PgPool, query: &str) {
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
        lease::heartbeat(
            &pool,
            a.task_id,
            "a",
            a.lease_epoch,
            TTL,
            std::time::Duration::from_secs(5)
        )
        .await,
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
    let renewal = tokio::spawn(async move {
        lease::heartbeat(
            &other,
            task,
            "a",
            epoch,
            TTL,
            std::time::Duration::from_secs(5),
        )
        .await
    });
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
        lease::heartbeat(
            &pool,
            c.task_id,
            "other",
            c.lease_epoch,
            TTL,
            std::time::Duration::from_secs(5)
        )
        .await,
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
        std::time::Duration::from_secs(5),
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
    for _ in 1..300 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    for _ in 0..2 {
        assert!(claim(&core, "holds").await.is_none());
    }
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
    for _ in 1..300 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    for _ in 0..2 {
        assert!(claim(&core, "holds").await.is_none());
    }
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
        300
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
        std::time::Duration::from_secs(5),
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
    let renewal = tokio::spawn(async move {
        lease::heartbeat(
            &other,
            task,
            "a",
            epoch,
            TTL,
            std::time::Duration::from_secs(5),
        )
        .await
    });
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

#[sqlx::test(migrations = "./migrations")]
async fn removed_requester_keeps_queued_and_running_jobs_recoverable(pool: PgPool) {
    for stage in ["queued", "complete", "reclaim"] {
        let f = setup(&pool).await;
        let core = core(&pool);
        let active = if stage == "queued" {
            None
        } else {
            Some(claim(&core, "a").await.unwrap())
        };
        let run = match &active {
            Some(c) => Some(start_run(&core, c).await),
            None => None,
        };
        let other = setup(&pool).await;
        sqlx::query("DELETE FROM memberships WHERE workspace_id=$1 AND user_id=$2")
            .bind(f.workspace)
            .bind(f.user)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace)).await,
            Err(KatError::NotFound)
        ));
        match stage {
            "complete" => {
                let c = active.as_ref().unwrap();
                lease::complete(&core, c.task_id, "a", c.lease_epoch, answered(run.unwrap()))
                    .await
                    .unwrap();
            }
            "reclaim" => {
                let c = active.as_ref().unwrap();
                lease::abandon(&core, c.task_id, "a", c.lease_epoch)
                    .await
                    .unwrap();
                let recovered = claim(&core, "b").await.unwrap();
                assert_eq!(recovered.task_id, c.task_id);
                assert_eq!(terminals(&pool, run.unwrap()).await, ["RUN_ERROR"]);
                finish(&core, &recovered, "b").await;
            }
            _ => {
                let c = claim(&core, "b").await.unwrap();
                assert_eq!(c.task_id, TaskId(f.task));
                finish(&core, &c, "b").await;
            }
        }
        assert_eq!(
            outcome(&pool, JobId(f.job)).await.0.as_deref(),
            Some("answered")
        );
        let next = claim(&core, "other").await.unwrap();
        assert_eq!(next.task_id, TaskId(other.task));
        finish(&core, &next, "other").await;
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn candidate_error_does_not_abort_other_workspaces(pool: PgPool) {
    let bad = setup(&pool).await;
    let other = setup(&pool).await;
    let core = core(&pool);
    // A malformed historical task must end without blocking unrelated jobs.
    sqlx::query("UPDATE tasks SET input='{}' WHERE id=$1")
        .bind(bad.task)
        .execute(&pool)
        .await
        .unwrap();
    let next = claim(&core, "other").await.unwrap();
    assert_eq!(next.task_id, TaskId(other.task));
    assert_eq!(
        outcome(&pool, JobId(bad.job)).await,
        (Some("error".into()), Some("internal_error".into()))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn held_task_terminal_matrix(pool: PgPool) {
    for condition in [
        "task_deadline",
        "job_deadline",
        "retry_cap",
        "policy_change",
        "revoked_grant",
    ] {
        let f = setup(&pool).await;
        let core = core(&pool);
        let active = claim(&core, "active").await.unwrap();
        let waiting = enqueue(&pool, &f, KatId(f.kat)).await;
        assert!(claim(&core, "holds").await.is_none());
        let job: JobId = sqlx::query_scalar("SELECT job_id FROM tasks WHERE id=$1")
            .bind(waiting)
            .fetch_one(&pool)
            .await
            .unwrap();
        let expected = match condition {
            "task_deadline" => {
                sqlx::query("UPDATE tasks SET first_claimed_at=clock_timestamp()-interval '6 minutes' WHERE id=$1").bind(waiting).execute(&pool).await.unwrap();
                "time_limit"
            }
            "job_deadline" => {
                sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '11 minutes' WHERE id=$1").bind(job).execute(&pool).await.unwrap();
                "time_limit"
            }
            "retry_cap" => {
                sqlx::query("UPDATE tasks SET attempts=max_attempts WHERE id=$1")
                    .bind(waiting)
                    .execute(&pool)
                    .await
                    .unwrap();
                "retries_exhausted"
            }
            "policy_change" => {
                kat::update(
                    &pool,
                    &tenant(&pool, &f).await,
                    KatId(f.kat),
                    kat::KatPatch {
                        tool_allowlist: Some(vec!["read_thread".into()]),
                        ..Default::default()
                    },
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
                "policy_changed"
            }
            "revoked_grant" => {
                let tenant = tenant(&pool, &f).await;
                let other = tenancy::space::create(
                    &pool,
                    &tenant,
                    "Other",
                    SpacePolicy {
                        allow_external: false,
                    },
                )
                .await
                .unwrap();
                kat::grant_space(
                    &pool,
                    &tenant,
                    KatId(f.kat),
                    other,
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
                sqlx::query("UPDATE kats SET default_space_id=$2 WHERE id=$1")
                    .bind(f.kat)
                    .bind(other)
                    .execute(&pool)
                    .await
                    .unwrap();
                kat::revoke_space(
                    &pool,
                    &tenant,
                    KatId(f.kat),
                    SpaceId(f.space),
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
                // Isolate the grant predicate from the separately tested policy-version predicate.
                sqlx::query("UPDATE tasks t SET kat_policy_version=k.policy_version FROM kats k WHERE t.id=$1 AND k.id=t.kat_id AND k.workspace_id=t.workspace_id")
                    .bind(waiting).execute(&pool).await.unwrap();
                "policy_changed"
            }
            _ => unreachable!(),
        };
        assert!(claim(&core, "terminal").await.is_none(), "{condition}");
        assert_eq!(task_state(&pool, waiting).await, "failed", "{condition}");
        assert_eq!(
            outcome(&pool, job).await.1.as_deref(),
            Some(expected),
            "{condition}"
        );
        assert_eq!(task_state(&pool, active.task_id).await, "leased");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn other_expired_lease_does_not_consume_capacity(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let expired = claim(&core, "first").await.unwrap();
    let next = enqueue(&pool, &f, KatId(f.kat)).await;
    expire(&pool, expired.task_id).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE id=$1 FOR NO KEY UPDATE")
        .bind(expired.task_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(claim(&core, "second").await.unwrap().task_id, next);
    tx.rollback().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn distinct_threads_serialize_at_kat_capacity(pool: PgPool) {
    for capacity in [1, 3] {
        let mut f = setup(&pool).await;
        sqlx::query("UPDATE kats SET max_concurrency=$2 WHERE id=$1")
            .bind(f.kat)
            .bind(capacity)
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..capacity {
            f.thread = uuid::Uuid::now_v7();
            sqlx::query("INSERT INTO threads(id,workspace_id,space_id,team_id,title) VALUES($1,$2,$3,$4,'Other thread')")
                .bind(f.thread).bind(f.workspace).bind(f.space).bind(f.team).execute(&pool).await.unwrap();
            enqueue(&pool, &f, KatId(f.kat)).await;
        }
        let mut core = core(&pool);
        Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(30);
        let core = Arc::new(core);
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
            .bind(f.kat)
            .execute(&mut *gate)
            .await
            .unwrap();
        let mut claims = tokio::task::JoinSet::new();
        for i in 0..=capacity {
            let core = core.clone();
            claims.spawn(async move { claim(&core, &format!("worker-{i}")).await });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            // Statistics snapshots are cached for the observer transaction.
            sqlx::query("SELECT pg_stat_clear_snapshot()")
                .execute(&mut *gate)
                .await
                .unwrap();
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id FROM kats WHERE workspace_id%'")
                .fetch_one(&mut *gate).await.unwrap();
            if waiting == i64::from(capacity + 1) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "claimers did not serialize at Kat lock"
            );
            tokio::task::yield_now().await;
        }
        gate.rollback().await.unwrap();
        let mut granted = 0;
        while let Some(result) = claims.join_next().await {
            granted += usize::from(result.unwrap().is_some());
        }
        assert_eq!(granted, capacity as usize);
        sqlx::query("UPDATE jobs SET status='cancelled' WHERE workspace_id=$1")
            .bind(f.workspace)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn new_holds_do_not_hide_ready_work_in_same_poll(pool: PgPool) {
    for paused_workspace in [false, true] {
        let f = setup(&pool).await;
        let core = core(&pool);
        if paused_workspace {
            paused(&pool, &f, true).await;
        } else {
            claim(&core, "busy").await.unwrap();
        }
        for _ in 0..32 {
            enqueue(&pool, &f, KatId(f.kat)).await;
        }
        let other = setup(&pool).await;
        let next = claim(&core, "other").await.unwrap();
        assert_eq!(next.task_id, TaskId(other.task));
        finish(&core, &next, "other").await;
        sqlx::query("UPDATE jobs SET status='cancelled' WHERE workspace_id=$1")
            .bind(f.workspace)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn repeated_hold_after_snapshot_change_emits_no_duplicate_notice(pool: PgPool) {
    let f = setup(&pool).await;
    let mut core = core(&pool);
    Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(30);
    let core = Arc::new(core);
    claim(&core, "active").await.unwrap();
    enqueue(&pool, &f, KatId(f.kat)).await;
    assert!(claim(&core, "first-hold").await.is_none());
    // Pruning admits a stale kat_busy hold while paused; unpause after its snapshot.
    paused(&pool, &f, true).await;
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *gate)
        .await
        .unwrap();
    let other = core.clone();
    let pending = tokio::spawn(async move { claim(&other, "repeat-hold").await });
    waiting(&pool, "SELECT id FROM threads WHERE workspace_id").await;
    paused(&pool, &f, false).await;
    gate.rollback().await.unwrap();
    assert!(pending.await.unwrap().is_none());
    let holds = events(&pool, ThreadId(f.thread))
        .await
        .into_iter()
        .filter(|e| e["name"] == "katwork.job_status" && e["value"]["holdReason"] == "kat_busy")
        .count();
    assert_eq!(holds, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn candidate_lock_failure_does_not_abort_poll(pool: PgPool) {
    let blocked = setup(&pool).await;
    let other = setup(&pool).await;
    let core = core(&pool);
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(blocked.thread)
        .execute(&mut *gate)
        .await
        .unwrap();
    assert_eq!(
        claim(&core, "other").await.unwrap().task_id,
        TaskId(other.task)
    );
    gate.rollback().await.unwrap();
    assert_eq!(
        claim(&core, "unblocked").await.unwrap().task_id,
        TaskId(blocked.task)
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn candidate_commit_failure_does_not_abort_poll(pool: PgPool) {
    let bad = setup(&pool).await;
    let other = setup(&pool).await;
    sqlx::query("UPDATE tasks SET input=input||'{\"fail_commit\":true}' WHERE id=$1")
        .bind(bad.task)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE FUNCTION reject_claim_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.input->>'fail_commit'='true' AND NEW.state='leased' THEN
                RAISE EXCEPTION 'injected commit failure';
            END IF;
            RETURN NEW;
        END $$;
        CREATE CONSTRAINT TRIGGER reject_claim_commit AFTER UPDATE ON tasks
        DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_claim_commit()",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        claim(&core(&pool), "other").await.unwrap().task_id,
        TaskId(other.task)
    );
    assert_eq!(task_state(&pool, TaskId(bad.task)).await, "queued");
}
