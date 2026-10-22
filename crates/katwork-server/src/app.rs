use std::sync::Arc;

use axum::{
    Extension, Router,
    routing::{get, post, put},
};
use katwork::runtime::Core;

use crate::routes::{auth, health, workspaces};

#[derive(Clone)]
pub struct AppState {
    pub core: Arc<Core>,
}

pub fn build_router(state: AppState) -> Router {
    let config = state.core.config.clone();
    let router = Router::new()
        .route("/healthz", get(health::health))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/reset/request", post(auth::request_reset))
        .route("/api/auth/reset/confirm", post(auth::confirm_reset))
        .route("/api/auth/me", get(auth::me))
        .route(
            "/api/workspaces",
            get(workspaces::list).post(workspaces::create),
        )
        .route(
            "/api/workspaces/{workspace_id}/spaces",
            get(workspaces::list_spaces).post(workspaces::create_space),
        )
        .route(
            "/api/workspaces/{workspace_id}/paused",
            put(workspaces::set_paused),
        )
        .layer(Extension(auth::LoginLimiter::default()))
        .layer(Extension(auth::ResetLimiter::default()))
        .layer(Extension(auth::ResetWork::default()))
        .with_state(state);
    crate::guards::apply(router, &config)
}
