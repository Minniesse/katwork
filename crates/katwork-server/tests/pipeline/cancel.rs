#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_ends_job_cancelled(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    assert_eq!(
        f.status(job).await,
        (Some("cancelled".into()), Some("cancelled".into()))
    );
    support::assert_code(
        f.call("POST", &format!("jobs/{job}/cancel"), None).await,
        409,
        "job_already_finished",
    )
    .await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_queued_job_terminalizes_its_task(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    let state: String = sqlx::query_scalar("SELECT state FROM tasks WHERE job_id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "cancelled");
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM task_leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leases, 0);
    assert!(
        lease::claim(
            &f.core,
            "worker",
            Duration::from_secs(60),
            &katwork::agent::Limits::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE author_kind='kat'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_other_workspace_job_is_404(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
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
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR NO KEY UPDATE")
        .bind(job)
        .execute(&mut *tx)
        .await
        .unwrap();
    let r = timeout(
        Duration::from_secs(2),
        request(
            &f.app,
            "POST",
            &format!("/api/workspaces/{other}/jobs/{job}/cancel"),
            None,
            Some(&f.cookie),
        ),
    )
    .await
    .unwrap();
    support::assert_code(r, 404, "not_found").await;
    let r = request(
        &f.app,
        "GET",
        &format!("/api/workspaces/{other}/jobs/{job}"),
        None,
        Some(&f.cookie),
    )
    .await;
    support::assert_code(r, 404, "not_found").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn job_view_shows_queue_position_while_queued(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, a) = f.submit("@Writer one").await;
    f.submit("@Researcher unrelated queue").await;
    let (_, b) = f.submit("@Writer two").await;
    for (job, position) in [(a, 1), (b, 2)] {
        let r = f.call("GET", &format!("jobs/{job}"), None).await;
        assert_eq!(r.status(), 200);
        let v = body(r).await;
        assert_eq!(v["queue_position"], position);
        assert!(v["status"].is_null());
    }
    let c = lease::claim(
        &f.core,
        "worker",
        Duration::from_secs(60),
        &katwork::agent::Limits::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(c.job_id, a);
    assert!(
        body(f.call("GET", &format!("jobs/{a}"), None).await).await["queue_position"].is_null()
    );
    assert_eq!(
        body(f.call("GET", &format!("jobs/{b}"), None).await).await["queue_position"],
        1
    );
    f.call("POST", &format!("jobs/{b}/cancel"), None).await;
    assert!(
        body(f.call("GET", &format!("jobs/{b}"), None).await).await["queue_position"].is_null()
    );
}

use katwork::{
    agent::{Limits, RunFacts, StopReason},
    events::{AgUiEvent, EventSink, KatworkEvent, PgEventSink, RunStartedEvent},
    fence::Fence,
};
pub(super) async fn claim(f: &Fixture) -> lease::ClaimedTask {
    lease::claim(
        &f.core,
        "worker",
        Duration::from_secs(60),
        &Limits::default(),
    )
    .await
    .unwrap()
    .unwrap()
}
pub(super) async fn start_run(
    f: &Fixture,
    c: &lease::ClaimedTask,
) -> (RunId, MessageId, PgEventSink) {
    let run = RunId::new();
    let reply = MessageId::new();
    sqlx::query("INSERT INTO runs(id,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state) VALUES($1,$2,$3,$4,$5,$6,$7,'test','running')")
        .bind(run).bind(c.workspace_id).bind(c.job_id).bind(c.task_id).bind(c.kat_id).bind(c.kind.wire()).bind(c.lease_epoch).execute(&f.core.pool).await.unwrap();
    let sink = PgEventSink::new(
        f.core.pool.clone(),
        f.core.events.clone(),
        f.core.config.lock_timeout,
        c.workspace_id,
        c.thread_id,
        Some(run),
        Some(Fence {
            task_id: c.task_id,
            epoch: c.lease_epoch,
        }),
    );
    sink.emit(AgUiEvent::RunStarted {
        thread_id: c.thread_id.to_string(),
        run_id: run.to_string(),
        protocol_version: Some("1.0".into()),
    })
    .await
    .unwrap();
    sink.emit(
        KatworkEvent::RunStarted(RunStartedEvent {
            run_id: run,
            job_id: c.job_id,
            kat_id: c.kat_id,
            reply_message_id: reply,
            synthetic: true,
        })
        .into(),
    )
    .await
    .unwrap();
    (run, reply, sink)
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_stores_streamed_text_as_a_stopped_reply(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let c = claim(&f).await;
    let (run, reply, sink) = start_run(&f, &c).await;
    for chunks in [vec!["Old draft"], vec!["Hel", "lo"]] {
        sink.emit(AgUiEvent::TextMessageStart {
            message_id: reply.to_string(),
            role: Some("assistant".into()),
        })
        .await
        .unwrap();
        for chunk in chunks {
            sink.emit(AgUiEvent::TextMessageContent {
                message_id: reply.to_string(),
                delta: chunk.into(),
            })
            .await
            .unwrap();
        }
    }
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    let page = message::list(&pool, &f.tenant, f.thread, None, 50)
        .await
        .unwrap();
    let saved = page.messages.last().unwrap();
    assert_eq!(saved.id, reply);
    assert_eq!(saved.body, "Hello");
    assert!(saved.stopped);
    assert!(saved.synthetic);
    assert_eq!(saved.job_id, Some(job));
    let events = f.events().await;
    let stored = events
        .iter()
        .rposition(|e| e["name"] == "katwork.message_stored")
        .unwrap();
    let closed = events
        .iter()
        .position(|e| e["name"] == "katwork.run_closed")
        .unwrap();
    assert!(stored < closed);
    assert_eq!(events[closed]["value"]["runId"], json!(run));
    assert_eq!(events[closed + 1]["type"], "RUN_ERROR");
    assert_eq!(events[closed + 1]["code"], "cancelled");
    assert!(matches!(
        sink.emit(AgUiEvent::TextMessageContent {
            message_id: reply.to_string(),
            delta: "late".into()
        })
        .await,
        Err(katwork::error::KatError::LeaseLost)
    ));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_running_without_text_stores_no_reply(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let c = claim(&f).await;
    start_run(&f, &c).await;
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    assert_eq!(
        message::list(&pool, &f.tenant, f.thread, None, 50)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_racing_complete_does_not_deadlock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..50 {
        sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '2 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        let (_, job) = f.submit("hello").await;
        let c = claim(&f).await;
        let (run, reply, _) = start_run(&f, &c).await;
        let mut facts = RunFacts::not_run(StopReason::NoEligibleKat);
        facts.run_id = Some(run);
        facts.reply_message_id = reply;
        facts.stop = None;
        facts.final_text = Some("Answer".into());
        let barrier = tokio::sync::Barrier::new(2);
        let (cancel, complete) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    barrier.wait().await;
                    f.call("POST", &format!("jobs/{job}/cancel"), None).await
                },
                async {
                    barrier.wait().await;
                    lease::complete(&f.core, c.task_id, "worker", c.lease_epoch, facts).await
                }
            )
        })
        .await
        .unwrap();
        assert!(matches!(cancel.status().as_u16(), 202 | 409));
        assert!(
            matches!(
                complete,
                Ok(())
                    | Err(katwork::error::KatError::LeaseLost)
                    | Err(katwork::error::KatError::JobAlreadyFinished)
            ),
            "{complete:?}"
        );
        let status = f.status(job).await.0.unwrap();
        assert!(matches!(status.as_str(), "cancelled" | "answered"));
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM events WHERE run_id=$1 AND kind IN ('RUN_ERROR','RUN_FINISHED')",
        )
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_closes_open_calls_before_terminal_events(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let c = claim(&f).await;
    let (run, _, _) = start_run(&f, &c).await;
    let call = ToolCallRecordId::new();
    sqlx::query("INSERT INTO tool_calls(id,workspace_id,run_id,step,tool_call_id,tool_name,side_effect,args_canonical,args_sha256,state) VALUES($1,$2,$3,1,'pending','read_thread','read','{}','test','started')").bind(call).bind(f.tenant.workspace_id()).bind(run).execute(&pool).await.unwrap();
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    let saved: (String, bool) =
        sqlx::query_as("SELECT state,ended_at IS NOT NULL FROM tool_calls WHERE id=$1")
            .bind(call)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(saved, ("outcome_unknown".into(), true));
    let events = f.events().await;
    let result = events
        .iter()
        .position(|e| e["type"] == "TOOL_CALL_RESULT")
        .unwrap();
    assert_eq!(events[result]["toolCallId"], json!(call));
    assert_eq!(events[result + 1]["name"], "katwork.tool_status");
    assert_eq!(events[result + 1]["value"]["ok"], false);
    assert_eq!(events[result + 2]["name"], "katwork.kat_state");
    assert_eq!(events[result + 3]["name"], "katwork.run_closed");
    assert_eq!(events[result + 4]["type"], "RUN_ERROR");
    assert_eq!(events[result + 5]["name"], "katwork.job_status");
}

async fn colleague(f: &Fixture, role: &str) -> (Tenant, String) {
    let user = UserId::new();
    sqlx::query("INSERT INTO users(id,email,password_hash,display_name,locale) VALUES($1,$2,'unused','Colleague','en')")
        .bind(user).bind(format!("{user}@example.test")).execute(&f.core.pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(workspace_id,user_id,role) VALUES($1,$2,$3)")
        .bind(f.tenant.workspace_id())
        .bind(user)
        .bind(role)
        .execute(&f.core.pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&f.core.pool, user, f.tenant.workspace_id())
        .await
        .unwrap();
    let token = katwork::auth::session::create(&f.core.pool, user)
        .await
        .unwrap();
    (tenant, format!("kw_session={}", token.0))
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_rejects_colleague_member_before_any_writes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let (tenant, cookie) = colleague(&f, "member").await;
    let before = f.events().await;
    let response = request(
        &f.app,
        "POST",
        &format!(
            "/api/workspaces/{}/jobs/{job}/cancel",
            f.tenant.workspace_id()
        ),
        None,
        Some(&cookie),
    )
    .await;
    support::assert_code(response, 403, "forbidden").await;
    assert!(matches!(
        katwork::jobs::job::cancel(
            &pool,
            &f.core.events,
            &tenant,
            job,
            std::time::Duration::from_secs(5)
        )
        .await,
        Err(katwork::error::KatError::Forbidden)
    ));
    assert_eq!(f.events().await, before);
    assert_eq!(f.status(job).await.0, None);
    let state: String = sqlx::query_scalar("SELECT state FROM tasks WHERE job_id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    f.call("POST", &format!("jobs/{job}/cancel"), None).await;
    let response = request(
        &f.app,
        "POST",
        &format!(
            "/api/workspaces/{}/jobs/{job}/cancel",
            f.tenant.workspace_id()
        ),
        None,
        Some(&cookie),
    )
    .await;
    support::assert_code(response, 403, "forbidden").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_allows_requester_and_workspace_admin_or_owner(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE memberships SET role='member' WHERE workspace_id=$1 AND user_id=$2")
        .bind(f.tenant.workspace_id())
        .bind(f.tenant.user_id())
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("my job").await;
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    for role in ["admin", "owner"] {
        let (_, job) = f.submit("another job").await;
        let (_, cookie) = colleague(&f, role).await;
        let response = request(
            &f.app,
            "POST",
            &format!(
                "/api/workspaces/{}/jobs/{job}/cancel",
                f.tenant.workspace_id()
            ),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), 202);
        assert_eq!(f.status(job).await.0.as_deref(), Some("cancelled"));
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancel_rechecks_admin_role_after_job_lock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let (admin, _) = colleague(&f, "admin").await;
    let before = f.events().await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR NO KEY UPDATE")
        .bind(job)
        .execute(&mut *holder)
        .await
        .unwrap();
    let cancel = katwork::jobs::job::cancel(
        &pool,
        &f.core.events,
        &admin,
        job,
        std::time::Duration::from_secs(5),
    );
    tokio::pin!(cancel);
    tokio::select! {
        result = &mut cancel => panic!("cancel bypassed job lock: {result:?}"),
        () = wait_for_locks(&pool, "jobs", 1) => {}
    }
    sqlx::query("UPDATE memberships SET role='member' WHERE workspace_id=$1 AND user_id=$2")
        .bind(f.tenant.workspace_id())
        .bind(admin.user_id())
        .execute(&pool)
        .await
        .unwrap();
    let mut task_holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE job_id=$1 FOR NO KEY UPDATE")
        .bind(job)
        .execute(&mut *task_holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(2), cancel).await.unwrap(),
        Err(katwork::error::KatError::Forbidden)
    ));
    assert_eq!(f.status(job).await.0, None);
    let state: String = sqlx::query_scalar("SELECT state FROM tasks WHERE job_id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    assert_eq!(f.events().await, before);
    task_holder.rollback().await.unwrap();
}
