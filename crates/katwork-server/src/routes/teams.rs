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
    ids::{DotId, SpaceId, TeamId},
    team::{NewTeam, Team, TeamView, team},
    tenancy::Tenant,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct SpacePath {
    space_id: SpaceId,
}
#[derive(Deserialize)]
pub(crate) struct TeamPath {
    team_id: TeamId,
}
#[derive(Deserialize)]
pub(crate) struct MemberPath {
    team_id: TeamId,
    dot_id: DotId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Membership {
    is_lead: bool,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
) -> Result<Json<Vec<Team>>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        team::list(&state.core.pool, &tenant, path.space_id).await?,
    ))
}
pub(crate) async fn create(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    body: Result<Json<NewTeam>, JsonRejection>,
) -> Result<(StatusCode, Json<TeamView>), ApiError> {
    let path = route_path(path)?;
    let id = team::create(&state.core.pool, &tenant, path.space_id, payload(body)?).await?;
    Ok((
        StatusCode::CREATED,
        Json(team::get(&state.core.pool, &tenant, id).await?),
    ))
}
pub(crate) async fn get(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<TeamPath>, PathRejection>,
) -> Result<Json<TeamView>, ApiError> {
    let path = route_path(path)?;
    Ok(Json(
        team::get(&state.core.pool, &tenant, path.team_id).await?,
    ))
}
pub(crate) async fn add_member(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<MemberPath>, PathRejection>,
    body: Result<Json<Membership>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let path = route_path(path)?;
    team::add_member(
        &state.core.pool,
        &tenant,
        path.team_id,
        path.dot_id,
        payload(body)?.is_lead,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn remove_member(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<MemberPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let path = route_path(path)?;
    team::remove_member(&state.core.pool, &tenant, path.team_id, path.dot_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
