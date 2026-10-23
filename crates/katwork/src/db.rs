use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};

use crate::error::KatError;

pub async fn set_lock_timeout(conn: &mut PgConnection, timeout: Duration) -> Result<(), KatError> {
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(format!("{}ms", timeout.as_millis().max(1)))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn connect(url: &str) -> Result<PgPool, KatError> {
    match PgPoolOptions::new().connect(url).await {
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

pub async fn migrate(pool: &PgPool) -> Result<(), KatError> {
    sqlx::migrate!().run(pool).await?;
    Ok(())
}
