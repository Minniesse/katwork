mod support;

use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use support::*;
use tower::ServiceExt;
use uuid::Uuid;

async fn reset_token(pool: &PgPool, user_id: Uuid) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    sqlx::query("INSERT INTO password_resets (id, user_id, token_sha256, expires_at) VALUES ($1, $2, $3, clock_timestamp() + interval '30 minutes')")
        .bind(Uuid::now_v7()).bind(user_id).bind(format!("{:x}", Sha256::digest(token.as_bytes()))).execute(pool).await.unwrap();
    token
}

async fn confirm(app: &axum::Router, token: &str, password: &str) -> axum::response::Response {
    request(
        app,
        "POST",
        "/api/auth/reset/confirm",
        Some(json!({"token": token, "new_password": password})),
        None,
    )
    .await
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_with_correct_password_sets_cookie(pool: PgPool) {
    let (user_id, password) = user(&pool).await;
    let app = app(&pool, None);
    let response = login(&app, &password).await;
    assert_eq!(response.status(), 200);
    let header = response.headers()["set-cookie"].to_str().unwrap();
    for flag in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=1209600"] {
        assert!(header.contains(flag));
    }
    let cookie = cookie(&response);
    assert_eq!(
        body(request(&app, "GET", "/api/auth/me", None, Some(&cookie)).await).await,
        json!({"user_id": user_id})
    );
    let raw = cookie.strip_prefix("kw_session=").unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(raw).unwrap().len(), 32);
    let stored: String = sqlx::query_scalar("SELECT token_sha256 FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, format!("{:x}", Sha256::digest(raw.as_bytes())));
    let duration: bool = sqlx::query_scalar("SELECT expires_at BETWEEN created_at + interval '14 days' - interval '1 second' AND created_at + interval '14 days' + interval '1 second' FROM sessions WHERE user_id = $1")
        .bind(user_id).fetch_one(&pool).await.unwrap();
    assert!(duration);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_with_wrong_password_is_401_without_detail(pool: PgPool) {
    user(&pool).await;
    let app = app(&pool, None);
    let wrong = Uuid::now_v7().to_string();
    let known = login(&app, &wrong).await;
    let unknown = request(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "unknown@example.test", "password": wrong})),
        None,
    )
    .await;
    assert_code(known, 401, "unauthenticated").await;
    assert_code(unknown, 401, "unauthenticated").await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_request_same_response_for_unknown_email(pool: PgPool) {
    user(&pool).await;
    let app = app(&pool, None);
    for email in ["person@example.test", "unknown@example.test"] {
        let response = request(
            &app,
            "POST",
            "/api/auth/reset/request",
            Some(json!({"email": email})),
            None,
        )
        .await;
        assert_eq!(response.status(), 202);
        assert_eq!(body(response).await, json!({}));
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_token_single_use(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = reset_token(&pool, id).await;
    let app = app(&pool, None);
    let password = Uuid::now_v7().to_string();
    assert_eq!(confirm(&app, &token, &password).await.status(), 200);
    assert_code(
        confirm(&app, &token, &password).await,
        400,
        "reset_token_invalid",
    )
    .await;
    assert_eq!(login(&app, &password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_token_expires_after_30_minutes(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = reset_token(&pool, id).await;
    sqlx::query("UPDATE password_resets SET expires_at = clock_timestamp() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_code(
        confirm(&app(&pool, None), &token, &Uuid::now_v7().to_string()).await,
        400,
        "reset_token_invalid",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_revokes_existing_sessions(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let app = app(&pool, None);
    let first = cookie(&login(&app, &password).await);
    let second = cookie(&login(&app, &password).await);
    let token = reset_token(&pool, id).await;
    let new_password = Uuid::now_v7().to_string();
    assert_eq!(confirm(&app, &token, &new_password).await.status(), 200);
    for cookie in [first, second] {
        assert_code(
            request(&app, "GET", "/api/auth/me", None, Some(&cookie)).await,
            401,
            "unauthenticated",
        )
        .await;
    }
    assert_code(login(&app, &password).await, 401, "unauthenticated").await;
    assert_eq!(login(&app, &new_password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn logout_revokes_cookie_and_expired_sessions_fail(pool: PgPool) {
    let (_, password) = user(&pool).await;
    let app = app(&pool, None);
    let token = cookie(&login(&app, &password).await);
    let response = request(&app, "POST", "/api/auth/logout", None, Some(&token)).await;
    assert_eq!(response.status(), 200);
    let header = response.headers()["set-cookie"].to_str().unwrap();
    assert!(header.contains("Max-Age=0"));
    assert!(header.contains("HttpOnly"));
    assert_code(
        request(&app, "GET", "/api/auth/me", None, Some(&token)).await,
        401,
        "unauthenticated",
    )
    .await;
    let token = cookie(&login(&app, &password).await);
    sqlx::query("UPDATE sessions SET expires_at = clock_timestamp() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_code(
        request(&app, "GET", "/api/auth/me", None, Some(&token)).await,
        401,
        "unauthenticated",
    )
    .await;
    assert_code(
        request(&app, "GET", "/api/auth/me", None, None).await,
        401,
        "unauthenticated",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cookie_secure_when_public_url_is_https(pool: PgPool) {
    let (_, password) = user(&pool).await;
    let app = app(&pool, Some("https://katwork.example/"));
    let response = app
        .oneshot(
            Request::post("/api/auth/login")
                .header("host", "katwork.example")
                .header("origin", "https://katwork.example")
                .header("content-type", "application/json")
                .extension(axum::extract::ConnectInfo(
                    "127.0.0.1:7000".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .body(Body::from(
                    json!({"email": "person@example.test", "password": password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .any(|part| part.trim() == "Secure")
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cookie_not_secure_on_http_public_url(pool: PgPool) {
    let (_, password) = user(&pool).await;
    for url in [None, Some("http://localhost:3000")] {
        let response = login(&app(&pool, url), &password).await;
        assert_eq!(response.status(), 200);
        assert!(
            !response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_rate_limited_per_email_and_peer_ip(pool: PgPool) {
    let app = app(&pool, None);
    let password = Uuid::now_v7().to_string();
    for _ in 0..10 {
        assert_eq!(login(&app, &password).await.status(), 401);
    }
    assert_code(login(&app, &password).await, 429, "rate_limited").await;
    let body = Some(json!({"email": "person@example.test", "password": password}));
    assert_eq!(
        request_from(
            &app,
            "POST",
            "/api/auth/login",
            body,
            None,
            "127.0.0.2:7000"
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/auth/login",
            Some(json!({"email": "other@example.test", "password": password})),
            None
        )
        .await
        .status(),
        401
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_reset_consumes_token_once(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = reset_token(&pool, id).await;
    let app = app(&pool, None);
    let first = Uuid::now_v7().to_string();
    let second = Uuid::now_v7().to_string();
    let (a, b) = tokio::join!(
        confirm(&app, &token, &first),
        confirm(&app, &token, &second)
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 400]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn auth_rejects_invalid_json_and_oversized_bodies(pool: PgPool) {
    let app = app(&pool, None);
    for (body, status, code) in [
        ("{".to_owned(), 400, "request_invalid"),
        ("x".repeat(1_048_577), 413, "body_too_large"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header("host", "localhost")
                    .header("origin", "http://localhost:3000")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_code(response, status, code).await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn weak_or_oversized_password_does_not_consume_reset_token(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = reset_token(&pool, id).await;
    let app = app(&pool, None);
    for password in ["ก".repeat(7), "x".repeat(1025)] {
        assert_code(
            confirm(&app, &token, &password).await,
            400,
            "password_invalid",
        )
        .await;
    }
    assert_eq!(
        confirm(&app, &token, &Uuid::now_v7().to_string())
            .await
            .status(),
        200
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_invalidates_other_pending_reset_tokens(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let first = reset_token(&pool, id).await;
    let second = reset_token(&pool, id).await;
    let app = app(&pool, None);
    let password = Uuid::now_v7().to_string();
    assert_eq!(confirm(&app, &first, &password).await.status(), 200);
    assert_code(
        confirm(&app, &second, &password).await,
        400,
        "reset_token_invalid",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn public_session_interface_authenticates_and_revokes(pool: PgPool) {
    use katwork::{auth::session, ids::UserId};
    let (id, _) = user(&pool).await;
    let token = session::create(&pool, UserId(id)).await.unwrap();
    assert!(!format!("{token:?}").contains(&token.0));
    assert_eq!(
        session::authenticate(&pool, &token.0)
            .await
            .unwrap()
            .user_id,
        UserId(id)
    );
    session::revoke(&pool, &token.0).await.unwrap();
    assert_eq!(
        session::authenticate(&pool, &token.0)
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
}

#[test]
fn password_hashes_use_argon2id_unique_salts_and_reject_corruption() {
    use katwork::auth::password;
    let value = Uuid::now_v7().to_string();
    let first = password::hash(&value).unwrap();
    let second = password::hash(&value).unwrap();
    assert!(first.starts_with("$argon2id$v=19$"));
    assert_ne!(first, second);
    assert!(password::verify(&value, &first));
    assert!(!password::verify(&Uuid::now_v7().to_string(), &first));
    assert!(!password::verify(&value, "corrupt"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn duplicate_session_cookies_fail_closed(pool: PgPool) {
    let (_, password) = user(&pool).await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let duplicated = format!("{cookie}; {cookie}");
    assert_code(
        request(&app, "GET", "/api/auth/me", None, Some(&duplicated)).await,
        401,
        "unauthenticated",
    )
    .await;
}

#[sqlx::test(migrations = false)]
async fn migration_normalizes_existing_email_and_login_accepts_any_case(pool: PgPool) {
    let mut old = sqlx::migrate!("../katwork/migrations");
    old.migrations = old
        .iter()
        .filter(|migration| migration.version <= 2)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    old.run(&pool).await.unwrap();
    let (id, password) = user(&pool).await;
    sqlx::query("UPDATE users SET email = ' Mixed@Example.test ' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    katwork::db::migrate(&pool).await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "mixed@example.test");
    let app = app(&pool, None);
    for email in [
        "mixed@example.test",
        "MIXED@EXAMPLE.TEST",
        " Mixed@Example.test ",
    ] {
        let response = request(
            &app,
            "POST",
            "/api/auth/login",
            Some(json!({"email": email, "password": password})),
            None,
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(body(response).await, json!({"user_id": id}));
    }
}
