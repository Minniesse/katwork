use std::time::Duration;

use sqlx::{PgConnection, PgPool, Postgres, Transaction, types::Json};

use super::{HarnessEnd, TaskInput, job};
use crate::{
    agent::{Limits, RunFacts, RunKind, StopReason},
    db,
    error::KatError,
    events::{JobStatusEvent, KatworkEvent, RunEnd, log, sink::close_locked_run},
    fence::{self, Fence},
    ids::*,
    runtime::Core,
    team::kat,
    tenancy::{self, Tenant},
    types::{HoldReason, ReasonCode},
};

#[derive(Debug)]
pub struct ClaimedTask {
    pub task_id: TaskId,
    pub job_id: JobId,
    pub kat_id: KatId,
    pub workspace_id: WorkspaceId,
    pub thread_id: ThreadId,
    pub requested_by: UserId,
    pub input: TaskInput,
    pub attempts: i32,
    pub lease_epoch: LeaseEpoch,
    pub policy_version: i64,
    pub kind: RunKind,
    pub remaining: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HeartbeatStatus {
    Continue,
    Stop(StopReason),
}

#[derive(sqlx::FromRow)]
struct Candidate {
    id: TaskId,
    workspace_id: WorkspaceId,
    job_id: JobId,
    kat_id: KatId,
    thread_id: ThreadId,
    space_id: SpaceId,
    requested_by: UserId,
    input: Json<serde_json::Value>,
    kat_policy_version: i64,
    attempts: i32,
    max_attempts: i32,
    effects_started: bool,
    hold_reason: Option<String>,
}

fn micros(duration: Duration) -> Result<i64, KatError> {
    i64::try_from(duration.as_micros()).map_err(|_| KatError::RequestInvalid)
}
fn owner(worker_id: &str) -> Result<(), KatError> {
    if worker_id.is_empty() || worker_id.contains('\0') {
        return Err(KatError::RequestInvalid);
    }
    Ok(())
}
fn ttl_micros(ttl: Duration) -> Result<i64, KatError> {
    let ttl = micros(ttl)?;
    if ttl == 0 {
        return Err(KatError::RequestInvalid);
    }
    Ok(ttl)
}

/// Oldest first, best effort: SKIP LOCKED never waits for an earlier job/task.
/// Each candidate owns a transaction; holds must not starve later candidates in this poll.
pub async fn claim(
    core: &Core,
    worker_id: &str,
    ttl: Duration,
    limits: &Limits,
) -> Result<Option<ClaimedTask>, KatError> {
    owner(worker_id)?;
    let ttl = ttl_micros(ttl)?;
    let task_deadline = micros(limits.task_deadline)?;
    let job_deadline = micros(limits.job_deadline)?;
    let mut visited: Vec<TaskId> = Vec::new();
    // Bound each poll while allowing bursts of new holds to reveal later workspaces.
    for _ in 0..256 {
        let mut tx = core.pool.begin().await?;
        db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
        // Advisory pruning keeps persistent holds out of the candidate budget.
        // Expired leases and terminal conditions always reach the locked checks below.
        let candidate: Option<Candidate> = sqlx::query_as(
            "SELECT t.id,t.workspace_id,t.job_id,t.kat_id,j.thread_id,th.space_id,
             j.requested_by,t.input,t.kat_policy_version,t.attempts,t.max_attempts,
             t.effects_started_at IS NOT NULL AS effects_started,t.hold_reason
             FROM tasks t JOIN jobs j ON j.id=t.job_id AND j.workspace_id=t.workspace_id
             JOIN threads th ON th.id=j.thread_id AND th.workspace_id=j.workspace_id
             LEFT JOIN task_leases l ON l.task_id=t.id
             JOIN workspaces w ON w.id=t.workspace_id
             JOIN kats k ON k.id=t.kat_id AND k.workspace_id=t.workspace_id
             WHERE j.status IS NULL AND NOT (t.id=ANY($1))
             AND (t.state='queued' OR (t.state='leased' AND l.expires_at<=clock_timestamp()))
             AND NOT (t.state='queued' AND t.hold_reason IS NOT NULL
                 AND t.attempts<t.max_attempts
                 AND (t.first_claimed_at IS NULL OR t.first_claimed_at+$2*interval '1 microsecond'>clock_timestamp())
                 AND j.created_at+$3*interval '1 microsecond'>clock_timestamp()
                 AND ((t.hold_reason='workspace_paused' AND w.paused)
                     OR (t.hold_reason='kat_busy' AND NOT w.paused AND k.status='active'
                         AND k.policy_version=t.kat_policy_version
                         AND EXISTS (SELECT 1 FROM kat_spaces s WHERE s.workspace_id=t.workspace_id
                             AND s.kat_id=t.kat_id AND s.space_id=th.space_id)
                         AND (SELECT count(*) FROM tasks busy JOIN task_leases live ON live.task_id=busy.id
                             WHERE busy.workspace_id=t.workspace_id AND busy.kat_id=t.kat_id
                             AND busy.state='leased' AND live.expires_at>clock_timestamp())>=k.max_concurrency)))
             ORDER BY t.created_at,t.id LIMIT 1 FOR NO KEY UPDATE OF j,t SKIP LOCKED",
        )
        .bind(&visited)
        .bind(task_deadline)
        .bind(job_deadline)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(c) = candidate else { return Ok(None) };
        visited.push(c.id);
        let result = claim_locked(&mut tx, &c, worker_id, ttl, task_deadline, job_deadline).await;
        match result {
            Ok((claimed, seq)) => match commit(core, tx, c.thread_id, seq).await {
                Ok(()) if claimed.is_some() => return Ok(claimed),
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(code = error.code(), task_id = %c.id, "worker_candidate_commit_failed")
                }
            },
            Err(error) => {
                tx.rollback().await?;
                tracing::warn!(code = error.code(), task_id = %c.id, "worker_candidate_failed");
            }
        }
    }
    Ok(None)
}

