use axum::{
    Json,
    extract::{
        FromRequestParts, Path,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderMap, StatusCode, request::Parts},
};
use katwork::{
    auth::{AuthUser, session},
    error::KatError,
    ids::WorkspaceId,
    tenancy::{self, Tenant},
};

use crate::{app::AppState, error::ApiError};

pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    let mut token = None;
    for value in headers.get_all("cookie") {
        for part in value.to_str().ok()?.split(';') {
            if let Some(("kw_session", value)) = part.trim().split_once('=') {
                if token.is_some() {
                    return None;
                }
                token = Some(value);
            }
        }
    }
    token
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = session_cookie(&parts.headers).ok_or(KatError::Unauthenticated)?;
        Ok(session::authenticate(&state.core.pool, token).await?)
    }
}

#[derive(serde::Deserialize)]
struct WorkspacePath {
    workspace_id: WorkspaceId,
}

impl FromRequestParts<AppState> for Tenant {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        let Path(path) = Path::<WorkspacePath>::from_request_parts(parts, state)
            .await
            .map_err(|_| KatError::RequestInvalid)?;
        Ok(tenancy::resolve(&state.core.pool, user.user_id, path.workspace_id).await?)
    }
}

pub(crate) fn payload<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(value)| value).map_err(|error| {
        ApiError(if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            KatError::BodyTooLarge
        } else {
            KatError::RequestInvalid
        })
    })
}

pub(crate) fn path<T>(path: Result<Path<T>, PathRejection>) -> Result<T, ApiError> {
    path.map(|Path(value)| value)
        .map_err(|_| ApiError(KatError::RequestInvalid))
}
