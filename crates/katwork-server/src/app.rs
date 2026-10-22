use std::sync::Arc;

use axum::{Router, routing::get};
use katwork::runtime::Core;

use crate::routes::health;

#[derive(Clone)]
pub struct AppState {
    pub core: Arc<Core>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::health))
        .with_state(state)
}
