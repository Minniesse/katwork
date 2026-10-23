use futures::StreamExt;
use katwork::{
    config::{Config, ProviderEnv},
    models::*,
    types::{ModelTier, ReasonCode, SpacePolicy},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

fn config(demo: bool) -> Config {
    Config::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some("x".repeat(32)),
        "KATWORK_DEMO" if demo => Some("1".into()),
        _ => None,
    })
    .unwrap()
}
fn request() -> ChatRequest {
    ChatRequest {
        system: "Be helpful".into(),
        messages: vec![ChatMsg::User {
            content: "Hi".into(),
            name: None,
        }],
        tools: vec![],
        max_output_tokens: 128,
        temperature: None,
    }
}
fn done(stop: StopKind) -> ChatChunk {
    ChatChunk::Done {
        stop,
        turn: AssistantTurn {
            content: "answer".into(),
            tool_calls: vec![],
            replay: None,
        },
    }
}
#[test]
fn registry_refuses_external_provider_for_local_only_space() {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let entry = ModelEntry {
        provider_id: "external".into(),
        model: "test".into(),
        tier: ModelTier::Fast,
        capabilities: Capabilities {
            tools: true,
            residency: Residency::External,
            synthetic: false,
        },
        max_output_tokens: 2048,
        temperature: None,
        price: None,
    };
    let registry = Registry::from_entries(vec![(entry, provider)]);
    assert!(matches!(
        registry.for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: false
            }
        ),
        Err(ModelError::ResidencyDenied)
    ));
    assert!(
        registry
            .for_tier(
                ModelTier::Fast,
                &SpacePolicy {
                    allow_external: true
                }
            )
            .is_ok()
    );
}
#[test]
fn invalid_provider_base_url_is_config_invalid() {
    for key in ["OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"] {
        for bad in [
            "garbage",
            "ftp://host",
            "file:///tmp",
            "https://",
            "mailto:person@example.invalid",
        ] {
            let result = Config::from_lookup(|name| match name {
                "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
                "KATWORK_SECRET" => Some("x".repeat(32)),
                n if n == key => Some(bad.into()),
                _ => None,
            });
            assert_eq!(result.unwrap_err().code(), "config_invalid");
        }
    }
}
#[test]
fn provider_init_failure_resolves_to_provider_unavailable() {
    let mut cfg = config(true);
    cfg.openai = Some(ProviderEnv {
        api_key: uuid::Uuid::now_v7().to_string(),
        base_url: Some("file:///no".into()),
    });
    assert!(matches!(
        Registry::from_config(&cfg).for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: true
            }
        ),
        Err(ModelError::ProviderUnavailable)
    ));
}
#[tokio::test]
async fn demo_mode_uses_synthetic_provider_without_keys() {
    let registry = Registry::from_config(&config(true));
    for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Strong] {
        let selected = registry
            .for_tier(
                tier,
                &SpacePolicy {
                    allow_external: false,
                },
            )
            .unwrap();
        assert!(selected.entry.capabilities.synthetic);
        assert!(selected.provider.capabilities().synthetic);
        let chunks = selected
            .provider
            .stream(request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            chunks.last(),
            Some(Ok(ChatChunk::Done {
                stop: StopKind::EndTurn,
                ..
            }))
        ));
        assert_eq!(selected.entry.price, None);
    }
}
#[test]
fn no_keys_and_no_demo_is_provider_unavailable() {
    assert!(matches!(
        Registry::from_config(&config(false)).for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: true
            }
        ),
        Err(ModelError::ProviderUnavailable)
    ));
}
#[tokio::test]
async fn demo_mode_retains_no_requests_after_many_calls() {
    let provider = FakeProvider::demo();
    for _ in 0..100 {
        let chunks = provider
            .stream(request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(chunks.last(), Some(Ok(ChatChunk::Done { .. }))));
    }
    assert!(provider.received().is_empty());
}
#[tokio::test(start_paused = true)]
async fn stream_stall_returns_model_timeout() {
    for (turn, phase) in [
        (FakeTurn::Hang, TimeoutPhase::FirstChunk),
        (
            FakeTurn::ChunksThenHang(vec![ChatChunk::TextDelta("a".into())]),
            TimeoutPhase::Idle,
        ),
        (
            FakeTurn::ChunksThenHang(vec![ChatChunk::Progress]),
            TimeoutPhase::Idle,
        ),
    ] {
        let provider = FakeProvider::scripted(vec![turn]);
        let stream = provider.stream(request()).await.unwrap();
        let chunks = guard_stream(
            stream,
            StreamTimeouts {
                first_chunk: Duration::from_secs(2),
                idle: Duration::from_secs(3),
            },
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(chunks.last(), Some(&Err(ModelError::Timeout { phase })));
    }
}
#[tokio::test(start_paused = true)]
async fn usage_and_empty_text_do_not_reset_content_deadlines() {
    for non_content in [
        ChatChunk::Usage(Usage::default()),
        ChatChunk::TextDelta(String::new()),
    ] {
        for after_text in [false, true] {
            let non_content = non_content.clone();
            let stream = async_stream::stream! {
                if after_text {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    yield Ok(ChatChunk::TextDelta("hello".into()));
                }
                for _ in 0..12 {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    yield Ok(non_content.clone());
                }
                std::future::pending::<()>().await;
            };
            let start = tokio::time::Instant::now();
            let chunks = guard_stream(Box::pin(stream), StreamTimeouts::default())
                .collect::<Vec<_>>()
                .await;
            let phase = if after_text {
                TimeoutPhase::Idle
            } else {
                TimeoutPhase::FirstChunk
            };
            assert_eq!(chunks.last(), Some(&Err(ModelError::Timeout { phase })));
            assert_eq!(
                start.elapsed(),
                Duration::from_secs(if after_text { 40 } else { 30 })
            );
        }
    }
}
#[tokio::test]
async fn stream_without_done_is_protocol_error() {
    let provider = FakeProvider::scripted(vec![FakeTurn::Chunks(vec![])]);
    let chunks = guard_stream(
        provider.stream(request()).await.unwrap(),
        StreamTimeouts::default(),
    )
    .collect::<Vec<_>>()
    .await;
    assert!(matches!(chunks.last(), Some(Err(ModelError::Protocol(_)))));
}
#[test]
fn input_charge_bound_covers_multibyte_text() {
    let mut req = request();
    req.system = "👋ฯ".repeat(100);
    assert!(req.input_charge_bound() >= req.system.len() as u64);
    assert_eq!(
        req.input_charge_bound(),
        serde_json::to_vec(&req).unwrap().len() as u64
    );
}
#[test]
fn input_charge_bound_includes_tool_schemas() {
    let mut req = request();
    let before = req.input_charge_bound();
    req.tools.push(ToolSpec {
        name: "test".into(),
        description: "desc".into(),
        parameters: json!({"description":"x".repeat(20_000)}),
    });
    assert!(req.input_charge_bound() >= before + 20_000);
}
#[test]
fn input_charge_bound_covers_provider_tool_prompt_overhead() {
    let mut req = request();
    req.system = "Hi".into();
    req.tools.push(ToolSpec {
        name: "t".into(),
        description: String::new(),
        parameters: json!({"type":"object"}),
    });
    let request_bytes = serde_json::to_vec(&req).unwrap().len() as u64;
    assert!(req.input_charge_bound() >= request_bytes + 1024);
}
#[tokio::test]
async fn tool_use_stop_without_call_is_protocol_error() {
    let provider = FakeProvider::scripted(vec![FakeTurn::Chunks(vec![done(StopKind::ToolUse)])]);
    let chunks = guard_stream(
        provider.stream(request()).await.unwrap(),
        StreamTimeouts::default(),
    )
    .collect::<Vec<_>>()
    .await;
    assert!(matches!(chunks.last(), Some(Err(ModelError::Protocol(_)))));
}
#[tokio::test]
async fn scripted_turns_record_requests_and_are_not_synthetic() {
    let provider = FakeProvider::scripted(vec![
        FakeTurn::Error(ModelError::AuthFailed),
        FakeTurn::Chunks(vec![done(StopKind::EndTurn)]),
    ]);
    assert!(!provider.capabilities().synthetic);
    assert!(matches!(
        provider.stream(request()).await,
        Err(ModelError::AuthFailed)
    ));
    assert_eq!(
        provider
            .clone()
            .stream(request())
            .await
            .unwrap()
            .count()
            .await,
        1
    );
    assert_eq!(provider.received().len(), 2);
}
#[test]
fn every_model_error_has_one_stable_reason() {
    for (error, reason) in [
        (
            ModelError::ProviderUnavailable,
            ReasonCode::ProviderUnavailable,
        ),
        (
            ModelError::Other("private".into()),
            ReasonCode::ProviderUnavailable,
        ),
        (ModelError::ResidencyDenied, ReasonCode::ResidencyDenied),
        (
            ModelError::Timeout {
                phase: TimeoutPhase::FirstChunk,
            },
            ReasonCode::ProviderUnavailable,
        ),
        (
            ModelError::Timeout {
                phase: TimeoutPhase::Idle,
            },
            ReasonCode::ProviderUnavailable,
        ),
        (
            ModelError::RateLimited {
                retry_after_secs: Some(2),
            },
            ReasonCode::ProviderRateLimited,
        ),
        (
            ModelError::QuotaExceeded { resets_at: None },
            ReasonCode::ProviderQuota,
        ),
        (ModelError::AuthFailed, ReasonCode::ProviderAuthFailed),
        (
            ModelError::Protocol("private".into()),
            ReasonCode::ModelProtocolError,
        ),
    ] {
        assert_eq!(error.reason_code(), reason);
        assert!(!error.to_string().contains("private"));
    }
}

#[tokio::test(start_paused = true)]
async fn progressing_stream_has_no_total_timeout() {
    let stream = async_stream::stream! {
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(20)).await;
            yield Ok(ChatChunk::TextDelta("still working".into()));
        }
        yield Ok(done(StopKind::EndTurn));
    };
    let start = tokio::time::Instant::now();
    let chunks = guard_stream(Box::pin(stream), StreamTimeouts::default())
        .collect::<Vec<_>>()
        .await;
    assert!(start.elapsed() >= Duration::from_secs(80));
    assert!(matches!(chunks.last(), Some(Ok(ChatChunk::Done { .. }))));
}

