//! A real HTTP server on loopback for the tests of this workspace.
//!
//! Nothing here is simulated: [`TestServer`] binds a TCP socket on
//! `127.0.0.1:0`, serves it with hyper, and hands out a base URL that is
//! always an **IP literal**. That matters for connection-count assertions,
//! because a hostname would let the connector race a second socket across the
//! resolved addresses.
//!
//! Three wire protocols are available, selected by [`Protocol`]: HTTP/1.1, h2c
//! (HTTP/2 over cleartext, prior knowledge, no upgrade dance) and HTTP/2 over
//! TLS with ALPN `h2` and a freshly generated self-signed certificate.
//!
//! The server records every request it serves and counts every TCP connection
//! it accepts, so a test can assert both what was sent and how many
//! connections carried it. The caller supplies the response as an async
//! closure, which may capture state (to answer a retry sequence differently on
//! each attempt) and may await (to hold a response back past a deadline).
//!
//! The listener is released when the [`TestServer`] is dropped.

#![forbid(unsafe_code)]

mod tls;

use std::{
    convert::Infallible,
    fmt,
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri, Version,
    header::CONTENT_TYPE,
};
use http_body_util::{BodyExt as _, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::CertificateDer;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

/// The response type a handler returns.
pub type TestResponse = Response<Full<Bytes>>;

/// A response with `status`, `body` and `content-type: application/json`.
#[must_use]
pub fn json_response(status: StatusCode, body: impl Into<Bytes>) -> TestResponse {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// A handler future, boxed so that handlers of different concrete types can be
/// stored behind one pointer.
type HandlerFuture = Pin<Box<dyn Future<Output = TestResponse> + Send>>;

/// The stored form of a caller-supplied handler.
type BoxedHandler = Arc<dyn Fn(RecordedRequest) -> HandlerFuture + Send + Sync>;

/// Wire protocol a [`TestServer`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Cleartext HTTP/1.1.
    Http1,
    /// Cleartext HTTP/2 with prior knowledge (h2c): the client sends the HTTP/2
    /// preface immediately, with no `Upgrade` negotiation.
    H2c,
    /// HTTP/2 over TLS, negotiated through ALPN. The certificate is generated
    /// per server and is available from [`TestServer::certificate_der`].
    Http2Tls,
}

impl Protocol {
    /// Every protocol, for a test that runs over each of them.
    pub const ALL: [Self; 3] = [Self::Http1, Self::H2c, Self::Http2Tls];
}

/// Everything the server observed about one request it served.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Request method.
    pub method: Method,
    /// Request target as the server received it.
    pub uri: Uri,
    /// Request headers, in the order hyper decoded them.
    pub headers: HeaderMap,
    /// The fully collected request body.
    pub body: Bytes,
    /// The HTTP version the request arrived on.
    pub version: Version,
}

impl RecordedRequest {
    /// Every value of the header `name`, as text, in the order received.
    ///
    /// # Panics
    ///
    /// Panics if a value is not visible ASCII, which no header a test reads
    /// carries.
    #[must_use]
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().expect("a header value the test reads is text"))
            .collect()
    }
}

