use std::{error::Error, process::ExitCode, sync::Arc, time::Duration};

use katwork::error::KatError;
use katwork_server::{
    app::{AppState, build_router},
    config::{Config, redact_database_secrets},
};
use sqlx::{Connection, PgConnection, postgres::PgPoolOptions};

#[tokio::main]
async fn main() -> ExitCode {
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
    if args.next().is_some() {
        return Err(KatError::CommandInvalid);
    }
    match command.as_str() {
        "serve" => {
            let config = Arc::new(Config::from_env()?);
            let pool = match PgPoolOptions::new().connect(&config.database_url).await {
                Ok(pool) => pool,
                Err(mut error) => {
                    if matches!(error, sqlx::Error::PoolTimedOut) {
                        // The pool drops transient causes; one bounded attempt recovers them.
                        if let Ok(Err(cause)) = tokio::time::timeout(
                            Duration::from_secs(5),
                            PgConnection::connect(&config.database_url),
                        )
                        .await
                        {
                            error = cause;
                        }
                    }
                    return Err(KatError::DatabaseUnavailable(Box::new(error)));
                }
            };
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
            axum::serve(listener, build_router(AppState { pool, config })).await?;
            Ok(())
        }
        // Database migrations and demo data are wired in Tasks 2 and 3.
        "migrate" | "seed-demo" => Ok(()),
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
