pub mod space;
pub mod workspace;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{
    error::KatError,
    ids::{UserId, WorkspaceId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

/// Membership resolved by [`resolve`]; callers cannot construct a tenant directly.
///
/// ```compile_fail
/// use katwork::{ids::{UserId, WorkspaceId}, tenancy::{Role, Tenant}};
///
/// let tenant = Tenant {
///     workspace_id: WorkspaceId::new(),
///     user_id: UserId::new(),
///     role: Role::Owner,
/// };
/// ```
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::manual_non_exhaustive,
    reason = "Membership construction must be restricted within this crate too."
)]
pub struct Tenant {
    pub workspace_id: WorkspaceId,
    pub user_id: UserId,
    pub role: Role,
    _resolved: (),
}

impl Tenant {
    pub fn require_admin(&self) -> Result<(), KatError> {
        match self.role {
            Role::Owner | Role::Admin => Ok(()),
            Role::Member => Err(KatError::Forbidden),
        }
    }
}

fn validate_name(name: &str) -> Result<&str, KatError> {
    if name.chars().any(char::is_control) {
        return Err(KatError::RequestInvalid);
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(KatError::RequestInvalid);
    }
    Ok(name)
}

pub async fn resolve(
    pool: &PgPool,
    user_id: UserId,
    workspace_id: WorkspaceId,
) -> Result<Tenant, KatError> {
    let role =
        sqlx::query_scalar("SELECT role FROM memberships WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?
            .ok_or(KatError::NotFound)?;
    Ok(Tenant {
        workspace_id,
        user_id,
        role,
        _resolved: (),
    })
}
