use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
    time::Duration,
};

use axum::{Extension, Router, extract::ConnectInfo};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use katwork::config::TransportConfig;

use crate::log_throttle::LogThrottle;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
    time::{Instant, Sleep},
};

pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn serve(tcp: TcpListener, router: Router) -> io::Result<()> {
    serve_with_shutdown(
        tcp,
        router,
        TransportConfig::default(),
        std::future::pending(),
    )
    .await
}

pub async fn serve_with_shutdown(
    tcp: TcpListener,
    router: Router,
    config: TransportConfig,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    serve_connections(
        tcp,
        router,
        config,
        shutdown,
        TcpListener::poll_accept,
        configure_socket,
    )
    .await
}

async fn serve_connections(
    tcp: TcpListener,
    router: Router,
    config: TransportConfig,
    shutdown: impl Future<Output = ()>,
    mut accept: impl FnMut(&TcpListener, &mut Context<'_>) -> Poll<io::Result<(TcpStream, SocketAddr)>>,
    configure_socket: impl Fn(&TcpStream) -> io::Result<()>,
) -> io::Result<()> {
    let mut connections = JoinSet::new();
    let mut peers: HashMap<IpAddr, Weak<Semaphore>> = HashMap::new();
    let mut last_accept_warning = None;
    let mut last_socket_warning = None;
    let mut drops = LogThrottle::default();
    let (stop, _) = watch::channel(());
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = std::future::poll_fn(|cx| accept(&tcp, cx)), if connections.len() < config.max_connections => {
                let (stream, addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) if matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused) => continue,
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput || tcp.local_addr().is_err() => break Err(error),
                    Err(error) => {
                        // Pending network errors can surface from accept; resource exhaustion also needs a retry delay.
                        warn_transport_error(&mut last_accept_warning, "accept", &error);
                        tokio::select! {
                            () = &mut shutdown => break Ok(()),
                            () = tokio::time::sleep(Duration::from_secs(1)) => continue,
                        }
                    }
                };
                if let Err(error) = configure_socket(&stream) {
                    warn_transport_error(&mut last_socket_warning, "socket tuning", &error);
                }
                // Headers do not exist at accept time. A trusted proxy carries every client behind
                // it, so it skips the per-peer cap; it still counts toward the global one.
                let permit = if config.trusts_proxy(addr.ip()) {
                    None
                } else {
                    let ip = crate::peer::ip_bucket(addr.ip());
                    peers.retain(|_, slots| slots.strong_count() > 0);
                    let slots = peers.get(&ip).and_then(Weak::upgrade).unwrap_or_else(|| {
                        let slots = Arc::new(Semaphore::new(config.max_connections_per_ip));
                        peers.insert(ip, Arc::downgrade(&slots));
                        slots
                    });
                    let Ok(permit) = slots.try_acquire_owned() else {
                        if drops.should_log(ip, Instant::now()) {
                            tracing::warn!(reason = "per_peer_cap", peer = %ip, "connection_dropped");
                        }
                        continue;
                    };
                    Some(permit)
                };
                let app = router.clone().layer(Extension(ConnectInfo(addr)));
                let config = config.clone();
                let mut stopping = stop.subscribe();
                connections.spawn(async move {
                    let _permit = permit;
                    let io = IdleConnection {
                        stream,
                        idle: Box::pin(tokio::time::sleep(config.idle_timeout)),
                        idle_timeout: config.idle_timeout,
                        write_stalled: None,
                    };
                    // Match axum's HTTP/1-only feature set and start the header timer before any bytes arrive.
                    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
                    builder.http1().timer(TokioTimer::new()).header_read_timeout(config.header_timeout);
                    let connection = builder.serve_connection(TokioIo::new(io), TowerToHyperService::new(app));
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = &mut connection => {},
                        _ = stopping.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                        }
                    }
                });
            }
        }
    };
    drop(tcp);
    drop(stop);
    if tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    result
}

fn warn_transport_error(last_warning: &mut Option<Instant>, operation: &str, error: &io::Error) {
    let now = Instant::now();
    if last_warning.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60)) {
        // The OS message names the cause (EMFILE has no stable ErrorKind) and carries no peer address.
        tracing::warn!(operation, error = %error, "TCP transport operation failed");
        *last_warning = Some(now);
    }
}

fn configure_socket(stream: &TcpStream) -> io::Result<()> {
    let socket = socket2::SockRef::from(stream);
    socket.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(30))
            .with_interval(Duration::from_secs(10))
            .with_retries(3),
    )?;
    #[cfg(target_os = "linux")]
    socket.set_tcp_user_timeout(Some(WRITE_TIMEOUT))?;
    Ok(())
}

struct IdleConnection {
    stream: TcpStream,
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    write_stalled: Option<Pin<Box<Sleep>>>,
}

