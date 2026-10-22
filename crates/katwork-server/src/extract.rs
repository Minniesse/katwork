use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use katwork::{
    auth::{AuthUser, session},
    error::KatError,
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
