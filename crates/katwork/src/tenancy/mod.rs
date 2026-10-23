pub mod space;
pub mod workspace;

use serde::{Deserialize, Serialize};
use sqlx::{Executor, Postgres};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::{
    error::KatError,
    ids::{TaskId, UserId, WorkspaceId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

/// Workspace scope resolved by [`resolve`] for requests or by persisted task authority internally.
/// Callers cannot construct a tenant directly.
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
                | GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
        )
        // Default ignorables outside Cc/Cf/Cn, plus the visually blank Braille cell.
        || matches!(ch, '\u{034f}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}'
            | '\u{ffa0}' | '\u{e0100}'..='\u{e01ef}')
    }) {
        return Err(KatError::RequestInvalid);
    }
    let name = name.trim();
    if name.chars().count() > 120
        || name
            .chars()
            .next()
            .is_some_and(|ch| ch.general_category_group() == GeneralCategoryGroup::Mark)
        || !name.chars().any(|ch| {
            ch.general_category_group() == GeneralCategoryGroup::Letter
                || ch.general_category() == GeneralCategory::DecimalNumber
        })
    {
        return Err(KatError::RequestInvalid);
    }
    let mut marks = 0;
    for ch in name.nfd() {
        if ch.general_category_group() == GeneralCategoryGroup::Mark {
            marks += 1;
            if marks > 4 {
                return Err(KatError::RequestInvalid);
            }
        } else {
            marks = 0;
        }
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

/// Internal run scope comes from persisted job/Kat authority, not current requester membership.
/// This never grants administrative rights or replaces request-time membership checks.
pub(crate) async fn for_task<'e, E: Executor<'e, Database = Postgres>>(
    conn: E,
    task: TaskId,
) -> Result<Tenant, KatError> {
    let (workspace_id, user_id) = sqlx::query_as(
        "SELECT t.workspace_id,j.requested_by FROM tasks t
         JOIN jobs j ON j.id=t.job_id AND j.workspace_id=t.workspace_id
         JOIN kats k ON k.id=t.kat_id AND k.workspace_id=t.workspace_id WHERE t.id=$1",
    )
    .bind(task)
    .fetch_optional(conn)
    .await?
    .ok_or(KatError::NotFound)?;
    Ok(Tenant {
        workspace_id,
        user_id,
        role: Role::Member,
    })
}
