#![allow(dead_code)]
mod support;

use async_trait::async_trait;
use futures::{FutureExt, StreamExt, future::BoxFuture, stream::BoxStream};
use katwork::{
    agent::{guards, run, *},
    error::KatError,
    events::{AgUiEvent, EventHub, EventSink, PgEventSink, RunEnd},
    fence::Fence,
    ids::*,
    models::*,
    team::kat::{self, Kat, KatPatch},
    tenancy,
    types::{KatStatus, OutwardKind, SideEffect, SpacePolicy},
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::Fixture;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

type Action =
    dyn Fn(ToolCtx, Value) -> BoxFuture<'static, Result<ToolOutput, ToolError>> + Send + Sync;
struct Probe {
    name: String,
    effect: SideEffect,
    calls: Arc<AtomicUsize>,
    action: Arc<Action>,
}
impl Probe {
    fn new(name: &str, effect: SideEffect) -> Self {
        Self {
            name: name.into(),
            effect,
            calls: Arc::default(),
            action: Arc::new(|_, _| async { Ok(ToolOutput::Text("result".into())) }.boxed()),
        }
    }
    fn action(
        mut self,
        action: impl Fn(ToolCtx, Value) -> BoxFuture<'static, Result<ToolOutput, ToolError>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.action = Arc::new(action);
        self
    }
}
#[async_trait]
impl Tool for Probe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "Test tool".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn side_effect(&self) -> SideEffect {
        self.effect
    }
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.action)(ctx.clone(), args).await
    }
}

struct Sink {
    pg: PgEventSink,
    events: Mutex<Vec<AgUiEvent>>,
    fail: AtomicUsize,
}
#[async_trait]
impl EventSink for Sink {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        if self.fail.load(Ordering::SeqCst) == 1 {
            return Err(KatError::Busy);
        }
        let seq = self.pg.emit(event.clone()).await?;
        self.events.lock().unwrap().push(event);
        Ok(seq)
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.pg.close_run(end, code).await
    }
}
struct Harness {
    pool: PgPool,
    f: Fixture,
    kat: Kat,
    input: RunInput,
    models: Registry,
    fake: FakeProvider,
    tools: ToolRegistry,
    sink: Sink,
    limits: Limits,
}
fn entry(provider: &dyn ModelProvider) -> ModelEntry {
    ModelEntry {
        provider_id: provider.id().into(),
        model: "script".into(),
        tier: katwork::types::ModelTier::Standard,
        capabilities: provider.capabilities(),
        max_output_tokens: 4096,
        temperature: Some(0.2),
        price: None,
        timeouts: ModelTimeouts::HOSTED,
    }
}
impl Harness {
    async fn new(pool: PgPool, script: Vec<FakeTurn>) -> Self {
        let f = Fixture::new(&pool).await;
        f.live_lease(&pool).await;
        let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
            .await
            .unwrap();
        sqlx::query("UPDATE kats SET tool_allowlist=$2, appearance='{\"avatar\":\"kat\",\"accent\":\"blue\"}' WHERE id=$1")
            .bind(f.kat)
            .bind(vec!["read_thread", "ask_user", "probe", "other", "write"])
            .execute(&pool)
            .await
            .unwrap();
        let kat = kat::get(&pool, &tenant, KatId(f.kat)).await.unwrap();
        let input = RunInput {
            tenant,
            run_id: RunId::new(),
            thread_id: ThreadId(f.thread),
            space_id: SpaceId(f.space),
            job_id: JobId(f.job),
            task_id: TaskId(f.task),
            lease_epoch: LeaseEpoch(1),
            kind: RunKind::Initial,
            space_policy: SpacePolicy {
                allow_external: false,
            },
            history: vec![ChatMsg::User {
                content: "Hello".into(),
                name: None,
            }],
            reply_message_id: MessageId::new(),
            policy_version: kat.policy_version,
            actor: UserId(f.user),
            initiator: Initiator::Person,
            deadline: tokio::time::Instant::now() + Duration::from_secs(600),
            cancel: CancellationToken::new(),
        };
        let pg = PgEventSink::new(
            pool.clone(),
            EventHub::default(),
            Duration::from_secs(2),
            WorkspaceId(f.workspace),
            ThreadId(f.thread),
            Some(input.run_id),
            Some(Fence {
                task_id: input.task_id,
                epoch: input.lease_epoch,
            }),
        );
        let fake = FakeProvider::scripted(script);
        let models = Registry::from_entries(vec![(entry(&fake), Arc::new(fake.clone()))]);
        Self {
            pool,
            f,
            kat,
            input,
            models,
            fake,
            tools: ToolRegistry::builtin(),
            sink: Sink {
                pg,
                events: Mutex::default(),
                fail: AtomicUsize::new(0),
            },
            limits: Limits::default(),
        }
    }
    fn add(&mut self, probe: Probe) -> Arc<AtomicUsize> {
        let count = probe.calls.clone();
        self.tools.register(Arc::new(probe)).unwrap();
        count
    }
    async fn run(&self) -> Result<RunFacts, RunAbort> {
        run::execute(
            &RunEnv {
                pool: &self.pool,
                models: &self.models,
                tools: &self.tools,
                sink: &self.sink,
            },
            &self.kat,
            self.input.clone(),
            &self.limits,
        )
        .await
    }
    async fn rows(&self) -> Vec<sqlx::postgres::PgRow> {
        sqlx::query("SELECT * FROM tool_calls WHERE run_id=$1 ORDER BY id")
            .bind(self.input.run_id)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
    async fn usage(&self) -> Vec<sqlx::postgres::PgRow> {
        sqlx::query("SELECT * FROM model_calls WHERE run_id=$1 ORDER BY attempt")
            .bind(self.input.run_id)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
    async fn effects(&self) -> bool {
        sqlx::query_scalar("SELECT effects_started_at IS NOT NULL FROM tasks WHERE id=$1")
            .bind(self.input.task_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}
fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args_json: args.to_string(),
    }
}
fn turn(text: &str, calls: Vec<ToolCall>) -> FakeTurn {
    ending(text, calls, StopKind::EndTurn)
}
fn ending(text: &str, calls: Vec<ToolCall>, stop: StopKind) -> FakeTurn {
    FakeTurn::Chunks(vec![
        ChatChunk::TextDelta(text.into()),
        ChatChunk::Usage(Usage {
            input: Some(10),
            output: Some(10),
            complete: true,
            ..Usage::default()
        }),
        ChatChunk::Done {
            stop,
            turn: AssistantTurn {
                content: text.into(),
                tool_calls: calls,
                replay: None,
            },
        },
    ])
}
fn tool_results(fake: &FakeProvider) -> Vec<(String, String, String, bool)> {
    fake.received()
        .last()
        .unwrap()
        .messages
        .iter()
        .filter_map(|m| match m {
            ChatMsg::Tool {
                tool_call_id,
                name,
                content,
                is_error,
            } => Some((
                tool_call_id.clone(),
                name.clone(),
                content.clone(),
                *is_error,
            )),
            _ => None,
        })
        .collect()
}

#[sqlx::test]
async fn plain_text_reply_sets_final_text(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("Hello Kat", vec![])]).await;
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("Hello Kat"));
    assert_eq!(facts.stop, None);
    let state: String = sqlx::query_scalar("SELECT state FROM runs WHERE id=$1")
        .bind(h.input.run_id)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(state, "running");
    assert!(!facts.run_closed);
}
#[sqlx::test]
async fn tool_call_then_reply(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("Done", vec![]),
        ],
    )
    .await;
    let n = h.add(Probe::new("probe", SideEffect::Read));
    let facts = h.run().await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert_eq!(facts.final_text.as_deref(), Some("Done"));
    assert_eq!(
        tool_results(&h.fake),
        vec![("a".into(), "probe".into(), "result".into(), false)]
    );
}
#[sqlx::test]
async fn unknown_tool_id_returns_error_result_and_continues(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "unregistered", json!({}))]),
            turn("Done", vec![]),
        ],
    )
    .await;
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("Done"));
    assert_eq!(
        tool_results(&h.fake)[0],
        (
            "a".into(),
            "unregistered".into(),
            "not_allowed".into(),
            true
        )
    );
}
#[sqlx::test]
async fn registered_tool_outside_kat_allowlist_is_refused(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "forbidden", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let n = h.add(Probe::new("forbidden", SideEffect::Read));
    let facts = h.run().await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 0);
    assert_eq!(facts.tool_calls[0].state, ToolCallState::Refused);
    assert!(tool_results(&h.fake)[0].3);
}
#[sqlx::test]
async fn destructive_and_outward_tools_refused_before_dispatch(pool: PgPool) {
    for effect in [
        SideEffect::Destructive,
        SideEffect::Outward(OutwardKind::Send),
    ] {
        let mut h = Harness::new(
            pool.clone(),
            vec![turn("", vec![call("approval", "probe", json!({"x":1}))])],
        )
        .await;
        let n = h.add(Probe::new("probe", effect));
        let facts = h.run().await.unwrap();
        assert_eq!(facts.stop, Some(StopReason::NeedsApproval));
        assert_eq!(n.load(Ordering::SeqCst), 0);
        let pending = facts.pending_tool_call.unwrap();
        assert_eq!(pending.tool_call_id, "approval");
        assert_eq!(pending.args_canonical, "{\"x\":1}");
        assert_eq!(pending.args_sha256.len(), 64);
    }
}
#[sqlx::test]
async fn navigate_exec_and_credential_tools_are_not_allowed(pool: PgPool) {
    for effect in [
        SideEffect::Navigate,
        SideEffect::Exec,
        SideEffect::CredentialUse,
    ] {
        let mut h = Harness::new(
            pool.clone(),
            vec![
                turn("", vec![call("a", "probe", json!({}))]),
                turn("ok", vec![]),
            ],
        )
        .await;
        let n = h.add(Probe::new("probe", effect));
        h.run().await.unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert!(tool_results(&h.fake)[0].3);
    }
}
#[sqlx::test]
async fn no_tool_runs_from_a_max_tokens_turn(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![ending(
            "partial",
            vec![call("a", "probe", json!({}))],
            StopKind::MaxTokens,
        )],
    )
    .await;
    let n = h.add(Probe::new("probe", SideEffect::Read));
    let facts = h.run().await.unwrap();
    assert_eq!(facts.stop, Some(StopReason::OutputTruncated));
    assert_eq!(facts.final_text.as_deref(), Some("partial"));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
