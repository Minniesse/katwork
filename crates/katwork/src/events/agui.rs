use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    chat::{Author, Message, NoticeMeta},
    ids::{JobId, KatId, MessageId, ModelCallId, RunId, ToolCallRecordId},
    types::{HoldReason, JobStatus, Question, ReasonCode, SideEffect},
};

/// AG-UI 1.0 event objects. The multiplexed thread stream uses KatWork's folding rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase"
)]
pub enum AgUiEvent {
    RunStarted {
        thread_id: String,
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        protocol_version: Option<String>,
    },
    TextMessageStart {
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
    TextMessageContent {
        message_id: String,
        delta: String,
    },
    TextMessageEnd {
        message_id: String,
    },
    ToolCallStart {
        tool_call_id: String,
        tool_call_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
    },
    ToolCallArgs {
        tool_call_id: String,
        delta: String,
    },
    ToolCallEnd {
        tool_call_id: String,
    },
    ToolCallResult {
        message_id: String,
        tool_call_id: String,
        content: String,
    },
    RunFinished {
        thread_id: String,
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<RunOutcome>,
    },
    RunError {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Custom {
        name: String,
        value: Value,
    },
}

impl AgUiEvent {
    pub fn kind(&self) -> &str {
        match self {
            Self::RunStarted { .. } => "RUN_STARTED",
            Self::TextMessageStart { .. } => "TEXT_MESSAGE_START",
            Self::TextMessageContent { .. } => "TEXT_MESSAGE_CONTENT",
            Self::TextMessageEnd { .. } => "TEXT_MESSAGE_END",
            Self::ToolCallStart { .. } => "TOOL_CALL_START",
            Self::ToolCallArgs { .. } => "TOOL_CALL_ARGS",
            Self::ToolCallEnd { .. } => "TOOL_CALL_END",
            Self::ToolCallResult { .. } => "TOOL_CALL_RESULT",
            Self::RunFinished { .. } => "RUN_FINISHED",
            Self::RunError { .. } => "RUN_ERROR",
            Self::Custom { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RunOutcome {
    Success {
        #[serde(skip_serializing_if = "Option::is_none")]
        pending_tool_call_ids: Option<Vec<String>>,
    },
    Interrupt {
        interrupts: Vec<Value>,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KatState {
    Idle,
    Working,
    Researching,
    Waiting,
    NeedsApproval,
    Error,
    Paused,
}

impl KatState {
    pub fn for_tool(effect: SideEffect) -> Self {
        match effect {
            SideEffect::Read | SideEffect::WorkspaceRead | SideEffect::Navigate => {
                Self::Researching
            }
            _ => Self::Working,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEnd {
    Finished,
    Cancelled,
    Interrupted,
}

impl RunEnd {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KatStateEvent {
    pub kat_id: KatId,
    pub state: KatState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStatusEvent {
    pub job_id: JobId,
    pub status: Option<JobStatus>,
    pub reason: Option<ReasonCode>,
    pub hold_reason: Option<HoldReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEvent {
    pub job_id: JobId,
    pub inputs: Value,
    pub verdict: Value,
    pub policy_version: u32,
    pub thresholds: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatusEvent {
    pub tool_call_id: ToolCallRecordId,
    pub ok: bool,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    pub run_id: RunId,
    pub kat_id: KatId,
    pub model_call_id: ModelCallId,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStoredEvent {
    pub message_id: MessageId,
    pub author: Author,
    pub author_name: String,
    pub body: String,
    pub synthetic: bool,
    pub stopped: bool,
    pub question: Option<Question>,
    pub meta: Option<NoticeMeta>,
    pub job_id: Option<JobId>,
    pub client_msg_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&Message> for MessageStoredEvent {
    fn from(m: &Message) -> Self {
        Self {
            message_id: m.id,
            author: m.author,
            author_name: m.author_name.clone(),
            body: m.body.clone(),
            synthetic: m.synthetic,
            stopped: m.stopped,
            question: m.question.clone(),
            meta: m.meta.clone(),
            job_id: m.job_id,
            client_msg_id: m.client_msg_id.clone(),
            created_at: m.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartedEvent {
    pub run_id: RunId,
    pub job_id: JobId,
    pub kat_id: KatId,
    pub reply_message_id: MessageId,
    pub synthetic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunClosedEvent {
    pub run_id: RunId,
    pub end: RunEnd,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "name", content = "value")]
pub enum KatworkEvent {
    #[serde(rename = "katwork.kat_state")]
    KatState(KatStateEvent),
    #[serde(rename = "katwork.job_status")]
    JobStatus(JobStatusEvent),
    #[serde(rename = "katwork.decision")]
    Decision(DecisionEvent),
    #[serde(rename = "katwork.tool_status")]
    ToolStatus(ToolStatusEvent),
    #[serde(rename = "katwork.usage")]
    Usage(UsageEvent),
    #[serde(rename = "katwork.message_stored")]
    MessageStored(MessageStoredEvent),
    #[serde(rename = "katwork.run_started")]
    RunStarted(RunStartedEvent),
    #[serde(rename = "katwork.run_closed")]
    RunClosed(RunClosedEvent),
}

impl From<KatworkEvent> for AgUiEvent {
    fn from(event: KatworkEvent) -> Self {
        let mut envelope = serde_json::to_value(event).expect("event payloads are JSON values");
        Self::Custom {
            name: envelope["name"]
                .as_str()
                .expect("tagged event name")
                .to_owned(),
            value: envelope["value"].take(),
        }
    }
}
