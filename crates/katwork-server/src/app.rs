use std::{path::Path, sync::Arc};

use axum::{
    Extension, Router,
    routing::{get, post, put},
};
use katwork::runtime::Core;

use crate::{
    routes::{auth, health, kats, teams, threads, workspaces},
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
        .route(
            "/api/workspaces/{workspace_id}/spaces/{space_id}/kats",
            get(kats::list).post(kats::hire),
        )
        .route(
            "/api/workspaces/{workspace_id}/kats/{kat_id}",
            get(kats::get).patch(kats::update),
        )
        .route(
            "/api/workspaces/{workspace_id}/kats/{kat_id}/spaces/{space_id}",
            put(kats::grant).delete(kats::revoke),
        )
        .route(
            "/api/workspaces/{workspace_id}/spaces/{space_id}/teams",
            get(teams::list).post(teams::create),
        )
        .route(
            "/api/workspaces/{workspace_id}/teams/{team_id}",
            get(teams::get),
        )
        .route(
            "/api/workspaces/{workspace_id}/teams/{team_id}/members/{kat_id}",
            put(teams::add_member).delete(teams::remove_member),
        )
        .route(
            "/api/workspaces/{workspace_id}/threads",
            post(threads::create),
        )
        .route(
            "/api/workspaces/{workspace_id}/spaces/{space_id}/threads",
            get(threads::list),
        )
        .route(
            "/api/workspaces/{workspace_id}/threads/{thread_id}",
            get(threads::get),
        )
        .route(
            "/api/workspaces/{workspace_id}/threads/{thread_id}/messages",
            get(threads::messages).post(threads::post_message),
        )
        .fallback_service(static_files::service(web_dir.to_owned()))
        .layer(Extension(auth::LoginLimiter::default()))
        .layer(Extension(auth::ResetLimiter::default()))
        .layer(Extension(auth::ResetWork::default()))
        .with_state(state);
    crate::guards::apply(router, &config)
}