#[sqlx::test]
async fn no_tool_runs_from_a_failed_turn(pool: PgPool) {
    for stop in [StopKind::Refusal, StopKind::Other("untrusted".into())] {
        let mut h = Harness::new(
            pool.clone(),
            vec![ending("", vec![call("a", "probe", json!({}))], stop)],
        )
        .await;
        let n = h.add(Probe::new("probe", SideEffect::Read));
        assert_eq!(h.run().await.unwrap().stop, Some(StopReason::ModelProtocol));
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }
}
async fn repeats(pool: PgPool, effect: SideEffect, alternating: bool) {
    let script = (0..12)
        .map(|i| {
            turn(
                "",
                vec![call(
                    &i.to_string(),
                    "probe",
                    if alternating {
                        json!({"x":i%2})
                    } else if i % 2 == 0 {
                        json!({"b":1,"a":2})
                    } else {
                        json!({"a":2,"b":1})
                    },
                )],
            )
        })
        .collect();
    let mut h = Harness::new(pool, script).await;
    let n = h.add(Probe::new("probe", effect));
    let facts = h.run().await.unwrap();
    assert_eq!(facts.stop, Some(StopReason::LoopDetected));
    assert_eq!(n.load(Ordering::SeqCst), if alternating { 8 } else { 2 });
}
#[sqlx::test]
async fn same_tool_same_args_three_times_stops_loop_detected(pool: PgPool) {
    repeats(pool, SideEffect::Read, false).await;
}
#[sqlx::test]
async fn alternating_two_calls_five_times_in_ten_stops(pool: PgPool) {
    repeats(pool, SideEffect::Read, true).await;
}

