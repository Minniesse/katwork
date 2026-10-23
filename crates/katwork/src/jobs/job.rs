use sqlx::{Acquire, PgConnection, Postgres, Transaction, types::Json};

pub use super::{HarnessEnd, JobSummary};
use crate::{
    agent::RunFacts,
    chat::{Author, NewMessage, NoticeKat, NoticeMeta, message},
    error::KatError,
    events::{
        AgUiEvent, DecisionEvent, JobStatusEvent, KatState, KatStateEvent, KatworkEvent,
        MessageStoredEvent, RunClosedEvent, RunEnd, log,
    },
    ids::*,
    policy::{self, PolicyInputs, PolicyStatus, Thresholds, Verdict},
    team::kat,
    tenancy::Tenant,
    types::{JobStatus, ReasonCode},
};

pub async fn create(
    conn: &mut PgConnection,
    tenant: &Tenant,
    thread_id: ThreadId,
    request_message_id: MessageId,
) -> Result<JobId, KatError> {
    insert_job(conn, tenant, thread_id, request_message_id, None).await
}

pub async fn create_follow_up(
    conn: &mut PgConnection,
    tenant: &Tenant,
    parent: JobId,
    request_message_id: MessageId,
) -> Result<JobId, KatError> {
    let (thread, status): (ThreadId, Option<String>) =
        sqlx::query_as("SELECT thread_id,status FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(tenant.workspace_id())
            .bind(parent)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(KatError::NotFound)?;
    if status.as_deref() != Some("needs_input") {
        return Err(KatError::JobNotAwaitingInput);
    }
    let request_thread: ThreadId =
        sqlx::query_scalar("SELECT thread_id FROM messages WHERE workspace_id=$1 AND id=$2")
            .bind(tenant.workspace_id())
            .bind(request_message_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(KatError::NotFound)?;
    if request_thread != thread {
        return Err(KatError::JobNotAwaitingInput);
    }
    insert_job(conn, tenant, thread, request_message_id, Some(parent)).await
}

async fn insert_job(
    conn: &mut PgConnection,
    tenant: &Tenant,
    thread: ThreadId,
    request: MessageId,
    parent: Option<JobId>,
) -> Result<JobId, KatError> {
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM messages m
        JOIN memberships u ON u.workspace_id=m.workspace_id AND u.user_id=m.author_user_id
        WHERE m.workspace_id=$1 AND m.id=$2 AND m.thread_id=$3 AND m.author_user_id=$4)",
    )
    .bind(tenant.workspace_id())
    .bind(request)
    .bind(thread)
    .bind(tenant.user_id())
    .fetch_one(&mut *conn)
    .await?;
    if !valid {
        return Err(KatError::NotFound);
    }
    sqlx::query_scalar(
        "INSERT INTO jobs
        (id,workspace_id,thread_id,request_message_id,requested_by,parent_job_id)
        VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (request_message_id) DO NOTHING RETURNING id",
    )
    .bind(JobId::new())
    .bind(tenant.workspace_id())
    .bind(thread)
    .bind(request)
    .bind(tenant.user_id())
    .bind(parent)
    .fetch_optional(conn)
    .await?
    .ok_or(KatError::RequestInvalid)
}

pub async fn latest_in_thread(
    conn: &mut PgConnection,
    tenant: &Tenant,
    thread: ThreadId,
) -> Result<Option<JobSummary>, KatError> {
    sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id())
        .bind(thread)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(KatError::NotFound)?;
    Ok(sqlx::query_as(include_str!("latest_in_thread.sql"))
        .bind(tenant.workspace_id())
        .bind(thread)
        .fetch_optional(conn)
        .await?)
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: JobId,
    workspace_id: WorkspaceId,
    thread_id: ThreadId,
    status: Option<String>,
}

impl JobRow {
    fn unfinished(&self) -> Result<(), KatError> {
        if self.status.is_some() {
            Err(KatError::JobAlreadyFinished)
        } else {
            Ok(())
        }
    }
}

