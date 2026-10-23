use std::{net::SocketAddr, time::Duration};

use axum::{Router, routing::get};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{Instant, timeout},
};

async fn server() -> (SocketAddr, JoinHandle<()>) {
    configured_server(katwork::config::TransportConfig::default()).await
}

async fn configured_server(
    config: katwork::config::TransportConfig,
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        katwork_server::transport::serve_with_shutdown(
            listener,
            Router::new().route("/healthz", get(|| async { "ok" })),
            config,
            std::future::pending(),
        )
        .await
        .unwrap();
    });
    (addr, task)
}

async fn closed(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    match stream.read_to_end(&mut bytes).await {
        Ok(_) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
    }
}

#[tokio::test]
async fn partial_header_connection_closes_at_ten_seconds() {
    let (addr, task) = server().await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    let start = Instant::now();
    client
        .write_all(b"GET /healthz HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(11), closed(&mut client)).await;
    task.abort();
    result.expect("partial header retained a connection slot");
    assert!(start.elapsed() >= Duration::from_secs(9));
}

#[tokio::test]
async fn idle_socket_flood_releases_capacity_within_header_timeout() {
    let (addr, task) = configured_server(katwork::config::TransportConfig {
        max_connections_per_ip: 256,
        ..Default::default()
    })
    .await;
    let mut idle = Vec::new();
    for _ in 0..256 {
        idle.push(TcpStream::connect(addr).await.unwrap());
    }
    let start = Instant::now();
    let result = timeout(Duration::from_secs(11), async {
        loop {
            let mut client = TcpStream::connect(addr).await.unwrap();
            let mut response = String::new();
            if client
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .is_ok()
                && client.read_to_string(&mut response).await.is_ok()
                && response.starts_with("HTTP/1.1 200")
            {
                assert!(
                    start.elapsed() >= Duration::from_secs(9),
                    "global connection cap was bypassed"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for client in &mut idle {
            closed(client).await;
        }
    })
    .await;
    task.abort();
    result.expect("256 idle sockets blocked health or stayed open beyond 11 seconds");
}

#[tokio::test]
async fn one_peer_cannot_hold_more_than_thirty_two_connections() {
    let (addr, task) = server().await;
    let mut idle = Vec::new();
    for _ in 0..32 {
        idle.push(TcpStream::connect(addr).await.unwrap());
    }
    let mut excess = TcpStream::connect(addr).await.unwrap();
    let result = timeout(Duration::from_secs(1), closed(&mut excess)).await;
    task.abort();
    result.expect("peer exceeded its 32 connection share");
}

#[tokio::test]
async fn keep_alive_connection_closes_at_configured_idle_timeout() {
    let (addr, task) = configured_server(katwork::config::TransportConfig {
        header_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = [0; 1024];
    let size = client.read(&mut response).await.unwrap();
    assert!(response[..size].starts_with(b"HTTP/1.1 200"));
    let start = Instant::now();
    let result = timeout(Duration::from_secs(1), closed(&mut client)).await;
    task.abort();
    result.expect("idle keep-alive retained a slot");
    assert!(start.elapsed() >= Duration::from_millis(150));
}

#[tokio::test]
async fn graceful_shutdown_drains_in_flight_response_and_closes_idle_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let handler_entered = entered.clone();
    let handler_release = release.clone();
    let router = Router::new().route(
        "/healthz",
        get(move || {
            let entered = handler_entered.clone();
            let release = handler_release.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                "ok"
            }
        }),
    );
    let task = tokio::spawn(katwork_server::transport::serve_with_shutdown(
        listener,
        router,
        Default::default(),
        async {
            stop_rx.await.unwrap();
        },
    ));
    let mut idle = TcpStream::connect(addr).await.unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    stop_tx.send(()).unwrap();
    timeout(Duration::from_secs(1), closed(&mut idle))
        .await
        .unwrap();
    assert!(!task.is_finished(), "shutdown discarded an active request");
    release.notify_one();
    let mut response = String::new();
    timeout(Duration::from_secs(1), client.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.ends_with("ok"));
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
}
#[path = "transport/events.rs"]
mod events;

async fn served(stream: &mut TcpStream) {
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = [0; 1024];
    let size = timeout(Duration::from_secs(5), stream.read(&mut response))
        .await
        .expect("open connection was not served")
        .unwrap();
    assert!(
        response[..size].starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response[..size])
    );
}

#[tokio::test]
async fn trusted_proxy_is_exempt_from_the_per_peer_cap() {
    // One peer address, 127.0.0.1, is trusted by one server and not by the other.
    let (untrusted, untrusted_task) = server().await;
    let mut held = Vec::new();
    for _ in 0..32 {
        let mut stream = TcpStream::connect(untrusted).await.unwrap();
        served(&mut stream).await;
        held.push(stream);
    }
    let mut excess = TcpStream::connect(untrusted).await.unwrap();
    timeout(Duration::from_secs(5), closed(&mut excess))
        .await
        .expect("untrusted peer exceeded its 32 connection share");
    untrusted_task.abort();

    // The trusted peer holds 40 connections; the global cap, here 40, still applies to it.
    let (trusted, trusted_task) = configured_server(katwork::config::TransportConfig {
        max_connections: 40,
        trusted_proxies: vec![("127.0.0.1".parse().unwrap(), 32)],
        ..Default::default()
    })
    .await;
    let mut open = Vec::new();
    for _ in 0..40 {
        let mut stream = TcpStream::connect(trusted).await.unwrap();
        served(&mut stream).await;
        open.push(stream);
    }
    for stream in &mut open {
        served(stream).await;
    }
    let mut waiting = TcpStream::connect(trusted).await.unwrap();
    waiting
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut byte = [0; 1];
    assert!(
        timeout(Duration::from_millis(200), waiting.read(&mut byte))
            .await
            .is_err(),
        "a connection past the global cap was served"
    );
    drop(open.pop());
    let mut response = [0; 1024];
    let size = timeout(Duration::from_secs(5), waiting.read(&mut response))
        .await
        .expect("freed capacity was not reused")
        .unwrap();
    assert!(response[..size].starts_with(b"HTTP/1.1 200"));
    trusted_task.abort();
}

#[tokio::test]
async fn dropped_connections_are_logged() {
    use tracing::instrument::WithSubscriber;

    #[derive(Clone, Default)]
    struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // With one dispatcher registered, tracing takes a callsite's interest from whichever thread
    // hits it first; a parallel test hitting it uncaptured would cache "never". A second, idle
    // dispatcher makes every callsite's interest come from all registered dispatchers.
    let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(
        async move {
            katwork_server::transport::serve_with_shutdown(
                listener,
                Router::new().route("/healthz", get(|| async { "ok" })),
                katwork::config::TransportConfig::default(),
                std::future::pending(),
            )
            .await
            .unwrap();
        }
        .with_subscriber(subscriber),
    );
    let mut open = Vec::new();
    for _ in 0..32 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        served(&mut stream).await;
        open.push(stream);
    }
    for _ in 0..3 {
        let mut excess = TcpStream::connect(addr).await.unwrap();
        timeout(Duration::from_secs(5), closed(&mut excess))
            .await
            .expect("peer exceeded its 32 connection share");
    }
    task.abort();
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let dropped: Vec<_> = text
        .lines()
        .filter(|line| line.contains("connection_dropped"))
        .collect();
    // One line per bucket per 60 s, however many connections it drops.
    assert_eq!(dropped.len(), 1, "{text}");
    assert!(dropped[0].contains("WARN"), "{text}");
    assert!(dropped[0].contains(r#"reason="per_peer_cap""#), "{text}");
    assert!(dropped[0].contains("peer=127.0.0.1"), "{text}");
}
