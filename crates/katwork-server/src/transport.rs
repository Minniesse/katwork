use std::{
    convert::Infallible,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    Extension, Router,
    extract::ConnectInfo,
    serve::{IncomingStream, Listener},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub async fn serve(tcp: TcpListener, router: Router) -> io::Result<()> {
    let listener = LimitedListener {
        tcp,
        slots: Arc::new(Semaphore::new(256)),
    };
    axum::serve(
        listener,
        tower::service_fn(move |stream: IncomingStream<'_, LimitedListener>| {
            let app = router
                .clone()
                .layer(Extension(ConnectInfo(*stream.remote_addr())));
            async move { Ok::<_, Infallible>(app) }
        }),
    )
    .await
}

struct LimitedListener {
    tcp: TcpListener,
    slots: Arc<Semaphore>,
}

impl Listener for LimitedListener {
    type Io = LimitedConnection;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("connection semaphore stays open");
        let (stream, addr) = Listener::accept(&mut self.tcp).await;
        (
            LimitedConnection {
                stream,
                _permit: permit,
            },
            addr,
        )
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}

struct LimitedConnection {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl AsyncRead for LimitedConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for LimitedConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
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
    use axum::serve::Listener;

    #[tokio::test]
    async fn open_connections_are_bounded_and_capacity_returns_on_close() {
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let mut listener = LimitedListener {
            tcp,
            slots: Arc::new(Semaphore::new(1)),
        };
        let _first = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (first, _) = listener.accept().await;
        let _second = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), listener.accept())
            .await
            .unwrap();
    }
}
