use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ModelTier {
    Fast,
    Standard,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpacePolicy {
    pub allow_external: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    En,
    Th,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum KatStatus {
    Active,
    Paused,
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant),+
        }

        impl $name {
            pub fn wire(&self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
    };
}

wire_enum!(JobStatus {
    Accepted => "accepted",
    Ready => "ready",
    Answered => "answered",
    Stopped => "stopped",
    NeedsInput => "needs_input",
    Cancelled => "cancelled",
    Error => "error",
});

wire_enum!(HoldReason {
    KatBusy => "kat_busy",
    ProviderQuota => "provider_quota",
    WorkspacePaused => "workspace_paused",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutwardKind {
    Send,
    Publish,
    Pay,
    Share,
    ExternalApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffect {
    Read,
    Navigate,
    WorkspaceRead,
    Draft,
    WorkspaceWrite,
    Exec,
    CredentialUse,
    Destructive,
    Outward(OutwardKind),
}

impl SideEffect {
    pub fn from_wire(wire: &str) -> Option<Self> {
        Some(match wire {
            "read" => Self::Read,
            "navigate" => Self::Navigate,
            "workspace_read" => Self::WorkspaceRead,
            "draft" => Self::Draft,
            "workspace_write" => Self::WorkspaceWrite,
            "exec" => Self::Exec,
            "credential_use" => Self::CredentialUse,
            "destructive" => Self::Destructive,
            "outward_send" => Self::Outward(OutwardKind::Send),
            "outward_publish" => Self::Outward(OutwardKind::Publish),
            "outward_pay" => Self::Outward(OutwardKind::Pay),
            "outward_share" => Self::Outward(OutwardKind::Share),
            "outward_external_api" => Self::Outward(OutwardKind::ExternalApi),
            _ => return None,
        })
    }

    pub fn wire(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Navigate => "navigate",
            Self::WorkspaceRead => "workspace_read",
            Self::Draft => "draft",
            Self::WorkspaceWrite => "workspace_write",
            Self::Exec => "exec",
            Self::CredentialUse => "credential_use",
            Self::Destructive => "destructive",
            Self::Outward(OutwardKind::Send) => "outward_send",
            Self::Outward(OutwardKind::Publish) => "outward_publish",
            Self::Outward(OutwardKind::Pay) => "outward_pay",
            Self::Outward(OutwardKind::Share) => "outward_share",
            Self::Outward(OutwardKind::ExternalApi) => "outward_external_api",
        }
    }
}

impl Serialize for SideEffect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire())
    }
}

impl<'de> Deserialize<'de> for SideEffect {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Self::from_wire(&wire).ok_or_else(|| serde::de::Error::custom("unknown side effect"))
    }
}

wire_enum!(ReasonCode {
    ProviderUnavailable => "provider_unavailable",
    ProviderRateLimited => "provider_rate_limited",
    ProviderQuota => "provider_quota",
    ProviderAuthFailed => "provider_auth_failed",
    ModelProtocolError => "model_protocol_error",
    TimeLimit => "time_limit",
    QuotaExceeded => "quota_exceeded",
    WorkspacePaused => "workspace_paused",
    StepLimit => "step_limit",
    TokenBudget => "token_budget",
    OutputTruncated => "output_truncated",
    ResidencyDenied => "residency_denied",
    LoopDetected => "loop_detected",
    PolicyChanged => "policy_changed",
    ApprovalPending => "approval_pending",
    QuestionPending => "question_pending",
    InterruptedReviewRequired => "interrupted_review_required",
    KatPaused => "kat_paused",
    InternalError => "internal_error",
    RetriesExhausted => "retries_exhausted",
    Cancelled => "cancelled",
    OutcomeUnknown => "outcome_unknown",
    NoEligibleKat => "no_eligible_kat",
    KatNotInSpace => "kat_not_in_space",
    LeaseLost => "lease_lost",
    TooManyRequests => "too_many_requests",
    WorkspacePausedStopped => "workspace_paused_stopped",
    KatPausedStopped => "kat_paused_stopped",
    MentionNeedsSpace => "mention_needs_space",
    ExtraMentionIgnored => "extra_mention_ignored",
    ReviewBeforeRerun => "review_before_rerun",
});

impl ReasonCode {
    pub const ALL: [Self; 31] = [
        Self::ProviderUnavailable,
        Self::ProviderRateLimited,
        Self::ProviderQuota,
        Self::ProviderAuthFailed,
        Self::ModelProtocolError,
        Self::TimeLimit,
        Self::QuotaExceeded,
        Self::WorkspacePaused,
        Self::StepLimit,
        Self::TokenBudget,
        Self::OutputTruncated,
        Self::ResidencyDenied,
        Self::LoopDetected,
        Self::PolicyChanged,
        Self::ApprovalPending,
        Self::QuestionPending,
        Self::InterruptedReviewRequired,
        Self::KatPaused,
        Self::InternalError,
        Self::RetriesExhausted,
        Self::Cancelled,
        Self::OutcomeUnknown,
        Self::NoEligibleKat,
        Self::KatNotInSpace,
        Self::LeaseLost,
        Self::TooManyRequests,
        Self::WorkspacePausedStopped,
        Self::KatPausedStopped,
        Self::MentionNeedsSpace,
        Self::ExtraMentionIgnored,
        Self::ReviewBeforeRerun,
    ];
}
