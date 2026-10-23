use super::Initiator;
use crate::{fence::Fence, ids::*, tenancy::Tenant};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ComputerHandle;
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub lock_timeout: std::time::Duration,
    pub tenant: Tenant,
    pub thread_id: ThreadId,
    pub space_id: SpaceId,
    pub kat_id: KatId,
    pub job_id: JobId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub step: u32,
    pub tool_call_id: String,
    pub idempotency_key: String,
    pub lease_epoch: LeaseEpoch,
    pub policy_version: i64,
    pub actor: UserId,
    pub initiator: Initiator,
    pub computer: Option<ComputerHandle>,
    pub pool: PgPool,
    pub cancel: CancellationToken,
}
impl ToolCtx {
    pub fn fence(&self) -> Fence {
        Fence {
            task_id: self.task_id,
            epoch: self.lease_epoch,
        }
    }
}
