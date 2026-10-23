use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use tracing::instrument::WithSubscriber;

use super::{Role, Tenant};
use crate::{
    auth::{
        accounts, invite as invite_token,
        mailer::{self, MailKind, Mailer},
    },
    config::Config,
    error::KatError,
    ids::UserId,
    types::Locale,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Member {
    pub user_id: UserId,
    pub email: String,
    pub display_name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteDelivery {
    Email,
    Link,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub email: String,
    pub role: Role,
    pub delivery: InviteDelivery,
}

#[derive(Clone, Serialize)]
pub struct InviteResult {
    pub user_id: UserId,
    pub created: bool,
    pub link: Option<String>,
}

impl fmt::Debug for InviteResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InviteResult")
            .field("user_id", &self.user_id)
            .field("created", &self.created)
            .field("link", &self.link.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// What the invite form can offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct InviteOptions {
    /// Invites can go by email: SMTP is configured. The demo console mailer never counts.
    pub email: bool,
}

/// Owner or admin only.
pub fn invite_options(config: &Config, tenant: &Tenant) -> Result<InviteOptions, KatError> {
    tenant.require_admin()?;
    Ok(InviteOptions {
        email: config.smtp.is_some(),
    })
}

/// The workspace's members, ordered by email; owner or admin only.
pub async fn list(pool: &PgPool, tenant: &Tenant) -> Result<Vec<Member>, KatError> {
    tenant.require_admin()?;
    Ok(sqlx::query_as(
        "SELECT u.id AS user_id, u.email, u.display_name, m.role
        FROM memberships m JOIN users u ON u.id = m.user_id
        WHERE m.workspace_id = $1 ORDER BY u.email",
    )
    .bind(tenant.workspace_id())
    .fetch_all(pool)
    .await?)
}

/// Locks the workspace row, the first class of the global lock order, which serializes every
/// membership change in the workspace, and re-reads the actor's role behind that lock. Returns
/// the actor and the workspace's name.
async fn lock_workspace(
    conn: &mut PgConnection,
    tenant: &Tenant,
) -> Result<(Tenant, String), KatError> {
    let name: String =
        sqlx::query_scalar("SELECT name FROM workspaces WHERE id = $1 FOR NO KEY UPDATE")
            .bind(tenant.workspace_id())
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(KatError::NotFound)?;
    let actor = super::resolve(&mut *conn, tenant.user_id(), tenant.workspace_id()).await?;
    actor.require_admin()?;
    Ok((actor, name))
}

async fn role_of(
    conn: &mut PgConnection,
    tenant: &Tenant,
    user_id: UserId,
) -> Result<Option<Role>, KatError> {
    Ok(
        sqlx::query_scalar("SELECT role FROM memberships WHERE workspace_id = $1 AND user_id = $2")
            .bind(tenant.workspace_id())
            .bind(user_id)
            .fetch_optional(conn)
            .await?,
    )
}

/// Fails with `last_owner_required` unless another owner stays.
async fn keep_an_owner(conn: &mut PgConnection, tenant: &Tenant) -> Result<(), KatError> {
    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memberships WHERE workspace_id = $1 AND role = 'owner'",
    )
    .bind(tenant.workspace_id())
    .fetch_one(conn)
    .await?;
    if owners <= 1 {
        return Err(KatError::LastOwnerRequired);
    }
    Ok(())
}

/// Adds the account for `invite.email` to the workspace, creating it when there is none. Only a
/// new account gets a set-password link, returned or mailed; an existing one gets the membership
/// and nothing else, so an admin can never take over an account made elsewhere (ADR-0004). The
/// mail is sent after the commit without delaying the response; a failed delivery leaves the
/// invite standing, and the person can still use password reset.
pub async fn invite(
    pool: &PgPool,
    config: &Config,
    mailer: Arc<dyn Mailer>,
    tenant: &Tenant,
    invite: Invite,
) -> Result<InviteResult, KatError> {
    tenant.require_admin()?;
    if invite.role == Role::Owner && tenant.role() != Role::Owner {
        return Err(KatError::Forbidden);
    }
    // The demo console mailer prints only the demo account's mail, so it is not a delivery path.
    if invite.delivery == InviteDelivery::Email && config.smtp.is_none() {
        return Err(KatError::SmtpUnconfigured);
    }
    let email = accounts::normalize_email(&invite.email)?;
    // Hashed before any lock: Argon2 can wait for a slot, and every tool call of a running Kat
    // takes this workspace row (`guards::authorize`). Unused when the account already exists.
    let hash = accounts::unusable_password_hash().await?;
    let mut tx = pool.begin().await?;
    let (actor, workspace) = lock_workspace(&mut tx, tenant).await?;
    if invite.role == Role::Owner && actor.role() != Role::Owner {
        return Err(KatError::Forbidden);
    }
    // A new account starts in the inviter's language, which also orders the mail.
    let locale = match sqlx::query_scalar::<_, String>("SELECT locale FROM users WHERE id = $1")
        .bind(actor.user_id())
        .fetch_one(&mut *tx)
        .await?
        .as_str()
    {
        "th" => Locale::Th,
        _ => Locale::En,
    };
    let user = accounts::ensure_user_with_hash(&mut tx, &email, locale, hash).await?;
    if role_of(&mut tx, &actor, user.user_id).await?.is_some() {
        return Err(KatError::MemberExists);
    }
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(actor.workspace_id())
        .bind(user.user_id)
        .bind(invite.role)
        .execute(&mut *tx)
        .await?;
    let token = if user.created {
        Some(invite_token::issue(&mut tx, user.user_id).await?)
    } else {
        None
    };
    tx.commit().await?;
    let link = token.map(|token| invite_token::link(config.public_url_or_default(), &token));
    let link = match (invite.delivery, link) {
        (InviteDelivery::Link, link) => link,
        (InviteDelivery::Email, Some(link)) => {
            let mail = mailer::invite_mail(email, locale, &link, &workspace);
            tokio::spawn(
                async move {
                    if mailer.send(&mail).await.is_err() {
                        tracing::warn!(kind = ?MailKind::Invite, outcome = "failed", "invite_mail_not_sent");
                    }
                }
                .with_current_subscriber(),
            );
            None
        }
        (InviteDelivery::Email, None) => None,
    };
    Ok(InviteResult {
        user_id: user.user_id,
        created: user.created,
        link,
    })
}

/// Changes a member's role. Only an owner grants `Owner` or changes an owner's role, and the
/// last owner stays one. It applies on that user's next request, because `resolve` reads the
/// membership every time.
pub async fn set_role(
    pool: &PgPool,
    tenant: &Tenant,
    user_id: UserId,
    role: Role,
) -> Result<(), KatError> {
    tenant.require_admin()?;
    let mut tx = pool.begin().await?;
    let (actor, _) = lock_workspace(&mut tx, tenant).await?;
    let current = role_of(&mut tx, &actor, user_id)
        .await?
        .ok_or(KatError::NotFound)?;
    if (role == Role::Owner || current == Role::Owner) && actor.role() != Role::Owner {
        return Err(KatError::Forbidden);
    }
    if current == Role::Owner && role != Role::Owner {
        keep_an_owner(&mut tx, &actor).await?;
    }
    sqlx::query("UPDATE memberships SET role = $3 WHERE workspace_id = $1 AND user_id = $2")
        .bind(actor.workspace_id())
        .bind(user_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a member from the workspace. Only the membership row goes: messages and jobs
/// reference the user, so their history stays. Only an owner removes an owner, and the last
/// owner stays.
pub async fn remove(pool: &PgPool, tenant: &Tenant, user_id: UserId) -> Result<(), KatError> {
    tenant.require_admin()?;
    let mut tx = pool.begin().await?;
    let (actor, _) = lock_workspace(&mut tx, tenant).await?;
    let current = role_of(&mut tx, &actor, user_id)
        .await?
        .ok_or(KatError::NotFound)?;
    if current == Role::Owner {
        if actor.role() != Role::Owner {
            return Err(KatError::Forbidden);
        }
        keep_an_owner(&mut tx, &actor).await?;
    }
    sqlx::query("DELETE FROM memberships WHERE workspace_id = $1 AND user_id = $2")
        .bind(actor.workspace_id())
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
