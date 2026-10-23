use futures::{FutureExt, StreamExt};
use katwork::models::*;
use std::time::Duration;

#[cfg(feature = "smoke-fixtures")]
fn request() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![ChatMsg::User {
            name: None,
            content: "สวัสดี".into(),
        }],
        tools: vec![],
        max_output_tokens: 100,
        temperature: None,
    }
}

#[cfg(feature = "smoke-fixtures")]
#[tokio::test(start_paused = true)]
async fn held_demo_delays_each_run_before_first_chunk_without_blocking_other_runs() {
    let provider = FakeProvider::demo_held(Duration::from_secs(20));
    assert!(provider.capabilities().synthetic);
    let mut first = provider.stream(request()).await.unwrap();
    let mut second = provider.stream(request()).await.unwrap();
    assert!(first.next().now_or_never().is_none());
    assert!(second.next().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(19)).await;
    assert!(first.next().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    for stream in [&mut first, &mut second] {
        assert!(
            matches!(stream.next().now_or_never(), Some(Some(Ok(ChatChunk::TextDelta(text)))) if text.contains("สวัสดี"))
        );
        let rest: Vec<_> = stream.collect().await;
        assert!(matches!(
            rest.last(),
            Some(Ok(ChatChunk::Done {
                stop: StopKind::EndTurn,
                ..
            }))
        ));
    }
    let mut third = provider.stream(request()).await.unwrap();
    assert!(third.next().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(20)).await;
    assert!(third.next().now_or_never().is_some());
}

#[test]
fn smoke_environment_only_affects_feature_builds() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "registry_probe"])
        .env("KATWORK_SMOKE_TEST", "1")
        .env("KATWORK_SMOKE_HOLD_SECS", "20")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test(start_paused = true)]
async fn registry_probe() {
    if std::env::var("KATWORK_SMOKE_TEST").is_err() {
        return;
    }
    let config = katwork::config::Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://localhost/fixture".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        "KATWORK_DEMO" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    let registry = Registry::from_config(&config);
    let selected = registry
        .for_tier(
            katwork::types::ModelTier::Fast,
            &katwork::types::SpacePolicy {
                allow_external: false,
            },
        )
        .unwrap();
    let request = ChatRequest {
        system: String::new(),
        messages: vec![],
        tools: vec![],
        max_output_tokens: 100,
        temperature: None,
    };
    let mut stream = selected.provider.stream(request).await.unwrap();
    let first = stream.next().now_or_never();
    assert_eq!(first.is_none(), cfg!(feature = "smoke-fixtures"));
    if first.is_none() {
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(stream.next().now_or_never().is_some());
    }
}
