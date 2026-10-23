use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use super::{
    mailer::{self, Mailer},
    password, random_token, token_hash, valid_token,
};
use crate::{error::KatError, ids::UserId, types::Locale};

/// The two kinds of set-password token. They share the format and the single-use and
/// session rules, and each route accepts only its own kind.
#[derive(Clone, Copy)]
pub(super) enum TokenKind {
    Reset,
    Invite,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Invite => "invite",
        }
    }

    fn lifetime_minutes(self) -> i32 {
        match self {
            Self::Reset => 30,
            Self::Invite => 72 * 60,
        }
    }

    fn invalid(self) -> KatError {
        match self {
            Self::Reset => KatError::ResetTokenInvalid,
            Self::Invite => KatError::InviteTokenInvalid,
        }
    }
}

pub async fn request(
    pool: &PgPool,
    mailer: &dyn Mailer,
    email: &str,
    link_base: &str,
    lock_timeout: std::time::Duration,
) -> Result<(), KatError> {
    let mail = crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        let user: Option<(UserId, String, String)> =
            sqlx::query_as("SELECT id, email, locale FROM users WHERE email = $1 FOR NO KEY UPDATE")
                .bind(email.trim().to_lowercase())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((user_id, email, locale)) = user else {
            return Ok(None);
        };
        let outstanding: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE user_id = $1 AND kind = 'reset' AND used_at IS NULL AND expires_at > clock_timestamp()")
            .bind(user_id).fetch_one(&mut *tx).await?;
        if outstanding >= 3 {
            return Ok(None);
        }
        let token = issue(&mut tx, user_id).await?;
        tx.commit().await?;
        let locale = if locale == "th" {
            Locale::Th
        } else {
            Locale::En
        };
        Ok(Some(mailer::reset_mail(email, locale, &link(link_base, &token))))
    })
    .await?;
    let Some(mail) = mail else {
        return Ok(());
    };
    // Delivery failures must not reveal whether the account exists.
    let _ = mailer.send(&mail).await;
    Ok(())
}

/// Inserts a reset token for the account and returns the raw token, which is never stored.
pub async fn issue(conn: &mut PgConnection, user_id: UserId) -> Result<String, KatError> {
    issue_kind(conn, user_id, TokenKind::Reset).await
}

pub fn link(link_base: &str, token: &str) -> String {
    format!("{}/reset?token={token}", link_base.trim_end_matches('/'))
}

/// Sets the password from an unused, unexpired reset token and returns the account's email.
pub async fn confirm(
    pool: &PgPool,
    token: &str,
    new_password: &str,
    lock_timeout: std::time::Duration,
) -> Result<String, KatError> {
    consume(pool, TokenKind::Reset, token, new_password, lock_timeout).await
}

pub(super) async fn issue_kind(
    conn: &mut PgConnection,
    user_id: UserId,
    kind: TokenKind,
) -> Result<String, KatError> {
    let token = random_token()?;
    sqlx::query("INSERT INTO password_resets (id, user_id, token_sha256, kind, expires_at) VALUES ($1, $2, $3, $4, clock_timestamp() + make_interval(mins => $5))")
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(token_hash(&token))
        .bind(kind.as_str())
        .bind(kind.lifetime_minutes())
        .execute(conn)
        .await?;
    Ok(token)
}

pub(super) async fn consume(
    pool: &PgPool,
    kind: TokenKind,
    token: &str,
    new_password: &str,
    lock_timeout: std::time::Duration,
) -> Result<String, KatError> {
    if !valid_token(token) {
        return Err(kind.invalid());
    }
    let digest = token_hash(token);
    // An unlocked look first, so a wrong token never costs an Argon2 hash.
    let user_id: UserId = sqlx::query_scalar("SELECT user_id FROM password_resets WHERE token_sha256 = $1 AND kind = $2 AND used_at IS NULL AND expires_at > clock_timestamp()")
        .bind(&digest).bind(kind.as_str()).fetch_optional(pool).await?.ok_or(kind.invalid())?;
    let new_password = new_password.to_owned();
    let hash = password::run(move || password::hash(&new_password)).await??;
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        let email: String =
            sqlx::query_scalar("SELECT email FROM users WHERE id = $1 FOR NO KEY UPDATE")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
        // Lock the token row, then check it in a separate statement, so expiry is judged after any wait.
        sqlx::query("SELECT id FROM password_resets WHERE token_sha256 = $1 FOR NO KEY UPDATE")
            .bind(&digest)
            .fetch_one(&mut *tx)
            .await?;
        let consumed: Option<UserId> = sqlx::query_scalar("UPDATE password_resets SET used_at = clock_timestamp() WHERE token_sha256 = $1 AND kind = $2 AND used_at IS NULL AND expires_at > clock_timestamp() RETURNING user_id")
            .bind(&digest).bind(kind.as_str()).fetch_optional(&mut *tx).await?;
        if consumed != Some(user_id) {
            return Err(kind.invalid());
        }
        sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
            .bind(user_id)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        revoke_credentials(&mut tx, user_id).await?;
        tx.commit().await?;
        Ok(email)
    })
    .await
}

/// Ends the account's sessions and its pending tokens of both kinds.
pub(super) async fn revoke_credentials(
    conn: &mut PgConnection,
    user_id: UserId,
) -> Result<(), KatError> {
    sqlx::query("UPDATE password_resets SET used_at = clock_timestamp() WHERE user_id = $1 AND used_at IS NULL")
        .bind(user_id).execute(&mut *conn).await?;
    sqlx::query("UPDATE sessions SET revoked_at = clock_timestamp() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id).execute(conn).await?;
    Ok(())
}
