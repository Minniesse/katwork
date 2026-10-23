use serde::{Deserialize, Serialize};

use crate::{
    agent::{RunFacts, StopReason, ToolCallState},
    events::{DecisionEvent, KatworkEvent},
    ids::JobId,
    types::{JobStatus, ReasonCode, SideEffect},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    RunCheapChecks,
    AskUser,
    HandToLead,
    SaveDraftWithReport,
    Retry,
    ReviewBeforeRetry,
    RestartJob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    pub version: u32,
}

impl Thresholds {
    pub const SP1: Self = Self { version: 1 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    Accepted,
    Ready,
    Answered,
    Stopped,
    NeedsInput,
}

impl From<PolicyStatus> for JobStatus {
    fn from(status: PolicyStatus) -> Self {
        match status {
            PolicyStatus::Accepted => Self::Accepted,
            PolicyStatus::Ready => Self::Ready,
            PolicyStatus::Answered => Self::Answered,
            PolicyStatus::Stopped => Self::Stopped,
            PolicyStatus::NeedsInput => Self::NeedsInput,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub status: PolicyStatus,
    pub reason: Option<ReasonCode>,
    pub next: Option<NextStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyInputs {
    pub stop: Option<StopReason>,
    pub executor_unavailable: bool,
    pub has_final_text: bool,
    pub pending_question: bool,
    pub artifacts_created: u32,
    pub tool_calls: u32,
    pub effects_dispatched: bool,
    pub synthetic: bool,
}

impl PolicyInputs {
    pub fn from_facts(facts: &RunFacts) -> Self {
        Self {
            stop: facts.stop,
            executor_unavailable: facts.executor_unavailable,
            has_final_text: facts.final_text.is_some(),
            pending_question: facts.pending_question.is_some(),
            artifacts_created: facts.artifacts_created,
            tool_calls: u32::try_from(facts.tool_calls.len()).unwrap_or(u32::MAX),
            // Unknown effects fail closed; only a known Read or refused call is safe to repeat.
            effects_dispatched: facts.tool_calls.iter().any(|call| {
                call.side_effect != Some(SideEffect::Read) && call.state != ToolCallState::Refused
            }),
            synthetic: facts.synthetic,
        }
    }
}

pub fn decide_job(facts: &RunFacts, t: &Thresholds) -> Verdict {
    decide(&PolicyInputs::from_facts(facts), t)
}

pub fn decide(inputs: &PolicyInputs, _t: &Thresholds) -> Verdict {
    use NextStep::*;
    use ReasonCode as R;
    use StopReason as S;

    let (reason, next) = if inputs.executor_unavailable {
        (R::ProviderUnavailable, Retry)
    } else if let Some(stop) = inputs.stop {
        match stop {
            S::ProviderUnavailable => (R::ProviderUnavailable, Retry),
            S::ProviderRateLimited => (R::ProviderRateLimited, Retry),
            S::ModelProtocol => (R::ModelProtocolError, Retry),
            S::TimeLimit => (R::TimeLimit, Retry),
            S::ProviderQuota => (R::ProviderQuota, Retry),
            S::QuotaExceeded => (R::QuotaExceeded, Retry),
            S::WorkspacePaused => (R::WorkspacePaused, Retry),
            S::TooManyRequests => (R::TooManyRequests, Retry),
            S::ProviderAuthFailed => (R::ProviderAuthFailed, AskUser),
            S::KatPaused => (R::KatPaused, AskUser),
            S::ResidencyDenied => (R::ResidencyDenied, AskUser),
            S::NoEligibleKat => (R::NoEligibleKat, AskUser),
            S::StepLimit => (R::StepLimit, SaveDraftWithReport),
            S::TokenBudget => (R::TokenBudget, SaveDraftWithReport),
            S::OutputTruncated => (R::OutputTruncated, SaveDraftWithReport),
            S::LoopDetected => (R::LoopDetected, HandToLead),
            S::PolicyChanged => (R::PolicyChanged, RestartJob),
            S::NeedsApproval => (R::ApprovalPending, AskUser),
            S::Interrupted => (R::InterruptedReviewRequired, ReviewBeforeRetry),
            S::OutcomeUnknown => (R::OutcomeUnknown, ReviewBeforeRetry),
            S::Cancelled => (R::Cancelled, Retry),
        }
    } else if inputs.pending_question {
        (R::QuestionPending, AskUser)
    } else if inputs.has_final_text {
        return Verdict {
            status: if inputs.artifacts_created == 0 {
                PolicyStatus::Answered
            } else {
                PolicyStatus::Ready
            },
            reason: None,
            next: None,
        };
    } else {
        (R::ModelProtocolError, Retry)
    };
    Verdict {
        status: if matches!(reason, R::ApprovalPending | R::QuestionPending) {
            PolicyStatus::NeedsInput
        } else {
            PolicyStatus::Stopped
        },
        reason: Some(reason),
        next: Some(if next == Retry && inputs.effects_dispatched {
            ReviewBeforeRetry
        } else {
            next
        }),
    }
}

pub fn decision_event(
    job_id: JobId,
    facts: &RunFacts,
    v: &Verdict,
    t: &Thresholds,
) -> KatworkEvent {
    KatworkEvent::Decision(DecisionEvent {
        job_id,
        inputs: serde_json::json!(PolicyInputs::from_facts(facts)),
        verdict: serde_json::json!(v),
        thresholds: serde_json::json!(t),
        policy_version: t.version,
    })
}