// These internal helpers receive a job id whose row the harness already owns and locked.
async fn owned_job(conn: &mut PgConnection, job: JobId) -> Result<JobRow, KatError> {
    let row: JobRow =
        sqlx::query_as("SELECT id,workspace_id,thread_id,status FROM jobs WHERE id=$1")
            .bind(job)
            .fetch_optional(conn)
            .await?
            .ok_or(KatError::NotFound)?;
    row.unfinished()?;
    Ok(row)
}

/// Caller holds job and thread locks, in that order, and has appended this job's decision.
pub async fn finish_by_policy(
    tx: &mut Transaction<'_, Postgres>,
    job_id: JobId,
    v: &Verdict,
    decision_seq: EventSeq,
) -> Result<EventSeq, KatError> {
    let job = owned_job(tx, job_id).await?;
    let Json(event): Json<AgUiEvent> = sqlx::query_scalar(
        "SELECT payload FROM events
        WHERE workspace_id=$1 AND thread_id=$2 AND seq=$3 AND kind='katwork.decision'",
    )
    .bind(job.workspace_id)
    .bind(job.thread_id)
    .bind(decision_seq)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(KatError::RequestInvalid)?;
    let AgUiEvent::Custom { name, value } = event else {
        return Err(KatError::RequestInvalid);
    };
    let decision: DecisionEvent =
        serde_json::from_value(value).map_err(|_| KatError::RequestInvalid)?;
    let inputs: PolicyInputs =
        serde_json::from_value(decision.inputs).map_err(|_| KatError::RequestInvalid)?;
    let thresholds: Thresholds =
        serde_json::from_value(decision.thresholds).map_err(|_| KatError::RequestInvalid)?;
    if name != "katwork.decision"
        || decision.job_id != job_id
        || thresholds != Thresholds::SP1
        || decision.policy_version != thresholds.version
        || decision.verdict != serde_json::json!(v)
        || policy::decide(&inputs, &thresholds) != *v
    {
        return Err(KatError::RequestInvalid);
    }
    finish(
        tx,
        &job,
        v.status.into(),
        v.reason,
        "policy",
        Some(decision_seq),
    )
    .await
}

/// Caller holds job and thread locks; it closes runs and releases tasks in the same transaction.
pub async fn finish_by_harness(
    tx: &mut Transaction<'_, Postgres>,
    job_id: JobId,
    end: HarnessEnd,
) -> Result<EventSeq, KatError> {
    let job = owned_job(tx, job_id).await?;
    let (status, reason) = match end {
        HarnessEnd::Cancelled => (JobStatus::Cancelled, ReasonCode::Cancelled),
        HarnessEnd::Error { code } => (JobStatus::Error, code),
    };
    finish(tx, &job, status, Some(reason), "harness", None).await
}

async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    job: &JobRow,
    status: JobStatus,
    reason: Option<ReasonCode>,
    decided_by: &str,
    decision_seq: Option<EventSeq>,
) -> Result<EventSeq, KatError> {
    let changed = sqlx::query(
        "UPDATE jobs SET status=$3,reason=$4,decided_by=$5,decision_seq=$6,
        finished_at=clock_timestamp() WHERE workspace_id=$1 AND id=$2 AND status IS NULL",
    )
    .bind(job.workspace_id)
    .bind(job.id)
    .bind(status.wire())
    .bind(reason.map(|r| r.wire()))
    .bind(decided_by)
    .bind(decision_seq)
    .execute(&mut **tx)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(KatError::JobAlreadyFinished);
    }
    log::append(
        tx,
        job.workspace_id,
        job.thread_id,
        None,
        &KatworkEvent::JobStatus(JobStatusEvent {
            job_id: job.id,
            status: Some(status),
            reason,
            hold_reason: None,
        })
        .into(),
    )
    .await
}

