#[path = "events/protocol.rs"]
mod protocol;
#[allow(dead_code)]
mod support;

use std::time::Duration;

use katwork::{
    events::{AgUiEvent, DeltaCoalescer, EventHub, EventSink, PgEventSink, RunEnd, log},
    fence::Fence,
    ids::{EventSeq, LeaseEpoch, RunId, TaskId, ThreadId, UserId, WorkspaceId},
    tenancy::{self, Tenant},
};
use sqlx::PgPool;
use support::Fixture;
use tokio::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

pub const SKIPPED_FIXTURES: &[(&str, &str)] = &[
    (
        "RunFinishedEvent/valid/result-array.json",
        "reply is the stored message",
    ),
    (
        "RunFinishedEvent/valid/result-number.json",
        "reply is the stored message",
    ),
    (
        "RunFinishedEvent/valid/result-object.json",
        "reply is the stored message",
    ),
    (
        "RunFinishedEvent/valid/result-string.json",
        "reply is the stored message",
    ),
    (
        "RunFinishedEvent/valid/usage.json",
        "usage is per attempt in katwork.usage",
    ),
    (
        "RunFinishedEvent/valid/usage-cache-breakdown.json",
        "usage is per attempt in katwork.usage",
    ),
    (
        "RunErrorEvent/valid/usage-cache-breakdown.json",
        "usage is per attempt in katwork.usage",
    ),
    (
        "RunStartedEvent/valid/with-input.json",
        "runs start from intake",
    ),
    (
        "TextMessageContentEvent/valid/attributed-to-a-subagent.json",
        "SP1 has no subagents",
    ),
    (
        "TextMessageContentEvent/valid/metadata-empty.json",
        "identity and order use the thread sequence",
    ),
    (
        "TextMessageContentEvent/valid/metadata-reserved-key.json",
        "identity and order use the thread sequence",
    ),
    (
        "TextMessageContentEvent/valid/metadata-value-null.json",
        "identity and order use the thread sequence",
    ),
    (
        "TextMessageEndEvent/valid/metadata-prototype-named-key.json",
        "identity and order use the thread sequence",
    ),
    (
        "TextMessageEndEvent/valid/raw-event-array.json",
        "raw provider events are never sent",
    ),
    (
        "TextMessageEndEvent/valid/raw-event-number.json",
        "raw provider events are never sent",
    ),
    (
        "TextMessageEndEvent/valid/raw-event-object.json",
        "raw provider events are never sent",
    ),
    (
        "TextMessageEndEvent/valid/raw-event-string.json",
        "raw provider events are never sent",
    ),
    (
        "TextMessageEndEvent/valid/timestamp-at-safe-integer-maximum.json",
        "identity and order use the thread sequence",
    ),
    (
        "TextMessageEndEvent/valid/timestamp-at-safe-integer-minimum.json",
        "identity and order use the thread sequence",
    ),
    (
        "ToolCallResultEvent/valid/file-source-part.json",
        "tool results are text",
    ),
    (
        "ToolCallResultEvent/valid/parts-content.json",
        "tool results are text",
    ),
];

fn delta(text: &str) -> AgUiEvent {
    AgUiEvent::TextMessageContent {
        message_id: "reply".into(),
        delta: text.into(),
    }
}

