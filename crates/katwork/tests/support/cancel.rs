//! Real PostgreSQL barriers; deadlines bound hangs without changing the I/O clock.
#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#![allow(dead_code, reason = "Shared by core and HTTP cancellation tests")]

use std::{future::Future, time::Duration};

use sqlx::{Connection, PgConnection, PgPool};

pub const DEADLINE: Duration = Duration::from_secs(10);

pub async fn backend_pid(pool: &PgPool) -> i32 {
    tokio::time::timeout(
        DEADLINE,
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(pool),
    )
    .await
    .expect("pool slot did not return")
    .unwrap()
}

pub async fn wait_for_lock(pool: &PgPool, pid: i32) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='active' AND wait_event_type='Lock')")
                .bind(pid).fetch_one(pool).await.unwrap();
            if waiting { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("victim never reached its database lock");
}

/// Accept a closed backend or one idle outside a transaction, never an active/leaked transaction.
pub async fn assert_backend_idle(pool: &PgPool, pid: i32) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let state: Option<String> =
                sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid=$1")
                    .bind(pid)
                    .fetch_optional(pool)
                    .await
                    .unwrap();
            if state.is_none() || state.as_deref() == Some("idle") {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("backend remained active or idle in transaction");
}

pub async fn poll_once_then_drop(future: impl Future) {
    let mut future = Box::pin(future);
    assert!(futures::poll!(future.as_mut()).is_pending());
    drop(future);
}

/// The caller holds advisory lock 48 in another transaction before calling this helper.
pub async fn park_in_begin(conn: &mut PgConnection, observer: &PgPool, pid: i32) {
    let mut beginning = Box::pin(conn.begin_with("BEGIN; SELECT pg_advisory_xact_lock(48)"));
    tokio::select! {
        _ = beginning.as_mut() => panic!("BEGIN escaped the barrier"),
        () = wait_for_lock(observer, pid) => {},
    }
    poll_once_then_drop(beginning).await;
}