#[sqlx::test]
async fn same_mutation_with_new_call_id_counts_as_repeat(pool: PgPool) {
    repeats(pool, SideEffect::WorkspaceWrite, false).await;
}
#[test]
fn canonical_args_do_not_depend_on_key_order() {
    let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
    assert_eq!(guards::canonical_args(&a), guards::canonical_args(&b));
    assert_eq!(guards::canonical_args(&a), r#"{"a":{"c":3,"d":2},"b":1}"#);
    use sha2::{Digest, Sha256};
    assert_eq!(
        Sha256::digest(guards::canonical_args(&a)),
        Sha256::digest(guards::canonical_args(&b))
    );
}
#[test]
fn duplicate_tool_name_is_rejected() {
    let mut r = ToolRegistry::new();
    let first: Arc<dyn Tool> = Arc::new(Probe::new("probe", SideEffect::Read));
    r.register(first.clone()).unwrap();
    let err = r
        .register(Arc::new(Probe::new("probe", SideEffect::Destructive)))
        .unwrap_err();
    assert_eq!(err.name, "probe");
    assert!(Arc::ptr_eq(&r.get("probe").unwrap(), &first));
    let builtins = ToolRegistry::builtin();
    assert_eq!(builtins.get("ask_user").unwrap().spec().name, "ask_user");
    assert_eq!(
        builtins.get("read_thread").unwrap().spec().name,
        "read_thread"
    );
}
#[sqlx::test]
async fn loop_guard_refusal_pairs_every_tool_call(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("first", "probe", json!({}))]),
            turn(
                "",
                vec![
                    call("second", "probe", json!({})),
                    call("third", "probe", json!({})),
                    call("last", "other", json!({})),
                ],
            ),
        ],
    )
    .await;
    let n = h.add(Probe::new("probe", SideEffect::Read));
    let other = h.add(Probe::new("other", SideEffect::Read));
    assert_eq!(h.run().await.unwrap().stop, Some(StopReason::LoopDetected));
    assert_eq!(n.load(Ordering::SeqCst), 2);
    assert_eq!(other.load(Ordering::SeqCst), 0);
    let rows = h.rows().await;
    assert_eq!(rows.len(), 4);
    for (row, id, name) in [(&rows[2], "third", "probe"), (&rows[3], "last", "other")] {
        assert_eq!(row.get::<String, _>("tool_call_id"), id);
        assert_eq!(row.get::<String, _>("tool_name"), name);
        assert_eq!(row.get::<String, _>("state"), "refused");
        assert_eq!(row.get::<String, _>("result"), "loop_detected");
    }
}
#[sqlx::test]
async fn malformed_or_oversized_arguments_never_dispatch(pool: PgPool) {
    for args in [
        "{".to_string(),
        format!("{{\"x\":\"{}\"}}", "x".repeat(65537)),
        "null".into(),
    ] {
        let mut h = Harness::new(
            pool.clone(),
            vec![
                turn(
                    "",
                    vec![ToolCall {
                        id: "bad".into(),
                        name: "probe".into(),
                        args_json: args,
                    }],
                ),
                turn("ok", vec![]),
            ],
        )
        .await;
        let n = h.add(Probe::new("probe", SideEffect::Read));
        h.run().await.unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert!(tool_results(&h.fake)[0].3);
    }
}
#[sqlx::test]
async fn nul_in_tool_name_or_result_is_stored_replaced(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn(
                "",
                vec![
                    call("a", "bad\0name", json!({})),
                    call("b", "probe", json!({})),
                ],
            ),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(
        Probe::new("probe", SideEffect::Read)
            .action(|_, _| async { Ok(ToolOutput::Text("a\0b".into())) }.boxed()),
    );
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(rows[0].get::<String, _>("tool_name"), "bad�name");
    assert_eq!(rows[1].get::<String, _>("result"), "a�b");
}
#[sqlx::test]
async fn answer_on_step_24_accepted_tool_batch_needing_25_stops(pool: PgPool) {
    for tools_on_last in [false, true] {
        let mut script: Vec<_> = (0..23)
            .map(|i| turn("", vec![call("call", "probe", json!({"i":i}))]))
            .collect();
        script.push(turn(
            "answer",
            if tools_on_last {
                vec![call("last", "probe", json!({"i":24}))]
            } else {
                vec![]
            },
        ));
        let mut h = Harness::new(pool.clone(), script).await;
        let n = h.add(Probe::new("probe", SideEffect::Read));
        let facts = h.run().await.unwrap();
        assert_eq!(h.fake.received().len(), 24);
        assert_eq!(n.load(Ordering::SeqCst), 23);
        assert_eq!(facts.stop, tools_on_last.then_some(StopReason::StepLimit));
        if !tools_on_last {
            assert_eq!(facts.final_text.as_deref(), Some("answer"));
        }
    }
}
#[sqlx::test]
async fn token_budget_200k_stops_before_next_attempt(pool: PgPool) {
    let mut t = match turn("", vec![call("a", "probe", json!({}))]) {
        FakeTurn::Chunks(c) => c,
        _ => unreachable!(),
    };
    t.insert(
        2,
        ChatChunk::Usage(Usage {
            input: Some(199_999),
            output: Some(1),
            complete: true,
            ..Usage::default()
        }),
    );
    let mut h = Harness::new(pool, vec![FakeTurn::Chunks(t), turn("never", vec![])]).await;
    h.add(Probe::new("probe", SideEffect::Read));
    assert_eq!(h.run().await.unwrap().stop, Some(StopReason::TokenBudget));
    assert_eq!(h.fake.received().len(), 1);
}
#[test]
fn tool_result_truncated_to_8000_scalars_including_marker_thai() {
    let result = guards::truncate_result(&"สวัสดี".repeat(3000), 8000);
    assert_eq!(result.chars().count(), 8000);
    assert!(result.ends_with("…[truncated]"));
    let value = json!({"text":"ก".repeat(9000)}).to_string();
    let preview = guards::truncate_result(&value, 8000);
    assert!(preview.starts_with("[JSON truncated] "));
    assert!(preview.chars().count() <= 8000);
    for cap in 0..25 {
        assert!(guards::truncate_result(&value, cap).chars().count() <= cap);
    }
}
#[sqlx::test]
async fn ask_user_ends_run_needs_input(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![turn(
            "draft",
            vec![
                call(
                    "ask",
                    "ask_user",
                    json!({"question":"Which?","options":["One"]}),
                ),
                call("later", "read_thread", json!({})),
            ],
        )],
    )
    .await;
    let facts = h.run().await.unwrap();
    assert_eq!(facts.pending_question.unwrap().question, "Which?");
    assert_eq!(facts.final_text, None);
    let rows = h.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].get::<String, _>("result"), "superseded_by_question");
    assert_eq!(rows[0].get::<String, _>("state"), "succeeded");
}
#[sqlx::test]
async fn ask_user_in_middle_of_batch_keeps_earlier_results(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![
                call("first", "read_thread", json!({})),
                call("ask", "ask_user", json!({"question":"Which?","options":[]})),
                call("last", "read_thread", json!({})),
            ],
        )],
    )
    .await;
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(
        rows.iter()
            .map(|r| r.get::<String, _>("state"))
            .collect::<Vec<_>>(),
        ["succeeded", "succeeded", "refused"]
    );
}
#[sqlx::test]
async fn ask_user_rejects_arguments_outside_its_schema(pool: PgPool) {
    for args in [
        json!({"question":"x".repeat(501),"options":[]}),
        json!({"question":"?","options":["a","b","c","d","e"]}),
        json!({"question":"?","options":[],"target":"elsewhere"}),
    ] {
        let h = Harness::new(
            pool.clone(),
            vec![
                turn("", vec![call("a", "ask_user", args)]),
                turn("ok", vec![]),
            ],
        )
        .await;
        assert!(h.run().await.unwrap().pending_question.is_none());
        assert!(tool_results(&h.fake)[0].3);
    }
}
#[sqlx::test]
async fn read_thread_returns_a_bounded_page(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![
            turn(
                "",
                vec![
                    call("bad", "read_thread", json!({"limit":51})),
                    call("page", "read_thread", json!({"limit":1})),
                ],
            ),
            turn("ok", vec![]),
        ],
    )
    .await;
    let later = MessageId::new();
    sqlx::query("INSERT INTO messages(id,workspace_id,thread_id,author_kind,body) VALUES($1,$2,$3,'system','notice')").bind(later).bind(h.f.workspace).bind(h.f.thread).execute(&h.pool).await.unwrap();
    h.run().await.unwrap();
    let results = tool_results(&h.fake);
    assert!(results[0].3);
    let page: Value = serde_json::from_str(&results[1].2).unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["id"], later.to_string());
    assert_eq!(page["next_before"], later.to_string());
}
#[sqlx::test]
async fn refused_outward_call_does_not_mark_effects_started(pool: PgPool) {
    let mut h = Harness::new(pool, vec![turn("", vec![call("a", "probe", json!({}))])]).await;
    h.add(Probe::new("probe", SideEffect::Outward(OutwardKind::Send)));
    h.run().await.unwrap();
    assert!(!h.effects().await);
}
#[sqlx::test]
async fn unknown_tool_is_ledgered_refused_with_no_effect(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "unknown", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<String, _>("state"), "refused");
    assert!(rows[0].get::<Option<String>, _>("side_effect").is_none());
    assert!(!h.effects().await);
}
#[sqlx::test]
async fn run_insert_failure_aborts_without_dispatch(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("never", vec![])]).await;
    h.pool.close().await;
    assert!(matches!(
        h.run().await,
        Err(RunAbort::Infrastructure { run_id: None, .. })
    ));
    assert!(h.fake.received().is_empty());
}
#[sqlx::test]
async fn ledger_start_failure_prevents_tool_dispatch(pool: PgPool) {
    let mut h = Harness::new(pool, vec![turn("", vec![call("a", "probe", json!({}))])]).await;
    let n = h.add(Probe::new("probe", SideEffect::Read));
    sqlx::query("ALTER TABLE tool_calls ADD CONSTRAINT reject_calls CHECK (false) NOT VALID")
        .execute(&h.pool)
        .await
        .unwrap();
    assert!(matches!(
        h.run().await,
        Err(RunAbort::Infrastructure { .. })
    ));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
#[sqlx::test]
async fn event_write_failure_aborts_run(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("never", vec![])]).await;
    h.sink.fail.store(1, Ordering::SeqCst);
    assert!(matches!(
        h.run().await,
        Err(RunAbort::Infrastructure { code: "busy", .. })
    ));
    assert!(h.fake.received().is_empty());
}
#[sqlx::test]
async fn tool_output_never_enters_system_role(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(
        Probe::new("probe", SideEffect::Read)
            .action(|_, _| async { Ok(ToolOutput::Text("OVERRIDE_SYSTEM_MARKER".into())) }.boxed()),
    );
    h.run().await.unwrap();
    for req in h.fake.received() {
        assert!(!req.system.contains("OVERRIDE_SYSTEM_MARKER"));
    }
    assert!(
        tool_results(&h.fake)[0]
            .2
            .contains("OVERRIDE_SYSTEM_MARKER")
    );
}
#[sqlx::test]
async fn capability_preamble_lists_effective_tools(pool: PgPool) {
    let mut h = Harness::new(pool, vec![turn("ok", vec![])]).await;
    h.add(Probe::new("forbidden", SideEffect::Read));
    h.run().await.unwrap();
    let req = &h.fake.received()[0];
    assert!(req.system.contains("ask_user"));
    assert!(req.system.contains("read_thread"));
    assert!(!req.system.contains("forbidden"));
    assert!(!req.system.contains("never ask"));
    assert_eq!(req.tools.len(), 2);
}
#[sqlx::test]
async fn run_started_carries_synthetic_from_the_selected_entry(pool: PgPool) {
    for synthetic in [false, true] {
        let mut h = Harness::new(pool.clone(), vec![turn("ok", vec![])]).await;
        if synthetic {
            let p = FakeProvider::demo();
            h.models = Registry::from_entries(vec![(entry(&p), Arc::new(p))]);
        }
        assert_eq!(h.run().await.unwrap().synthetic, synthetic);
        let events = h.sink.events.lock().unwrap();
        let start = events
            .iter()
            .position(|e| e.kind() == "katwork.run_started")
            .unwrap();
        let text = events
            .iter()
            .position(|e| e.kind() == "TEXT_MESSAGE_CONTENT")
            .unwrap();
        assert!(start < text);
        let value = serde_json::to_value(&events[start]).unwrap();
        assert_eq!(value["value"]["synthetic"], synthetic);
        assert_eq!(value["value"]["jobId"], h.input.job_id.to_string());
    }
}
#[sqlx::test]
async fn usage_recorded_once_per_attempt(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(Probe::new("probe", SideEffect::Read));
    let facts = h.run().await.unwrap();
    assert_eq!(facts.usage.len(), 2);
    assert_eq!(h.usage().await.len(), 2);
    assert_eq!(
        h.sink
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind() == "katwork.usage")
            .count(),
        2
    );
}
#[sqlx::test]
async fn retry_before_first_content_gets_its_own_row(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![
            FakeTurn::Error(ModelError::ProviderUnavailable),
            turn("ok", vec![]),
        ],
    )
    .await;
    assert_eq!(h.run().await.unwrap().final_text.as_deref(), Some("ok"));
    assert_eq!(h.fake.received().len(), 2);
    let rows = h.usage().await;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].get::<i64, _>("charged_tokens") >= 4096);
    assert_eq!(rows[1].get::<i64, _>("charged_tokens"), 20);
}
#[sqlx::test]
async fn max_output_tokens_capped_by_remaining_budget(pool: PgPool) {
    let mut h = Harness::new(pool, vec![turn("ok", vec![])]).await;
    h.limits.max_total_tokens = 300;
    h.run().await.unwrap();
    assert_eq!(h.fake.received()[0].max_output_tokens, 300);
    assert_eq!(h.fake.received()[0].temperature, Some(0.2));
}
#[sqlx::test]
async fn missing_usage_is_charged_conservatively_and_stored_null(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![FakeTurn::Chunks(vec![ChatChunk::Done {
            stop: StopKind::EndTurn,
            turn: AssistantTurn {
                content: "ok".into(),
                tool_calls: vec![],
                replay: None,
            },
        }])],
    )
    .await;
    h.run().await.unwrap();
    let rows = h.usage().await;
    let request = &h.fake.received()[0];
    assert_eq!(
        rows[0].get::<i64, _>("charged_tokens"),
        request.input_charge_bound() as i64 + i64::from(request.max_output_tokens)
    );
    assert!(rows[0].get::<Option<i64>, _>("input_tokens").is_none());
    assert!(rows[0].get::<Option<i64>, _>("output_tokens").is_none());
}
#[sqlx::test]
async fn duplicate_call_ids_stay_distinguishable(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("same", "probe", json!({"x":1}))]),
            turn("", vec![call("same", "probe", json!({"x":2}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(Probe::new("probe", SideEffect::Read));
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(rows.len(), 2);
    let events = h.sink.events.lock().unwrap();
    let ids: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgUiEvent::ToolCallStart { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}
#[sqlx::test]
async fn successful_sibling_audited_when_another_fails(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn(
                "",
                vec![call("a", "probe", json!({})), call("b", "other", json!({}))],
            ),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(Probe::new("probe", SideEffect::Read));
    h.add(
        Probe::new("other", SideEffect::Read)
            .action(|_, _| async { Err(ToolError::Failed("tool_failed".into())) }.boxed()),
    );
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(rows[0].get::<String, _>("state"), "succeeded");
    assert_eq!(rows[1].get::<String, _>("state"), "failed");
}

async fn revoke(ctx: &ToolCtx, pause: bool, grant: bool) {
    if grant {
        let other = SpaceId::new();
        sqlx::query(
            "INSERT INTO spaces(id,workspace_id,name,allow_external) VALUES($1,$2,'Default',false)",
        )
        .bind(other)
        .bind(ctx.tenant.workspace_id())
        .execute(&ctx.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO kat_spaces(workspace_id,kat_id,space_id) VALUES($1,$2,$3)")
            .bind(ctx.tenant.workspace_id())
            .bind(ctx.kat_id)
            .bind(other)
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE kats SET default_space_id=$3,policy_version=policy_version+1 WHERE workspace_id=$1 AND id=$2").bind(ctx.tenant.workspace_id()).bind(ctx.kat_id).bind(other).execute(&ctx.pool).await.unwrap();
        sqlx::query("DELETE FROM kat_spaces WHERE workspace_id=$1 AND kat_id=$2 AND space_id=$3")
            .bind(ctx.tenant.workspace_id())
            .bind(ctx.kat_id)
            .bind(ctx.space_id)
            .execute(&ctx.pool)
            .await
            .unwrap();
    } else {
        kat::update(
            &ctx.pool,
            &ctx.tenant,
            ctx.kat_id,
            KatPatch {
                status: pause.then_some(KatStatus::Paused),
                tool_allowlist: (!pause).then(Vec::new),
                ..KatPatch::default()
            },
        )
        .await
        .unwrap();
    }
}
#[sqlx::test]
async fn allowlist_revoked_mid_run_stops_before_next_tool(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![call("a", "probe", json!({})), call("b", "other", json!({}))],
        )],
    )
    .await;
    h.add(Probe::new("probe", SideEffect::Read).action(|ctx, _| {
        async move {
            revoke(&ctx, false, false).await;
            Ok(ToolOutput::Text("revoked".into()))
        }
        .boxed()
    }));
    let n = h.add(Probe::new("other", SideEffect::Read));
    assert_eq!(h.run().await.unwrap().stop, Some(StopReason::PolicyChanged));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
#[sqlx::test]
async fn policy_change_between_attempts_stops_before_next_model_call(pool: PgPool) {
    for grant in [false, true] {
        let mut h = Harness::new(
            pool.clone(),
            vec![
                turn("", vec![call("a", "probe", json!({}))]),
                turn("never", vec![]),
            ],
        )
        .await;
        h.add(Probe::new("probe", SideEffect::Read).action(move |ctx, _| {
            async move {
                revoke(&ctx, !grant, grant).await;
                Ok(ToolOutput::Text("changed".into()))
            }
            .boxed()
        }));
        let facts = h.run().await.unwrap();
        assert_eq!(
            facts.stop,
            Some(if grant {
                StopReason::PolicyChanged
            } else {
                StopReason::KatPaused
            })
        );
        assert_eq!(h.fake.received().len(), 1);
        assert!(!facts.run_closed);
    }
}
#[sqlx::test]
async fn effects_marker_is_committed_before_a_non_read_call_runs(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "write", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(|ctx, _| {
            async move {
                let marked: bool = sqlx::query_scalar(
                    "SELECT effects_started_at IS NOT NULL FROM tasks WHERE id=$1",
                )
                .bind(ctx.task_id)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
                assert!(marked);
                Ok(ToolOutput::Text("ok".into()))
            }
            .boxed()
        }),
    );
    h.run().await.unwrap();
}
async fn write_effect(ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
    let mut tx = ctx.pool.begin().await.unwrap();
    match guards::authorize_write(&mut tx, ctx).await {
        Ok(Some(stop)) => return Err(ToolError::Halted(stop)),
        Err(KatError::LeaseLost) => return Err(ToolError::Halted(StopReason::Interrupted)),
        Err(e) => panic!("{e}"),
        Ok(None) => {}
    }
    sqlx::query("INSERT INTO test_effects(id) VALUES($1)")
        .bind(ctx.idempotency_key.clone())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Ok(ToolOutput::Text("written".into()))
}
async fn effects_table(pool: &PgPool) {
    sqlx::query("CREATE TABLE test_effects(id TEXT PRIMARY KEY)")
        .execute(pool)
        .await
        .unwrap();
}
async fn count_effects(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM test_effects")
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test]
async fn policy_revoked_before_write_commit_blocks_effect(pool: PgPool) {
    effects_table(&pool).await;
    let mut h = Harness::new(pool, vec![turn("", vec![call("a", "write", json!({}))])]).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (a, b) = (entered.clone(), release.clone());
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(move |ctx, _| {
            let (a, b) = (a.clone(), b.clone());
            async move {
                a.notify_one();
                b.notified().await;
                write_effect(&ctx).await
            }
            .boxed()
        }),
    );
    let edit = async {
        entered.notified().await;
        kat::update(
            &h.pool,
            &h.input.tenant,
            h.kat.id,
            KatPatch {
                tool_allowlist: Some(vec![]),
                ..KatPatch::default()
            },
        )
        .await
        .unwrap();
        release.notify_one();
    };
    let (facts, ()) = tokio::join!(h.run(), edit);
    assert_eq!(facts.unwrap().stop, Some(StopReason::PolicyChanged));
    assert_eq!(count_effects(&h.pool).await, 0);
    assert_eq!(h.rows().await[0].get::<String, _>("state"), "failed");
}
#[sqlx::test]
async fn policy_edit_waits_for_an_open_write(pool: PgPool) {
    effects_table(&pool).await;
    let mut h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![call("a", "write", json!({})), call("b", "other", json!({}))],
        )],
    )
    .await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let edited = Arc::new(Notify::new());
    let (a, b, c) = (entered.clone(), release.clone(), edited.clone());
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(move |ctx, _| {
            let (a, b, c) = (a.clone(), b.clone(), c.clone());
            async move {
                let mut tx = ctx.pool.begin().await.unwrap();
                assert_eq!(guards::authorize_write(&mut tx, &ctx).await.unwrap(), None);
                a.notify_one();
                b.notified().await;
                sqlx::query("INSERT INTO test_effects(id) VALUES($1)")
                    .bind(ctx.idempotency_key)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                c.notified().await;
                Ok(ToolOutput::Text("ok".into()))
            }
            .boxed()
        }),
    );
    let n = h.add(Probe::new("other", SideEffect::Read));
    let edit = async {
        entered.notified().await;
        let mut update = Box::pin(kat::update(
            &h.pool,
            &h.input.tenant,
            h.kat.id,
            KatPatch {
                tool_allowlist: Some(vec![]),
                ..KatPatch::default()
            },
        ));
        // Observe the database lock wait, not scheduling or elapsed time.
        loop {
            tokio::select! {
                result=&mut update=>panic!("edit passed open write: {result:?}"),
                waiting=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND (query LIKE '%kats%' OR query LIKE '%workspaces%'))").fetch_one(&h.pool)=>if waiting.unwrap(){break;}
            }
        }
        release.notify_one();
        update.await.unwrap();
        edited.notify_one();
    };
    let (facts, ()) = tokio::join!(h.run(), edit);
    assert_eq!(facts.unwrap().stop, Some(StopReason::PolicyChanged));
    assert_eq!(count_effects(&h.pool).await, 1);
    assert_eq!(n.load(Ordering::SeqCst), 0);
}
#[sqlx::test]
async fn old_worker_cannot_commit_tool_write_after_reclaim(pool: PgPool) {
    effects_table(&pool).await;
    let mut h = Harness::new(pool, vec![turn("", vec![call("a", "write", json!({}))])]).await;
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(|ctx, _| {
            async move {
                sqlx::query("UPDATE tasks SET lease_epoch=lease_epoch+1 WHERE id=$1")
                    .bind(ctx.task_id)
                    .execute(&ctx.pool)
                    .await
                    .unwrap();
                write_effect(&ctx).await
            }
            .boxed()
        }),
    );
    assert!(matches!(h.run().await, Err(RunAbort::LeaseLost)));
    assert_eq!(count_effects(&h.pool).await, 0);
}

