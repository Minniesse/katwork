#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;
use katwork::{chat::UserPost, pipeline};

#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_rolled_back_leaves_no_message_job_or_task(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    pipeline::intake(
        &mut tx,
        &f.core,
        &f.tenant,
        f.thread,
        UserPost {
            body: "hello".into(),
            client_msg_id: Some("rollback".into()),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let counts:(i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM messages),(SELECT count(*) FROM jobs),(SELECT count(*) FROM tasks),(SELECT count(*) FROM events),(SELECT last_event_seq FROM threads LIMIT 1)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0, 0));
    let retry = body(f.post("hello", Some("rollback")).await).await;
    assert!(retry["job_id"].is_string());
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn post_to_other_workspace_thread_is_404_without_waiting(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = WorkspaceId::new();
    sqlx::query("INSERT INTO workspaces(id,name) VALUES($1,'Other')")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memberships(workspace_id,user_id,role) VALUES($1,$2,'member')")
        .bind(other)
        .bind(f.tenant.user_id())
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *tx)
        .await
        .unwrap();
    let r = timeout(
        Duration::from_secs(2),
        request(
            &f.app,
            "POST",
            &format!("/api/workspaces/{other}/threads/{}/messages", f.thread),
            Some(json!({"body":"hello"})),
            Some(&f.cookie),
        ),
    )
    .await
    .unwrap();
    support::assert_code(r, 404, "not_found").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn post_fails_busy_when_thread_lock_is_held_past_timeout(pool: PgPool) {
    let f = Fixture::with_timeout(&pool, 200).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *tx)
        .await
        .unwrap();
    let r = f.post("hello", Some("busy")).await;
    assert_eq!(r.headers()["retry-after"], "1");
    support::assert_code(r, 503, "busy").await;
    tx.commit().await.unwrap();
    assert_eq!(f.post("hello", Some("busy")).await.status(), 201);
    let counts: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM messages),(SELECT count(*) FROM jobs)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counts, (1, 1));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn follow_up_intake_racing_late_cancel_does_not_deadlock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..50 {
        sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '2 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        let parent = f.awaiting().await;
        let barrier = tokio::sync::Barrier::new(2);
        let (post, cancel) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    barrier.wait().await;
                    f.submit("yes").await
                },
                async {
                    barrier.wait().await;
                    f.call("POST", &format!("jobs/{parent}/cancel"), None).await
                }
            )
        })
        .await
        .unwrap();
        support::assert_code(cancel, 409, "job_already_finished").await;
        let saved: JobId = sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
            .bind(post.1)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(saved, parent);
    }
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn quota_reject_racing_claim_does_not_deadlock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..50 {
        sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '2 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE kats SET daily_token_budget=NULL WHERE id=$1")
            .bind(f.kats[0].id)
            .execute(&pool)
            .await
            .unwrap();
        let (_, queued) = f.submit("queued").await;
        sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
            .bind(f.kats[0].id)
            .execute(&pool)
            .await
            .unwrap();
        let barrier = tokio::sync::Barrier::new(2);
        let (posted, claimed) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    barrier.wait().await;
                    f.submit("reject").await
                },
                async {
                    barrier.wait().await;
                    lease::claim(
                        &f.core,
                        "worker",
                        Duration::from_secs(60),
                        &katwork::agent::Limits::default(),
                    )
                    .await
                }
            )
        })
        .await
        .unwrap();
        assert_eq!(
            f.status(posted.1).await.1.as_deref(),
            Some("quota_exceeded")
        );
        assert!(claimed.is_ok(), "{claimed:?}");
        assert_eq!(f.status(queued).await.1.as_deref(), Some("quota_exceeded"));
        support::assert_code(
            f.call("POST", &format!("jobs/{queued}/cancel"), None).await,
            409,
            "job_already_finished",
        )
        .await;
    }
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn latest_in_thread_uses_id_order_when_created_at_is_older(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut older = pool.begin().await.unwrap();
    sqlx::query("SELECT now()")
        .execute(&mut *older)
        .await
        .unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *holder)
        .await
        .unwrap();
    let first = pipeline::intake(
        &mut holder,
        &f.core,
        &f.tenant,
        f.thread,
        UserPost {
            body: "first".into(),
            client_msg_id: None,
        },
    )
    .await
    .unwrap();
    // Polling the blocked intake proves the older transaction actually waited on the thread.
    let second = {
        let waiting = pipeline::intake(
            &mut older,
            &f.core,
            &f.tenant,
            f.thread,
            UserPost {
                body: "second".into(),
                client_msg_id: None,
            },
        );
        tokio::pin!(waiting);
        let poll_wait = async {
            loop {
                let found:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT%FROM threads%')").fetch_one(&pool).await.unwrap();
                if found {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        timeout(Duration::from_secs(5),async {tokio::select! {result=&mut waiting=>panic!("intake did not wait: {result:?}"),()=poll_wait=>{holder.commit().await.unwrap();waiting.await.unwrap()}}}).await.unwrap()
    };
    older.commit().await.unwrap();
    let times: (bool,) = sqlx::query_as(
        "SELECT a.created_at>b.created_at FROM jobs a,jobs b WHERE a.id=$1 AND b.id=$2",
    )
    .bind(first.job_id)
    .bind(second.job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(times.0);
    let latest = katwork::jobs::job::latest_in_thread(
        &mut pool.acquire().await.unwrap(),
        &f.tenant,
        f.thread,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(latest.id, second.job_id);
    sqlx::query("UPDATE jobs SET status='needs_input',reason='question_pending' WHERE id=$1")
        .bind(second.job_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET state='done' WHERE job_id=$1")
        .bind(second.job_id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, followup) = f.submit("yes").await;
    let parent: JobId = sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
        .bind(followup)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(parent, second.job_id);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_rollback_leaves_no_events(pool: PgPool) {
    use futures::FutureExt;
    let f = Fixture::new(&pool).await;
    f.submit("existing").await;
    let before = f.events().await;
    let mut subscriber = f.core.events.subscribe(f.thread);
    let mut tx = pool.begin().await.unwrap();
    pipeline::intake(
        &mut tx,
        &f.core,
        &f.tenant,
        f.thread,
        UserPost {
            body: "discarded".into(),
            client_msg_id: Some("rollback-events".into()),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(f.events().await, before);
    assert!(subscriber.recv().now_or_never().is_none());
    let seq: EventSeq = sqlx::query_scalar("SELECT last_event_seq FROM threads WHERE id=$1")
        .bind(f.thread)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(seq, EventSeq(2));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn quota_rechecks_budget_after_waiting_for_kat_lock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.kats[0].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let pending = f.submit("hello");
    tokio::pin!(pending);
    let waiting = async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id FROM kats WHERE workspace_id=%')").fetch_one(&pool).await.unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    let (_,job)=timeout(Duration::from_secs(5),async {tokio::select! {
        result=&mut pending=>panic!("intake did not wait: {result:?}"),
        ()=waiting=>{
            sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1").bind(f.kats[0].id).execute(&mut *holder).await.unwrap();
            holder.commit().await.unwrap();pending.await
        }
    }}).await.unwrap();
    assert_eq!(f.status(job).await.1.as_deref(), Some("quota_exceeded"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

async fn eligibility_change_while_waiting(pool: PgPool, change: &'static str) {
    let f = Fixture::new(&pool).await;
    if change.starts_with("DELETE") {
        let elsewhere = SpaceId::new();
        sqlx::query("INSERT INTO spaces(id,workspace_id,name,allow_external) VALUES($1,$2,'Elsewhere',false)")
            .bind(elsewhere).bind(f.tenant.workspace_id()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO kat_spaces(kat_id,workspace_id,space_id) VALUES($1,$2,$3)")
            .bind(f.kats[2].id)
            .bind(f.tenant.workspace_id())
            .bind(elsewhere)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE kats SET default_space_id=$2 WHERE id=$1")
            .bind(f.kats[2].id)
            .bind(elsewhere)
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.kats[2].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let post = f.post("@Writer hello", Some("eligibility"));
    tokio::pin!(post);
    tokio::select! {
        _ = &mut post => panic!("intake bypassed Kat lock"),
        () = wait_for_locks(&pool, "kats", 1) => {}
    }
    sqlx::query(change)
        .bind(f.kats[2].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
    support::assert_code(post.await, 503, "busy").await;
    let counts: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM messages),(SELECT count(*) FROM jobs),(SELECT count(*) FROM tasks),(SELECT count(*) FROM events)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    let retry = body(f.post("@Writer hello", Some("eligibility")).await).await;
    let job = serde_json::from_value(retry["job_id"].clone()).unwrap();
    let (kat, input) = f.task(job).await;
    assert_eq!(kat, f.kats[0].id);
    assert_eq!(input.routing.rule, katwork::jobs::RouteRule::Lead);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_rechecks_pause_after_kat_lock(pool: PgPool) {
    eligibility_change_while_waiting(pool, "UPDATE kats SET status='paused' WHERE id=$1").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_rechecks_grant_after_kat_lock(pool: PgPool) {
    eligibility_change_while_waiting(pool, "DELETE FROM kat_spaces WHERE kat_id=$1 AND space_id<>(SELECT default_space_id FROM kats WHERE id=$1)").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_rechecks_membership_after_kat_lock(pool: PgPool) {
    eligibility_change_while_waiting(pool, "UPDATE team_members SET active=false WHERE kat_id=$1")
        .await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn intake_refreshes_policy_version_after_kat_lock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.kats[2].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let post = f.submit("@Writer hello");
    tokio::pin!(post);
    tokio::select! {
        _ = &mut post => panic!("intake bypassed Kat lock"),
        () = wait_for_locks(&pool, "kats", 1) => {}
    }
    sqlx::query("UPDATE kats SET policy_version=policy_version+1 WHERE id=$1")
        .bind(f.kats[2].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
    let (_, job) = post.await;
    let version: i64 = sqlx::query_scalar("SELECT kat_policy_version FROM tasks WHERE job_id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, f.kats[2].policy_version + 1);
}
