use axum::{
    Router,
    body::HttpBody,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, uri::Authority},
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
}

pub fn apply(router: Router, config: &Config) -> Router {
    let url = Url::parse(config.public_url_or_default()).ok();
    let config = GuardConfig {
        host: url
            .as_ref()
            .and_then(|url| url.host_str().map(str::to_owned)),
        origin: url.map(|url| url.origin().ascii_serialization()),
    };
    router
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(middleware::from_fn_with_state(config, guard))
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn check(request: &Request, config: &GuardConfig) -> Result<(), KatError> {
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
        .ok_or(KatError::HostNotAllowed)?;
    if !host.host().eq_ignore_ascii_case("localhost")
        && !config
            .host
            .as_ref()
            .is_some_and(|allowed| host.host().eq_ignore_ascii_case(allowed))
    {
        return Err(KatError::HostNotAllowed);
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        if config.origin.is_none()
            || single_header(request.headers(), "origin") != config.origin.as_deref()
            || request
                .headers()
                .get_all("sec-fetch-site")
                .iter()
                .any(|value| value == "cross-site")
        {
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
}