/// Why a [`TestServer`] could not be started.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The loopback listener could not be bound.
    #[error("could not bind a loopback listener: {0}")]
    Bind(#[source] std::io::Error),
    /// The port the operating system chose could not be read back.
    #[error("could not read back the bound address: {0}")]
    LocalAddr(#[source] std::io::Error),
    /// The self-signed test certificate could not be generated.
    #[error("could not generate the test certificate: {0}")]
    Certificate(#[source] rcgen::Error),
    /// rustls rejected the generated certificate or the requested versions.
    #[error("could not build the server TLS configuration: {0}")]
    TlsConfig(#[source] rustls::Error),
}

/// Shared between the accept loop, every connection task and the handle the
/// test holds.
struct State {
    handler: BoxedHandler,
    requests: Mutex<Vec<RecordedRequest>>,
    accepted_connections: AtomicU64,
}

/// A running HTTP server on `127.0.0.1`.
///
/// Dropping the value stops accepting, drops the live connections and releases
/// the port.
pub struct TestServer {
    addr: SocketAddr,
    base_url: String,
    certificate: Option<CertificateDer<'static>>,
    state: Arc<State>,
    shutdown: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl fmt::Debug for TestServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestServer")
            .field("base_url", &self.base_url)
            .field("requests", &self.request_count())
            .field("accepted_connections", &self.state.accepted_connections.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl TestServer {
    /// Binds a listener on `127.0.0.1:0`, starts serving `protocol` and returns
    /// once the port is known.
    ///
    /// `handler` is invoked once per request, after the body has been collected
    /// and the request recorded. It may capture state and may await, which is
    /// how a test drives a retry sequence or holds a response past a deadline.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot
    /// be set up, and [`Error::Certificate`] or [`Error::TlsConfig`] when
    /// [`Protocol::Http2Tls`] is requested and TLS cannot be configured.
    pub async fn start<F, Fut>(protocol: Protocol, handler: F) -> Result<Self, Error>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResponse> + Send + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::Bind)?;
        let addr = listener.local_addr().map_err(Error::LocalAddr)?;

        let (acceptor, certificate) = match protocol {
            Protocol::Http1 | Protocol::H2c => (None, None),
            Protocol::Http2Tls => {
                let (acceptor, certificate) = tls::self_signed_acceptor()?;
                (Some(acceptor), Some(certificate))
            }
        };

        let scheme = if certificate.is_some() { "https" } else { "http" };
        let state = Arc::new(State {
            handler: Arc::new(move |request| Box::pin(handler(request)) as HandlerFuture),
            requests: Mutex::new(Vec::new()),
            accepted_connections: AtomicU64::new(0),
        });

        let (shutdown, shutdown_rx) = watch::channel(false);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            protocol,
            acceptor,
            Arc::clone(&state),
            shutdown_rx,
        ));

        Ok(Self {
            addr,
            base_url: format!("{scheme}://{addr}"),
            certificate,
            state,
            shutdown,
            accept_task,
        })
    }

    /// Starts serving `protocol` as [`start`](Self::start) does, answering the
    /// `n`th request, counted from 1, with `answer(n, request)`.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub async fn start_nth<F>(protocol: Protocol, answer: F) -> Result<Self, Error>
    where
        F: Fn(usize, &RecordedRequest) -> TestResponse + Send + Sync + 'static,
    {
        let served = AtomicUsize::new(0);
        Self::start(protocol, move |request| {
            let response = answer(served.fetch_add(1, Ordering::SeqCst) + 1, &request);
            async move { response }
        })
        .await
    }

    /// The address the server is listening on, always an IPv4 loopback address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Base URL with no trailing slash, for example `http://127.0.0.1:52341`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Absolute URL for `path`, which may be given with or without its leading
    /// slash.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        let path = path.strip_prefix('/').unwrap_or(path);
        format!("{}/{path}", self.base_url)
    }

    /// Every request served so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.request_log().clone()
    }

    /// How many requests have been served so far.
    #[must_use]
    pub fn request_count(&self) -> usize {
        self.request_log().len()
    }

    /// How many TCP connections have been accepted so far.
    ///
    /// Counted at accept time, so a connection that fails its TLS handshake is
    /// still counted.
    #[must_use]
    pub fn accepted_connections(&self) -> u64 {
        self.state.accepted_connections.load(Ordering::Relaxed)
    }

    /// The server's certificate in DER, for [`Protocol::Http2Tls`] only.
    ///
    /// A client that adds this to its root store will accept the handshake; a
    /// client that does not will reject it.
    #[must_use]
    pub fn certificate_der(&self) -> Option<CertificateDer<'static>> {
        self.certificate.clone()
    }

    /// A poisoned request log still holds every request recorded before the
    /// panic, and losing that is worse for a failing test than reading past the
    /// poison flag.
    fn request_log(&self) -> std::sync::MutexGuard<'_, Vec<RecordedRequest>> {
        self.state.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // The watch value is level-triggered, so a connection task that has not
        // reached its await point yet still observes the shutdown.
        let _ = self.shutdown.send(true);
        self.accept_task.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    protocol: Protocol,
    acceptor: Option<TlsAcceptor>,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let stream = tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                // A failed accept says nothing about the next one, and a test
                // that cares will fail on its own assertions.
                Err(_) => continue,
            },
        };
        state.accepted_connections.fetch_add(1, Ordering::Relaxed);
        // Nagle's algorithm would add latency to the small request/response
        // pairs these tests measure.
        let _ = stream.set_nodelay(true);

        tokio::spawn(serve_connection(
            stream,
            protocol,
            acceptor.clone(),
            Arc::clone(&state),
            shutdown.clone(),
        ));
    }
}

async fn serve_connection(
    stream: TcpStream,
    protocol: Protocol,
    acceptor: Option<TlsAcceptor>,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    let service = service_fn(move |request| dispatch(Arc::clone(&state), request));

    match (protocol, acceptor) {
        (Protocol::Http1, _) => {
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                _ = connection => {}
                _ = shutdown.changed() => {}
            }
        }
        (Protocol::H2c, _) => {
            let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                _ = connection => {}
                _ = shutdown.changed() => {}
            }
        }
        (Protocol::Http2Tls, Some(acceptor)) => {
            // A rejected handshake is the expected outcome of the
            // untrusted-client test, so it ends the connection quietly.
            let Ok(stream) = acceptor.accept(stream).await else {
                return;
            };
            let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                _ = connection => {}
                _ = shutdown.changed() => {}
            }
        }
        (Protocol::Http2Tls, None) => {}
    }
}

async fn dispatch(
    state: Arc<State>,
    request: Request<Incoming>,
) -> Result<TestResponse, Infallible> {
    let (parts, body) = request.into_parts();
    let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            // Reporting the failure as a response body is more useful to a
            // failing test than an empty recorded request would be.
            let mut response = Response::new(Full::new(Bytes::from(format!(
                "test server could not read the request body: {error}"
            ))));
            *response.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(response);
        }
    };

    let recorded = RecordedRequest {
        method: parts.method,
        uri: parts.uri,
        headers: parts.headers,
        body,
        version: parts.version,
    };
    state.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(recorded.clone());

    Ok((state.handler)(recorded).await)
}

/// Binds a loopback listener that answers every connection with `reply`, as
/// raw bytes, once the request has arrived, then closes it: a server that
/// does not speak HTTP, or speaks it wrongly.
///
/// # Errors
///
/// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot be
/// set up.
pub async fn raw_server(reply: impl AsRef<[u8]>) -> Result<SocketAddr, Error> {
    let reply = Bytes::copy_from_slice(reply.as_ref());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::Bind)?;
    let addr = listener.local_addr().map_err(Error::LocalAddr)?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0; 4096];
            // How much of the request one read returns does not matter to
            // the client.
            let _ = stream.read(&mut buffer).await;
            let _ = stream.write_all(&reply).await;
            let _ = stream.shutdown().await;
        }
    });
    Ok(addr)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
