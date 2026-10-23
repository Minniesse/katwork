use async_trait::async_trait;
use sqlx::PgPool;
use std::time::Duration;

use super::{AgUiEvent, EventHub, KatworkEvent, RunClosedEvent, RunEnd, ToolStatusEvent, log};
use crate::{
    db,
    error::KatError,
    fence::{self, Fence},
    ids::{EventSeq, MessageId, RunId, ThreadId, ToolCallRecordId, WorkspaceId},
};

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError>;
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError>;
}

pub struct PgEventSink {
    pool: PgPool,
    hub: EventHub,
    lock_timeout: Duration,
    workspace_id: WorkspaceId,
    thread_id: ThreadId,
    run_id: Option<RunId>,
    fence: Option<Fence>,
}

impl PgEventSink {
    pub fn new(
        pool: PgPool,
        hub: EventHub,
        lock_timeout: Duration,
        workspace_id: WorkspaceId,
        thread_id: ThreadId,
        run_id: Option<RunId>,
        fence: Option<Fence>,
    ) -> Self {
        Self {
            pool,
            hub,
            lock_timeout,
            workspace_id,
            thread_id,
            run_id,
            fence,
        }
    }
}

#[async_trait]
impl EventSink for PgEventSink {
    async fn emit(&self, mut event: AgUiEvent) -> Result<EventSeq, KatError> {
        if let AgUiEvent::RunError { message, code } = &mut event {
            *message = code.as_ref().ok_or(KatError::RequestInvalid)?.clone();
        }
        let mut tx = self.pool.begin().await?;
        db::set_lock_timeout(&mut tx, self.lock_timeout).await?;
        if let Some(fence) = &self.fence {
            fence::check(&mut tx, fence).await?;
        }
        let seq = log::append(
            &mut tx,
            self.workspace_id,
            self.thread_id,
            self.run_id,
            &event,
        )
        .await?;
        tx.commit().await?;
        self.hub.notify(self.thread_id, seq);
        Ok(seq)
    }

    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        let run = self.run_id.ok_or(KatError::RequestInvalid)?;
        let fence = self.fence.as_ref().ok_or(KatError::RequestInvalid)?;
        let mut tx = self.pool.begin().await?;
        db::set_lock_timeout(&mut tx, self.lock_timeout).await?;
        fence::check(&mut tx, fence).await?;
        sqlx::query(
            "SELECT id FROM runs WHERE workspace_id=$1 AND id=$2 AND task_id=$3 FOR NO KEY UPDATE",
        )
        .bind(self.workspace_id)
        .bind(run)
        .bind(fence.task_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KatError::NotFound)?;
        let running: bool = sqlx::query_scalar(
            "SELECT state='running' AND ended_at IS NULL FROM runs WHERE workspace_id=$1 AND id=$2",
        )
        .bind(self.workspace_id)
        .bind(run)
        .fetch_one(&mut *tx)
        .await?;
        if !running {
            return Err(KatError::LeaseLost);
        }
        let calls: Vec<ToolCallRecordId> = sqlx::query_scalar("SELECT id FROM tool_calls WHERE workspace_id=$1 AND run_id=$2 AND ended_at IS NULL ORDER BY id FOR NO KEY UPDATE")
            .bind(self.workspace_id).bind(run).fetch_all(&mut *tx).await?;
        sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
            .bind(self.workspace_id)
            .bind(self.thread_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(KatError::NotFound)?;
        let code = code.replace('\0', "\u{fffd}");
        for call in calls {
            log::append(
                &mut tx,
                self.workspace_id,
                self.thread_id,
                Some(run),
                &AgUiEvent::ToolCallResult {
                    message_id: MessageId::new().to_string(),
                    tool_call_id: call.to_string(),
                    content: code.clone(),
                },
            )
            .await?;
            log::append(
                &mut tx,
                self.workspace_id,
                self.thread_id,
                Some(run),
                &KatworkEvent::ToolStatus(ToolStatusEvent {
                    tool_call_id: call,
                    ok: false,
                    code: Some(code.clone()),
                })
                .into(),
            )
            .await?;
            sqlx::query("UPDATE tool_calls SET state='outcome_unknown', result=$3, ended_at=now() WHERE workspace_id=$1 AND id=$2")
                .bind(self.workspace_id).bind(call).bind(&code).execute(&mut *tx).await?;
        }
        log::append(
            &mut tx,
            self.workspace_id,
            self.thread_id,
            Some(run),
            &KatworkEvent::RunClosed(RunClosedEvent {
                run_id: run,
                end,
                code: Some(code.clone()),
            })
            .into(),
        )
        .await?;
        let seq = log::append(
            &mut tx,
            self.workspace_id,
            self.thread_id,
            Some(run),
            &AgUiEvent::RunError {
                message: code.clone(),
                code: Some(code),
            },
        )
        .await?;
        sqlx::query("UPDATE runs SET state=$3, ended_at=now() WHERE workspace_id=$1 AND id=$2")
            .bind(self.workspace_id)
            .bind(run)
            .bind(end.wire())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.hub.notify(self.thread_id, seq);
        Ok(seq)
    }
}
