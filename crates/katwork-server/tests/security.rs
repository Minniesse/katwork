#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;

use axum::{body::Body, extract::ConnectInfo, http::Request};
use katwork::{
    auth::{mailer::CapturingMailer, reset},
    config::Config,
    runtime::Core,
};
use katwork_server::app::{AppState, build_router};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    time::{Instant, timeout},
};
use tower::ServiceExt;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_response_never_waits_for_account_work(pool: PgPool) {
    support::user(&pool).await;
    let single = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let held = single.acquire().await.unwrap();
    let app = support::app(&single, None);
    for _ in 0..30 {
        for email in ["person@example.test", "unknown@example.test"] {
            let response = timeout(
                Duration::from_millis(100),
                support::request(
                    &app,
                    "POST",
                    "/api/auth/reset/request",
                    Some(json!({"email": email})),
                    None,
                ),
            )
            .await
            .expect("response waited for the database");
            assert_eq!(response.status(), 202);
        }
    }
    drop(held);
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn stalled_smtp_does_not_delay_http_and_disconnects_after_ten_seconds(pool: PgPool) {
    support::user(&pool).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = support::config(None);
    config.smtp = Some(katwork::config::SmtpConfig {
        host: "127.0.0.1".into(),
        port: Some(listener.local_addr().unwrap().port()),
        username: None,
        password: None,
        from: None,
    });
    let app = build_router(AppState {
        core: Arc::new(Core::new(pool, Arc::new(config))),
    });
    for email in ["unknown@example.test", "person@example.test"] {
        let response = timeout(
            Duration::from_millis(100),
            support::request(
                &app,
                "POST",
                "/api/auth/reset/request",
                Some(json!({"email": email})),
                None,
            ),
        )
        .await
        .expect("SMTP delayed HTTP response");
        assert_eq!(response.status(), 202);
    }
    let (mut stream, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let started = Instant::now();
    let read = timeout(Duration::from_secs(11), stream.read(&mut [0]))
        .await
        .expect("SMTP socket remained open past total send timeout");
    assert_eq!(read.unwrap(), 0);
    assert!(started.elapsed() >= Duration::from_secs(9));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_tokens_are_capped_at_three_even_with_concurrent_requests(pool: PgPool) {
    support::user(&pool).await;
    let mailer = Arc::new(CapturingMailer::default());
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let pool = pool.clone();
        let mailer = mailer.clone();
        tasks.spawn(async move {
            reset::request(
                &pool,
                mailer.as_ref(),
                "person@example.test",
                "http://localhost:3000",
            )
            .await
            .unwrap()
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE used_at IS NULL AND expires_at > clock_timestamp()")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(count, 3);
    assert_eq!(mailer.sent().len(), 3);
    sqlx::query("UPDATE password_resets SET expires_at = clock_timestamp() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    reset::request(
        &pool,
        mailer.as_ref(),
        "person@example.test",
        "http://localhost:3000",
    )
    .await
    .unwrap();
    assert_eq!(mailer.sent().len(), 4);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn forwarded_ips_are_used_only_behind_trusted_proxies(pool: PgPool) {
    for trusted in [false, true] {
        let config = Config::from_lookup(|name| match name {
            "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
            "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
            "KATWORK_TRUSTED_PROXIES" if trusted => Some("127.0.0.0/8, ::1/128".into()),
            _ => None,
        })
        .unwrap();
        let app = build_router(AppState {
            core: Arc::new(Core::new(pool.clone(), Arc::new(config))),
        });
        for i in 0..11 {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/auth/login")
                        .header("host", "localhost")
                        .header("origin", "http://localhost:3000")
                        .header("content-type", "application/json")
                        .header(
                            "x-forwarded-for",
                            if i < 10 { "192.0.2.1" } else { "192.0.2.2" },
                        )
                        .extension(ConnectInfo(
                            "127.0.0.1:7000".parse::<std::net::SocketAddr>().unwrap(),
                        ))
                        .body(Body::from(
                            json!({"email": "unknown@example.test", "password": "x".repeat(1025)})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), if i < 10 || trusted { 401 } else { 429 });
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_does_not_reuse_a_preexisting_session_cookie(pool: PgPool) {
    let (_, password) = support::user(&pool).await;
    let app = support::app(&pool, None);
    let first = support::cookie(&support::login(&app, &password).await);
    let response = support::request(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "person@example.test", "password": password})),
        Some(&first),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_ne!(support::cookie(&response), first);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn me_stays_responsive_during_fifty_logins_with_two_connections(pool: PgPool) {
    let (id, _) = support::user(&pool).await;
    let token = katwork::auth::session::create(&pool, katwork::ids::UserId(id))
        .await
        .unwrap();
    let cookie = format!("kw_session={}", token.0);
    let small = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let app = support::app(&small, None);
    let mut tasks = tokio::task::JoinSet::new();
    for i in 1..=50 {
        let app = app.clone();
        tasks.spawn(async move {
            support::request_from(
                &app,
                "POST",
                "/api/auth/login",
                Some(json!({"email": "person@example.test", "password": "wrong-password"})),
                None,
                &format!("192.0.2.{i}:7000"),
            )
            .await
            .status()
        });
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND state = 'idle in transaction'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(held, 0, "password hashing holds a transaction/connection");
    for _ in 0..10 {
        let response = timeout(
            Duration::from_millis(100),
            support::request(&app, "GET", "/api/auth/me", None, Some(&cookie)),
        )
        .await
        .expect("login traffic starved /me of a DB connection");
        assert_eq!(response.status(), 200);
    }
    while let Some(result) = tasks.join_next().await {
        assert!([401, 503].contains(&result.unwrap().as_u16()));
    }
    small.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_limits_apply_before_lookup_and_always_return_202(pool: PgPool) {
    let (id, _) = support::user(&pool).await;
    let capture = Arc::new(CapturingMailer::default());
    let mut core = Core::new(pool.clone(), Arc::new(support::config(None)));
    core.mailer = capture.clone();
    let app = build_router(AppState {
        core: Arc::new(core),
    });
    for i in 1..=3 {
        let response = support::request(
            &app,
            "POST",
            "/api/auth/reset/request",
            Some(json!({"email": " PERSON@example.test "})),
            None,
        )
        .await;
        assert_eq!(response.status(), 202);
        timeout(Duration::from_secs(2), async {
            while capture.sent().len() != i {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        sqlx::query("UPDATE password_resets SET used_at = clock_timestamp() WHERE user_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        support::request_from(
            &app,
            "POST",
            "/api/auth/reset/request",
            Some(json!({"email": "person@example.test"})),
            None,
            "192.0.2.9:7000"
        )
        .await
        .status(),
        202
    );
    // Exhaust the unknown address, then create it: its earlier attempts must still count.
    for _ in 0..3 {
        assert_eq!(
            support::request(
                &app,
                "POST",
                "/api/auth/reset/request",
                Some(json!({"email": "unknown@example.test"})),
                None
            )
            .await
            .status(),
            202
        );
    }
    // Let the accepted background lookups finish before the account becomes known.
    tokio::time::sleep(Duration::from_millis(50)).await;
    sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'unknown@example.test', 'hash', 'Unknown', 'en')")
        .bind(uuid::Uuid::now_v7()).execute(&pool).await.unwrap();
    assert_eq!(
        support::request(
            &app,
            "POST",
            "/api/auth/reset/request",
            Some(json!({"email": "unknown@example.test"})),
            None
        )
        .await
        .status(),
        202
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(capture.sent().len(), 3);

    // A fresh IP gets twenty total requests, including unknown addresses.
    for i in 0..20 {
        assert_eq!(
            support::request_from(
                &app,
                "POST",
                "/api/auth/reset/request",
                Some(json!({"email": format!("nobody-{i}@example.test")})),
                None,
                "192.0.2.10:7000"
            )
            .await
            .status(),
            202
        );
    }
    assert_eq!(
        support::request_from(
            &app,
            "POST",
            "/api/auth/reset/request",
            Some(json!({"email": "unknown@example.test"})),
            None,
            "192.0.2.10:7000"
        )
        .await
        .status(),
        202
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(capture.sent().len(), 3);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_rechecks_password_after_concurrent_password_change(pool: PgPool) {
    let (id, password) = support::user(&pool).await;
    let mut change = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(id)
        .fetch_one(&mut *change)
        .await
        .unwrap();
    let app = support::app(&pool, None);
    let login = tokio::spawn(async move { support::login(&app, &password).await });
    timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT password_hash%')")
                .fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    sqlx::query("UPDATE users SET password_hash = 'changed' WHERE id = $1")
        .bind(id)
        .execute(&mut *change)
        .await
        .unwrap();
    change.commit().await.unwrap();
    support::assert_code(login.await.unwrap(), 401, "unauthenticated").await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn ten_thousand_sprayed_emails_do_not_block_another_users_login(pool: PgPool) {
    let (_, password) = support::user(&pool).await;
    let app = support::app(&pool, None);
    for i in 0..10_100 {
        let response = support::request_from(
            &app,
            "POST",
            "/api/auth/login",
            Some(json!({"email": format!("spray-{i}@example.test"), "password": "x".repeat(1025)})),
            None,
            "192.0.2.1:7000",
        )
        .await;
        assert_eq!(response.status(), if i < 100 { 401 } else { 429 });
    }
    let response = support::request_from(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "person@example.test", "password": password})),
        None,
        "192.0.2.2:7000",
    )
    .await;
    assert_eq!(response.status(), 200);
}
