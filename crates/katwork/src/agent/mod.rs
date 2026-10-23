pub mod builtin;
pub mod ctx;
pub mod guards;
pub mod ledger;
pub mod registry;
pub mod run;
pub mod tool;

pub use ctx::*;
pub use registry::{DuplicateTool, ToolRegistry};
pub use tool::{Tool, ToolError, ToolOutput};

use crate::{
    events::EventSink,
    ids::*,
    models::{ChatMsg, Registry},
    tenancy::Tenant,
    types::{Question, SideEffect, SpacePolicy},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Initiator {
    Person,
    Routine,
    Handoff,
    System,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Initial,
    Repair,
    Recovery,
}
impl RunKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Repair => "repair",
            Self::Recovery => "recovery",
        }
    }
}
#[derive(Debug, Clone)]
pub struct RunInput {
    pub tenant: Tenant,
    pub run_id: RunId,
    pub thread_id: ThreadId,
    pub space_id: SpaceId,
    pub job_id: JobId,
    pub task_id: TaskId,
    pub lease_epoch: LeaseEpoch,
    pub kind: RunKind,
    pub space_policy: SpacePolicy,
    pub history: Vec<ChatMsg>,
    pub reply_message_id: MessageId,
    pub policy_version: i64,
    pub actor: UserId,
    pub initiator: Initiator,
    pub deadline: Instant,
    pub cancel: CancellationToken,
}
pub struct RunEnv<'a> {
    pub lock_timeout: std::time::Duration,
    pub pool: &'a PgPool,
    pub models: &'a Registry,
    pub tools: &'a ToolRegistry,
    pub sink: &'a dyn EventSink,
}
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunAbort {
    #[error("lease_lost")]
    LeaseLost,
    #[error("{code}")]
    Infrastructure {
        run_id: Option<RunId>,
        code: &'static str,
    },
}
#[derive(Debug, Clone)]
pub struct RunFacts {
    pub run_id: Option<RunId>,
    pub reply_message_id: MessageId,
    pub final_text: Option<String>,
    pub stop: Option<StopReason>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub artifacts_created: u32,
    pub pending_tool_call: Option<PendingCall>,
    pub pending_question: Option<Question>,
    pub synthetic: bool,
    pub executor_unavailable: bool,
    pub run_closed: bool,
    pub usage: Vec<ModelCallId>,
}
impl RunFacts {
    pub fn not_run(stop: StopReason) -> Self {
        Self {
            run_id: None,
            reply_message_id: MessageId::new(),
            final_text: None,
            stop: Some(stop),
            tool_calls: vec![],
            artifacts_created: 0,
            pending_tool_call: None,
            pending_question: None,
            synthetic: false,
            executor_unavailable: false,
            run_closed: false,
            usage: vec![],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: ToolCallRecordId,
    pub step: u32,
    pub tool_call_id: String,
    pub name: String,
    pub side_effect: Option<SideEffect>,
    pub state: ToolCallState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallState {
    Started,
    Succeeded,
    Failed,
    TimedOut,
    OutcomeUnknown,
    Refused,
}
impl ToolCallState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Refused => "refused",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    pub record_id: ToolCallRecordId,
    pub tool_call_id: String,
    pub name: String,
    pub args_canonical: String,
    pub args_sha256: String,
}
macro_rules! stops {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum StopReason { $($variant),+ }
        impl StopReason { pub fn wire(self)-> &'static str {match self {$(Self::$variant=>$code),+}} }
    }
}
stops! {
    ProviderUnavailable=>"provider_unavailable", ProviderRateLimited=>"provider_rate_limited",ProviderQuota=>"provider_quota",ProviderAuthFailed=>"provider_auth_failed",
    ModelProtocol=>"model_protocol",StepLimit=>"step_limit",LoopDetected=>"loop_detected",NeedsApproval=>"needs_approval",TokenBudget=>"token_budget",OutputTruncated=>"output_truncated",TimeLimit=>"time_limit",
    PolicyChanged=>"policy_changed",ResidencyDenied=>"residency_denied",QuotaExceeded=>"quota_exceeded",TooManyRequests=>"too_many_requests",WorkspacePaused=>"workspace_paused",KatPaused=>"kat_paused",
    NoEligibleKat=>"no_eligible_kat",Interrupted=>"interrupted",OutcomeUnknown=>"outcome_unknown",Cancelled=>"cancelled"
}
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_steps: u32,
    pub tool_timeout: Duration,
    pub result_max_chars: usize,
    pub repeat_limit: u32,
    pub repeat_window: (u32, u32),
    pub max_total_tokens: u64,
    pub task_deadline: Duration,
    pub job_deadline: Duration,
    pub model_retries: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 24,
            tool_timeout: Duration::from_secs(60),
            result_max_chars: 8000,
            repeat_limit: 3,
            repeat_window: (5, 10),
            max_total_tokens: 200_000,
            task_deadline: Duration::from_secs(300),
            job_deadline: Duration::from_secs(600),
            model_retries: 1,
        }
    }
}