async fn tenant(pool: &PgPool, f: &Fixture) -> Tenant {
    tenancy::resolve(pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap()
}

fn sink(pool: &PgPool, f: &Fixture) -> PgEventSink {
    PgEventSink::new(
        pool.clone(),
        EventHub::new(),
        Duration::from_millis(200),
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(RunId(f.run)),
        Some(Fence {
            task_id: TaskId(f.task),
            epoch: LeaseEpoch(1),
        }),
    )
}

async fn events(pool: &PgPool, f: &Fixture) -> Vec<(EventSeq, AgUiEvent)> {
    log::since(
        pool,
        &tenant(pool, f).await,
        ThreadId(f.thread),
        EventSeq(0),
        10_000,
    )
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_appenders_never_skip_on_resume(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let a = sink(&pool, &f);
    let b = sink(&pool, &f);
    let (left, right) = tokio::join!(
        async {
            for _ in 0..50 {
                a.emit(delta("a")).await.unwrap();
            }
        },
        async {
            for _ in 0..50 {
                b.emit(delta("b")).await.unwrap();
            }
        },
    );
    assert_eq!((left, right), ((), ()));
    let t = tenant(&pool, &f).await;
    for cursor in 0..100 {
        let found = log::since(&pool, &t, ThreadId(f.thread), EventSeq(cursor), 500)
            .await
            .unwrap();
        assert_eq!(
            found.iter().map(|e| e.0.0).collect::<Vec<_>>(),
            ((cursor + 1)..=100).collect::<Vec<_>>()
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn since_pages_without_gap_or_duplicate(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    for _ in 0..5000 {
        log::append(
            &mut tx,
            WorkspaceId(f.workspace),
            ThreadId(f.thread),
            None,
            &delta("x"),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let writer = sink(&pool, &f);
    let writer = tokio::spawn(async move {
        for _ in 0..100 {
            writer.emit(delta("y")).await.unwrap();
        }
    });
    let t = tenant(&pool, &f).await;
    let mut seq = EventSeq(0);
    while seq.0 < 5100 {
        for (next, _) in log::since(&pool, &t, ThreadId(f.thread), seq, 500)
            .await
            .unwrap()
        {
            assert_eq!(next.0, seq.0 + 1);
            seq = next;
        }
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();
    assert_eq!(log::tail(&pool, &t, ThreadId(f.thread)).await.unwrap(), seq);
}

#[sqlx::test(migrations = "./migrations")]
async fn close_run_is_atomic_with_its_terminal_event(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    f.tool_call(&pool, 0, Some("read"), "started")
        .await
        .unwrap();
    sqlx::query("ALTER TABLE events ADD CONSTRAINT fail_terminal CHECK (kind <> 'RUN_ERROR')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sink(&pool, &f)
            .close_run(RunEnd::Interrupted, "lease_lost")
            .await
            .is_err()
    );
    assert!(events(&pool, &f).await.is_empty());
    let state: (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT state, ended_at FROM runs WHERE id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("running".into(), None));
    let call: (String, bool) =
        sqlx::query_as("SELECT state, ended_at IS NULL FROM tool_calls WHERE run_id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(call, ("started".into(), true));
    assert_eq!(
        log::tail(&pool, &tenant(&pool, &f).await, ThreadId(f.thread))
            .await
            .unwrap(),
        EventSeq(0)
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn run_closed_immediately_precedes_its_terminal_event(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    f.tool_call(&pool, 0, Some("read"), "started")
        .await
        .unwrap();
    let other = RunId::new();
    sqlx::query("INSERT INTO runs (id,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state) SELECT $1,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state FROM runs WHERE id=$2")
        .bind(other).bind(f.run).execute(&pool).await.unwrap();
    let a = sink(&pool, &f);
    let b = PgEventSink::new(
        pool.clone(),
        EventHub::new(),
        Duration::from_secs(5),
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        Some(other),
        Some(Fence {
            task_id: TaskId(f.task),
            epoch: LeaseEpoch(1),
        }),
    );
    let (a, b) = tokio::join!(
        a.close_run(RunEnd::Cancelled, "cancelled"),
        b.close_run(RunEnd::Interrupted, "lease_lost")
    );
    a.unwrap();
    b.unwrap();
    let all = events(&pool, &f).await;
    assert_eq!(all.len(), 6);
    let mut closed = 0;
    for pair in all.windows(2) {
        if let AgUiEvent::Custom { name, value } = &pair[0].1
            && name == "katwork.run_closed"
        {
            closed += 1;
            assert_eq!(pair[1].0.0, pair[0].0.0 + 1);
            assert!(
                matches!(&pair[1].1, AgUiEvent::RunError { message, code: Some(code) } if code == value["code"].as_str().unwrap() && message == code)
            );
        }
    }
    assert_eq!(closed, 2);
    let call: (String, bool, String) = sqlx::query_as(
        "SELECT state, ended_at IS NOT NULL, result FROM tool_calls WHERE run_id=$1",
    )
    .bind(f.run)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(call, ("outcome_unknown".into(), true, "cancelled".into()));
    assert_eq!(
        sink(&pool, &f)
            .close_run(RunEnd::Cancelled, "cancelled")
            .await
            .unwrap_err()
            .code(),
        "lease_lost"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn run_error_message_is_the_code(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    sink(&pool, &f)
        .close_run(RunEnd::Interrupted, "lease_lost")
        .await
        .unwrap();
    assert!(
        matches!(&events(&pool, &f).await[1].1, AgUiEvent::RunError { message, code: Some(code) } if message == "lease_lost" && message == code)
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn fenced_sink_rejects_writes_after_epoch_change(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    sqlx::query("UPDATE tasks SET lease_epoch=2 WHERE id=$1")
        .bind(f.task)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sink(&pool, &f)
            .emit(delta("late"))
            .await
            .unwrap_err()
            .code(),
        "lease_lost"
    );
    assert!(events(&pool, &f).await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn nul_in_delta_does_not_fail_append(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    sink(&pool, &f).emit(delta("a\0b")).await.unwrap();
    assert_eq!(events(&pool, &f).await[0].1, delta("a\u{fffd}b"));
}

#[sqlx::test(migrations = "./migrations")]
async fn sink_lock_wait_is_bounded(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM threads WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.thread)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        sink(&pool, &f)
            .emit(delta("blocked"))
            .await
            .unwrap_err()
            .code(),
        "busy"
    );
    tx.rollback().await.unwrap();
    assert!(events(&pool, &f).await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn log_is_tenant_scoped_and_rollback_does_not_consume_seq(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let foreign = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        log::append(
            &mut tx,
            WorkspaceId(f.workspace),
            ThreadId(foreign.thread),
            None,
            &delta("x")
        )
        .await
        .unwrap_err()
        .code(),
        "not_found"
    );
    tx.rollback().await.unwrap();
    let t = tenant(&pool, &f).await;
    assert_eq!(
        log::since(&pool, &t, ThreadId(foreign.thread), EventSeq(0), 10)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    let mut tx = pool.begin().await.unwrap();
    log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        None,
        &delta("rolled back"),
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let seq = log::append(
        &mut tx,
        WorkspaceId(f.workspace),
        ThreadId(f.thread),
        None,
        &AgUiEvent::RunStarted {
            thread_id: f.thread.to_string(),
            run_id: f.run.to_string(),
            protocol_version: Some("1.0".into()),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(seq, EventSeq(1));
    let kind: String = sqlx::query_scalar("SELECT kind FROM events WHERE thread_id=$1")
        .bind(f.thread)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "RUN_STARTED");
}

fn check_splits(text: &str) {
    for split in (0..=text.len()).filter(|i| text.is_char_boundary(*i)) {
        let mut c = DeltaCoalescer::new();
        let now = Instant::now();
        let mut chunks = Vec::new();
        for (index, piece) in [&text[..split], &text[split..]].into_iter().enumerate() {
            if let Some(chunk) =
                c.push(piece, now + Duration::from_millis(101 * (index as u64 + 1)))
            {
                assert!(!chunk.ends_with(['เ', 'แ', 'โ', 'ใ', 'ไ']));
                chunks.push(chunk);
            }
        }
        chunks.extend(c.finish());
        assert_eq!(chunks.concat(), text);
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let mut offset = 0;
        for chunk in chunks {
            offset += chunk.len();
            assert!(
                boundaries.contains(&offset),
                "split {split}, offset {offset}"
            );
        }
        assert!(c.finish().is_none());
    }
}

#[test]
fn thai_delta_never_splits_a_grapheme_cluster() {
    check_splits("ที่นี่สวัสดีครับ 👋🏽");
}

#[test]
fn coalescer_holds_back_trailing_cluster() {
    for text in ["👋🏽", "🇹🇭", "เก", "👨‍👩‍👧", "เก่ง"] {
        check_splits(text);
    }
}

#[test]
fn coalescer_releases_on_time_or_size_and_flushes_at_end() {
    let mut c = DeltaCoalescer::new();
    let now = Instant::now();
    assert!(c.push("abc", now).is_none());
    assert_eq!(
        c.push("", now + Duration::from_millis(100)),
        Some("ab".into())
    );
    assert_eq!(c.finish(), Some("c".into()));
    let mut c = DeltaCoalescer::new();
    assert_eq!(c.push(&"x".repeat(512), Instant::now()).unwrap().len(), 511);
    assert_eq!(c.finish(), Some("x".into()));
}

#[test]
fn hub_drops_sender_when_last_receiver_leaves() {
    let hub = EventHub::new();
    let thread = ThreadId::new();
    let a = hub.subscribe(thread);
    let b = hub.subscribe(thread);
    drop(a);
    assert_eq!(hub.sender_count(), 1);
    drop(b);
    assert_eq!(hub.sender_count(), 0);
    hub.notify(thread, EventSeq(1));
    assert_eq!(hub.sender_count(), 0);
}

#[test]
fn hub_senders_stay_bounded_across_quiet_threads() {
    let hub = EventHub::new();
    for _ in 0..1000 {
        drop(hub.subscribe(ThreadId::new()));
    }
    assert_eq!(hub.sender_count(), 0);
    let held: Vec<_> = (0..3).map(|_| hub.subscribe(ThreadId::new())).collect();
    assert_eq!(hub.sender_count(), 3);
    drop(held);
    assert_eq!(hub.sender_count(), 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn emit_refuses_terminal_events(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    for event in [
        AgUiEvent::RunError {
            message: "diagnostic".into(),
            code: Some("provider_unavailable".into()),
        },
        AgUiEvent::RunFinished {
            thread_id: f.thread.to_string(),
            run_id: f.run.to_string(),
            outcome: None,
        },
    ] {
        assert_eq!(
            sink(&pool, &f).emit(event).await.unwrap_err().code(),
            "request_invalid"
        );
    }
    assert!(events(&pool, &f).await.is_empty());
    assert_eq!(
        log::tail(&pool, &tenant(&pool, &f).await, ThreadId(f.thread))
            .await
            .unwrap(),
        EventSeq(0)
    );
}
