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

#[tokio::test]
async fn pending_request_bodies_count_against_the_concurrency_limit() {
    let app = app();
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..256 {
        let signal = started.clone();
        let body = Body::from_stream(async_stream::stream! {
            signal.add_permits(1);
            std::future::pending::<()>().await;
            yield Ok::<_, std::io::Error>("{}");
        });
        tasks.spawn(
            app.clone().oneshot(
                Request::post("/api/auth/login")
                    .header("host", "localhost")
                    .header("origin", "https://katwork.example:8443")
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            ),
        );
    }
    let _entered = started.acquire_many(256).await.unwrap();
    let response = app
        .oneshot(
            Request::get("/healthz")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
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
        // `/healthz` skips the Host check; every other path keeps it.
        let mut request = Request::get("/api/auth/me");
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
        // Past the Host check, `/api/auth/me` without a session is 401.
        code(
            app()
                .oneshot(
                    Request::get("/api/auth/me")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
            401,
            "unauthenticated",
        )
        .await;
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
        ("/api/auth/me", "evil.example"),
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
                Request::get("/api/auth/me")
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

fn default_app() -> Router {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
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

#[tokio::test]
async fn loopback_ip_hosts_are_allowed() {
    for app in [app(), default_app()] {
        for host in [
            "127.0.0.1:3000",
            "127.0.0.1",
            "[::1]:3000",
            "[::1]",
            "localhost:3000",
        ] {
            code(
                app.clone()
                    .oneshot(
                        Request::get("/api/auth/me")
                            .header("host", host)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
                401,
                "unauthenticated",
            )
            .await;
        }
        for host in [
            "127.0.0.2",
            "127.0.0.2:3000",
            "[::2]:3000",
            "0.0.0.0:3000",
            "[::1",
        ] {
            code(
                app.clone()
                    .oneshot(
                        Request::get("/api/auth/me")
                            .header("host", host)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
                403,
                "host_not_allowed",
            )
            .await;
        }
    }
}

#[tokio::test]
async fn healthz_skips_the_host_check() {
    for method in ["GET", "HEAD"] {
        let response = app()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/healthz")
                    .header("host", "katwork:3000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{method}");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'self'; frame-ancestors 'none'"
        );
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    }
    for (method, uri) in [
        ("GET", "/api/auth/me"),
        ("GET", "/healthz/"),
        ("GET", "/healthz/extra"),
        ("POST", "/healthz"),
    ] {
        code(
            app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("host", "katwork:3000")
                        .header("origin", "http://katwork:3000")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
            403,
            "host_not_allowed",
        )
        .await;
    }
}

#[tokio::test]
async fn origin_matching_the_allowed_host_is_accepted() {
    let logout = |host: &'static str, origin: &'static str, cross_site: bool| {
        let mut request = Request::post("/api/auth/logout")
            .header("host", host)
            .header("origin", origin);
        if cross_site {
            request = request.header("sec-fetch-site", "cross-site");
        }
        request.body(Body::empty()).unwrap()
    };
    // The public URL defaults to http://localhost:3000, so the scheme is http.
    for (host, origin) in [
        ("127.0.0.1:3000", "http://127.0.0.1:3000"),
        ("[::1]:3000", "http://[::1]:3000"),
        ("localhost:3000", "http://localhost:3000"),
        ("localhost:4000", "http://localhost:4000"),
        ("127.0.0.1:3000", "http://localhost:3000"),
    ] {
        let response = default_app()
            .oneshot(logout(host, origin, false))
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{host} {origin}");
    }
    for (host, origin, cross_site) in [
        ("127.0.0.1:3000", "http://127.0.0.1:4000", false),
        ("127.0.0.1:3000", "https://127.0.0.1:3000", false),
        ("127.0.0.1:3000", "http://127.0.0.1", false),
        ("127.0.0.1:3000", "http://127.0.0.1:3000", true),
        (
            "localhost:4000",
            "http://localhost:3000.evil.example",
            false,
        ),
    ] {
        code(
            default_app()
                .oneshot(logout(host, origin, cross_site))
                .await
                .unwrap(),
            403,
            "cross_site_request",
        )
        .await;
    }
    // With an https public URL the Host-derived origin is https too.
    let response = app()
        .oneshot(logout("127.0.0.1:3000", "https://127.0.0.1:3000", false))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    code(
        app()
            .oneshot(logout("127.0.0.1:3000", "http://127.0.0.1:3000", false))
            .await
            .unwrap(),
        403,
        "cross_site_request",
    )
    .await;
}

#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    async fn capture(&self, request: Request<Body>) -> Response {
        self.capture_with(&app(), request).await
    }

    async fn capture_with(&self, app: &Router, request: Request<Body>) -> Response {
        use tracing::instrument::WithSubscriber;
        let logs = self.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || logs.clone())
            .finish();
        app.clone()
            .oneshot(request)
            .with_subscriber(subscriber)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn host_and_origin_rejections_log_the_rejected_value() {
    // With one dispatcher registered, tracing takes a callsite's interest from whichever thread
    // hits it first; a parallel test hitting it uncaptured would cache "never". A second, idle
    // dispatcher makes every callsite's interest come from all registered dispatchers.
    let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let marker = uuid::Uuid::now_v7().to_string();
    let logs = Logs::default();
    let response = logs
        .capture(
            Request::get("/api/auth/me")
                .header("host", "evil.example")
                .header("cookie", format!("kw_session={marker}"))
                .header("user-agent", &marker)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    code(response, 403, "host_not_allowed").await;
    let text = logs.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with(" WARN") || text.starts_with("WARN"),
        "{text}"
    );
    assert!(text.contains("host_not_allowed"), "{text}");
    assert!(text.contains(r#"host="evil.example""#), "{text}");
    assert!(!text.contains(&marker), "{text}");

    let logs = Logs::default();
    let response = logs
        .capture(
            Request::post("/api/auth/logout")
                .header("host", "localhost")
                .header("origin", "https://evil.example")
                .header("cookie", format!("kw_session={marker}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    code(response, 403, "cross_site_request").await;
    let text = logs.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("WARN"), "{text}");
    assert!(text.contains("cross_site_request"), "{text}");
    assert!(text.contains(r#"origin="https://evil.example""#), "{text}");
    assert!(!text.contains("localhost"), "{text}");
    assert!(!text.contains(&marker), "{text}");

    // A long value is cut to 256 bytes on a character boundary; control characters are escaped.
    let logs = Logs::default();
    let long = format!("{}.example", "a".repeat(1_000));
    let response = logs
        .capture(
            Request::get("/api/auth/me")
                .header("host", &long)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    code(response, 403, "host_not_allowed").await;
    let text = logs.text();
    assert!(
        text.contains(&format!(r#"host="{}""#, "a".repeat(256))),
        "{text}"
    );
    assert!(!text.contains(&"a".repeat(257)), "{text}");

    let logs = Logs::default();
    let thai = format!("{}ก.example", "a".repeat(255));
    let response = logs
        .capture(
            Request::post("/api/auth/logout")
                .header("host", "localhost")
                .header(
                    "origin",
                    axum::http::HeaderValue::from_bytes(thai.as_bytes()).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    code(response, 403, "cross_site_request").await;
    let text = logs.text();
    assert!(
        text.contains(&format!(r#"origin="{}""#, "a".repeat(255))),
        "{text}"
    );
    assert!(!text.contains('ก'), "{text}");

    let logs = Logs::default();
    let response = logs
        .capture(
            Request::post("/api/auth/logout")
                .header("host", "localhost")
                .header("origin", "https://evil.example\t\"forged=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    code(response, 403, "cross_site_request").await;
    let text = logs.text();
    assert!(
        text.contains(r#"origin="https://evil.example\t\"forged=1""#),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn rejection_logs_are_throttled_per_peer_and_kind() {
    // See host_and_origin_rejections_log_the_rejected_value for the idle dispatcher.
    let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let logs = Logs::default();
    let app = app();
    let from = |peer: &str, request: axum::http::request::Builder| {
        request
            .extension(axum::extract::ConnectInfo(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::empty())
            .unwrap()
    };
    let bad_host = || Request::get("/api/auth/me").header("host", "evil.example");
    let cross_site = || {
        Request::post("/api/auth/logout")
            .header("host", "localhost")
            .header("origin", "https://evil.example")
    };
    for _ in 0..3 {
        let response = logs
            .capture_with(&app, from("192.0.2.1:7000", bad_host()))
            .await;
        code(response, 403, "host_not_allowed").await;
    }
    let response = logs
        .capture_with(&app, from("192.0.2.2:7000", bad_host()))
        .await;
    code(response, 403, "host_not_allowed").await;
    for _ in 0..2 {
        let response = logs
            .capture_with(&app, from("192.0.2.1:7001", cross_site()))
            .await;
        code(response, 403, "cross_site_request").await;
    }
    let lines =
        |text: String, event: &str| text.lines().filter(|line| line.contains(event)).count();
    // Every request is refused; one line per peer and kind is logged per minute.
    assert_eq!(lines(logs.text(), "host_not_allowed"), 2, "{}", logs.text());
    assert_eq!(
        lines(logs.text(), "cross_site_request"),
        1,
        "{}",
        logs.text()
    );
    tokio::time::advance(std::time::Duration::from_secs(59)).await;
    let response = logs
        .capture_with(&app, from("192.0.2.1:7000", bad_host()))
        .await;
    code(response, 403, "host_not_allowed").await;
    assert_eq!(lines(logs.text(), "host_not_allowed"), 2, "{}", logs.text());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let response = logs
        .capture_with(&app, from("192.0.2.1:7000", bad_host()))
        .await;
    code(response, 403, "host_not_allowed").await;
    assert_eq!(lines(logs.text(), "host_not_allowed"), 3, "{}", logs.text());
}
