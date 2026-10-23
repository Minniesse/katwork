use std::time::Duration;

use axum::{
    Router,
    body::{Body, Bytes, HttpBody},
    error_handling::HandleErrorLayer,
    extract::{DefaultBodyLimit, FromRequest, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use katwork::{config::Config, error::KatError};
use url::Url;

use crate::error::ApiError;

#[derive(Clone)]
struct GuardConfig {
    host: Option<String>,
    origin: Option<String>,
    scheme: Option<String>,
}

/// Hosts a local browser or a port-forward reaches the server by, on any port.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Logged header values are cut to this many bytes.
const LOGGED_VALUE_BYTES: usize = 256;

pub fn apply(router: Router, config: &Config) -> Router {
    let url = Url::parse(config.public_url_or_default()).ok();
    let config = GuardConfig {
        host: url
            .as_ref()
            .and_then(|url| url.host_str().map(str::to_owned)),
        scheme: url.as_ref().map(|url| url.scheme().to_owned()),
        origin: url.map(|url| url.origin().ascii_serialization()),
    };
    router
        .layer(middleware::from_fn(read_body))
        // Task 9: SSE must bypass these timeout/concurrency layers, keeping the security guards.
        .layer(
            tower::ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: axum::BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        StatusCode::REQUEST_TIMEOUT
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }))
                .timeout(Duration::from_secs(30))
                .load_shed()
                .layer(tower::limit::GlobalConcurrencyLimitLayer::new(256)),
        )
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(middleware::from_fn_with_state(config, guard))
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

/// Every value of the header, lossily decoded and cut to [`LOGGED_VALUE_BYTES`] on a
/// character boundary, for a rejection log. Its Debug form escapes control characters.
fn logged_value(headers: &HeaderMap, name: &str) -> String {
    let mut value = headers
        .get_all(name)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    if value.len() > LOGGED_VALUE_BYTES {
        let mut end = LOGGED_VALUE_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn check(request: &Request, config: &GuardConfig) -> Result<(), KatError> {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    // A container health probe addresses the server by its service name.
    if safe && request.uri().path() == "/healthz" {
        return Ok(());
    }
    let host = single_header(request.headers(), "host")
        .and_then(|value| value.parse::<Authority>().ok())
        .filter(|authority| !authority.as_str().contains('@'))
        .filter(|authority| {
            authority
                .as_str()
                .strip_prefix(authority.host())
                .is_some_and(|rest| {
                    rest.is_empty()
                        || rest
                            .strip_prefix(':')
                            .is_some_and(|port| port.parse::<u16>().is_ok())
                })
        })
        .filter(|authority| {
            let host = authority.host();
            LOOPBACK_HOSTS
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
                || config
                    .host
                    .as_ref()
                    .is_some_and(|allowed| host.eq_ignore_ascii_case(allowed))
        });
    let Some(host) = host else {
        tracing::warn!(
            host = ?logged_value(request.headers(), "host"),
            "host_not_allowed"
        );
        return Err(KatError::HostNotAllowed);
    };
    if !safe {
        let origin = single_header(request.headers(), "origin");
        // The public URL's origin, or the one the allowed Host names under the public URL's scheme.
        let same_origin = origin.is_some_and(|origin| {
            config.origin.as_deref() == Some(origin)
                || config.scheme.as_ref().is_some_and(|scheme| {
                    origin.eq_ignore_ascii_case(&format!("{scheme}://{}", host.as_str()))
                })
        });
        if !same_origin
            || request
                .headers()
                .get_all("sec-fetch-site")
                .iter()
                .any(|value| value == "cross-site")
        {
            tracing::warn!(
                origin = ?logged_value(request.headers(), "origin"),
                "cross_site_request"
            );
            return Err(KatError::CrossSiteRequest);
        }
        if !request.body().is_end_stream()
            && !single_header(request.headers(), "content-type").is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            })
        {
            return Err(KatError::JsonRequired);
        }
    }
    Ok(())
}

async fn guard(State(config): State<GuardConfig>, request: Request, next: Next) -> Response {
    let auth = request.uri().path().starts_with("/api/auth/");
    let mut response = match check(&request, &config) {
        Ok(()) => next.run(request).await,
        Err(error) => ApiError(error).into_response(),
    };
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; frame-ancestors 'none'"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if auth {
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
    }
    response
}

async fn read_body(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        Bytes::from_request(Request::from_parts(parts.clone(), body), &()),
    )
    .await;
    match bytes {
        Ok(Ok(bytes)) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Ok(Err(error)) => ApiError(if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            KatError::BodyTooLarge
        } else {
            KatError::RequestInvalid
        })
        .into_response(),
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}
