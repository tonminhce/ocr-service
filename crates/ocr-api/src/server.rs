//! Explicit HTTP/1 transport bounds complement body accounting. Once Hyper has
//! consumed a body, only copied 16-KiB frames remain in its bounded write queue;
//! connection and stalled-write limits cover that tail and slow TCP readers.
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, Sleep},
};

use crate::config::TransportSettings;

pub async fn serve(
    listener: TcpListener,
    app: Router,
    settings: TransportSettings,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(settings.max_connections));
    let mut tasks = JoinSet::new();
    let (stop, _) = tokio::sync::watch::channel(false);
    tokio::pin!(shutdown);
    loop {
        // Reap all completed handles before admitting a new socket. Connection
        // permits alone would not cap completed tasks retained in the JoinSet.
        while tasks.try_join_next().is_some() {}
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    // At transport capacity no request has been parsed safely;
                    // close immediately instead of allocating another task.
                    drop(stream);
                    continue;
                };
                let _ = stream.set_nodelay(true);
                let app = app.clone();
                let settings = settings.clone();
                let mut stopped = stop.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    let io = TokioIo::new(StallIo::new(stream, settings.write_stall_timeout));
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new())
                        .header_read_timeout(settings.header_timeout)
                        .max_buf_size(settings.write_buffer_bytes)
                        .writev(false)
                        .pipeline_flush(false);
                    let connection = builder.serve_connection(io, TowerToHyperService::new(app));
                    tokio::pin!(connection);
                    let deadline = tokio::time::sleep(settings.connection_lifetime);
                    tokio::pin!(deadline);
                    tokio::select! {
                        result = &mut connection => { if let Err(error) = result { tracing::debug!(%error, "HTTP connection ended"); } },
                        _ = &mut deadline => { tracing::debug!("HTTP connection lifetime exceeded"); },
                        _ = stopped.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = tokio::time::timeout(Duration::from_secs(5), &mut connection).await;
                        },
                    }
                });
            },
        }
    }
    let _ = stop.send(true);
    while tasks.join_next().await.is_some() {}
    Ok(())
}

struct StallIo {
    stream: TcpStream,
    timeout: Duration,
    stalled: Option<Pin<Box<Sleep>>>,
    flush_stalled: Option<Pin<Box<Sleep>>>,
}
impl StallIo {
    fn new(stream: TcpStream, timeout: Duration) -> Self {
        Self {
            stream,
            timeout,
            stalled: None,
            flush_stalled: None,
        }
    }
    fn pending_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let timeout = self.timeout;
        let timer = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(Instant::now() + timeout)));
        if timer.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP write stalled",
            )))
        } else {
            Poll::Pending
        }
    }
}
impl AsyncRead for StallIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for StallIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write(cx, buf) {
            Poll::Pending => self.pending_write(cx).map_ok(|()| 0),
            ready => {
                self.stalled = None;
                ready
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.stream).poll_flush(cx) {
            Poll::Pending => {
                let timeout = self.timeout;
                let timer = self
                    .flush_stalled
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                if timer.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "HTTP flush stalled",
                    )))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(result) => {
                self.flush_stalled = None;
                Poll::Ready(result)
            }
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
