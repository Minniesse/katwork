use super::*;
use std::collections::HashMap;

// Normalize generated identities consistently; every field, value and sequence stays otherwise exact.
fn normalize(value: &mut Value, ids: &mut HashMap<String, String>) {
    match value {
        Value::String(text) if uuid::Uuid::parse_str(text).is_ok() => {
            let next = format!("00000000-0000-7000-8000-{:012}", ids.len() + 1);
            *text = ids.entry(text.clone()).or_insert(next).clone();
        }
        Value::Array(values) => values.iter_mut().for_each(|v| normalize(v, ids)),
        Value::Object(fields) => {
            // Object insertion order is not part of JSON's wire contract.
            let mut keys: Vec<_> = fields.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if key == "createdAt" {
                    assert!(fields[&key].is_string(), "createdAt must remain a string");
                    fields[&key] = json!("2026-10-23T00:00:00Z");
                } else {
                    normalize(&mut fields[&key], ids);
                }
            }
        }
        _ => {}
    }
}

async fn assert_fixture(f: &Fixture, name: &str) {
    let rows: Vec<(i64, Json<Value>)> = sqlx::query_as(
        "SELECT seq,payload FROM events WHERE workspace_id=$1 AND thread_id=$2 ORDER BY seq",
    )
    .bind(f.tenant.workspace_id())
    .bind(f.thread)
    .fetch_all(&f.core.pool)
    .await
    .unwrap();
    let mut actual = json!(
        rows.into_iter()
            .map(|(seq, Json(event))| json!({"seq":seq,"event":event}))
            .collect::<Vec<_>>()
    );
    normalize(&mut actual, &mut HashMap::new());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../web/src/testing/fixtures/pipeline-{name}.json"
    ));
    if std::env::var_os("UPDATE_PIPELINE_FIXTURES").is_some() {
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&actual).unwrap()),
        )
        .unwrap();
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        actual, expected,
        "pipeline-{name}.json drifted; review the wire change before regenerating"
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn answered_pipeline_matches_web_fixture(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let response = f.post("Hello", Some("fixture-request")).await;
    assert_eq!(response.status(), 201);
    let job: JobId = serde_json::from_value(body(response).await["job_id"].clone()).unwrap();
    f.run(job).await;
    assert_eq!(f.status(job).await.0.as_deref(), Some("answered"));
    assert_fixture(&f, "answered").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_pipeline_matches_web_fixture(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![FakeTurn::ChunksThenHang(
        vec![ChatChunk::TextDelta("Partial reply ".repeat(40) + "!")],
    )]));
    let (_, job) = f.submit("Hello").await;
    let mut workers = worker::spawn_pool(f.core.clone(), 1, Duration::from_secs(60));
    timeout(Duration::from_secs(10), async {
        loop {
            if f.events()
                .await
                .iter()
                .any(|e| e["type"] == "TEXT_MESSAGE_CONTENT")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker did not stream text");
    assert_eq!(
        f.call("POST", &format!("jobs/{job}/cancel"), None)
            .await
            .status(),
        202
    );
    f.core.shutdown.cancel();
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    assert_eq!(f.status(job).await.0.as_deref(), Some("cancelled"));
    assert_fixture(&f, "cancelled").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn needs_input_pipeline_matches_web_fixture(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let ask = ToolCall {
        id: "ask".into(),
        name: "ask_user".into(),
        args_json: json!({"question":"Which option?","options":["One","Two"]}).to_string(),
    };
    f.provider(FakeProvider::scripted(vec![FakeTurn::Chunks(vec![
        ChatChunk::ToolCall(ask.clone()),
        ChatChunk::Done {
            stop: StopKind::ToolUse,
            turn: AssistantTurn {
                content: String::new(),
                tool_calls: vec![ask],
                replay: None,
            },
        },
    ])]));
    let (_, job) = f.submit("Please ask me").await;
    f.run(job).await;
    assert_eq!(
        f.status(job).await,
        (Some("needs_input".into()), Some("question_pending".into()))
    );
    assert_fixture(&f, "needs-input").await;
}
