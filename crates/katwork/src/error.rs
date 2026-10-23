#[derive(Debug, thiserror::Error)]
pub enum KatError {
    #[error("config_invalid")]
    ConfigInvalid,
    #[error("command_invalid")]
    CommandInvalid,
    #[error("database_unavailable")]
    DatabaseUnavailable(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("database_error")]
    Database(#[source] sqlx::Error),
    #[error("migration_failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("lease_lost")]
    LeaseLost,
    #[error("io_error")]
    Io(#[from] std::io::Error),
}

impl From<sqlx::Error> for KatError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_) => Self::DatabaseUnavailable(Box::new(error)),
            _ => Self::Database(error),
        }
    }
}

impl KatError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigInvalid => "config_invalid",
            Self::CommandInvalid => "command_invalid",
            Self::DatabaseUnavailable(_) => "database_unavailable",
            Self::Database(_) => "database_error",
            Self::Migration(_) => "migration_failed",
            Self::LeaseLost => "lease_lost",
            Self::Io(_) => "io_error",
        }
    }
}
