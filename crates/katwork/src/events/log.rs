use super::AgUiEvent;
use crate::{
    error::KatError,
    ids::{EventSeq, RunId, ThreadId, WorkspaceId},
    tenancy::Tenant,
};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction, types::Json};

/// Caller owns the transaction and takes any workspace/job/task locks before this thread lock.
pub async fn append(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: WorkspaceId,
    thread_id: ThreadId,
    run_id: Option<RunId>,
    event: &AgUiEvent,
) -> Result<EventSeq, KatError> {
    let seq = sqlx::query_scalar("UPDATE threads SET last_event_seq=last_event_seq+1 WHERE id=$1 AND workspace_id=$2 RETURNING last_event_seq")
        .bind(thread_id).bind(workspace_id).fetch_optional(&mut **tx).await?.ok_or(KatError::NotFound)?;
    let mut payload = serde_json::to_value(event).expect("event payloads are JSON values");
    replace_nul(&mut payload);
    sqlx::query("INSERT INTO events (thread_id,seq,workspace_id,run_id,kind,payload) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(thread_id).bind(seq).bind(workspace_id).bind(run_id).bind(event.kind().replace('\0', "\u{fffd}")).bind(Json(payload))
        .execute(&mut **tx).await?;
    Ok(seq)
}

fn replace_nul(value: &mut Value) {
    match value {
        Value::String(text) => {
            if text.contains('\0') {
                *text = text.replace('\0', "\u{fffd}");
            }
        }
        Value::Array(values) => values.iter_mut().for_each(replace_nul),
        Value::Object(values) => {
            let old = std::mem::take(values);
            for (key, mut value) in old {
                replace_nul(&mut value);
                values.insert(key.replace('\0', "\u{fffd}"), value);
            }
        }
        _ => {}
    }
}

pub async fn tail(pool: &PgPool, tenant: &Tenant, thread: ThreadId) -> Result<EventSeq, KatError> {
    sqlx::query_scalar("SELECT last_event_seq FROM threads WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id())
        .bind(thread)
        .fetch_optional(pool)
        .await?
        .ok_or(KatError::NotFound)
}

pub async fn since(
    pool: &PgPool,
    tenant: &Tenant,
    thread: ThreadId,
    after: EventSeq,
    limit: u32,
) -> Result<Vec<(EventSeq, AgUiEvent)>, KatError> {
    tail(pool, tenant, thread).await?;
    let rows: Vec<(EventSeq, Json<AgUiEvent>)> = sqlx::query_as("SELECT seq,payload FROM events WHERE workspace_id=$1 AND thread_id=$2 AND seq>$3 ORDER BY seq LIMIT $4")
        .bind(tenant.workspace_id()).bind(thread).bind(after).bind(i64::from(limit)).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(seq, Json(event))| (seq, event))
        .collect())
}
