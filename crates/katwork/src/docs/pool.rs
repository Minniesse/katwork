//! The bounded pool of dedicated document threads. A parse runs here, never on an async task or
//! `spawn_blocking`: a running blocking task cannot be stopped, and its queue has no bound.
use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

use super::{DocError, DocLimits, deadline, watchdog::WedgeSource};
use crate::ids::WorkspaceId;

const STACK_BYTES: usize = 64 << 20;

/// A queued job; it gets the index of the worker that picked it up.
type Job = Box<dyn FnOnce(usize) + Send>;

pub struct DocPool {
    shared: Arc<Shared>,
    jobs: mpsc::SyncSender<Job>,
}

struct Shared {
    limits: DocLimits,
    /// One permit per thread, so the pool never runs more jobs than threads.
    threads: Arc<Semaphore>,
    /// `max(1, workers − 1)` permits per workspace with a job running or waiting.
    workspaces: Mutex<HashMap<WorkspaceId, Arc<Semaphore>>>,
    per_workspace: usize,
    epoch: Instant,
    /// When each worker's current job started, in nanoseconds after `epoch` plus one; 0 is idle.
    started: Box<[AtomicU64]>,
}

/// A queued or running job's places: its workspace's and a thread's. Dropping it frees both,
/// whether `run` was dropped while waiting or the job ended on its thread.
struct Slot {
    shared: Arc<Shared>,
    workspace: WorkspaceId,
    workspace_permit: Option<OwnedSemaphorePermit>,
    thread_permit: Option<OwnedSemaphorePermit>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.thread_permit.take();
        self.workspace_permit.take();
        let mut workspaces = self
            .shared
            .workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Only the map holds a semaphore no job waits on or holds.
        if workspaces
            .get(&self.workspace)
            .is_some_and(|semaphore| Arc::strong_count(semaphore) == 1)
        {
            workspaces.remove(&self.workspace);
        }
    }
}

