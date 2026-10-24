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

#[cfg(feature = "smoke-fixtures")]
#[tokio::test]
async fn smoke_wedge_once_blocks_every_thread_only_without_its_marker() {
    use katwork::docs::{DocLimits, DocPool, pool::smoke::wedge_once};
    use std::{path::Path, sync::Arc};

    let pool = Arc::new(
        DocPool::new(
            2,
            DocLimits {
                call_timeout: Duration::from_millis(10),
                ..DocLimits::default()
            },
        )
        .unwrap(),
    );
    // Compose passes the switch as `${VAR:-}`: empty means off.
    assert!(!wedge_once(&pool, Path::new("")).unwrap());
    let dir = std::env::temp_dir().join(format!("katwork-wedge-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&dir).unwrap();
    let marker = dir.join("wedged");
    std::fs::write(&marker, b"").unwrap();
    // A restarted server finds its marker and leaves the pool alone.
    assert!(!wedge_once(&pool, &marker).unwrap());
    std::fs::remove_file(&marker).unwrap();
    assert!(wedge_once(&pool, &marker).unwrap());
    assert!(marker.exists());
    tokio::time::timeout(Duration::from_secs(30), async {
        while !pool.wedged() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("every thread blocked");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn smoke_call_timeout_only_affects_feature_builds() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "doc_limits_probe"])
        .env("KATWORK_SMOKE_TEST", "1")
        .env("KATWORK_SMOKE_DOC_CALL_TIMEOUT_MS", "200")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
async fn doc_limits_probe() {
    if std::env::var("KATWORK_SMOKE_TEST").is_err() {
        return;
    }
    let config = katwork::config::Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://localhost/fixture".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    let core = katwork::runtime::Core::new(pool, std::sync::Arc::new(config));
    let limits = core.docs.limits();
    let expected = if cfg!(feature = "smoke-fixtures") {
        Duration::from_millis(200)
    } else {
        Duration::from_secs(30)
    };
    assert_eq!(
        (limits.call_timeout, limits.wedge_exit_after),
        (expected, expected)
    );
}
