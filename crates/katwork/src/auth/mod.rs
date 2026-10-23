pub mod accounts;
pub mod invite;
pub mod mailer;
pub mod password;
pub mod reset;
pub mod session;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgPool};

use crate::{
    config::Config,
    error::KatError,
    ids::{SpaceId, UserId, WorkspaceId},
    types::Locale,
};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct AuthUser {
    pub user_id: UserId,
}

fn random_token() -> Result<String, KatError> {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| KatError::AuthFailed)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && URL_SAFE_NO_PAD
            .decode(token)
            .is_ok_and(|bytes| bytes.len() == 32)
}

pub async fn login(
    pool: &PgPool,
    email: &str,
    password: &str,
    lock_timeout: std::time::Duration,
) -> Result<(AuthUser, session::SessionToken), KatError> {
    let user: Option<(UserId, String)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(pool)
            .await?;
    let hash = user.as_ref().map(|(_, hash)| hash.clone());
    let password = password.to_owned();
    let valid = password::run(move || match hash {
        Some(hash) => password::verify(&password, &hash),
        None => {
            password::verify_unknown(&password);
            false
        }
    })
    .await?;
    if !valid {
        return Err(KatError::Unauthenticated);
    }
    let (user_id, verified_hash) = user.ok_or(KatError::Unauthenticated)?;
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        // Recheck under the user lock so a concurrent reset cannot issue a session for the old password.
        let current_hash: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 FOR NO KEY UPDATE")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?;
        if current_hash.as_deref() != Some(&verified_hash) {
            return Err(KatError::Unauthenticated);
        }
        let token = session::create_on(&mut tx, user_id).await?;
        tx.commit().await?;
        Ok((AuthUser { user_id }, token))
    })
    .await
}

/// The demo user's workspaces, each with one Space that allows hosted providers and that
/// Space's default team in the workspace's language.
pub(crate) const DEMO_WORKSPACES: [(&str, &str, Locale); 2] = [
    ("Demo", "Demo", Locale::En),
    ("เดโม", "พื้นที่ทดลอง", Locale::Th),
];

pub async fn seed_demo(
    pool: &PgPool,
    config: &Config,
    reset_password: bool,
) -> Result<Option<String>, KatError> {
    if !config.demo {
        return Err(KatError::ConfigInvalid {
            var: "KATWORK_DEMO",
        });
    }
    let password = config
        .demo_password
        .clone()
        .map(Ok)
        .unwrap_or_else(random_token)?;
    let candidate = password.clone();
    let hash = password::run(move || password::hash(&candidate)).await??;
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, config.lock_timeout).await?;
        // A transaction-scoped lock makes concurrent seed commands idempotent before a user exists.
        sqlx::query("SELECT pg_advisory_xact_lock(3000003)")
            .execute(&mut *tx)
            .await?;
        let existing: Option<(UserId, String)> = sqlx::query_as(
            "SELECT id, password_hash FROM users WHERE email = 'demo@katwork.local' FOR NO KEY UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let user_id = existing.as_ref().map(|(id, _)| *id).unwrap_or_default();
        let changed = match (&existing, &config.demo_password) {
            (Some(_), _) if reset_password => true,
            // The operator changed KATWORK_DEMO_PASSWORD: apply it, as a reset would.
            (Some((_, stored)), Some(chosen)) => {
                let (chosen, stored) = (chosen.clone(), stored.clone());
                !password::run(move || password::verify(&chosen, &stored)).await?
            }
            _ => false,
        };
        if existing.is_none() {
            sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'demo@katwork.local', $2, 'Demo', 'en')")
                .bind(user_id).bind(&hash).execute(&mut *tx).await?;
        } else if changed {
            sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
                .bind(user_id)
                .bind(&hash)
                .execute(&mut *tx)
                .await?;
            reset::revoke_credentials(&mut tx, user_id).await?;
        }
        let mut seeded = Vec::new();
        for (name, space, locale) in DEMO_WORKSPACES {
            let workspace: Option<WorkspaceId> = sqlx::query_scalar("SELECT w.id FROM workspaces w JOIN memberships m ON m.workspace_id = w.id WHERE m.user_id = $1 AND w.name = $2 ORDER BY w.id LIMIT 1")
                .bind(user_id).bind(name).fetch_optional(&mut *tx).await?;
            let workspace_id = workspace.unwrap_or_default();
            if workspace.is_none() {
                sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)")
                    .bind(workspace_id)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
                )
                .bind(workspace_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) SELECT $1, $2, $3, true WHERE NOT EXISTS (SELECT 1 FROM spaces WHERE workspace_id = $2 AND name = $3)")
                .bind(SpaceId::new()).bind(workspace_id).bind(space).execute(&mut *tx).await?;
            seeded.push((workspace_id, space, locale));
        }
        // Lock order: both workspace rows, in ascending id, before either roster is written.
        let mut locked: Vec<_> = seeded.iter().map(|(id, ..)| id.0).collect();
        locked.sort();
        for id in locked {
            sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR NO KEY UPDATE")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        for (workspace_id, space, locale) in seeded {
            let tenant = crate::tenancy::resolve(&mut *tx, user_id, workspace_id).await?;
            let space_id = sqlx::query_scalar(
                "SELECT id FROM spaces WHERE workspace_id=$1 AND name=$2 ORDER BY id LIMIT 1",
            )
            .bind(workspace_id)
            .bind(space)
            .fetch_one(&mut *tx)
            .await?;
            crate::team::team::create_default_on(&mut tx, &tenant, space_id, locale).await?;
        }
        tx.commit().await?;
        Ok(
            (reset_password || (existing.is_none() && config.demo_password.is_none()))
                .then_some(password),
        )
    })
    .await
}
