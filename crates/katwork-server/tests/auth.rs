#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
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
    session::revoke(&pool, &token.0, std::time::Duration::from_secs(5))
        .await
        .unwrap();
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

async fn issued(pool: &PgPool, user_id: Uuid, kind: &str) -> String {
    use katwork::{
        auth::{invite, reset},
        ids::UserId,
    };
    let mut conn = pool.acquire().await.unwrap();
    match kind {
        "invite" => invite::issue(&mut conn, UserId(user_id)).await.unwrap(),
        _ => reset::issue(&mut conn, UserId(user_id)).await.unwrap(),
    }
}

async fn accept(app: &axum::Router, token: &str, password: &str) -> axum::response::Response {
    request(
        app,
        "POST",
        "/api/auth/invite/accept",
        Some(json!({"token": token, "new_password": password})),
        None,
    )
    .await
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_and_reset_tokens_cannot_be_swapped(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let invite = issued(&pool, id, "invite").await;
    let reset = issued(&pool, id, "reset").await;
    let app = app(&pool, None);
    let new_password = Uuid::now_v7().to_string();
    assert_code(
        confirm(&app, &invite, &new_password).await,
        400,
        "reset_token_invalid",
    )
    .await;
    assert_code(
        accept(&app, &reset, &new_password).await,
        400,
        "invite_token_invalid",
    )
    .await;
    // Neither refusal consumed anything: each token still works at its own route.
    assert_eq!(login(&app, &password).await.status(), 200);
    assert_eq!(confirm(&app, &reset, &new_password).await.status(), 200);
    let kinds: Vec<(String, bool)> =
        sqlx::query_as("SELECT kind, used_at IS NOT NULL FROM password_resets ORDER BY kind")
            .fetch_all(&pool)
            .await
            .unwrap();
    // The reset ended the account's other pending tokens, of both kinds.
    assert_eq!(
        kinds,
        vec![("invite".to_owned(), true), ("reset".to_owned(), true)]
    );
    let invite = issued(&pool, id, "invite").await;
    let pending = issued(&pool, id, "reset").await;
    let session = cookie(&login(&app, &new_password).await);
    let invited_password = Uuid::now_v7().to_string();
    assert_eq!(accept(&app, &invite, &invited_password).await.status(), 204);
    assert_code(
        request(&app, "GET", "/api/auth/me", None, Some(&session)).await,
        401,
        "unauthenticated",
    )
    .await;
    assert_code(
        confirm(&app, &pending, &new_password).await,
        400,
        "reset_token_invalid",
    )
    .await;
    assert_eq!(login(&app, &invited_password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_invite_accept_consumes_once(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = issued(&pool, id, "invite").await;
    let app = app(&pool, None);
    let first = Uuid::now_v7().to_string();
    let second = Uuid::now_v7().to_string();
    let (a, b) = tokio::join!(accept(&app, &token, &first), accept(&app, &token, &second));
    let mut results = Vec::new();
    for response in [a, b] {
        let status = response.status().as_u16();
        results.push((status, body(response).await));
    }
    results.sort_by_key(|(status, _)| *status);
    assert_eq!(
        results,
        vec![
            (204, serde_json::Value::Null),
            (400, json!({"code": "invite_token_invalid"}))
        ]
    );
    let used: i64 =
        sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE used_at IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(used, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn weak_password_does_not_consume_invite_token(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let token = issued(&pool, id, "invite").await;
    let app = app(&pool, None);
    for password in ["ก".repeat(7), "x".repeat(1025)] {
        assert_code(
            accept(&app, &token, &password).await,
            400,
            "password_invalid",
        )
        .await;
    }
    for token in [String::new(), "not-a-token".into(), "A".repeat(43)] {
        assert_code(
            accept(&app, &token, &Uuid::now_v7().to_string()).await,
            400,
            "invite_token_invalid",
        )
        .await;
    }
    assert_eq!(
        accept(&app, &token, &Uuid::now_v7().to_string())
            .await
            .status(),
        204
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn successful_logins_never_lock_out(pool: PgPool) {
    let (_, password) = user(&pool).await;
    let app = app(&pool, None);
    for _ in 0..30 {
        assert_eq!(login(&app, &password).await.status(), 200);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn a_successful_login_clears_the_failure_count(pool: PgPool) {
    let (_, password) = user(&pool).await;
    let app = app(&pool, None);
    let wrong = Uuid::now_v7().to_string();
    for _ in 0..9 {
        assert_eq!(login(&app, &wrong).await.status(), 401);
    }
    assert_eq!(login(&app, &password).await.status(), 200);
    for _ in 0..10 {
        assert_eq!(login(&app, &wrong).await.status(), 401);
    }
    assert_code(login(&app, &wrong).await, 429, "rate_limited").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_confirm_clears_login_lockout(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let app = app(&pool, None);
    let wrong = Uuid::now_v7().to_string();
    for _ in 0..10 {
        assert_eq!(login(&app, &wrong).await.status(), 401);
    }
    assert_code(login(&app, &password).await, 429, "rate_limited").await;
    let new_password = Uuid::now_v7().to_string();
    let token = reset_token(&pool, id).await;
    assert_eq!(confirm(&app, &token, &new_password).await.status(), 200);
    assert_eq!(login(&app, &new_password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_accept_clears_login_lockout(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let app = app(&pool, None);
    let wrong = Uuid::now_v7().to_string();
    for _ in 0..10 {
        assert_eq!(login(&app, &wrong).await.status(), 401);
    }
    let new_password = Uuid::now_v7().to_string();
    let token = issued(&pool, id, "invite").await;
    assert_eq!(accept(&app, &token, &new_password).await.status(), 204);
    assert_eq!(login(&app, &new_password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn login_429_sends_retry_after(pool: PgPool) {
    let app = app(&pool, None);
    let wrong = Uuid::now_v7().to_string();
    for _ in 0..10 {
        assert_eq!(login(&app, &wrong).await.status(), 401);
    }
    let limited = login(&app, &wrong).await;
    let retry_after: u64 = limited.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=900).contains(&retry_after), "{retry_after}");
    assert_code(limited, 429, "rate_limited").await;
}

#[sqlx::test(migrations = false)]
async fn invite_tokens_migration_keeps_existing_tokens_as_reset_tokens(pool: PgPool) {
    let mut old = sqlx::migrate!("../katwork/migrations");
    old.migrations = old
        .iter()
        .filter(|migration| migration.version <= 5)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    old.run(&pool).await.unwrap();
    let (id, _) = user(&pool).await;
    let token = reset_token(&pool, id).await;
    katwork::db::migrate(&pool).await.unwrap();
    let kind: String = sqlx::query_scalar("SELECT kind FROM password_resets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "reset");
    let refused = sqlx::query("INSERT INTO password_resets (id, user_id, token_sha256, kind, expires_at) VALUES ($1, $2, 'x', 'other', clock_timestamp())")
        .bind(Uuid::now_v7())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        refused
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514")
    );
    let app = app(&pool, None);
    assert_code(
        accept(&app, &token, &Uuid::now_v7().to_string()).await,
        400,
        "invite_token_invalid",
    )
    .await;
    assert_eq!(
        confirm(&app, &token, &Uuid::now_v7().to_string())
            .await
            .status(),
        200
    );
}

const LINK_BASE: &str = "http://localhost:3000";

fn local_space() -> Option<katwork::types::SpacePolicy> {
    Some(katwork::types::SpacePolicy {
        allow_external: false,
    })
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_create_user_creates_one_new_workspace(pool: PgPool) {
    use katwork::{
        auth::accounts::{WorkspaceRef, create_user},
        tenancy::Role,
        types::Locale,
    };
    let acme = WorkspaceRef::parse("Acme");
    let (first, second) = tokio::join!(
        create_user(
            &pool,
            "first@example.test",
            &acme,
            Role::Owner,
            local_space(),
            Locale::En,
            LINK_BASE
        ),
        create_user(
            &pool,
            "second@example.test",
            &acme,
            Role::Owner,
            local_space(),
            Locale::En,
            LINK_BASE
        ),
    );
    first.unwrap();
    second.unwrap();
    let count = |sql: &'static str| sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool);
    // The second command waits for the first and joins the workspace it created.
    assert_eq!(count("SELECT count(*) FROM workspaces").await.unwrap(), 1);
    assert_eq!(count("SELECT count(*) FROM spaces").await.unwrap(), 1);
    assert_eq!(count("SELECT count(*) FROM teams").await.unwrap(), 1);
    assert_eq!(count("SELECT count(*) FROM kats").await.unwrap(), 6);
    assert_eq!(
        count("SELECT count(*) FROM memberships WHERE role = 'owner'")
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_create_user_for_one_email_creates_one_account(pool: PgPool) {
    use katwork::{
        auth::accounts::{WorkspaceRef, create_user},
        tenancy::Role,
        types::Locale,
    };
    let workspace = Uuid::now_v7();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Acme')")
        .bind(workspace)
        .execute(&pool)
        .await
        .unwrap();
    for (reference, email) in [
        (
            WorkspaceRef::parse(&workspace.to_string()),
            "by-id@example.test",
        ),
        (WorkspaceRef::parse("Acme"), "by-name@example.test"),
    ] {
        let shouted = format!(" {} ", email.to_uppercase());
        let (first, second) = tokio::join!(
            create_user(
                &pool,
                email,
                &reference,
                Role::Member,
                None,
                Locale::En,
                LINK_BASE
            ),
            create_user(
                &pool,
                &shouted,
                &reference,
                Role::Admin,
                None,
                Locale::Th,
                LINK_BASE
            ),
        );
        let mut codes: Vec<_> = [first, second]
            .into_iter()
            .map(|result| result.map(|_| "ok").unwrap_or_else(|error| error.code()))
            .collect();
        codes.sort();
        assert_eq!(codes, ["ok", "user_exists"], "{reference:?}");
        for sql in [
            "SELECT count(*) FROM users WHERE email = $1",
            "SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id WHERE u.email = $1",
            "SELECT count(*) FROM password_resets r JOIN users u ON u.id = r.user_id WHERE u.email = $1",
        ] {
            let rows: i64 = sqlx::query_scalar(sql)
                .bind(email)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(rows, 1, "{sql} {reference:?}");
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_confirm_racing_invite_accept_has_one_winner(pool: PgPool) {
    let (id, _) = user(&pool).await;
    let app = app(&pool, None);
    for _ in 0..10 {
        let reset = issued(&pool, id, "reset").await;
        let invite = issued(&pool, id, "invite").await;
        let reset_password = Uuid::now_v7().to_string();
        let invite_password = Uuid::now_v7().to_string();
        let (confirmed, accepted) = tokio::join!(
            confirm(&app, &reset, &reset_password),
            accept(&app, &invite, &invite_password)
        );
        // Each use ends the account's other pending tokens, so exactly one of them wins.
        let winner = match (confirmed.status().as_u16(), accepted.status().as_u16()) {
            (200, 400) => {
                assert_code(accepted, 400, "invite_token_invalid").await;
                reset_password
            }
            (400, 204) => {
                assert_code(confirmed, 400, "reset_token_invalid").await;
                invite_password
            }
            other => panic!("expected exactly one winner, got {other:?}"),
        };
        assert_eq!(login(&app, &winner).await.status(), 200);
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE used_at IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending, 0);
    }
}
