#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;

#[derive(Clone, Copy)]
enum Interruption {
    ModelPause,
    ModelDeadline,
    ModelCancel,
    Deadline,
    Cancel,
    ToolPause,
}
struct InterruptSink<'a> {
    h: &'a Harness,
    interruption: Interruption,
    tool_end: usize,
    seen: AtomicUsize,
}
#[async_trait]
impl EventSink for InterruptSink<'_> {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        let model_pause = matches!(self.interruption, Interruption::ModelPause)
            && matches!(event, AgUiEvent::TextMessageStart { .. });
        let model_start = matches!(event, AgUiEvent::TextMessageStart { .. });
        let tool_end = matches!(event, AgUiEvent::ToolCallEnd { .. })
            && self.seen.fetch_add(1, Ordering::SeqCst) == self.tool_end;
        let seq = self.h.sink.emit(event).await?;
        if model_start {
            match self.interruption {
                Interruption::ModelDeadline => tokio::time::advance(Duration::from_secs(91)).await,
                Interruption::ModelCancel => self.h.input.cancel.cancel(),
                _ => {}
            }
        }
        if model_pause || (tool_end && matches!(self.interruption, Interruption::ToolPause)) {
            kat::update(
                &self.h.pool,
                &self.h.input.tenant,
                self.h.kat.id,
                KatPatch {
                    status: Some(KatStatus::Paused),
                    ..KatPatch::default()
                },
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap();
        }
        if tool_end {
            match self.interruption {
                Interruption::Deadline => tokio::time::advance(Duration::from_secs(91)).await,
                Interruption::Cancel => self.h.input.cancel.cancel(),
                Interruption::ModelPause
                | Interruption::ModelDeadline
                | Interruption::ModelCancel
                | Interruption::ToolPause => {}
            }
        }
        Ok(seq)
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.h.sink.close_run(end, code).await
    }
}
async fn interrupted_run(h: &Harness, interruption: Interruption, tool_end: usize) -> RunFacts {
    run::execute(
        &RunEnv {
            lock_timeout: std::time::Duration::from_secs(5),
            pool: &h.pool,
            models: &h.models,
            tools: &h.tools,
            sink: &InterruptSink {
                h,
                interruption,
                tool_end,
                seen: AtomicUsize::new(0),
            },
        },
        &h.kat,
        h.input.clone(),
        &h.limits,
    )
    .await
    .unwrap()
}
// The independent reviewer's event-sink interleaving, using the real PgEventSink.
#[sqlx::test]
async fn revocation_during_model_start_event_prevents_request(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("data left after pause", vec![])]).await;
    let facts = interrupted_run(&h, Interruption::ModelPause, 0).await;
    assert!(
        h.fake.received().is_empty(),
        "provider received a request after committed pause"
    );
    assert_eq!(facts.stop, Some(StopReason::KatPaused));
    assert_unsent_model(&h, &facts).await;
}
async fn assert_unsent_model(h: &Harness, facts: &RunFacts) {
    assert!(h.fake.received().is_empty());
    assert!(
        h.usage().await.is_empty(),
        "an unsent request must not consume a model attempt or budget"
    );
    assert!(facts.usage.is_empty());
    let events = h.sink.events.lock().unwrap();
    assert!(!events.iter().any(|e| e.kind() == "katwork.usage"));
    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgUiEvent::TextMessageStart { message_id, .. } => Some(message_id),
            _ => None,
        })
        .collect();
    let ends: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgUiEvent::TextMessageEnd { message_id } => Some(message_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts, ends,
        "every unsent attempt must close its opened text message"
    );
}
#[sqlx::test]
async fn deadline_before_model_request_is_not_charged(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("not sent", vec![])]).await;
    let _clock = Clock::pause();
    let facts = interrupted_run(&h, Interruption::ModelDeadline, 0).await;
    assert_eq!(facts.stop, Some(StopReason::TimeLimit));
    assert_unsent_model(&h, &facts).await;
}
#[sqlx::test]
async fn cancel_before_model_request_is_not_charged(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("not sent", vec![])]).await;
    let facts = interrupted_run(&h, Interruption::ModelCancel, 0).await;
    assert_eq!(facts.stop, Some(StopReason::Cancelled));
    assert!(facts.run_closed);
    assert_unsent_model(&h, &facts).await;
}
async fn before_tool_poll(
    pool: PgPool,
    interruption: Interruption,
    reason: StopReason,
    prior_effect: bool,
) {
    let mut calls = vec![];
    if prior_effect {
        calls.push(call("prior", "write", json!({"prior":true})));
    }
    calls.extend([
        call("first", "write", json!({})),
        call("sibling", "other", json!({})),
    ]);
    let mut h = Harness::new(pool, vec![turn("", calls)]).await;
    let n = h.add(Probe::new("write", SideEffect::WorkspaceWrite));
    let sibling = h.add(Probe::new("other", SideEffect::Read));
    let _clock = Clock::pause();
    let facts = interrupted_run(&h, interruption, usize::from(prior_effect)).await;
    assert_eq!(facts.stop, Some(reason));
    assert_eq!(n.load(Ordering::SeqCst), usize::from(prior_effect));
    assert_eq!(sibling.load(Ordering::SeqCst), 0);
    let rows = h.rows().await;
    for row in &rows[usize::from(prior_effect)..] {
        assert_eq!(row.get::<String, _>("state"), "refused");
        assert_eq!(row.get::<String, _>("result"), reason.wire());
        assert!(
            row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at")
                .is_some()
        );
    }
    assert!(
        facts.tool_calls[usize::from(prior_effect)..]
            .iter()
            .all(|c| c.state == ToolCallState::Refused)
    );
    assert_eq!(h.effects().await, prior_effect);
    assert_eq!(rows.len(), 2 + usize::from(prior_effect));
    assert_tool_closed(&h, facts.tool_calls[usize::from(prior_effect)].id, reason).await;
}
async fn assert_tool_closed(h: &Harness, id: ToolCallRecordId, reason: StopReason) {
    let events: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM events WHERE run_id=$1 ORDER BY seq")
            .bind(h.input.run_id)
            .fetch_all(&h.pool)
            .await
            .unwrap();
    let id = id.to_string();
    let call_events: Vec<_> = events
        .iter()
        .filter(|e| e["toolCallId"] == id || e["value"]["toolCallId"] == id)
        .collect();
    assert_eq!(
        call_events
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "TOOL_CALL_START",
            "TOOL_CALL_ARGS",
            "TOOL_CALL_END",
            "TOOL_CALL_RESULT",
            "CUSTOM"
        ]
    );
    assert_eq!(call_events[3]["content"], reason.wire());
    assert_eq!(call_events[4]["name"], "katwork.tool_status");
    assert_eq!(call_events[4]["value"]["ok"], false);
    assert_eq!(call_events[4]["value"]["code"], reason.wire());
    if reason == StopReason::Cancelled {
        assert_eq!(events.last().unwrap()["type"], "RUN_ERROR");
    }
}
#[sqlx::test]
async fn deadline_before_tool_poll_closes_started_row(pool: PgPool) {
    before_tool_poll(pool, Interruption::Deadline, StopReason::TimeLimit, false).await;
}
#[sqlx::test]
async fn cancel_before_tool_poll_refuses_without_effects(pool: PgPool) {
    before_tool_poll(pool, Interruption::Cancel, StopReason::Cancelled, false).await;
}
#[sqlx::test]
async fn pause_before_tool_poll_refuses_without_effects(pool: PgPool) {
    before_tool_poll(pool, Interruption::ToolPause, StopReason::KatPaused, false).await;
}
#[sqlx::test]
async fn undispatched_call_preserves_prior_effect_marker(pool: PgPool) {
    before_tool_poll(pool, Interruption::Deadline, StopReason::TimeLimit, true).await;
}
async fn stopped_siblings(pool: PgPool, halted: bool) {
    let mut h = Harness::new(
        pool,
        vec![turn(
            "",
            vec![
                call("first", "write", json!({})),
                call("second", "other", json!({})),
            ],
        )],
    )
    .await;
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(move |_, _| {
            async move {
                Err(if halted {
                    ToolError::Halted(StopReason::PolicyChanged)
                } else {
                    ToolError::OutcomeUnknown
                })
            }
            .boxed()
        }),
    );
    let n = h.add(Probe::new("other", SideEffect::Read));
    let facts = h.run().await.unwrap();
    let reason = if halted {
        StopReason::PolicyChanged
    } else {
        StopReason::OutcomeUnknown
    };
    assert_eq!(facts.stop, Some(reason));
    assert_eq!(n.load(Ordering::SeqCst), 0);
    let rows = h.rows().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].get::<String, _>("tool_call_id"), "second");
    assert_eq!(rows[1].get::<String, _>("state"), "refused");
    assert_eq!(rows[1].get::<String, _>("result"), reason.wire());
    assert_eq!(facts.tool_calls[1].state, ToolCallState::Refused);
}
#[sqlx::test]
async fn halted_calls_audit_unexecuted_siblings(pool: PgPool) {
    stopped_siblings(pool, true).await;
}
#[sqlx::test]
async fn unknown_calls_audit_unexecuted_siblings(pool: PgPool) {
    stopped_siblings(pool, false).await;
}

