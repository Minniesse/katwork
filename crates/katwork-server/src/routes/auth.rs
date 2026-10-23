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
    auth::{self, AuthUser, invite, reset, session},
    error::KatError,
};
use serde::Deserialize;
use serde_json::json;
use tokio::time::Instant;

use crate::{
    app::AppState,
    error::ApiError,
    extract::{payload, session_cookie},
    peer::{client_ip, ip_bucket},
};

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
enum LimitKey {
    Email(String),
    Ip(IpAddr),
    Pair(String, IpAddr),
}

#[derive(Default)]
struct Attempts {
    entries: HashMap<LimitKey, Bucket>,
    sequence: u64,
}

/// A fixed window: when it started, how many attempts it holds, and when it was last used.
#[derive(Clone, Copy)]
struct Bucket {
    started: Instant,
    count: u16,
    used: u64,
}

impl Attempts {
    /// How long until `key`'s window ends, when it is full; `None` when it has room.
    fn full(&self, key: &LimitKey, limit: u16, window: Duration, now: Instant) -> Option<Duration> {
        let bucket = self.entries.get(key)?;
        let ends = bucket.started + window;
        (now < ends && bucket.count >= limit).then(|| ends - now)
    }

    /// Counts one attempt in `key`'s window and returns the window's start.
    fn charge(&mut self, key: LimitKey, window: Duration, now: Instant) -> Instant {
        if self.entries.len() == 10_000 && !self.entries.contains_key(&key) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, bucket)| bucket.used)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.entries.remove(&oldest);
        }
        self.sequence += 1;
        let bucket = self.entries.entry(key).or_insert(Bucket {
            started: now,
            count: 0,
            used: self.sequence,
        });
        if now.duration_since(bucket.started) >= window {
            *bucket = Bucket {
                started: now,
                count: 0,
                used: self.sequence,
            };
        }
        bucket.used = self.sequence;
        bucket.count += 1;
        bucket.started
    }

    /// Takes back one attempt, unless the window it was counted in has since ended.
    fn refund(&mut self, key: &LimitKey, started: Instant) {
        if let Some(bucket) = self.entries.get_mut(key)
            && bucket.started == started
        {
            bucket.count = bucket.count.saturating_sub(1);
        }
    }

    /// Admits an attempt into every bucket, or none: a full bucket refuses it with the time
    /// until the last full window ends, rounded up and at least one second.
    fn reserve(&mut self, buckets: &[(LimitKey, u16, Duration)]) -> Result<Vec<Instant>, KatError> {
        let now = Instant::now();
        let wait = buckets
            .iter()
            .filter_map(|(key, limit, window)| self.full(key, *limit, *window, now))
            .max();
        if let Some(wait) = wait {
            let retry_after_secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            return Err(KatError::RateLimited {
                retry_after_secs: retry_after_secs.max(1),
            });
        }
        Ok(buckets
            .iter()
            .map(|(key, _, window)| self.charge(key.clone(), *window, now))
            .collect())
    }
}

const QUARTER_HOUR: Duration = Duration::from_secs(15 * 60);
const HOUR: Duration = Duration::from_secs(3600);

// Process-local fixed windows; restart or LRU eviction resets a bucket, replicas do not share it.
// IP: 100/15 min, email+IP: 10/15 min, then email: 100/hour, counting only failed attempts.
// An attempt is admitted into all three buckets or none, so a rejected pair consumes nothing.
// Distributed failed attempts can still exhaust the email backstop.
// Shared IPs share the IP quota; the same email+IP cannot distinguish an owner from an attacker.
#[derive(Clone, Default)]
pub(crate) struct LoginLimiter(Arc<Mutex<Attempts>>);

/// One admitted login attempt. Dropped, it stays counted; [`LoginAttempt::finish`] settles it.
pub(crate) struct LoginAttempt {
    limiter: LoginLimiter,
    keys: [(LimitKey, Instant); 3],
}

