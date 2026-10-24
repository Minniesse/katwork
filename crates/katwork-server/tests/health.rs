use std::sync::Arc;

use axum::{body::Body, http::Request};
use katwork::runtime::Core;
use katwork_server::{
    app::{AppState, build_router},
    config::Config,
};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_ok_and_version() {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    let app = build_router(AppState {
        core: Arc::new(Core::new(pool, Arc::new(config))),
    });
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
        })
    );
}

#[tokio::test]
async fn healthz_reports_a_wedged_document_pool() {
    use katwork::{
        docs::{DocLimits, DocPool},
        ids::WorkspaceId,
    };
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .unwrap();
    let docs = Arc::new(
        DocPool::new(
            1,
            DocLimits {
                call_timeout: Duration::from_millis(50),
                ..DocLimits::default()
            },
        )
        .unwrap(),
    );
    let core = Arc::new(Core::new(pool, Arc::new(config)).with_docs(docs.clone()));
    let cancel = core.shutdown.child_token();
    let app = build_router(AppState { core });
    let health = |app: axum::Router| async move {
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        )
    };
    let poll = |want: u16| {
        let app = app.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let (status, body) = health(app.clone()).await;
                    if status == want {
                        return body;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("healthz reached the expected status")
        }
    };
    assert_eq!(health(app.clone()).await.0, 200);

    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let blocked = tokio::spawn({
        let (docs, started, barrier) = (docs.clone(), started.clone(), barrier.clone());
        async move {
            docs.run(WorkspaceId::new(), &cancel, None, move |_| {
                started.store(true, Ordering::SeqCst);
                barrier.wait();
                Ok(())
            })
            .await
        }
    });
    assert_eq!(
        poll(503).await,
        serde_json::json!({"code": "doc_pool_wedged"})
    );
    assert!(started.load(Ordering::SeqCst));
    barrier.wait();
    assert_eq!(
        blocked.await.unwrap(),
        Err(katwork::docs::DocError::CallTimeout)
    );
    assert_eq!(poll(200).await["status"], "ok");
}
