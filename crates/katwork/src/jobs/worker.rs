use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::{
    sync::watch,
    task::JoinSet,
    time::{Instant, sleep, sleep_until},
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
            while !core.shutdown.is_cancelled() {
                let claim_started = Instant::now();
                // A cancelled BEGIN can poison the pool; always drain admission SQL.
                match lease::claim(&core, &worker_id, ttl, &limits).await {
                    Ok(Some(c)) => {
                        if core.shutdown.is_cancelled() {
                            abandon(&core, &c, &worker_id).await;
                            break;
                        }
                        let claimed_at = Instant::now();
                        work(
                            &core,
                            &worker_id,
                            ttl,
                            c,
                            claim_started,
                            claimed_at,
                            &limits,
                        )
                        .await;
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(code = error.code(), "worker_claim_failed"),
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
    Stop(StopReason),
    Lost,
}

async fn renew(
    core: &Core,
    c: &ClaimedTask,
    worker: &str,
    ttl: Duration,
    window: watch::Sender<Instant>,
    stop: CancellationToken,
) -> Renewal {
    let mut next = *window.borrow() - ttl + ttl / 3;
    loop {
        tokio::select! {
            biased;
            ()=stop.cancelled()=>return Renewal::Lost,
            ()=sleep_until(next)=>{},
        }
        // The watchdog cancels execution independently while this SQL drains to completion.
        let started = Instant::now();
        match lease::heartbeat(&core.pool, c.task_id, worker, c.lease_epoch, ttl).await {
            Ok(HeartbeatStatus::Continue) => {}
            Ok(HeartbeatStatus::Stop(reason)) => return Renewal::Stop(reason),
            Err(error) => {
                tracing::warn!(code = error.code(), "worker_renewal_failed");
                return Renewal::Lost;
            }
        }
        let _ = window.send(started + ttl);
        next = started + ttl / 3;
    }
}

async fn expired(mut window: watch::Receiver<Instant>) {
    loop {
        let until = *window.borrow_and_update();
        tokio::select! {
            biased;
            ()=sleep_until(until)=>return,
            changed=window.changed()=>if changed.is_err() { return; },
        }
    }
}

async fn work(
    core: &Core,
    worker: &str,
    ttl: Duration,
    c: ClaimedTask,
    claim_started: Instant,
    claimed_at: Instant,
    limits: &Limits,
) {
    let cancel = CancellationToken::new();
    let stop_renewal = CancellationToken::new();
    // Even the first window starts before SQL; commit latency must not extend authority.
    let (window, watchdog) = watch::channel(claim_started + ttl);
    let execution =
        AssertUnwindSafe(execute(core, &c, claimed_at, limits, cancel.clone())).catch_unwind();
    let renewal = renew(core, &c, worker, ttl, window, stop_renewal.clone());
    tokio::pin!(execution, renewal);
    let mut renewal_done = false;
    let mut lost = false;
    let mut stop_reason = None;
    let result = tokio::select! {
        biased;
        result=&mut execution=>Some(result),
        ()=core.shutdown.cancelled()=>None,
        renewal=&mut renewal=>{
            renewal_done = true;
            match renewal {
                Renewal::Lost=>lost=true,
                Renewal::Stop(reason)=>stop_reason=Some(reason),
            }
            None
        },
        ()=expired(watchdog)=>{lost=true;None},
    };
    if result.is_none() {
        cancel.cancel();
    }
    stop_renewal.cancel();
    // No branch drops execution or heartbeat SQL, including on watchdog expiry and shutdown.
    let (result, (), ()) = tokio::join!(
        async {
            match result {
                Some(result) => result,
                None => execution.await,
            }
        },
        async {
            if !renewal_done {
                renewal.await;
            }
        },
        async {
            if lost {
                abandon(core, &c, worker).await;
            }
        },
    );
    if lost {
        return;
    }
    match result {
        Ok(Ok(mut facts)) => {
            if let Some(reason) = stop_reason {
                facts.stop = Some(reason);
            }
            if facts.stop == Some(StopReason::Cancelled) && core.shutdown.is_cancelled() {
                abandon(core, &c, worker).await;
                return;
            }
            if let Err(error) = lease::complete(core, c.task_id, worker, c.lease_epoch, facts).await
            {
                tracing::warn!(code = error.code(), "worker_completion_failed");
                if !matches!(error, crate::error::KatError::LeaseLost) {
                    abandon(core, &c, worker).await;
                }
            }
        }
        Ok(Err(RunAbort::LeaseLost)) => {}
        Ok(Err(RunAbort::Infrastructure { code, .. })) => {
            tracing::warn!(code, "worker_run_aborted");
            abandon(core, &c, worker).await;
        }
        Err(_) => {
            tracing::error!(code = "internal_error", "worker_run_panicked");
            if let Err(error) = lease::fail(core, c.task_id, worker, c.lease_epoch).await {
                tracing::warn!(code = error.code(), "worker_failure_cleanup_failed");
                abandon(core, &c, worker).await;
            }
        }
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
        let tenant = tenancy::for_task(&core.pool, c.task_id).await?;
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
    run::execute_with_shutdown(
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
        Some(&core.shutdown),
    )
    .await
}
