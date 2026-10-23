pub mod kat;
pub mod roles;
#[expect(clippy::module_inception)]
pub mod team;

pub use kat::{Appearance, Autonomy, HireKat, Kat, KatPatch, KatRole, NewKat};
pub use team::{NewTeam, Team, TeamView};

use caseless::Caseless;
use sqlx::PgConnection;
use unicode_normalization::UnicodeNormalization;

use crate::{error::KatError, tenancy::Tenant};

pub(crate) fn name_key(name: &str) -> String {
    fold_name(name)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// Preserve edge whitespace so incremental mention matching can collapse it across chunks.
pub(crate) fn fold_name(name: &str) -> String {
    fold_name_checked(name).0
}

fn fold_name_checked(name: &str) -> (String, bool) {
    let mut key = name.to_owned();
    // NFC can expose another Thai run; bound work and report whether a fixed point was confirmed.
    for _ in 0..4 {
        let next = fold_name_once(&key);
        if next == key {
            return (next, true);
        }
        key = next;
    }
    (key, false)
}

fn fold_name_once(name: &str) -> String {
    // Validated names exclude default ignorables; compatibility folding preserves display text.
    let mut key: Vec<_> = name.nfkc().default_case_fold().nfkc().collect();
    // Thai above-vowels have combining class zero; Unicode normalization alone keeps tone-first input distinct.
    for marks in key.split_mut(|ch| !is_thai_mark(*ch)) {
        marks.sort_by_key(|ch| (if *ch == '\u{0e4d}' { '\u{0e47}' } else { *ch }, *ch));
    }
    let mut key: Vec<_> = key.into_iter().nfc().collect();
    key.dedup_by(|a, b| a == b && is_thai_mark(*a));
    key.into_iter().collect()
}

fn is_thai_mark(ch: char) -> bool {
    matches!(ch, '\u{0e31}' | '\u{0e34}'..='\u{0e3a}' | '\u{0e47}'..='\u{0e4e}')
}

pub(crate) async fn lock_workspace(
    conn: &mut PgConnection,
    tenant: &Tenant,
) -> Result<(), KatError> {
    // Serialize roster edits, including renames racing membership changes, before any Kat lock.
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

fn validate_name(name: &str) -> Result<&str, KatError> {
    let name = crate::tenancy::validate_name(name)?;
    let (key, converged) = fold_name_checked(name);
    if !converged || key.contains('@') {
        return Err(KatError::RequestInvalid);
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::{fold_name, roles::default_roster, team, validate_name};
    use crate::types::Locale;

    #[test]
    fn default_names_pass_validate_name_and_fold_to_fixed_points() {
        for locale in [Locale::En, Locale::Th] {
            let mut names: Vec<_> = default_roster(locale)
                .into_iter()
                .map(|new| new.name)
                .collect();
            names.push(team::default_name(locale).to_owned());
            for name in &names {
                assert_eq!(validate_name(name).ok(), Some(name.as_str()), "{name:?}");
                let key = fold_name(name);
                assert_eq!(fold_name(&key), key, "{name:?}");
                assert!(!key.contains('@'), "{name:?}");
                if locale == Locale::Th {
                    assert_eq!(key, *name, "{name:?}");
                }
            }
        }
        for (workspace, space, _) in crate::auth::DEMO_WORKSPACES {
            for name in [workspace, space] {
                assert_eq!(crate::tenancy::validate_name(name).ok(), Some(name));
            }
        }
    }

    #[test]
    fn fold_name_is_idempotent_with_mixed_thai_marks() {
        let name = "ß\u{0e38}\u{0308}\u{0e4e}\u{0e38}";
        let folded = fold_name(name);
        assert_eq!(fold_name(&folded), folded);
    }

    #[test]
    fn fold_name_is_idempotent_with_class_zero_thai_marks() {
        let name = "ก\u{0e38}\u{0e31}\u{3099}\u{0e31}";
        assert!(validate_name(name).is_ok());
        let folded = fold_name(name);
        assert_eq!(fold_name(&folded), folded);
        assert_eq!(folded, "ก\u{0e31}\u{3099}\u{0e31}\u{0e38}");
    }

    #[test]
    fn fold_name_is_idempotent_with_interleaved_voicing_mark() {
        let name = "\u{0e38}\u{0e34}\u{3099}\u{0e34}";
        let folded = fold_name(name);
        assert_eq!(fold_name(&folded), folded);
        assert_eq!(folded, "\u{0e34}\u{3099}\u{0e34}\u{0e38}");
    }

    #[test]
    fn names_reject_folds_that_exhaust_the_pass_limit() {
        // Halfwidth voicing marks are letters before NFKC, so ordinary name validation accepts this chain.
        let name = format!("ก\u{0e38}{}\u{0e31}", "\u{0e31}\u{ff9e}".repeat(5));
        assert!(crate::tenancy::validate_name(&name).is_ok());
        assert!(matches!(
            validate_name(&name),
            Err(crate::error::KatError::RequestInvalid)
        ));
    }

    #[test]
    fn fold_name_is_idempotent_for_all_three_mark_combinations() {
        assert_mark_combinations_are_idempotent(3);
    }

    #[test]
    #[ignore = "exhaustive four-mark probe takes more than 3 seconds in a debug build"]
    fn fold_name_is_idempotent_for_all_four_mark_combinations() {
        assert_mark_combinations_are_idempotent(4);
    }

    fn assert_mark_combinations_are_idempotent(count: u32) {
        let marks: Vec<_> = "\u{0e31}\u{0e34}\u{0e35}\u{0e36}\u{0e37}\u{0e38}\u{0e39}\u{0e3a}\u{0e47}\u{0e48}\u{0e49}\u{0e4a}\u{0e4b}\u{0e4c}\u{0e4d}\u{0e4e}\u{0300}\u{0308}\u{0315}\u{0323}\u{0345}\u{3099}".chars().collect();
        for mut index in 0..marks.len().pow(count) {
            let mut name = String::from("ก");
            for _ in 0..count {
                name.push(marks[index % marks.len()]);
                index /= marks.len();
            }
            assert!(validate_name(&name).is_ok(), "{name:?}");
            let folded = fold_name(&name);
            assert_eq!(fold_name(&folded), folded, "{name:?}");
        }
    }
}
