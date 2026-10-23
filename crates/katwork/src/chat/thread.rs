use sqlx::PgPool;

use super::Thread;
use crate::{
    error::KatError,
    ids::{SpaceId, TeamId, ThreadId},
    tenancy::{self, Tenant},
};

pub async fn create(
    pool: &PgPool,
    tenant: &Tenant,
    space: SpaceId,
    team: TeamId,
    title: &str,
) -> Result<Thread, KatError> {
    if title.chars().any(char::is_control) || title.trim().chars().count() > 120 {
        return Err(KatError::ThreadTitleInvalid);
    }
    sqlx::query_as(
        "INSERT INTO threads (id, workspace_id, space_id, team_id, title)
        SELECT $1, workspace_id, space_id, id, $5 FROM teams
        WHERE workspace_id=$2 AND space_id=$3 AND id=$4
        RETURNING id, space_id, team_id, title",
    )
    .bind(ThreadId::new())
    .bind(tenant.workspace_id())
    .bind(space)
    .bind(team)
    .bind(title.trim())
    .fetch_optional(pool)
    .await?
    .ok_or(KatError::NotFound)
}

pub async fn get(pool: &PgPool, tenant: &Tenant, id: ThreadId) -> Result<Thread, KatError> {
    sqlx::query_as(
        "SELECT id, space_id, team_id, title FROM threads WHERE workspace_id=$1 AND id=$2",
    )
    .bind(tenant.workspace_id())
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(KatError::NotFound)
}

pub async fn list(
    pool: &PgPool,
    tenant: &Tenant,
    space: SpaceId,
    limit: u16,
) -> Result<Vec<Thread>, KatError> {
    tenancy::space::get(pool, tenant, space).await?;
    Ok(sqlx::query_as(include_str!("thread_list.sql"))
        .bind(tenant.workspace_id())
        .bind(space)
        .bind(i64::from(limit.clamp(1, 100)))
        .fetch_all(pool)
        .await?)
}
