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
    closed: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(body: Vec<u8>, status: u16, split: Option<usize>, hold: bool) -> Self {
        let chunks = match split {
            Some(n) => vec![body[..n].to_vec(), body[n..].to_vec()],
            None => vec![body],
        };
        Self::start(chunks, status, hold, "", Duration::from_millis(5), false).await
    }
    async fn start(
        chunks: Vec<Vec<u8>>,
        status: u16,
        hold: bool,
        headers: &str,
        interval: Duration,
        truncate: bool,
    ) -> Self {
        let headers = headers.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let request = Arc::new(Mutex::new(None));
        let captured = request.clone();
        let arrived = Arc::new(Notify::new());
        let received = arrived.clone();
        let closed = Arc::new(Notify::new());
        let disconnected = closed.clone();
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
            if hold && chunks.iter().all(Vec::is_empty) {
                match socket.read(&mut buf).await {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    other => panic!("expected client disconnect, got {other:?}"),
                }
                disconnected.notify_one();
                return;
            }
            let header = format!(
                "HTTP/1.1 {status} fixture\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n{headers}\r\n"
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            for chunk in chunks {
                if socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                tokio::time::sleep(interval).await;
            }
            if hold {
                match socket.read(&mut buf).await {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    other => panic!("expected client disconnect, got {other:?}"),
                }
                disconnected.notify_one();
            } else if !truncate {
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        });
        Self {
            url,
            request,
            arrived,
            closed,
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
async fn responses_request_disables_storage_and_uses_only_function_tools() {
    let server = Server::new(fixture("responses_final.sse"), 200, None, false).await;
    turn(&collect(&server.provider(Some(OpenAiApi::Responses))).await);
    let captured = server.request.lock().await.clone().unwrap();
    assert_eq!(captured["store"], false);
    assert!(captured.get("previous_response_id").is_none());
    let tools = captured["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert!(tools.iter().all(|tool| tool["type"] == "function"));
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
async fn malformed_call_is_protocol_error() {
    protocol(&chat("chat_malformed.sse").await);
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
async fn cancellation_closes_the_provider_connection() {
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
    tokio::time::timeout(Duration::from_secs(1), server.closed.notified())
        .await
        .expect("provider socket must close when the stream is dropped");
}
#[tokio::test]
async fn unknown_events_are_ignored_by_every_adapter() {
    for (api, fixture_name, expected) in [
        (Some(OpenAiApi::Chat), "chat_unknown.sse", "สวัสดี 👋"),
        (
            Some(OpenAiApi::Responses),
            "responses_final.sse",
            "final answer",
        ),
        (None, "anthropic_usage.sse", "hello"),
    ] {
        let server = Server::new(fixture(fixture_name), 200, None, false).await;
        let chunks = collect(&server.provider(api)).await;
        assert_eq!(turn(&chunks).content, expected);
    }
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
    assert_eq!(captured["messages"][3]["content"], "Error: refused");
    assert!(captured.get("previous_response_id").is_none());
    assert_eq!(captured["messages"][0]["role"], "system");
}
#[tokio::test]
async fn openai_tool_errors_keep_error_prefix_on_both_routes() {
    for (api, response) in [
        (OpenAiApi::Chat, "chat_thai.sse"),
        (OpenAiApi::Responses, "responses_final.sse"),
    ] {
        for (is_error, expected) in [(true, "Error: result"), (false, "result")] {
            let server = Server::new(fixture(response), 200, None, false).await;
            let mut req = request();
            req.messages.push(ChatMsg::Assistant {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "a".into(),
                    name: "first".into(),
                    args_json: "{}".into(),
                }],
                replay: None,
            });
            req.messages.push(ChatMsg::Tool {
                tool_call_id: "a".into(),
                name: "first".into(),
                content: "result".into(),
                is_error,
            });
            let chunks = server
                .provider(Some(api))
                .stream(req)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            turn(&chunks);
            let captured = server.request.lock().await.clone().unwrap();
            match api {
                OpenAiApi::Chat => assert_eq!(captured["messages"][3]["content"], expected),
                OpenAiApi::Responses => {
                    let result = captured["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["type"] == "function_call_output")
                        .unwrap();
                    assert_eq!(result["output"], expected);
                }
            }
        }
    }
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

#[tokio::test]
async fn anthropic_http_status_and_retry_after_are_preserved() {
    for (status, kind, headers, expected) in [
        (401, "authentication_error", "", ModelError::AuthFailed),
        (403, "permission_error", "", ModelError::AuthFailed),
        (
            400,
            "billing_error",
            "",
            ModelError::QuotaExceeded { resets_at: None },
        ),
        (
            429,
            "rate_limit_error",
            "",
            ModelError::RateLimited {
                retry_after_secs: None,
            },
        ),
        (
            429,
            "rate_limit_error",
            "retry-after: 7\r\n",
            ModelError::RateLimited {
                retry_after_secs: Some(7),
            },
        ),
        (503, "api_error", "", ModelError::ProviderUnavailable),
        (529, "overloaded_error", "", ModelError::ProviderUnavailable),
    ] {
        let body =
            serde_json::to_vec(&json!({"type":"error","error":{"type":kind,"message":"fixture"}}))
                .unwrap();
        let server = Server::start(vec![body], status, false, headers, Duration::ZERO, false).await;
        assert_eq!(
            collect(&server.provider(None)).await.last(),
            Some(&Err(expected))
        );
    }
}

#[tokio::test]
async fn provider_redirect_is_not_followed() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let headers = format!(
        "Location: http://{}/redirected\r\n",
        target.local_addr().unwrap()
    );
    let server = Server::start(vec![], 307, false, &headers, Duration::ZERO, false).await;
    let chunks = tokio::time::timeout(
        Duration::from_secs(1),
        collect(&server.provider(Some(OpenAiApi::Chat))),
    )
    .await
    .expect("redirect must fail without waiting on its target");
    assert!(matches!(chunks.last(), Some(Err(_))));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), target.accept())
            .await
            .is_err()
    );
}

// Task 8 owns retries and billing; Task 7 exposes content before a terminal error.
async fn assert_content_before_failure(
    body: Vec<u8>,
    api: Option<OpenAiApi>,
    truncate: bool,
    content: bool,
) {
    let server = Server::start(vec![body], 200, false, "", Duration::ZERO, truncate).await;
    let chunks = collect(&server.provider(api)).await;
    assert_eq!(chunks.last(), Some(&Err(ModelError::ProviderUnavailable)));
    assert!(
        !chunks
            .iter()
            .any(|chunk| matches!(chunk, Ok(ChatChunk::ToolCall(_) | ChatChunk::Done { .. })))
    );
    assert_eq!(
        chunks[..chunks.len() - 1].iter().any(|chunk| match chunk {
            Ok(ChatChunk::TextDelta(text)) => !text.is_empty(),
            Ok(ChatChunk::Progress) => true,
            _ => false,
        }),
        content
    );
}

#[tokio::test]
async fn content_precedes_connection_failure() {
    let arguments = String::from_utf8(fixture("chat_arguments.sse")).unwrap();
    let arguments: String = arguments.split_inclusive("\n\n").take(2).collect();
    for body in [arguments.into_bytes(),
        b"data: {\"id\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n".to_vec()] {
        assert_content_before_failure(body, Some(OpenAiApi::Chat), true, true).await;
    }
}

#[tokio::test]
async fn tool_progress_precedes_provider_failure() {
    assert_content_before_failure(fixture("anthropic_arguments_error.sse"), None, false, true)
        .await;
    assert_content_before_failure(fixture("anthropic_error.sse"), None, false, false).await;
}

#[tokio::test]
async fn anthropic_spend_cap_maps_to_quota() {
    let server = Server::new(fixture("anthropic_spend_cap.json"), 429, None, false).await;
    assert_eq!(
        collect(&server.provider(None)).await.last(),
        Some(&Err(ModelError::QuotaExceeded { resets_at: None }))
    );
}

#[tokio::test]
async fn anthropic_organization_spend_limit_maps_to_quota() {
    let server = Server::new(
        fixture("anthropic_organization_spend_limit.json"),
        400,
        None,
        false,
    )
    .await;
    assert_eq!(
        collect(&server.provider(None)).await.last(),
        Some(&Err(ModelError::QuotaExceeded { resets_at: None }))
    );
}

#[tokio::test]
async fn anthropic_workspace_spend_limit_maps_to_quota() {
    let server = Server::new(
        fixture("anthropic_workspace_spend_limit.json"),
        400,
        None,
        false,
    )
    .await;
    assert_eq!(
        collect(&server.provider(None)).await.last(),
        Some(&Err(ModelError::QuotaExceeded { resets_at: None }))
    );
}

#[tokio::test]
async fn anthropic_credit_balance_maps_to_quota() {
    let server = Server::new(fixture("anthropic_credit_balance.json"), 400, None, false).await;
    assert_eq!(
        collect(&server.provider(None)).await.last(),
        Some(&Err(ModelError::QuotaExceeded { resets_at: None }))
    );
}

#[tokio::test]
async fn anthropic_quota_matching_preserves_other_errors() {
    for (status, kind, message, details, expected) in [
        (
            400,
            "invalid_request_error",
            "Invalid request: You have reached your specified API usage limits",
            json!({}),
            ModelError::Protocol("invalid_provider_response".into()),
        ),
        (
            400,
            "invalid_request_error",
            "The request exceeds the maximum token count",
            json!({}),
            ModelError::Protocol("invalid_provider_response".into()),
        ),
        (
            429,
            "rate_limit_error",
            "You have reached your specified API usage limits",
            json!({}),
            ModelError::RateLimited {
                retry_after_secs: None,
            },
        ),
        (
            429,
            "rate_limit_error",
            "fixture",
            json!({"error_code":"requests_per_minute"}),
            ModelError::RateLimited {
                retry_after_secs: None,
            },
        ),
        (
            401,
            "authentication_error",
            "fixture",
            json!({"error_code":"enforced_spend_limit_reached"}),
            ModelError::AuthFailed,
        ),
    ] {
        let body = serde_json::to_vec(
            &json!({"type":"error","error":{"type":kind,"message":message,"details":details}}),
        )
        .unwrap();
        let server = Server::new(body, status, None, false).await;
        assert_eq!(
            collect(&server.provider(None)).await.last(),
            Some(&Err(expected))
        );
    }
}

fn tool_turn_fixture(
    api: Option<OpenAiApi>,
    ids: &[&str],
    args: &str,
    stop: &str,
    close: bool,
) -> Vec<u8> {
    let mut events = Vec::new();
    match api {
        Some(OpenAiApi::Chat) => {
            let calls: Vec<_> = ids.iter().enumerate().map(|(index, id)| json!({
                "index":index,"id":id,"type":"function","function":{"name":"first","arguments":args}
            })).collect();
            events.push(json!({"id":"chat_test","choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":null}]}));
            if close {
                events.push(json!({"id":"chat_test","choices":[{"index":0,"delta":{},"finish_reason":stop}]}));
            }
        }
        Some(OpenAiApi::Responses) => {
            let calls: Vec<_> = ids.iter().enumerate().map(|(index, id)| json!({
                "id":format!("fc_{index}"),"type":"function_call","call_id":id,"name":"first","arguments":args,
                "status":if close {"completed"} else {"in_progress"}
            })).collect();
            events.push(json!({"type":if stop == "completed" {"response.completed"} else {"response.incomplete"},"response":{
                "id":"resp_test","object":"response","created_at":1,"model":"gpt-4.1",
                "status":if stop == "completed" {"completed"} else {"incomplete"},
                "incomplete_details":{"reason":stop},"output":calls
            }}));
        }
        None => {
            events.push(json!({"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}));
            for (index, id) in ids.iter().enumerate() {
                events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":id,"name":"first","input":{}}}));
                events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":args}}));
                if close {
                    events.push(json!({"type":"content_block_stop","index":index}));
                }
            }
            events.push(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":1}}));
            events.push(json!({"type":"message_stop"}));
        }
    }
    let mut body: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    if matches!(api, Some(OpenAiApi::Chat)) {
        body.push_str("data: [DONE]\n\n");
    }
    body.into_bytes()
}

const ROUTES: [(Option<OpenAiApi>, &str, &str); 3] = [
    (Some(OpenAiApi::Chat), "tool_calls", "chat_thai.sse"),
    (
        Some(OpenAiApi::Responses),
        "completed",
        "responses_final.sse",
    ),
    (None, "tool_use", "anthropic_usage.sse"),
];

#[tokio::test]
async fn rejected_stops_emit_no_tool_calls_on_every_route() {
    for (api, stops) in [
        (
            Some(OpenAiApi::Chat),
            ["length", "content_filter", "future_stop"],
        ),
        (
            Some(OpenAiApi::Responses),
            ["max_output_tokens", "content_filter", "future_stop"],
        ),
        (None, ["max_tokens", "refusal", "future_stop"]),
    ] {
        for (index, stop) in stops.into_iter().enumerate() {
            let server = Server::new(
                tool_turn_fixture(api, &["a"], "{}", stop, true),
                200,
                None,
                false,
            )
            .await;
            let chunks = collect(&server.provider(api)).await;
            assert!(
                !chunks
                    .iter()
                    .any(|chunk| matches!(chunk, Ok(ChatChunk::ToolCall(_)))),
                "{api:?}/{stop}: {chunks:?}"
            );
            // Rig rejects Anthropic refusals and unknown Responses terminal states as protocol failures.
            if (api.is_none() && index == 1)
                || (matches!(api, Some(OpenAiApi::Responses)) && index == 2)
            {
                protocol(&chunks);
                assert!(
                    !chunks
                        .iter()
                        .any(|chunk| matches!(chunk, Ok(ChatChunk::Done { .. })))
                );
            } else {
                let expected = match index {
                    0 => StopKind::MaxTokens,
                    1 => StopKind::Refusal,
                    _ => StopKind::Other(stop.into()),
                };
                assert!(
                    matches!(chunks.last(), Some(Ok(ChatChunk::Done { stop, .. })) if *stop == expected),
                    "{api:?}: {chunks:?}"
                );
                assert!(turn(&chunks).tool_calls.is_empty());
                assert!(turn(&chunks).replay.is_none());
            }
        }
    }
}

#[tokio::test]
async fn unfinished_accepted_call_is_protocol_error_on_every_route() {
    for (api, stop, _) in ROUTES {
        let server = Server::new(
            tool_turn_fixture(api, &["a"], "{\"x\":", stop, false),
            200,
            None,
            false,
        )
        .await;
        let chunks = collect(&server.provider(api)).await;
        protocol(&chunks);
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, Ok(ChatChunk::Done { .. })))
        );
    }
}