async fn claim_locked(
    tx: &mut Transaction<'_, Postgres>,
    c: &Candidate,
    worker_id: &str,
    ttl: i64,
    task_deadline: i64,
    job_deadline: i64,
) -> Result<(Option<ClaimedTask>, Option<EventSeq>), KatError> {
    // A renewal may have committed after the candidate snapshot without changing its task.
    let eligible: bool = sqlx::query_scalar(
        "SELECT t.state='queued' OR (t.state='leased' AND l.expires_at<=clock_timestamp())
         FROM tasks t LEFT JOIN task_leases l ON l.task_id=t.id
         WHERE t.workspace_id=$1 AND t.id=$2",
    )
    .bind(c.workspace_id)
    .bind(c.id)
    .fetch_one(&mut **tx)
    .await?;
    if !eligible {
        return Ok((None, None));
    }
    lock_lease(tx, c.id).await?;
    let runs: Vec<RunId> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE workspace_id=$1 AND task_id=$2 AND state='running'
         ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(c.workspace_id)
    .bind(c.id)
    .fetch_all(&mut **tx)
    .await?;
    let calls: Vec<(ToolCallRecordId, RunId)> = sqlx::query_as(
        "SELECT id,run_id FROM tool_calls WHERE workspace_id=$1 AND run_id=ANY($2)
         AND ended_at IS NULL ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(c.workspace_id)
    .bind(&runs)
    .fetch_all(&mut **tx)
    .await?;
    lock_thread(tx, c.workspace_id, c.thread_id).await?;
    sqlx::query("SELECT id FROM kats WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(c.workspace_id)
        .bind(c.kat_id)
        .fetch_one(&mut **tx)
        .await?;
    let tenant = tenancy::for_task(&mut **tx, c.id).await?;
    let was_leased: bool =
        sqlx::query_scalar("SELECT state='leased' FROM tasks WHERE workspace_id=$1 AND id=$2")
            .bind(c.workspace_id)
            .bind(c.id)
            .fetch_one(&mut **tx)
            .await?;
    let mut seq = None;
    if was_leased {
        for run in runs {
            let open = calls
                .iter()
                .filter(|(_, r)| *r == run)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            seq = Some(
                close_locked_run(
                    tx,
                    c.workspace_id,
                    c.thread_id,
                    run,
                    &open,
                    RunEnd::Interrupted,
                    "lease_lost",
                )
                .await?,
            );
        }
        release(tx, c.id).await?;
        // The committed effect marker, never a missing receipt, controls replay safety.
        if !c.effects_started {
            sqlx::query("UPDATE tasks SET state='queued' WHERE workspace_id=$1 AND id=$2")
                .bind(c.workspace_id)
                .bind(c.id)
                .execute(&mut **tx)
                .await?;
        }
    }
    let expired: bool = sqlx::query_scalar(
        "SELECT (t.first_claimed_at IS NOT NULL AND
             t.first_claimed_at+$1*interval '1 microsecond'<=clock_timestamp()) OR
             j.created_at+$2*interval '1 microsecond'<=clock_timestamp()
         FROM tasks t JOIN jobs j ON j.id=t.job_id AND j.workspace_id=t.workspace_id
         WHERE t.workspace_id=$3 AND t.id=$4",
    )
    .bind(task_deadline)
    .bind(job_deadline)
    .bind(c.workspace_id)
    .bind(c.id)
    .fetch_one(&mut **tx)
    .await?;
    let input = serde_json::from_value::<TaskInput>(c.input.0.clone());
    let stop =
        if expired {
            Some(StopReason::TimeLimit)
        } else if c.attempts >= c.max_attempts || input.is_err() {
            None
        } else if was_leased && c.effects_started {
            Some(StopReason::Interrupted)
        } else {
            let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id=$1")
                .bind(c.workspace_id)
                .fetch_one(&mut **tx)
                .await?;
            if paused {
                if let Some(s) = hold(tx, c, HoldReason::WorkspacePaused).await? {
                    seq = Some(s);
                }
                return Ok((None, seq));
            }
            if !kat::is_granted(tx, &tenant, c.kat_id, c.space_id).await? {
                Some(StopReason::PolicyChanged)
            } else if !kat::is_eligible(tx, &tenant, c.kat_id, c.space_id).await? {
                Some(StopReason::KatPaused)
            } else {
                let kat = kat::get_on(&mut **tx, &tenant, c.kat_id).await?;
                // Read capacity before usage so a finishing run cannot free a slot after a stale budget check.
                let live: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM tasks t JOIN task_leases l ON l.task_id=t.id
                 WHERE t.workspace_id=$1 AND t.kat_id=$2 AND t.id<>$3
                 AND t.state='leased' AND l.expires_at>clock_timestamp()",
                )
                .bind(c.workspace_id)
                .bind(c.kat_id)
                .bind(c.id)
                .fetch_one(&mut **tx)
                .await?;
                if kat.policy_version != c.kat_policy_version {
                    Some(StopReason::PolicyChanged)
                } else if super::quota::daily_exhausted(tx, c.workspace_id, c.kat_id).await? {
                    Some(StopReason::QuotaExceeded)
                } else {
                    if live >= i64::from(kat.max_concurrency) {
                        if let Some(s) = hold(tx, c, HoldReason::KatBusy).await? {
                            seq = Some(s);
                        }
                        return Ok((None, seq));
                    }
                    let (attempts, epoch): (i32, LeaseEpoch) = sqlx::query_as(
                    "UPDATE tasks SET state='leased',attempts=attempts+1,lease_epoch=lease_epoch+1,
                     hold_reason=NULL,first_claimed_at=COALESCE(first_claimed_at,clock_timestamp())
                     WHERE workspace_id=$1 AND id=$2 RETURNING attempts,lease_epoch",
                ).bind(c.workspace_id).bind(c.id).fetch_one(&mut **tx).await?;
                    sqlx::query(
                        "INSERT INTO task_leases(task_id,worker_id,lease_epoch,expires_at)
                     VALUES($1,$2,$3,clock_timestamp()+$4*interval '1 microsecond')",
                    )
                    .bind(c.id)
                    .bind(worker_id)
                    .bind(epoch)
                    .bind(ttl)
                    .execute(&mut **tx)
                    .await?;
                    let remaining: i64 = sqlx::query_scalar(include_str!("remaining.sql"))
                        .bind(task_deadline)
                        .bind(job_deadline)
                        .bind(c.workspace_id)
                        .bind(c.id)
                        .fetch_one(&mut **tx)
                        .await?;
                    return Ok((
                        Some(ClaimedTask {
                            task_id: c.id,
                            job_id: c.job_id,
                            kat_id: c.kat_id,
                            workspace_id: c.workspace_id,
                            thread_id: c.thread_id,
                            requested_by: c.requested_by,
                            input: input.map_err(|_| KatError::RequestInvalid)?,
                            attempts,
                            lease_epoch: epoch,
                            policy_version: c.kat_policy_version,
                            kind: if c.attempts > 0 {
                                RunKind::Recovery
                            } else {
                                RunKind::Initial
                            },
                            remaining: Duration::from_micros(u64::try_from(remaining).unwrap_or(0)),
                        }),
                        seq,
                    ));
                }
            }
        };
    let state = if stop == Some(StopReason::Interrupted) {
        "interrupted"
    } else {
        "failed"
    };
    sqlx::query("UPDATE tasks SET state=$3,hold_reason=NULL WHERE workspace_id=$1 AND id=$2")
        .bind(c.workspace_id)
        .bind(c.id)
        .bind(state)
        .execute(&mut **tx)
        .await?;
    release(tx, c.id).await?;
    seq = Some(match stop {
        Some(stop) => {
            job::conclude(
                tx,
                &tenant,
                c.job_id,
                c.thread_id,
                Some(c.kat_id),
                &c.id.to_string(),
                &RunFacts::not_run(stop),
            )
            .await?
        }
        None => {
            job::finish_by_harness(
                tx,
                c.job_id,
                HarnessEnd::Error {
                    code: if input.is_err() {
                        ReasonCode::InternalError
                    } else {
                        ReasonCode::RetriesExhausted
                    },
                },
            )
            .await?
        }
    });
    Ok::<_, KatError>((None, seq))
}

