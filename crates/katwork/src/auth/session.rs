use std::fmt;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{AuthUser, random_token, token_hash, valid_token};
use crate::{error::KatError, ids::UserId};

pub struct SessionToken(pub String);

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken([redacted])")
    }
}

pub async fn create(pool: &PgPool, user_id: UserId) -> Result<SessionToken, KatError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    let token = create_on(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(token)
}

pub(super) async fn create_on(
    conn: &mut PgConnection,
    user_id: UserId,
) -> Result<SessionToken, KatError> {
    let token = SessionToken(random_token()?);
    sqlx::query("INSERT INTO sessions (id, user_id, token_sha256, expires_at) VALUES ($1, $2, $3, clock_timestamp() + interval '14 days')")
        .bind(Uuid::now_v7()).bind(user_id).bind(token_hash(&token.0)).execute(conn).await?;
    Ok(token)
}

pub async fn authenticate(pool: &PgPool, token: &str) -> Result<AuthUser, KatError> {
    if !valid_token(token) {
        return Err(KatError::Unauthenticated);
    }
    let user_id = sqlx::query_scalar("SELECT user_id FROM sessions WHERE token_sha256 = $1 AND revoked_at IS NULL AND expires_at > clock_timestamp()")
        .bind(token_hash(token)).fetch_optional(pool).await?.ok_or(KatError::Unauthenticated)?;
    Ok(AuthUser { user_id })
}

pub async fn revoke(pool: &PgPool, token: &str) -> Result<(), KatError> {
    if valid_token(token) {
        sqlx::query("UPDATE sessions SET revoked_at = clock_timestamp() WHERE token_sha256 = $1 AND revoked_at IS NULL")
            .bind(token_hash(token)).execute(pool).await?;
    }
    Ok(())
}
