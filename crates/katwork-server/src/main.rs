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
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rig=info".into()),
        )
        .with(tracing_subscriber::filter::filter_fn(
            dependency_log_allowed,
        ))
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

fn dependency_log_allowed(metadata: &tracing::Metadata<'_>) -> bool {
    // SQLx logs secret options and Rig logs provider messages, even at WARN.
    !metadata.target().starts_with("sqlx") && !metadata.target().starts_with("rig")
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
            let transport = config.transport.clone();
            let core = Arc::new(Core::new(pool, config));
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
            katwork_server::transport::serve_with_shutdown(
                listener,
                build_router(AppState { core }),
                transport,
                shutdown_signal(),
            )
            .await?;
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

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = terminate => {} }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn configured_registry_logs_one_safe_init_failure_per_provider() {
        use katwork::{
            config::ProviderEnv,
            models::*,
            types::{ModelTier, SpacePolicy},
        };
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        for id in ["openai", "anthropic"] {
            let key = uuid::Uuid::now_v7().to_string();
            let url = format!("file:///{}", uuid::Uuid::now_v7());
            let mut cfg = Config::from_lookup(|name| match name {
                "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
                "KATWORK_SECRET" => Some("x".repeat(32)),
                "KATWORK_DEMO" => Some("1".into()),
                _ => None,
            })
            .unwrap();
            let env = ProviderEnv {
                api_key: key.clone(),
                base_url: Some(url.clone()),
            };
            if id == "openai" {
                cfg.openai = Some(env);
            } else {
                cfg.anthropic = Some(env);
            }
            let output = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_ansi(false)
                    .with_target(false)
                    .with_writer({
                        let output = output.clone();
                        move || Capture(output.clone())
                    }),
            );
            let _guard = subscriber.set_default();
            let registry = Registry::from_config(&cfg);
            for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Strong] {
                assert!(matches!(
                    registry.for_tier(
                        tier,
                        &SpacePolicy {
                            allow_external: true
                        }
                    ),
                    Err(ModelError::ProviderUnavailable)
                ));
            }
            let logged = String::from_utf8(output.lock().unwrap().clone()).unwrap();
            assert_eq!(
                logged.trim(),
                format!("WARN provider_init_failed provider_id=\"{id}\"")
            );
            assert!(!logged.contains(&key));
            assert!(!logged.contains(&url));
        }
    }

    #[test]
    fn real_main_tracing_setup_blocks_dependency_secrets() {
        for filter in [None, Some("trace,rig=trace,rig_core=trace")] {
            let secret = uuid::Uuid::now_v7().to_string();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "tests::main_tracing_probe", "--nocapture"])
                .env("KATWORK_TEST_TRACING_SECRET", &secret)
                .env_remove("RUST_LOG");
            if let Some(filter) = filter {
                command.env("RUST_LOG", filter);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("main_tracing_probe_safe"), "{stderr}");
            assert!(
                !stderr.contains(&secret),
                "main installed an unsafe subscriber"
            );
        }
    }

    #[test]
    fn main_tracing_probe() {
        let Ok(secret) = std::env::var("KATWORK_TEST_TRACING_SECRET") else {
            return;
        };
        // The test runner's arguments make run() reject the command after real main installs tracing.
        assert_eq!(main(), ExitCode::FAILURE);
        tracing::trace!(target: "rig::completions", %secret);
        tracing::debug!(target: "rig_core::completions", %secret);
        tracing::warn!(target: "rig_core::providers", %secret);
        tracing::error!(target: "rig::providers", %secret);
        tracing::warn!(target: "sqlx_postgres::options::parse", %secret);
        tracing::info!(target: "katwork", "main_tracing_probe_safe");
    }

    #[test]
    fn rig_trace_prompts_are_blocked_even_with_explicit_trace_directives() {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(
                "trace,rig::completions=trace",
            ))
            .with(tracing_subscriber::filter::filter_fn(
                dependency_log_allowed,
            ))
            .with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_ansi(false)
                    .with_writer({
                        let output = output.clone();
                        move || Capture(output.clone())
                    }),
            );
        let _guard = subscriber.set_default();
        tracing::trace!(target: "rig::completions", "private prompt");
        tracing::debug!(target: "rig_core::completions", "private response");
        tracing::warn!(target: "sqlx_postgres::options::parse", "private database option");
        tracing::warn!(target: "rig_core::providers", "private provider message");
        tracing::error!(target: "rig::providers", "private provider body");
        tracing::info!(target: "katwork", "safe status");
        let logged = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("safe status"));
        assert!(!logged.contains("private"));
    }

    #[tokio::test]
    async fn provider_error_messages_never_reach_server_logs() {
        use katwork::{config::ProviderEnv, models::*};
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

        for filter in ["info,rig=info", "trace,rig_core=trace"] {
            for status in [200, 503] {
                let secret = format!("fixture-sensitive-{}", uuid::Uuid::now_v7());
                let body = serde_json::json!({"error": {
                    "type": "server_error", "message": secret
                }})
                .to_string();
                let body = if status == 200 {
                    format!("data: {body}\n\n")
                } else {
                    body
                };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = BufReader::new(socket);
                    let mut content_length = 0;
                    loop {
                        let mut line = String::new();
                        assert!(socket.read_line(&mut line).await.unwrap() > 0);
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            content_length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                    socket
                        .read_exact(&mut vec![0; content_length])
                        .await
                        .unwrap();
                    socket.write_all(format!(
                        "HTTP/1.1 {status} fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nx-request-id: fixture-request\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).as_bytes()).await.unwrap();
                });
                let output = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
                let subscriber = tracing_subscriber::registry()
                    .with(tracing_subscriber::EnvFilter::new(filter))
                    .with(tracing_subscriber::filter::filter_fn(
                        dependency_log_allowed,
                    ))
                    .with(
                        tracing_subscriber::fmt::layer()
                            .without_time()
                            .with_ansi(false)
                            .with_writer({
                                let output = output.clone();
                                move || Capture(output.clone())
                            }),
                    );
                let _guard = subscriber.set_default();
                let provider = RigProvider::openai(
                    &ProviderEnv {
                        api_key: uuid::Uuid::now_v7().to_string(),
                        base_url: Some(format!("http://{address}")),
                    },
                    "fixture",
                    OpenAiApi::Chat,
                )
                .unwrap();
                let mut stream = provider
                    .stream(ChatRequest {
                        system: "fixture".into(),
                        messages: vec![],
                        tools: vec![],
                        max_output_tokens: 10,
                        temperature: None,
                    })
                    .await
                    .unwrap();
                let result = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
                assert_eq!(result, Some(Err(ModelError::ProviderUnavailable)));
                server.await.unwrap();
                let logged = String::from_utf8(output.lock().unwrap().clone()).unwrap();
                assert!(
                    !logged.contains(&secret),
                    "provider body leaked through {filter}"
                );
                assert!(logged.contains("provider_unavailable"), "{logged}");
                assert!(logged.contains("error_body_bytes="), "{logged}");
                if status == 503 {
                    assert!(logged.contains("status=503"), "{logged}");
                    assert!(logged.contains("fixture-request"), "{logged}");
                }
            }
        }
    }

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
