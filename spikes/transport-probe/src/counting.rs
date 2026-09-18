//! A connector wrapper that counts connections the client opens and how many
//! of them are still alive.
//!
//! The TestServer counts connections it ACCEPTED, and that number never goes
//! down, so it cannot answer "how many are still open one second later". The
//! operating system could, but `netstat` produces no output in this sandbox,
//! and a measurement that silently reports zero is worse than none. Counting
//! inside the client is exact and needs nothing outside the process: the
//! counter goes up when the connector yields a stream and down when that
//! stream is dropped.

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::{Connected, Connection};
use tower_service::Service;

/// Shared counters, readable while the client is still alive.
#[derive(Debug, Clone, Default)]
pub struct Counters {
    opened: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}

impl Counters {
    /// Connections this client has opened since it was built.
    #[must_use]
    pub fn opened(&self) -> usize {
        self.opened.load(Ordering::Relaxed)
    }

    /// Connections this client is holding open right now.
    #[must_use]
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }
}

/// Wraps a connector so that every stream it produces is counted.
#[derive(Debug, Clone)]
pub struct CountingConnector<C> {
    inner: C,
    counters: Counters,
}

impl<C> CountingConnector<C> {
    pub fn new(inner: C) -> (Self, Counters) {
        let counters = Counters::default();
        (Self { inner, counters: counters.clone() }, counters)
    }
}

impl<C, Request> Service<Request> for CountingConnector<C>
where
    C: Service<Request>,
    C::Future: Send + 'static,
    C::Error: 'static,
{
    type Response = CountingIo<C::Response>;
    type Error = C::Error;
    // Boxed because the wrapper has to run code after the inner future
    // resolves; a spike does not need a hand-written state machine for that.
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let future = self.inner.call(request);
        let counters = self.counters.clone();
        Box::pin(async move {
            let io = future.await?;
            counters.opened.fetch_add(1, Ordering::Relaxed);
            counters.live.fetch_add(1, Ordering::Relaxed);
            Ok(CountingIo { inner: io, counters })
        })
    }
}

/// A connection whose drop decrements the live counter.
#[derive(Debug)]
pub struct CountingIo<T> {
    inner: T,
    counters: Counters,
}

impl<T> Drop for CountingIo<T> {
    fn drop(&mut self) {
        self.counters.live.fetch_sub(1, Ordering::Relaxed);
    }
}

// Every delegation below uses `Pin::new` on the inner value, which is sound
// without `unsafe` precisely because `T: Unpin` is required: the wrapper never
// has to project a pin through a field.
impl<T: Read + Unpin> Read for CountingIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: Write + Unpin> Write for CountingIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

impl<T: Connection> Connection for CountingIo<T> {
    fn connected(&self) -> Connected {
        self.inner.connected()
    }
}
