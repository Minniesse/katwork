pub mod app;
pub mod config;
pub mod error;
pub mod extract;
pub mod guards;
mod log_throttle;
mod peer;
mod routes;
pub mod static_files;
pub mod transport;

use std::{sync::Arc, thread::JoinHandle, time::Duration};

use katwork::{
    docs::watchdog::{self, Watchdog, WedgeSource},
    runtime::Core,
};

/// Starts the watchdog that exits the server with code 70 once the document pool has stayed
/// wedged for `wedge_exit_after`, so Compose's `restart: unless-stopped` starts a fresh one.
pub fn spawn_doc_watchdog(core: &Core) -> std::io::Result<JoinHandle<()>> {
    let source: Arc<dyn WedgeSource> = core.docs.clone();
    let exit: Arc<dyn Fn(i32) + Send + Sync> = Arc::new(|code| std::process::exit(code));
    let after = core.docs.limits().wedge_exit_after;
    watchdog::spawn(Watchdog::new(source, after, exit), Duration::from_secs(1))
}
