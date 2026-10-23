//! Rig is confined to model I/O here. KatWork owns retries and tool execution.
use super::*;
use crate::config::ProviderEnv;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};
use rig_core::{
    completion::{CompletionRequest, CompletionResponse, FinishReason, ToolDefinition},
    error::ProviderError,
    http_client::{
        self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
    },
    message::{
        AssistantContent, AssistantMessage, CallId, Message, ToolFunction, ToolName, ToolResult,
        ToolResultContent,
    },
    providers::{anthropic::AnthropicConfig, openai::OpenAIConfig},
    streaming::{Item, StreamEvent},
};
use std::{collections::HashSet, sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy)]
pub struct HttpLimits {
    pub connect: Duration,
    pub read: Duration,
    pub max_response_bytes: usize,
    pub max_error_body_bytes: usize,
    pub max_sse_frame_bytes: usize,
}
impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(30),
            max_response_bytes: 16 * 1024 * 1024,
            max_error_body_bytes: 64 * 1024,
            max_sse_frame_bytes: 1024 * 1024,
        }
    }
}
#[derive(Clone)]
pub struct BoundedHttp {
    client: reqwest::Client,
    limits: HttpLimits,
}
impl BoundedHttp {
    pub fn new(limits: HttpLimits) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .connect_timeout(limits.connect)
            .read_timeout(limits.read)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| ModelError::ProviderUnavailable)?;
        Ok(Self { client, limits })
    }
    #[allow(
        clippy::result_large_err,
        reason = "Rig fixes the transport error type"
    )]
    async fn open(&self, req: Request<Bytes>) -> http_client::Result<reqwest::Response> {
        let (parts, body) = req.into_parts();
        let mut response = self
            .client
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body)
            .send()
            .await
            .map_err(http_client::Error::instance)?;
        if !response.status().is_success() {
            let status = response.status();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            while bytes.len() < self.limits.max_error_body_bytes {
                let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(http_client::Error::instance)?
                else {
                    break;
                };
                let take = chunk
                    .len()
                    .min(self.limits.max_error_body_bytes - bytes.len());
                bytes.extend_from_slice(&chunk[..take]);
            }
            let valid =
                std::str::from_utf8(&bytes).map_or_else(|error| error.valid_up_to(), str::len);
            let body = std::str::from_utf8(&bytes[..valid]).unwrap().to_owned();
            return Err(http_client::Error::non_success_with_details(
                status, headers, body,
            ));
        }
        Ok(response)
    }
    fn body(&self, response: reqwest::Response, sse: bool) -> http_client::BoxedStream {
        let limits = self.limits;
        Box::pin(async_stream::try_stream! {
            let mut stream = response.bytes_stream();
            let mut total = 0usize;
            let mut frame_bytes = 0usize;
            let mut line_bytes = 0usize;
            let mut last_cr = false;
            let mut decoder = Utf8Decoder::default();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(http_client::Error::instance)?;
                total = total.saturating_add(chunk.len());
                if total > limits.max_response_bytes {
                    Err(http_client::Error::instance(ModelError::Protocol(
                        "response_too_large".into(),
                    )))?;
                }
                if sse {
                    for &byte in &chunk {
                        frame_bytes += 1;
                        if frame_bytes > limits.max_sse_frame_bytes {
                            Err(http_client::Error::instance(ModelError::Protocol(
                                "sse_frame_too_large".into(),
                            )))?;
                        }
                        if byte == b'\n' && last_cr {
                            last_cr = false;
                            continue;
                        }
                        if matches!(byte, b'\r' | b'\n') {
                            if line_bytes == 0 {
                                frame_bytes = 0;
                            }
                            line_bytes = 0;
                        } else {
                            line_bytes += 1;
                        }
                        last_cr = byte == b'\r';
                    }
                    let decoded = decoder.push(&chunk).map_err(http_client::Error::instance)?;
                    if !decoded.is_empty() {
                        yield Bytes::from(decoded);
                    }
                } else {
                    yield chunk;
                }
            }
            if sse && !decoder.pending.is_empty() {
                Err(http_client::Error::instance(ModelError::Protocol(
                    "invalid_utf8".into(),
                )))?;
            }
        })
    }
}
/// Retains at most three trailing bytes between network reads.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}
impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ModelError> {
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return Err(ModelError::Protocol("invalid_utf8".into())),
        };
        let trailing = self.pending.split_off(valid);
        Ok(std::mem::replace(&mut self.pending, trailing))
    }
}
impl HttpClientExt for BoundedHttp {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let http = self.clone();
        let req = req.map(Into::into);
        async move {
            let response = http.open(req).await?;
            let mut head = Response::builder().status(response.status());
            *head.headers_mut().unwrap() = response.headers().clone();
            let mut stream = http.body(response, false);
            let body: LazyBody<U> = Box::pin(async move {
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                }
                Ok(U::from(Bytes::from(bytes)))
            });
            head.body(body).map_err(http_client::Error::Protocol)
        }
    }
    fn send_multipart<U>(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        std::future::ready(Err(http_client::Error::instance(ModelError::Protocol(
            "multipart_disabled".into(),
        ))))
    }
    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let http = self.clone();
        let req = req.map(Into::into);
        async move {
            let response = http.open(req).await?;
            let mut head = Response::builder().status(response.status());
            *head.headers_mut().unwrap() = response.headers().clone();
            // Model streaming endpoints use SSE regardless of response header spelling.
            head.body(http.body(response, true))
                .map_err(http_client::Error::Protocol)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum OpenAiApi {
    Chat,
    Responses,
}
#[derive(Clone)]
enum Backend {
    OpenAi(Box<OpenAIConfig>, OpenAiApi),
    Anthropic(AnthropicConfig),
}
#[derive(Clone)]
pub struct RigProvider {
    backend: Backend,
    model: String,
    http: BoundedHttp,
}
fn validate_env(env: &ProviderEnv) -> Result<(), ModelError> {
    if env.api_key.trim().is_empty() || http_client::HeaderValue::from_str(&env.api_key).is_err() {
        return Err(ModelError::AuthFailed);
    }
    if let Some(base) = &env.base_url {
        let url =
            url::Url::parse(base).map_err(|_| ModelError::Protocol("invalid_base_url".into()))?;
        if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
            return Err(ModelError::Protocol("invalid_base_url".into()));
        }
    }
    Ok(())
}
impl RigProvider {
    pub fn openai(env: &ProviderEnv, model: &str, api: OpenAiApi) -> Result<Self, ModelError> {
        validate_env(env)?;
        let mut config = OpenAIConfig::new(env.api_key.clone());
        if let Some(base) = &env.base_url {
            config = config.with_base_url(base);
        }
        Ok(Self {
            backend: Backend::OpenAi(Box::new(config), api),
            model: model.into(),
            http: BoundedHttp::new(HttpLimits::default())?,
        })
    }
    pub fn anthropic(env: &ProviderEnv, model: &str) -> Result<Self, ModelError> {
        validate_env(env)?;
        let mut config = AnthropicConfig::new(env.api_key.clone());
        if let Some(base) = &env.base_url {
            config = config.with_base_url(base);
        }
        Ok(Self {
            backend: Backend::Anthropic(config),
            model: model.into(),
            http: BoundedHttp::new(HttpLimits::default())?,
        })
    }
    pub fn with_http(mut self, http: BoundedHttp) -> Self {
        self.http = http;
        self
    }
    fn stream_with_http<H: HttpClientExt + Clone + 'static>(
        &self,
        req: ChatRequest,
        http: H,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        let mut request = to_rig(req, self.id())?;
        if matches!(self.backend, Backend::OpenAi(_, OpenAiApi::Responses)) {
            request = request.provider_option(
                rig_core::providers::openai::extension::OpenAiOptions::new().store(false),
            );
        }
        let final_wire = match &self.backend {
            Backend::OpenAi(config, OpenAiApi::Responses) => Some(
                rig_core::providers::openai::responses_api::wire::Responses::new(
                    config.as_ref().clone(),
                    &self.model,
                ),
            ),
            _ => None,
        };
        let sent = request.clone();
        let model = match &self.backend {
            Backend::OpenAi(config, api) => {
                let client = config.as_ref().clone().connect(http.clone());
                match api {
                    OpenAiApi::Chat => client.chat(&self.model).erase(),
                    OpenAiApi::Responses => client.responses(&self.model).erase(),
                }
            }
            Backend::Anthropic(config) => config
                .clone()
                .connect(http.clone())
                .completion(&self.model)
                .erase(),
        };
        let mut stream = model
            .stream(request)
            .map_err(|error| model_error(error, TimeoutPhase::FirstChunk))?;
        let provider_id = self.id().to_owned();
        Ok(Box::pin(async_stream::try_stream! {
            let mut phase = TimeoutPhase::FirstChunk;
            while let Some(item) = stream.next().await {
                let Item::Event(event) = item.map_err(|error| model_error(error, phase))? else {
                    continue;
                };
                if let StreamEvent::Text { text, .. } = event {
                    yield ChatChunk::TextDelta(text);
                } else {
                    yield ChatChunk::Progress;
                }
                phase = TimeoutPhase::Idle;
            }
            let mut response = stream
                .finish()
                .await
                .map_err(|error| model_error(error, phase))?;
            if let Some(wire) = final_wire {
                // Rig's stream keeps closed parts; the final document is authoritative.
                response = rig_core::wire::decode_reply(
                    &wire,
                    &sent,
                    rig_core::wire::Mode::Unary,
                    [rig_core::wire::WireFrame::Bytes(
                        serde_json::to_vec(&response.raw)
                            .map_err(|_| ModelError::Protocol("invalid_final_response".into()))?,
                    )],
                )
                .map_err(|error| model_error(error, phase))?;
            }
            let usage = &response.usage;
            yield ChatChunk::Usage(Usage {
                input: usage.input_tokens,
                cache_read: usage.cached_input_tokens,
                cache_write: usage.cache_creation_input_tokens,
                output: usage.output_tokens,
                complete: usage.input_tokens.is_some() && usage.output_tokens.is_some(),
            });
            let (stop, turn) = accepted_turn(response, &provider_id)?;
            for call in &turn.tool_calls {
                yield ChatChunk::ToolCall(call.clone());
            }
            yield ChatChunk::Done { stop, turn };
        }))
    }
}
#[async_trait]
impl ModelProvider for RigProvider {
    fn id(&self) -> &str {
        match self.backend {
            Backend::OpenAi(_, OpenAiApi::Chat) => "openai:chat",
            Backend::OpenAi(_, OpenAiApi::Responses) => "openai:responses",
            Backend::Anthropic(_) => "anthropic",
        }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::External,
            synthetic: false,
        }
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        self.stream_with_http(req, self.http.clone())
    }
}
fn tool_name(name: &str) -> Result<ToolName, ModelError> {
    ToolName::new(name).map_err(|_| ModelError::Protocol("empty_tool_name".into()))
}
fn to_rig(req: ChatRequest, provider_id: &str) -> Result<CompletionRequest, ModelError> {
    let mut messages = vec![Arc::new(Message::system(req.system))];
    for message in req.messages {
        let message = match message {
            ChatMsg::User { content, name } => Message::user(match name {
                Some(name) => format!("{name}: {content}"),
                None => content,
            }),
            ChatMsg::Tool {
                tool_call_id,
                name,
                content,
                is_error,
            } => {
                if tool_call_id.is_empty() {
                    return Err(ModelError::Protocol("empty_tool_call_id".into()));
                }
                let content = if is_error && provider_id.starts_with("openai:") {
                    format!("Error: {content}")
                } else {
                    content
                };
                Message::tool_results(vec![ToolResult {
                    call: CallId::from_wire(tool_call_id),
                    name: tool_name(&name)?,
                    content: vec![ToolResultContent::text(content)],
                    is_error,
                }])
            }
            ChatMsg::Assistant {
                content,
                tool_calls,
                replay,
            } => {
                let mut parts = Vec::new();
                if !content.is_empty() {
                    parts.push(AssistantContent::text(&content));
                }
                for call in &tool_calls {
                    let args: serde_json::Value = serde_json::from_str(&call.args_json)
                        .map_err(|_| ModelError::Protocol("invalid_arguments".into()))?;
                    if call.id.is_empty() || !args.is_object() {
                        return Err(ModelError::Protocol("invalid_tool_call".into()));
                    }
                    parts.push(AssistantContent::ToolCall(
                        rig_core::message::ToolCall::new(
                            CallId::from_wire(&call.id),
                            ToolFunction::new(tool_name(&call.name)?, args),
                        ),
                    ));
                }
                let canonical = AssistantMessage::new(parts);
                let assistant =
                    match replay.filter(|r| r.provider_id == provider_id && r.version == 1) {
                        Some(replay) => {
                            let replay: AssistantMessage = serde_json::from_value(replay.data)
                                .map_err(|_| ModelError::Protocol("invalid_replay".into()))?;
                            let replay_calls = replay
                                .tool_calls()
                                .map(|c| (c.id.wire().into_owned(), c.function.clone()))
                                .collect::<Vec<_>>();
                            let canonical_calls = canonical
                                .tool_calls()
                                .map(|c| (c.id.wire().into_owned(), c.function.clone()))
                                .collect::<Vec<_>>();
                            if replay.text() != content || replay_calls != canonical_calls {
                                return Err(ModelError::Protocol("replay_mismatch".into()));
                            }
                            replay
                        }
                        None => canonical,
                    };
                Message::Assistant(assistant)
            }
        };
        messages.push(Arc::new(message));
    }
    let mut request = CompletionRequest::new("");
    request.chat_history = messages;
    request.max_tokens = Some(u64::from(req.max_output_tokens));
    request.temperature = req.temperature.map(f64::from);
    for spec in req.tools {
        request.tools.push(Arc::new(ToolDefinition::new(
            tool_name(&spec.name)?,
            spec.description,
            spec.parameters,
        )));
    }
    Ok(request)
}
fn accepted_turn(
    mut response: CompletionResponse,
    provider_id: &str,
) -> Result<(StopKind, AssistantTurn), ModelError> {
    let stop = match response.finish_reason() {
        Some(FinishReason::Stop) => StopKind::EndTurn,
        Some(FinishReason::ToolCalls) => StopKind::ToolUse,
        Some(FinishReason::Length) => StopKind::MaxTokens,
        Some(FinishReason::ContentFilter) => StopKind::Refusal,
        Some(FinishReason::Other(reason)) => StopKind::Other(reason),
        Some(_) => StopKind::Other("unknown".into()),
        None => return Err(ModelError::Protocol("missing_finish_reason".into())),
    };
    if response.error.is_some() || response.aborted.is_some() {
        return Err(ModelError::Protocol("failed_turn".into()));
    }
    validate_wire_ids(&response.raw, provider_id)?;
    let accepted = matches!(stop, StopKind::EndTurn | StopKind::ToolUse);
    let mut calls = Vec::new();
    let mut ids = HashSet::new();
    if accepted {
        if response.unfinished_calls().next().is_some() {
            return Err(ModelError::Protocol("unfinished_tool_call".into()));
        }
        for part in &mut response.choice {
            if let AssistantContent::ToolCall(call) = part {
                if call.function.invalid_arguments.is_some() {
                    return Err(ModelError::Protocol("invalid_arguments".into()));
                }
                if call.id.is_local() {
                    call.id = CallId::from_wire(format!("kw_call_{}", calls.len()));
                    call.native = None;
                }
                let id = call.id.wire().into_owned();
                if !ids.insert(id.clone()) {
                    return Err(ModelError::Protocol("duplicate_tool_call_id".into()));
                }
                calls.push(ToolCall {
                    id,
                    name: call.function.name.to_string(),
                    args_json: serde_json::to_string(&call.function.arguments)
                        .map_err(|_| ModelError::Protocol("invalid_arguments".into()))?,
                });
            }
        }
        if stop == StopKind::ToolUse && calls.is_empty() {
            return Err(ModelError::Protocol("tool_use_without_call".into()));
        }
    }
    let replay = if accepted {
        match response.message() {
            Some(Message::Assistant(message)) => Some(ProviderReplay {
                provider_id: provider_id.into(),
                version: 1,
                data: serde_json::to_value(message)
                    .map_err(|_| ModelError::Protocol("invalid_replay".into()))?,
            }),
            _ => None,
        }
    } else {
        None
    };
    Ok((
        stop,
        AssistantTurn {
            content: response.text(),
            tool_calls: calls,
            replay,
        },
    ))
}
fn validate_wire_ids(raw: &serde_json::Value, provider_id: &str) -> Result<(), ModelError> {
    let (calls, id_key, kind) = match provider_id {
        "openai:chat" => (raw.pointer("/choices/0/message/tool_calls"), "id", None),
        "openai:responses" => (raw.get("output"), "call_id", Some("function_call")),
        "anthropic" => (raw.get("content"), "id", Some("tool_use")),
        _ => return Ok(()),
    };
    let mut ids = HashSet::new();
    for call in calls.and_then(|v| v.as_array()).into_iter().flatten() {
        if kind.is_some_and(|kind| call.get("type").and_then(|v| v.as_str()) != Some(kind)) {
            continue;
        }
        if let Some(id) = call
            .get(id_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            && !ids.insert(id)
        {
            return Err(ModelError::Protocol("duplicate_tool_call_id".into()));
        }
    }
    Ok(())
}
fn model_error(error: ProviderError, phase: TimeoutPhase) -> ModelError {
    if let ProviderError::Http(http) = &error {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(http.as_ref());
        while let Some(cause) = source {
            if let Some(error) = cause.downcast_ref::<ModelError>() {
                return error.clone();
            }
            if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
                return if error.is_timeout() {
                    ModelError::Timeout { phase }
                } else {
                    ModelError::ProviderUnavailable
                };
            }
            source = cause.source();
        }
    }
    let status = error.provider_response_status().map(|s| s.as_u16());
    let body = error
        .provider_response_json()
        .ok()
        .flatten()
        .unwrap_or_default();
    let detail = body
        .get("error")
        .or_else(|| body.pointer("/response/error"))
        .unwrap_or(&body);
    let code = detail
        .get("code")
        .and_then(|v| v.as_str())
        .or_else(|| detail.get("type").and_then(|v| v.as_str()))
        .unwrap_or("");
    if matches!(status, Some(401 | 403))
        || matches!(error, ProviderError::InvalidAuthentication(_))
        || matches!(
            code,
            "authentication_error" | "permission_error" | "invalid_api_key"
        )
    {
        return ModelError::AuthFailed;
    }
    if matches!(
        code,
        "insufficient_quota" | "quota_exceeded" | "billing_error" | "credit_balance_too_low"
    ) {
        return ModelError::QuotaExceeded { resets_at: None };
    }
    if status == Some(429) || matches!(code, "rate_limit_error" | "rate_limit_exceeded") {
        return ModelError::RateLimited {
            retry_after_secs: error
                .provider_response_headers()
                .and_then(|h| h.get("retry-after"))
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok()),
        };
    }
    if status.is_some_and(|s| s >= 500)
        || matches!(code, "overloaded_error" | "server_error" | "api_error")
    {
        return ModelError::ProviderUnavailable;
    }
    match error {
        ProviderError::Http(_) => ModelError::ProviderUnavailable,
        ProviderError::Truncated => ModelError::Protocol("stream_without_done".into()),
        _ => ModelError::Protocol("invalid_provider_response".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[derive(Clone)]
    struct TimedHttp {
        frames: Vec<(Duration, &'static str)>,
        attempts: Arc<AtomicUsize>,
    }
    impl HttpClientExt for TimedHttp {
        fn send<T: Into<Bytes> + Send, U: From<Bytes> + Send + 'static>(
            &self,
            _: Request<T>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
        {
            std::future::ready(Err(http_client::Error::instance(std::io::Error::other(
                "fixture is streaming only",
            ))))
        }
        fn send_multipart<U: From<Bytes> + Send + 'static>(
            &self,
            _: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
        {
            std::future::ready(Err(http_client::Error::instance(std::io::Error::other(
                "fixture is streaming only",
            ))))
        }
        async fn send_streaming<T: Into<Bytes> + Send>(
            &self,
            _: Request<T>,
        ) -> http_client::Result<StreamingResponse> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let frames = self.frames.clone();
            let frames: http_client::BoxedStream = Box::pin(async_stream::stream! {
                for (delay, frame) in frames {
                    tokio::time::sleep(delay).await;
                    yield Ok(Bytes::from_static(frame.as_bytes()));
                }
            });
            Ok(Response::builder()
                .header("content-type", "text/event-stream")
                .body(frames)
                .unwrap())
        }
    }

    fn timed_stream(
        frames: Vec<(Duration, &'static str)>,
        api: Option<OpenAiApi>,
    ) -> (
        BoxStream<'static, Result<ChatChunk, ModelError>>,
        Arc<AtomicUsize>,
    ) {
        let env = ProviderEnv {
            api_key: uuid::Uuid::now_v7().to_string(),
            base_url: None,
        };
        let provider = match api {
            Some(api) => RigProvider::openai(&env, "gpt-4.1", api),
            None => RigProvider::anthropic(&env, "claude-sonnet-4-6"),
        }
        .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let request = ChatRequest {
            system: "test".into(),
            messages: vec![ChatMsg::User {
                content: "hi".into(),
                name: None,
            }],
            tools: vec![],
            max_output_tokens: 128,
            temperature: None,
        };
        let stream = provider
            .stream_with_http(
                request,
                TimedHttp {
                    frames,
                    attempts: attempts.clone(),
                },
            )
            .unwrap();
        (stream, attempts)
    }

    async fn timed_fixture(
        frames: Vec<(Duration, &'static str)>,
        api: Option<OpenAiApi>,
    ) -> Vec<Result<ChatChunk, ModelError>> {
        let (stream, attempts) = timed_stream(frames, api);
        let start = tokio::time::Instant::now();
        let chunks = guard_stream(stream, StreamTimeouts::default())
            .collect::<Vec<_>>()
            .await;
        assert!(
            matches!(chunks.last(), Some(Ok(ChatChunk::Done { .. }))),
            "{api:?}: {chunks:?}"
        );
        assert!(start.elapsed() > Duration::from_secs(30));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            chunks
                .iter()
                .filter(|chunk| matches!(chunk, Ok(ChatChunk::Usage(_))))
                .count(),
            1
        );
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, Ok(ChatChunk::Progress)))
        );
        chunks
    }

    #[tokio::test(start_paused = true)]
    async fn argument_only_stream_keeps_first_and_idle_timers_alive() {
        let chunks = timed_fixture(
            include_str!("../../tests/fixtures/models/chat_arguments.sse")
                .split_inclusive("\n\n")
                .map(|frame| (Duration::from_secs(20), frame))
                .collect(),
            Some(OpenAiApi::Chat),
        )
        .await;
        let Some(Ok(ChatChunk::Done { stop, turn })) = chunks.last() else {
            unreachable!()
        };
        assert_eq!(*stop, StopKind::ToolUse);
        assert!(turn.content.is_empty());
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].args_json, r#"{"x":123}"#);
        assert!(chunks.contains(&Ok(ChatChunk::Usage(Usage {
            input: Some(5),
            output: Some(7),
            complete: true,
            ..Usage::default()
        }))));
    }

    #[tokio::test(start_paused = true)]
    async fn reasoning_content_keeps_stream_alive() {
        let mut frames: Vec<_> = include_str!("../../tests/fixtures/models/anthropic_replay.sse")
            .split_inclusive("\n\n")
            .enumerate()
            .map(|(i, frame)| {
                // Empty starts and signature-only frames emit no Rig content event.
                let delay = if matches!(i, 2 | 4 | 6 | 7) { 20 } else { 0 };
                (Duration::from_secs(delay), frame)
            })
            .collect();
        frames.insert(3, frames[2]);
        timed_fixture(frames, None).await;
    }

    async fn assert_keepalive_timeout(
        frame: &'static str,
        api: Option<OpenAiApi>,
        after_text: bool,
    ) {
        let mut frames = Vec::new();
        if after_text {
            frames.push((Duration::from_secs(10), "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"hello\"}}\n\n"));
        }
        frames.extend(std::iter::repeat_n((Duration::from_secs(10), frame), 12));
        let (stream, _) = timed_stream(frames, api);
        let start = tokio::time::Instant::now();
        let chunks = guard_stream(stream, StreamTimeouts::default())
            .collect::<Vec<_>>()
            .await;
        let phase = if after_text {
            TimeoutPhase::Idle
        } else {
            TimeoutPhase::FirstChunk
        };
        assert_eq!(
            chunks.last(),
            Some(&Err(ModelError::Timeout { phase })),
            "{chunks:?}"
        );
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(if after_text { 40 } else { 30 })
        );
        if after_text {
            assert!(chunks.contains(&Ok(ChatChunk::TextDelta("hello".into()))));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ping_only_times_out_at_first_chunk() {
        assert_keepalive_timeout("event: ping\ndata: {\"type\":\"ping\"}\n\n", None, false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn comment_only_times_out_at_first_chunk() {
        assert_keepalive_timeout(": keepalive\n\n", Some(OpenAiApi::Chat), false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn bare_newline_only_times_out_at_first_chunk() {
        assert_keepalive_timeout("\n", Some(OpenAiApi::Chat), false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn empty_delta_chat_times_out_at_first_chunk() {
        assert_keepalive_timeout("data: {\"id\":\"chat_test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\n", Some(OpenAiApi::Chat), false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_chat_events_time_out_at_first_chunk() {
        assert_keepalive_timeout(
            "data: {\"type\":\"future_event\",\"extra\":\"ignored\"}\n\n",
            Some(OpenAiApi::Chat),
            false,
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_responses_events_time_out_at_first_chunk() {
        assert_keepalive_timeout("event: response.future_event\ndata: {\"type\":\"response.future_event\",\"extra\":\"ignored\"}\n\n", Some(OpenAiApi::Responses), false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn message_start_only_times_out_at_first_chunk() {
        let frame = include_str!("../../tests/fixtures/models/anthropic_replay.sse")
            .split_inclusive("\n\n")
            .next()
            .unwrap();
        assert_keepalive_timeout(frame, None, false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn usage_only_times_out_at_first_chunk() {
        assert_keepalive_timeout("data: {\"id\":\"chat_test\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4.1\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7,\"total_tokens\":12}}\n\n", Some(OpenAiApi::Chat), false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn pings_after_text_time_out_at_idle() {
        assert_keepalive_timeout("event: ping\ndata: {\"type\":\"ping\"}\n\n", None, true).await;
    }

    #[test]
    fn utf8_decoder_retains_split_codepoints() {
        for (text, split) in [("ส", 1), ("ส", 2), ("👋", 1)] {
            let mut decoder = Utf8Decoder::default();
            let bytes = text.as_bytes();
            assert!(decoder.push(&bytes[..split]).unwrap().is_empty());
            assert_eq!(decoder.push(&bytes[split..]).unwrap(), bytes);
            assert!(decoder.pending.is_empty());
        }
    }

    #[tokio::test]
    async fn sse_frame_cap_resets_on_bare_cr_lines() {
        let body = b"data: a\r\r".repeat(20);
        let (url, server) = response(200, body.clone()).await;
        let http = BoundedHttp::new(HttpLimits {
            max_sse_frame_bytes: 16,
            ..HttpLimits::default()
        })
        .unwrap();
        let response = http
            .send_streaming(Request::get(url).body(Bytes::new()).unwrap())
            .await
            .unwrap();
        let chunks = response.into_body().collect::<Vec<_>>().await;
        let decoded = chunks
            .into_iter()
            .flat_map(|chunk| chunk.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(decoded, body);
        server.await.unwrap();
    }

    async fn response(status: u16, body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(format!("HTTP/1.1 {status} fixture\r\nContent-Type: text/event-stream ; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn unary_response_is_capped_before_buffering() {
        let (url, server) = response(200, vec![0xff; 128]).await;
        let http = BoundedHttp::new(HttpLimits {
            max_response_bytes: 32,
            ..HttpLimits::default()
        })
        .unwrap();
        let reply: Response<LazyBody<Bytes>> = http
            .send(Request::get(url).body(Bytes::new()).unwrap())
            .await
            .unwrap();
        let error = reply.into_body().await.unwrap_err();
        assert!(
            matches!(error,http_client::Error::Instance(ref e) if e.downcast_ref::<ModelError>()==Some(&ModelError::Protocol("response_too_large".into())))
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_error_bytes_cannot_expand_beyond_the_error_cap() {
        let (url, server) = response(500, vec![0xff; 128]).await;
        let http = BoundedHttp::new(HttpLimits {
            max_error_body_bytes: 32,
            ..HttpLimits::default()
        })
        .unwrap();
        let error = http
            .open(Request::get(url).body(Bytes::new()).unwrap())
            .await
            .unwrap_err();
        assert!(error.non_success_body().unwrap().len() <= 32);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn sse_caps_cannot_be_bypassed_by_content_type_whitespace() {
        let (url, server) = response(200, b"data: ".repeat(20)).await;
        let http = BoundedHttp::new(HttpLimits {
            max_sse_frame_bytes: 32,
            ..HttpLimits::default()
        })
        .unwrap();
        let response = http
            .send_streaming(Request::get(url).body(Bytes::new()).unwrap())
            .await
            .unwrap();
        let chunks = response.into_body().collect::<Vec<_>>().await;
        assert!(chunks.iter().any(|chunk|matches!(chunk, Err(http_client::Error::Instance(e)) if e.downcast_ref::<ModelError>() == Some(&ModelError::Protocol("sse_frame_too_large".into())))));
        server.await.unwrap();
    }
}
