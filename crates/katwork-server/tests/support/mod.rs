pub mod cancel_sql;
pub mod locks;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{Router, body::Body, extract::ConnectInfo, http::Request, response::Response};
use katwork::{config::Config, runtime::Core};
use katwork_server::app::{AppState, build_router};
use rand_core::OsRng;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

/// Every connection of `pool` is idle in it, and no other client backend of the test database
/// is busy or inside a transaction. Opens its own observer connection, so it never takes a
/// permit a test pool shares.
#[allow(dead_code, reason = "Only some test binaries cancel database work")]
pub async fn assert_pool_clean(pool: &PgPool) {
    // sqlx test pools share permits: inspecting a pool must never grow it to maximum capacity.
    timeout(Duration::from_secs(30), async {
        use sqlx::Connection;
        let mut observer = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .unwrap();
        loop {
            let clean: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid() AND state IS DISTINCT FROM 'idle')")
                .fetch_one(&mut observer).await.unwrap();
            if pool.num_idle() as u32 == pool.size() && clean {
                break;
            }
            tokio::task::yield_now().await;
        }
        observer.close().await.unwrap();
    })
    .await
    .expect("worker shutdown left a busy pool or a backend inside a transaction");
}

pub fn config(public_url: Option<&str>) -> Config {
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        "KATWORK_PUBLIC_URL" => public_url.map(str::to_owned),
        _ => None,
    })
    .unwrap()
}

pub fn app(pool: &PgPool, public_url: Option<&str>) -> Router {
    build_router(AppState {
        core: Arc::new(Core::new(pool.clone(), Arc::new(config(public_url)))),
    })
}

pub async fn user(pool: &PgPool) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let password = Uuid::now_v7().to_string();
    // These fixtures test auth behavior; production hashing parameters have a separate regression test.
    let hash = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(1024, 1, 1, None).unwrap(),
    )
    .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
    .unwrap()
    .to_string();
    sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'person@example.test', $2, 'Person', 'en')")
        .bind(id).bind(hash).execute(pool).await.unwrap();
    (id, password)
}

pub async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Response {
    request_from(app, method, path, body, cookie, "127.0.0.1:7000").await
}

pub async fn request_from(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    address: &str,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("origin", "http://localhost:3000");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let body = match body {
        Some(value) => {
            request = request.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(address.parse::<SocketAddr>().unwrap()));
    app.clone().oneshot(request).await.unwrap()
}

pub async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

pub async fn assert_code(response: Response, status: u16, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(body(response).await, json!({"code": code}));
}

pub async fn login(app: &Router, password: &str) -> Response {
    request(
        app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "person@example.test", "password": password})),
        None,
    )
    .await
}

pub fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
