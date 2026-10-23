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
    ids::{KatId, SpaceId},
    team::{HireKat, Kat, KatPatch, kat, roles},
    tenancy::Tenant,
    types::Locale,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct SpacePath {
    space_id: SpaceId,
}
#[derive(Deserialize)]
pub(crate) struct KatPath {
    kat_id: KatId,
}
#[derive(Deserialize)]
pub(crate) struct GrantPath {
    kat_id: KatId,
    space_id: SpaceId,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
) -> Result<Json<Vec<Kat>>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        kat::list(&state.core.pool, &tenant, path.space_id).await?,
    ))
}
pub(crate) async fn hire(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    body: Result<Json<HireKat>, JsonRejection>,
) -> Result<(StatusCode, Json<Kat>), ApiError> {
    let path = route_path(path)?;
    let body = payload(body)?;
    let mut new = roles::template(&body.role, Locale::En);
    new.name = body.name;
    new.persona = body.persona;
    let id = kat::hire(
        &state.core.pool,
        &tenant,
        path.space_id,
        new,
        state.core.config.lock_timeout,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(kat::get(&state.core.pool, &tenant, id).await?),
    ))
}
pub(crate) async fn get(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<KatPath>, PathRejection>,
) -> Result<Json<Kat>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        kat::get(&state.core.pool, &tenant, path.kat_id).await?,
    ))
}
pub(crate) async fn update(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<KatPath>, PathRejection>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Kat>, ApiError> {
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
    let patch: KatPatch =
        serde_json::from_value(body).map_err(|_| katwork::error::KatError::RequestInvalid)?;
    Ok(Json(
        kat::update(
            &state.core.pool,
            &tenant,
            path.kat_id,
            patch,
            state.core.config.lock_timeout,
        )
        .await?,
    ))
}
pub(crate) async fn grant(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<GrantPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let path = route_path(path)?;
    kat::grant_space(
        &state.core.pool,
        &tenant,
        path.kat_id,
        path.space_id,
        state.core.config.lock_timeout,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn revoke(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<GrantPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let path = route_path(path)?;
    kat::revoke_space(
        &state.core.pool,
        &tenant,
        path.kat_id,
        path.space_id,
        state.core.config.lock_timeout,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
