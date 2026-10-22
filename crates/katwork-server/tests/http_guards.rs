use std::sync::Arc;

use axum::{Router, body::Body, http::Request, response::Response};
use katwork::{config::Config, runtime::Core};
use katwork_server::app::{AppState, build_router};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

fn app() -> Router {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        "KATWORK_PUBLIC_URL" => Some("https://katwork.example:8443".into()),
        _ => None,
    })
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    build_router(AppState {
        core: Arc::new(Core::new(pool, Arc::new(config))),
    })
}

async fn code(response: Response, status: u16, code: &str) {
    assert_eq!(response.status(), status);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"code": code})
    );
}

#[tokio::test]
async fn cross_site_post_is_403() {
    let response = app()
        .oneshot(
            Request::post("/api/auth/login")
                .header("host", "localhost")
                .header("origin", "https://evil.example")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    code(response, 403, "cross_site_request").await;
}

#[tokio::test]
async fn unrecognized_host_is_403() {
    for host in [
        None,
        Some("evil.example"),
        Some("localhost.evil.example"),
        Some("localhost@evil.example"),
        Some("localhost:bad"),
    ] {
        let mut request = Request::get("/healthz");
        if let Some(host) = host {
            request = request.header("host", host);
        }
        code(
            app()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap(),
            403,
            "host_not_allowed",
        )
        .await;
    }
}

#[tokio::test]
async fn allowed_host_ignores_port() {
    for host in [
        "localhost",
        "localhost:80",
        "katwork.example:9443",
        "KATWORK.EXAMPLE",
    ] {
        let response = app()
            .oneshot(
                Request::get("/healthz")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}

#[tokio::test]
async fn mutations_require_exact_origin_and_reject_cross_site_fetch() {
    for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        for origin in [
            None,
            Some("null"),
            Some("https://katwork.example"),
            Some("https://katwork.example:8443/"),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri("/api/auth/logout")
                .header("host", "localhost");
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            code(
                app()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap(),
                403,
                "cross_site_request",
            )
            .await;
        }
    }
    code(
        app()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header("host", "localhost")
                    .header("origin", "https://katwork.example:8443")
                    .header("sec-fetch-site", "cross-site")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}

#[tokio::test]
async fn non_json_mutation_is_415() {
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/jsonp"),
        Some("application/x-www-form-urlencoded"),
    ] {
        let mut request = Request::post("/api/auth/login")
            .header("host", "localhost")
            .header("origin", "https://katwork.example:8443");
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        code(
            app()
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap(),
            415,
            "json_required",
        )
        .await;
    }
}

#[tokio::test]
async fn responses_carry_nosniff_and_csp() {
    for (uri, host) in [
        ("/healthz", "localhost"),
        ("/missing", "localhost"),
        ("/healthz", "evil.example"),
    ] {
        let response = app()
            .oneshot(
                Request::get(uri)
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "default-src 'self'; frame-ancestors 'none'"
        );
    }
}

#[tokio::test]
async fn duplicate_host_and_origin_headers_are_rejected() {
    code(
        app()
            .oneshot(
                Request::get("/healthz")
                    .header("host", "localhost")
                    .header("host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "host_not_allowed",
    )
    .await;
    code(
        app()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header("host", "localhost")
                    .header("origin", "https://katwork.example:8443")
                    .header("origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}

#[tokio::test]
async fn auth_responses_disable_caching_and_all_responses_disable_referrers() {
    for (uri, host) in [
        ("/healthz", "localhost"),
        ("/missing", "localhost"),
        ("/api/auth/me", "localhost"),
        ("/api/auth/login", "evil.example"),
        ("/api/auth/missing", "localhost"),
    ] {
        let response = app()
            .oneshot(
                Request::get(uri)
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "no-referrer"
        );
        if uri.starts_with("/api/auth/") {
            assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn request_timeout_finishes_with_408_after_thirty_seconds() {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let router = katwork_server::guards::apply(
        Router::new().route(
            "/hang",
            axum::routing::get(|| async {
                std::future::pending::<()>().await;
            }),
        ),
        &config,
    );
    let start = tokio::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(31),
        router.oneshot(
            Request::get("/hang")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("request timeout missing")
    .unwrap();
    assert_eq!(response.status(), 408);
    assert_eq!(start.elapsed(), std::time::Duration::from_secs(30));
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
}

#[tokio::test]
async fn slow_client_body_read_times_out() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        katwork_server::transport::serve(listener, app())
            .await
            .unwrap()
    });
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(b"POST /api/auth/login HTTP/1.1\r\nHost: localhost\r\nOrigin: https://katwork.example:8443\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await.unwrap();
    let mut response = vec![0; 1024];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(11),
        client.read(&mut response),
    )
    .await;
    server.abort();
    let count = result.expect("slow body never timed out").unwrap();
    assert!(String::from_utf8_lossy(&response[..count]).starts_with("HTTP/1.1 408"));
}

#[tokio::test]
async fn concurrent_request_limit_rejects_overload() {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let signal = started.clone();
    let router = katwork_server::guards::apply(
        Router::new()
            .route(
                "/hang",
                axum::routing::get(move || {
                    let signal = signal.clone();
                    async move {
                        signal.add_permits(1);
                        std::future::pending::<()>().await;
                    }
                }),
            )
            .route("/ok", axum::routing::get(|| async {})),
        &config,
    );
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..256 {
        let router = router.clone();
        tasks.spawn(
            router.oneshot(
                Request::get("/hang")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
    }
    let _started = started.acquire_many(256).await.unwrap();
    let response = router
        .oneshot(
            Request::get("/ok")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
}
