use axum::{
    Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use katwork::{
    ids::UserId,
    tenancy::{
        Role, Tenant,
        members::{self, Invite, InviteOptions, InviteResult, Member},
    },
};
use serde::Deserialize;

use crate::{
    app::AppState,
    error::ApiError,
    extract::{path as route_path, payload},
};

// Every route checks the role before it reads a path id or the body (Global Constraints,
// "Roles before bodies"), so a member gets 403 whatever the request holds.

#[derive(Deserialize)]
pub(crate) struct MemberPath {
    user_id: UserId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewRole {
    role: Role,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    tenant: Tenant,
) -> Result<Json<Vec<Member>>, ApiError> {
    tenant.require_admin()?;
    Ok(Json(members::list(&state.core.pool, &tenant).await?))
}

pub(crate) async fn invite_options(
    State(state): State<AppState>,
    tenant: Tenant,
) -> Result<Json<InviteOptions>, ApiError> {
    tenant.require_admin()?;
    Ok(Json(members::invite_options(&state.core.config, &tenant)?))
}

pub(crate) async fn invite(
    State(state): State<AppState>,
    tenant: Tenant,
    body: Bytes,
) -> Result<(StatusCode, Json<InviteResult>), ApiError> {
    tenant.require_admin()?;
    let body = payload(Json::<Invite>::from_bytes(&body))?;
    let result = members::invite(
        &state.core.pool,
        &state.core.config,
        state.core.mailer.clone(),
        &tenant,
        body,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn set_role(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<MemberPath>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let user_id = route_path(path)?.user_id;
    let body = payload(Json::<NewRole>::from_bytes(&body))?;
    members::set_role(&state.core.pool, &tenant, user_id, body.role).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn remove(
    State(state): State<AppState>,
    tenant: Tenant,
    path: Result<Path<MemberPath>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    tenant.require_admin()?;
    let user_id = route_path(path)?.user_id;
    members::remove(&state.core.pool, &tenant, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