async fn hold(
    tx: &mut Transaction<'_, Postgres>,
    c: &Candidate,
    reason: HoldReason,
) -> Result<Option<EventSeq>, KatError> {
    if c.hold_reason.as_deref() == Some(reason.wire()) {
        return Ok(None);
    }
    sqlx::query("UPDATE tasks SET hold_reason=$3 WHERE workspace_id=$1 AND id=$2")
        .bind(c.workspace_id)
        .bind(c.id)
        .bind(reason.wire())
        .execute(&mut **tx)
        .await?;
    Ok(Some(
        log::append(
            tx,
            c.workspace_id,
            c.thread_id,
            None,
            &KatworkEvent::JobStatus(JobStatusEvent {
                job_id: c.job_id,
                status: None,
                reason: None,
                hold_reason: Some(reason),
            })
            .into(),
        )
        .await?,
    ))
}
async fn commit(
    core: &Core,
    tx: Transaction<'_, Postgres>,
    thread: ThreadId,
    seq: Option<EventSeq>,
) -> Result<(), KatError> {
    tx.commit().await?;
    if let Some(seq) = seq {
        core.events.notify(thread, seq);
    }
    Ok(())
}
async fn lock_lease(conn: &mut PgConnection, task: TaskId) -> Result<(), KatError> {
    sqlx::query("SELECT task_id FROM task_leases WHERE task_id=$1 FOR NO KEY UPDATE")
        .bind(task)
        .fetch_optional(conn)
        .await?;
    Ok(())
}
async fn lock_thread(
    conn: &mut PgConnection,
    workspace: WorkspaceId,
    thread: ThreadId,
) -> Result<(), KatError> {
    sqlx::query("SELECT id FROM threads WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(thread)
        .fetch_one(conn)
        .await?;
    Ok(())
}
async fn release(conn: &mut PgConnection, task: TaskId) -> Result<(), KatError> {
    sqlx::query("DELETE FROM task_leases WHERE task_id=$1")
        .bind(task)
        .execute(conn)
        .await?;
    Ok(())
}
async fn check_owner(conn: &mut PgConnection, task: TaskId, worker: &str) -> Result<(), KatError> {
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_leases WHERE task_id=$1 AND worker_id=$2)",
    )
    .bind(task)
    .bind(worker)
    .fetch_one(conn)
    .await?;
    if !owned {
        return Err(KatError::LeaseLost);
    }
    Ok(())
}

