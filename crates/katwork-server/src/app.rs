use std::{path::Path, sync::Arc};

use axum::{
    Extension, Router,
    routing::{get, post, put},
};
use katwork::runtime::Core;

use crate::{
    routes::{auth, health, workspaces},
    static_files,
};

#[derive(Clone)]
pub struct AppState {
    pub core: Arc<Core>,
}

pub fn build_router(state: AppState) -> Router {
    build_router_with_web_dir(state, Path::new(static_files::DEFAULT_WEB_DIR))
}

/// `build_router` serving the web app from `web_dir` instead of the default.
pub fn build_router_with_web_dir(state: AppState, web_dir: &Path) -> Router {
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
        .fallback_service(static_files::service(web_dir.to_owned()))
        .layer(Extension(auth::LoginLimiter::default()))
        .layer(Extension(auth::ResetLimiter::default()))
        .layer(Extension(auth::ResetWork::default()))
        .with_state(state);
    crate::guards::apply(router, &config)
}
