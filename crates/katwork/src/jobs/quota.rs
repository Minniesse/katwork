use sqlx::PgConnection;

use crate::{
    agent::StopReason,
    error::KatError,
    ids::{UserId, WorkspaceId},
    team::Kat,
};

/// Serialize admission on the Kat, then sample each window after any lock wait.
pub async fn check(
    conn: &mut PgConnection,
    kat: &Kat,
    sender: UserId,
) -> Result<Option<StopReason>, KatError> {
    // Kat is an internal, tenant-checked domain object; derive its scope before locking.
    let workspace: WorkspaceId = sqlx::query_scalar("SELECT workspace_id FROM kats WHERE id=$1")
        .bind(kat.id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    sqlx::query("SELECT id FROM kats WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(kat.id)
        .fetch_one(&mut *conn)
        .await?;
    let budget: Option<i64> =
        sqlx::query_scalar("SELECT daily_token_budget FROM kats WHERE workspace_id=$1 AND id=$2")
            .bind(workspace)
            .bind(kat.id)
            .fetch_one(&mut *conn)
            .await?;
    if let Some(budget) = budget {
        let charged: i64 = sqlx::query_scalar(include_str!("quota_daily.sql"))
            .bind(workspace)
            .bind(kat.id)
            .fetch_one(&mut *conn)
            .await?;
        if charged >= budget {
            return Ok(Some(StopReason::QuotaExceeded));
        }
    }
    let recent: i64 = sqlx::query_scalar(include_str!("quota_sender.sql"))
        .bind(workspace)
        .bind(sender)
        .bind(kat.id)
        .fetch_one(conn)
        .await?;
    Ok((recent >= 6).then_some(StopReason::TooManyRequests))
}
