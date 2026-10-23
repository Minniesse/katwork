use super::*;
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn registry(provider: Arc<dyn ModelProvider>) -> Registry {
    Registry::from_entries(vec![(
        ModelEntry {
            provider_id: provider.id().into(),
            model: "test".into(),
            tier: katwork::types::ModelTier::Standard,
            capabilities: provider.capabilities(),
            max_output_tokens: 4096,
            temperature: None,
            price: None,
            timeouts: ModelTimeouts {
                first_chunk: Duration::from_secs(600),
                idle: Duration::from_secs(600),
                run_wall_clock: Duration::from_secs(600),
            },
        },
        provider,
    )])
}
fn answer() -> FakeTurn {
    FakeTurn::Chunks(vec![
        ChatChunk::TextDelta("One reply".into()),
        ChatChunk::Done {
            stop: StopKind::EndTurn,
            turn: AssistantTurn {
                content: "One reply".into(),
                tool_calls: vec![],
                replay: None,
            },
        },
    ])
}
async fn execute(core: &Core, c: &ClaimedTask, remaining: Duration) -> Result<RunFacts, RunAbort> {
    let tenant = tenancy::resolve(&core.pool, c.requested_by, c.workspace_id)
        .await
        .unwrap();
    let kat = kat::get(&core.pool, &tenant, c.kat_id).await.unwrap();
    let thread = chat::thread::get(&core.pool, &tenant, c.thread_id)
        .await
        .unwrap();
    let run = RunId::new();
    let sink = sink(core, c, run);
    let tools = ToolRegistry::builtin();
    run::execute(
        &RunEnv {
            pool: &core.pool,
            models: &core.registry,
            tools: &tools,
            sink: &sink,
        },
        &kat,
        RunInput {
            tenant,
            run_id: run,
            thread_id: c.thread_id,
            space_id: thread.space_id,
            job_id: c.job_id,
            task_id: c.task_id,
            lease_epoch: c.lease_epoch,
            kind: c.kind,
            space_policy: SpacePolicy {
                allow_external: false,
            },
            history: vec![],
            reply_message_id: MessageId::new(),
            policy_version: c.policy_version,
            actor: c.requested_by,
            initiator: c.input.initiator,
            deadline: worker::run_deadline(Instant::now(), remaining),
            cancel: CancellationToken::new(),
        },
        &Limits::default(),
    )
    .await
}
// Hold virtual time still while PostgreSQL I/O is pending.
struct Clock(tokio::task::JoinHandle<()>);
impl Clock {
    fn pause() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition never reached");
}
async fn finished(pool: &PgPool, job: JobId) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if outcome(pool, job).await.0.is_some() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("job never finished");
}
struct Hanging {
    started: AtomicUsize,
    dropped: Arc<AtomicBool>,
}
struct DropSignal(Arc<AtomicBool>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl ModelProvider for Hanging {
    fn id(&self) -> &str {
        "hanging"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::Local,
            synthetic: false,
        }
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let guard = DropSignal(self.dropped.clone());
        Ok(async_stream::stream! {let _guard=guard;yield Ok(ChatChunk::TextDelta("partial".into()));std::future::pending::<()>().await;}.boxed())
    }
}
fn hanging(pool: &PgPool) -> (Arc<Core>, Arc<Hanging>) {
    let provider = Arc::new(Hanging {
        started: AtomicUsize::new(0),
        dropped: Arc::default(),
    });
    (
        Arc::new(core(pool).with_registry(registry(provider.clone()))),
        provider,
    )
}
#[sqlx::test(migrations = "./migrations")]
async fn worker_killed_between_execute_and_complete_closes_run_once(pool: PgPool) {
    setup(&pool).await;
    let fake = FakeProvider::scripted(vec![answer(), answer()]);
    let core = core(&pool).with_registry(registry(Arc::new(fake.clone())));
    let a = claim(&core, "a").await.unwrap();
    let facts = execute(&core, &a, a.remaining).await.unwrap();
    let old = facts.run_id.unwrap();
    assert!(terminals(&pool, old).await.is_empty());
    drop(facts);
    expire(&pool, a.task_id).await;
    let b = claim(&core, "b").await.unwrap();
    let facts = execute(&core, &b, b.remaining).await.unwrap();
    let new = facts.run_id.unwrap();
    lease::complete(&core, b.task_id, "b", b.lease_epoch, facts)
        .await
        .unwrap();
    assert!(claim(&core, "c").await.is_none());
    assert_eq!(terminals(&pool, old).await, ["RUN_ERROR"]);
    assert_eq!(terminals(&pool, new).await, ["RUN_FINISHED"]);
    assert_eq!(fake.received().len(), 2);
}
#[sqlx::test(migrations = "./migrations")]
async fn zero_remaining_makes_no_model_call(pool: PgPool) {
    setup(&pool).await;
    let fake = FakeProvider::scripted(vec![answer()]);
    let core = core(&pool).with_registry(registry(Arc::new(fake.clone())));
    let c = claim(&core, "a").await.unwrap();
    let facts = execute(&core, &c, Duration::ZERO).await.unwrap();
    assert_eq!(facts.stop, Some(StopReason::TimeLimit));
    assert!(fake.received().is_empty());
}
#[sqlx::test(migrations = "./migrations")]
async fn remaining_micros_from_sql_for_future_and_expired_deadlines(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let limits = Limits {
        task_deadline: Duration::from_secs(10),
        ..Limits::default()
    };
    let c = lease::claim(&core, "a", TTL, &limits)
        .await
        .unwrap()
        .unwrap();
    assert!(c.remaining > Duration::from_secs(9) && c.remaining <= Duration::from_secs(10));
    sqlx::query(
        "UPDATE tasks SET first_claimed_at=clock_timestamp()-interval '11 seconds' WHERE id=$1",
    )
    .bind(f.task)
    .execute(&pool)
    .await
    .unwrap();
    // Exercise the exact production SQL with an expired deadline, including its signed decode.
    let micros: i64 = sqlx::query_scalar(include_str!("../../src/jobs/remaining.sql"))
        .bind(10_000_000_i64)
        .bind(600_000_000_i64)
        .bind(c.workspace_id)
        .bind(c.task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(micros, 0);
}
#[sqlx::test(migrations = "./migrations")]
async fn worker_cancels_run_when_renewal_fails(pool: PgPool) {
    let f = setup(&pool).await;
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let (core, provider) = hanging(&single);
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    until(|| provider.started.load(Ordering::SeqCst) == 1).await;
    // Exhaust a one-connection pool: renewal cannot hit a database lock timeout.
    let held = core.pool.acquire().await.unwrap();
    tokio::time::advance(Duration::from_millis(20_001)).await;
    tokio::time::advance(Duration::from_secs(40)).await;
    until(|| provider.dropped.load(Ordering::SeqCst)).await;
    let live: bool =
        sqlx::query_scalar("SELECT expires_at>clock_timestamp() FROM task_leases WHERE task_id=$1")
            .bind(f.task)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(live, "watchdog must cancel before database expiry");
    drop(held);
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
    assert_eq!(outcome(&pool, JobId(f.job)).await, (None, None));
}
async fn heartbeat_pause(pool: &PgPool) -> (String, Vec<String>) {
    let f = setup(pool).await;
    let (core, provider) = hanging(pool);
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    until(|| provider.started.load(Ordering::SeqCst) == 1).await;
    paused(pool, &f, true).await;
    tokio::time::advance(Duration::from_millis(20_001)).await;
    finished(pool, JobId(f.job)).await;
    let run: RunId = sqlx::query_scalar("SELECT id FROM runs WHERE task_id=$1 AND lease_epoch>0")
        .bind(f.task)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        outcome(pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("workspace_paused".into()))
    );
    assert!(provider.dropped.load(Ordering::SeqCst));
    let ev = events(pool, ThreadId(f.thread)).await;
    assert_eq!(
        ev.iter().find(|e| e["type"] == "RUN_ERROR").unwrap()["code"],
        "cancelled"
    );
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
    (
        task_state(pool, TaskId(f.task)).await,
        terminals(pool, run).await,
    )
}
#[sqlx::test(migrations = "./migrations")]
async fn heartbeat_pause_has_one_terminal_run_event(pool: PgPool) {
    assert_eq!(heartbeat_pause(&pool).await.1, ["RUN_ERROR"]);
}
#[sqlx::test(migrations = "./migrations")]
async fn pausing_workspace_interrupts_running_task(pool: PgPool) {
    assert_eq!(heartbeat_pause(&pool).await.0, "interrupted");
}

