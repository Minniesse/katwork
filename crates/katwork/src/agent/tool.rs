use super::{StopReason, ToolCtx};
use crate::{
    models::ToolSpec,
    types::{Question, SideEffect},
};
use async_trait::async_trait;
use serde_json::Value;

/// Implementations own their future. Never detach effects; every write starts with authorize_write.
/// Non-Read effects must atomically store an idempotency receipt and reuse its recorded result.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn side_effect(&self) -> SideEffect;
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError>;
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
}
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
        }
    }
}