async fn invalid_shape(pool: PgPool, case: &str) {
    for stop in [
        StopKind::MaxTokens,
        StopKind::Refusal,
        StopKind::Other("unknown".into()),
        StopKind::EndTurn,
        StopKind::ToolUse,
    ] {
        let calls = match case {
            "count" => (0..65)
                .map(|i| call(&i.to_string(), "probe", json!({"i":i})))
                .collect(),
            "duplicate" => vec![
                call("same", "probe", json!({})),
                call("same", "probe", json!({})),
            ],
            "empty" => vec![call("", "probe", json!({}))],
            "id_length" => vec![call(&"a".repeat(257), "probe", json!({}))],
            "charset" => vec![call("id\0", "probe", json!({}))],
            "name_length" => vec![call("a", &"n".repeat(129), json!({}))],
            _ => unreachable!(),
        };
        let mut h = Harness::new(pool.clone(), vec![ending("partial", calls, stop)]).await;
        let n = h.add(Probe::new("probe", SideEffect::Read));
        let facts = h
            .run()
            .await
            .expect("bad model shape must not become an infrastructure abort");
        assert_eq!(facts.stop, Some(StopReason::ModelProtocol), "case {case}");
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert!(h.rows().await.is_empty(), "case {case} wrote ledger rows");
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
#[sqlx::test]
async fn turn_call_cap_applies_to_every_stop_kind(pool: PgPool) {
    invalid_shape(pool.clone(), "count").await;
    shape_limits_accept_exact_boundaries(pool).await;
}
#[sqlx::test]
async fn duplicate_call_ids_rejected_before_every_turn_kind(pool: PgPool) {
    invalid_shape(pool, "duplicate").await;
}
#[sqlx::test]
async fn empty_call_ids_rejected_before_every_turn_kind(pool: PgPool) {
    invalid_shape(pool, "empty").await;
}
#[sqlx::test]
async fn call_id_length_cap_applies_to_every_stop_kind(pool: PgPool) {
    invalid_shape(pool, "id_length").await;
}
#[sqlx::test]
async fn call_id_charset_applies_to_every_stop_kind(pool: PgPool) {
    invalid_shape(pool, "charset").await;
}
#[sqlx::test]
async fn tool_name_length_cap_applies_to_every_stop_kind(pool: PgPool) {
    invalid_shape(pool, "name_length").await;
}
#[sqlx::test]
async fn call_ids_require_printable_ascii(pool: PgPool) {
    for id in [
        "with space",
        "with\nnewline",
        "with\t tab",
        "ไทย",
        "delete\u{7f}",
    ] {
        let mut h = Harness::new(
            pool.clone(),
            vec![turn("", vec![call(id, "probe", json!({}))])],
        )
        .await;
        let n = h.add(Probe::new("probe", SideEffect::Read));
        assert_eq!(h.run().await.unwrap().stop, Some(StopReason::ModelProtocol));
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert!(h.rows().await.is_empty());
    }
}
async fn shape_limits_accept_exact_boundaries(pool: PgPool) {
    for stop in [
        StopKind::MaxTokens,
        StopKind::Refusal,
        StopKind::Other("unknown".into()),
        StopKind::EndTurn,
        StopKind::ToolUse,
    ] {
        let calls = (0..64)
            .map(|i| call(&format!("{i:0256}"), &"n".repeat(128), json!({})))
            .collect();
        let h = Harness::new(
            pool.clone(),
            vec![ending("partial", calls, stop), turn("ok", vec![])],
        )
        .await;
        h.run().await.unwrap();
        assert_eq!(h.rows().await.len(), 64);
    }
}
#[sqlx::test]
async fn stale_kat_snapshot_cannot_use_new_policy_version(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "write", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let n = h.add(Probe::new("write", SideEffect::WorkspaceWrite));
    let updated = kat::update(
        &h.pool,
        &h.input.tenant,
        h.kat.id,
        KatPatch {
            tool_allowlist: Some(vec![]),
            ..KatPatch::default()
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    h.input.policy_version = updated.policy_version;
    assert_eq!(h.run().await.unwrap().stop, Some(StopReason::PolicyChanged));
    assert!(h.fake.received().is_empty());
    assert_eq!(n.load(Ordering::SeqCst), 0);
    assert!(!h.effects().await);
}
#[sqlx::test]
async fn caller_cannot_override_space_residency(pool: PgPool) {
    let mut h = Harness::new(pool, vec![turn("local", vec![])]).await;
    h.input.space_policy.allow_external = true;
    let hosted = FakeProvider::scripted(vec![turn("leak", vec![])]);
    let mut external = entry(&hosted);
    external.capabilities.residency = Residency::External;
    h.models = Registry::from_entries(vec![
        (external, Arc::new(hosted.clone())),
        (entry(&h.fake), Arc::new(h.fake.clone())),
    ]);
    assert_eq!(h.run().await.unwrap().final_text.as_deref(), Some("local"));
    assert!(hosted.received().is_empty());
}

#[sqlx::test]
async fn workspace_pause_waits_for_authorized_write(pool: PgPool) {
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
    let pause = async {
        entered.notified().await;
        let mut update = Box::pin(tenancy::workspace::set_paused(
            &h.pool,
            &h.input.tenant,
            true,
            std::time::Duration::from_secs(5),
        ));
        loop {
            tokio::select! {
                result=&mut update=>panic!("pause passed open write: {result:?}"),
                waiting=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'UPDATE workspaces%')").fetch_one(&h.pool)=>if waiting.unwrap(){break;}
            }
        }
        release.notify_one();
        update.await.unwrap();
        edited.notify_one();
    };
    let (facts, ()) = tokio::join!(h.run(), pause);
    assert_eq!(facts.unwrap().stop, Some(StopReason::WorkspacePaused));
    assert_eq!(count_effects(&h.pool).await, 1);
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

async fn retry_interruption(
    pool: PgPool,
    interruption: Interruption,
    reason: StopReason,
    timed_out: bool,
) {
    let mut h = Harness::new(pool, vec![turn("", vec![call("a", "probe", json!({}))])]).await;
    let n = h.add(Probe::new("probe", SideEffect::Read).action(move |_, _| {
        async move {
            Err(if timed_out {
                ToolError::Timeout
            } else {
                ToolError::OutcomeUnknown
            })
        }
        .boxed()
    }));
    sqlx::raw_sql("CREATE FUNCTION block_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
        IF NEW.attempts=2 THEN PERFORM pg_advisory_xact_lock(38); END IF; RETURN NEW; END $$;
        CREATE TRIGGER block_retry BEFORE UPDATE ON tool_calls FOR EACH ROW EXECUTE FUNCTION block_retry();")
        .execute(&h.pool).await.unwrap();
    let mut blocker = h.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(38)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let _clock = Clock::pause();
    let interrupt = async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'UPDATE tool_calls%')").fetch_one(&h.pool).await.unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
        match interruption {
            Interruption::Deadline => tokio::time::advance(Duration::from_secs(91)).await,
            Interruption::Cancel => h.input.cancel.cancel(),
            Interruption::ToolPause => {
                kat::update(
                    &h.pool,
                    &h.input.tenant,
                    h.kat.id,
                    KatPatch {
                        status: Some(KatStatus::Paused),
                        ..KatPatch::default()
                    },
                    std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            }
            Interruption::ModelPause | Interruption::ModelDeadline | Interruption::ModelCancel => {
                unreachable!()
            }
        }
        blocker.rollback().await.unwrap();
    };
    let (facts, ()) = tokio::join!(h.run(), interrupt);
    let facts = facts.unwrap();
    assert_eq!(facts.stop, Some(reason));
    assert_eq!(n.load(Ordering::SeqCst), 1);
    let rows = h.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>("attempts"), 2);
    assert_eq!(rows[0].get::<String, _>("state"), "outcome_unknown");
    assert_eq!(rows[0].get::<String, _>("result"), reason.wire());
    assert!(
        rows[0]
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at")
            .is_some()
    );
    assert_eq!(facts.tool_calls[0].state, ToolCallState::OutcomeUnknown);
    assert!(!h.effects().await);
    assert_tool_closed(&h, facts.tool_calls[0].id, reason).await;
}
#[sqlx::test]
async fn deadline_during_read_retry_closes_row(pool: PgPool) {
    retry_interruption(pool, Interruption::Deadline, StopReason::TimeLimit, false).await;
}
#[sqlx::test]
async fn cancel_during_read_retry_preserves_unknown_outcome(pool: PgPool) {
    retry_interruption(pool, Interruption::Cancel, StopReason::Cancelled, false).await;
}
#[sqlx::test]
async fn pause_during_read_retry_preserves_unknown_outcome(pool: PgPool) {
    retry_interruption(pool, Interruption::ToolPause, StopReason::KatPaused, false).await;
}
#[sqlx::test]
async fn cancelled_read_retry_after_timeout_preserves_unknown_outcome(pool: PgPool) {
    retry_interruption(pool, Interruption::Cancel, StopReason::Cancelled, true).await;
}

#[sqlx::test]
async fn configured_timeout_reaches_ledger_transactions(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.lock_timeout = Duration::from_millis(37);
    sqlx::raw_sql("CREATE TABLE observed_timeout(value text);
        CREATE FUNCTION observe_timeout() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
        INSERT INTO observed_timeout VALUES(current_setting('lock_timeout')); RETURN NEW; END $$;
        CREATE TRIGGER tool_timeout AFTER INSERT OR UPDATE ON tool_calls FOR EACH ROW EXECUTE FUNCTION observe_timeout();
        CREATE TRIGGER model_timeout AFTER INSERT ON model_calls FOR EACH ROW EXECUTE FUNCTION observe_timeout();
        CREATE TRIGGER run_timeout AFTER INSERT ON runs FOR EACH ROW EXECUTE FUNCTION observe_timeout();")
        .execute(&h.pool).await.unwrap();
    h.add(Probe::new("probe", SideEffect::Read));
    h.run().await.unwrap();
    let observed: Vec<String> = sqlx::query_scalar("SELECT value FROM observed_timeout")
        .fetch_all(&h.pool)
        .await
        .unwrap();
    assert!(observed.len() >= 5);
    assert!(
        observed.iter().all(|value| value == "37ms"),
        "observed {observed:?}"
    );
}
#[sqlx::test]
async fn configured_timeout_reaches_tool_write_transaction(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "write", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.lock_timeout = Duration::from_millis(37);
    h.add(
        Probe::new("write", SideEffect::WorkspaceWrite).action(|ctx, _| {
            async move {
                let mut tx = ctx.pool.begin().await.unwrap();
                assert_eq!(guards::authorize_write(&mut tx, &ctx).await.unwrap(), None);
                let timeout: String = sqlx::query_scalar("SHOW lock_timeout")
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
                assert_eq!(timeout, "37ms");
                tx.commit().await.unwrap();
                Ok(ToolOutput::Text("ok".into()))
            }
            .boxed()
        }),
    );
    h.run().await.unwrap();
}

struct BatchRefusalSink<'a>(&'a Harness);
#[async_trait]
impl EventSink for BatchRefusalSink<'_> {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        if let AgUiEvent::ToolCallStart {
            tool_call_id,
            tool_call_name,
            ..
        } = &event
            && tool_call_name == "other"
        {
            let state: String = sqlx::query_scalar("SELECT state FROM tool_calls WHERE id=$1")
                .bind(tool_call_id.parse::<uuid::Uuid>().unwrap())
                .fetch_one(&self.0.pool)
                .await?;
            assert_eq!(
                state, "refused",
                "batch guard must refuse before starting a receipt"
            );
            assert!(!self.0.effects().await);
        }
        self.0.sink.emit(event).await
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.0.sink.close_run(end, code).await
    }
}
#[sqlx::test]
async fn batch_authority_refuses_before_starting_next_call(pool: PgPool) {
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
    let n = h.add(Probe::new("other", SideEffect::WorkspaceWrite));
    let facts = run::execute(
        &RunEnv {
            lock_timeout: std::time::Duration::from_secs(5),
            pool: &h.pool,
            models: &h.models,
            tools: &h.tools,
            sink: &BatchRefusalSink(&h),
        },
        &h.kat,
        h.input.clone(),
        &h.limits,
    )
    .await
    .unwrap();
    assert_eq!(facts.stop, Some(StopReason::PolicyChanged));
    assert_eq!(n.load(Ordering::SeqCst), 0);
    let rows = h.rows().await;
    assert_eq!(rows[1].get::<String, _>("state"), "refused");
    assert_eq!(rows[1].get::<String, _>("result"), "policy_changed");
}
