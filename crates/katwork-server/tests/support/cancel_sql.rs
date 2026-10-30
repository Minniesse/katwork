//! Drops a database operation while the server holds it inside a statement. The server is the
//! barrier: a blocker holds an ACCESS EXCLUSIVE lock on a table, the helper polls `pg_locks`
//! for this operation's waiting backend, drops the operation's future, releases the table and
//! checks how that backend ended. Deadlines only bound a hang; nothing here sleeps for a state.
//!
//! The core and HTTP test crates keep byte-identical copies of this file
//! (`support_copies_are_identical` in the server's `files.rs` checks it).
#![allow(
    clippy::disallowed_methods,
    reason = "The blocker and the observer are connections of their own, outside the pool"
)]
#![allow(dead_code, reason = "Shared by the core and HTTP cancellation tests")]

use std::{future::Future, time::Duration};

use sqlx::{Connection, PgConnection, PgPool};

const POLL: Duration = Duration::from_millis(20);
const REACH: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(5);

/// The plan's helper: `op()` is polled until its backend waits on `table`, then dropped; once
/// the table is released, that backend must be idle or gone, never idle in a transaction.
/// (A worker's reviewed drain exception returns its connection to the pool, idle.)
pub async fn drop_during_statement<F, Fut, T>(pool: &PgPool, table: &str, op: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    drop_and_settle(pool, table, op).await;
}

/// `drop_during_statement` for an operation that runs inside `db::guarded` on `pool`, a pool of
/// one connection: the dropped operation's connection must also be closed, so the pool's next
/// connection is a new backend. A plain pool query is drained back into the pool instead and
/// comes back with the same backend; the plan's check cannot tell the two apart, since a test
/// pool soon closes an idle connection anyway.
pub async fn drop_during_guarded_statement<F, Fut, T>(pool: &PgPool, table: &str, op: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    assert_eq!(
        pool.options().get_max_connections(),
        1,
        "the op's pool must hold one connection, so its next connection shows whether it closed"
    );
    let pid = drop_and_settle(pool, table, op).await;
    let next: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_ne!(
        next, pid,
        "the backend of the op dropped on {table} stayed connected: \
         a db::guarded operation closes its connection when dropped"
    );
}

/// A committed write on a connection of `pool`, to show the pool still works.
pub async fn probe_write(pool: &PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("CREATE TEMP TABLE probe (x int) ON COMMIT DROP")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO probe VALUES (1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let x: i32 = sqlx::query_scalar("SELECT x FROM probe")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(x, 1);
    tx.commit().await.unwrap();
}

/// Drops `op` during a statement on `table` and waits for its backend to be idle or gone;
/// returns that backend's pid.
async fn drop_and_settle<F, Fut, T>(pool: &PgPool, table: &str, op: F) -> i32
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    assert!(
        !table.is_empty()
            && table
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
        "table names are plain identifiers: {table:?}"
    );
    let mut blocker = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"
    )))
    .execute(&mut blocker)
    .await
    .unwrap();
    let mut observer = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let mut future = Box::pin(op());
    let reached = tokio::time::timeout(REACH, async {
        loop {
            tokio::select! {
                biased;
                _ = &mut future => panic!("op never reached {table}: it finished first"),
                () = tokio::time::sleep(POLL) => {}
            }
            if let Some(pid) = waiting_on(&mut observer, table, blocker_pid).await {
                return pid;
            }
        }
    })
    .await;
    let Ok(pid) = reached else {
        panic!("op never reached {table} within {REACH:?}");
    };
    drop(future);
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
    blocker.close().await.unwrap();
    let settled = tokio::time::timeout(SETTLE, async {
        loop {
            let state: Option<Option<String>> =
                sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
                    .bind(pid)
                    .fetch_optional(&mut observer)
                    .await
                    .unwrap();
            match state {
                None => return,
                Some(Some(state)) if state == "idle" => return,
                _ => tokio::time::sleep(POLL).await,
            }
        }
    })
    .await;
    if settled.is_err() {
        let state: Option<Option<String>> =
            sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_optional(&mut observer)
                .await
                .unwrap();
        panic!("the backend of the op dropped on {table} ended neither idle nor gone: {state:?}");
    }
    observer.close().await.unwrap();
    pid
}

/// A backend of this database, other than the blocker, waiting for a lock on `table`.
async fn waiting_on(observer: &mut PgConnection, table: &str, blocker_pid: i32) -> Option<i32> {
    sqlx::query_scalar(
        "SELECT pid FROM pg_locks
         WHERE NOT granted AND locktype = 'relation' AND relation = to_regclass($1)
           AND pid <> $2
           AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
         ORDER BY pid LIMIT 1",
    )
    .bind(table)
    .bind(blocker_pid)
    .fetch_optional(observer)
    .await
    .unwrap()
}
