#[path = "../../../katwork/tests/support/mod.rs"]
#[allow(dead_code)]
mod data;
#[path = "../support/mod.rs"]
#[allow(dead_code)]
mod support;

use axum::{Router, body::Body, response::Response, routing::get};
use katwork::{auth::session, config::Config, ids::UserId, runtime::Core};
use katwork_server::{
    app::{AppState, build_router},
    transport::serve_with_shutdown,
};
use sqlx::PgPool;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

async fn serve(
    pool: &PgPool,
    proxy: bool,
) -> (
    Arc<Core>,
    SocketAddr,
    oneshot::Sender<()>,
    JoinHandle<std::io::Result<()>>,
) {
    let config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/test".into()),
        "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
        "KATWORK_TRUSTED_PROXIES" if proxy => Some("127.0.0.1/32".into()),
        _ => None,
    })
    .unwrap();
    let transport = config.transport.clone();
    let core = Arc::new(Core::new(pool.clone(), Arc::new(config)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (send, receive) = oneshot::channel();
    let token = core.shutdown.clone();
    let task = tokio::spawn(serve_with_shutdown(
        listener,
        build_router(AppState { core: core.clone() }),
        transport,
        async move {
            let _ = receive.await;
            token.cancel();
        },
    ));
    (core, addr, send, task)
}

async fn send(stream: &mut TcpStream, method: &str, path: &str, token: &str, body: &str) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\nCookie: kw_session={token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
}

async fn read_until(stream: &mut TcpStream, needle: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection ended before expected bytes");
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.windows(needle.len()).any(|w| w == needle) {
                return bytes;
            }
        }
    })
    .await
    .expect("connection stalled")
}

fn path(f: &data::Fixture) -> String {
    format!(
        "/api/workspaces/{}/threads/{}/events",
        f.workspace, f.thread
    )
}

async fn open(addr: SocketAddr, path: &str, token: &str) -> TcpStream {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    send(&mut socket, "GET", path, token, "").await;
    let response = read_until(&mut socket, b"retry: 3000\n\n").await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    socket
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn stream_ends_on_shutdown_token(pool: PgPool) {
    let f = data::Fixture::new(&pool).await;
    let token = session::create(&pool, UserId(f.user)).await.unwrap().0;
    let (core, addr, stop, task) = serve(&pool, false).await;
    let mut stream = open(addr, &path(&f), &token).await;
    core.shutdown.cancel();
    stop.send(()).unwrap();
    let mut tail = Vec::new();
    timeout(Duration::from_secs(1), stream.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(core.events.sender_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_aborts_connections_after_grace() {
    let spinner = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, receive) = oneshot::channel();
    let (seen, mut notice) = tokio::sync::mpsc::channel(1);
    let router = Router::new().route(
        "/",
        get(move || async move {
            Response::new(Body::from_stream(futures::stream::pending::<
                Result<String, std::io::Error>,
            >()))
        }),
    );
    let task = tokio::spawn(serve_with_shutdown(
        listener,
        router,
        Default::default(),
        async move {
            receive.await.unwrap();
            seen.send(()).await.unwrap();
        },
    ));
    let mut stream = TcpStream::connect(addr).await.unwrap();
    send(&mut stream, "GET", "/", "", "").await;
    read_until(&mut stream, b"\r\n\r\n").await;
    stop.send(()).unwrap();
    notice.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    spinner.abort();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn four_streams_leave_two_connections_for_requests(pool: PgPool) {
    let f = data::Fixture::new(&pool).await;
    let token = session::create(&pool, UserId(f.user)).await.unwrap().0;
    let (_core, addr, stop, task) = serve(&pool, false).await;
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(open(addr, &path(&f), &token).await);
    }
    // Exactly six sockets, matching a browser's HTTP/1 host budget.
    let mut fifth = TcpStream::connect(addr).await.unwrap();
    let mut sixth = TcpStream::connect(addr).await.unwrap();
    send(&mut fifth, "GET", &path(&f), &token, "").await;
    let excess = read_until(&mut fifth, b"0\r\n\r\n").await;
    assert!(excess.starts_with(b"HTTP/1.1 200"));
    assert!(excess.windows(16).any(|w| w == b"too_many_streams"));
    let messages = format!(
        "/api/workspaces/{}/threads/{}/messages",
        f.workspace, f.thread
    );
    send(
        &mut fifth,
        "POST",
        &messages,
        &token,
        "{\"body\":\"hello\"}",
    )
    .await;
    let response = read_until(&mut fifth, b"}").await;
    assert!(response.starts_with(b"HTTP/1.1 201"));
    // The job cancel route belongs to Task 12; exercise the spare socket through auth today.
    send(&mut sixth, "GET", "/api/auth/me", &token, "").await;
    assert!(
        read_until(&mut sixth, b"}")
            .await
            .starts_with(b"HTTP/1.1 200")
    );
    send(&mut fifth, "POST", "/api/auth/logout", &token, "").await;
    assert!(
        read_until(&mut fifth, b"\r\n\r\n")
            .await
            .starts_with(b"HTTP/1.1 200")
    );
    drop(streams);
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn streams_through_trusted_proxy_peer_do_not_block_login(pool: PgPool) {
    let f = data::Fixture::new(&pool).await;
    let (_, password) = support::user(&pool).await;
    let (_core, addr, stop, task) = serve(&pool, true).await;
    let mut streams = Vec::new();
    let mut token = String::new();
    for _ in 0..10 {
        token = session::create(&pool, UserId(f.user)).await.unwrap().0;
        for _ in 0..4 {
            streams.push(open(addr, &path(&f), &token).await);
        }
    }
    let mut socket = TcpStream::connect(addr).await.unwrap();
    send(&mut socket, "GET", &path(&f), &token, "").await;
    let excess = read_until(&mut socket, b"0\r\n\r\n").await;
    assert!(excess.starts_with(b"HTTP/1.1 200"));
    assert!(excess.windows(16).any(|w| w == b"too_many_streams"));
    let body = serde_json::json!({"email":"person@example.test","password":password}).to_string();
    send(&mut socket, "POST", "/api/auth/login", "", &body).await;
    assert!(
        read_until(&mut socket, b"}")
            .await
            .starts_with(b"HTTP/1.1 200")
    );
    drop(streams);
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
