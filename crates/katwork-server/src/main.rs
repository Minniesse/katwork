use std::{
    collections::HashMap, error::Error, io::Write, net::IpAddr, process::ExitCode, sync::Arc,
};
use tracing_subscriber::prelude::*;
use url::Url;

use katwork::{
    auth::{
        self,
        accounts::{self, WorkspaceRef},
    },
    db,
    error::KatError,
    runtime::Core,
    tenancy::Role,
    types::{Locale, SpacePolicy},
};
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
    // The code, plus the variable for `config_invalid (VAR)`; never a value.
    let mut lines = vec![match error {
        KatError::ConfigInvalid { .. } => error.to_string(),
        _ => error.code().to_owned(),
    }];
    // What the operator does next; none of these lines carries a value or a link.
    match error {
        KatError::CommandInvalid => lines.push(USAGE.trim_end().to_owned()),
        KatError::MigrationRequired => {
            lines.push("  run `katwork-server migrate` first, then try again".into())
        }
        KatError::LinkNotPrinted => lines.extend([
            "  the account change was saved, but its link could not be written to stdout".into(),
            "  run `katwork-server reset-link --email <email>` for a new link".into(),
        ]),
        _ => {}
    }
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

const USAGE: &str = "\
usage: katwork-server [serve]
       katwork-server healthcheck
       katwork-server migrate
       katwork-server seed-demo [--reset-password]
       katwork-server create-user --email <email> --workspace <name|id> --role <owner|admin|member> [--space-policy <local|hosted>] [--locale <en|th>]
       katwork-server reset-link --email <email>
";

/// `--name value` pairs, each name from `allowed` and given at most once.
fn flags(
    args: &[String],
    allowed: &[&'static str],
) -> Result<HashMap<&'static str, String>, KatError> {
    let mut flags = HashMap::new();
    let mut args = args.iter();
    while let Some(name) = args.next() {
        let name = allowed
            .iter()
            .find(|allowed| **allowed == name)
            .ok_or(KatError::CommandInvalid)?;
        let value = args.next().ok_or(KatError::CommandInvalid)?;
        if flags.insert(*name, value.clone()).is_some() {
            return Err(KatError::CommandInvalid);
        }
    }
    Ok(flags)
}

enum Command {
    Help,
    Serve,
    Healthcheck,
    Migrate,
    SeedDemo {
        reset_password: bool,
    },
    CreateUser {
        email: String,
        workspace: WorkspaceRef,
        role: Role,
        space_policy: Option<SpacePolicy>,
        locale: Locale,
    },
    ResetLink {
        email: String,
    },
}

fn parse(args: &[String]) -> Result<Command, KatError> {
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => ("serve", &[][..]),
    };
    if command == "help"
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return Ok(Command::Help);
    }
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    Ok(match (command, rest.as_slice()) {
        ("serve", []) => Command::Serve,
        ("healthcheck", []) => Command::Healthcheck,
        ("migrate", []) => Command::Migrate,
        ("seed-demo", []) => Command::SeedDemo {
            reset_password: false,
        },
        ("seed-demo", ["--reset-password"]) => Command::SeedDemo {
            reset_password: true,
        },
        ("create-user", _) => {
            let mut flags = flags(
                &args[1..],
                &[
                    "--email",
                    "--workspace",
                    "--role",
                    "--space-policy",
                    "--locale",
                ],
            )?;
            let mut required = |name| flags.remove(name).ok_or(KatError::CommandInvalid);
            let email = required("--email")?;
            let workspace = WorkspaceRef::parse(&required("--workspace")?);
            let role = match required("--role")?.as_str() {
                "owner" => Role::Owner,
                "admin" => Role::Admin,
                "member" => Role::Member,
                _ => return Err(KatError::CommandInvalid),
            };
            let space_policy = match flags.remove("--space-policy").as_deref() {
                None => None,
                Some("local") => Some(SpacePolicy {
                    allow_external: false,
                }),
                Some("hosted") => Some(SpacePolicy {
                    allow_external: true,
                }),
                Some(_) => return Err(KatError::CommandInvalid),
            };
            let locale = match flags.remove("--locale").as_deref() {
                None | Some("en") => Locale::En,
                Some("th") => Locale::Th,
                Some(_) => return Err(KatError::CommandInvalid),
            };
            Command::CreateUser {
                email,
                workspace,
                role,
                space_policy,
                locale,
            }
        }
        ("reset-link", _) => Command::ResetLink {
            email: flags(&args[1..], &["--email"])?
                .remove("--email")
                .ok_or(KatError::CommandInvalid)?,
        },
        _ => return Err(KatError::CommandInvalid),
    })
}

