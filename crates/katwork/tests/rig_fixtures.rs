use futures::StreamExt;
use katwork::{config::ProviderEnv, models::*};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, Notify},
};

struct Server {
    url: String,
    request: Arc<Mutex<Option<Value>>>,
    arrived: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(body: Vec<u8>, status: u16, split: Option<usize>, hold: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let request = Arc::new(Mutex::new(None));
        let captured = request.clone();
        let arrived = Arc::new(Notify::new());
        let received = arrived.clone();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&raw[..end]);
                    let len = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + len {
                        *captured.lock().await =
                            Some(serde_json::from_slice(&raw[end + 4..end + 4 + len]).unwrap());
                        break;
                    }
                }
            }
            received.notify_one();
            if hold {
                std::future::pending::<()>().await;
            }
            let header = format!(
                "HTTP/1.1 {status} fixture\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            let chunks = match split {
                Some(n) => vec![&body[..n], &body[n..]],
                None => vec![body.as_slice()],
            };
            for chunk in chunks {
                if socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if socket.write_all(chunk).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        Self {
            url,
            request,
            arrived,
            task,
        }
    }
    fn provider(&self, api: Option<OpenAiApi>) -> RigProvider {
        let env = ProviderEnv {
            api_key: uuid::Uuid::now_v7().to_string(),
            base_url: Some(self.url.clone()),
        };
        match api {
            Some(api) => RigProvider::openai(&env, "gpt-4.1", api),
            None => RigProvider::anthropic(&env, "claude-sonnet-4-6"),
        }
        .unwrap()
    }
}
fn request() -> ChatRequest {
    ChatRequest {
        system: "system only".into(),
        messages: vec![ChatMsg::User {
            content: "hello".into(),
            name: None,
        }],
        tools: vec![ToolSpec {
            name: "first".into(),
            description: "test".into(),
            parameters: json!({"type":"object","properties":{}}),
        }],
        max_output_tokens: 128,
        temperature: None,
    }
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/models/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}
async fn collect(provider: &RigProvider) -> Vec<Result<ChatChunk, ModelError>> {
    match provider.stream(request()).await {
        Ok(stream) => {
            guard_stream(stream, StreamTimeouts::default())
                .collect()
                .await
        }
        Err(e) => vec![Err(e)],
    }
}
async fn chat(name: &str) -> Vec<Result<ChatChunk, ModelError>> {
    let server = Server::new(fixture(name), 200, None, false).await;
    collect(&server.provider(Some(OpenAiApi::Chat))).await
}
fn turn(chunks: &[Result<ChatChunk, ModelError>]) -> &AssistantTurn {
    chunks
        .iter()
        .find_map(|c| match c {
            Ok(ChatChunk::Done { turn, .. }) => Some(turn),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no Done: {chunks:?}"))
}
fn protocol(chunks: &[Result<ChatChunk, ModelError>]) {
    assert!(
        matches!(chunks.last(), Some(Err(ModelError::Protocol(_)))),
        "{chunks:?}"
    );
    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, Ok(ChatChunk::ToolCall(_))))
    );
}
#[tokio::test]
async fn thai_split_mid_codepoint_across_http_chunks_decodes() {
    let body = fixture("chat_thai.sse");
    let split = body.windows(3).position(|w| w == "ส".as_bytes()).unwrap() + 1;
    let server = Server::new(body, 200, Some(split), false).await;
    let chunks = collect(&server.provider(Some(OpenAiApi::Chat))).await;
    assert_eq!(turn(&chunks).content, "สวัสดี 👋");
}
#[tokio::test]
async fn openai_chat_usage_only_tail_is_recorded() {
    let chunks = chat("chat_usage.sse").await;
    assert!(chunks.contains(&Ok(ChatChunk::Usage(Usage {
        input: Some(11),
        cache_read: Some(3),
        cache_write: None,
        output: Some(7),
        complete: true
    }))));
    let missing = chat("chat_thai.sse").await;
    assert!(missing.contains(&Ok(ChatChunk::Usage(Usage {
        input: None,
        cache_read: None,
        cache_write: None,
        output: None,
        complete: false
    }))));
}
#[tokio::test]
async fn openai_responses_final_text_replaces_deltas() {
    let server = Server::new(fixture("responses_final.sse"), 200, None, false).await;
    let chunks = collect(&server.provider(Some(OpenAiApi::Responses))).await;
    assert!(chunks.contains(&Ok(ChatChunk::TextDelta("draft".into()))));
    assert_eq!(turn(&chunks).content, "final answer");
}
#[tokio::test]
async fn completed_tool_call_then_response_failed_is_not_executable() {
    let server = Server::new(fixture("responses_failed.sse"), 200, None, false).await;
    let chunks = collect(&server.provider(Some(OpenAiApi::Responses))).await;
    assert_eq!(
        chunks.last(),
        Some(&Err(ModelError::ProviderUnavailable)),
        "{chunks:?}"
    );
    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, Ok(ChatChunk::ToolCall(_) | ChatChunk::Done { .. })))
    );
}
#[tokio::test]
async fn absent_finish_reason_is_protocol_error() {
    protocol(&chat("chat_no_finish.sse").await);
}
#[tokio::test]
async fn interleaved_argument_fragments_assemble_per_call() {
    let chunks = chat("chat_interleaved.sse").await;
    let calls = &turn(&chunks).tool_calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].args_json).unwrap(),
        json!({"x":1})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[1].args_json).unwrap(),
        json!({"y":2})
    );
}
#[tokio::test]
async fn malformed_and_unfinished_calls_are_protocol_errors() {
    for name in ["chat_malformed.sse", "chat_unfinished.sse"] {
        protocol(&chat(name).await);
    }
}
#[tokio::test]
async fn several_calls_in_one_turn_keep_order() {
    let chunks = chat("chat_interleaved.sse").await;
    assert_eq!(
        turn(&chunks)
            .tool_calls
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
}
#[tokio::test]
async fn missing_call_id_is_assigned_and_duplicate_in_turn_is_protocol_error() {
    let chunks = chat("chat_missing_id.sse").await;
    assert_eq!(turn(&chunks).tool_calls[0].id, "kw_call_0");
    protocol(&chat("chat_duplicate_id.sse").await);
}
#[tokio::test]
async fn max_tokens_keeps_partial_text_without_executable_calls() {
    let chunks = chat("chat_truncated.sse").await;
    assert_eq!(turn(&chunks).content, "partial");
    assert!(turn(&chunks).tool_calls.is_empty());
    assert!(matches!(
        chunks.last(),
        Some(Ok(ChatChunk::Done {
            stop: StopKind::MaxTokens,
            ..
        }))
    ));
    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, Ok(ChatChunk::ToolCall(_))))
    );
}
#[tokio::test]
async fn anthropic_usage_snapshots_merge_not_sum() {
    let server = Server::new(fixture("anthropic_usage.sse"), 200, None, false).await;
    let chunks = collect(&server.provider(None)).await;
    assert!(
        chunks.contains(&Ok(ChatChunk::Usage(Usage {
            input: Some(20),
            cache_read: Some(6),
            cache_write: Some(4),
            output: Some(3),
            complete: true
        }))),
        "{chunks:?}"
    );
}
#[tokio::test]
async fn anthropic_error_results_map_to_model_errors() {
    let server = Server::new(fixture("anthropic_error.sse"), 200, None, false).await;
    assert_eq!(
        collect(&server.provider(None)).await.last(),
        Some(&Err(ModelError::ProviderUnavailable))
    );
}
#[tokio::test]
async fn cancel_during_network_wait_returns_promptly() {
    let server = Server::new(vec![], 200, None, true).await;
    let provider = server.provider(Some(OpenAiApi::Chat));
    let task = tokio::spawn(async move { collect(&provider).await });
    server.arrived.notified().await;
    task.abort();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
}
#[tokio::test]
async fn unknown_events_are_ignored() {
    assert_eq!(turn(&chat("chat_unknown.sse").await).content, "สวัสดี 👋");
}
#[tokio::test]
async fn replay_round_trips_to_the_same_provider() {
    let server = Server::new(fixture("chat_interleaved.sse"), 200, None, false).await;
    let chunks = collect(&server.provider(Some(OpenAiApi::Chat))).await;
    let previous = turn(&chunks).clone();
    assert!(previous.replay.is_some());
    let server = Server::new(fixture("chat_thai.sse"), 200, None, false).await;
    let provider = server.provider(Some(OpenAiApi::Chat));
    let mut req = request();
    req.messages.push(ChatMsg::Assistant {
        content: previous.content,
        tool_calls: previous.tool_calls.clone(),
        replay: previous.replay,
    });
    for call in previous.tool_calls {
        req.messages.push(ChatMsg::Tool {
            tool_call_id: call.id,
            name: call.name,
            content: "refused".into(),
            is_error: true,
        });
    }
    let chunks = provider
        .stream(req)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    turn(&chunks);
    let captured = server.request.lock().await.clone().unwrap();
    assert_eq!(captured["messages"][2]["tool_calls"][0]["id"], "a");
    assert_eq!(captured["messages"][3]["tool_call_id"], "a");
    assert!(captured.get("previous_response_id").is_none());
    assert_eq!(captured["messages"][0]["role"], "system");
}
#[tokio::test]
async fn oversized_response_is_rejected_before_decoding() {
    let server = Server::new(fixture("oversized_response.sse"), 200, Some(32), false).await;
    let p = server.provider(Some(OpenAiApi::Chat)).with_http(
        BoundedHttp::new(HttpLimits {
            max_response_bytes: 64,
            ..HttpLimits::default()
        })
        .unwrap(),
    );
    assert_eq!(
        collect(&p).await.last(),
        Some(&Err(ModelError::Protocol("response_too_large".into())))
    );
}
#[tokio::test]
async fn oversized_error_body_is_capped_before_rig_reads_it() {
    let server = Server::new(fixture("oversized_error.json"), 429, None, false).await;
    let p = server.provider(Some(OpenAiApi::Chat)).with_http(
        BoundedHttp::new(HttpLimits {
            max_error_body_bytes: 32,
            ..HttpLimits::default()
        })
        .unwrap(),
    );
    assert_eq!(
        collect(&p).await.last(),
        Some(&Err(ModelError::RateLimited {
            retry_after_secs: None
        }))
    );
}
#[tokio::test]
async fn unterminated_sse_frame_over_cap_is_rejected() {
    let server = Server::new(fixture("oversized_frame.sse"), 200, Some(32), false).await;
    let p = server.provider(Some(OpenAiApi::Chat)).with_http(
        BoundedHttp::new(HttpLimits {
            max_sse_frame_bytes: 64,
            ..HttpLimits::default()
        })
        .unwrap(),
    );
    assert_eq!(
        collect(&p).await.last(),
        Some(&Err(ModelError::Protocol("sse_frame_too_large".into())))
    );
}
#[tokio::test]
#[ignore = "opt-in paid provider request; requires KATWORK_LIVE_MODELS=1 and OPENAI_API_KEY"]
async fn live_openai_chat_smoke() {
    if std::env::var("KATWORK_LIVE_MODELS").as_deref() != Ok("1") {
        return;
    }
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    let provider = RigProvider::openai(
        &ProviderEnv {
            api_key,
            base_url: None,
        },
        "gpt-4.1-mini",
        OpenAiApi::Chat,
    )
    .unwrap();
    turn(&collect(&provider).await);
}

