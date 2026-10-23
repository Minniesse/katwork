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
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while capture.sent().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !logs.text().contains("mail_not_sent") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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
        to: "demo@katwork.local".into(),
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

#[tokio::test]
async fn demo_console_never_logs_non_demo_reset_links() {
    let mut config = support::config(None);
    config.demo = true;
    let mail = Mail {
        kind: MailKind::PasswordReset,
        to: "person@example.test".into(),
        subject: "Reset password".into(),
        body: "http://localhost:3000/reset?token=private".into(),
    };
    let logs = Logs::default();
    mailer::from_config(&config)
        .send(&mail)
        .with_subscriber(subscriber(&logs))
        .await
        .unwrap();
    assert!(logs.text().contains("smtp_unconfigured"));
    assert!(!logs.text().contains(&mail.to));
    assert!(!logs.text().contains(&mail.body));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_link_uses_configured_origin_instead_of_host(pool: PgPool) {
    use axum::{body::Body, extract::ConnectInfo, http::Request};
    use tower::ServiceExt;
    support::user(&pool).await;
    let capture = Arc::new(CapturingMailer::default());
    let mut core = Core::new(
        pool,
        Arc::new(support::config(Some("https://katwork.example"))),
    );
    core.mailer = capture.clone();
    let app = build_router(AppState {
        core: Arc::new(core),
    });
    let response = app
        .oneshot(
            Request::post("/api/auth/reset/request")
                .header("host", "localhost:9999")
                .header("origin", "https://katwork.example")
                .header("content-type", "application/json")
                .extension(ConnectInfo(
                    "127.0.0.1:7000".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .body(Body::from(
                    json!({"email": "person@example.test"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while capture.sent().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let body = &capture.sent()[0].body;
    assert!(
        body.lines()
            .last()
            .unwrap()
            .starts_with("https://katwork.example/reset?token="),
        "{body}"
    );
}

#[tokio::test]
async fn smtp_timeout_logs_failed_without_reset_content() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = support::config(None);
    config.smtp = Some(katwork::config::SmtpConfig {
        host: "127.0.0.1".into(),
        port: Some(listener.local_addr().unwrap().port()),
        username: None,
        password: None,
        from: None,
    });
    let mailer = mailer::from_config(&config);
    let mail = Mail {
        kind: MailKind::PasswordReset,
        to: "person@example.test".into(),
        subject: "Reset".into(),
        body: "private-reset-token".into(),
    };
    let logs = Logs::default();
    let send =
        tokio::spawn(async move { mailer.send(&mail).await }.with_subscriber(subscriber(&logs)));
    let (_stream, _) = listener.accept().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(11), send)
        .await
        .expect("send did not time out")
        .unwrap();
    assert_eq!(result.unwrap_err().code(), "mail_delivery_failed");
    assert!(logs.text().contains("failed"));
    assert!(!logs.text().contains("private-reset-token"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_mail_is_bilingual_in_the_users_locale_order(pool: PgPool) {
    use katwork::types::Locale;
    const TH_SUBJECT: &str = "ตั้งรหัสผ่าน KatWork ใหม่";
    const EN_SUBJECT: &str = "Reset your KatWork password";
    const TH_BODY: &str = "ใช้ลิงก์นี้ภายใน 30 นาทีเพื่อตั้งรหัสผ่านใหม่ หากคุณไม่ได้ขอเปลี่ยนรหัสผ่าน ให้ละเว้นอีเมลนี้";
    const EN_BODY: &str = "Use this link within 30 minutes to set a new password. If you did not ask for this, ignore this email.";
    let link = format!("https://katwork.example/reset?token={}", Uuid::now_v7());
    for (locale, subject, first, second) in [
        (
            Locale::Th,
            format!("{TH_SUBJECT} / {EN_SUBJECT}"),
            TH_BODY,
            EN_BODY,
        ),
        (
            Locale::En,
            format!("{EN_SUBJECT} / {TH_SUBJECT}"),
            EN_BODY,
            TH_BODY,
        ),
    ] {
        let mail = mailer::reset_mail("person@example.test".into(), locale, &link);
        assert!(matches!(mail.kind, MailKind::PasswordReset));
        assert_eq!(mail.to, "person@example.test");
        assert_eq!(mail.subject, subject);
        assert_eq!(mail.body.matches(&link).count(), 1, "{}", mail.body);
        assert_eq!(mail.body.lines().last(), Some(link.as_str()));
        let first_at = mail.body.find(first).unwrap();
        let second_at = mail.body.find(second).unwrap();
        assert!(first_at < second_at, "{}", mail.body);
    }
    // The reset route mails in the account's own locale.
    let (user_id, _) = support::user(&pool).await;
    sqlx::query("UPDATE users SET locale = 'th' WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let capture = CapturingMailer::default();
    reset::request(
        &pool,
        &capture,
        "person@example.test",
        "https://katwork.example/",
    )
    .await
    .unwrap();
    let sent = capture.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].subject, format!("{TH_SUBJECT} / {EN_SUBJECT}"));
    assert!(sent[0].body.starts_with(TH_BODY), "{}", sent[0].body);
    let last = sent[0].body.lines().last().unwrap();
    assert!(
        last.starts_with("https://katwork.example/reset?token="),
        "{last}"
    );
}