/// Caller locks job, tasks (and the run being closed), then thread, before any write.
/// Errors roll back this conclusion; commit and notify only after all caller-owned writes succeed.
pub async fn conclude(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &Tenant,
    job_id: JobId,
    thread_id: ThreadId,
    kat_id: Option<KatId>,
    idem_key: &str,
    facts: &RunFacts,
) -> Result<EventSeq, KatError> {
    let mut savepoint = tx.begin().await?;
    let tx = &mut savepoint;
    let job: JobRow = sqlx::query_as(
        "SELECT id,workspace_id,thread_id,status FROM jobs
        WHERE workspace_id=$1 AND id=$2 AND thread_id=$3",
    )
    .bind(tenant.workspace_id())
    .bind(job_id)
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(KatError::NotFound)?;
    job.unfinished()?;
    if idem_key.is_empty() || idem_key.contains('\0') {
        return Err(KatError::RequestInvalid);
    }
    let kat = match kat_id {
        Some(id) => Some(kat::get_on(&mut **tx, tenant, id).await?),
        None => None,
    };
    if let Some(run) = facts.run_id {
        let (state, ended): (String, bool) = sqlx::query_as(
            "SELECT state,ended_at IS NOT NULL FROM runs
            WHERE workspace_id=$1 AND id=$2 AND job_id=$3 AND kat_id=$4",
        )
        .bind(job.workspace_id)
        .bind(run)
        .bind(job_id)
        .bind(kat_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(KatError::NotFound)?;
        let valid = if facts.run_closed {
            state == "cancelled" && ended
        } else {
            state == "running" && !ended
        };
        if !valid {
            return Err(KatError::RequestInvalid);
        }
        // The loop must close its ledger calls before handing facts to conclusion.
        let open_calls: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tool_calls
            WHERE workspace_id=$1 AND run_id=$2 AND ended_at IS NULL)",
        )
        .bind(job.workspace_id)
        .bind(run)
        .fetch_one(&mut **tx)
        .await?;
        if open_calls {
            return Err(KatError::RequestInvalid);
        }
    } else {
        let running: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM runs
            WHERE workspace_id=$1 AND job_id=$2 AND state='running')",
        )
        .bind(job.workspace_id)
        .bind(job_id)
        .fetch_one(&mut **tx)
        .await?;
        if facts.run_closed || running {
            return Err(KatError::RequestInvalid);
        }
    }
    let v = policy::decide_job(facts, &Thresholds::SP1);
    // Job events remain thread-scoped, including when the loop already closed its run.
    let decision_seq = log::append(
        tx,
        job.workspace_id,
        thread_id,
        None,
        &policy::decision_event(job_id, facts, &v, &Thresholds::SP1).into(),
    )
    .await?;
    if let Some(kat) = &kat {
        let question = facts
            .pending_question
            .as_ref()
            .map(|q| crate::types::Question {
                question: q.question.replace('\0', "\u{fffd}"),
                options: q
                    .options
                    .iter()
                    .map(|s| s.replace('\0', "\u{fffd}"))
                    .collect(),
            });
        let body = question.as_ref().map(|q| q.question.clone()).or_else(|| {
            facts
                .final_text
                .as_ref()
                .map(|s| s.replace('\0', "\u{fffd}"))
        });
        if let Some(body) = body {
            store_message(
                tx,
                tenant,
                &job,
                NewMessage {
                    id: Some(facts.reply_message_id),
                    author: Author::Kat(kat.id),
                    body,
                    client_msg_id: None,
                    idempotency_key: Some(format!("reply:{idem_key}")),
                    synthetic: facts.synthetic,
                    stopped: false,
                    question,
                    meta: None,
                    job_id: Some(job_id),
                },
            )
            .await?;
        }
    }
    if v.status == PolicyStatus::Stopped {
        let code = match v.reason.ok_or(KatError::RequestInvalid)? {
            ReasonCode::WorkspacePaused => ReasonCode::WorkspacePausedStopped,
            ReasonCode::KatPaused => ReasonCode::KatPausedStopped,
            code => code,
        };
        let kats = kat
            .as_ref()
            .map(|k| NoticeKat {
                id: k.id,
                name: k.name.clone(),
            })
            .into_iter()
            .collect();
        store_message(
            tx,
            tenant,
            &job,
            NewMessage {
                id: None,
                author: Author::System,
                body: code.wire().into(),
                client_msg_id: None,
                idempotency_key: Some(format!("notice:{idem_key}")),
                synthetic: false,
                stopped: false,
                question: None,
                meta: Some(NoticeMeta { code, kats }),
                job_id: Some(job_id),
            },
        )
        .await?;
    }
    let mut seq = finish_by_policy(tx, job_id, &v, decision_seq).await?;
    if let Some(run_id) = facts.run_id {
        let state = match v.reason {
            Some(ReasonCode::ApprovalPending) => KatState::NeedsApproval,
            Some(ReasonCode::QuestionPending) => KatState::Waiting,
            Some(ReasonCode::WorkspacePaused) => KatState::Paused,
            _ => KatState::Idle,
        };
        seq = log::append(
            tx,
            job.workspace_id,
            thread_id,
            None,
            &KatworkEvent::KatState(KatStateEvent {
                kat_id: kat_id.ok_or(KatError::NotFound)?,
                state,
            })
            .into(),
        )
        .await?;
        if !facts.run_closed {
            let code = v.reason.map(|r| r.wire());
            sqlx::query(
                "UPDATE runs SET state='finished',stop_reason=$3,ended_at=clock_timestamp()
                WHERE workspace_id=$1 AND id=$2",
            )
            .bind(job.workspace_id)
            .bind(run_id)
            .bind(code)
            .execute(&mut **tx)
            .await?;
            log::append(
                tx,
                job.workspace_id,
                thread_id,
                Some(run_id),
                &KatworkEvent::RunClosed(RunClosedEvent {
                    run_id,
                    end: RunEnd::Finished,
                    code: code.map(str::to_owned),
                })
                .into(),
            )
            .await?;
            seq = log::append(
                tx,
                job.workspace_id,
                thread_id,
                Some(run_id),
                &AgUiEvent::RunFinished {
                    thread_id: thread_id.to_string(),
                    run_id: run_id.to_string(),
                    outcome: None,
                },
            )
            .await?;
        }
    }
    savepoint.commit().await?;
    Ok(seq)
}