#[tokio::test]
async fn malformed_arguments_are_not_executable_on_every_route() {
    for (api, stop, _) in ROUTES {
        let server = Server::new(
            tool_turn_fixture(api, &["a"], "{broken}", stop, true),
            200,
            None,
            false,
        )
        .await;
        let chunks = collect(&server.provider(api)).await;
        protocol(&chunks);
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, Ok(ChatChunk::Done { .. })))
        );
    }
}

#[tokio::test]
async fn provider_call_ids_are_validated_on_every_route() {
    for (api, stop, _) in ROUTES {
        let server = Server::new(
            tool_turn_fixture(api, &["", ""], "{}", stop, true),
            200,
            None,
            false,
        )
        .await;
        let chunks = collect(&server.provider(api)).await;
        assert_eq!(
            turn(&chunks)
                .tool_calls
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["kw_call_0", "kw_call_1"],
            "{api:?}"
        );
        let server = Server::new(
            tool_turn_fixture(api, &["duplicate", "duplicate"], "{}", stop, true),
            200,
            None,
            false,
        )
        .await;
        let chunks = collect(&server.provider(api)).await;
        protocol(&chunks);
        assert_eq!(
            chunks.last(),
            Some(&Err(ModelError::Protocol("duplicate_tool_call_id".into())))
        );
    }
}

