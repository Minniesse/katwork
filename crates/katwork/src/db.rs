#![allow(
    clippy::disallowed_methods,
    reason = "The connection guard and pool regression probes live here"
)]

use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool, Row, migrate::Migrator, postgres::PgPoolOptions};

use crate::error::KatError;

/// A cancelled SQL future must never return a possibly open transaction to the pool.
struct GuardedConnection {
    connection: sqlx::pool::PoolConnection<sqlx::Postgres>,
    complete: bool,
}

impl GuardedConnection {
    async fn acquire(pool: &PgPool) -> Result<Self, KatError> {
        Ok(Self {
            connection: pool.acquire().await?,
            complete: false,
        })
    }

    fn connection(&mut self) -> &mut PgConnection {
        &mut self.connection
    }

    async fn finish<T>(mut self, result: Result<T, KatError>) -> Result<T, KatError> {
        recycle(&mut self.connection).await?;
        self.complete = true;
        result
    }
}

/// Run database work with close-on-cancellation and rollback-on-normal-return semantics.
/// Keep external I/O outside this scope; a dropped operation must release its backend.
pub async fn guarded<T>(
    pool: &PgPool,
    operation: impl AsyncFnOnce(&mut PgConnection) -> Result<T, KatError>,
) -> Result<T, KatError> {
    let mut connection = GuardedConnection::acquire(pool).await?;
    let result = operation(connection.connection()).await;
    connection.finish(result).await
}

impl Drop for GuardedConnection {
    fn drop(&mut self) {
        if !self.complete {
            self.connection.close_on_drop();
        }
    }
}

pub async fn set_lock_timeout(conn: &mut PgConnection, timeout: Duration) -> Result<(), KatError> {
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(format!("{}ms", timeout.as_millis().max(1)))
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn set_configured_lock_timeout(conn: &mut PgConnection) -> Result<(), KatError> {
    let timeout = crate::config::lock_timeout(std::env::var("KATWORK_LOCK_TIMEOUT_MS").ok())?;
    set_lock_timeout(conn, timeout).await
}

async fn recycle(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    // Drain the rollback queued by Transaction::drop before checking the server state.
    conn.flush().await?;
    // sqlx 0.9 misses transaction depth if BEGIN is cancelled before its reply.
    // Simple-query protocol keeps both timestamps equal for an implicit transaction.
    let in_transaction = match sqlx::raw_sql("SELECT now() <> statement_timestamp()")
        .fetch_one(&mut *conn)
        .await
    {
        Ok(row) => row.try_get(0)?,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("25P02") => true,
        Err(error) => return Err(error),
    };
    if in_transaction {
        sqlx::query("ROLLBACK").execute(conn).await?;
    }
    Ok(())
}

fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET idle_in_transaction_session_timeout = '60s'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .after_release(|conn, _| {
            Box::pin(async move {
                recycle(conn).await?;
                Ok(true)
            })
        })
}

pub async fn connect(url: &str) -> Result<PgPool, KatError> {
    match pool_options().connect(url).await {
        Ok(pool) => Ok(pool),
        Err(mut error) => {
            if matches!(error, sqlx::Error::PoolTimedOut) {
                // The pool drops transient causes; one bounded attempt recovers them.
                if let Ok(Err(cause)) =
                    tokio::time::timeout(Duration::from_secs(5), PgConnection::connect(url)).await
                {
                    error = cause;
                }
            }
            Err(KatError::DatabaseUnavailable(Box::new(error)))
        }
    }
}

static MIGRATOR: Migrator = sqlx::migrate!();

