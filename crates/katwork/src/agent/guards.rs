use super::{StopReason, ToolCtx};
use crate::{
    error::KatError,
    fence::{self, Fence},
    ids::{KatId, SpaceId},
    team::kat,
    tenancy::Tenant,
};
use serde_json::Value;
use sqlx::PgConnection;
use std::collections::VecDeque;

pub async fn authorize(
    conn: &mut PgConnection,
    tenant: &Tenant,
    fence: &Fence,
    kat_id: KatId,
    space_id: SpaceId,
    policy_version: i64,
    lock_timeout: std::time::Duration,
) -> Result<Option<StopReason>, KatError> {
    crate::db::set_lock_timeout(conn, lock_timeout).await?;
    scoped_task(conn, tenant, fence).await?;
    sqlx::query("SELECT id FROM workspaces WHERE id=$1 FOR SHARE")
        .bind(tenant.workspace_id())
        .fetch_one(&mut *conn)
        .await?;
    fence::check(conn, fence).await?;
    sqlx::query("SELECT id FROM kats WHERE workspace_id=$1 AND id=$2 FOR SHARE")
        .bind(tenant.workspace_id())
        .bind(kat_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    // The task share lock is still held, but the lease can expire during the Kat lock wait.
    fence::check(conn, fence).await?;
    policy(conn, tenant, kat_id, space_id, policy_version).await
}
pub(crate) async fn scoped_task(
    conn: &mut PgConnection,
    tenant: &Tenant,
    fence: &Fence,
) -> Result<(), KatError> {
    sqlx::query("SELECT id FROM tasks WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id())
        .bind(fence.task_id)
        .fetch_optional(conn)
        .await?
        .ok_or(KatError::NotFound)?;
    Ok(())
}
/// Call first inside a write transaction; workspace, task and Kat share locks remain until commit.
/// Tools needing intervening row classes require a lock-order-aware guard before they can ship.
pub async fn authorize_write(
    conn: &mut PgConnection,
    ctx: &ToolCtx,
) -> Result<Option<StopReason>, KatError> {
    authorize(
        conn,
        &ctx.tenant,
        &ctx.fence(),
        ctx.kat_id,
        ctx.space_id,
        ctx.policy_version,
        ctx.lock_timeout,
    )
    .await
}
async fn policy(
    conn: &mut PgConnection,
    tenant: &Tenant,
    kat_id: KatId,
    space_id: SpaceId,
    version: i64,
) -> Result<Option<StopReason>, KatError> {
    let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id=$1")
        .bind(tenant.workspace_id())
        .fetch_one(&mut *conn)
        .await?;
    if paused {
        return Ok(Some(StopReason::WorkspacePaused));
    }
    if !kat::is_granted(conn, tenant, kat_id, space_id).await? {
        return Ok(Some(StopReason::PolicyChanged));
    }
    if !kat::is_eligible(conn, tenant, kat_id, space_id).await? {
        return Ok(Some(StopReason::KatPaused));
    }
    if kat::get_on(conn, tenant, kat_id).await?.policy_version != version {
        return Ok(Some(StopReason::PolicyChanged));
    }
    Ok(None)
}
pub fn canonical_args(args: &Value) -> String {
    let mut args = args.clone();
    args.sort_all_objects();
    args.to_string()
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatKey {
    pub tool_name: String,
    pub canonical_args: String,
    pub target_revision: Option<String>,
}
pub struct RepeatGuard {
    limit: u32,
    window: (u32, u32),
    recent: VecDeque<RepeatKey>,
    last: Option<RepeatKey>,
    consecutive: u32,
}
impl RepeatGuard {
    pub fn new(limit: u32, window: (u32, u32)) -> Self {
        Self {
            limit,
            window,
            recent: VecDeque::new(),
            last: None,
            consecutive: 0,
        }
    }
    pub fn check(&mut self, key: &RepeatKey) -> bool {
        self.consecutive = if self.last.as_ref() == Some(key) {
            self.consecutive.saturating_add(1)
        } else {
            1
        };
        self.last = Some(key.clone());
        self.recent.push_back(key.clone());
        while self.recent.len() > self.window.1 as usize {
            self.recent.pop_front();
        }
        self.consecutive < self.limit
            && self.recent.iter().filter(|k| *k == key).count() < (self.window.0 as usize)
    }
}
pub fn truncate_result(text: &str, max_scalars: usize) -> String {
    if text.chars().count() <= max_scalars {
        return text.to_owned();
    }
    let marker = "…[truncated]";
    let prefix = if serde_json::from_str::<Value>(text).is_ok() {
        "[JSON truncated] "
    } else {
        ""
    };
    let marker_len = marker.chars().count();
    if max_scalars <= marker_len {
        return marker.chars().take(max_scalars).collect();
    }
    prefix
        .chars()
        .chain(text.chars())
        .take(max_scalars - marker_len)
        .chain(marker.chars())
        .collect()
}
