use super::{Initiator, RunBudget};
use crate::{
    artifacts::{BlobStore, tools::ExtractCache},
    docs::{DocError, DocLimits, DocPool, Extracted, MediaKind},
    fence::Fence,
    ids::*,
    runtime::Core,
    tenancy::Tenant,
};
use sqlx::PgPool;
use std::sync::Arc;
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
    /// The loop's dispatch deadline for this attempt: `min(now + tool_timeout, run deadline)`.
    pub deadline: tokio::time::Instant,
    pub budget: RunBudget,
}
impl ToolCtx {
    pub fn fence(&self) -> Fence {
        Fence {
            task_id: self.task_id,
            epoch: self.lease_epoch,
        }
    }
}

/// A document reader: `docs::extract` in the product, a fake in tests.
pub type ExtractFn =
    Arc<dyn Fn(MediaKind, &[u8], &DocLimits) -> Result<Extracted, DocError> + Send + Sync>;

/// What the document tools need beyond `ToolCtx`.
#[derive(Clone)]
pub struct ToolDeps {
    pub docs: Arc<DocPool>,
    pub blobs: Arc<dyn BlobStore>,
    pub extract: ExtractFn,
    pub cache: Arc<ExtractCache>,
}
impl ToolDeps {
    pub fn from_core(core: &Core) -> ToolDeps {
        ToolDeps {
            docs: core.docs.clone(),
            blobs: core.blobs.clone(),
            extract: Arc::new(crate::docs::extract),
            cache: core.extract_cache.clone(),
        }
    }
}