async fn run() -> Result<(), KatError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args)? {
        Command::Help => {
            // Nothing to recover if stdout is gone.
            let _ = std::io::stdout().lock().write_all(USAGE.as_bytes());
            Ok(())
        }
        Command::Serve => {
            let config = Arc::new(Config::from_env()?);
            if !config.cookie_secure() {
                tracing::warn!("session cookie is not Secure on an HTTP public URL");
            }
            let bind = std::env::var("KATWORK_BIND").ok();
            if lan_bind_without_public_url(bind.as_deref(), &config) {
                tracing::warn!(
                    "KATWORK_BIND exposes KatWork beyond this host, but KATWORK_PUBLIC_URL is unset or names this machine: other machines get host_not_allowed and printed links point at it; set KATWORK_PUBLIC_URL to the address people open"
                );
            }
            let pool = db::connect(&config.database_url).await?;
            let transport = config.transport.clone();
            let core = Arc::new(Core::new(pool, config));
            core.registry.log_tiers();
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
            let mut workers = katwork::jobs::worker::spawn_pool(
                core.clone(),
                4,
                std::time::Duration::from_secs(60),
            );
            let shutdown = core.shutdown.clone();
            let served = katwork_server::transport::serve_with_shutdown(
                listener,
                build_router(AppState { core: core.clone() }),
                transport,
                async move {
                    shutdown_signal().await;
                    core.shutdown.cancel();
                },
            )
            .await;
            shutdown.cancel();
            while let Some(result) = workers.join_next().await {
                if result.is_err() {
                    tracing::error!("worker_task_failed");
                }
            }
            served?;
            Ok(())
        }
        Command::Healthcheck => Ok(healthcheck(([127, 0, 0, 1], 3000).into()).await?),
        Command::Migrate => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url).await?;
            db::migrate(&pool).await
        }
        Command::SeedDemo { reset_password } => {
            let config = Config::from_env()?;
            if !config.demo {
                return Err(KatError::ConfigInvalid {
                    var: "KATWORK_DEMO",
                });
            }
            let pool = db::connect(&config.database_url).await?;
            if let Some(password) = auth::seed_demo(&pool, &config, reset_password).await? {
                println!("{password}");
            }
            Ok(())
        }
        // The operator commands print the link only to stdout; it never goes through tracing.
        Command::CreateUser {
            email,
            workspace,
            role,
            space_policy,
            locale,
        } => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url).await?;
            db::require_migrated(&pool).await?;
            let link = accounts::create_user(
                &pool,
                &email,
                &workspace,
                role,
                space_policy,
                locale,
                &config,
            )
            .await?;
            print_link(&link)
        }
        Command::ResetLink { email } => {
            let config = Config::from_env()?;
            let pool = db::connect(&config.database_url).await?;
            db::require_migrated(&pool).await?;
            let link = accounts::reset_link(&pool, &email, config.public_url_or_default()).await?;
            print_link(&link)
        }
    }
}

async fn healthcheck(address: std::net::SocketAddr) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut socket = tokio::net::TcpStream::connect(address).await?;
        socket
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut status = [0; 13];
        socket.read_exact(&mut status).await?;
        if &status != b"HTTP/1.1 200 " {
            return Err(std::io::Error::other("health endpoint failed"));
        }
        Ok(())
    })
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}

/// Writes an operator command's link, the only thing it prints. A closed or failing stdout is
/// `link_not_printed`, never a panic, and the link never goes to stderr.
fn print_link(link: &str) -> Result<(), KatError> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{link}")
        .and_then(|()| stdout.flush())
        .map_err(|_| KatError::LinkNotPrinted)
}

/// Compose publishes port 3000 on `KATWORK_BIND`. Past loopback, people on other machines open
/// KatWork by this host's address, which a localhost public URL neither allows as a Host nor
/// puts in printed links.
fn lan_bind_without_public_url(bind: Option<&str>, config: &Config) -> bool {
    let local = |host: &str| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    };
    let exposed = bind
        .map(str::trim)
        .is_some_and(|bind| !bind.is_empty() && !local(bind));
    let public_is_local = Url::parse(config.public_url_or_default())
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_none_or(|host| local(&host));
    exposed && public_is_local
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
    #[tokio::test]
    async fn healthcheck_accepts_only_a_complete_success_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (response, healthy) in [
            ("HTTP/1.1 200 OK\r\n", true),
            ("HTTP/1.1 503 Unavailable\r\n", false),
            ("HTTP", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let expected =
                    b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                let mut request = vec![0; expected.len()];
                socket.read_exact(&mut request).await.unwrap();
                assert_eq!(request, expected);
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            assert_eq!(super::healthcheck(address).await.is_ok(), healthy);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn healthcheck_times_out_when_the_server_stalls() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let check = tokio::spawn(super::healthcheck(address));
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 1];
        socket.read_exact(&mut request).await.unwrap();
        // Establish real I/O before virtual time can advance past the connection attempt.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(
            check.await.unwrap().unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

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
        for id in ["openai", "anthropic", "local"] {
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
            } else if id == "anthropic" {
                cfg.anthropic = Some(env);
            } else {
                cfg.local = Some(katwork::config::LocalEnv {
                    base_url: url.clone(),
                    model: "fixture".into(),
                    api_key: Some(key.clone()),
                });
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
            let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
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
        let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
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
                let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
                let _guard = subscriber.set_default();
                let provider = RigProvider::openai(
                    &ProviderEnv {
                        api_key: uuid::Uuid::now_v7().to_string(),
                        base_url: Some(format!("http://{address}")),
                    },
                    "fixture",
                    OpenAiApi::Chat,
                    HttpLimits::default(),
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
    fn config_error_report_names_only_the_variable() {
        let error = KatError::ConfigInvalid {
            var: "KATWORK_LOCAL_BASE_URL",
        };
        assert_eq!(
            error_report(&error, None, None),
            "config_invalid (KATWORK_LOCAL_BASE_URL)"
        );
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