struct ErrorAfter {
    chunks: Vec<ChatChunk>,
    error: ModelError,
    requests: AtomicUsize,
}
#[async_trait]
impl ModelProvider for ErrorAfter {
    fn id(&self) -> &str {
        "error_after"
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
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(futures::stream::iter(
            self.chunks
                .clone()
                .into_iter()
                .map(Ok)
                .chain([Err(self.error.clone())]),
        )
        .boxed())
    }
}
#[sqlx::test]
async fn no_retry_after_content_or_progress(pool: PgPool) {
    for chunk in [
        ChatChunk::TextDelta("part".into()),
        ChatChunk::Progress,
        ChatChunk::ToolCall(call("a", "probe", json!({}))),
    ] {
        let mut h = Harness::new(pool.clone(), vec![]).await;
        let p = Arc::new(ErrorAfter {
            chunks: vec![chunk],
            error: ModelError::ProviderUnavailable,
            requests: AtomicUsize::new(0),
        });
        h.models = Registry::from_entries(vec![(entry(&*p), p.clone())]);
        assert_eq!(
            h.run().await.unwrap().stop,
            Some(StopReason::ProviderUnavailable)
        );
        assert_eq!(p.requests.load(Ordering::SeqCst), 1);
        assert_eq!(h.usage().await.len(), 1);
    }
}
fn error_cases() -> Vec<(ModelError, StopReason)> {
    vec![
        (
            ModelError::ProviderUnavailable,
            StopReason::ProviderUnavailable,
        ),
        (
            ModelError::Other("secret-diagnostic".into()),
            StopReason::ProviderUnavailable,
        ),
        (
            ModelError::Timeout {
                phase: TimeoutPhase::FirstChunk,
            },
            StopReason::ProviderUnavailable,
        ),
        (
            ModelError::Timeout {
                phase: TimeoutPhase::Idle,
            },
            StopReason::ProviderUnavailable,
        ),
        (
            ModelError::RateLimited {
                retry_after_secs: Some(0),
            },
            StopReason::ProviderRateLimited,
        ),
        (
            ModelError::QuotaExceeded { resets_at: None },
            StopReason::ProviderQuota,
        ),
        (ModelError::AuthFailed, StopReason::ProviderAuthFailed),
        (
            ModelError::Protocol("secret-diagnostic".into()),
            StopReason::ModelProtocol,
        ),
        (ModelError::ResidencyDenied, StopReason::ResidencyDenied),
    ]
}
#[sqlx::test]
async fn every_model_error_maps_to_a_stable_stop_reason(pool: PgPool) {
    for (error, stop) in error_cases() {
        let h = Harness::new(
            pool.clone(),
            vec![FakeTurn::Error(error.clone()), FakeTurn::Error(error)],
        )
        .await;
        let facts = h.run().await.unwrap();
        assert_eq!(facts.stop, Some(stop));
        assert_eq!(
            facts.executor_unavailable,
            stop == StopReason::ProviderUnavailable
        );
        assert!(!format!("{:?}", h.sink.events.lock().unwrap()).contains("secret-diagnostic"));
    }
}
#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[sqlx::test]
async fn provider_error_text_never_reaches_logs(pool: PgPool) {
    use tracing::instrument::WithSubscriber;
    let log = LogBuffer(Arc::default());
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    async {
        for (error, _) in error_cases() {
            let h = Harness::new(
                pool.clone(),
                vec![FakeTurn::Error(error.clone()), FakeTurn::Error(error)],
            )
            .await;
            h.run().await.unwrap();
            assert!(!format!("{:?}", h.sink.events.lock().unwrap()).contains("secret-diagnostic"));
        }
    }
    .with_subscriber(subscriber)
    .await;
    assert!(!String::from_utf8_lossy(&log.0.lock().unwrap()).contains("secret-diagnostic"));
}
#[sqlx::test]
async fn provider_change_rechecks_residency(pool: PgPool) {
    // Registry selection is immutable during a run. Every retry must still apply Space residency.
    let mut h = Harness::new(
        pool,
        vec![
            FakeTurn::Error(ModelError::ProviderUnavailable),
            turn("local", vec![]),
        ],
    )
    .await;
    let hosted = FakeProvider::scripted(vec![turn("leak", vec![])]);
    let mut external = entry(&hosted);
    external.capabilities.residency = Residency::External;
    h.models = Registry::from_entries(vec![
        (external, Arc::new(hosted.clone())),
        (entry(&h.fake), Arc::new(h.fake.clone())),
    ]);
    assert_eq!(h.run().await.unwrap().final_text.as_deref(), Some("local"));
    assert!(hosted.received().is_empty());
    assert_eq!(h.fake.received().len(), 2);
}

