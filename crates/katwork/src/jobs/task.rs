use sqlx::{PgConnection, types::Json};

use super::TaskInput;
use crate::{error::KatError, ids::*};

/// Intake holds the job and thread locks, and any Kat lock, before enqueueing.
pub async fn enqueue(
    conn: &mut PgConnection,
    job_id: JobId,
    kat_id: KatId,
    kat_policy_version: i64,
    input: &TaskInput,
    idempotency_key: &str,
) -> Result<TaskId, KatError> {
    if idempotency_key.is_empty() || idempotency_key.contains('\0') || kat_policy_version < 1 {
        return Err(KatError::RequestInvalid);
    }
    // This internal API receives a caller-owned job id; derive scope before checking other ids.
    let workspace: WorkspaceId = sqlx::query_scalar("SELECT workspace_id FROM jobs WHERE id=$1")
        .bind(job_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT j.status FROM jobs j
        JOIN kats k ON k.workspace_id=j.workspace_id AND k.id=$3
        WHERE j.workspace_id=$1 AND j.id=$2 AND j.thread_id=$4
        AND j.request_message_id=$5 AND j.requested_by=$6",
    )
    .bind(workspace)
    .bind(job_id)
    .bind(kat_id)
    .bind(input.thread_id)
    .bind(input.request_message_id)
    .bind(input.actor)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(KatError::NotFound)?;
    let inserted: Option<TaskId> = if status.is_none() {
        sqlx::query_scalar(
            "INSERT INTO tasks
            (id,workspace_id,job_id,kat_id,kat_policy_version,input,state,idempotency_key)
            VALUES ($1,$2,$3,$4,$5,$6,'queued',$7)
            ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
        )
        .bind(TaskId::new())
        .bind(workspace)
        .bind(job_id)
        .bind(kat_id)
        .bind(kat_policy_version)
        .bind(Json(input))
        .bind(idempotency_key)
        .fetch_optional(&mut *conn)
        .await?
    } else {
        None
    };
    if let Some(id) = inserted {
        return Ok(id);
    }
    let existing: Option<(TaskId, JobId, KatId, i64, Json<TaskInput>)> = sqlx::query_as(
        "SELECT id,job_id,kat_id,kat_policy_version,input FROM tasks
        WHERE workspace_id=$1 AND idempotency_key=$2",
    )
    .bind(workspace)
    .bind(idempotency_key)
    .fetch_optional(conn)
    .await?;
    if let Some((id, job, kat, version, Json(saved))) = existing {
        if job == job_id && kat == kat_id && version == kat_policy_version && saved == *input {
            return Ok(id);
        }
        return Err(KatError::RequestInvalid);
    }
    Err(if status.is_some() {
        KatError::JobAlreadyFinished
    } else {
        KatError::RequestInvalid
    })
}
