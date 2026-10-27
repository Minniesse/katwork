//! The deadline, cancel token and memory budget of the job running on this thread. Every guard,
//! decoder and loop that can grow calls `check`, so a timed-out, cancelled or oversized job frees
//! its pool thread soon after.
use std::{cell::RefCell, time::Instant};

use tokio_util::sync::CancellationToken;

use super::{DocError, heap};

struct Scope {
    deadline: Instant,
    cancel: Option<CancellationToken>,
    /// `DocLimits::max_read_bytes`: the live bytes the job may hold on this thread.
    max_read_bytes: u64,
    checks: u32,
    /// Checks after this count fail with `Timeout` (`expire_after_checks`).
    expire_after: Option<u32>,
}

thread_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// Restores the enclosing scope (none, outside a nested call) when `scope` ends or unwinds.
struct Restore(Option<Scope>);

impl Drop for Restore {
    fn drop(&mut self) {
        let outer = self.0.take();
        SCOPE.with_borrow_mut(|scope| *scope = outer);
    }
}

/// Runs `f` with this thread's job deadline, cancel token and memory budget set, counting the
/// bytes it holds (`heap::ReadHeap`), and clears all of it after `f`, on a panic too. The pool
/// runs every job inside one; tests call readers inside one directly.
pub fn scope<T>(
    deadline: Instant,
    cancel: Option<CancellationToken>,
    max_read_bytes: u64,
    f: impl FnOnce() -> T,
) -> T {
    let outer = SCOPE.with_borrow_mut(|scope| {
        scope.replace(Scope {
            deadline,
            cancel,
            max_read_bytes,
            checks: 0,
            expire_after: None,
        })
    });
    let _restore = Restore(outer);
    let _counted = heap::Opened::new();
    f()
}

/// `Ok` outside a scope (the fuzz targets run readers without a pool), `Err(Cancelled)` once the
/// scope's token is cancelled, `Err(Timeout)` once its deadline has passed, and
/// `Err(TooComplex("memory"))` once the job holds more than its `max_read_bytes`.
pub fn check() -> Result<(), DocError> {
    SCOPE.with_borrow_mut(|scope| {
        let Some(scope) = scope else {
            return Ok(());
        };
        scope.checks = scope.checks.saturating_add(1);
        if scope
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(DocError::Cancelled);
        }
        if scope.expire_after.is_some_and(|after| scope.checks > after)
            || Instant::now() >= scope.deadline
        {
            return Err(DocError::Timeout);
        }
        if heap::live() > scope.max_read_bytes {
            return Err(DocError::TooComplex("memory"));
        }
        Ok(())
    })
}

/// Inside the current scope, check number `n + 1` and every later one is `Err(Timeout)`, so a
/// test can make a deadline pass mid-job without a clock. Outside a scope it does nothing.
#[doc(hidden)]
pub fn expire_after_checks(n: u32) {
    SCOPE.with_borrow_mut(|scope| {
        if let Some(scope) = scope {
            scope.expire_after = Some(n);
        }
    });
}