#[tokio::test]
async fn outbound_invalid_history_fails_before_network() {
    for (api, _, _) in ROUTES {
        for (id, name, args, code) in [
            ("", "first", "{}", "invalid_tool_call"),
            ("a", "", "{}", "empty_tool_name"),
            ("a", "first", "[]", "invalid_tool_call"),
            ("a", "first", "{broken}", "invalid_arguments"),
        ] {
            let server = Server::new(vec![], 200, None, true).await;
            let provider = server.provider(api);
            let mut req = request();
            req.messages.push(ChatMsg::Assistant {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: id.into(),
                    name: name.into(),
                    args_json: args.into(),
                }],
                replay: None,
            });
            let Err(error) = provider.stream(req).await else {
                panic!("invalid history accepted")
            };
            assert_eq!(error, ModelError::Protocol(code.into()));
            assert!(server.request.lock().await.is_none());
        }
        for (id, name, code) in [
            ("", "first", "empty_tool_call_id"),
            ("a", "", "empty_tool_name"),
        ] {
            let server = Server::new(vec![], 200, None, true).await;
            let mut req = request();
            req.messages.push(ChatMsg::Tool {
                tool_call_id: id.into(),
                name: name.into(),
                content: "result".into(),
                is_error: false,
            });
            let Err(error) = server.provider(api).stream(req).await else {
                panic!("invalid tool result accepted")
            };
            assert_eq!(error, ModelError::Protocol(code.into()));
            assert!(server.request.lock().await.is_none());
        }
        let server = Server::new(vec![], 200, None, true).await;
        let provider = server.provider(api);
        let mut req = request();
        req.messages.push(ChatMsg::Assistant {
            content: String::new(),
            tool_calls: vec![],
            replay: Some(ProviderReplay {
                provider_id: provider.id().into(),
                version: 1,
                data: json!({"invalid":"replay"}),
            }),
        });
        let Err(error) = provider.stream(req).await else {
            panic!("malformed replay accepted")
        };
        assert_eq!(error, ModelError::Protocol("invalid_replay".into()));
        assert!(server.request.lock().await.is_none());
    }
}

