#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[allow(dead_code)]
mod support;

use katwork::{
    agent::{ledger, run, *},
    chat::{self, Author, NewMessage},
    config::Config,
    error::KatError,
    events::{AgUiEvent, EventSink, PgEventSink},
    fence::Fence,
    ids::*,
    jobs::{
        RouteRule, Routing, TaskInput, job,
        lease::{self, ClaimedTask},
        task, worker,
    },
    models::*,
    runtime::Core,
    team::{kat, team},
    tenancy::{self, Tenant},
    types::{KatStatus, Locale, SideEffect, SpacePolicy},
};
use serde_json::Value;
use sqlx::{PgPool, types::Json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use support::Fixture;
use tokio::{
    sync::Barrier,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

const TTL: Duration = Duration::from_secs(60);

fn core(pool: &PgPool) -> Core {
    let config = Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://unused".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        "KATWORK_DEMO" => Some("1".into()),
        "KATWORK_LOCK_TIMEOUT_MS" => Some("500".into()),
        _ => None,
    })
    .unwrap();
    Core::new(pool.clone(), Arc::new(config))
}
async fn tenant(pool: &PgPool, f: &Fixture) -> Tenant {
    tenancy::resolve(pool, UserId(f.user), WorkspaceId(f.workspace))
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
async fn setup(pool: &PgPool) -> Fixture {
    let f = Fixture::new(pool).await;
    // The shared schema fixture includes a historical run; this task has not been claimed yet.
    sqlx::query("UPDATE runs SET state='finished',ended_at=clock_timestamp() WHERE id=$1")
        .bind(f.run)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE kats SET appearance='{\"avatar\":\"kat\",\"accent\":\"blue\"}' WHERE id=$1",
    )
    .bind(f.kat)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tasks SET input=$2 WHERE id=$1")
        .bind(f.task)
        .bind(Json(input(&f)))
        .execute(pool)
        .await
        .unwrap();
    f
}
async fn enqueue(pool: &PgPool, f: &Fixture, kat: KatId) -> TaskId {
    let tenant = tenant(pool, f).await;
    let mut tx = pool.begin().await.unwrap();
    let request = chat::message::append(
        &mut tx,
        &tenant,
        ThreadId(f.thread),
        NewMessage {
            id: None,
            author: Author::User(UserId(f.user)),
            body: "Next task".into(),
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
    .id;
    let job = job::create(&mut tx, &tenant, ThreadId(f.thread), request)
        .await
        .unwrap();
    let mut input = input(f);
    input.request_message_id = request;
    let version: i64 = sqlx::query_scalar("SELECT policy_version FROM kats WHERE id=$1")
        .bind(kat)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let task = task::enqueue(
        &mut tx,
        job,
        kat,
        version,
        &input,
        &TaskId::new().to_string(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    task
}
async fn claim(core: &Core, owner: &str) -> Option<ClaimedTask> {
    lease::claim(core, owner, TTL, &Limits::default())
        .await
        .unwrap()
}
async fn expire(pool: &PgPool, task: TaskId) {
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE task_id=$1",
    )
    .bind(task)
    .execute(pool)
    .await
    .unwrap();
}
async fn start_run(core: &Core, c: &ClaimedTask) -> RunId {
    let id = RunId::new();
    sqlx::query("INSERT INTO runs(id,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state) VALUES($1,$2,$3,$4,$5,$6,$7,'test','running')")
        .bind(id).bind(c.workspace_id).bind(c.job_id).bind(c.task_id).bind(c.kat_id)
        .bind(c.kind.wire()).bind(c.lease_epoch).execute(&core.pool).await.unwrap();
    sink(core, c, id)
        .emit(AgUiEvent::RunStarted {
            thread_id: c.thread_id.to_string(),
            run_id: id.to_string(),
            protocol_version: Some("1.0".into()),
        })
        .await
        .unwrap();
    id
}
fn sink(core: &Core, c: &ClaimedTask, run: RunId) -> PgEventSink {
    PgEventSink::new(
        core.pool.clone(),
        core.events.clone(),
        core.config.lock_timeout,
        c.workspace_id,
        c.thread_id,
        Some(run),
        Some(Fence {
            task_id: c.task_id,
            epoch: c.lease_epoch,
        }),
    )
}
async fn tool(core: &Core, c: &ClaimedTask, run: RunId, effect: SideEffect) -> ToolCallRecordId {
    let ctx = ToolCtx {
        lock_timeout: std::time::Duration::from_secs(5),
        tenant: tenancy::resolve(&core.pool, c.requested_by, c.workspace_id)
            .await
            .unwrap(),
        thread_id: c.thread_id,
        space_id: sqlx::query_scalar("SELECT space_id FROM threads WHERE id=$1")
            .bind(c.thread_id)
            .fetch_one(&core.pool)
            .await
            .unwrap(),
        kat_id: c.kat_id,
        job_id: c.job_id,
        task_id: c.task_id,
        run_id: run,
        step: 1,
        tool_call_id: "probe".into(),
        idempotency_key: run.to_string(),
        lease_epoch: c.lease_epoch,
        policy_version: c.policy_version,
        actor: c.requested_by,
        initiator: Initiator::Person,
        computer: None,
        pool: core.pool.clone(),
        cancel: CancellationToken::new(),
    };
    ledger::start_tool_call(&core.pool, &ctx, "probe", effect, "{}")
        .await
        .unwrap()
}
fn answered(run: RunId) -> RunFacts {
    let mut facts = RunFacts::not_run(StopReason::Interrupted);
    facts.run_id = Some(run);
    facts.stop = None;
    facts.final_text = Some("One reply".into());
    facts
}
async fn task_state(pool: &PgPool, task: TaskId) -> String {
    sqlx::query_scalar("SELECT state FROM tasks WHERE id=$1")
        .bind(task)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn outcome(pool: &PgPool, job: JobId) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT status,reason FROM jobs WHERE id=$1")
        .bind(job)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn events(pool: &PgPool, thread: ThreadId) -> Vec<Value> {
    sqlx::query_scalar::<_, Json<Value>>(
        "SELECT payload FROM events WHERE thread_id=$1 ORDER BY seq",
    )
    .bind(thread)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|v| v.0)
    .collect()
}
async fn terminals(pool: &PgPool, run: RunId) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM events WHERE run_id=$1 AND kind IN ('RUN_ERROR','RUN_FINISHED') ORDER BY seq")
        .bind(run).fetch_all(pool).await.unwrap()
}
async fn finish(core: &Core, c: &ClaimedTask, owner: &str) {
    let run = start_run(core, c).await;
    lease::complete(core, c.task_id, owner, c.lease_epoch, answered(run))
        .await
        .unwrap();
}
async fn paused(pool: &PgPool, f: &Fixture, value: bool) {
    sqlx::query("UPDATE workspaces SET paused=$2 WHERE id=$1")
        .bind(f.workspace)
        .bind(value)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn two_workers_never_claim_same_task(pool: PgPool) {
    let f = setup(&pool).await;
    sqlx::query("UPDATE kats SET max_concurrency=100 WHERE id=$1")
        .bind(f.kat)
        .execute(&pool)
        .await
        .unwrap();
    for _ in 1..50 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    let core = core(&pool);
    let a = async {
        let mut ids = vec![];
        while let Some(c) = claim(&core, "a").await {
            ids.push(c.task_id);
        }
        ids
    };
    let b = async {
        let mut ids = vec![];
        while let Some(c) = claim(&core, "b").await {
            ids.push(c.task_id);
        }
        ids
    };
    let (a, b) = tokio::join!(a, b);
    let ids: Vec<_> = a.into_iter().chain(b).collect();
    assert_eq!(ids.len(), 50);
    assert_eq!(ids.into_iter().collect::<HashSet<_>>().len(), 50);
    let live: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_leases WHERE expires_at>clock_timestamp()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live, 50);
}
#[sqlx::test(migrations = "./migrations")]
async fn expired_lease_is_reclaimed_once(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    assert_eq!(a.task_id, b.task_id);
    assert!(b.lease_epoch.0 > a.lease_epoch.0);
    assert_eq!(b.kind, RunKind::Recovery);
    assert!(claim(&core, "c").await.is_none());
    let before = events(&pool, a.thread_id).await;
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
    assert!(matches!(
        lease::complete(&core, a.task_id, "a", a.lease_epoch, answered(run)).await,
        Err(KatError::LeaseLost)
    ));
    assert!(matches!(
        sink(&core, &a, run)
            .emit(AgUiEvent::TextMessageContent {
                message_id: MessageId::new().to_string(),
                delta: "late".into()
            })
            .await,
        Err(KatError::LeaseLost)
    ));
    assert_eq!(events(&pool, a.thread_id).await, before);
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
}
#[sqlx::test(migrations = "./migrations")]
async fn crash_after_effect_marker_never_requeues(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    let call = tool(&core, &a, run, SideEffect::WorkspaceWrite).await;
    expire(&pool, a.task_id).await;
    for _ in 0..2 {
        assert!(claim(&core, "b").await.is_none());
    }
    assert_eq!(task_state(&pool, a.task_id).await, "interrupted");
    assert_eq!(
        outcome(&pool, a.job_id).await,
        (
            Some("stopped".into()),
            Some("interrupted_review_required".into())
        )
    );
    let state: String = sqlx::query_scalar("SELECT state FROM tool_calls WHERE id=$1")
        .bind(call)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "outcome_unknown");
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
    let recovery: i64 =
        sqlx::query_scalar("SELECT count(*) FROM runs WHERE task_id=$1 AND kind='recovery'")
            .bind(a.task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recovery, 0);
}
#[sqlx::test(migrations = "./migrations")]
async fn expired_lease_after_side_effect_is_interrupted_not_rerun(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    let call = tool(&core, &a, run, SideEffect::WorkspaceWrite).await;
    ledger::end_tool_call(
        &pool,
        &Fence {
            task_id: a.task_id,
            epoch: a.lease_epoch,
        },
        call,
        ToolCallState::Succeeded,
        Some("written"),
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    expire(&pool, a.task_id).await;
    assert!(claim(&core, "b").await.is_none());
    assert_eq!(task_state(&pool, a.task_id).await, "interrupted");
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
}
#[sqlx::test(migrations = "./migrations")]
async fn renewal_committed_between_snapshot_and_lock_is_not_reclaimed(pool: PgPool) {
    setup(&pool).await;
    let mut core = core(&pool);
    Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(30);
    let core = Arc::new(core);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    expire(&pool, a.task_id).await;
    // An updatable view gates only the candidate statement, after its MVCC snapshot.
    sqlx::raw_sql(
        "ALTER TABLE task_leases RENAME TO lease_rows;
        CREATE FUNCTION candidate_gate() RETURNS boolean LANGUAGE plpgsql AS $$
        BEGIN
            IF current_query() LIKE 'SELECT t.id,t.workspace_id%' THEN
                PERFORM pg_advisory_xact_lock(45);
            END IF;
            RETURN true;
        END $$;
        CREATE VIEW task_leases AS SELECT * FROM lease_rows WHERE candidate_gate()",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(45)")
        .execute(&mut *gate)
        .await
        .unwrap();
    // Stage the renewal before the candidate's snapshot; commit it before the task lock.
    let mut renewal = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE id=$1 FOR SHARE")
        .bind(a.task_id)
        .execute(&mut *renewal)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE lease_rows SET expires_at=clock_timestamp()+interval '60 seconds' WHERE task_id=$1",
    )
    .bind(a.task_id)
    .execute(&mut *renewal)
    .await
    .unwrap();
    let other = core.clone();
    let reclaim = tokio::spawn(async move { claim(&other, "b").await });
    races::waiting(&pool, "SELECT t.id,t.workspace_id").await;
    renewal.commit().await.unwrap();
    gate.commit().await.unwrap();
    assert!(reclaim.await.unwrap().is_none());
    let epoch: i64 = sqlx::query_scalar("SELECT lease_epoch FROM tasks WHERE id=$1")
        .bind(a.task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(epoch, a.lease_epoch.0);
    assert!(terminals(&pool, run).await.is_empty());
    lease::heartbeat(
        &pool,
        a.task_id,
        "a",
        a.lease_epoch,
        TTL,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    lease::complete(&core, a.task_id, "a", a.lease_epoch, answered(run))
        .await
        .unwrap();
}
#[sqlx::test(migrations = "./migrations")]
async fn claims_follow_queue_order(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    sqlx::query("UPDATE kats SET max_concurrency=10 WHERE id=$1")
        .bind(f.kat)
        .execute(&pool)
        .await
        .unwrap();
    for _ in 0..4 {
        enqueue(&pool, &f, KatId(f.kat)).await;
    }
    // Equal timestamps also exercise the id tie-break.
    sqlx::query("UPDATE tasks SET created_at=statement_timestamp()")
        .execute(&pool)
        .await
        .unwrap();
    let expected: Vec<TaskId> = sqlx::query_scalar("SELECT id FROM tasks ORDER BY created_at,id")
        .fetch_all(&pool)
        .await
        .unwrap();
    for id in expected {
        assert_eq!(claim(&core, "a").await.unwrap().task_id, id);
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn older_task_locked_by_another_worker_may_be_overtaken(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let second = enqueue(&pool, &f, KatId(f.kat)).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.task)
        .execute(&mut *tx)
        .await
        .unwrap();
    let c = claim(&core, "b").await.unwrap();
    assert_eq!(c.task_id, second);
    tx.commit().await.unwrap();
    finish(&core, &c, "b").await;
    assert_eq!(claim(&core, "a").await.unwrap().task_id, TaskId(f.task));
}
#[sqlx::test(migrations = "./migrations")]
async fn reclaim_does_not_duplicate_reply(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    finish(&core, &b, "b").await;
    assert!(matches!(
        lease::complete(&core, a.task_id, "a", a.lease_epoch, answered(run)).await,
        Err(KatError::LeaseLost)
    ));
    for _ in 0..3 {
        assert!(claim(&core, "c").await.is_none());
    }
    let replies: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE job_id=$1 AND author_kind='kat'")
            .bind(a.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(replies, 1);
    assert_eq!(
        events(&pool, a.thread_id)
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.job_status" && e["value"]["status"] == "answered")
            .count(),
        1
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn reclaimed_run_closes_open_calls_before_new_run_started(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    tool(&core, &a, run, SideEffect::Read).await;
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    start_run(&core, &b).await;
    let ev = events(&pool, a.thread_id).await;
    let kinds: Vec<_> = ev
        .iter()
        .map(|e| e["name"].as_str().unwrap_or(e["type"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [
            "RUN_STARTED",
            "TOOL_CALL_RESULT",
            "katwork.tool_status",
            "katwork.run_closed",
            "RUN_ERROR",
            "RUN_STARTED"
        ]
    );
    assert_eq!(ev[3]["value"]["end"], "interrupted");
    assert_eq!(ev[3]["value"]["code"], "lease_lost");
    assert_eq!(ev[4]["code"], "lease_lost");
}
#[sqlx::test(migrations = "./migrations")]
async fn held_recovery_closes_old_run_once(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    expire(&pool, a.task_id).await;
    paused(&pool, &f, true).await;
    for _ in 0..3 {
        assert!(claim(&core, "b").await.is_none());
    }
    assert_eq!(task_state(&pool, a.task_id).await, "queued");
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
    assert_eq!(
        events(&pool, a.thread_id)
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.job_status")
            .count(),
        1
    );
    paused(&pool, &f, false).await;
    let b = claim(&core, "b").await.unwrap();
    assert_eq!(b.kind, RunKind::Recovery);
    start_run(&core, &b).await;
    assert!(claim(&core, "c").await.is_none());
}
async fn exhausted(pool: &PgPool, polls: usize) {
    let f = setup(pool).await;
    let core = core(pool);
    sqlx::query("UPDATE tasks SET attempts=max_attempts WHERE id=$1")
        .bind(f.task)
        .execute(pool)
        .await
        .unwrap();
    for _ in 0..polls {
        assert!(claim(&core, "a").await.is_none());
    }
    assert_eq!(task_state(pool, TaskId(f.task)).await, "failed");
    assert_eq!(
        outcome(pool, JobId(f.job)).await,
        (Some("error".into()), Some("retries_exhausted".into()))
    );
    assert_eq!(
        events(pool, ThreadId(f.thread))
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.job_status")
            .count(),
        1
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn retries_exhausted_finishes_job_error(pool: PgPool) {
    exhausted(&pool, 1).await;
}
#[sqlx::test(migrations = "./migrations")]
async fn retries_exhausted_branch_finishes_once_across_repeated_polls(pool: PgPool) {
    exhausted(&pool, 4).await;
}
#[sqlx::test(migrations = "./migrations")]
async fn cancelled_job_task_is_never_claimed(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    sqlx::query("UPDATE jobs SET status='cancelled' WHERE id=$1")
        .bind(f.job)
        .execute(&pool)
        .await
        .unwrap();
    assert!(claim(&core, "a").await.is_none());
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "queued");
    assert!(events(&pool, ThreadId(f.thread)).await.is_empty());
}
async fn deadline(pool: &PgPool, polls: usize) {
    let f = setup(pool).await;
    let core = core(pool);
    let a = claim(&core, "a").await.unwrap();
    start_run(&core, &a).await;
    expire(pool, a.task_id).await;
    sqlx::query(
        "UPDATE tasks SET first_claimed_at=clock_timestamp()-interval '6 minutes' WHERE id=$1",
    )
    .bind(f.task)
    .execute(pool)
    .await
    .unwrap();
    for _ in 0..polls {
        assert!(claim(&core, "b").await.is_none());
    }
    assert_eq!(task_state(pool, a.task_id).await, "failed");
    assert_eq!(
        outcome(pool, a.job_id).await,
        (Some("stopped".into()), Some("time_limit".into()))
    );
    assert_eq!(
        events(pool, a.thread_id)
            .await
            .iter()
            .filter(|e| e["name"] == "katwork.job_status")
            .count(),
        1
    );
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM task_leases WHERE task_id=$1")
        .bind(a.task_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(leases, 0);
}
#[sqlx::test(migrations = "./migrations")]
async fn deadline_branch_finishes_once_across_repeated_polls(pool: PgPool) {
    deadline(&pool, 4).await;
}
#[sqlx::test(migrations = "./migrations")]
async fn task_past_deadline_finishes_time_limit(pool: PgPool) {
    deadline(&pool, 1).await;
}
#[sqlx::test(migrations = "./migrations")]
async fn one_kat_runs_one_task_at_a_time(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let second = enqueue(&pool, &f, KatId(f.kat)).await;
    let a = claim(&core, "a").await.unwrap();
    assert!(claim(&core, "b").await.is_none());
    let hold: String = sqlx::query_scalar("SELECT hold_reason FROM tasks WHERE id=$1")
        .bind(second)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(hold, "kat_busy");
    finish(&core, &a, "a").await;
    assert_eq!(claim(&core, "b").await.unwrap().task_id, second);
}
#[sqlx::test(migrations = "./migrations")]
async fn lead_runs_three_tasks_at_once_specialists_one(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let tenant = tenant(&pool, &f).await;
    let team_id = team::create_default(
        &pool,
        &tenant,
        SpaceId(f.space),
        Locale::En,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    let roster = team::get(&pool, &tenant, team_id).await.unwrap();
    sqlx::query("UPDATE tasks SET state='cancelled' WHERE id=$1")
        .bind(f.task)
        .execute(&pool)
        .await
        .unwrap();
    for kat in roster.members {
        let capacity = if kat.role == kat::KatRole::Lead { 3 } else { 1 };
        assert_eq!(kat.max_concurrency, capacity);
        for _ in 0..=capacity {
            enqueue(&pool, &f, kat.id).await;
        }
        for _ in 0..capacity {
            assert_eq!(claim(&core, "a").await.unwrap().kat_id, kat.id);
        }
        assert!(claim(&core, "b").await.is_none());
        let holds: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tasks WHERE kat_id=$1 AND hold_reason='kat_busy'",
        )
        .bind(kat.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(holds, 1);
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn expired_claim_racing_paused_kat_claim_does_not_deadlock(pool: PgPool) {
    for _ in 0..50 {
        let f = setup(&pool).await;
        let core = core(&pool);
        let a = claim(&core, "a").await.unwrap();
        let run = start_run(&core, &a).await;
        enqueue(&pool, &f, KatId(f.kat)).await;
        expire(&pool, a.task_id).await;
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
        let barrier = Barrier::new(2);
        let (a, b) = timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    barrier.wait().await;
                    claim(&core, "b").await
                },
                async {
                    barrier.wait().await;
                    claim(&core, "c").await
                }
            )
        })
        .await
        .unwrap();
        assert!(a.is_none() && b.is_none());
        assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn simultaneous_claims_for_one_kat_dispatch_once(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    enqueue(&pool, &f, KatId(f.kat)).await;
    let barrier = Barrier::new(2);
    let (a, b) = tokio::join!(
        async {
            barrier.wait().await;
            claim(&core, "a").await
        },
        async {
            barrier.wait().await;
            claim(&core, "b").await
        }
    );
    assert_eq!(usize::from(a.is_some()) + usize::from(b.is_some()), 1);
    let holds: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE hold_reason='kat_busy'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(holds, 1);
}
#[sqlx::test(migrations = "./migrations")]
async fn expired_lease_does_not_block_reclaim_at_max_concurrency_one(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    assert_eq!(a.task_id, b.task_id);
    assert_eq!(b.kind, RunKind::Recovery);
}
#[sqlx::test(migrations = "./migrations")]
async fn paused_workspace_claims_nothing(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    paused(&pool, &f, true).await;
    for _ in 0..3 {
        assert!(claim(&core, "a").await.is_none());
    }
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "queued");
    let ev = events(&pool, ThreadId(f.thread)).await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["value"]["holdReason"], "workspace_paused");
}
#[sqlx::test(migrations = "./migrations")]
async fn kat_paused_while_queued_is_not_run(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
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
    assert!(claim(&core, "a").await.is_none());
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "failed");
    assert_eq!(
        outcome(&pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("kat_paused".into()))
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn grant_revoked_while_queued_is_not_run(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
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
    assert!(claim(&core, "a").await.is_none());
    assert_eq!(task_state(&pool, TaskId(f.task)).await, "failed");
    assert_eq!(
        outcome(&pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("policy_changed".into()))
    );
}
#[sqlx::test(migrations = "./migrations")]
async fn infrastructure_abort_goes_through_reclaim(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    let run = start_run(&core, &a).await;
    lease::abandon(&core, a.task_id, "a", a.lease_epoch)
        .await
        .unwrap();
    assert_eq!(outcome(&pool, a.job_id).await, (None, None));
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
    let b = claim(&core, "b").await.unwrap();
    assert_eq!(b.kind, RunKind::Recovery);
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
}
#[test]
fn run_deadline_is_claimed_at_plus_remaining() {
    let now = Instant::now();
    assert_eq!(
        worker::run_deadline(now, Duration::from_secs(10)),
        now + Duration::from_secs(10)
    );
    assert_eq!(worker::run_deadline(now, Duration::ZERO), now);
}
#[sqlx::test(migrations = "./migrations")]
async fn deadline_comes_from_database_time(pool: PgPool) {
    setup(&pool).await;
    let core = core(&pool);
    let a = claim(&core, "a").await.unwrap();
    expire(&pool, a.task_id).await;
    sqlx::query(
        "UPDATE tasks SET first_claimed_at=clock_timestamp()-interval '290 seconds' WHERE id=$1",
    )
    .bind(a.task_id)
    .execute(&pool)
    .await
    .unwrap();
    let b = claim(&core, "b").await.unwrap();
    assert!(b.remaining > Duration::from_secs(9) && b.remaining <= Duration::from_secs(10));
}
#[sqlx::test(migrations = "./migrations")]
async fn task_deadline_starts_at_first_claim(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    sqlx::query("UPDATE tasks SET created_at=clock_timestamp()-interval '3 minutes' WHERE id=$1")
        .bind(f.task)
        .execute(&pool)
        .await
        .unwrap();
    let a = claim(&core, "a").await.unwrap();
    assert!(a.remaining > Duration::from_secs(299) && a.remaining <= Duration::from_secs(300));
    let first: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT first_claimed_at FROM tasks WHERE id=$1")
            .bind(a.task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    expire(&pool, a.task_id).await;
    claim(&core, "b").await.unwrap();
    let next: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT first_claimed_at FROM tasks WHERE id=$1")
            .bind(a.task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(first, next);
}

#[path = "leases/worker.rs"]
mod worker_tests;

#[path = "leases/races.rs"]
mod races;
