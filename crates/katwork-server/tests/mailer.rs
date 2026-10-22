use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use katwork::{
    auth::{
        mailer::{self, CapturingMailer, Mail, MailKind},
        reset,
    },
    runtime::Core,
};
use katwork_server::app::{AppState, build_router};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::instrument::WithSubscriber;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn subscriber(logs: &Logs) -> impl tracing::Subscriber + Send + Sync + 'static {
    let logs = logs.clone();
    tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || logs.clone())
        .finish()
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_token_never_appears_in_logs(pool: PgPool) {
    let (user_id, password) = support::user(&pool).await;
    let capture = Arc::new(CapturingMailer::default());
    let logs = Logs::default();
    let mut core = Core::new(pool.clone(), Arc::new(support::config(None)));
    core.mailer = capture.clone();
    let app = build_router(AppState {
        core: Arc::new(core),
    });
    let response = support::request(
        &app,
        "POST",
        "/api/auth/reset/request",
        Some(json!({"email": "person@example.test"})),
        None,
    )
    .with_subscriber(subscriber(&logs))
    .await;
    assert_eq!(response.status(), 202);
    let sent = capture.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "person@example.test");
    let link = sent[0]
        .body
        .split_whitespace()
        .find(|part| part.starts_with("http://"))
        .unwrap();
    assert!(link.starts_with("http://localhost:3000/reset?token="));
    let token = link.split_once("token=").unwrap().1;
    let stored: String =
        sqlx::query_scalar("SELECT token_sha256 FROM password_resets WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, format!("{:x}", Sha256::digest(token.as_bytes())));
    let within_30: bool = sqlx::query_scalar("SELECT expires_at BETWEEN clock_timestamp() + interval '29 minutes' AND clock_timestamp() + interval '30 minutes' FROM password_resets WHERE user_id = $1").bind(user_id).fetch_one(&pool).await.unwrap();
    assert!(within_30);
    for secret in [token, link, &password] {
        assert!(!logs.text().contains(secret));
    }
    assert!(!format!("{:?}", sent[0]).contains(token));
    let new_password = Uuid::now_v7().to_string();
    reset::confirm(&pool, token, &new_password)
        .with_subscriber(subscriber(&logs))
        .await
        .unwrap();
    let response = support::login(&app, &new_password)
        .with_subscriber(subscriber(&logs))
        .await;
    assert_eq!(response.status(), 200);
    for secret in [token, new_password.as_str()] {
        assert!(!logs.text().contains(secret));
    }
    reset::request(
        &pool,
        capture.as_ref(),
        "unknown@example.test",
        "http://localhost:3000",
    )
    .await
    .unwrap();
    assert_eq!(capture.sent().len(), 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_without_smtp_returns_202_and_sends_nothing(pool: PgPool) {
    support::user(&pool).await;
    let logs = Logs::default();
    let app = support::app(&pool, None);
    let response = support::request(
        &app,
        "POST",
        "/api/auth/reset/request",
        Some(json!({"email": "person@example.test"})),
        None,
    )
    .with_subscriber(subscriber(&logs))
    .await;
    assert_eq!(response.status(), 202);
    let text = logs.text();
    assert!(text.contains("mail_not_sent"));
    assert!(text.contains("smtp_unconfigured"));
    assert!(!text.contains("person@example.test"));
    assert!(!text.contains("token="));
}

#[tokio::test]
async fn only_demo_console_backend_logs_mail_content() {
    let token = Uuid::now_v7().to_string();
    let mail = Mail {
        kind: MailKind::PasswordReset,
        to: "person@example.test".into(),
        subject: "Reset password".into(),
        body: format!("https://katwork.example/reset?token={token}"),
    };
    for demo in [false, true] {
        let mut config = support::config(None);
        config.demo = demo;
        let logs = Logs::default();
        mailer::from_config(&config)
            .send(&mail)
            .with_subscriber(subscriber(&logs))
            .await
            .unwrap();
        assert_eq!(logs.text().contains(&token), demo);
        if demo {
            assert!(logs.text().contains("demo only"));
        }
    }
}

#[tokio::test]
async fn smtp_errors_never_log_mail_or_credentials_and_override_demo() {
    let password = Uuid::now_v7().to_string();
    let token = Uuid::now_v7().to_string();
    let mut config = support::config(None);
    config.demo = true;
    config.smtp = Some(katwork::config::SmtpConfig {
        host: "127.0.0.1".into(),
        port: Some(1),
        username: Some("mailer".into()),
        password: Some(password.clone()),
        from: Some("katwork@example.test".into()),
    });
    let mail = Mail {
        kind: MailKind::PasswordReset,
        to: "person@example.test".into(),
        subject: token.clone(),
        body: format!("https://katwork.example/reset?token={token}"),
    };
    let logs = Logs::default();
    let error = mailer::from_config(&config)
        .send(&mail)
        .with_subscriber(subscriber(&logs))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "mail_delivery_failed");
    let text = logs.text();
    assert!(text.contains("failed"));
    for secret in [password, token, mail.to, mail.body] {
        assert!(!text.contains(&secret));
    }
    assert!(!text.contains("demo only"));
}