async fn store_message(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &Tenant,
    job: &JobRow,
    new: NewMessage,
) -> Result<(), KatError> {
    let appended = message::append(tx, tenant, job.thread_id, new).await?;
    if !appended.created {
        return Err(KatError::RequestInvalid);
    }
    log::append(
        tx,
        job.workspace_id,
        job.thread_id,
        None,
        &KatworkEvent::MessageStored(MessageStoredEvent::from(&appended.message)).into(),
    )
    .await?;
    Ok(())
}

/// Stop only an unfinished, tenant-owned job. All terminal writers serialize on its job row.
pub async fn cancel(
    pool: &sqlx::PgPool,
    hub: &crate::events::EventHub,
    tenant: &Tenant,
    job_id: JobId,
) -> Result<(), KatError> {
    use crate::events::{
        RunStartedEvent,
        sink::{close_locked_calls, close_locked_run},
    };
    let mut tx = pool.begin().await?;
    crate::db::set_configured_lock_timeout(&mut tx).await?;
    let workspace = tenant.workspace_id();
    sqlx::query("SELECT id FROM jobs WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KatError::NotFound)?;
    let job = owned_job(&mut tx, job_id).await?;
    let tasks: Vec<TaskId> = sqlx::query_scalar(include_str!("cancel_tasks.sql"))
        .bind(workspace)
        .bind(job_id)
        .fetch_all(&mut *tx)
        .await?;
    let ids: Vec<_> = tasks.iter().map(|id| id.0).collect();
    sqlx::query(
        "SELECT task_id FROM task_leases WHERE task_id=ANY($1) ORDER BY task_id FOR NO KEY UPDATE",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    let runs: Vec<(RunId, KatId)> = sqlx::query_as("SELECT id,kat_id FROM runs WHERE workspace_id=$1 AND job_id=$2 ORDER BY id FOR NO KEY UPDATE")
        .bind(workspace).bind(job_id).fetch_all(&mut *tx).await?;
    let run_ids: Vec<_> = runs.iter().map(|(id, _)| id.0).collect();
    let calls: Vec<(ToolCallRecordId, RunId)> = sqlx::query_as("SELECT id,run_id FROM tool_calls WHERE workspace_id=$1 AND run_id=ANY($2) AND ended_at IS NULL ORDER BY id FOR NO KEY UPDATE")
        .bind(workspace).bind(&run_ids).fetch_all(&mut *tx).await?;
    sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(job.thread_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("UPDATE tasks SET state='cancelled',hold_reason=NULL WHERE workspace_id=$1 AND job_id=$2 AND state IN ('queued','leased')")
        .bind(workspace).bind(job_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM task_leases WHERE task_id=ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    for (run, kat) in runs {
        let running: bool = sqlx::query_scalar(
            "SELECT state='running' AND ended_at IS NULL FROM runs WHERE workspace_id=$1 AND id=$2",
        )
        .bind(workspace)
        .bind(run)
        .fetch_one(&mut *tx)
        .await?;
        if !running {
            continue;
        }
        let open: Vec<_> = calls
            .iter()
            .filter(|(_, r)| *r == run)
            .map(|(id, _)| *id)
            .collect();
        close_locked_calls(&mut tx, workspace, job.thread_id, run, &open, "cancelled").await?;
        let started: Option<Json<RunStartedEvent>> = sqlx::query_scalar("SELECT payload->'value' FROM events WHERE workspace_id=$1 AND thread_id=$2 AND run_id=$3 AND kind='katwork.run_started' ORDER BY seq DESC LIMIT 1")
            .bind(workspace).bind(job.thread_id).bind(run).fetch_optional(&mut *tx).await?;
        if let Some(Json(started)) = started {
            let text: Option<String> = sqlx::query_scalar("SELECT string_agg(payload->>'delta','' ORDER BY seq) FROM events WHERE workspace_id=$1 AND thread_id=$2 AND run_id=$3 AND kind='TEXT_MESSAGE_CONTENT' AND payload->>'messageId'=$4 AND seq>(SELECT coalesce(max(seq),0) FROM events WHERE workspace_id=$1 AND thread_id=$2 AND run_id=$3 AND kind='TEXT_MESSAGE_START' AND payload->>'messageId'=$4)")
                .bind(workspace).bind(job.thread_id).bind(run).bind(started.reply_message_id.to_string()).fetch_one(&mut *tx).await?;
            if let Some(body) = text.filter(|s| !s.is_empty()) {
                store_message(
                    &mut tx,
                    tenant,
                    &job,
                    NewMessage {
                        id: Some(started.reply_message_id),
                        author: Author::Kat(kat),
                        body,
                        client_msg_id: None,
                        idempotency_key: Some(format!("stopped:{run}")),
                        synthetic: started.synthetic,
                        stopped: true,
                        question: None,
                        meta: None,
                        job_id: Some(job_id),
                    },
                )
                .await?;
            }
        }
        log::append(
            &mut tx,
            workspace,
            job.thread_id,
            None,
            &KatworkEvent::KatState(KatStateEvent {
                kat_id: kat,
                state: KatState::Idle,
            })
            .into(),
        )
        .await?;
        close_locked_run(
            &mut tx,
            workspace,
            job.thread_id,
            run,
            &[],
            RunEnd::Cancelled,
            "cancelled",
        )
        .await?;
    }
    let seq = finish_by_harness(&mut tx, job_id, HarnessEnd::Cancelled).await?;
    tx.commit().await?;
    hub.notify(job.thread_id, seq);
    Ok(())
}

pub use super::JobView;

pub async fn view(
    pool: &sqlx::PgPool,
    tenant: &Tenant,
    job_id: JobId,
) -> Result<JobView, KatError> {
    sqlx::query_as(include_str!("job_view.sql"))
        .bind(tenant.workspace_id())
        .bind(job_id)
        .fetch_optional(pool)
        .await?
        .ok_or(KatError::NotFound)
}
