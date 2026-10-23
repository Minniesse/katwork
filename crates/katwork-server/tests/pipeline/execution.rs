use super::*;
use eventsource_stream::Eventsource;
use futures::StreamExt;

pub(super) fn answer(draft: &str, final_text: &str) -> FakeTurn {
    FakeTurn::Chunks(vec![
        ChatChunk::TextDelta(draft.into()),
        ChatChunk::Done {
            stop: StopKind::EndTurn,
            turn: AssistantTurn {
                content: final_text.into(),
                tool_calls: vec![],
                replay: None,
            },
        },
    ])
}
fn kind(event: &Value) -> &str {
    event["name"]
        .as_str()
        .unwrap_or_else(|| event["type"].as_str().unwrap())
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn full_event_sequence(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let response = f
        .call("GET", &format!("threads/{}/events", f.thread), None)
        .await;
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream().eventsource();
    let (_, job) = f.submit("hello").await;
    let mut workers = worker::spawn_pool(f.core.clone(), 1, Duration::from_secs(60));
    let events = timeout(Duration::from_secs(15), async {
        let mut received = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if event.event == "ping" {
                continue;
            }
            let value: Value = serde_json::from_str(&event.data).unwrap();
            let done = value["type"] == "RUN_FINISHED";
            received.push(value);
            if done {
                return received;
            }
        }
        panic!("stream ended before the answered job finished");
    })
    .await
    .expect("event stream stalled");
    f.core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    assert_eq!(f.status(job).await.0.as_deref(), Some("answered"));
    assert_eq!(events, f.events().await);
    let kinds: Vec<_> = events
        .iter()
        .map(kind)
        .filter(|k| *k != "katwork.usage")
        .collect();
    assert_eq!(
        kinds,
        vec![
            "katwork.message_stored",
            "katwork.job_status",
            "RUN_STARTED",
            "katwork.run_started",
            "katwork.kat_state",
            "TEXT_MESSAGE_START",
            "TEXT_MESSAGE_CONTENT",
            "TEXT_MESSAGE_END",
            "katwork.decision",
            "katwork.message_stored",
            "katwork.job_status",
            "katwork.kat_state",
            "katwork.run_closed",
            "RUN_FINISHED"
        ]
    );
    assert_eq!(
        events
            .iter()
            .rfind(|e| kind(e) == "katwork.job_status")
            .unwrap()["value"]["status"],
        "answered"
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn demo_mode_reply_is_flagged_synthetic(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    let page = body(
        f.call("GET", &format!("threads/{}/messages", f.thread), None)
            .await,
    )
    .await;
    assert_eq!(page["messages"][1]["synthetic"], true);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn stream_message_id_equals_stored_reply_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    let events = f.events().await;
    let started = events
        .iter()
        .find(|e| kind(e) == "TEXT_MESSAGE_START")
        .unwrap();
    let stored = events
        .iter()
        .rfind(|e| kind(e) == "katwork.message_stored")
        .unwrap();
    assert_eq!(started["messageId"], stored["value"]["messageId"]);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn stored_reply_snapshot_replaces_provisional_text(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![answer("Draft", "Final")]));
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    let events = f.events().await;
    assert_eq!(
        events
            .iter()
            .find(|e| kind(e) == "TEXT_MESSAGE_CONTENT")
            .unwrap()["delta"],
        "Draft"
    );
    assert_eq!(
        events
            .iter()
            .rfind(|e| kind(e) == "katwork.message_stored")
            .unwrap()["value"]["body"],
        "Final"
    );
    let page = message::list(&pool, &f.tenant, f.thread, None, 50)
        .await
        .unwrap();
    assert_eq!(page.messages[1].body, "Final");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn run_started_carries_the_job_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, a) = f.submit("one").await;
    let (_, b) = f.submit("two").await;
    let mut workers = worker::spawn_pool(f.core.clone(), 2, Duration::from_secs(60));
    timeout(Duration::from_secs(15), async {
        while f.status(a).await.0.is_none() || f.status(b).await.0.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.core.shutdown.cancel();
    while let Some(r) = workers.join_next().await {
        r.unwrap();
    }
    let events = f.events().await;
    let mut jobs: Vec<JobId> = events
        .iter()
        .filter(|e| kind(e) == "katwork.run_started")
        .map(|e| serde_json::from_value(e["value"]["jobId"].clone()).unwrap())
        .collect();
    jobs.sort_by_key(|j| j.0);
    let mut expected = vec![a, b];
    expected.sort_by_key(|j| j.0);
    assert_eq!(jobs, expected);
}
async fn stopped(f: &Fixture, reason: &str) {
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    assert_eq!(
        f.status(job).await,
        (Some("stopped".into()), Some(reason.into()))
    );
    assert_eq!(f.notices().await[0]["code"], reason);
    let events = f.events().await;
    let at = events
        .iter()
        .position(|e| kind(e) == "katwork.run_closed")
        .unwrap();
    assert_eq!(events[at]["value"]["code"], reason);
    assert_eq!(kind(&events[at + 1]), "RUN_FINISHED");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn provider_unavailable_ends_job_stopped_with_reason(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![
        FakeTurn::Error(
            ModelError::ProviderUnavailable
        );
        2
    ]));
    stopped(&f, "provider_unavailable").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn provider_quota_and_rate_limit_end_stopped_with_named_reason(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![FakeTurn::Error(
        ModelError::QuotaExceeded { resets_at: None },
    )]));
    stopped(&f, "provider_quota").await;
    // A new Core resets shutdown; the same thread retains the first notice.
    f.provider(FakeProvider::scripted(vec![
        FakeTurn::Error(
            ModelError::RateLimited {
                retry_after_secs: None
            }
        );
        2
    ]));
    let (_, job) = f.submit("again").await;
    f.run(job).await;
    assert_eq!(
        f.status(job).await.1.as_deref(),
        Some("provider_rate_limited")
    );
    assert_eq!(
        f.notices().await.last().unwrap()["code"],
        "provider_rate_limited"
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn local_only_space_with_only_hosted_keys_stops_residency_denied(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let fake = FakeProvider::scripted(vec![answer("bad", "bad")]);
    let registry = Registry::from_entries(vec![(
        ModelEntry {
            provider_id: "hosted".into(),
            model: "fake".into(),
            tier: f.kats[0].model_tier,
            capabilities: Capabilities {
                tools: true,
                residency: Residency::External,
                synthetic: false,
            },
            max_output_tokens: 100,
            temperature: None,
            price: None,
            timeouts: ModelTimeouts::HOSTED,
        },
        Arc::new(fake.clone()),
    )]);
    f.core = Arc::new(Core::new(pool.clone(), f.core.config.clone()).with_registry(registry));
    f.app = build_router(AppState {
        core: f.core.clone(),
    });
    stopped(&f, "residency_denied").await;
    assert!(fake.received().is_empty());
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn restart_mid_run_posts_exactly_one_reply(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let c = super::cancel::claim(&f).await;
    let (old, _, _) = super::cancel::start_run(&f, &c).await;
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE task_id=$1",
    )
    .bind(c.task_id)
    .execute(&pool)
    .await
    .unwrap();
    f.run(job).await;
    assert_eq!(f.status(job).await.0.as_deref(), Some("answered"));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE job_id=$1 AND author_kind='kat'")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let events = f.events().await;
    assert_eq!(
        events
            .iter()
            .filter(|e| e["name"] == "katwork.run_closed" && e["value"]["runId"] == json!(old))
            .count(),
        1
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn follow_up_to_approval_pending_job_never_dispatches_the_pending_call(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let parent = f.awaiting().await;
    sqlx::query("UPDATE jobs SET reason='approval_pending' WHERE id=$1")
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
    let pending = json!({"name":"send","args_json":"{}"});
    sqlx::query("UPDATE tasks SET pending_tool_call=$2 WHERE job_id=$1")
        .bind(parent)
        .bind(&pending)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("yes").await;
    f.run(job).await;
    assert_eq!(f.status(job).await.0.as_deref(), Some("answered"));
    let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM tool_calls")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(calls, 0);
    let saved: Value = sqlx::query_scalar("SELECT pending_tool_call FROM tasks WHERE job_id=$1")
        .bind(parent)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, pending);
}
