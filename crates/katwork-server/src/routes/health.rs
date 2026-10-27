use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::app::AppState;

/// 503 `doc_pool_wedged` while every document thread is stuck, which turns the Compose
/// healthcheck red; the watchdog then restarts the process.
pub async fn health(State(state): State<AppState>) -> Response {
    if state.core.docs.wedged() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"code": "doc_pool_wedged"})),
        )
            .into_response();
    }
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}