struct PauseAfterTurn {
    pool: PgPool,
    workspace: WorkspaceId,
    calls: AtomicUsize,
    text: bool,
}
#[async_trait]
impl ModelProvider for PauseAfterTurn {
    fn id(&self) -> &str {
        "pausing"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::Local,
            synthetic: false,
        }
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let pool = self.pool.clone();
        let workspace = self.workspace;
        let text = self.text;
        Ok(async_stream::stream! {
            sqlx::query("UPDATE workspaces SET paused=true WHERE id=$1").bind(workspace).execute(&pool).await.unwrap();
            if text { yield Ok(ChatChunk::TextDelta("x".repeat(600))); }
            yield Err(ModelError::ProviderUnavailable);
        }.boxed())
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn pause_between_attempts_before_heartbeat_has_one_terminal_run_event(pool: PgPool) {
    let f = setup(&pool).await;
    let provider = Arc::new(PauseAfterTurn {
        pool: pool.clone(),
        workspace: WorkspaceId(f.workspace),
        calls: AtomicUsize::new(0),
        text: false,
    });
    let core = Arc::new(core(&pool).with_registry(registry(provider.clone())));
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    finished(&pool, JobId(f.job)).await;
    let run: RunId = sqlx::query_scalar("SELECT id FROM runs WHERE task_id=$1 AND lease_epoch>0")
        .bind(f.task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(terminals(&pool, run).await, ["RUN_FINISHED"]);
    assert_eq!(
        outcome(&pool, JobId(f.job)).await,
        (Some("stopped".into()), Some("workspace_paused".into()))
    );
    let ev = events(&pool, ThreadId(f.thread)).await;
    let status = ev
        .iter()
        .position(|e| {
            e["name"] == "katwork.job_status" && e["value"]["reason"] == "workspace_paused"
        })
        .unwrap();
    let end = ev.iter().position(|e| e["type"] == "RUN_FINISHED").unwrap();
    assert!(status < end);
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
}
#[sqlx::test(migrations = "./migrations")]
async fn lease_expiring_mid_run_drops_stale_worker_facts(pool: PgPool) {
    let f = setup(&pool).await;
    let (core, provider) = hanging(&pool);
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    until(|| provider.started.load(Ordering::SeqCst) == 1).await;
    expire(&pool, TaskId(f.task)).await;
    paused(&pool, &f, true).await;
    assert!(claim(&core, "recovery").await.is_none());
    tokio::time::advance(Duration::from_millis(20_001)).await;
    until(|| provider.dropped.load(Ordering::SeqCst)).await;
    assert_eq!(outcome(&pool, JobId(f.job)).await, (None, None));
    let run: RunId = sqlx::query_scalar("SELECT id FROM runs WHERE task_id=$1 AND lease_epoch>0")
        .bind(f.task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
}
#[sqlx::test(migrations = "./migrations")]
async fn shutdown_abandons_active_work_for_restart(pool: PgPool) {
    let f = setup(&pool).await;
    let (core, provider) = hanging(&pool);
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 2, TTL);
    until(|| provider.started.load(Ordering::SeqCst) == 1).await;
    core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    assert!(provider.dropped.load(Ordering::SeqCst));
    assert_eq!(outcome(&pool, JobId(f.job)).await, (None, None));
    let restarted = super::core(&pool);
    let c = claim(&restarted, "restart").await.unwrap();
    assert_eq!(c.kind, RunKind::Recovery);
    finish(&restarted, &c, "restart").await;
    assert_eq!(
        outcome(&pool, c.job_id).await.0.as_deref(),
        Some("answered")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn pool_drains_ready_tasks_without_idle_delay(pool: PgPool) {
    let f = setup(&pool).await;
    let next = enqueue(&pool, &f, KatId(f.kat)).await;
    let next_job: JobId = sqlx::query_scalar("SELECT job_id FROM tasks WHERE id=$1")
        .bind(next)
        .fetch_one(&pool)
        .await
        .unwrap();
    let fake = FakeProvider::scripted(vec![answer(), answer()]);
    let core = Arc::new(core(&pool).with_registry(registry(Arc::new(fake.clone()))));
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    finished(&pool, next_job).await;
    assert_eq!(fake.received().len(), 2);
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
}

#[sqlx::test(migrations = "./migrations")]
async fn infrastructure_failure_in_worker_gets_no_policy_verdict(pool: PgPool) {
    let f = setup(&pool).await;
    sqlx::query("ALTER TABLE events ADD CONSTRAINT fail_text CHECK (kind<>'TEXT_MESSAGE_CONTENT')")
        .execute(&pool)
        .await
        .unwrap();
    // Pause before the failed event so recovery reaches a stable hold, never a polling race.
    let provider = Arc::new(PauseAfterTurn {
        pool: pool.clone(),
        workspace: WorkspaceId(f.workspace),
        calls: AtomicUsize::new(0),
        text: true,
    });
    let core = Arc::new(core(&pool).with_registry(registry(provider.clone())));
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let held:bool=sqlx::query_scalar("SELECT state='queued' AND hold_reason IS NOT DISTINCT FROM 'workspace_paused' FROM tasks WHERE id=$1")
            .bind(f.task).fetch_one(&pool).await.unwrap();
        if held {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker never recovered into hold"
        );
        tokio::task::yield_now().await;
    }
    core.shutdown.cancel();
    while workers.join_next().await.is_some() {}
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(outcome(&pool, JobId(f.job)).await, (None, None));
    let run: RunId = sqlx::query_scalar("SELECT id FROM runs WHERE task_id=$1 AND lease_epoch>0")
        .bind(f.task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
    sqlx::query("ALTER TABLE events DROP CONSTRAINT fail_text")
        .execute(&pool)
        .await
        .unwrap();
    paused(&pool, &f, false).await;
    let c = claim(&core, "restarted").await.unwrap();
    assert_eq!(c.kind, RunKind::Recovery);
    finish(&core, &c, "restarted").await;
}

struct Panicking;
#[async_trait]
impl ModelProvider for Panicking {
    fn id(&self) -> &str {
        "panicking"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::Local,
            synthetic: false,
        }
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        panic!("injected provider panic")
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn panicking_tasks_finish_and_cannot_wipe_out_pool(pool: PgPool) {
    let f = setup(&pool).await;
    let other = setup(&pool).await;
    let core = Arc::new(core(&pool).with_registry(registry(Arc::new(Panicking))));
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    finished(&pool, JobId(other.job)).await;
    for f in [f, other] {
        assert_eq!(
            outcome(&pool, JobId(f.job)).await,
            (Some("error".into()), Some("internal_error".into()))
        );
        assert_eq!(task_state(&pool, TaskId(f.task)).await, "failed");
        let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM task_leases WHERE task_id=$1")
            .bind(f.task)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(leases, 0);
        let run: RunId =
            sqlx::query_scalar("SELECT id FROM runs WHERE task_id=$1 AND lease_epoch>0")
                .bind(f.task)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(terminals(&pool, run).await, ["RUN_ERROR"]);
        assert!(
            events(&pool, ThreadId(f.thread))
                .await
                .iter()
                .all(|e| e["name"] != "katwork.decision")
        );
    }
    assert_eq!(workers.len(), 1);
    core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn removed_requester_worker_still_answers(pool: PgPool) {
    let f = setup(&pool).await;
    sqlx::query("DELETE FROM memberships WHERE workspace_id=$1 AND user_id=$2")
        .bind(f.workspace)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap();
    let core = Arc::new(
        core(&pool).with_registry(registry(Arc::new(FakeProvider::scripted(vec![answer()])))),
    );
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    finished(&pool, JobId(f.job)).await;
    core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    assert_eq!(
        outcome(&pool, JobId(f.job)).await.0.as_deref(),
        Some("answered")
    );
}

struct ShutdownAnswer(CancellationToken);
#[async_trait]
impl ModelProvider for ShutdownAnswer {
    fn id(&self) -> &str {
        "shutdown_answer"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::Local,
            synthetic: false,
        }
    }
    async fn stream(
        &self,
        _: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        let shutdown = self.0.clone();
        Ok(futures::stream::once(async move {
            shutdown.cancel();
            Ok(ChatChunk::Done {
                stop: StopKind::EndTurn,
                turn: AssistantTurn {
                    content: "Finished answer".into(),
                    tool_calls: vec![],
                    replay: None,
                },
            })
        })
        .boxed())
    }
}
#[sqlx::test(migrations = "./migrations")]
async fn shutdown_preserves_finished_answer(pool: PgPool) {
    let f = setup(&pool).await;
    let core = core(&pool);
    let provider = Arc::new(ShutdownAnswer(core.shutdown.clone()));
    let core = Arc::new(core.with_registry(registry(provider)));
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    timeout(Duration::from_secs(10), async {
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        outcome(&pool, JobId(f.job)).await.0.as_deref(),
        Some("answered")
    );
    let replies: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE job_id=$1 AND author_kind='kat'")
            .bind(f.job)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(replies, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn shutdown_drains_claim_before_releasing_lease(pool: PgPool) {
    let f = setup(&pool).await;
    let mut core = core(&pool);
    Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(30);
    let core = Arc::new(core);
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    super::races::waiting(&pool, "SELECT id FROM threads WHERE workspace_id").await;
    core.shutdown.cancel();
    blocker.rollback().await.unwrap();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    let (attempts, live): (i32, bool) = sqlx::query_as("SELECT t.attempts,COALESCE(l.expires_at>clock_timestamp(),false) FROM tasks t LEFT JOIN task_leases l ON l.task_id=t.id WHERE t.id=$1")
        .bind(f.task).fetch_one(&pool).await.unwrap();
    assert_eq!(attempts, 1, "shutdown dropped the in-flight claim");
    assert!(!live, "shutdown stranded the committed claim");
}

#[sqlx::test(migrations = "./migrations")]
async fn first_watchdog_window_includes_claim_latency(pool: PgPool) {
    let f = setup(&pool).await;
    let fake = FakeProvider::scripted(vec![answer()]);
    let mut core = core(&pool).with_registry(registry(Arc::new(fake.clone())));
    Arc::get_mut(&mut core.config).unwrap().lock_timeout = Duration::from_secs(30);
    let core = Arc::new(core);
    let _clock = Clock::pause();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    super::races::waiting(&pool, "SELECT id FROM threads WHERE workspace_id").await;
    tokio::time::advance(TTL + Duration::from_secs(1)).await;
    gate.rollback().await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let invalidated: bool = sqlx::query_scalar("SELECT lease_epoch>1 FROM tasks WHERE id=$1")
            .bind(f.task)
            .fetch_one(&pool)
            .await
            .unwrap();
        if invalidated {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "first watchdog window excluded claim latency"
        );
        tokio::task::yield_now().await;
    }
    core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    assert!(fake.received().is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn failed_heartbeat_abandons_without_waiting_for_ttl(pool: PgPool) {
    let f = setup(&pool).await;
    let (core, provider) = hanging(&pool);
    let _clock = Clock::pause();
    let mut workers = worker::spawn_pool(core.clone(), 1, TTL);
    until(|| provider.started.load(Ordering::SeqCst) == 1).await;
    sqlx::raw_sql("CREATE FUNCTION reject_renewal() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.expires_at>OLD.expires_at THEN RAISE EXCEPTION 'injected renewal failure'; END IF;
            RETURN NEW;
        END $$;
        CREATE TRIGGER reject_renewal BEFORE UPDATE ON task_leases FOR EACH ROW EXECUTE FUNCTION reject_renewal()")
        .execute(&pool).await.unwrap();
    paused(&pool, &f, true).await;
    tokio::time::advance(Duration::from_millis(20_001)).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let recovered: bool = sqlx::query_scalar("SELECT state='queued' AND hold_reason IS NOT DISTINCT FROM 'workspace_paused' FROM tasks WHERE id=$1")
            .bind(f.task).fetch_one(&pool).await.unwrap();
        if recovered {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "failed heartbeat did not abandon promptly"
        );
        tokio::task::yield_now().await;
    }
    assert!(provider.dropped.load(Ordering::SeqCst));
    assert_eq!(outcome(&pool, JobId(f.job)).await, (None, None));
    core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
}
