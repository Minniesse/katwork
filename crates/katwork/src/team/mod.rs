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
    // Validated names exclude default ignorables; compatibility folding preserves display text.
    let mut key: Vec<_> = name.nfkc().default_case_fold().nfkc().collect();
    // Thai above-vowels have combining class zero; Unicode normalization alone keeps tone-first input distinct.
    for marks in key.split_mut(|ch| !is_thai_mark(*ch)) {
        marks.sort_by_key(|ch| (if *ch == '\u{0e4d}' { '\u{0e47}' } else { *ch }, *ch));
    }
    key.dedup_by(|a, b| a == b && is_thai_mark(*a));
    key.into_iter()
        .nfc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_thai_mark(ch: char) -> bool {
    matches!(ch, '\u{0e31}' | '\u{0e34}'..='\u{0e3a}' | '\u{0e47}'..='\u{0e4e}')
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
