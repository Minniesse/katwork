use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool, migrate::Migrator, postgres::PgPoolOptions};

use crate::error::KatError;

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

fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new().after_release(|conn, _| {
        Box::pin(async move {
            // sqlx 0.9 can miss transaction depth when BEGIN is cancelled before its reply.
            sqlx::query("ROLLBACK").execute(conn).await?;
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
mod tests {
    use super::*;

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
        let mut beginning = Box::pin(conn.begin());
        assert!(futures::poll!(beginning.as_mut()).is_pending());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let idle: bool = sqlx::query_scalar("SELECT state='idle in transaction' AND query='BEGIN' FROM pg_stat_activity WHERE pid=$1")
                .bind(pid).fetch_one(&pool).await.unwrap();
            if idle {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "BEGIN never reached server"
            );
            tokio::task::yield_now().await;
        }
        drop(beginning);
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