// Keep paused time from auto-advancing while SQL I/O is pending.
struct Clock {
    spinner: tokio::task::JoinHandle<()>,
}
impl Clock {
    fn pause() -> Self {
        tokio::time::pause();
        Self {
            spinner: tokio::spawn(async {
                loop {
                    tokio::task::yield_now().await;
                }
            }),
        }
    }
}
impl Drop for Clock {
    fn drop(&mut self) {
        self.spinner.abort();
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
async fn wall_clock_case(pool: PgPool, local: bool, input_secs: u64, expected: u64) {
    let mut h = Harness::new(
        pool,
        vec![FakeTurn::ChunksThenHang(vec![ChatChunk::Progress])],
    )
    .await;
    let mut e = entry(&h.fake);
    e.timeouts = if local {
        ModelTimeouts::LOCAL
    } else {
        ModelTimeouts::HOSTED
    };
    e.timeouts.idle = Duration::from_secs(1000);
    h.models = Registry::from_entries(vec![(e, Arc::new(h.fake.clone()))]);
    let _clock = Clock::pause();
    h.input.deadline = tokio::time::Instant::now() + Duration::from_secs(input_secs);
    let mut run = Box::pin(h.run());
    let advance = async {
        until(|| !h.fake.received().is_empty()).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(expected - 1)).await;
    };
    tokio::select! {result=&mut run=>panic!("ended early: {result:?}"),()=advance=>{}}
    tokio::select! {biased;result=&mut run=>panic!("ended before deadline: {result:?}"),()=tokio::task::yield_now()=>{}}
    tokio::time::advance(Duration::from_millis(1_001)).await;
    assert_eq!(run.await.unwrap().stop, Some(StopReason::TimeLimit));
}
#[sqlx::test]
async fn hosted_run_wall_clock_90s_stops_time_limit(pool: PgPool) {
    wall_clock_case(pool, false, 600, 90).await;
}
#[sqlx::test]
async fn local_run_wall_clock_is_300s(pool: PgPool) {
    wall_clock_case(pool, true, 600, 300).await;
}
#[sqlx::test]
async fn run_deadline_never_exceeds_input_deadline(pool: PgPool) {
    wall_clock_case(pool, true, 10, 10).await;
}
#[sqlx::test]
async fn attempt_uses_its_entrys_stream_timeouts(pool: PgPool) {
    let mut h = Harness::new(pool, vec![FakeTurn::Hang]).await;
    let mut e = entry(&h.fake);
    e.timeouts = ModelTimeouts::LOCAL;
    h.models = Registry::from_entries(vec![(e, Arc::new(h.fake.clone()))]);
    h.limits.model_retries = 0;
    let _clock = Clock::pause();
    let mut run = Box::pin(h.run());
    tokio::select! {result=&mut run=>panic!("early: {result:?}"),()=async{until(||!h.fake.received().is_empty()).await;tokio::task::yield_now().await;tokio::time::advance(Duration::from_secs(30)).await;}=>{}}
    tokio::select! {biased;result=&mut run=>panic!("hosted timeout used: {result:?}"),()=tokio::task::yield_now()=>{}}
    tokio::time::advance(Duration::from_millis(90_001)).await;
    assert_eq!(
        run.await.unwrap().stop,
        Some(StopReason::ProviderUnavailable)
    );
}
#[sqlx::test]
async fn tool_timeout_60s_returns_error_result(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let n = h.add(
        Probe::new("probe", SideEffect::Read)
            .action(|_, _| async { futures::future::pending().await }.boxed()),
    );
    // Let both 60-second Read attempts fit in the run.
    let mut e = entry(&h.fake);
    e.timeouts = ModelTimeouts::LOCAL;
    h.models = Registry::from_entries(vec![(e, Arc::new(h.fake.clone()))]);
    let _clock = Clock::pause();
    let progress = async {
        until(|| n.load(Ordering::SeqCst) == 1).await;
        tokio::time::advance(Duration::from_millis(60_001)).await;
        until(|| n.load(Ordering::SeqCst) == 2).await;
        tokio::time::advance(Duration::from_millis(60_001)).await;
    };
    let (result, ()) = tokio::join!(h.run(), progress);
    assert_eq!(result.unwrap().final_text.as_deref(), Some("ok"));
    assert!(tool_results(&h.fake)[0].3);
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
async fn timeout_write_case(pool: PgPool, commit_first: bool) {
    effects_table(&pool).await;
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "write", json!({}))]),
            turn("", vec![call("b", "write", json!({}))]),
        ],
    )
    .await;
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(move |ctx, _| {
            let signal = signal.clone();
            async move {
                if commit_first {
                    write_effect(&ctx).await?;
                }
                signal.notify_one();
                tokio::time::sleep(Duration::from_secs(120)).await;
                write_effect(&ctx).await
            }
            .boxed()
        }),
    );
    let _clock = Clock::pause();
    let progress = async {
        entered.notified().await;
        tokio::time::advance(Duration::from_millis(60_001)).await;
    };
    let (result, ()) = tokio::join!(h.run(), progress);
    assert_eq!(result.unwrap().stop, Some(StopReason::OutcomeUnknown));
    assert_eq!(h.fake.received().len(), 1);
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::task::yield_now().await;
    assert_eq!(count_effects(&h.pool).await, i64::from(commit_first));
    assert_eq!(
        h.rows().await[0].get::<String, _>("state"),
        "outcome_unknown"
    );
}
#[sqlx::test]
async fn timed_out_tool_cannot_write_afterwards(pool: PgPool) {
    timeout_write_case(pool, false).await;
}