#[tokio::test]
async fn replay_version_and_canonical_calls_are_checked_on_every_route() {
    for (api, stop, response) in ROUTES {
        let server = Server::new(
            tool_turn_fixture(api, &["a"], "{\"x\":1}", stop, true),
            200,
            None,
            false,
        )
        .await;
        let chunks = collect(&server.provider(api)).await;
        let previous = turn(&chunks);
        for field in ["id", "name", "args"] {
            let server = Server::new(vec![], 200, None, true).await;
            let mut calls = previous.tool_calls.clone();
            match field {
                "id" => calls[0].id = "b".into(),
                "name" => calls[0].name = "second".into(),
                _ => calls[0].args_json = "{\"x\":2}".into(),
            }
            let mut req = request();
            req.messages.push(ChatMsg::Assistant {
                content: previous.content.clone(),
                tool_calls: calls,
                replay: previous.replay.clone(),
            });
            let Err(error) = server.provider(api).stream(req).await else {
                panic!("mismatched replay accepted")
            };
            assert_eq!(error, ModelError::Protocol("replay_mismatch".into()));
            assert!(server.request.lock().await.is_none());
        }
        let server = Server::new(fixture(response), 200, None, false).await;
        let mut req = request();
        req.messages.push(ChatMsg::Assistant {
            content: "canonical-version-fallback".into(),
            tool_calls: vec![],
            replay: Some(ProviderReplay {
                version: 2,
                data: json!({"invalid":"must not decode"}),
                ..previous.replay.clone().unwrap()
            }),
        });
        turn(
            &server
                .provider(api)
                .stream(req)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await,
        );
        let body = server.request.lock().await.clone().unwrap().to_string();
        assert!(body.contains("canonical-version-fallback"));
        assert!(!body.contains("must not decode"));
    }
}

