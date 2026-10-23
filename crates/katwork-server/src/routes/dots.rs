use crate::{
    app::AppState,
    error::ApiError,
    extract::{path as route_path, payload},
};
use axum::{
    Json,
    extract::{
        Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::StatusCode,
};
use katwork::{
    ids::{DotId, SpaceId},
    team::{Dot, DotPatch, HireDot, dot, roles},
    tenancy::Tenant,
    types::Locale,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct SpacePath {
    space_id: SpaceId,
}
#[derive(Deserialize)]
pub(crate) struct DotPath {
    dot_id: DotId,
}
#[derive(Deserialize)]
pub(crate) struct GrantPath {
    dot_id: DotId,
    space_id: SpaceId,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
) -> Result<Json<Vec<Dot>>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        dot::list(&state.core.pool, &tenant, path.space_id).await?,
    ))
}
pub(crate) async fn hire(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    body: Result<Json<HireDot>, JsonRejection>,
) -> Result<(StatusCode, Json<Dot>), ApiError> {
    let path = route_path(path)?;
    let body = payload(body)?;
    let locale: String = sqlx::query_scalar("SELECT locale FROM users WHERE id=$1")
        .bind(tenant.user_id())
        .fetch_one(&state.core.pool)
        .await
        .map_err(katwork::error::KatError::from)?;
    let mut new = roles::template(
        &body.role,
        if locale == "th" {
            Locale::Th
        } else {
            Locale::En
        },
    );
    new.name = body.name;
    new.persona = body.persona;
    let id = dot::hire(&state.core.pool, &tenant, path.space_id, new).await?;
    Ok((
        StatusCode::CREATED,
        Json(dot::get(&state.core.pool, &tenant, id).await?),
    ))
}
pub(crate) async fn get(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<DotPath>, PathRejection>,
) -> Result<Json<Dot>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        dot::get(&state.core.pool, &tenant, path.dot_id).await?,
    ))
}
pub(crate) async fn update(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<DotPath>, PathRejection>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Dot>, ApiError> {
    let body = payload(body)?;
    if [
        "instructions",
        "tool_allowlist",
        "model_tier",
        "autonomy",
        "status",
        "max_concurrency",
        "daily_token_budget",
    ]
    .iter()
    .any(|field| body.get(field).is_some())
    {
        tenant.require_admin()?;
    }
    let path = route_path(path)?;
    let patch: DotPatch =
        serde_json::from_value(body).map_err(|_| katwork::error::KatError::RequestInvalid)?;
    Ok(Json(
        dot::update(&state.core.pool, &tenant, path.dot_id, patch).await?,
    ))
}
pub(crate) async fn grant(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<GrantPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let path = route_path(path)?;
    dot::grant_space(&state.core.pool, &tenant, path.dot_id, path.space_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn revoke(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<GrantPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let path = route_path(path)?;
    dot::revoke_space(&state.core.pool, &tenant, path.dot_id, path.space_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
