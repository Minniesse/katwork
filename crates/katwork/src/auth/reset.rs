use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    mailer::{Mail, MailKind, Mailer},
    password, random_token, token_hash, valid_token,
};
use crate::{error::KatError, ids::UserId};

pub async fn request(
    pool: &PgPool,
    mailer: &dyn Mailer,
    email: &str,
    link_base: &str,
) -> Result<(), KatError> {
    let mut tx = pool.begin().await?;
    let user: Option<(UserId, String)> =
        sqlx::query_as("SELECT id, email FROM users WHERE email = $1 FOR NO KEY UPDATE")
            .bind(email.trim().to_lowercase())
            .fetch_optional(&mut *tx)
            .await?;
    let Some((user_id, email)) = user else {
        return Ok(());
    };
    let token = random_token()?;
    sqlx::query("INSERT INTO password_resets (id, user_id, token_sha256, expires_at) VALUES ($1, $2, $3, clock_timestamp() + interval '30 minutes')")
        .bind(Uuid::now_v7()).bind(user_id).bind(token_hash(&token)).execute(&mut *tx).await?;
    tx.commit().await?;
    let mail = Mail {
        kind: MailKind::PasswordReset,
        to: email,
        subject: "KatWork password reset".into(),
        body: format!("{}/reset?token={token}", link_base.trim_end_matches('/')),
    };
    // Delivery failures must not reveal whether the account exists.
    let _ = mailer.send(&mail).await;
    Ok(())
}

pub async fn confirm(pool: &PgPool, token: &str, new_password: &str) -> Result<(), KatError> {
    if !valid_token(token) {
        return Err(KatError::ResetTokenInvalid);
    }
    let digest = token_hash(token);
    let user_id: UserId = sqlx::query_scalar("SELECT user_id FROM password_resets WHERE token_sha256 = $1 AND used_at IS NULL AND expires_at > clock_timestamp()")
        .bind(&digest).fetch_optional(pool).await?.ok_or(KatError::ResetTokenInvalid)?;
    let new_password = new_password.to_owned();
    let hash = tokio::task::spawn_blocking(move || password::hash(&new_password))
        .await
        .map_err(|_| KatError::AuthFailed)??;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    let used = sqlx::query("UPDATE password_resets SET used_at = clock_timestamp() WHERE token_sha256 = $1 AND used_at IS NULL AND expires_at > clock_timestamp()")
        .bind(digest).execute(&mut *tx).await?;
    if used.rows_affected() != 1 {
        return Err(KatError::ResetTokenInvalid);
    }
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    revoke_credentials(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(())
}

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
