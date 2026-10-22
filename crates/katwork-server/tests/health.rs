use std::sync::Arc;

use axum::{body::Body, http::Request};
use katwork_server::{
    app::{AppState, build_router},
    config::Config,
};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_ok_and_version() {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some("test-only-secret-at-least-32-bytes".into()),
        _ => None,
    })
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    let app = build_router(AppState {
        pool,
        config: Arc::new(config),
    });
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
        })
    );
}
