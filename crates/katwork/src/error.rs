#[derive(Debug, thiserror::Error)]
pub enum KatError {
    #[error("kat_name_conflict")]
    KatNameConflict,
    #[error("default_space_required")]
    DefaultSpaceRequired,
    #[error("grant_in_use")]
    GrantInUse,
    #[error("team_lead_conflict")]
    TeamLeadConflict,
    #[error("team_lead_required")]
    TeamLeadRequired,
    #[error("kat_not_in_space")]
    KatNotInSpace,

    #[error("not_found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("space_policy_required")]
    SpacePolicyRequired,
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("reset_token_invalid")]
    ResetTokenInvalid,
    #[error("invite_token_invalid")]
    InviteTokenInvalid,
    #[error("user_exists")]
    UserExists,
    #[error("workspace_ambiguous")]
    WorkspaceAmbiguous,
    #[error("password_invalid")]
    PasswordInvalid,
    #[error("rate_limited")]
    RateLimited { retry_after_secs: u64 },
    #[error("request_invalid")]
    RequestInvalid,
    #[error("body_too_large")]
    BodyTooLarge,
    #[error("auth_busy")]
    AuthBusy,
    #[error("auth_failed")]
    AuthFailed,
    #[error("mail_delivery_failed")]
    MailDeliveryFailed,

    #[error("host_not_allowed")]
    HostNotAllowed,
    #[error("cross_site_request")]
    CrossSiteRequest,
    #[error("json_required")]
    JsonRequired,
    /// Names the variable (or flag) that failed, never its value.
    #[error("config_invalid ({var})")]
    ConfigInvalid { var: &'static str },
    #[error("command_invalid")]
    CommandInvalid,
    #[error("migration_required")]
    MigrationRequired,
    #[error("link_not_printed")]
    LinkNotPrinted,
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
            Self::KatNameConflict => "kat_name_conflict",
            Self::DefaultSpaceRequired => "default_space_required",
            Self::GrantInUse => "grant_in_use",
            Self::TeamLeadConflict => "team_lead_conflict",
            Self::TeamLeadRequired => "team_lead_required",
            Self::KatNotInSpace => "kat_not_in_space",

            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::SpacePolicyRequired => "space_policy_required",
            Self::Unauthenticated => "unauthenticated",
            Self::ResetTokenInvalid => "reset_token_invalid",
            Self::InviteTokenInvalid => "invite_token_invalid",
            Self::UserExists => "user_exists",
            Self::WorkspaceAmbiguous => "workspace_ambiguous",
            Self::PasswordInvalid => "password_invalid",
            Self::RateLimited { .. } => "rate_limited",
            Self::RequestInvalid => "request_invalid",
            Self::BodyTooLarge => "body_too_large",
            Self::AuthBusy => "auth_busy",
            Self::AuthFailed => "auth_failed",
            Self::MailDeliveryFailed => "mail_delivery_failed",

            Self::HostNotAllowed => "host_not_allowed",
            Self::CrossSiteRequest => "cross_site_request",
            Self::JsonRequired => "json_required",
            Self::ConfigInvalid { .. } => "config_invalid",
            Self::CommandInvalid => "command_invalid",
            Self::MigrationRequired => "migration_required",
            Self::LinkNotPrinted => "link_not_printed",
            Self::DatabaseUnavailable(_) => "database_unavailable",
            Self::Database(_) => "database_error",
            Self::Migration(_) => "migration_failed",
            Self::LeaseLost => "lease_lost",
            Self::Io(_) => "io_error",
        }
    }
}
