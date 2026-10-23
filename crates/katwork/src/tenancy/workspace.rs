use serde::Serialize;
use sqlx::{Connection, PgPool};

use super::{Role, Tenant};
use crate::{
    error::KatError,
    ids::{UserId, WorkspaceId},
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub role: Role,
    pub paused: bool,
}

pub async fn create(
    pool: &PgPool,
    name: &str,
    owner: UserId,
    lock_timeout: std::time::Duration,
) -> Result<WorkspaceId, KatError> {
    let name = super::validate_name(name)?;
    let id = WorkspaceId::new();
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)")
            .bind(id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
        )
        .bind(id)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    })
    .await
}

pub async fn list_for(pool: &PgPool, user_id: UserId) -> Result<Vec<WorkspaceSummary>, KatError> {
    Ok(sqlx::query_as("SELECT w.id, w.name, m.role, w.paused FROM workspaces w JOIN memberships m ON m.workspace_id = w.id WHERE m.user_id = $1 ORDER BY w.id")
        .bind(user_id).fetch_all(pool).await?)
}

pub async fn set_paused(
    pool: &PgPool,
    tenant: &Tenant,
    paused: bool,
    lock_timeout: std::time::Duration,
) -> Result<(), KatError> {
    tenant.require_admin()?;
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        // A non-key update locks only this workspace and permits concurrent foreign-key checks.
        let result = sqlx::query("UPDATE workspaces SET paused = $2 WHERE id = $1")
            .bind(tenant.workspace_id())
            .bind(paused)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KatError::NotFound);
        }
        tx.commit().await?;
        Ok(())
    })
    .await
}