impl LoginLimiter {
    pub(crate) fn reserve(&self, email: &str, ip: IpAddr) -> Result<LoginAttempt, KatError> {
        let ip = ip_bucket(ip);
        let buckets = [
            (LimitKey::Ip(ip), 100, QUARTER_HOUR),
            (LimitKey::Pair(email.to_owned(), ip), 10, QUARTER_HOUR),
            (LimitKey::Email(email.to_owned()), 100, HOUR),
        ];
        let started = self
            .0
            .lock()
            .map_err(|_| KatError::AuthFailed)?
            .reserve(&buckets)?;
        let mut started = started.into_iter();
        Ok(LoginAttempt {
            limiter: self.clone(),
            keys: buckets.map(|(key, _, _)| (key, started.next().unwrap())),
        })
    }

    /// Removes every bucket of `email`: each email+IP bucket and the backstop, never an IP bucket.
    pub(crate) fn clear_email(&self, email: &str) {
        if let Ok(mut attempts) = self.0.lock() {
            attempts.entries.retain(|key, _| match key {
                LimitKey::Email(other) | LimitKey::Pair(other, _) => other != email,
                LimitKey::Ip(_) => true,
            });
        }
    }
}

impl LoginAttempt {
    /// Settles the attempt by the login's outcome. Only a password that was checked and wrong
    /// (`unauthenticated`) stays counted; a success is [`Self::succeeded`], and an error that
    /// came before any check (`auth_busy`, a database error) is handed back without clearing
    /// earlier failures, so making the server busy never wipes a lockout.
    pub(crate) fn finish<T>(self, result: &Result<T, KatError>) {
        match result {
            Ok(_) => self.succeeded(),
            Err(KatError::Unauthenticated) => {}
            Err(_) => self.unchecked(),
        }
    }

    fn unchecked(self) {
        let Ok(mut attempts) = self.limiter.0.lock() else {
            return;
        };
        for (key, started) in &self.keys {
            attempts.refund(key, *started);
        }
    }

    /// The password verified: give the attempt back to the IP and email buckets, and clear
    /// the email+IP bucket.
    pub(crate) fn succeeded(self) {
        let Ok(mut attempts) = self.limiter.0.lock() else {
            return;
        };
        let [(ip, ip_started), (pair, _), (email, email_started)] = self.keys;
        attempts.refund(&ip, ip_started);
        attempts.refund(&email, email_started);
        attempts.entries.remove(&pair);
    }
}

#[derive(Clone, Default)]
pub(crate) struct ResetLimiter(Arc<Mutex<Attempts>>);

impl ResetLimiter {
    fn check(&self, email: &str, ip: IpAddr) -> Result<(), KatError> {
        let ip = ip_bucket(ip);
        let mut attempts = self.0.lock().map_err(|_| KatError::AuthFailed)?;
        // Each bucket counts the requests it admits, so an email over its limit still uses the IP's.
        attempts.reserve(&[(LimitKey::Ip(ip), 20, HOUR)])?;
        attempts.reserve(&[(LimitKey::Email(email.to_owned()), 3, HOUR)])?;
        Ok(())
    }
}

/// How many reset lookups may run in the background at once.
pub const RESET_WORK_SLOTS: u32 = 32;

/// The background reset lookups. A request past the slots is dropped, still answering 202.
#[derive(Clone)]
pub struct ResetWork(pub(crate) Arc<tokio::sync::Semaphore>);

impl Default for ResetWork {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(
            RESET_WORK_SLOTS as usize,
        )))
    }
}

