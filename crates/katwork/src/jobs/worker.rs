use std::{sync::Arc, time::Duration};

use tokio::{
    task::JoinSet,
    time::{Instant, sleep, sleep_until, timeout_at},
};
use tokio_util::sync::CancellationToken;

use super::lease::{self, ClaimedTask, HeartbeatStatus};
use crate::{
    agent::{Limits, RunAbort, RunEnv, RunFacts, RunInput, StopReason, ToolRegistry, run},
    chat::thread,
    events::PgEventSink,
    fence::Fence,
    ids::{MessageId, RunId},
    pipeline,
    runtime::Core,
    team::kat,
    tenancy::{self, space},
};

pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

pub fn run_deadline(claimed_at: Instant, remaining: Duration) -> Instant {
    claimed_at + remaining
}

/// Cancel Core::shutdown and drain the returned set to stop admission and abandon active work.
/// Dropping the set aborts workers; their leases remain recoverable after expiry.
pub fn spawn_pool(core: Arc<Core>, n: usize, ttl: Duration) -> JoinSet<()> {
    let mut workers = JoinSet::new();
    // A sub-microsecond TTL cannot produce a usable PostgreSQL lease or heartbeat interval.
    if ttl.as_micros() == 0 || i64::try_from(ttl.as_micros()).is_err() {
        tracing::error!(code = "request_invalid", "worker_pool_invalid_ttl");
        return workers;
    }
    for _ in 0..n {
        let core = core.clone();
        workers.spawn(async move {
            let worker_id = uuid::Uuid::now_v7().to_string();
            let limits = Limits::default();
            loop {
                let claimed = tokio::select! {
                    biased;
                    ()=core.shutdown.cancelled()=>break,
                    result=lease::claim(&core,&worker_id,ttl,&limits)=>result,
                };
                match claimed {
                    Ok(Some(c)) => {
                        let claimed_at = Instant::now();
                        work(&core, &worker_id, ttl, c, claimed_at, &limits).await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(code = error.code(), "worker_claim_failed"),
                }
                if core.shutdown.is_cancelled() {
                    break;
                }
                tokio::select! {
                    ()=core.shutdown.cancelled()=>break,
                    ()=sleep(Duration::from_secs(1))=>{},
                }
            }
        });
    }
    workers
}

enum Renewal {
    Stop(StopReason, Instant),
    Lost,
}

async fn renew(
    core: &Core,
    c: &ClaimedTask,
    worker: &str,
    ttl: Duration,
    claimed_at: Instant,
) -> Renewal {
    // Start each renewal's safety window before its SQL, so database latency cannot extend it.
    let mut valid_until = claimed_at + ttl;
    let mut next = claimed_at + ttl / 3;
    loop {
        tokio::select! {
            biased;
            ()=sleep_until(valid_until)=>return Renewal::Lost,
            ()=sleep_until(next)=>{},
        }
        let started = Instant::now();
        match timeout_at(
            valid_until,
            lease::heartbeat(&core.pool, c.task_id, worker, c.lease_epoch, ttl),
        )
        .await
        {
            Ok(Ok(HeartbeatStatus::Continue)) => {}
            Ok(Ok(HeartbeatStatus::Stop(reason))) => return Renewal::Stop(reason, started + ttl),
            Ok(Err(error)) => {
                tracing::warn!(code = error.code(), "worker_renewal_failed");
                return Renewal::Lost;
            }
            Err(_) => return Renewal::Lost,
        }
        valid_until = started + ttl;
        next = started + ttl / 3;
    }
}

async fn work(
    core: &Core,
    worker: &str,
    ttl: Duration,
    c: ClaimedTask,
    claimed_at: Instant,
    limits: &Limits,
) {
    let cancel = CancellationToken::new();
    let result = {
        let execution = execute(core, &c, claimed_at, limits, cancel.clone());
        tokio::pin!(execution);
        // Renewal must keep being polled while execution owns SQL locks, and vice versa.
        tokio::select! {
            biased;
            ()=core.shutdown.cancelled()=>{
                cancel.cancel();
                None
            }
            renewal=renew(core,&c,worker,ttl,claimed_at)=>{
                cancel.cancel();
                match renewal {
                    Renewal::Lost=>return,
                    Renewal::Stop(reason,until)=>match timeout_at(until,&mut execution).await {
                        Ok(Ok(mut facts))=>{facts.stop=Some(reason);Some(Ok(facts))},
                        Ok(Err(error))=>Some(Err(error)),
                        Err(_)=>return,
                    }
                }
            }
            result=&mut execution=>Some(result),
        }
    };
    match result {
        Some(Ok(facts)) => {
            if let Err(error) = lease::complete(core, c.task_id, worker, c.lease_epoch, facts).await
            {
                tracing::warn!(code = error.code(), "worker_completion_failed");
                if !matches!(error, crate::error::KatError::LeaseLost) {
                    abandon(core, &c, worker).await;
                }
            }
        }
        Some(Err(RunAbort::LeaseLost)) => {}
        Some(Err(RunAbort::Infrastructure { code, .. })) => {
            tracing::warn!(code, "worker_run_aborted");
            abandon(core, &c, worker).await;
        }
        None => abandon(core, &c, worker).await,
    }
}

async fn abandon(core: &Core, c: &ClaimedTask, worker: &str) {
    if let Err(error) = lease::abandon(core, c.task_id, worker, c.lease_epoch).await {
        tracing::warn!(code = error.code(), "worker_abandon_failed");
    }
}

async fn execute(
    core: &Core,
    c: &ClaimedTask,
    claimed_at: Instant,
    limits: &Limits,
    cancel: CancellationToken,
) -> Result<RunFacts, RunAbort> {
    let prepare = async {
        let tenant = tenancy::resolve(&core.pool, c.requested_by, c.workspace_id).await?;
        let kat = kat::get(&core.pool, &tenant, c.kat_id).await?;
        let thread = thread::get(&core.pool, &tenant, c.thread_id).await?;
        let space = space::get(&core.pool, &tenant, thread.space_id).await?;
        let history = pipeline::load_history(
            core,
            &tenant,
            c.thread_id,
            c.input.request_message_id,
            c.kat_id,
            24_000,
        )
        .await?;
        Ok::<_, crate::error::KatError>((tenant, kat, thread, space, history))
    };
    let (tenant, kat, thread, space, history) =
        prepare.await.map_err(|error| RunAbort::Infrastructure {
            run_id: None,
            code: error.code(),
        })?;
    let run_id = RunId::new();
    let sink = PgEventSink::new(
        core.pool.clone(),
        core.events.clone(),
        core.config.lock_timeout,
        c.workspace_id,
        c.thread_id,
        Some(run_id),
        Some(Fence {
            task_id: c.task_id,
            epoch: c.lease_epoch,
        }),
    );
    let tools = ToolRegistry::builtin();
    run::execute(
        &RunEnv {
            pool: &core.pool,
            models: &core.registry,
            tools: &tools,
            sink: &sink,
        },
        &kat,
        RunInput {
            tenant,
            run_id,
            thread_id: c.thread_id,
            space_id: thread.space_id,
            job_id: c.job_id,
            task_id: c.task_id,
            lease_epoch: c.lease_epoch,
            kind: c.kind,
            space_policy: space.policy,
            history: history.messages,
            reply_message_id: MessageId::new(),
            policy_version: c.policy_version,
            actor: c.input.actor,
            initiator: c.input.initiator,
            deadline: run_deadline(claimed_at, c.remaining),
            cancel,
        },
        limits,
    )
    .await
}
