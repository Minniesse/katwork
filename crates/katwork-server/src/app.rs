use std::sync::Arc;

use axum::{Router, routing::get};
use sqlx::PgPool;

use crate::{config::Config, routes::health};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::health))
        .with_state(state)
}