pub async fn heartbeat(
    pool: &PgPool,
    task_id: TaskId,
    worker_id: &str,
    epoch: LeaseEpoch,
    ttl: Duration,
) -> Result<HeartbeatStatus, KatError> {
    owner(worker_id)?;
    let ttl = ttl_micros(ttl)?;
    let mut tx = pool.begin().await?;
    db::set_configured_lock_timeout(&mut tx).await?;
    let fence = Fence { task_id, epoch };
    fence::check(&mut tx, &fence).await?;
    lock_lease(&mut tx, task_id).await?;
    // Two heartbeats can share the task lock; expiry must be checked after the lease lock too.
    fence::check(&mut tx, &fence).await?;
    check_owner(&mut tx, task_id, worker_id).await?;
    let paused: bool = sqlx::query_scalar(
        "SELECT w.paused FROM workspaces w JOIN tasks t ON t.workspace_id=w.id WHERE t.id=$1",
    )
    .bind(task_id)
    .fetch_one(&mut *tx)
    .await?;
    // The policy read can itself wait (for example behind DDL); never revive an expired lease.
    let renewed=sqlx::query("UPDATE task_leases SET expires_at=clock_timestamp()+$2*interval '1 microsecond' WHERE task_id=$1 AND expires_at>clock_timestamp()")
        .bind(task_id).bind(ttl).execute(&mut *tx).await?;
    if renewed.rows_affected() != 1 {
        return Err(KatError::LeaseLost);
    }
    tx.commit().await?;
    Ok(if paused {
        HeartbeatStatus::Stop(StopReason::WorkspacePaused)
    } else {
        HeartbeatStatus::Continue
    })
}

