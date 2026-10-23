#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[allow(dead_code)]
mod support;

use katwork::{
    agent::{Initiator, PendingCall, RunFacts, StopReason},
    chat::{self, Author, NewMessage},
    error::KatError,
    events::{AgUiEvent, KatworkEvent, RunClosedEvent, RunEnd, log},
    ids::*,
    jobs::{HarnessEnd, RouteRule, Routing, TaskInput, job, task},
    policy::{self, Thresholds},
    tenancy::{self, Tenant},
    types::Question,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use support::Fixture;

async fn tenant(pool: &PgPool, f: &Fixture) -> Tenant {
    tenancy::resolve(pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap()
}
async fn locked(pool: &PgPool, f: &Fixture) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.unwrap();
    katwork::db::set_lock_timeout(&mut tx, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    sqlx::query("SELECT id FROM jobs WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(f.workspace)
        .bind(f.job)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "SELECT id FROM tasks WHERE workspace_id=$1 AND job_id=$2 ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(f.workspace)
    .bind(f.job)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "SELECT id FROM runs WHERE workspace_id=$1 AND job_id=$2 ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(f.workspace)
    .bind(f.job)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(f.workspace)
        .bind(f.thread)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}
fn answered(f: &Fixture) -> RunFacts {
    let mut facts = RunFacts::not_run(StopReason::ModelProtocol);
    facts.run_id = Some(RunId(f.run));
    facts.stop = None;
    facts.final_text = Some("คำตอบ".into());
    facts.synthetic = true;
    facts
}
fn stopped(f: &Fixture, stop: StopReason) -> RunFacts {
    let mut facts = RunFacts::not_run(stop);
    facts.run_id = Some(RunId(f.run));
    facts
}
async fn conclude(pool: &PgPool, f: &Fixture, facts: &RunFacts) -> Result<EventSeq, KatError> {
    let t = tenant(pool, f).await;
    let mut tx = locked(pool, f).await;
    let seq = job::conclude(
        &mut tx,
        &t,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        &f.task.to_string(),
        facts,
    )
    .await?;
    tx.commit().await?;
    Ok(seq)
}
async fn events(pool: &PgPool, f: &Fixture) -> Vec<Value> {
    let rows: Vec<(sqlx::types::Json<Value>,)> = sqlx::query_as(
        "SELECT payload FROM events WHERE workspace_id=$1 AND thread_id=$2 ORDER BY seq",
    )
    .bind(f.workspace)
    .bind(f.thread)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(v,)| v.0).collect()
}
async fn outcome(
    pool: &PgPool,
    f: &Fixture,
) -> (Option<String>, Option<String>, Option<String>, Option<i64>) {
    sqlx::query_as(
        "SELECT status,reason,decided_by,decision_seq FROM jobs WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(f.job)
    .fetch_one(pool)
    .await
    .unwrap()
}
fn input(f: &Fixture) -> TaskInput {
    TaskInput {
        thread_id: ThreadId(f.thread),
        request_message_id: MessageId(f.message),
        actor: UserId(f.user),
        initiator: Initiator::Person,
        routing: Routing {
            rule: RouteRule::Lead,
            ignored: vec![],
        },
    }
}
async fn new_request(pool: &PgPool, f: &Fixture, thread: ThreadId) -> MessageId {
    chat::message::append(
        &mut pool.acquire().await.unwrap(),
        &tenant(pool, f).await,
        thread,
        NewMessage {
            id: None,
            author: Author::User(UserId(f.user)),
            body: "Follow up".into(),
            client_msg_id: None,
            idempotency_key: None,
            synthetic: false,
            stopped: false,
            question: None,
            meta: None,
            job_id: None,
        },
    )
    .await
    .unwrap()
    .message
    .id
}

#[sqlx::test(migrations = "./migrations")]
async fn finish_by_policy_records_reason_and_decided_by(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let facts = RunFacts::not_run(StopReason::TimeLimit);
    let v = policy::decide_job(&facts, &Thresholds::SP1);
    let mut tx = locked(&pool, &f).await;
    let seq = log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        None,
        &policy::decision_event(JobId(f.job), &facts, &v, &Thresholds::SP1).into(),
    )
    .await
    .unwrap();
    assert_eq!(
        job::finish_by_policy(&mut tx, JobId(f.job), &v, seq)
            .await
            .unwrap()
            .0,
        seq.0 + 1
    );
    tx.commit().await.unwrap();
    assert_eq!(
        outcome(&pool, &f).await,
        (
            Some("stopped".into()),
            Some("time_limit".into()),
            Some("policy".into()),
            Some(seq.0)
        )
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn finish_by_harness_sets_cancelled_and_error(pool: PgPool) {
    for (end, status, reason) in [
        (HarnessEnd::Cancelled, "cancelled", "cancelled"),
        (
            HarnessEnd::Error {
                code: katwork::types::ReasonCode::RetriesExhausted,
            },
            "error",
            "retries_exhausted",
        ),
    ] {
        let f = Fixture::new(&pool).await;
        let mut tx = locked(&pool, &f).await;
        job::finish_by_harness(&mut tx, JobId(f.job), end)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            outcome(&pool, &f).await,
            (
                Some(status.into()),
                Some(reason.into()),
                Some("harness".into()),
                None
            )
        );
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn cannot_finish_twice(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    conclude(&pool, &f, &answered(&f)).await.unwrap();
    let mut tx = locked(&pool, &f).await;
    assert_eq!(
        job::finish_by_harness(&mut tx, JobId(f.job), HarnessEnd::Cancelled)
            .await
            .unwrap_err()
            .code(),
        "job_already_finished"
    );
    tx.commit().await.unwrap();
    assert_eq!(outcome(&pool, &f).await.0.as_deref(), Some("answered"));
}
#[sqlx::test(migrations = "./migrations")]
async fn decision_event_precedes_job_status(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut facts = answered(&f);
    facts.stop = Some(StopReason::TimeLimit);
    conclude(&pool, &f, &facts).await.unwrap();
    let ev = events(&pool, &f).await;
    assert_eq!(
        ev.iter()
            .map(|v| v["name"].as_str().unwrap_or(v["type"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [
            "katwork.decision",
            "katwork.message_stored",
            "katwork.message_stored",
            "katwork.job_status",
            "katwork.kat_state",
            "katwork.run_closed",
            "RUN_FINISHED"
        ]
    );
    assert_eq!(ev[4]["value"]["state"], "idle");
}
#[sqlx::test(migrations = "./migrations")]
async fn paused_conclusion_uses_stopped_notice_without_changing_job_reason(pool: PgPool) {
    for (stop, reason, notice) in [
        (
            StopReason::WorkspacePaused,
            "workspace_paused",
            "workspace_paused_stopped",
        ),
        (StopReason::KatPaused, "kat_paused", "kat_paused_stopped"),
    ] {
        let f = Fixture::new(&pool).await;
        let mut facts = answered(&f);
        facts.stop = Some(stop);
        conclude(&pool, &f, &facts).await.unwrap();
        assert_eq!(outcome(&pool, &f).await.1.as_deref(), Some(reason));
        let (body, meta): (String, Value) = sqlx::query_as(
            "SELECT body,meta FROM messages WHERE workspace_id=$1 AND job_id=$2 AND author_kind='system'",
        )
        .bind(f.workspace).bind(f.job).fetch_one(&pool).await.unwrap();
        assert_eq!(body, notice);
        assert_eq!(meta["code"], notice);
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn conclude_rejects_open_tool_calls_even_when_facts_omit_them(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.tool_call(&pool, 1, Some("read"), "started")
        .await
        .unwrap();
    let mut facts = answered(&f);
    facts.stop = Some(StopReason::TimeLimit);
    let mut tx = locked(&pool, &f).await;
    let result = job::conclude(
        &mut tx,
        &tenant(&pool, &f).await,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        "open-call",
        &facts,
    )
    .await;
    assert_eq!(result.unwrap_err().code(), "request_invalid");
    tx.commit().await.unwrap();
    assert_eq!(outcome(&pool, &f).await.0, None);
    assert!(events(&pool, &f).await.is_empty());
    let state: (String, bool) =
        sqlx::query_as("SELECT state,ended_at IS NULL FROM runs WHERE id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("running".into(), true));
}
#[sqlx::test(migrations = "./migrations")]
async fn conclude_without_run_id_rejects_a_running_run(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = locked(&pool, &f).await;
    let result = job::conclude(
        &mut tx,
        &tenant(&pool, &f).await,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        "missing-run",
        &RunFacts::not_run(StopReason::TimeLimit),
    )
    .await;
    assert_eq!(result.unwrap_err().code(), "request_invalid");
    tx.commit().await.unwrap();
    assert_eq!(outcome(&pool, &f).await.0, None);
    assert!(events(&pool, &f).await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn conclude_without_run_id_accepts_a_job_with_no_run(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let request = new_request(&pool, &f, ThreadId(f.thread)).await;
    f.job = job::create(
        &mut pool.acquire().await.unwrap(),
        &tenant(&pool, &f).await,
        ThreadId(f.thread),
        request,
    )
    .await
    .unwrap()
    .0;
    conclude(&pool, &f, &RunFacts::not_run(StopReason::TimeLimit))
        .await
        .unwrap();
    assert_eq!(outcome(&pool, &f).await.0.as_deref(), Some("stopped"));
    assert!(
        events(&pool, &f)
            .await
            .iter()
            .all(|e| e["type"] != "RUN_FINISHED")
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn conclude_notice_collision_rolls_back_even_if_caller_commits(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut facts = answered(&f);
    facts.stop = Some(StopReason::TimeLimit);
    let mut tx = locked(&pool, &f).await;
    // Keep a caller-owned write outside the conclusion's savepoint.
    sqlx::query("UPDATE messages SET idempotency_key='notice:collision' WHERE id=$1")
        .bind(f.message)
        .execute(&mut *tx)
        .await
        .unwrap();
    let err = job::conclude(
        &mut tx,
        &tenant(&pool, &f).await,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        "collision",
        &facts,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "request_invalid");
    tx.commit().await.unwrap();
    assert!(events(&pool, &f).await.is_empty());
    assert_eq!(outcome(&pool, &f).await, (None, None, None, None));
    let messages: Vec<(MessageId, Option<String>)> =
        sqlx::query_as("SELECT id,idempotency_key FROM messages WHERE workspace_id=$1")
            .bind(f.workspace)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        messages,
        [(MessageId(f.message), Some("notice:collision".into()))]
    );
    let state: (String, bool) =
        sqlx::query_as("SELECT state,ended_at IS NULL FROM runs WHERE id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("running".into(), true));
    let seq: i64 = sqlx::query_scalar("SELECT last_event_seq FROM threads WHERE id=$1")
        .bind(f.thread)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(seq, 0);
    conclude(&pool, &f, &facts).await.unwrap();
    assert_eq!(
        events(&pool, &f)
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.decision")
            .count(),
        1
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn conclusion_messages_link_to_their_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut facts = answered(&f);
    facts.stop = Some(StopReason::StepLimit);
    conclude(&pool, &f, &facts).await.unwrap();
    let page = chat::message::list(
        &pool,
        &tenant(&pool, &f).await,
        ThreadId(f.thread),
        None,
        100,
    )
    .await
    .unwrap();
    let replies = page
        .messages
        .iter()
        .filter(|m| m.author != Author::User(UserId(f.user)))
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().all(|m| m.job_id == Some(JobId(f.job))));
    assert_eq!(replies[0].id, facts.reply_message_id);
    assert!(replies[0].synthetic);
}
#[sqlx::test(migrations = "./migrations")]
async fn follow_up_job_links_parent_needs_input_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let request = new_request(&pool, &f, ThreadId(f.thread)).await;
    let t = tenant(&pool, &f).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        job::create_follow_up(&mut conn, &t, JobId(f.job), request)
            .await
            .unwrap_err()
            .code(),
        "job_not_awaiting_input"
    );
    conclude(&pool, &f, &stopped(&f, StopReason::NeedsApproval))
        .await
        .unwrap();
    let child = job::create_follow_up(&mut conn, &t, JobId(f.job), request)
        .await
        .unwrap();
    let row: (JobId, Option<String>, UserId) = sqlx::query_as(
        "SELECT parent_job_id,status,requested_by FROM jobs WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(child)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(row, (JobId(f.job), None, UserId(f.user)));
}
#[sqlx::test(migrations = "./migrations")]
async fn enqueue_is_idempotent_on_key(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let inp = input(&f);
    let mut conn = pool.acquire().await.unwrap();
    let a = task::enqueue(&mut conn, JobId(f.job), KatId(f.kat), 7, &inp, "enqueue")
        .await
        .unwrap();
    let b = task::enqueue(&mut conn, JobId(f.job), KatId(f.kat), 7, &inp, "enqueue")
        .await
        .unwrap();
    assert_eq!(a, b);
    let row: (i64, Value, String) = sqlx::query_as(
        "SELECT kat_policy_version,input,state FROM tasks WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(a)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        row,
        (7, serde_json::to_value(inp).unwrap(), "queued".into())
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn conclude_retry_returns_one_reply_and_one_status(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let facts = answered(&f);
    conclude(&pool, &f, &facts).await.unwrap();
    let before = events(&pool, &f).await;
    // Even committing a failed retry must not leave another decision.
    let mut tx = locked(&pool, &f).await;
    let err = job::conclude(
        &mut tx,
        &tenant(&pool, &f).await,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        &f.task.to_string(),
        &facts,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "job_already_finished");
    tx.commit().await.unwrap();
    assert_eq!(events(&pool, &f).await, before);
    for name in [
        "katwork.decision",
        "katwork.message_stored",
        "katwork.job_status",
    ] {
        assert_eq!(before.iter().filter(|v| v["name"] == name).count(), 1);
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn stopped_notice_names_the_kat_in_meta(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    conclude(&pool, &f, &stopped(&f, StopReason::KatPaused))
        .await
        .unwrap();
    let ev = events(&pool, &f).await;
    assert_eq!(ev[1]["value"]["body"], "kat_paused_stopped");
    assert_eq!(
        ev[1]["value"]["meta"],
        json!({"code":"kat_paused_stopped","kats":[{"id":f.kat,"name":"นักวิจัย"}]})
    );
    assert_eq!(ev.len(), 6);
}
#[sqlx::test(migrations = "./migrations")]
async fn reply_with_nul_is_stored_with_replacement_character(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut facts = answered(&f);
    facts.final_text = Some("ก\0ข".into());
    conclude(&pool, &f, &facts).await.unwrap();
    let body: String =
        sqlx::query_scalar("SELECT body FROM messages WHERE workspace_id=$1 AND id=$2")
            .bind(f.workspace)
            .bind(facts.reply_message_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(body, "ก�ข");
    assert_eq!(events(&pool, &f).await[1]["value"]["body"], body);
}
#[sqlx::test(migrations = "./migrations")]
async fn run_closed_precedes_run_finished(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let seq = conclude(&pool, &f, &answered(&f)).await.unwrap();
    let ev = events(&pool, &f).await;
    assert_eq!(seq.0 as usize, ev.len());
    assert_eq!(
        ev[ev.len() - 2],
        json!({"type":"CUSTOM","name":"katwork.run_closed","value":{"runId":f.run,"end":"finished","code":null}})
    );
    assert_eq!(
        ev.last().unwrap(),
        &json!({"type":"RUN_FINISHED","threadId":f.thread.to_string(),"runId":f.run.to_string()})
    );
    let row: (String, bool) = sqlx::query_as(
        "SELECT state,ended_at IS NOT NULL FROM runs WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(f.run)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("finished".into(), true));
}
#[sqlx::test(migrations = "./migrations")]
async fn run_closed_carries_the_reason_of_a_run_that_did_not_answer(pool: PgPool) {
    for (stop, code, state) in [
        (
            Some(StopReason::ProviderUnavailable),
            "provider_unavailable",
            "idle",
        ),
        (Some(StopReason::TimeLimit), "time_limit", "idle"),
        (None, "question_pending", "waiting"),
        (
            Some(StopReason::NeedsApproval),
            "approval_pending",
            "needs_approval",
        ),
        (
            Some(StopReason::WorkspacePaused),
            "workspace_paused",
            "paused",
        ),
    ] {
        let f = Fixture::new(&pool).await;
        let mut facts = answered(&f);
        facts.stop = stop;
        if stop.is_none() {
            facts.pending_question = Some(Question {
                question: "Which\0 one?".into(),
                options: vec!["A\0B".into()],
            });
        }
        conclude(&pool, &f, &facts).await.unwrap();
        let ev = events(&pool, &f).await;
        assert_eq!(ev[ev.len() - 2]["value"]["code"], code);
        assert_eq!(ev[ev.len() - 3]["value"]["state"], state);
        if stop.is_none() {
            assert_eq!(ev[1]["value"]["body"], "Which� one?");
            assert_eq!(ev[1]["value"]["question"]["options"], json!(["A�B"]));
        }
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn latest_in_thread_orders_by_id_not_created_at(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let request = new_request(&pool, &f, ThreadId(f.thread)).await;
    let t = tenant(&pool, &f).await;
    let mut conn = pool.acquire().await.unwrap();
    let newer = job::create(&mut conn, &t, ThreadId(f.thread), request)
        .await
        .unwrap();
    assert!(newer.0 > f.job);
    sqlx::query(
        "UPDATE jobs SET created_at=created_at-interval '1 day' WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(newer)
    .execute(&mut *conn)
    .await
    .unwrap();
    let mut inp = input(&f);
    inp.request_message_id = request;
    task::enqueue(&mut conn, newer, KatId(f.kat), 1, &inp, "newer")
        .await
        .unwrap();
    let latest = job::latest_in_thread(&mut conn, &t, ThreadId(f.thread))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, newer);
    assert_eq!(latest.status, None);
    assert_eq!(latest.kat_id, Some(KatId(f.kat)));
}

#[sqlx::test(migrations = "./migrations")]
async fn conclude_rejects_foreign_or_mismatched_context_without_writes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let foreign = Fixture::new(&pool).await;
    let t = tenant(&pool, &f).await;
    for (job_id, thread, kat, run) in [
        (
            JobId(foreign.job),
            ThreadId(foreign.thread),
            KatId(foreign.kat),
            RunId(foreign.run),
        ),
        (
            JobId(f.job),
            ThreadId(foreign.thread),
            KatId(f.kat),
            RunId(f.run),
        ),
        (
            JobId(f.job),
            ThreadId(f.thread),
            KatId(foreign.kat),
            RunId(f.run),
        ),
        (
            JobId(f.job),
            ThreadId(f.thread),
            KatId(f.kat),
            RunId(foreign.run),
        ),
    ] {
        let mut tx = locked(&pool, &f).await;
        let mut facts = answered(&f);
        facts.run_id = Some(run);
        assert_eq!(
            job::conclude(&mut tx, &t, job_id, thread, Some(kat), "foreign", &facts)
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
        tx.commit().await.unwrap();
        assert!(events(&pool, &f).await.is_empty());
        assert!(events(&pool, &foreign).await.is_empty());
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn create_and_enqueue_validate_request_and_tenant(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let foreign = Fixture::new(&pool).await;
    let t = tenant(&pool, &f).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        job::create(
            &mut conn,
            &t,
            ThreadId(f.thread),
            MessageId(foreign.message)
        )
        .await
        .unwrap_err()
        .code(),
        "not_found"
    );
    assert_eq!(
        job::latest_in_thread(&mut conn, &t, ThreadId(foreign.thread))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        job::create_follow_up(&mut conn, &t, JobId(foreign.job), MessageId(f.message))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        task::enqueue(
            &mut conn,
            JobId(f.job),
            KatId(foreign.kat),
            1,
            &input(&f),
            "foreign"
        )
        .await
        .unwrap_err()
        .code(),
        "not_found"
    );
    let mut inp = input(&f);
    inp.actor = UserId(foreign.user);
    assert_eq!(
        task::enqueue(&mut conn, JobId(f.job), KatId(f.kat), 1, &inp, "actor")
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn enqueue_key_cannot_replay_different_arguments(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    task::enqueue(&mut conn, JobId(f.job), KatId(f.kat), 1, &input(&f), "same")
        .await
        .unwrap();
    assert_eq!(
        task::enqueue(&mut conn, JobId(f.job), KatId(f.kat), 2, &input(&f), "same")
            .await
            .unwrap_err()
            .code(),
        "request_invalid"
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn policy_finish_requires_its_own_matching_decision(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let facts = RunFacts::not_run(StopReason::TimeLimit);
    let v = policy::decide_job(&facts, &Thresholds::SP1);
    let mut tx = locked(&pool, &f).await;
    assert_eq!(
        job::finish_by_policy(&mut tx, JobId(f.job), &v, EventSeq(999))
            .await
            .unwrap_err()
            .code(),
        "request_invalid"
    );
    tx.commit().await.unwrap();
    assert_eq!(outcome(&pool, &f).await.0, None);
}
#[sqlx::test(migrations = "./migrations")]
async fn conclusion_rolls_back_messages_status_and_terminal_together(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = locked(&pool, &f).await;
    job::conclude(
        &mut tx,
        &tenant(&pool, &f).await,
        JobId(f.job),
        ThreadId(f.thread),
        Some(KatId(f.kat)),
        "rollback",
        &answered(&f),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(outcome(&pool, &f).await.0, None);
    assert!(events(&pool, &f).await.is_empty());
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE workspace_id=$1 AND thread_id=$2")
            .bind(f.workspace)
            .bind(f.thread)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    let state: String =
        sqlx::query_scalar("SELECT state FROM runs WHERE workspace_id=$1 AND id=$2")
            .bind(f.workspace)
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "running");
}
#[sqlx::test(migrations = "./migrations")]
async fn already_closed_run_gets_no_second_terminal_or_later_run_event(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = locked(&pool, &f).await;
    sqlx::query("UPDATE runs SET state='cancelled',ended_at=clock_timestamp() WHERE workspace_id=$1 AND id=$2").bind(f.workspace).bind(f.run).execute(&mut *tx).await.unwrap();
    log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(RunId(f.run)),
        &KatworkEvent::RunClosed(RunClosedEvent {
            run_id: RunId(f.run),
            end: RunEnd::Cancelled,
            code: Some("cancelled".into()),
        })
        .into(),
    )
    .await
    .unwrap();
    log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(RunId(f.run)),
        &AgUiEvent::RunError {
            code: Some("cancelled".into()),
            message: "cancelled".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut facts = RunFacts::not_run(StopReason::Cancelled);
    facts.run_id = Some(RunId(f.run));
    facts.run_closed = true;
    conclude(&pool, &f, &facts).await.unwrap();
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events WHERE workspace_id=$1 AND run_id=$2 ORDER BY seq",
    )
    .bind(f.workspace)
    .bind(f.run)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(kinds, ["katwork.run_closed", "RUN_ERROR"]);
}
#[sqlx::test(migrations = "./migrations")]
async fn approval_follow_up_gets_a_fresh_task_without_the_pending_call(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.tool_call(&pool, 1, Some("destructive"), "refused")
        .await
        .unwrap();
    let record: ToolCallRecordId = sqlx::query_scalar(
        "UPDATE tool_calls SET ended_at=clock_timestamp(),result='needs_approval'
        WHERE workspace_id=$1 AND run_id=$2 RETURNING id",
    )
    .bind(f.workspace)
    .bind(f.run)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut facts = stopped(&f, StopReason::NeedsApproval);
    facts.pending_tool_call = Some(PendingCall {
        record_id: record,
        tool_call_id: "call".into(),
        name: "tool".into(),
        args_canonical: "{}".into(),
        args_sha256: "hash".into(),
    });
    let pending = serde_json::to_value(&facts.pending_tool_call).unwrap();
    sqlx::query("UPDATE tasks SET pending_tool_call=$2 WHERE id=$1")
        .bind(f.task)
        .bind(&pending)
        .execute(&pool)
        .await
        .unwrap();
    conclude(&pool, &f, &facts).await.unwrap();
    assert_eq!(
        outcome(&pool, &f).await.1.as_deref(),
        Some("approval_pending")
    );

    let request = new_request(&pool, &f, ThreadId(f.thread)).await;
    let mut conn = pool.acquire().await.unwrap();
    let child = job::create_follow_up(&mut conn, &tenant(&pool, &f).await, JobId(f.job), request)
        .await
        .unwrap();
    let mut inp = input(&f);
    inp.request_message_id = request;
    inp.routing.rule = RouteRule::FollowUp;
    let child_task = task::enqueue(&mut conn, child, KatId(f.kat), 1, &inp, "follow-up")
        .await
        .unwrap();
    assert_ne!(child_task, TaskId(f.task));
    let saved: (String, Value, Option<Value>) =
        sqlx::query_as("SELECT state,input,pending_tool_call FROM tasks WHERE id=$1")
            .bind(child_task)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(
        saved,
        ("queued".into(), serde_json::to_value(inp).unwrap(), None)
    );
    let parent_pending: Value =
        sqlx::query_scalar("SELECT pending_tool_call FROM tasks WHERE id=$1")
            .bind(f.task)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(parent_pending, pending);
    let calls: Vec<(ToolCallRecordId, String, bool)> = sqlx::query_as(
        "SELECT id,state,ended_at IS NOT NULL FROM tool_calls WHERE workspace_id=$1",
    )
    .bind(f.workspace)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(calls, [(record, "refused".into(), true)]);
}

async fn race(pool: &PgPool, f: &Fixture, cancel: bool) {
    let facts = answered(f);
    let t = tenant(pool, f).await;
    let gate = tokio::sync::Barrier::new(2);
    let left = async {
        gate.wait().await;
        conclude(pool, f, &facts).await
    };
    let right = async {
        gate.wait().await;
        if !cancel {
            return conclude(pool, f, &facts).await;
        }
        let mut tx = locked(pool, f).await;
        // Stand in for Task 12's caller-owned run closure without adding its cancel route.
        sqlx::query("UPDATE runs SET state='cancelled',stop_reason='cancelled',ended_at=clock_timestamp() WHERE workspace_id=$1 AND id=$2")
            .bind(f.workspace).bind(f.run).execute(&mut *tx).await?;
        log::append(
            &mut tx,
            WorkspaceId(f.workspace),
            ThreadId(f.thread),
            Some(RunId(f.run)),
            &KatworkEvent::RunClosed(RunClosedEvent {
                run_id: RunId(f.run),
                end: RunEnd::Cancelled,
                code: Some("cancelled".into()),
            })
            .into(),
        )
        .await?;
        log::append(
            &mut tx,
            WorkspaceId(f.workspace),
            ThreadId(f.thread),
            Some(RunId(f.run)),
            &AgUiEvent::RunError {
                code: Some("cancelled".into()),
                message: "cancelled".into(),
            },
        )
        .await?;
        let seq = job::finish_by_harness(&mut tx, JobId(f.job), HarnessEnd::Cancelled).await?;
        tx.commit().await?;
        Ok(seq)
    };
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(left, right)
    })
    .await
    .unwrap();
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()).unwrap().code(), "job_already_finished");
    let ev = events(pool, f).await;
    assert_eq!(
        ev.iter()
            .filter(|v| v["type"] == "RUN_FINISHED" || v["type"] == "RUN_ERROR")
            .count(),
        1
    );
    assert_eq!(
        ev.iter()
            .filter(|v| v["name"] == "katwork.run_closed")
            .count(),
        1
    );
    assert_eq!(
        ev.iter()
            .filter(|v| v["name"] == "katwork.job_status")
            .count(),
        1
    );
    let page = chat::message::list(pool, &t, ThreadId(f.thread), None, 100)
        .await
        .unwrap();
    let count = page
        .messages
        .iter()
        .filter(|m| matches!(m.author, Author::Kat(_)))
        .count();
    assert_eq!(
        count,
        usize::from(outcome(pool, f).await.0.as_deref() == Some("answered"))
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn conclude_racing_cancel_has_one_terminal_writer(pool: PgPool) {
    for _ in 0..4 {
        race(&pool, &Fixture::new(&pool).await, true).await;
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn double_conclude_has_one_reply_decision_and_status(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    race(&pool, &f, false).await;
    assert_eq!(
        events(&pool, &f)
            .await
            .iter()
            .filter(|v| v["name"] == "katwork.decision")
            .count(),
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn follow_up_rejects_a_request_from_another_thread(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let t = tenant(&pool, &f).await;
    conclude(&pool, &f, &stopped(&f, StopReason::NeedsApproval))
        .await
        .unwrap();
    let thread = ThreadId::new();
    sqlx::query(
        "INSERT INTO threads (id,workspace_id,space_id,team_id,title) VALUES ($1,$2,$3,$4,'Other')",
    )
    .bind(thread)
    .bind(f.workspace)
    .bind(f.space)
    .bind(f.team)
    .execute(&pool)
    .await
    .unwrap();
    let request = new_request(&pool, &f, thread).await;
    let err = job::create_follow_up(
        &mut pool.acquire().await.unwrap(),
        &t,
        JobId(f.job),
        request,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "job_not_awaiting_input");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE workspace_id=$1")
        .bind(f.workspace)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn inconsistent_run_closure_is_rejected_before_writing(pool: PgPool) {
    for (state, ended, closed) in [
        ("running", false, true),
        ("finished", true, false),
        ("running", true, false),
        ("cancelled", false, true),
    ] {
        let f = Fixture::new(&pool).await;
        sqlx::query("UPDATE runs SET state=$3,ended_at=CASE WHEN $4 THEN clock_timestamp() ELSE NULL END WHERE workspace_id=$1 AND id=$2")
            .bind(f.workspace).bind(f.run).bind(state).bind(ended).execute(&pool).await.unwrap();
        let mut facts = answered(&f);
        facts.run_closed = closed;
        let mut tx = locked(&pool, &f).await;
        let err = job::conclude(
            &mut tx,
            &tenant(&pool, &f).await,
            JobId(f.job),
            ThreadId(f.thread),
            Some(KatId(f.kat)),
            "invalid-closure",
            &facts,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "request_invalid", "{state}, {ended}, {closed}");
        tx.commit().await.unwrap();
        assert!(events(&pool, &f).await.is_empty());
        assert_eq!(outcome(&pool, &f).await.0, None);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn policy_finish_rejects_a_forged_or_foreign_decision(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let facts = RunFacts::not_run(StopReason::TimeLimit);
    let v = policy::decide_job(&facts, &Thresholds::SP1);
    for changed in ["jobId", "inputs", "thresholds", "policyVersion", "verdict"] {
        let mut tx = locked(&pool, &f).await;
        let AgUiEvent::Custom { name, mut value } = AgUiEvent::from(policy::decision_event(
            JobId(f.job),
            &facts,
            &v,
            &Thresholds::SP1,
        )) else {
            panic!("custom")
        };
        value[changed] = match changed {
            "jobId" => json!(JobId::new()),
            "inputs" => json!(policy::PolicyInputs::from_facts(&answered(&f))),
            "thresholds" => json!({"version":2}),
            "policyVersion" => json!(2),
            _ => json!({"status":"answered","reason":null,"next":null}),
        };
        let seq = log::append(
            &mut tx,
            WorkspaceId(f.workspace),
            ThreadId(f.thread),
            None,
            &AgUiEvent::Custom { name, value },
        )
        .await
        .unwrap();
        assert_eq!(
            job::finish_by_policy(&mut tx, JobId(f.job), &v, seq)
                .await
                .unwrap_err()
                .code(),
            "request_invalid",
            "{changed}"
        );
        tx.commit().await.unwrap();
        assert_eq!(outcome(&pool, &f).await.0, None);
    }
    assert!(
        events(&pool, &f)
            .await
            .iter()
            .all(|ev| ev["name"] == "katwork.decision")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn latest_job_uses_newest_task_and_empty_thread_has_no_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let t = tenant(&pool, &f).await;
    let mut conn = pool.acquire().await.unwrap();
    let thread = ThreadId::new();
    sqlx::query(
        "INSERT INTO threads (id,workspace_id,space_id,team_id,title) VALUES ($1,$2,$3,$4,'Empty')",
    )
    .bind(thread)
    .bind(f.workspace)
    .bind(f.space)
    .bind(f.team)
    .execute(&mut *conn)
    .await
    .unwrap();
    assert!(
        job::latest_in_thread(&mut conn, &t, thread)
            .await
            .unwrap()
            .is_none()
    );
    let request = new_request(&pool, &f, thread).await;
    let job_id = job::create(&mut conn, &t, thread, request).await.unwrap();
    assert_eq!(
        job::latest_in_thread(&mut conn, &t, thread)
            .await
            .unwrap()
            .unwrap()
            .kat_id,
        None
    );
    let newer_kat = KatId::new();
    let mut kat_tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO kats (id,workspace_id,default_space_id,name,role,persona,instructions,tool_allowlist,model_tier,autonomy) SELECT $1,workspace_id,default_space_id,'Other','researcher','','','{}','standard','ask_first' FROM kats WHERE workspace_id=$2 AND id=$3")
        .bind(newer_kat).bind(f.workspace).bind(f.kat).execute(&mut *kat_tx).await.unwrap();
    sqlx::query("INSERT INTO kat_spaces (workspace_id,kat_id,space_id) VALUES ($1,$2,$3)")
        .bind(f.workspace)
        .bind(newer_kat)
        .bind(f.space)
        .execute(&mut *kat_tx)
        .await
        .unwrap();
    kat_tx.commit().await.unwrap();
    let mut inp = input(&f);
    inp.thread_id = thread;
    inp.request_message_id = request;
    let older = task::enqueue(&mut conn, job_id, KatId(f.kat), 1, &inp, "older")
        .await
        .unwrap();
    let newer = task::enqueue(&mut conn, job_id, newer_kat, 1, &inp, "newer")
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET created_at=(SELECT created_at FROM tasks WHERE workspace_id=$1 AND id=$2) WHERE workspace_id=$1 AND id=$3")
        .bind(f.workspace).bind(older).bind(newer).execute(&mut *conn).await.unwrap();
    assert_eq!(
        job::latest_in_thread(&mut conn, &t, thread)
            .await
            .unwrap()
            .unwrap()
            .kat_id,
        Some(newer_kat)
    );
    sqlx::query(
        "UPDATE tasks SET created_at=created_at + interval '1 day' WHERE workspace_id=$1 AND id=$2",
    )
    .bind(f.workspace)
    .bind(older)
    .execute(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        job::latest_in_thread(&mut conn, &t, thread)
            .await
            .unwrap()
            .unwrap()
            .kat_id,
        Some(KatId(f.kat))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn heartbeat_stop_can_override_a_closed_run_reason(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = locked(&pool, &f).await;
    sqlx::query("UPDATE runs SET state='cancelled',stop_reason='cancelled',ended_at=clock_timestamp() WHERE workspace_id=$1 AND id=$2")
        .bind(f.workspace).bind(f.run).execute(&mut *tx).await.unwrap();
    log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(RunId(f.run)),
        &KatworkEvent::RunClosed(RunClosedEvent {
            run_id: RunId(f.run),
            end: RunEnd::Cancelled,
            code: Some("cancelled".into()),
        })
        .into(),
    )
    .await
    .unwrap();
    log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(RunId(f.run)),
        &AgUiEvent::RunError {
            code: Some("cancelled".into()),
            message: "cancelled".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut facts = RunFacts::not_run(StopReason::WorkspacePaused);
    facts.run_id = Some(RunId(f.run));
    facts.run_closed = true;
    conclude(&pool, &f, &facts).await.unwrap();
    assert_eq!(
        outcome(&pool, &f).await.1.as_deref(),
        Some("workspace_paused")
    );
    let ev = events(&pool, &f).await;
    assert_eq!(
        ev.iter()
            .filter(|v| v["name"] == "katwork.run_closed")
            .count(),
        1
    );
    assert_eq!(ev.last().unwrap()["value"]["state"], "paused");
}
