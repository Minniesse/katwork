use std::{
    collections::HashMap,
    future::Future,
    io,
    net::IpAddr,
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
    let mut connections = JoinSet::new();
    let mut peers: HashMap<IpAddr, Weak<Semaphore>> = HashMap::new();
    let mut drops = LogThrottle::default();
    let (stop, _) = watch::channel(());
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = tcp.accept(), if connections.len() < config.max_connections => {
                let (stream, addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) if matches!(error.kind(), io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted) => continue,
                    Err(error) => break Err(error),
                };
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
    while connections.join_next().await.is_some() {}
    result
}

struct IdleConnection {
    stream: TcpStream,
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
}

impl IdleConnection {
    fn check_idle(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.idle.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection idle timeout",
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
        self.check_idle(cx)?;
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
        self.check_idle(cx)?;
        let result = Pin::new(&mut self.stream).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity();
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