#[tokio::test]
async fn http_auth_quota_and_rate_limits_are_distinct() {
    for (status, body, expected) in [
        (
            401,
            json!({"error":{"code":"invalid_api_key"}}),
            ModelError::AuthFailed,
        ),
        (
            429,
            json!({"error":{"code":"insufficient_quota"}}),
            ModelError::QuotaExceeded { resets_at: None },
        ),
        (
            429,
            json!({"error":{"type":"rate_limit_error"}}),
            ModelError::RateLimited {
                retry_after_secs: None,
            },
        ),
        (
            429,
            json!({"error":{"code":null,"type":"insufficient_quota"}}),
            ModelError::QuotaExceeded { resets_at: None },
        ),
        (
            503,
            json!({"error":{"type":"overloaded_error"}}),
            ModelError::ProviderUnavailable,
        ),
    ] {
        let server = Server::new(serde_json::to_vec(&body).unwrap(), status, None, false).await;
        assert_eq!(
            collect(&server.provider(Some(OpenAiApi::Chat)))
                .await
                .last(),
            Some(&Err(expected))
        );
    }
}

#[tokio::test]
async fn crlf_frames_and_invalid_utf8_are_handled_without_lossy_replacement() {
    let body = String::from_utf8(fixture("chat_thai.sse"))
        .unwrap()
        .replace('\n', "\r\n")
        .into_bytes();
    let server = Server::new(body, 200, Some(1), false).await;
    assert_eq!(
        turn(&collect(&server.provider(Some(OpenAiApi::Chat))).await).content,
        "สวัสดี 👋"
    );
    let server = Server::new(
        vec![b'd', b'a', b't', b'a', b':', 0xff, b'\n', b'\n'],
        200,
        None,
        false,
    )
    .await;
    protocol(&collect(&server.provider(Some(OpenAiApi::Chat))).await);
}

