use serde::Serialize;
use sqlx::PgPool;

use super::Tenant;
use crate::{error::KatError, ids::SpaceId, types::SpacePolicy};

#[derive(Debug, Serialize)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub policy: SpacePolicy,
}

pub async fn create(
    pool: &PgPool,
    tenant: &Tenant,
    name: &str,
    policy: SpacePolicy,
) -> Result<SpaceId, KatError> {
    tenant.require_admin()?;
    let name = super::validate_name(name)?;
    let id = SpaceId::new();
    sqlx::query(
        "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(tenant.workspace_id)
    .bind(name)
    .bind(policy.allow_external)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn list(pool: &PgPool, tenant: &Tenant) -> Result<Vec<Space>, KatError> {
    let rows: Vec<(SpaceId, String, bool)> = sqlx::query_as(
        "SELECT id, name, allow_external FROM spaces WHERE workspace_id = $1 ORDER BY id",
    )
    .bind(tenant.workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, allow_external)| Space {
            id,
            name,
            policy: SpacePolicy { allow_external },
        })
        .collect())
}

pub async fn get(pool: &PgPool, tenant: &Tenant, id: SpaceId) -> Result<Space, KatError> {
    let (id, name, allow_external) = sqlx::query_as(
        "SELECT id, name, allow_external FROM spaces WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tenant.workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(KatError::NotFound)?;
    Ok(Space {
        id,
        name,
        policy: SpacePolicy { allow_external },
    })
}
