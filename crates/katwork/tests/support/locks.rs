//! Waits for a backend to block on a lock, by polling PostgreSQL's lock tables, never by
//! sleeping for a guess.
//!
//! The core and HTTP test crates keep byte-identical copies of this file
//! (`support_copies_are_identical` in the server's `files.rs` checks it).
#![allow(
    clippy::disallowed_methods,
    reason = "The observer is a connection of its own, outside the pool"
)]
#![allow(dead_code, reason = "Shared by the core and HTTP lock tests")]

use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool};

const POLL: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(10);

/// The pid of a backend of this database, other than `holder_pid`, that waits (`granted =
/// false`) on `relation`, or on a row, transaction or advisory lock that `holder_pid` holds.
pub async fn wait_until_blocked_on(pool: &PgPool, holder_pid: i32, relation: &str) -> i32 {
    let mut observer = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let found = tokio::time::timeout(DEADLINE, async {
        loop {
            let waiter: Option<i32> = sqlx::query_scalar(
                "SELECT l.pid FROM pg_locks l
                 JOIN pg_stat_activity a ON a.pid = l.pid
                 WHERE NOT l.granted AND l.pid <> $1 AND a.datname = current_database()
                   AND ((l.locktype = 'relation' AND l.relation = to_regclass($2))
                        OR pg_blocking_pids(l.pid) @> ARRAY[$1]::int[])
                 ORDER BY l.pid LIMIT 1",
            )
            .bind(holder_pid)
            .bind(relation)
            .fetch_optional(&mut observer)
            .await
            .unwrap();
            match waiter {
                Some(pid) => return pid,
                None => tokio::time::sleep(POLL).await,
            }
        }
    })
    .await;
    observer.close().await.unwrap();
    found.unwrap_or_else(|_| {
        panic!("no backend waited on {relation} or on a lock of backend {holder_pid}")
    })
}