#[tokio::test]
async fn provider_request_preserves_roles_limits_and_tools() {
    for (api, _, response) in ROUTES {
        let server = Server::new(fixture(response), 200, None, false).await;
        let mut req = request();
        req.max_output_tokens = 321;
        req.temperature = Some(0.25);
        req.messages[0] = ChatMsg::User {
            content: "user only".into(),
            name: Some("Mali".into()),
        };
        req.tools[0].parameters = json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"],"additionalProperties":false});
        req.messages.push(ChatMsg::Assistant {
            content: "checking".into(),
            tool_calls: vec![ToolCall {
                id: "a".into(),
                name: "first".into(),
                args_json: "{\"city\":\"Bangkok\"}".into(),
            }],
            replay: None,
        });
        req.messages.push(ChatMsg::Tool {
            tool_call_id: "a".into(),
            name: "first".into(),
            content: "denied".into(),
            is_error: true,
        });
        turn(
            &server
                .provider(api)
                .stream(req.clone())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await,
        );
        let body = server.request.lock().await.clone().unwrap();
        let schema = &req.tools[0].parameters;
        let expected = match api {
            Some(OpenAiApi::Chat) => json!({
                "model":"gpt-4.1", "messages":[
                    {"role":"system","content":[{"type":"text","text":"system only"}]},
                    {"role":"user","content":"Mali: user only"},
                    {"role":"assistant","content":"checking","tool_calls":[{"type":"function","id":"a","function":{"name":"first","arguments":"{\"city\":\"Bangkok\"}"}}]},
                    {"role":"tool","tool_call_id":"a","content":"Error: denied"}
                ], "tools":[{"type":"function","function":{"name":"first","description":"test","parameters":schema}}],
                "temperature":0.25,"max_tokens":321,"stream_options":{"include_usage":true},"stream":true
            }),
            Some(OpenAiApi::Responses) => json!({
                "model":"gpt-4.1","instructions":"system only","temperature":0.25,
                "tools":[{"type":"function","name":"first","parameters":schema,"strict":false,"description":"test"}],
                "store":false,"stream":true,"include":["reasoning.encrypted_content"],"input":[
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"Mali: user only"}]},
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking","annotations":[]}],"status":"completed","id":"msg_rig_2"},
                    {"type":"function_call","call_id":"a","name":"first","arguments":"{\"city\":\"Bangkok\"}"},
                    {"type":"function_call_output","call_id":"a","output":"Error: denied","status":"completed"}
                ], "max_output_tokens":321
            }),
            None => json!({
                "model":"claude-sonnet-4-6","messages":[
                    {"role":"user","content":[{"type":"text","text":"Mali: user only"}]},
                    {"role":"assistant","content":[{"type":"text","text":"checking"},{"type":"tool_use","id":"a","name":"first","input":{"city":"Bangkok"}}]},
                    {"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":[{"type":"text","text":"denied"}],"is_error":true}]}
                ],"max_tokens":321,"system":[{"type":"text","text":"system only"}],"temperature":0.25,
                "tools":[{"name":"first","description":"test","input_schema":schema,"eager_input_streaming":true}],
                "stream":true,"tool_choice":{"type":"auto"}
            }),
        };
        // Exact objects also exclude hosted tools, continuation IDs and extra provider options.
        assert_eq!(body, expected, "{api:?}");
    }
}

