use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::types::ReasonCode;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError>;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<ChatMsg>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}
impl ChatRequest {
    /// Conservative input charge for the registry's UTF-8 byte tokenizers.
    /// Includes an initial 1,024-token tool-use allowance for each supported
    /// provider, covering Anthropic's implicit tool system prompt as well as OpenAI.
    pub fn input_charge_bound(&self) -> u64 {
        let tool_overhead = if self.tools.is_empty() { 0 } else { 1024 };
        serde_json::to_vec(self)
            .map_or(u64::MAX, |json| json.len() as u64)
            .saturating_add(tool_overhead)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMsg {
    User {
        content: String,
        name: Option<String>,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
        replay: Option<ProviderReplay>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
        is_error: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}
/// Trusted history only: callers keep this state within its workspace and thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderReplay {
    pub provider_id: String,
    pub version: u32,
    pub data: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ChatChunk {
    /// Non-text content (part boundaries, reasoning or tool arguments).
    /// `guard_stream` forwards this: callers must disable retries once content arrives,
    /// including `Progress`, without emitting user text or executing a tool.
    Progress,
    TextDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Done {
        stop: StopKind,
        turn: AssistantTurn,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantTurn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub replay: Option<ProviderReplay>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopKind {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other(String),
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output: Option<u64>,
    pub complete: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    FirstChunk,
    Idle,
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("provider_unavailable")]
    ProviderUnavailable,
    #[error("residency_denied")]
    ResidencyDenied,
    #[error("provider_timeout")]
    Timeout { phase: TimeoutPhase },
    #[error("provider_rate_limited")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("provider_quota")]
    QuotaExceeded { resets_at: Option<DateTime<Utc>> },
    #[error("provider_auth_failed")]
    AuthFailed,
    #[error("model_protocol_error")]
    Protocol(String),
    #[error("provider_unavailable")]
    Other(String),
}
impl ModelError {
    /// Exhaustive gateway outcome; the agent loop maps this to its StopReason.
    pub fn reason_code(&self) -> ReasonCode {
        match self {
            Self::ProviderUnavailable | Self::Timeout { .. } | Self::Other(_) => {
                ReasonCode::ProviderUnavailable
            }
            Self::ResidencyDenied => ReasonCode::ResidencyDenied,
            Self::RateLimited { .. } => ReasonCode::ProviderRateLimited,
            Self::QuotaExceeded { .. } => ReasonCode::ProviderQuota,
            Self::AuthFailed => ReasonCode::ProviderAuthFailed,
            Self::Protocol(_) => ReasonCode::ModelProtocolError,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub tools: bool,
    pub residency: Residency,
    pub synthetic: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Residency {
    Local,
    External,
}
