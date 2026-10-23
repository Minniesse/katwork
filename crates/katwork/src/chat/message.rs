use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, types::Json};

use super::{Appended, Author, Message, MessagePage, NewMessage};
use crate::{
    error::KatError,
    ids::{EventSeq, JobId, MessageId, ThreadId},
    tenancy::{self, Tenant},
};

const SELECT: &str = include_str!("message_select.sql");

const EVENT_SEQ: &str = include_str!("event_seq.sql");

/// User bodies are capped at 32,768 UTF-8 bytes; client ids at 64 Unicode scalars.
pub fn validate_user_post(body: &str, client_msg_id: Option<&str>) -> Result<(), KatError> {
    if body.trim().is_empty() {
        return Err(KatError::MessageEmpty);
    }
    if body.contains('\0') {
        return Err(KatError::MessageInvalid);
    }
    if body.len() > 32_768 {
        return Err(KatError::MessageTooLarge);
    }
    if let Some(id) = client_msg_id {
        if id.chars().count() > 64 {
            return Err(KatError::ClientMsgIdTooLong);
        }
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(KatError::ClientMsgIdInvalid);
        }
    }
    Ok(())
}

/// Insert once or return the stored original. Callers must use Read Committed isolation.
pub async fn append(
    conn: &mut PgConnection,
    tenant: &Tenant,
    thread: ThreadId,
    new: NewMessage,
) -> Result<Appended, KatError> {
    // Check before the insert so a foreign thread cannot trigger even a foreign-key row lock.
    sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id())
        .bind(thread)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    let (kind, user, kat) = match new.author {
        Author::User(id) => {
            if id != tenant.user_id() {
                return Err(KatError::NotFound);
            }
            ("user", Some(id), None)
        }
        Author::Kat(id) => ("kat", None, Some(id)),
        Author::System => ("system", None, None),
    };
    let inserted: Option<MessageId> = sqlx::query_scalar("INSERT INTO messages
        (id,workspace_id,thread_id,author_kind,author_user_id,author_kat_id,body,client_msg_id,idempotency_key,synthetic,stopped,question,meta,job_id)
        SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14
        WHERE $4<>'user' OR EXISTS (SELECT 1 FROM memberships WHERE workspace_id=$2 AND user_id=$5)
        ON CONFLICT DO NOTHING RETURNING id")
        .bind(new.id.unwrap_or_default()).bind(tenant.workspace_id()).bind(thread).bind(kind).bind(user).bind(kat)
        .bind(&new.body).bind(&new.client_msg_id).bind(&new.idempotency_key).bind(new.synthetic).bind(new.stopped)
        .bind(new.question.map(Json)).bind(new.meta.map(Json)).bind(new.job_id)
        .fetch_optional(&mut *conn).await?;
    let message: Message = if let Some(id) = inserted {
        QueryBuilder::<Postgres>::new(SELECT)
            .push(" AND m.id=$3")
            .build_query_as()
            .bind(tenant.workspace_id())
            .bind(thread)
            .bind(id)
            .fetch_one(&mut *conn)
            .await?
    } else {
        // A removed member cannot insert or retrieve a retry through a stale Tenant.
        if let Some(user) = user {
            tenancy::resolve(&mut *conn, user, tenant.workspace_id()).await?;
        }
        // A new statement sees a concurrent winner that INSERT's snapshot could not see.
        QueryBuilder::<Postgres>::new(SELECT)
            .push(" AND ((m.author_user_id=$3 AND m.client_msg_id=$4) OR m.idempotency_key=$5)")
            .build_query_as()
            .bind(tenant.workspace_id())
            .bind(thread)
            .bind(user)
            .bind(&new.client_msg_id)
            .bind(&new.idempotency_key)
            .fetch_one(&mut *conn)
            .await?
    };
    if inserted.is_none()
        && new.client_msg_id.is_some()
        && message.client_msg_id == new.client_msg_id
        && message.body != new.body
    {
        return Err(KatError::ClientMsgIdConflict);
    }
    Ok(Appended {
        message,
        created: inserted.is_some(),
    })
}

pub async fn set_job(
    conn: &mut PgConnection,
    tenant: &Tenant,
    message: MessageId,
    job: JobId,
) -> Result<(), KatError> {
    let result = sqlx::query(
        "UPDATE messages SET job_id=$3 WHERE workspace_id=$1 AND id=$2
        AND EXISTS (SELECT 1 FROM jobs WHERE workspace_id=$1 AND id=$3)",
    )
    .bind(tenant.workspace_id())
    .bind(message)
    .bind(job)
    .execute(conn)
    .await?;
    if result.rows_affected() == 0 {
        return Err(KatError::NotFound);
    }
    Ok(())
}

pub async fn list(
    pool: &PgPool,
    tenant: &Tenant,
    thread: ThreadId,
    before: Option<MessageId>,
    limit: u16,
) -> Result<MessagePage, KatError> {
    if limit == 0 {
        return Err(KatError::RequestInvalid);
    }
    // Read the cursor first: a later commit can be replayed twice, never omitted from both views.
    let event_seq: EventSeq = sqlx::query_scalar(EVENT_SEQ)
        .bind(tenant.workspace_id())
        .bind(thread)
        .fetch_optional(pool)
        .await?
        .ok_or(KatError::NotFound)?;
    let mut messages: Vec<Message> = QueryBuilder::<Postgres>::new(SELECT)
        .push(" AND ($3::uuid IS NULL OR m.id<$3) ORDER BY m.id DESC LIMIT $4")
        .build_query_as()
        .bind(tenant.workspace_id())
        .bind(thread)
        .bind(before)
        .bind(i64::from(limit.min(200)))
        .fetch_all(pool)
        .await?;
    messages.reverse();
    Ok(MessagePage {
        messages,
        event_seq,
    })
}
