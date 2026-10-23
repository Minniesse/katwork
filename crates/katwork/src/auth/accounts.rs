use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{invite, password, random_token, reset};
use crate::{
    error::KatError,
    ids::{SpaceId, UserId, WorkspaceId},
    tenancy::{self, Role},
    types::SpacePolicy,
};

/// Trims and lowercases; more than 254 bytes, or anything but exactly one `@` with text on
/// both sides, is `request_invalid`.
pub fn normalize_email(email: &str) -> Result<String, KatError> {
    let email = email.trim().to_lowercase();
    let well_formed = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    });
    if email.len() > 254 || !well_formed {
        return Err(KatError::RequestInvalid);
    }
    Ok(email)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnsuredUser {
    pub user_id: UserId,
    pub created: bool,
}

/// The account for `email`, created when there is none. A new account's password is the hash
/// of 32 random bytes nobody ever sees, so it has no usable password until its link is used.
pub async fn ensure_user(conn: &mut PgConnection, email: &str) -> Result<EnsuredUser, KatError> {
    let email = normalize_email(email)?;
    let display_name = email.split('@').next().unwrap_or_default().to_owned();
    let unknown = random_token()?;
    let hash = password::run(move || password::hash(&unknown)).await??;
    // A concurrent insert of the same email makes this one wait, then do nothing; the select
    // that follows is a new statement and sees the committed row.
    let inserted: Option<UserId> = sqlx::query_scalar("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, $2, $3, $4, 'en') ON CONFLICT DO NOTHING RETURNING id")
        .bind(UserId::new())
        .bind(&email)
        .bind(hash)
        .bind(display_name)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(user_id) = inserted {
        return Ok(EnsuredUser {
            user_id,
            created: true,
        });
    }
    let user_id = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(conn)
        .await?;
    Ok(EnsuredUser {
        user_id,
        created: false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRef {
    Id(WorkspaceId),
    Name(String),
}

impl WorkspaceRef {
    /// A string that parses as a UUID is an id; anything else is a name.
    pub fn parse(s: &str) -> WorkspaceRef {
        match Uuid::parse_str(s.trim()) {
            Ok(id) => Self::Id(WorkspaceId(id)),
            Err(_) => Self::Name(s.to_owned()),
        }
    }
}

/// Creates an account in `workspace` with `role` and returns its invite link. An unknown
/// workspace name is created, with a first Space `General`, only for an owner and only with a
/// `space_policy`. Everything happens in one transaction, so a refusal changes nothing.
pub async fn create_user(
    pool: &PgPool,
    email: &str,
    workspace: &WorkspaceRef,
    role: Role,
    space_policy: Option<SpacePolicy>,
    link_base: &str,
) -> Result<String, KatError> {
    let email = normalize_email(email)?;
    let mut tx = pool.begin().await?;
    // An existing account is served by `reset-link`, so an operator never overwrites one here.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email = $1)")
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(KatError::UserExists);
    }
    let workspace_id = match workspace {
        WorkspaceRef::Id(id) => sqlx::query_scalar("SELECT id FROM workspaces WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(KatError::NotFound)?,
        WorkspaceRef::Name(name) => {
            // Serializes name-based commands, so two of them never create one name twice.
            sqlx::query("SELECT pg_advisory_xact_lock(3000004)")
                .execute(&mut *tx)
                .await?;
            let found: Vec<WorkspaceId> =
                sqlx::query_scalar("SELECT id FROM workspaces WHERE name = $1 ORDER BY id LIMIT 2")
                    .bind(name.trim())
                    .fetch_all(&mut *tx)
                    .await?;
            match found[..] {
                [id] => id,
                [_, _, ..] => return Err(KatError::WorkspaceAmbiguous),
                [] if role != Role::Owner => return Err(KatError::NotFound),
                [] => {
                    let policy = space_policy.ok_or(KatError::ConfigInvalid {
                        var: "--space-policy",
                    })?;
                    let name = tenancy::validate_name(name)?;
                    let id = WorkspaceId::new();
                    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)")
                        .bind(id)
                        .bind(name)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'General', $3)")
                        .bind(SpaceId::new())
                        .bind(id)
                        .bind(policy.allow_external)
                        .execute(&mut *tx)
                        .await?;
                    id
                }
            }
        }
    };
    let user = ensure_user(&mut tx, &email).await?;
    if !user.created {
        return Err(KatError::UserExists);
    }
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user.user_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    let token = invite::issue(&mut tx, user.user_id).await?;
    tx.commit().await?;
    Ok(invite::link(link_base, &token))
}

/// A reset link for an existing account; an unknown email is `not_found`.
pub async fn reset_link(pool: &PgPool, email: &str, link_base: &str) -> Result<String, KatError> {
    let email = normalize_email(email)?;
    let mut conn = pool.acquire().await?;
    let user_id: UserId = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    let token = reset::issue(&mut conn, user_id).await?;
    Ok(reset::link(link_base, &token))
}