impl ResetWork {
    /// Returns once no spawned lookup holds a slot.
    pub async fn drained(&self) {
        let _all = self.0.acquire_many(RESET_WORK_SLOTS).await;
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
/// The body of reset confirm and invite accept.
#[derive(Deserialize)]
pub(crate) struct SetPassword {
    token: String,
    new_password: String,
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
    let attempt = limiter.reserve(&email, client_ip(peer, &headers, &state.core.config))?;
    let login = auth::login(&state.core.pool, &email, &body.password).await;
    attempt.finish(&login);
    let (user, token) = login?;
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
    Extension(limiter): Extension<LoginLimiter>,
    body: Result<Json<SetPassword>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = payload(body)?;
    let email = reset::confirm(&state.core.pool, &body.token, &body.new_password).await?;
    // The new password ends any lockout the old one earned.
    limiter.clear_email(&email);
    Ok(Json(json!({})))
}

pub(crate) async fn accept_invite(
    State(state): State<AppState>,
    Extension(limiter): Extension<LoginLimiter>,
    body: Result<Json<SetPassword>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let body = payload(body)?;
    let email = invite::accept(&state.core.pool, &body.token, &body.new_password).await?;
    limiter.clear_email(&email);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn login_limit_expires_at_fifteen_minutes() {
        let limiter = LoginLimiter::default();
        let ip = "127.0.0.1".parse().unwrap();
        for _ in 0..10 {
            limiter.reserve("person@example.test", ip).unwrap();
        }
        assert_eq!(
            limiter
                .reserve("person@example.test", ip)
                .err()
                .unwrap()
                .code(),
            "rate_limited"
        );
        tokio::time::advance(Duration::from_secs(899)).await;
        assert!(limiter.reserve("person@example.test", ip).is_err());
        tokio::time::advance(Duration::from_secs(1)).await;
        limiter.reserve("person@example.test", ip).unwrap();
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
            let result = limiter.reserve(&format!("random-{i}@example.test"), attacker);
            assert_eq!(result.is_ok(), i < 100);
        }
        limiter
            .reserve("owner@example.test", "192.0.2.2".parse().unwrap())
            .unwrap();
    }

    #[test]
    fn ipv6_rotation_shares_login_and_reset_ip_buckets() {
        let login = LoginLimiter::default();
        for i in 1..=100 {
            let ip = format!("2001:db8:1:2::{i:x}").parse().unwrap();
            login
                .reserve(&format!("person-{i}@example.test"), ip)
                .unwrap();
        }
        assert!(
            login
                .reserve("next@example.test", "2001:db8:1:2::ffff".parse().unwrap())
                .is_err()
        );
        login
            .reserve("next@example.test", "2001:db8:1:3::1".parse().unwrap())
            .unwrap();
        let pair = LoginLimiter::default();
        for i in 1..=10 {
            pair.reserve(
                "owner@example.test",
                format!("2001:db8::{i:x}").parse().unwrap(),
            )
            .unwrap();
        }
        assert!(
            pair.reserve("owner@example.test", "2001:db8::ffff".parse().unwrap())
                .is_err()
        );
        let reset = ResetLimiter::default();
        for i in 1..=20 {
            reset
                .check(
                    &format!("person-{i}@example.test"),
                    format!("2001:db8::{i:x}").parse().unwrap(),
                )
                .unwrap();
        }
        assert!(
            reset
                .check("next@example.test", "2001:db8::ffff".parse().unwrap())
                .is_err()
        );
        reset
            .check("next@example.test", "2001:db8:0:1::1".parse().unwrap())
            .unwrap();
    }

    #[test]
    fn rejected_pairs_do_not_consume_the_email_backstop() {
        let limiter = LoginLimiter::default();
        let attacker = "192.0.2.1".parse().unwrap();
        for _ in 0..10 {
            limiter.reserve("owner@example.test", attacker).unwrap();
        }
        for _ in 0..1000 {
            assert!(limiter.reserve("owner@example.test", attacker).is_err());
        }
        for i in 2..=10 {
            let ip = format!("192.0.2.{i}").parse().unwrap();
            for _ in 0..10 {
                limiter.reserve("owner@example.test", ip).unwrap();
            }
        }
        assert!(
            limiter
                .reserve("owner@example.test", "192.0.2.11".parse().unwrap())
                .is_err()
        );
    }

    #[test]
    fn shared_ip_different_emails_have_separate_pair_quotas() {
        let limiter = LoginLimiter::default();
        let shared = "192.0.2.1".parse().unwrap();
        for _ in 0..10 {
            limiter.reserve("attacker@example.test", shared).unwrap();
        }
        assert!(limiter.reserve("attacker@example.test", shared).is_err());
        for _ in 0..10 {
            limiter.reserve("owner@example.test", shared).unwrap();
        }
        assert!(limiter.reserve("owner@example.test", shared).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn full_login_limiter_evicts_lru_and_recovers() {
        let limiter = LoginLimiter::default();
        for i in 0..10_001u32 {
            limiter
                .reserve(&format!("random-{i}@example.test"), IpAddr::V4(i.into()))
                .unwrap();
        }
        limiter
            .reserve("owner@example.test", "192.0.2.1".parse().unwrap())
            .unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        limiter
            .reserve("random-0@example.test", IpAddr::V4(0.into()))
            .unwrap();
    }

    #[test]
    fn distributed_login_guessing_has_an_email_backstop() {
        let limiter = LoginLimiter::default();
        for i in 0..100u32 {
            limiter
                .reserve("owner@example.test", IpAddr::V4(i.into()))
                .unwrap();
        }
        assert!(matches!(
            limiter.reserve("owner@example.test", "192.0.2.1".parse().unwrap()),
            Err(KatError::RateLimited { .. })
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
                .reserve(&[(LimitKey::Email(i.to_string()), 10, hour)])
                .unwrap();
        }
        attempts
            .reserve(&[(LimitKey::Email("0".into()), 10, hour)])
            .unwrap();
        attempts
            .reserve(&[(LimitKey::Email("new".into()), 10, hour)])
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
        tokio::time::timeout(Duration::from_secs(2), work.drained())
            .await
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

#[cfg(test)]
mod lockout_tests {
    use super::*;

    fn retry_after(result: Result<LoginAttempt, KatError>) -> u64 {
        match result {
            Err(KatError::RateLimited { retry_after_secs }) => retry_after_secs,
            Err(other) => panic!("expected rate_limited, got {}", other.code()),
            Ok(_) => panic!("expected rate_limited, got an attempt"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_the_longest_wait_of_every_full_bucket() {
        let limiter = LoginLimiter::default();
        let ip = "192.0.2.1".parse().unwrap();
        for i in 0..90 {
            limiter.reserve(&format!("{i}@example.test"), ip).unwrap();
        }
        tokio::time::advance(Duration::from_secs(300)).await;
        for _ in 0..10 {
            limiter.reserve("owner@example.test", ip).unwrap();
        }
        // The IP window (from 0 s) ends at 900 s, the owner's pair window (from 300 s) at 1200 s.
        assert_eq!(retry_after(limiter.reserve("owner@example.test", ip)), 900);
        assert_eq!(retry_after(limiter.reserve("other@example.test", ip)), 600);
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(retry_after(limiter.reserve("owner@example.test", ip)), 900);
        assert_eq!(retry_after(limiter.reserve("other@example.test", ip)), 600);
        tokio::time::advance(Duration::from_millis(599_500)).await;
        assert_eq!(retry_after(limiter.reserve("owner@example.test", ip)), 300);
        limiter.reserve("other@example.test", ip).unwrap();
        tokio::time::advance(Duration::from_millis(299_999)).await;
        assert_eq!(retry_after(limiter.reserve("owner@example.test", ip)), 1);
        tokio::time::advance(Duration::from_millis(1)).await;
        limiter.reserve("owner@example.test", ip).unwrap();
    }

    #[test]
    fn a_refused_attempt_counts_in_no_bucket() {
        let limiter = LoginLimiter::default();
        let ip = "192.0.2.1".parse().unwrap();
        for _ in 0..10 {
            limiter.reserve("owner@example.test", ip).unwrap();
        }
        for _ in 0..1000 {
            assert!(limiter.reserve("owner@example.test", ip).is_err());
        }
        // The refused attempts used none of the IP's 100.
        for i in 0..90 {
            limiter.reserve(&format!("{i}@example.test"), ip).unwrap();
        }
        assert!(limiter.reserve("next@example.test", ip).is_err());
    }

    #[test]
    fn a_success_gives_back_its_attempt_and_clears_its_pair() {
        let limiter = LoginLimiter::default();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..99 {
            limiter.reserve("owner@example.test", ip).ok();
        }
        // 10 counted in the pair; the other 89 were refused and counted nowhere.
        for i in 0..89 {
            limiter.reserve(&format!("{i}@example.test"), ip).unwrap();
        }
        for _ in 0..1000 {
            limiter
                .reserve("other@example.test", ip)
                .unwrap()
                .succeeded();
        }
        let attempts = limiter.0.lock().unwrap();
        assert_eq!(attempts.entries[&LimitKey::Ip(ip)].count, 99);
        assert_eq!(
            attempts.entries[&LimitKey::Email("other@example.test".into())].count,
            0
        );
        assert!(
            !attempts
                .entries
                .contains_key(&LimitKey::Pair("other@example.test".into(), ip))
        );
    }

    #[test]
    fn clear_email_removes_its_buckets_and_never_an_ip_bucket() {
        let limiter = LoginLimiter::default();
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "2001:db8::1".parse().unwrap();
        for ip in [first, second] {
            for _ in 0..10 {
                limiter.reserve("owner@example.test", ip).unwrap();
            }
            limiter.reserve("other@example.test", ip).unwrap();
        }
        limiter.clear_email("owner@example.test");
        let attempts = limiter.0.lock().unwrap();
        let mut keys: Vec<_> = attempts.entries.keys().cloned().collect();
        keys.retain(|key| match key {
            LimitKey::Email(email) | LimitKey::Pair(email, _) => email == "owner@example.test",
            LimitKey::Ip(_) => false,
        });
        assert!(keys.is_empty());
        for ip in [first, second] {
            assert_eq!(attempts.entries[&LimitKey::Ip(ip_bucket(ip))].count, 11);
            assert!(
                attempts
                    .entries
                    .contains_key(&LimitKey::Pair("other@example.test".into(), ip_bucket(ip)))
            );
        }
        drop(attempts);
        limiter.reserve("owner@example.test", first).unwrap();
    }
}

#[cfg(test)]
mod unchecked_attempt_tests {
    use super::*;

    #[test]
    fn auth_busy_and_database_errors_never_count_toward_lockout() {
        let limiter = LoginLimiter::default();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        // No password was checked, so none of these is a failed guess.
        for _ in 0..10 {
            limiter
                .reserve("owner@example.test", ip)
                .unwrap()
                .finish(&Err::<(), _>(KatError::AuthBusy));
        }
        for _ in 0..10 {
            limiter
                .reserve("owner@example.test", ip)
                .unwrap()
                .finish(&Err::<(), _>(KatError::DatabaseUnavailable(Box::new(
                    std::io::Error::other("down"),
                ))));
        }
        {
            let attempts = limiter.0.lock().unwrap();
            assert_eq!(attempts.entries[&LimitKey::Ip(ip)].count, 0);
            assert_eq!(
                attempts.entries[&LimitKey::Pair("owner@example.test".into(), ip)].count,
                0
            );
            assert_eq!(
                attempts.entries[&LimitKey::Email("owner@example.test".into())].count,
                0
            );
        }
        // A checked, wrong password still counts: ten of them lock the pair.
        for _ in 0..10 {
            limiter
                .reserve("owner@example.test", ip)
                .unwrap()
                .finish(&Err::<(), _>(KatError::Unauthenticated));
        }
        assert!(limiter.reserve("owner@example.test", ip).is_err());
    }

    #[test]
    fn an_unchecked_attempt_never_clears_earlier_failures() {
        let limiter = LoginLimiter::default();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for _ in 0..9 {
            limiter
                .reserve("owner@example.test", ip)
                .unwrap()
                .finish(&Err::<(), _>(KatError::Unauthenticated));
        }
        // An attacker who makes the server busy must not wipe the pair the way a success does.
        limiter
            .reserve("owner@example.test", ip)
            .unwrap()
            .finish(&Err::<(), _>(KatError::AuthBusy));
        limiter
            .reserve("owner@example.test", ip)
            .unwrap()
            .finish(&Err::<(), _>(KatError::Unauthenticated));
        assert!(limiter.reserve("owner@example.test", ip).is_err());
    }
}
