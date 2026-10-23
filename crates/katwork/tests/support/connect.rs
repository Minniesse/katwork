use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};

// Speak only enough PostgreSQL to control the pool's transient-error retry path.
async fn failing_server(message: Option<String>) -> (String, Arc<AtomicUsize>, JoinSet<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "postgres://katwork@{}/test?sslmode=disable",
        listener.local_addr().unwrap()
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let mut server = JoinSet::new();
    server.spawn(async move {
        let mut stalled = Vec::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            if let Some(message) = &message {
                let size = socket.read_u32().await.unwrap() as usize;
                let mut startup = vec![0; size - 4];
                socket.read_exact(&mut startup).await.unwrap();
                let fields = format!("SFATAL\0C57P03\0M{message}\0\0");
                socket.write_u8(b'E').await.unwrap();
                socket.write_u32((fields.len() + 4) as u32).await.unwrap();
                socket.write_all(fields.as_bytes()).await.unwrap();
            } else {
                stalled.push(socket);
            }
        }
    });
    (url, attempts, server)
}

#[tokio::test]
async fn connection_failure_fallback_recovers_a_redacted_cause() {
    use std::error::Error;
    let secret = uuid::Uuid::now_v7().to_string();
    let (url, attempts, _server) =
        failing_server(Some(format!("starting database {secret}"))).await;
    let url = url.replacen("katwork@", &format!("katwork:{secret}@"), 1);
    let config = crate::config::Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some(url.clone()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        connect_with(pool_options().acquire_timeout(Duration::from_secs(1)), &url),
    )
    .await
    .expect("connection failure must finish")
    .unwrap_err();
    assert_eq!(error.code(), "database_unavailable");
    let cause = error.source().unwrap();
    let database = cause
        .downcast_ref::<sqlx::Error>()
        .unwrap()
        .as_database_error()
        .unwrap();
    assert_eq!(database.code().as_deref(), Some("57P03"));
    assert!(
        cause.to_string().contains(&secret),
        "fixture must exercise redaction"
    );
    let report = config.redact_secrets(&cause.to_string());
    assert!(report.contains("starting database [redacted]"), "{report}");
    assert!(!report.contains(&secret));
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "pool and fallback must both connect"
    );
}

#[tokio::test]
async fn connection_failure_fallback_timeout_keeps_pool_cause() {
    use std::error::Error;
    let (url, attempts, _server) = failing_server(None).await;
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        connect_with(pool_options().acquire_timeout(Duration::from_secs(1)), &url),
    )
    .await
    .expect("fallback must stop even when the server never replies")
    .unwrap_err();
    assert_eq!(error.code(), "database_unavailable");
    assert!(matches!(
        error.source().unwrap().downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::PoolTimedOut)
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "one pool attempt and one bounded fallback"
    );
}