impl DocPool {
    /// Starts `workers` threads (at least one) with 64 MiB stacks; a thread that cannot be
    /// spawned is this function's error, so the server does not start without its pool.
    pub fn new(workers: usize, limits: DocLimits) -> Result<DocPool, std::io::Error> {
        let workers = workers.max(1);
        let (jobs, queue) = mpsc::sync_channel::<Job>(workers);
        let queue = Arc::new(Mutex::new(queue));
        let shared = Arc::new(Shared {
            limits,
            threads: Arc::new(Semaphore::new(workers)),
            workspaces: Mutex::new(HashMap::new()),
            per_workspace: workers.saturating_sub(1).max(1),
            epoch: Instant::now(),
            started: (0..workers).map(|_| AtomicU64::new(0)).collect(),
        });
        for index in 0..workers {
            let queue = queue.clone();
            std::thread::Builder::new()
                .name(format!("doc-worker-{index}"))
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    loop {
                        let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok(job) = job else {
                            return;
                        };
                        // A job catches its own panics; this keeps the thread if the wrapper
                        // ever panics.
                        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(index)));
                    }
                })?;
        }
        Ok(DocPool { shared, jobs })
    }

    pub fn limits(&self) -> &DocLimits {
        &self.shared.limits
    }

    /// Runs `job` on a pool thread. Waiting for a thread is cancel-safe: a cancel while queued
    /// starts nothing, and a `deadline` passed while queued is `Timeout`. Once a worker picks the
    /// job up it is raced against `started + call_timeout` (`CallTimeout`), `deadline`
    /// (`Timeout`) and `cancel` (`Cancelled`), and `run` returns at once on any of them while
    /// the job stops at its next `deadline::check`. Dropping this future cancels the job too.
    /// A panic in `job` is `Internal`.
    pub async fn run<T: Send + 'static>(
        &self,
        workspace: WorkspaceId,
        cancel: &CancellationToken,
        deadline: Option<tokio::time::Instant>,
        job: impl FnOnce(&DocLimits) -> Result<T, DocError> + Send + 'static,
    ) -> Result<T, DocError> {
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let expired = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(expired);
        let mut slot = Slot {
            shared: self.shared.clone(),
            workspace,
            workspace_permit: None,
            thread_permit: None,
        };
        let workspace_slots = self.shared.workspace_slots(workspace);
        tokio::select! {
            biased;
            () = token.cancelled() => return Err(DocError::Cancelled),
            () = &mut expired => return Err(DocError::Timeout),
            permit = workspace_slots.acquire_owned() => {
                slot.workspace_permit = Some(permit.map_err(|_| DocError::Internal)?);
            }
        }
        tokio::select! {
            biased;
            () = token.cancelled() => return Err(DocError::Cancelled),
            () = &mut expired => return Err(DocError::Timeout),
            permit = self.shared.threads.clone().acquire_owned() => {
                slot.thread_permit = Some(permit.map_err(|_| DocError::Internal)?);
            }
        }
        let (started_tx, started_rx) = oneshot::channel::<Instant>();
        let (result_tx, result_rx) = oneshot::channel::<Result<T, DocError>>();
        let shared = self.shared.clone();
        let caller_deadline = deadline.map(tokio::time::Instant::into_std);
        let job_token = token.clone();
        let queued: Job = Box::new(move |worker| {
            // `run` gave up while this job was queued: it never starts.
            if job_token.is_cancelled() {
                return;
            }
            let started = Instant::now();
            shared.started[worker].store(shared.mark(started), Ordering::Relaxed);
            let _ = started_tx.send(started);
            let call_deadline = started + shared.limits.call_timeout;
            let job_deadline = caller_deadline.map_or(call_deadline, |at| at.min(call_deadline));
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                deadline::scope(job_deadline, Some(job_token), || job(&shared.limits))
            }))
            .unwrap_or(Err(DocError::Internal));
            shared.started[worker].store(0, Ordering::Relaxed);
            drop(slot);
            let _ = result_tx.send(result);
        });
        // Each queued job holds a thread permit, so the channel (one place per thread) is never
        // full; it is closed only when every worker is gone.
        self.jobs.try_send(queued).map_err(|_| DocError::Internal)?;
        let started = tokio::select! {
            biased;
            () = token.cancelled() => return Err(DocError::Cancelled),
            () = &mut expired => return Err(DocError::Timeout),
            started = started_rx => started.map_err(|_| DocError::Internal)?,
        };
        let call_deadline = started + self.shared.limits.call_timeout;
        let (stop_at, stop) = match caller_deadline {
            Some(at) if at < call_deadline => (at, DocError::Timeout),
            _ => (call_deadline, DocError::CallTimeout),
        };
        tokio::select! {
            biased;
            // A job that saw its deadline pass (it checks the earlier of the two) stopped for the
            // same reason the timer below would have fired.
            result = result_rx => match result.unwrap_or(Err(DocError::Internal)) {
                Err(DocError::Timeout) => Err(stop),
                other => other,
            },
            () = token.cancelled() => Err(DocError::Cancelled),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(stop_at)) => Err(stop),
        }
    }

    /// When every thread has run one job for more than three call timeouts: the instant that
    /// became true.
    pub fn wedged_since(&self) -> Option<Instant> {
        let shared = &self.shared;
        let stuck_after = shared.limits.call_timeout.checked_mul(3)?;
        let now = Instant::now();
        let mut since: Option<Instant> = None;
        for started in &shared.started {
            let started = match started.load(Ordering::Relaxed) {
                0 => return None,
                mark => shared
                    .epoch
                    .checked_add(Duration::from_nanos(mark - 1))?
                    .checked_add(stuck_after)?,
            };
            if now <= started {
                return None;
            }
            since = Some(since.map_or(started, |since| since.max(started)));
        }
        since
    }

    pub fn wedged(&self) -> bool {
        self.wedged_since().is_some()
    }
}

impl Shared {
    fn workspace_slots(&self, workspace: WorkspaceId) -> Arc<Semaphore> {
        self.workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace)
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_workspace)))
            .clone()
    }

    fn mark(&self, started: Instant) -> u64 {
        let nanos = started.saturating_duration_since(self.epoch).as_nanos();
        u64::try_from(nanos).unwrap_or(u64::MAX - 1) + 1
    }
}

impl WedgeSource for DocPool {
    fn wedged_since(&self) -> Option<Instant> {
        DocPool::wedged_since(self)
    }
}

/// `KATWORK_SMOKE_WEDGE_ONCE`: blocks every pool thread forever, once per `marker` file, so the
/// smoke stack can watch the watchdog restart the container.
#[cfg(feature = "smoke-fixtures")]
pub mod smoke {
    use std::{path::Path, sync::Arc};

    use tokio_util::sync::CancellationToken;

    use super::DocPool;
    use crate::{docs::DocError, ids::WorkspaceId};

    /// Creates `marker` and blocks each thread with a job of its own workspace (one workspace
    /// cannot hold every thread). An empty path, or one that already exists, does nothing.
    pub fn wedge_once(pool: &Arc<DocPool>, marker: &Path) -> std::io::Result<bool> {
        if marker.as_os_str().is_empty() || marker.exists() {
            return Ok(false);
        }
        std::fs::File::create_new(marker)?;
        for _ in 0..pool.shared.started.len() {
            let pool = pool.clone();
            tokio::spawn(async move {
                let _ = pool
                    .run(
                        WorkspaceId::new(),
                        &CancellationToken::new(),
                        None,
                        |_| -> Result<(), DocError> {
                            loop {
                                std::thread::park();
                            }
                        },
                    )
                    .await;
            });
        }
        Ok(true)
    }
}