#[sqlx::test]
async fn unknown_outcome_write_stops_for_review_before_model_retries_it(pool: PgPool) {
    timeout_write_case(pool, true).await;
}
#[sqlx::test]
async fn read_retry_reuses_its_ledger_row(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let tries = Arc::new(AtomicUsize::new(0));
    h.add(Probe::new("probe", SideEffect::Read).action(move |_, _| {
        let n = tries.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                Err(ToolError::OutcomeUnknown)
            } else {
                Ok(ToolOutput::Text("ok".into()))
            }
        }
        .boxed()
    }));
    h.run().await.unwrap();
    let rows = h.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>("attempts"), 2);
    assert_eq!(rows[0].get::<String, _>("state"), "succeeded");
}
async fn cancel_case(pool: PgPool) {
    let cancelled_after = AtomicUsize::new(0);
    let h = Harness::new(
        pool,
        vec![FakeTurn::ChunksThenHang(vec![ChatChunk::TextDelta(
            "part".repeat(200),
        )])],
    )
    .await;
    let cancel = async {
        until(|| {
            h.sink
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.kind() == "TEXT_MESSAGE_CONTENT")
        })
        .await;
        cancelled_after.store(h.sink.events.lock().unwrap().len(), Ordering::SeqCst);
        h.input.cancel.cancel();
    };
    let (result, ()) = tokio::join!(h.run(), cancel);
    let facts = result.unwrap();
    assert_eq!(facts.stop, Some(StopReason::Cancelled));
    assert!(facts.run_closed);
    assert!(
        !h.sink.events.lock().unwrap()[cancelled_after.load(Ordering::SeqCst)..]
            .iter()
            .any(|event| event.kind().starts_with("TEXT_MESSAGE"))
    );
    let events: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM events WHERE run_id=$1 ORDER BY seq")
            .bind(h.input.run_id)
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert_eq!(events.last().unwrap(), "RUN_ERROR");
    assert!(!events.iter().any(|e| e == "RUN_FINISHED"));
    assert_eq!(
        events.iter().filter(|e| e.as_str() == "RUN_ERROR").count(),
        1
    );
}

