use axum::{
    Json,
    extract::{
        Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::StatusCode,
};
use katwork::{
    chat::{MessagePage, Thread, UserPost, message, thread},
    error::KatError,
    ids::{JobId, MessageId, SpaceId, TeamId, ThreadId},
    pipeline,
    tenancy::Tenant,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppState,
    error::ApiError,
    extract::{ApiJson, path as route_path},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateThread {
    space_id: SpaceId,
    team_id: TeamId,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
pub(crate) struct ThreadPath {
    thread_id: ThreadId,
}
#[derive(Deserialize)]
pub(crate) struct SpacePath {
    space_id: SpaceId,
}

#[derive(Deserialize)]
pub(crate) struct PageQuery {
    before: Option<MessageId>,
    #[serde(default = "default_limit")]
    limit: u16,
}
fn default_limit() -> u16 {
    50
}

fn query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(value)| value)
        .map_err(|_| KatError::RequestInvalid.into())
}

pub(crate) async fn create(
    State(state): State<AppState>,
    tenant: Tenant,
    ApiJson(body): ApiJson<CreateThread>,
) -> Result<(StatusCode, Json<Thread>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(
            thread::create(
                &state.core.pool,
                &tenant,
                body.space_id,
                body.team_id,
                &body.title,
            )
            .await?,
        ),
    ))
}

pub(crate) async fn get(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<ThreadPath>, PathRejection>,
) -> Result<Json<Thread>, ApiError> {
    Ok(Json(
        thread::get(&state.core.pool, &tenant, route_path(path)?.thread_id).await?,
    ))
}

pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<SpacePath>, PathRejection>,
    params: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<Vec<Thread>>, ApiError> {
    Ok(Json(
        thread::list(
            &state.core.pool,
            &tenant,
            route_path(path)?.space_id,
            query(params)?.limit,
        )
        .await?,
    ))
}

pub(crate) async fn messages(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<ThreadPath>, PathRejection>,
    params: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<MessagePage>, ApiError> {
    let params = query(params)?;
    Ok(Json(
        message::list(
            &state.core.pool,
            &tenant,
            route_path(path)?.thread_id,
            params.before,
            params.limit,
        )
        .await?,
    ))
}

#[derive(Serialize)]
pub(crate) struct Posted {
    message_id: MessageId,
    job_id: JobId,
}

pub(crate) async fn post_message(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<ThreadPath>, PathRejection>,
    ApiJson(post): ApiJson<UserPost>,
) -> Result<(StatusCode, Json<Posted>), ApiError> {
    let thread = route_path(path)?.thread_id;
    let appended = pipeline::submit(&state.core, &tenant, thread, post).await?;
    Ok((
        if appended.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(Posted {
            message_id: appended.message_id,
            job_id: appended.job_id,
        }),
    ))
}