#[tokio::test]
async fn core_initializes_and_can_override_registry_without_database_io() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://localhost/katwork")
        .unwrap();
    let core = katwork::runtime::Core::new(pool, Arc::new(config(true)));
    assert!(
        core.registry
            .for_tier(
                ModelTier::Fast,
                &SpacePolicy {
                    allow_external: false
                }
            )
            .unwrap()
            .entry
            .capabilities
            .synthetic
    );
    let core = core.with_registry(Registry::from_entries(vec![]));
    assert!(matches!(
        core.registry.for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: true
            }
        ),
        Err(ModelError::ProviderUnavailable)
    ));
}

#[test]
fn localhost_provider_remains_external_and_keys_disable_demo_fallback() {
    let mut cfg = config(true);
    cfg.openai = Some(ProviderEnv {
        api_key: uuid::Uuid::now_v7().to_string(),
        base_url: Some("http://localhost:1".into()),
    });
    let registry = Registry::from_config(&cfg);
    assert!(matches!(
        registry.for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: false
            }
        ),
        Err(ModelError::ResidencyDenied)
    ));
    let selected = registry
        .for_tier(
            ModelTier::Fast,
            &SpacePolicy {
                allow_external: true,
            },
        )
        .unwrap();
    assert!(!selected.entry.capabilities.synthetic);
    assert_eq!(selected.entry.max_output_tokens, 2048);
}