pub async fn complete(
    core: &Core,
    task_id: TaskId,
    worker_id: &str,
    epoch: LeaseEpoch,
    facts: RunFacts,
) -> Result<(), KatError> {
    owner(worker_id)?;
    let mut tx = core.pool.begin().await?;
    db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
    let (workspace, job_id, thread, kat_id): (WorkspaceId, JobId, ThreadId, KatId) =
        sqlx::query_as(
            "SELECT t.workspace_id,t.job_id,j.thread_id,t.kat_id
         FROM tasks t JOIN jobs j ON j.id=t.job_id AND j.workspace_id=t.workspace_id WHERE t.id=$1",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KatError::LeaseLost)?;
    sqlx::query("SELECT id FROM jobs WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(job_id)
        .fetch_one(&mut *tx)
        .await?;
    let fence = Fence { task_id, epoch };
    fence::lock(&mut tx, &fence).await?;
    lock_lease(&mut tx, task_id).await?;
    check_owner(&mut tx, task_id, worker_id).await?;
    if let Some(run) = facts.run_id {
        sqlx::query("SELECT id FROM runs WHERE workspace_id=$1 AND id=$2 AND task_id=$3 AND lease_epoch=$4 FOR NO KEY UPDATE")
            .bind(workspace).bind(run).bind(task_id).bind(epoch).fetch_optional(&mut *tx).await?.ok_or(KatError::RequestInvalid)?;
    }
    lock_thread(&mut tx, workspace, thread).await?;
    fence::lock(&mut tx, &fence).await?;
    let tenant: Tenant = tenancy::for_task(&mut *tx, task_id).await?;
    let seq = job::conclude(
        &mut tx,
        &tenant,
        job_id,
        thread,
        Some(kat_id),
        &task_id.to_string(),
        &facts,
    )
    .await?;
    sqlx::query("UPDATE tasks SET state=$3,pending_tool_call=$4,hold_reason=NULL WHERE workspace_id=$1 AND id=$2")
        .bind(workspace).bind(task_id).bind(if facts.stop==Some(StopReason::WorkspacePaused){"interrupted"}else{"done"})
        .bind(facts.pending_tool_call.map(Json)).execute(&mut *tx).await?;
    release(&mut tx, task_id).await?;
    commit(core, tx, thread, Some(seq)).await
}

/// Invalidate this owner immediately; the next poll applies the single recovery path.
pub async fn abandon(
    core: &Core,
    task_id: TaskId,
    worker_id: &str,
    epoch: LeaseEpoch,
) -> Result<(), KatError> {
    owner(worker_id)?;
    let mut tx = core.pool.begin().await?;
    db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
    let fence = Fence { task_id, epoch };
    fence::lock(&mut tx, &fence).await?;
    lock_lease(&mut tx, task_id).await?;
    fence::lock(&mut tx, &fence).await?;
    check_owner(&mut tx, task_id, worker_id).await?;
    sqlx::query("UPDATE tasks SET lease_epoch=lease_epoch+1 WHERE id=$1")
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE task_leases SET expires_at=clock_timestamp() WHERE task_id=$1")
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Contain a run panic; committed effects require review before another attempt.
pub(crate) async fn fail(
    core: &Core,
    task: TaskId,
    worker: &str,
    epoch: LeaseEpoch,
) -> Result<(), KatError> {
    let mut tx = core.pool.begin().await?;
    db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
    let (workspace, job, thread): (WorkspaceId, JobId, ThreadId) = sqlx::query_as(
        "SELECT t.workspace_id,t.job_id,j.thread_id FROM tasks t JOIN jobs j
         ON j.id=t.job_id AND j.workspace_id=t.workspace_id WHERE t.id=$1",
    )
    .bind(task)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("SELECT id FROM jobs WHERE workspace_id=$1 AND id=$2 FOR NO KEY UPDATE")
        .bind(workspace)
        .bind(job)
        .fetch_one(&mut *tx)
        .await?;
    let fence = Fence {
        task_id: task,
        epoch,
    };
    fence::lock(&mut tx, &fence).await?;
    lock_lease(&mut tx, task).await?;
    check_owner(&mut tx, task, worker).await?;
    let runs: Vec<RunId> = sqlx::query_scalar("SELECT id FROM runs WHERE workspace_id=$1 AND task_id=$2 AND state='running' ORDER BY id FOR NO KEY UPDATE")
        .bind(workspace).bind(task).fetch_all(&mut *tx).await?;
    let calls: Vec<(ToolCallRecordId, RunId)> = sqlx::query_as("SELECT id,run_id FROM tool_calls WHERE workspace_id=$1 AND run_id=ANY($2) AND ended_at IS NULL ORDER BY id FOR NO KEY UPDATE")
        .bind(workspace).bind(&runs).fetch_all(&mut *tx).await?;
    lock_thread(&mut tx, workspace, thread).await?;
    fence::lock(&mut tx, &fence).await?;
    let (kat, effects_started): (KatId, bool) = sqlx::query_as(
        "SELECT kat_id,effects_started_at IS NOT NULL FROM tasks WHERE workspace_id=$1 AND id=$2",
    )
    .bind(workspace)
    .bind(task)
    .fetch_one(&mut *tx)
    .await?;
    for run in runs {
        let open = calls
            .iter()
            .filter(|(_, r)| *r == run)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        close_locked_run(
            &mut tx,
            workspace,
            thread,
            run,
            &open,
            RunEnd::Interrupted,
            "internal_error",
        )
        .await?;
    }
    sqlx::query("UPDATE tasks SET state=$3,hold_reason=NULL WHERE workspace_id=$1 AND id=$2")
        .bind(workspace)
        .bind(task)
        .bind(if effects_started {
            "interrupted"
        } else {
            "failed"
        })
        .execute(&mut *tx)
        .await?;
    release(&mut tx, task).await?;
    let seq = if effects_started {
        let tenant = tenancy::for_task(&mut *tx, task).await?;
        job::conclude(
            &mut tx,
            &tenant,
            job,
            thread,
            Some(kat),
            &task.to_string(),
            &RunFacts::not_run(StopReason::Interrupted),
        )
        .await?
    } else {
        job::finish_by_harness(
            &mut tx,
            job,
            HarnessEnd::Error {
                code: ReasonCode::InternalError,
            },
        )
        .await?
    };
    commit(core, tx, thread, Some(seq)).await
}