pub async fn migrate(pool: &PgPool) -> Result<(), KatError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// `migration_required` unless every migration this binary carries has been applied, so an
/// operator command on a new or older database says to run `migrate` instead of failing midway.
pub async fn require_migrated(pool: &PgPool) -> Result<(), KatError> {
    let applied: Vec<i64> =
        match sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(pool)
            .await
        {
            Ok(applied) => applied,
            // undefined_table: nothing was ever migrated.
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42P01") => {
                Vec::new()
            }
            Err(error) => return Err(error.into()),
        };
    if MIGRATOR
        .iter()
        .any(|migration| !applied.contains(&migration.version))
    {
        return Err(KatError::MigrationRequired);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/cancel.rs"]
mod cancellation;

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "./migrations")]
    async fn guarded_cancelled_begin_closes_backend(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let mut guard = GuardedConnection::acquire(&single).await.unwrap();
        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(guard.connection())
            .await
            .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(48)")
            .execute(&mut *blocker)
            .await
            .unwrap();
        cancellation::park_in_begin(guard.connection(), &pool, pid).await;
        drop(guard);
        blocker.rollback().await.unwrap();
        assert_ne!(cancellation::backend_pid(&single).await, pid);
        cancellation::assert_backend_idle(&pool, pid).await;
        single.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn guarded_poll_once_begin_closes_backend(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let mut guard = GuardedConnection::acquire(&single).await.unwrap();
        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(guard.connection())
            .await
            .unwrap();
        cancellation::poll_once_then_drop(guard.connection().begin()).await;
        drop(guard);
        assert_ne!(cancellation::backend_pid(&single).await, pid);
        cancellation::assert_backend_idle(&pool, pid).await;
        single.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn guarded_errors_rollback_and_reuse_backend(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let pid = cancellation::backend_pid(&single).await;
        for sql in ["BEGIN", "BEGIN; SELECT 1/0"] {
            let result: Result<(), KatError> = guarded(&single, async |conn| {
                let _ = sqlx::raw_sql(sql).execute(conn).await;
                Err(KatError::RequestInvalid)
            })
            .await;
            assert!(matches!(result, Err(KatError::RequestInvalid)));
            assert_eq!(cancellation::backend_pid(&single).await, pid);
            cancellation::assert_backend_idle(&pool, pid).await;
        }
        single.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn pool_bounds_idle_transactions_without_limiting_statements(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let idle: String = sqlx::query_scalar("SHOW idle_in_transaction_session_timeout")
            .fetch_one(&single)
            .await
            .unwrap();
        assert_eq!(idle, "1min");
        let statement: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&single)
            .await
            .unwrap();
        assert_eq!(statement, "0");
        single.close().await;
    }

    #[derive(Clone, Default)]
    struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn autocommit_release_does_not_warn(pool: PgPool) {
        let logs = LogBuffer::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let mut conn = single.acquire().await.unwrap();
        sqlx::query("SELECT 1").execute(&mut *conn).await.unwrap();
        conn.return_to_pool().await;
        single.close().await;
        let logs = logs.0.lock().unwrap();
        assert!(
            !String::from_utf8_lossy(&logs).contains("there is no transaction in progress"),
            "autocommit release sent an unnecessary ROLLBACK"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn aborted_transaction_does_not_poison_next_borrower(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let mut conn = single.acquire().await.unwrap();
        sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
        let error = sqlx::query("SELECT 1/0")
            .execute(&mut *conn)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("22012")
        );
        let error = sqlx::query("SELECT 1")
            .execute(&mut *conn)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("25P02")
        );
        drop(conn);
        assert_eq!(
            sqlx::query_scalar::<_, i32>("SELECT 42")
                .fetch_one(&single)
                .await
                .unwrap(),
            42
        );
        single.close().await;
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn cancelled_begin_does_not_leak_transaction_to_next_borrower(pool: PgPool) {
        let single = pool_options()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE cancellation_probe(value boolean NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cancellation_probe VALUES(false)")
            .execute(&pool)
            .await
            .unwrap();
        let mut conn = single.acquire().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(1)")
            .execute(&mut *blocker)
            .await
            .unwrap();
        // Delay ReadyForQuery so sqlx cannot finish BEGIN before cancellation.
        let mut beginning = Box::pin(conn.begin_with("BEGIN; SELECT pg_advisory_xact_lock(1)"));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                _ = beginning.as_mut() => panic!("BEGIN completed while the advisory lock was held"),
                _ = async {
                    loop {
                        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='active' AND wait_event='advisory')")
                            .bind(pid).fetch_one(&pool).await.unwrap();
                        if waiting {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                } => {}
            }
        })
        .await
        .expect("BEGIN never reached the advisory lock");
        assert!(futures::poll!(beginning.as_mut()).is_pending());
        drop(beginning);
        blocker.rollback().await.unwrap();
        drop(conn);
        assert_eq!(
            sqlx::query("UPDATE cancellation_probe SET value=true")
                .execute(&single)
                .await
                .unwrap()
                .rows_affected(),
            1
        );
        let external: bool = sqlx::query_scalar("SELECT value FROM cancellation_probe")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            external,
            "successful autocommit write was hidden in leaked transaction"
        );
        single.close().await;
    }
}