impl IdleConnection {
    fn check_deadlines(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.idle.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection idle timeout",
            ))
        } else if self
            .write_stalled
            .as_mut()
            .is_some_and(|deadline| deadline.as_mut().poll(cx).is_ready())
        {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection write timeout",
            ))
        } else {
            Ok(())
        }
    }

    fn activity(&mut self) {
        self.idle.as_mut().reset(Instant::now() + self.idle_timeout);
    }
}

impl AsyncRead for IdleConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_deadlines(cx)?;
        let before = buf.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.activity();
        }
        result
    }
}

impl AsyncWrite for IdleConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check_deadlines(cx)?;
        let result = Pin::new(&mut self.stream).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.write_stalled = None;
            self.activity();
        } else if result.is_pending() {
            // This timer wakes hyper even when backpressure stops response-body polling.
            self.write_stalled
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(WRITE_TIMEOUT)));
            self.check_deadlines(cx)?;
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tracing::instrument::WithSubscriber;

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Logs {
        fn warnings(&self) -> usize {
            let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
            assert!(!text.contains("127.0.0.1"), "peer address leaked: {text}");
            text.matches("WARN").count()
        }
    }

    async fn health(addr: SocketAddr) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut client = TcpStream::connect(addr).await.unwrap();
            client
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.ends_with("ok"), "{response}");
        })
        .await
        .expect("listener stopped serving health requests");
    }

    #[tokio::test]
    async fn socket_tuning_failure_keeps_current_and_later_connections_serving() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        // With one dispatcher registered, tracing takes a callsite's interest from whichever thread
        // hits it first; a parallel test hitting it uncaptured would cache "never". A second, idle
        // dispatcher makes every callsite's interest come from all registered dispatchers.
        let _idle = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let server = tokio::spawn(
            serve_connections(
                listener,
                Router::new().route("/healthz", get(|| async { "ok" })),
                TransportConfig {
                    max_connections: 1,
                    ..Default::default()
                },
                async {
                    stopped.await.unwrap();
                },
                TcpListener::poll_accept,
                |_| Err(io::ErrorKind::InvalidInput.into()),
            )
            .with_subscriber(subscriber),
        );
        health(addr).await;
        health(addr).await;
        assert_eq!(logs.warnings(), 1, "socket warnings must be rate limited");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::time::resume();
        health(addr).await;
        assert_eq!(logs.warnings(), 2, "socket warning did not recover");
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    async fn accepts_after_error(kind: io::ErrorKind) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let mut error = Some(io::Error::from(kind));
        let server = tokio::spawn(serve_connections(
            listener,
            Router::new().route("/healthz", get(|| async { "ok" })),
            TransportConfig::default(),
            async {
                stopped.await.unwrap();
            },
            move |listener, cx| match error.take() {
                Some(error) => Poll::Ready(Err(error)),
                None => listener.poll_accept(cx),
            },
            configure_socket,
        ));
        health(addr).await;
        health(addr).await;
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn aborted_accept_keeps_listener_serving() {
        accepts_after_error(io::ErrorKind::ConnectionAborted).await;
    }

    #[tokio::test]
    async fn reset_accept_keeps_listener_serving() {
        accepts_after_error(io::ErrorKind::ConnectionReset).await;
    }

    #[tokio::test]
    async fn pending_network_error_keeps_listener_serving() {
        for kind in [
            io::ErrorKind::NetworkUnreachable,
            io::ErrorKind::Unsupported,
            io::ErrorKind::Other,
        ] {
            accepts_after_error(kind).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_interrupts_accept_error_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            serve_connections(
                listener,
                Router::new(),
                TransportConfig::default(),
                tokio::time::sleep(Duration::from_millis(100)),
                |_, _| Poll::Ready(Err(io::ErrorKind::OutOfMemory.into())),
                configure_socket,
            ),
        )
        .await
        .expect("accept retries blocked shutdown")
        .unwrap();
    }

    #[tokio::test]
    async fn fatal_listener_error_ends_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let error = serve_connections(
            listener,
            Router::new(),
            TransportConfig::default(),
            std::future::pending(),
            |_, _| Poll::Ready(Err(io::ErrorKind::InvalidInput.into())),
            configure_socket,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn accepted_socket_has_keepalive_and_user_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        configure_socket(&stream).unwrap();
        let socket = socket2::SockRef::from(&stream);
        assert!(socket.keepalive().unwrap());
        assert_eq!(
            socket.tcp_keepalive_time().unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            socket.tcp_keepalive_interval().unwrap(),
            Duration::from_secs(10)
        );
        assert_eq!(socket.tcp_keepalive_retries().unwrap(), 3);
        #[cfg(target_os = "linux")]
        assert_eq!(
            socket.tcp_user_timeout().unwrap(),
            Some(Duration::from_secs(60))
        );
    }
}
