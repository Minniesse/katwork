use std::{error::Error, process::ExitCode, sync::Arc};
use tracing_subscriber::prelude::*;

use katwork::{auth, db, error::KatError, runtime::Core};
use katwork_server::{
    app::{AppState, build_router},
    config::{Config, redact_database_secrets},
};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::registry()
        // SQLx logs raw unknown URL parameters and malformed pgpass lines in this target.
        .with(tracing_subscriber::filter::filter_fn(|metadata| {
            !metadata.target().starts_with("sqlx_postgres::options")
        }))
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let database_url = std::env::var("DATABASE_URL").ok();
            eprintln!(
                "{}",
                error_report(
                    &error,
                    Config::from_env().ok().as_ref(),
                    database_url.as_deref()
                )
            );
            ExitCode::FAILURE
        }
    }
}

fn error_report(error: &KatError, config: Option<&Config>, database_url: Option<&str>) -> String {
    let mut lines = vec![error.code().to_owned()];
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        let message = match config {
            Some(config) => config.redact_secrets(&message),
            None => redact_database_secrets(&message, database_url.unwrap_or_default()),
        };
        lines.push(format!("  caused by: {message}"));
        source = cause.source();
    }
    lines.join("\n")
}

async fn run() -> Result<(), KatError> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".into());
    let reset_password = match args.next().as_deref() {
        None => false,
        Some("--reset-password") if command == "seed-demo" => true,
        Some(_) => return Err(KatError::CommandInvalid),
    };
    if args.next().is_some() {
        return Err(KatError::CommandInvalid);
    }
    match command.as_str() {
        "serve" => {
            let config = Arc::new(Config::from_env()?);
            if !config.cookie_secure() {
                tracing::warn!("session cookie is not Secure on an HTTP public URL");
            }
            let pool = db::connect(&config.database_url).await?;
            let core = Arc::new(Core::new(pool, config));
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
            katwork_server::transport::serve(listener, build_router(AppState { core })).await?;
            Ok(())
        }
        "migrate" => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url).await?;
            db::migrate(&pool).await
        }
        "seed-demo" => {
            let config = Config::from_env()?;
            if !config.demo {
                return Err(KatError::ConfigInvalid);
            }
            let pool = db::connect(&config.database_url).await?;
            if let Some(password) = auth::seed_demo(&pool, &config, reset_password).await? {
                println!("{password}");
            }
            Ok(())
        }
        _ => Err(KatError::CommandInvalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_report_without_config_never_prints_url_credentials() {
        let error = KatError::Io(std::io::Error::other(
            "failed https://user:pass@host/path?api-key=key-secret",
        ));
        for (database_url, expected) in [
            (None, "io_error\n  caused by: [redacted]"),
            (
                Some("postgres://h/db"),
                "io_error\n  caused by: failed https://host/path",
            ),
        ] {
            let report = error_report(&error, None, database_url);
            assert_eq!(report, expected);
            for secret in ["user", "pass", "key-secret"] {
                assert!(!report.contains(secret), "{report}");
            }
        }
    }

    #[test]
    fn error_report_includes_nested_causes_without_database_password() {
        let config = Config::from_lookup(|name| match name {
            "DATABASE_URL" => Some("postgres://user:db%2Bpassword@localhost/db".into()),
            "KATWORK_SECRET" => Some("test-only-secret-at-least-32-bytes".into()),
            _ => None,
        })
        .unwrap();
        let error = KatError::DatabaseUnavailable(Box::new(KatError::Io(std::io::Error::other(
            "authentication failed for db+password (db%2Bpassword)",
        ))));
        let report = error_report(&error, Some(&config), None);
        assert!(report.starts_with("database_unavailable\n"));
        assert!(report.contains("caused by: io_error"));
        assert!(report.contains("caused by: authentication failed for [redacted] ([redacted])"));
        assert!(!report.contains("db+password"));
        assert!(!report.contains("db%2Bpassword"));
    }
}
