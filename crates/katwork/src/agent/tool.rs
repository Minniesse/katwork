use super::{StopReason, ToolCtx};
use crate::{
    models::ToolSpec,
    types::{Question, SideEffect},
};
use async_trait::async_trait;
use serde_json::Value;

/// Implementations own their future. Never detach effects; every write starts with authorize_write,
/// except `cite`: a `WorkspaceRead` tool whose citation insert is fenced (`fence::check`) only.
/// Non-Read effects must atomically store an idempotency receipt and reuse its recorded result.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn side_effect(&self) -> SideEffect;
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError>;
    /// Whether the result may carry content no person of the workspace wrote (U4). The loop
    /// records it on the call's row.
    fn output_trust(&self) -> OutputTrust {
        OutputTrust::Untrusted
    }
    /// The revision of what the call reads, so reading it again after it changed is not a
    /// repeat. The loop bounds it by the tool timeout and treats any failure as `None`.
    async fn target_revision(&self, _ctx: &ToolCtx, _args: &Value) -> Option<String> {
        None
    }
}
/// Whether a tool's result is trusted context (U4): `Trusted` only when it echoes a person's
/// answer or the Kat's own arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTrust {
    Trusted,
    #[default]
    Untrusted,
}
impl OutputTrust {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
        }
    }
}
#[derive(Debug)]
pub enum ToolOutput {
    Text(String),
    Ask(Question),
}
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("not_allowed")]
    NotAllowed,
    #[error("needs_approval")]
    NeedsApproval,
    #[error("invalid_args")]
    InvalidArgs(String),
    #[error("tool_failed")]
    Failed(String),
    #[error("timeout")]
    Timeout,
    #[error("outcome_unknown")]
    OutcomeUnknown,
    #[error("halted")]
    Halted(StopReason),
    /// A final failure the model sees by its code (`not_found`, `doc_timeout`, …): never
    /// retried, whatever the tool's effect. Codes come from `TOOL_CODES`.
    #[error("{0}")]
    Coded(&'static str),
}
/// Every code a `ToolError::Coded` may carry: the SP2a plan's tool error texts that tools of
/// this crate return, plus `not_found`, `blob_corrupt` and `file_type_unsupported`.
pub const TOOL_CODES: &[&str] = &[
    "not_found",
    "blob_corrupt",
    "file_too_large",
    "file_too_complex",
    "file_malformed",
    "file_encrypted",
    "file_type_unsupported",
    "doc_timeout",
    "span_unknown",
    "quote_not_in_span",
    "quote_ambiguous",
    "text_too_long",
    "citation_unknown",
    "artifact_not_writable",
    "read_back_failed",
    "xlsx_would_be_repaired",
];
impl ToolError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAllowed => "not_allowed",
            Self::NeedsApproval => "needs_approval",
            Self::InvalidArgs(_) => "invalid_args",
            Self::Failed(_) => "tool_failed",
            Self::Timeout => "timeout",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Halted(reason) => reason.wire(),
            Self::Coded(code) => code,
        }
    }
}
