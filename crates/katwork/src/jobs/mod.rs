pub mod job;
pub mod lease;
pub mod quota;
pub mod task;
pub mod worker;

use serde::{Deserialize, Serialize};

use crate::{
    agent::Initiator,
    ids::{JobId, KatId, MessageId, ThreadId, UserId},
    types::{JobStatus, ReasonCode},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessEnd {
    Cancelled,
    Error { code: ReasonCode },
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobSummary {
    pub id: JobId,
    #[sqlx(json(nullable))]
    pub status: Option<JobStatus>,
    pub kat_id: Option<KatId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRule {
    Mention,
    FollowUp,
    Lead,
    FirstEligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    pub rule: RouteRule,
    pub ignored: Vec<KatId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInput {
    pub thread_id: ThreadId,
    pub request_message_id: MessageId,
    pub routing: Routing,
    pub actor: UserId,
    pub initiator: Initiator,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobView {
    pub id: JobId,
    #[sqlx(json(nullable))]
    pub status: Option<JobStatus>,
    #[sqlx(json(nullable))]
    pub reason: Option<ReasonCode>,
    #[sqlx(json(nullable))]
    pub hold_reason: Option<crate::types::HoldReason>,
    #[sqlx(json(nullable))]
    pub queue_position: Option<u32>,
}