#[sqlx::test]
async fn no_done_after_cancellation(pool: PgPool) {
    cancel_case(pool).await;
}
struct ReclaimSink {
    inner: Sink,
    pool: PgPool,
    task: TaskId,
    count: AtomicUsize,
}
#[async_trait]
impl EventSink for ReclaimSink {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        if matches!(event, AgUiEvent::TextMessageContent { .. })
            && self.count.fetch_add(1, Ordering::SeqCst) == 0
        {
            sqlx::query("UPDATE tasks SET lease_epoch=lease_epoch+1 WHERE id=$1")
                .bind(self.task)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        self.inner.emit(event).await
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.inner.close_run(end, code).await
    }
}
#[sqlx::test]
async fn old_worker_cannot_emit_text_after_reclaim(pool: PgPool) {
    let h = Harness::new(pool, vec![turn(&"text".repeat(200), vec![])]).await;
    let sink = ReclaimSink {
        inner: h.sink,
        pool: h.pool.clone(),
        task: h.input.task_id,
        count: AtomicUsize::new(0),
    };
    let result = run::execute(
        &RunEnv {
            pool: &h.pool,
            models: &h.models,
            tools: &h.tools,
            sink: &sink,
        },
        &h.kat,
        h.input,
        &h.limits,
    )
    .await;
    assert!(matches!(result, Err(RunAbort::LeaseLost)));
    assert!(
        !sink
            .inner
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.kind() == "TEXT_MESSAGE_CONTENT")
    );
    assert_eq!(h.fake.received().len(), 1);
}

