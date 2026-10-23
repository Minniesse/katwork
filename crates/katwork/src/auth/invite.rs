use sqlx::{PgConnection, PgPool};

use super::reset::{TokenKind, consume, issue_kind};
use crate::{error::KatError, ids::UserId};

/// Inserts an invite token for the account, valid 72 hours and single use, and returns the
/// raw token, which is never stored.
pub async fn issue(conn: &mut PgConnection, user_id: UserId) -> Result<String, KatError> {
    issue_kind(conn, user_id, TokenKind::Invite).await
}

pub fn link(link_base: &str, token: &str) -> String {
    format!("{}/invite?token={token}", link_base.trim_end_matches('/'))
}

/// Sets the password from an unused, unexpired invite token, ends the account's sessions and
/// other pending tokens, and returns the account's email. Any other token is
/// `invite_token_invalid`, reset tokens included.
pub async fn accept(
    pool: &PgPool,
    token: &str,
    new_password: &str,
    lock_timeout: std::time::Duration,
) -> Result<String, KatError> {
    consume(pool, TokenKind::Invite, token, new_password, lock_timeout).await
}
