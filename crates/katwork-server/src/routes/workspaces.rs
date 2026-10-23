use axum::{
    Json,
    body::Bytes,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
};
use katwork::{
    auth::AuthUser,
    error::KatError,
    tenancy::{
        Tenant,
        space::{self, Space},
        workspace::{self, WorkspaceSummary},
    },
    types::SpacePolicy,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{app::AppState, error::ApiError, extract::payload};

#[derive(Deserialize)]
pub(crate) struct NewWorkspace {
    name: String,
}

#[derive(Deserialize)]
pub(crate) struct NewSpace {
    name: String,
    allow_external: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct Paused {
    paused: bool,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<WorkspaceSummary>>, ApiError> {
    Ok(Json(
        workspace::list_for(&state.core.pool, user.user_id).await?,
    ))
}

pub(crate) async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    body: Result<Json<NewWorkspace>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = payload(body)?;
    let id = workspace::create(&state.core.pool, &body.name, user.user_id).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub(crate) async fn list_spaces(
    State(state): State<AppState>,
    tenant: Tenant,
) -> Result<Json<Vec<Space>>, ApiError> {
    Ok(Json(space::list(&state.core.pool, &tenant).await?))
}

pub(crate) async fn create_space(
    State(state): State<AppState>,
    tenant: Tenant,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    tenant.require_admin()?;
    let body = payload(Json::<NewSpace>::from_bytes(&body))?;
    let policy = SpacePolicy {
        allow_external: body.allow_external.ok_or(KatError::SpacePolicyRequired)?,
    };
    let id = space::create(&state.core.pool, &tenant, &body.name, policy).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub(crate) async fn set_paused(
    State(state): State<AppState>,
    tenant: Tenant,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    tenant.require_admin()?;
    let body = payload(Json::<Paused>::from_bytes(&body))?;
    workspace::set_paused(
        &state.core.pool,
        &tenant,
        body.paused,
        state.core.config.lock_timeout,
    )
    .await?;
    Ok(Json(json!({})))
}
