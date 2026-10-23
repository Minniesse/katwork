pub mod mentions;
pub mod message;
pub mod thread;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    ids::{EventSeq, JobId, KatId, MessageId, SpaceId, TeamId, ThreadId, UserId},
    types::{JobStatus, Question, ReasonCode},
};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Thread {
    pub id: ThreadId,
    pub space_id: SpaceId,
    pub team_id: TeamId,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Author {
    User(UserId),
    Kat(KatId),
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoticeKat {
    pub id: KatId,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoticeMeta {
    pub code: ReasonCode,
    pub kats: Vec<NoticeKat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: MessageId,
    pub thread_id: ThreadId,
    #[sqlx(json)]
    pub author: Author,
    pub author_name: String,
    pub body: String,
    pub synthetic: bool,
    pub stopped: bool,
    #[sqlx(json(nullable))]
    pub question: Option<Question>,
    #[sqlx(json(nullable))]
    pub meta: Option<NoticeMeta>,
    pub job_id: Option<JobId>,
    #[sqlx(json(nullable))]
    pub job_status: Option<JobStatus>,
    #[sqlx(json(nullable))]
    pub job_reason: Option<ReasonCode>,
    pub client_msg_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewMessage {
    pub id: Option<MessageId>,
    pub author: Author,
    pub body: String,
    pub client_msg_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub synthetic: bool,
    pub stopped: bool,
    pub question: Option<Question>,
    pub meta: Option<NoticeMeta>,
    pub job_id: Option<JobId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserPost {
    pub body: String,
    pub client_msg_id: Option<String>,
}

#[derive(Debug)]
pub struct Appended {
    pub message: Message,
    pub created: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    pub event_seq: EventSeq,
}
