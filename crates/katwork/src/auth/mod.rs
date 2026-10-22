pub mod mailer;
pub mod password;
pub mod reset;
pub mod session;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    config::Config,
    error::KatError,
    ids::{UserId, WorkspaceId},
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
) -> Result<(AuthUser, session::SessionToken), KatError> {
    let mut tx = pool.begin().await?;
    // Serialize password verification and session creation against password changes.
    let user: Option<(UserId, String)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE email = $1 FOR NO KEY UPDATE")
            .bind(email)
            .fetch_optional(&mut *tx)
            .await?;
    let hash = user.as_ref().map(|(_, hash)| hash.clone());
    let password = password.to_owned();
    let valid = tokio::task::spawn_blocking(move || match hash {
        Some(hash) => password::verify(&password, &hash),
        None => {
            password::verify_unknown(&password);
            false
        }
    })
    .await
    .map_err(|_| KatError::AuthFailed)?;
    if !valid {
        return Err(KatError::Unauthenticated);
    }
    let user_id = user.ok_or(KatError::Unauthenticated)?.0;
    let token = session::create_on(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok((AuthUser { user_id }, token))
}

pub async fn seed_demo(
    pool: &PgPool,
    config: &Config,
    reset_password: bool,
) -> Result<Option<String>, KatError> {
    let password = config
        .demo_password
        .clone()
        .map(Ok)
        .unwrap_or_else(random_token)?;
    let candidate = password.clone();
    let hash = tokio::task::spawn_blocking(move || password::hash(&candidate))
        .await
        .map_err(|_| KatError::AuthFailed)??;
    let mut tx = pool.begin().await?;
    // A transaction-scoped lock makes concurrent seed commands idempotent before a user exists.
    sqlx::query("SELECT pg_advisory_xact_lock(3000003)")
        .execute(&mut *tx)
        .await?;
    let existing: Option<UserId> = sqlx::query_scalar(
        "SELECT id FROM users WHERE email = 'demo@katwork.local' FOR NO KEY UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let user_id = existing.unwrap_or_default();
    if existing.is_none() {
        sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'demo@katwork.local', $2, 'Demo', 'en')")
            .bind(user_id).bind(&hash).execute(&mut *tx).await?;
    } else if reset_password {
        sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
            .bind(user_id)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        reset::revoke_credentials(&mut tx, user_id).await?;
    }
    let workspace: Option<WorkspaceId> = sqlx::query_scalar("SELECT w.id FROM workspaces w JOIN memberships m ON m.workspace_id = w.id WHERE m.user_id = $1 AND w.name = 'Demo' ORDER BY w.id LIMIT 1")
        .bind(user_id).fetch_optional(&mut *tx).await?;
    if workspace.is_none() {
        let workspace_id = WorkspaceId::new();
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Demo')")
            .bind(workspace_id)
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
    tx.commit().await?;
    Ok(
        (reset_password || (existing.is_none() && config.demo_password.is_none()))
            .then_some(password),
    )
}
