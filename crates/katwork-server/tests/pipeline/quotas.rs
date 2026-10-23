use super::*;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_budget_exceeded_is_stopped_quota_exceeded(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("hello").await;
    assert_eq!(
        f.status(job).await,
        (Some("stopped".into()), Some("quota_exceeded".into()))
    );
    assert_eq!(f.notices().await[0]["code"], "quota_exceeded");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn retry_after_intake_stop_returns_same_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let a = body(f.post("hello", Some("stop")).await).await;
    let retry = f.post("hello", Some("stop")).await;
    assert_eq!(retry.status(), 200);
    assert_eq!(body(retry).await, a);
    let ids: Vec<JobId> = sqlx::query_scalar("SELECT job_id FROM messages ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|id| json!(id) == a["job_id"]));
    assert_eq!(f.events().await.len(), 4);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn rate_limit_is_independent_per_kat_and_per_sender(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..6 {
        f.submit("hello").await;
    }
    let (_, job) = f.submit("seventh").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("too_many_requests"));
    let (_, job) = f.submit("@Writer hello").await;
    assert!(f.status(job).await.0.is_none());
    let user = UserId::new();
    sqlx::query("INSERT INTO users(id,email,password_hash,display_name,locale) VALUES($1,'second@example.test','unused','Second','en')").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(workspace_id,user_id,role) VALUES($1,$2,'member')")
        .bind(f.tenant.workspace_id())
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&pool, user, f.tenant.workspace_id())
        .await
        .unwrap();
    let posted = katwork::pipeline::submit(
        &f.core,
        &tenant,
        f.thread,
        katwork::chat::UserPost {
            body: "hello".into(),
            client_msg_id: None,
        },
    )
    .await
    .unwrap();
    assert!(f.status(posted.job_id).await.0.is_none());
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn per_sender_throttle_window_boundary(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..6 {
        f.submit("hello").await;
    }
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '60 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("allowed").await;
    assert!(f.status(job).await.0.is_none());
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '59 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("blocked").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("too_many_requests"));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_budget_counts_charged_tokens_when_usage_is_missing(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![super::execution::answer(
        "Reply", "Reply",
    )]));
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    let (charged, complete): (i64, bool) =
        sqlx::query_as("SELECT charged_tokens,usage_complete FROM model_calls")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(charged > 0);
    assert!(!complete);
    sqlx::query("UPDATE kats SET daily_token_budget=$2 WHERE id=$1")
        .bind(f.kats[0].id)
        .bind(charged)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("next").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("quota_exceeded"));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_quota_window_excludes_old_charges(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    sqlx::query("UPDATE kats SET daily_token_budget=1 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE model_calls SET created_at=clock_timestamp()-interval '24 hours'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("allowed").await;
    assert!(f.status(job).await.0.is_none());
    sqlx::query("UPDATE model_calls SET created_at=clock_timestamp()-interval '23 hours'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("blocked").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("quota_exceeded"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_budget_claim_stops_admitted_burst(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let fake = FakeProvider::scripted(
        (0..6)
            .map(|_| super::execution::answer("Reply", "Reply"))
            .collect(),
    );
    f.provider(fake.clone());
    sqlx::query("UPDATE kats SET daily_token_budget=1 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for _ in 0..6 {
        jobs.push(f.submit("burst").await.1);
    }
    f.run(*jobs.last().unwrap()).await;
    assert_eq!(
        fake.received().len(),
        1,
        "queued tasks must stop after the first charged task"
    );
    assert_eq!(f.status(jobs[0]).await.0.as_deref(), Some("answered"));
    for job in &jobs[1..] {
        assert_eq!(
            f.status(*job).await,
            (Some("stopped".into()), Some("quota_exceeded".into()))
        );
    }
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM task_leases),(SELECT count(*) FROM tasks WHERE state='failed')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 0, 5));
    assert_eq!(
        f.notices()
            .await
            .iter()
            .filter(|n| n["code"] == "quota_exceeded")
            .count(),
        5
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn budgeted_kat_keeps_concurrency_and_bounds_burst(pool: PgPool) {
    use katwork::agent::{
        Limits, RunFacts, StopReason,
        ledger::{self, ModelCallRow},
    };
    use katwork::fence::Fence;
    let f = Fixture::new(&pool).await;
    assert_eq!(f.kats[0].max_concurrency, 3);
    sqlx::query("UPDATE kats SET daily_token_budget=1 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for _ in 0..6 {
        jobs.push(f.submit("burst").await.1);
    }
    let mut active = Vec::new();
    for _ in 0..3 {
        active.push(super::cancel::claim(&f).await);
    }
    assert!(
        lease::claim(
            &f.core,
            "worker",
            Duration::from_secs(60),
            &Limits::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    let charges = [11, 12, 13];
    for (c, charged) in active.iter().zip(charges) {
        let (run, reply, _) = super::cancel::start_run(&f, c).await;
        ledger::record_model_call(
            &pool,
            &Fence {
                task_id: c.task_id,
                epoch: c.lease_epoch,
            },
            &ModelCallRow {
                id: ModelCallId::new(),
                workspace_id: c.workspace_id,
                space_id: f.space,
                job_id: c.job_id,
                task_id: c.task_id,
                run_id: run,
                kat_id: c.kat_id,
                provider_id: "fake".into(),
                model: "script".into(),
                residency: "local".into(),
                attempt: 1,
                input_tokens: Some(charged),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                output_tokens: Some(0),
                usage_complete: true,
                charged_tokens: charged,
                cost_micros: None,
                latency_ms: 0,
                outcome: "done".into(),
            },
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();
        let mut facts = RunFacts::not_run(StopReason::NoEligibleKat);
        facts.stop = None;
        facts.run_id = Some(run);
        facts.reply_message_id = reply;
        facts.final_text = Some("Done".into());
        lease::complete(&f.core, c.task_id, "worker", c.lease_epoch, facts)
            .await
            .unwrap();
        assert!(
            lease::claim(
                &f.core,
                "worker",
                Duration::from_secs(60),
                &Limits::default()
            )
            .await
            .unwrap()
            .is_none(),
            "no new claim after the cap is reached"
        );
    }
    let (calls, total): (i64, i64) =
        sqlx::query_as("SELECT count(*),sum(charged_tokens)::bigint FROM model_calls")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((calls, total), (3, 36));
    assert!(total <= 1 + charges.iter().sum::<i64>());
    for job in &jobs[..3] {
        assert_eq!(f.status(*job).await.0.as_deref(), Some("answered"));
    }
    for job in &jobs[3..] {
        assert_eq!(f.status(*job).await.1.as_deref(), Some("quota_exceeded"));
        let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM tasks WHERE job_id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(attempts, 0);
    }
}

async fn stopped_answer_keeps_question(pool: PgPool, quota: bool) {
    let f = Fixture::new(&pool).await;
    let parent = f.awaiting().await;
    if quota {
        sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
            .bind(f.kats[2].id)
            .execute(&pool)
            .await
            .unwrap();
    } else {
        let current = thread::get(&pool, &f.tenant, f.thread).await.unwrap();
        let other = thread::create(&pool, &f.tenant, f.space, current.team_id, "Other")
            .await
            .unwrap();
        for _ in 0..5 {
            katwork::pipeline::submit(
                &f.core,
                &f.tenant,
                other.id,
                katwork::chat::UserPost {
                    body: "@Writer unrelated".into(),
                    client_msg_id: None,
                },
            )
            .await
            .unwrap();
        }
    }
    for _ in 0..2 {
        let (_, stopped) = f.submit("my answer").await;
        assert_eq!(
            f.status(stopped).await.1.as_deref(),
            Some(if quota {
                "quota_exceeded"
            } else {
                "too_many_requests"
            })
        );
        let saved: JobId = sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
            .bind(stopped)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(saved, parent);
    }
    sqlx::query("UPDATE kats SET daily_token_budget=NULL")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '2 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, retry) = f.submit("my answer again").await;
    let saved: JobId = sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
        .bind(retry)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, parent);
    let (kat, input) = f.task(retry).await;
    assert_eq!(kat, f.kats[2].id);
    assert_eq!(input.routing.rule, katwork::jobs::RouteRule::FollowUp);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn quota_stopped_answer_can_be_resent(pool: PgPool) {
    stopped_answer_keeps_question(pool, true).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn throttle_stopped_answer_can_be_resent(pool: PgPool) {
    stopped_answer_keeps_question(pool, false).await;
}
