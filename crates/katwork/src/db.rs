use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool, migrate::Migrator, postgres::PgPoolOptions};

use crate::error::KatError;

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
