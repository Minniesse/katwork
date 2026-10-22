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

#[derive(Hash, PartialEq, Eq, Clone)]
enum LimitKey {
    Email(String),
    Ip(IpAddr),
    Pair(String, IpAddr),
}

#[derive(Default)]
struct Attempts {
    entries: HashMap<LimitKey, (Instant, u16, u64)>,
    sequence: u64,
}

impl Attempts {
    fn check(&mut self, key: LimitKey, limit: u16, window: Duration) -> Result<(), KatError> {
        if self.entries.len() == 10_000 && !self.entries.contains_key(&key) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.2)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.entries.remove(&oldest);
        }
        let now = Instant::now();
        self.sequence += 1;
        let entry = self.entries.entry(key).or_insert((now, 0, self.sequence));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0, self.sequence);
        }
        entry.2 = self.sequence;
        if entry.1 >= limit {
            return Err(KatError::RateLimited);
        }
        entry.1 += 1;
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct LoginLimiter(Arc<Mutex<Attempts>>);

impl LoginLimiter {
    fn check(&self, email: &str, ip: IpAddr) -> Result<(), KatError> {
        let mut attempts = self.0.lock().map_err(|_| KatError::AuthFailed)?;
        let quarter_hour = Duration::from_secs(15 * 60);
        attempts.check(LimitKey::Ip(ip), 100, quarter_hour)?;
        attempts.check(LimitKey::Pair(email.to_owned(), ip), 10, quarter_hour)?;
        attempts.check(
            LimitKey::Email(email.to_owned()),
            100,
            Duration::from_secs(3600),
        )
    }
}

#[derive(Clone, Default)]
pub(crate) struct ResetLimiter(Arc<Mutex<Attempts>>);

impl ResetLimiter {
    fn check(&self, email: &str, ip: IpAddr) -> Result<(), KatError> {
        let mut attempts = self.0.lock().map_err(|_| KatError::AuthFailed)?;
        let hour = Duration::from_secs(3600);
        attempts.check(LimitKey::Ip(ip), 20, hour)?;
        attempts.check(LimitKey::Email(email.to_owned()), 3, hour)
    }
}

#[derive(Clone)]
pub(crate) struct ResetWork(pub Arc<tokio::sync::Semaphore>);

impl Default for ResetWork {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(32)))
    }
}

fn client_ip(peer: IpAddr, headers: &HeaderMap, config: &katwork::config::Config) -> IpAddr {
    if !config.trusts_proxy(peer) {
        return peer;
    }
    let mut values = headers.get_all("x-forwarded-for").iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return peer;
    };
    if values.next().is_some() {
        return peer;
    }
    let mut client = peer;
    // Walk from the trusted peer; ignore any attacker-supplied prefix beyond the first untrusted hop.
    for hop in value.rsplit(',') {
        if !config.trusts_proxy(client) {
            break;
        }
        let Ok(ip) = hop.trim().parse() else {
            return peer;
        };
        client = ip;
    }
    client
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
    headers: HeaderMap,
    body: Result<Json<Login>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = payload(body)?;
    let email = email(&body.email)?;
    let peer = peer.ok_or(KatError::RequestInvalid)?.0.0.ip();
    limiter.check(&email, client_ip(peer, &headers, &state.core.config))?;
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
    Extension(limiter): Extension<ResetLimiter>,
    Extension(work): Extension<ResetWork>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Result<Json<ResetRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    use tracing::instrument::WithSubscriber;

    let body = payload(body)?;
    let email = email(&body.email)?;
    let peer = peer.ok_or(KatError::RequestInvalid)?.0.0.ip();
    let ip = client_ip(peer, &headers, &state.core.config);
    if limiter.check(&email, ip).is_ok() {
        if let Ok(permit) = work.0.try_acquire_owned() {
            tokio::spawn(
                async move {
                    let _permit = permit;
                    if let Err(error) = reset::request(
                        &state.core.pool,
                        state.core.mailer.as_ref(),
                        &email,
                        state.core.config.public_url_or_default(),
                    )
                    .await
                    {
                        tracing::warn!(code = error.code(), "reset_request_failed");
                    }
                }
                .with_current_subscriber(),
            );
        } else {
            tracing::warn!("reset_queue_full: dropping work");
        }
    }
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

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn spraying_one_ip_does_not_lock_out_other_clients() {
        let limiter = LoginLimiter::default();
        let attacker = "192.0.2.1".parse().unwrap();
        for i in 0..10_100 {
            let result = limiter.check(&format!("random-{i}@example.test"), attacker);
            assert_eq!(result.is_ok(), i < 100);
        }
        limiter
            .check("owner@example.test", "192.0.2.2".parse().unwrap())
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn full_login_limiter_evicts_lru_and_recovers() {
        let limiter = LoginLimiter::default();
        for i in 0..10_001u32 {
            limiter
                .check(&format!("random-{i}@example.test"), IpAddr::V4(i.into()))
                .unwrap();
        }
        limiter
            .check("owner@example.test", "192.0.2.1".parse().unwrap())
            .unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        limiter
            .check("random-0@example.test", IpAddr::V4(0.into()))
            .unwrap();
    }

    #[test]
    fn distributed_login_guessing_has_an_email_backstop() {
        let limiter = LoginLimiter::default();
        for i in 0..100u32 {
            limiter
                .check("owner@example.test", IpAddr::V4(i.into()))
                .unwrap();
        }
        assert!(matches!(
            limiter.check("owner@example.test", "192.0.2.1".parse().unwrap()),
            Err(KatError::RateLimited)
        ));
    }
}

