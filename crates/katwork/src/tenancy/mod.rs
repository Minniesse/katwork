pub mod space;
pub mod workspace;

use serde::{Deserialize, Serialize};
use sqlx::{Executor, Postgres};
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

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
///
/// Resolved membership cannot be changed by callers:
///
/// ```compile_fail,E0616
/// use katwork::{ids::WorkspaceId, tenancy::Tenant};
/// fn change_workspace(tenant: &mut Tenant) {
///     tenant.workspace_id = WorkspaceId::new();
/// }
/// ```
///
/// ```compile_fail,E0616
/// use katwork::{ids::UserId, tenancy::Tenant};
/// fn change_user(tenant: &mut Tenant) {
///     tenant.user_id = UserId::new();
/// }
/// ```
///
/// ```compile_fail,E0616
/// use katwork::tenancy::{Role, Tenant};
/// fn change_role(tenant: &mut Tenant) {
///     tenant.role = Role::Owner;
/// }
/// ```
///
/// Read membership through accessors instead of public fields:
///
/// ```compile_fail,E0616
/// use katwork::{ids::WorkspaceId, tenancy::Tenant};
/// fn workspace(tenant: &Tenant) -> WorkspaceId {
///     tenant.workspace_id
/// }
/// ```
///
/// ```compile_fail,E0616
/// use katwork::{ids::UserId, tenancy::Tenant};
/// fn user(tenant: &Tenant) -> UserId {
///     tenant.user_id
/// }
/// ```
///
/// ```compile_fail,E0616
/// use katwork::tenancy::{Role, Tenant};
/// fn role(tenant: &Tenant) -> Role {
///     tenant.role
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Tenant {
    workspace_id: WorkspaceId,
    user_id: UserId,
    role: Role,
}

impl Tenant {
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn require_admin(&self) -> Result<(), KatError> {
        match self.role {
            Role::Owner | Role::Admin => Ok(()),
            Role::Member => Err(KatError::Forbidden),
        }
    }
}

pub(crate) fn validate_name(name: &str) -> Result<&str, KatError> {
    if name.chars().any(|ch| {
        matches!(
            ch.general_category(),
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        )
    }) {
        return Err(KatError::RequestInvalid);
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(KatError::RequestInvalid);
    }
    Ok(name)
}

pub async fn resolve<'e, E: Executor<'e, Database = Postgres>>(
    pool: E,
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
    })
}
