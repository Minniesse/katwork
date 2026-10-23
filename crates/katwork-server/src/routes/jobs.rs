use crate::{app::AppState, error::ApiError, extract::path};
use axum::{
    Json,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use katwork::{
    ids::JobId,
    jobs::{JobView, job},
    tenancy::Tenant,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct JobPath {
    job_id: JobId,
}

pub(crate) async fn get(
    State(state): State<AppState>,
    tenant: Tenant,
    id: Result<Path<JobPath>, PathRejection>,
) -> Result<Json<JobView>, ApiError> {
    Ok(Json(
        job::view(&state.core.pool, &tenant, path(id)?.job_id).await?,
    ))
}

pub(crate) async fn cancel(
    State(state): State<AppState>,
    tenant: Tenant,
    id: Result<Path<JobPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    job::cancel(
        &state.core.pool,
        &state.core.events,
        &tenant,
        path(id)?.job_id,
        state.core.config.lock_timeout,
    )
    .await?;
    Ok(StatusCode::ACCEPTED)
}