#[cfg(test)]
mod reset_limit_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn reset_limits_email_and_ip_identically_for_unknown_accounts() {
        for email in ["person@example.test", "unknown@example.test"] {
            let limiter = ResetLimiter::default();
            let ip = "192.0.2.1".parse().unwrap();
            for _ in 0..3 {
                limiter.check(email, ip).unwrap();
            }
            assert!(limiter.check(email, "192.0.2.2".parse().unwrap()).is_err());
            for i in 0..17 {
                limiter.check(&format!("{i}@example.test"), ip).unwrap();
            }
            assert!(limiter.check("new@example.test", ip).is_err());
            tokio::time::advance(Duration::from_secs(3600)).await;
            limiter.check(email, ip).unwrap();
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn limiter_capacity_evicts_least_recently_used_entry() {
        let mut attempts = Attempts::default();
        let hour = Duration::from_secs(3600);
        for i in 0..10_000 {
            attempts
                .check(LimitKey::Email(i.to_string()), 10, hour)
                .unwrap();
        }
        attempts
            .check(LimitKey::Email("0".into()), 10, hour)
            .unwrap();
        attempts
            .check(LimitKey::Email("new".into()), 10, hour)
            .unwrap();
        assert_eq!(attempts.entries.len(), 10_000);
        assert!(attempts.entries.contains_key(&LimitKey::Email("0".into())));
        assert!(!attempts.entries.contains_key(&LimitKey::Email("1".into())));
    }

    #[sqlx::test(migrations = "../katwork/migrations")]
    async fn reset_background_work_is_bounded_and_releases_capacity(pool: sqlx::PgPool) {
        let single = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .unwrap();
        let held = single.acquire().await.unwrap();
        let config = katwork::config::Config::from_lookup(|name| match name {
            "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
            "KATWORK_SECRET" => Some("test-only-secret-at-least-32-bytes".into()),
            _ => None,
        })
        .unwrap();
        let state = AppState {
            core: Arc::new(katwork::runtime::Core::new(
                single.clone(),
                Arc::new(config),
            )),
        };
        let limiter = ResetLimiter::default();
        let work = ResetWork::default();
        for i in 0..64u32 {
            let peer = SocketAddr::new(IpAddr::V4(i.into()), 7000);
            let response = request_reset(
                State(state.clone()),
                Extension(limiter.clone()),
                Extension(work.clone()),
                Some(Extension(ConnectInfo(peer))),
                HeaderMap::new(),
                Ok(Json(ResetRequest {
                    email: format!("{i}@example.test"),
                })),
            )
            .await
            .map_err(|error| error.0)
            .unwrap();
            assert_eq!(response.status(), 202);
        }
        assert_eq!(work.0.available_permits(), 0);
        drop(held);
        let _finished = tokio::time::timeout(
            Duration::from_secs(2),
            work.0.clone().acquire_many_owned(32),
        )
        .await
        .unwrap()
        .unwrap();
        single.close().await;
    }

    #[test]
    fn forwarded_chain_stops_at_first_untrusted_hop() {
        let config = katwork::config::Config::from_lookup(|name| match name {
            "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
            "KATWORK_SECRET" => Some("test-only-secret-at-least-32-bytes".into()),
            "KATWORK_TRUSTED_PROXIES" => Some("10.0.0.0/8, ::1/128".into()),
            _ => None,
        })
        .unwrap();
        let peer = "10.0.0.1".parse().unwrap();
        for (value, expected) in [
            ("198.51.100.1, 192.0.2.1, 10.0.0.2", "192.0.2.1"),
            ("spoofed, 192.0.2.1", "192.0.2.1"),
            ("192.0.2.1, bad", "10.0.0.1"),
            ("2001:db8::1, ::1", "2001:db8::1"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", value.parse().unwrap());
            assert_eq!(
                client_ip(peer, &headers, &config),
                expected.parse::<IpAddr>().unwrap()
            );
            headers.append("x-forwarded-for", "192.0.2.2".parse().unwrap());
            assert_eq!(client_ip(peer, &headers, &config), peer);
        }
    }
}
