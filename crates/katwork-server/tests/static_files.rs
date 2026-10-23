#[allow(dead_code)]
mod support;

use std::{path::PathBuf, sync::Arc};

use axum::{Router, body::to_bytes, http::Request};
use katwork::runtime::Core;
use katwork_server::app::{AppState, build_router_with_web_dir};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

struct WebDir(PathBuf);

impl WebDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("katwork-web-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>KatWork</title>",
        )
        .unwrap();
        std::fs::write(dir.join("assets/app-1234.js"), "console.log('app')").unwrap();
        std::fs::write(dir.join("theme-init.js"), "// theme").unwrap();
        Self(dir)
    }
}

impl Drop for WebDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(web: &WebDir) -> Router {
    // Static files never touch the database; the pool is never connected.
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://localhost/unused")
        .unwrap();
    build_router_with_web_dir(
        AppState {
            core: Arc::new(Core::new(pool, Arc::new(support::config(None)))),
        },
        &web.0,
    )
}

async fn get(app: &Router, path: &str) -> (u16, String, String) {
    let request = Request::builder()
        .uri(path)
        .header("host", "localhost")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn spa_routes_fall_back_to_index() {
    let web = WebDir::new();
    let app = app(&web);
    for path in ["/", "/login", "/reset?token=abc", "/workspaces/1/chat"] {
        let (status, content_type, body) = get(&app, path).await;
        assert_eq!(status, 200, "{path}");
        assert!(
            content_type.starts_with("text/html"),
            "{path}: {content_type}"
        );
        assert!(body.contains("<title>KatWork</title>"), "{path}");
    }
}

#[tokio::test]
async fn built_assets_are_served_with_their_type() {
    let web = WebDir::new();
    let app = app(&web);
    let (status, content_type, body) = get(&app, "/assets/app-1234.js").await;
    assert_eq!(status, 200);
    assert!(content_type.contains("javascript"), "{content_type}");
    assert_eq!(body, "console.log('app')");
    let (status, _, _) = get(&app, "/theme-init.js").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn missing_asset_is_404_not_index() {
    let web = WebDir::new();
    let app = app(&web);
    for path in [
        "/assets/missing.js",
        "/favicon.ico",
        "/../Cargo.toml",
        "/%2e%2e/Cargo.toml",
    ] {
        let (status, _, body) = get(&app, path).await;
        assert_eq!(status, 404, "{path}");
        assert!(!body.contains("KatWork"), "{path}");
    }
}

#[tokio::test]
async fn api_paths_never_fall_back() {
    let web = WebDir::new();
    let app = app(&web);
    for path in [
        "/api",
        "/api/",
        "/api/nope",
        "/api/auth/nope",
        "/api/workspaces/1/x",
    ] {
        let (status, content_type, body) = get(&app, path).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(content_type, "application/json", "{path}");
        assert_eq!(body, r#"{"code":"not_found"}"#, "{path}");
    }
    let (status, _, body) = get(&app, "/api/auth/login").await;
    assert_eq!(status, 405);
    assert!(!body.contains("KatWork"));
}

#[tokio::test]
async fn static_responses_carry_security_headers() {
    let web = WebDir::new();
    let app = app(&web);
    for path in ["/", "/assets/app-1234.js"] {
        let request = Request::builder()
            .uri(path)
            .header("host", "localhost")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let headers = response.headers();
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(
            headers["content-security-policy"],
            "default-src 'self'; frame-ancestors 'none'"
        );
    }
    let index = Request::builder()
        .uri("/login")
        .header("host", "localhost")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(index).await.unwrap();
    assert_eq!(response.headers()["cache-control"], "no-cache");
    let asset = Request::builder()
        .uri("/assets/app-1234.js")
        .header("host", "localhost")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(asset).await.unwrap();
    assert_eq!(
        response.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn unknown_host_is_refused_before_static_files() {
    let web = WebDir::new();
    let app = app(&web);
    let request = Request::builder()
        .uri("/")
        .header("host", "evil.example")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 403);
}
