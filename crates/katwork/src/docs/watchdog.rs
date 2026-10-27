//! Exits the process when the document pool stays wedged, so Compose's `restart: unless-stopped`
//! starts a fresh one: Docker never restarts a container that is only unhealthy. The leases the
//! process held expire and are reclaimed, and Postgres rolls back its open transactions.
use std::{
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The process exit code when the pool stayed wedged.
pub const EXIT_CODE: i32 = 70;

pub trait WedgeSource: Send + Sync {
    /// When every pool thread came to have run one job for more than three call timeouts.
    fn wedged_since(&self) -> Option<Instant>;
}

/// True exactly when the pool is wedged and has been for at least `after`.
pub fn should_exit(wedged_since: Option<Instant>, now: Instant, after: Duration) -> bool {
    wedged_since.is_some_and(|since| now.saturating_duration_since(since) >= after)
}

pub struct Watchdog {
    source: Arc<dyn WedgeSource>,
    after: Duration,
    exit: Arc<dyn Fn(i32) + Send + Sync>,
    fired: bool,
}

impl Watchdog {
    pub fn new(
        source: Arc<dyn WedgeSource>,
        after: Duration,
        exit: Arc<dyn Fn(i32) + Send + Sync>,
    ) -> Watchdog {
        Watchdog {
            source,
            after,
            exit,
            fired: false,
        }
    }

    /// Logs `doc_pool_wedged_exit` and calls `exit(70)` once the wedge has lasted `after`, then
    /// returns true; every later tick does nothing.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.fired {
            return false;
        }
        let since = self.source.wedged_since();
        if !should_exit(since, now, self.after) {
            return false;
        }
        self.fired = true;
        let wedged_ms = since.map_or(0, |since| {
            u64::try_from(now.saturating_duration_since(since).as_millis()).unwrap_or(u64::MAX)
        });
        tracing::error!(wedged_ms, "doc_pool_wedged_exit");
        (self.exit)(EXIT_CODE);
        true
    }
}

/// Ticks `watchdog` every `period` on its own thread, which returns after the first tick that
/// fired (in the server that tick never returns: it exits the process). A thread that cannot be
/// spawned is this function's error, so the server does not start without its watchdog.
pub fn spawn(mut watchdog: Watchdog, period: Duration) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("doc-watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(period);
                if watchdog.tick(Instant::now()) {
                    return;
                }
            }
        })
}