#[tokio::test]
async fn foreign_or_changed_replay_cannot_override_canonical_history() {
    let previous = chat("chat_thai.sse").await;
    let previous = turn(&previous);
    let server = Server::new(fixture("chat_thai.sse"), 200, None, false).await;
    let mut req = request();
    let mut replay = previous.replay.clone().unwrap();
    replay.provider_id = "another-provider".into();
    req.messages.push(ChatMsg::Assistant {
        content: "canonical".into(),
        tool_calls: vec![],
        replay: Some(replay.clone()),
    });
    let provider = server.provider(Some(OpenAiApi::Chat));
    let chunks = provider
        .stream(req.clone())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    turn(&chunks);
    assert_eq!(
        server.request.lock().await.as_ref().unwrap()["messages"][2]["content"],
        "canonical"
    );
    replay.provider_id = provider.id().into();
    req.messages[1] = ChatMsg::Assistant {
        content: "canonical".into(),
        tool_calls: vec![],
        replay: Some(replay),
    };
    assert!(matches!(
        provider.stream(req).await,
        Err(ModelError::Protocol(_))
    ));
}

#[tokio::test]
async fn anthropic_reasoning_signature_replays_only_to_anthropic() {
    let server = Server::new(fixture("anthropic_replay.sse"), 200, None, false).await;
    let previous = collect(&server.provider(None)).await;
    let previous = turn(&previous);
    for api in [None, Some(OpenAiApi::Chat)] {
        let server = Server::new(
            fixture(if api.is_some() {
                "chat_thai.sse"
            } else {
                "anthropic_usage.sse"
            }),
            200,
            None,
            false,
        )
        .await;
        let mut req = request();
        req.messages.push(ChatMsg::Assistant {
            content: previous.content.clone(),
            tool_calls: vec![],
            replay: previous.replay.clone(),
        });
        req.messages.push(ChatMsg::User {
            content: "next".into(),
            name: None,
        });
        let stream = server.provider(api).stream(req).await.unwrap();
        turn(&stream.collect::<Vec<_>>().await);
        let captured = server.request.lock().await.clone().unwrap();
        assert_eq!(
            captured.to_string().contains("fixture-signature"),
            api.is_none()
        );
    }
}