#[tokio::test]
async fn provider_errors_redact_keys_and_base_urls_in_display_and_debug() {
    // Local OpenAI-compatible endpoints share the OpenAI adapter; there is no LOCAL_API_KEY config field.
    for (api, _, _) in ROUTES {
        for status in [400, 401, 429, 500] {
            let key = uuid::Uuid::now_v7().to_string();
            let private_path = uuid::Uuid::now_v7().to_string();
            let body = json!({"error":{"type":"invalid_request_error","message":format!("{key} /{private_path}")}});
            let server = Server::new(serde_json::to_vec(&body).unwrap(), status, None, false).await;
            let base_url = format!("{}/{}", server.url, private_path);
            let env = ProviderEnv {
                api_key: key.clone(),
                base_url: Some(base_url.clone()),
            };
            let provider = match api {
                Some(api) => RigProvider::openai(&env, "gpt-4.1", api),
                None => RigProvider::anthropic(&env, "claude-sonnet-4-6"),
            }
            .unwrap();
            let chunks = collect(&provider).await;
            let Some(Err(error)) = chunks.last() else {
                panic!("{chunks:?}")
            };
            for output in [format!("{error}"), format!("{error:?}")] {
                for secret in [&key, &base_url, &server.url, &private_path] {
                    assert!(
                        !output.contains(secret),
                        "error leaked provider configuration"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn http_headers_stall_times_out_first_chunk() {
    let server = Server::new(vec![], 200, None, true).await;
    let provider = server.provider(Some(OpenAiApi::Chat)).with_http(
        BoundedHttp::new(HttpLimits {
            read: Duration::from_secs(2),
            ..HttpLimits::default()
        })
        .unwrap(),
    );
    // No guard_stream: only the real reqwest read deadline can end this request.
    let task = tokio::spawn(async move {
        provider
            .stream(request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    });
    server.arrived.notified().await;
    tokio::time::pause();
    let chunks = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("configured header deadline must fire")
        .unwrap();
    assert_eq!(
        chunks,
        [Err(ModelError::Timeout {
            phase: TimeoutPhase::FirstChunk
        })]
    );
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(10), server.closed.notified())
        .await
        .expect("client must disconnect");
}

#[tokio::test]
async fn http_body_stall_times_out_idle() {
    let frame = b"data: {\"id\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n";
    let server = Server::start(vec![frame.to_vec()], 200, true, "", Duration::ZERO, false).await;
    let provider = server.provider(Some(OpenAiApi::Chat)).with_http(
        BoundedHttp::new(HttpLimits {
            read: Duration::from_secs(2),
            ..HttpLimits::default()
        })
        .unwrap(),
    );
    let mut stream = provider.stream(request()).await.unwrap();
    loop {
        match stream.next().await.unwrap().unwrap() {
            ChatChunk::TextDelta(text) => {
                assert_eq!(text, "hello");
                break;
            }
            ChatChunk::Progress => {}
            other => panic!("expected content, got {other:?}"),
        }
    }
    tokio::time::pause();
    let tail = tokio::time::timeout(Duration::from_secs(3), stream.collect::<Vec<_>>())
        .await
        .expect("configured body deadline must fire");
    assert_eq!(
        tail.last(),
        Some(&Err(ModelError::Timeout {
            phase: TimeoutPhase::Idle
        }))
    );
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(10), server.closed.notified())
        .await
        .expect("client must disconnect");
}
