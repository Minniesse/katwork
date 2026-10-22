use std::sync::Arc;

use axum::{Router, body::Body, http::Request, response::Response};
use katwork::{config::Config, runtime::Core};
use katwork_server::app::{AppState, build_router};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

fn app() -> Router {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        "KATWORK_PUBLIC_URL" => Some("https://katwork.example:8443".into()),
        _ => None,
    })
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    build_router(AppState {
        core: Arc::new(Core::new(pool, Arc::new(config))),
    })
}

async fn code(response: Response, status: u16, code: &str) {
    assert_eq!(response.status(), status);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"code": code})
    );
}

#[tokio::test]
async fn cross_site_post_is_403() {
    let response = app()
        .oneshot(
            Request::post("/api/auth/login")
                .header("host", "localhost")
                .header("origin", "https://evil.example")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    code(response, 403, "cross_site_request").await;
}

#[tokio::test]
async fn unrecognized_host_is_403() {
    for host in [
        None,
        Some("evil.example"),
        Some("localhost.evil.example"),
        Some("localhost@evil.example"),
        Some("localhost:bad"),
    ] {
        let mut request = Request::get("/healthz");
        if let Some(host) = host {
            request = request.header("host", host);
        }
        code(
            app()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap(),
            403,
            "host_not_allowed",
        )
        .await;
    }
}

#[tokio::test]
async fn allowed_host_ignores_port() {
    for host in [
        "localhost",
        "localhost:80",
        "katwork.example:9443",
        "KATWORK.EXAMPLE",
    ] {
        let response = app()
            .oneshot(
                Request::get("/healthz")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}

#[tokio::test]
async fn mutations_require_exact_origin_and_reject_cross_site_fetch() {
    for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        for origin in [
            None,
            Some("null"),
            Some("https://katwork.example"),
            Some("https://katwork.example:8443/"),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri("/api/auth/logout")
                .header("host", "localhost");
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            code(
                app()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap(),
                403,
                "cross_site_request",
            )
            .await;
        }
    }
    code(
        app()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header("host", "localhost")
                    .header("origin", "https://katwork.example:8443")
                    .header("sec-fetch-site", "cross-site")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}

#[tokio::test]
async fn non_json_mutation_is_415() {
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/jsonp"),
        Some("application/x-www-form-urlencoded"),
    ] {
        let mut request = Request::post("/api/auth/login")
            .header("host", "localhost")
            .header("origin", "https://katwork.example:8443");
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        code(
            app()
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap(),
            415,
            "json_required",
        )
        .await;
    }
}

#[tokio::test]
async fn responses_carry_nosniff_and_csp() {
    for (uri, host) in [
        ("/healthz", "localhost"),
        ("/missing", "localhost"),
        ("/healthz", "evil.example"),
    ] {
        let response = app()
            .oneshot(
                Request::get(uri)
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "default-src 'self'; frame-ancestors 'none'"
        );
    }
}

#[tokio::test]
async fn duplicate_host_and_origin_headers_are_rejected() {
    code(
        app()
            .oneshot(
                Request::get("/healthz")
                    .header("host", "localhost")
                    .header("host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "host_not_allowed",
    )
    .await;
    code(
        app()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header("host", "localhost")
                    .header("origin", "https://katwork.example:8443")
                    .header("origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}