#[sqlx::test]
async fn unbounded_retry_after_does_not_panic_or_dispatch_again(pool: PgPool) {
    let h = Harness::new(
        pool,
        vec![
            FakeTurn::Error(ModelError::RateLimited {
                retry_after_secs: Some(u64::MAX),
            }),
            turn("never", vec![]),
        ],
    )
    .await;
    assert_eq!(
        h.run().await.unwrap().stop,
        Some(StopReason::ProviderRateLimited)
    );
    assert_eq!(h.fake.received().len(), 1);
}
#[sqlx::test]
async fn cancellation_drops_running_tool_and_closes_ledger(pool: PgPool) {
    effects_table(&pool).await;
    let mut h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![call("a", "write", json!({})), call("b", "other", json!({}))],
        )],
    )
    .await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (a, b) = (entered.clone(), release.clone());
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(move |ctx, _| {
            let (a, b) = (a.clone(), b.clone());
            async move {
                a.notify_one();
                b.notified().await;
                write_effect(&ctx).await
            }
            .boxed()
        }),
    );
    let later = h.add(Probe::new("other", SideEffect::Read));
    let cancel = async {
        entered.notified().await;
        h.input.cancel.cancel();
    };
    let (result, ()) = tokio::join!(h.run(), cancel);
    assert_eq!(result.unwrap().stop, Some(StopReason::Cancelled));
    release.notify_one();
    tokio::task::yield_now().await;
    assert_eq!(count_effects(&h.pool).await, 0);
    assert_eq!(later.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.rows().await[0].get::<String, _>("state"),
        "outcome_unknown"
    );
}
#[sqlx::test]
async fn foreign_tenant_input_never_locks_or_reads_foreign_task(pool: PgPool) {
    let mut h = Harness::new(pool.clone(), vec![turn("never", vec![])]).await;
    let other = Fixture::new(&pool).await;
    h.input.tenant = tenancy::resolve(&pool, UserId(other.user), WorkspaceId(other.workspace))
        .await
        .unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE id=$1 FOR NO KEY UPDATE")
        .bind(h.input.task_id)
        .execute(&mut *lock)
        .await
        .unwrap();
    assert!(matches!(
        h.run().await,
        Err(RunAbort::Infrastructure {
            code: "not_found",
            ..
        })
    ));
    assert!(h.fake.received().is_empty());
    lock.rollback().await.unwrap();
}
#[sqlx::test]
async fn args_cannot_override_thread_or_tenant(pool: PgPool) {
    let h = Harness::new(
        pool.clone(),
        vec![
            turn(
                "",
                vec![call(
                    "a",
                    "read_thread",
                    json!({"thread_id":ThreadId::new(),"workspace_id":WorkspaceId::new()}),
                )],
            ),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.run().await.unwrap();
    assert!(tool_results(&h.fake)[0].3);
    assert_eq!(h.rows().await[0].get::<String, _>("state"), "refused");
}
#[sqlx::test]
async fn duplicate_call_ids_in_one_turn_never_dispatch(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![
                call("same", "probe", json!({"x":1})),
                call("same", "probe", json!({"x":2})),
            ],
        )],
    )
    .await;
    let n = h.add(Probe::new("probe", SideEffect::Read));
    assert_eq!(h.run().await.unwrap().stop, Some(StopReason::ModelProtocol));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn rejected_turns_emit_no_tool_events(pool: PgPool) {
    for stop in [
        StopKind::MaxTokens,
        StopKind::Refusal,
        StopKind::Other("unknown".into()),
    ] {
        let h = Harness::new(
            pool.clone(),
            vec![ending(
                "partial",
                vec![call("a", "read_thread", json!({}))],
                stop,
            )],
        )
        .await;
        h.run().await.unwrap();
        assert_eq!(h.rows().await[0].get::<String, _>("state"), "refused");
        assert!(
            !h.sink
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.kind().starts_with("TOOL_CALL"))
        );
    }
}

#[path = "agent/review.rs"]
mod review;
