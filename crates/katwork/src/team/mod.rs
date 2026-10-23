pub mod dot;
pub mod roles;
#[expect(clippy::module_inception)]
pub mod team;

pub use dot::{Appearance, Autonomy, Dot, DotPatch, DotRole, HireDot, NewDot};
pub use team::{NewTeam, Team, TeamView};

use caseless::Caseless;
use sqlx::PgConnection;
use unicode_normalization::UnicodeNormalization;

use crate::{error::KatError, tenancy::Tenant};

fn name_key(name: &str) -> String {
    name.nfc().default_case_fold().nfc().collect()
}

pub(crate) async fn lock_workspace(
    conn: &mut PgConnection,
    tenant: &Tenant,
) -> Result<(), KatError> {
    // Serialize roster edits, including renames racing membership changes, before any Dot lock.
    sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR NO KEY UPDATE")
        .bind(tenant.workspace_id())
        .fetch_optional(conn)
        .await?
        .ok_or(KatError::NotFound)?;
    Ok(())
}

async fn check_space(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space: crate::ids::SpaceId,
) -> Result<(), KatError> {
    sqlx::query("SELECT id FROM spaces WHERE workspace_id = $1 AND id = $2")
        .bind(tenant.workspace_id())
        .bind(space)
        .fetch_optional(conn)
        .await?
        .ok_or(KatError::NotFound)?;
    Ok(())
}
