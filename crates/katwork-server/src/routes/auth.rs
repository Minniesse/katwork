use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Extension, Json,
    extract::{ConnectInfo, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
};
use katwork::{
    auth::{self, AuthUser, reset, session},
    error::KatError,
};
use serde::Deserialize;
use serde_json::json;
use tokio::time::Instant;

use crate::{app::AppState, error::ApiError, extract::session_cookie};

type Attempts = HashMap<(String, IpAddr), (Instant, u8)>;

#[derive(Clone, Default)]
pub(crate) struct LoginLimiter(Arc<Mutex<Attempts>>);

impl LoginLimiter {
    fn check(&self, email: &str, ip: IpAddr) -> Result<(), KatError> {
        let mut attempts = self.0.lock().map_err(|_| KatError::AuthFailed)?;
        let now = Instant::now();
        attempts.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(15 * 60));
        let key = (email.to_owned(), ip);
        // Reject new keys at capacity rather than evicting active limits.
        if attempts.len() >= 10_000 && !attempts.contains_key(&key) {
            return Err(KatError::RateLimited);
        }
        let (_, count) = attempts.entry(key).or_insert((now, 0));
        if *count >= 10 {
            return Err(KatError::RateLimited);
        }
        *count += 1;
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct Login {
    email: String,
    password: String,
}
#[derive(Deserialize)]
pub(crate) struct ResetRequest {
    email: String,
}
#[derive(Deserialize)]
pub(crate) struct ResetConfirm {
    token: String,
    new_password: String,
}

fn payload<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(value)| value).map_err(|error| {
        ApiError(if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            KatError::BodyTooLarge
        } else {
            KatError::RequestInvalid
        })
    })
}

fn email(value: &str) -> Result<String, ApiError> {
    let value = value.trim().to_lowercase();
    if value.len() > 254 {
        return Err(KatError::RequestInvalid.into());
    }
    Ok(value)
}

fn cookie(value: &str, secure: bool, max_age: u32) -> String {
    format!(
        "kw_session={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

pub(crate) async fn login(
    State(state): State<AppState>,
    Extension(limiter): Extension<LoginLimiter>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Result<Json<Login>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = payload(body)?;
    let email = email(&body.email)?;
    let peer = peer.ok_or(KatError::RequestInvalid)?.0.0.ip();
    limiter.check(&email, peer)?;
    let (user, token) = auth::login(&state.core.pool, &email, &body.password).await?;
    Ok((
        [(
            SET_COOKIE,
            cookie(
                &token.0,
                state.core.config.cookie_secure(),
                14 * 24 * 60 * 60,
            ),
        )],
        Json(user),
    )
        .into_response())
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = session_cookie(&headers) {
        session::revoke(&state.core.pool, token).await?;
    }
    Ok((
        [(SET_COOKIE, cookie("", state.core.config.cookie_secure(), 0))],
        Json(json!({})),
    )
        .into_response())
}

pub(crate) async fn me(user: AuthUser) -> Json<AuthUser> {
    Json(user)
}

pub(crate) async fn request_reset(
    State(state): State<AppState>,
    body: Result<Json<ResetRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = payload(body)?;
    reset::request(
        &state.core.pool,
        state.core.mailer.as_ref(),
        &email(&body.email)?,
        state.core.config.public_url_or_default(),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))).into_response())
}

pub(crate) async fn confirm_reset(
    State(state): State<AppState>,
    body: Result<Json<ResetConfirm>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = payload(body)?;
    reset::confirm(&state.core.pool, &body.token, &body.new_password).await?;
    Ok(Json(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn login_limit_expires_at_fifteen_minutes() {
        let limiter = LoginLimiter::default();
        let ip = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            limiter.check("person@example.test", ip).unwrap();
        }
        assert_eq!(
            limiter.check("person@example.test", ip).unwrap_err().code(),
            "rate_limited"
        );
        tokio::time::advance(Duration::from_secs(899)).await;
        assert!(limiter.check("person@example.test", ip).is_err());
        tokio::time::advance(Duration::from_secs(1)).await;
        limiter.check("person@example.test", ip).unwrap();
    }
}
