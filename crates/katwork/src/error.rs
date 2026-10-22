#[derive(Debug, thiserror::Error)]
pub enum KatError {
    #[error("config_invalid")]
    ConfigInvalid,
    #[error("command_invalid")]
    CommandInvalid,
    #[error("database_unavailable")]
    DatabaseUnavailable,
    #[error("io_error")]
    Io(#[from] std::io::Error),
}

impl KatError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigInvalid => "config_invalid",
            Self::CommandInvalid => "command_invalid",
            Self::DatabaseUnavailable => "database_unavailable",
            Self::Io(_) => "io_error",
        }
    }
}
