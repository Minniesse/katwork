use std::{
    convert::Infallible,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json,
    body::Body,
    extract::Request,
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use serde_json::json;
use tower::{Service, ServiceExt};
use tower_http::services::{ServeDir, ServeFile};

/// The built web app, relative to the working directory (`/app` in the image).
pub const DEFAULT_WEB_DIR: &str = "web/dist";

/// The router's fallback: the built web app, with client-side routes falling back to
/// `index.html`. `/api` and everything under it never fall back; an unknown API path is a
/// JSON 404.
pub fn service(
    dir: PathBuf,
) -> impl Service<
    Request,
    Response = Response,
    Error = Infallible,
    Future = impl Future<Output = Result<Response, Infallible>> + Send,
> + Clone
+ Send
+ Sync
+ 'static {
    let dir = Arc::new(dir);
    tower::service_fn(move |request: Request| {
        let dir = dir.clone();
        async move { Ok(respond(&dir, request).await) }
    })
}

async fn respond(dir: &Path, request: Request) -> Response {
    let path = request.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, Json(json!({"code": "not_found"}))).into_response();
    }
    // A path whose last segment has no extension is a client-side route; a missing file
    // with an extension stays a 404 rather than turning into HTML.
    let last_segment = path.rsplit('/').next().unwrap_or_default();
    let client_route = !last_segment.contains('.');
    let hashed_asset = path.starts_with("/assets/");
    // HTML by name as well as by type: a 304 revalidation carries no `Content-Type`. The name is
    // decoded as `ServeDir` decodes it, so `/index.%68tml` is `index.html` too.
    let html_name = percent_decode_str(last_segment)
        .decode_utf8()
        .is_ok_and(|name| {
            Path::new(&*name).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            })
        });
    let served = if client_route {
        ServeFile::new(dir.join("index.html"))
            .oneshot(request)
            .await
    } else {
        ServeDir::new(dir).oneshot(request).await
    };
    let mut response = match served {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    };
    let html = client_route
        || html_name
        || response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/html"));
    let cache = if html {
        // The app's HTML is never stored, so neither an HTTP cache nor the browser's
        // back-forward cache keeps a page that held an invite or reset token in memory. That
        // covers a direct `/index.html`, an HTML file or extensionless path under `/assets`, and
        // their 304s, whose headers replace a stored copy's; only the hashed files are immutable.
        "no-store"
    } else if hashed_asset && response.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}
