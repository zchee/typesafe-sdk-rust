//! The default transport: one pooled HTTP client per SDK client.
//!
//! It is hyper-util's pooled client over hyper-rustls, trusting the operating
//! system's roots through rustls-platform-verifier, plus any roots the caller
//! added. The rustls configuration is built here, once per client, because a
//! verifier with extra roots is something hyper-rustls' own constructors
//! cannot build.
//!
//! Connections are kept for reuse: an idle one is closed after 90 seconds,
//! and an HTTP/2 connection is kept alive by a PING every 30 seconds, idle or
//! not, so that a load balancer does not drop it between calls. Nagle's
//! algorithm is off, because every request and response here is small.

use std::{
    error::Error as StdError,
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use ::hyper::body::Incoming;
use http::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{self, connect::HttpConnector},
    rt::{TokioExecutor, TokioTimer},
};
use rustls::{ClientConfig, pki_types::CertificateDer};
use rustls_platform_verifier::{BuilderVerifierExt as _, Verifier};
use tower_service::Service;

use super::{Body, BoxError};
use crate::{error::Error, text};

/// How long an idle pooled connection is kept before it is closed.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How often an HTTP/2 connection is pinged to keep it open.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Which HTTP versions the default transport speaks.
///
/// The default is [`Http2Only`](HttpVersion::Http2Only) for an `https` base
/// URL and [`Auto`](HttpVersion::Auto) for an `http` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HttpVersion {
    /// HTTP/2 only: negotiated through TLS ALPN on `https`, and spoken with
    /// prior knowledge (h2c) on `http`.
    ///
    /// Every request of a client shares one multiplexed connection, including
    /// requests started together on a client that has no connection yet.
    Http2Only,
    /// HTTP/2 or HTTP/1.1 as the server chooses through ALPN on `https`, and
    /// HTTP/1.1 on `http`.
    ///
    /// Use it behind a proxy that speaks HTTP/1.1 only. A client that has no
    /// connection yet may open one per request started at the same time,
    /// because which version the server speaks is known only once one of them
    /// is open.
    Auto,
}

/// What the default transport is built from.
pub(crate) struct TransportSettings {
    pub(crate) version: HttpVersion,
    /// DER-encoded certificates trusted in addition to the operating system's.
    pub(crate) extra_roots: Vec<Vec<u8>>,
    pub(crate) connect_timeout: Option<Duration>,
}

/// The transport a client uses unless it is given another: a pooled HTTP/1.1
/// and HTTP/2 client over TLS.
///
/// Cloning it shares the connection pool.
#[derive(Clone)]
pub struct HyperTransport {
    client: legacy::Client<HttpsConnector<HttpConnector>, Body>,
    version: HttpVersion,
    extra_roots: usize,
    connect_timeout: Option<Duration>,
}

impl HyperTransport {
    /// Builds the transport and its TLS configuration.
    ///
    /// Needs no runtime: nothing connects until the first request, which must
    /// then run on a Tokio runtime with its time driver enabled.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// the certificate verifier cannot be built: an added root is not a
    /// certificate, or the operating system's roots cannot be loaded.
    pub(crate) fn new(settings: TransportSettings) -> Result<Self, Error> {
        let TransportSettings { version, extra_roots, connect_timeout } = settings;
        let root_count = extra_roots.len();
        let tls = tls_config(extra_roots.into_iter().map(CertificateDer::from).collect())?;

        let mut http = HttpConnector::new();
        // The TLS layer above decides between `http` and `https`; the TCP
        // layer has to accept both.
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(connect_timeout);

        // hyper-rustls fills ALPN from what is enabled here; offering only
        // `h2` is what keeps an HTTP/1.1-only server from being accepted
        // under `Http2Only`.
        let https = HttpsConnectorBuilder::new().with_tls_config(tls).https_or_http();
        let connector = match version {
            HttpVersion::Http2Only => https.enable_http2().wrap_connector(http),
            HttpVersion::Auto => https.enable_http1().enable_http2().wrap_connector(http),
        };

        let mut builder = legacy::Client::builder(TokioExecutor::new());
        // hyper panics on a time-based option that has no timer to run on.
        builder
            .timer(TokioTimer::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
            .http2_keep_alive_while_idle(true)
            .http2_only(version == HttpVersion::Http2Only);

        Ok(Self {
            client: builder.build(connector),
            version,
            extra_roots: root_count,
            connect_timeout,
        })
    }
}

impl fmt::Debug for HyperTransport {
    /// The settings the transport was built with; the roots as a count.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HyperTransport")
            .field("http_version", &self.version)
            .field("extra_roots", &self.extra_roots)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl Service<Request<Body>> for HyperTransport {
    type Response = Response<Incoming>;
    type Error = BoxError;
    type Future = HyperResponseFuture;

    /// Always ready: the pool takes any number of requests.
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> HyperResponseFuture {
        HyperResponseFuture {
            inner: self.client.request(request),
            connect_timeout: self.connect_timeout,
        }
    }
}

/// The response of one request sent by [`HyperTransport`].
#[must_use = "futures do nothing unless polled"]
pub struct HyperResponseFuture {
    inner: legacy::ResponseFuture,
    connect_timeout: Option<Duration>,
}

impl fmt::Debug for HyperResponseFuture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("HyperResponseFuture").finish_non_exhaustive()
    }
}

impl Future for HyperResponseFuture {
    type Output = Result<Response<Incoming>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Both fields are `Unpin`, so the pinned reference can be turned back
        // into a plain one and the inner future pinned in place again.
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(response)) => Poll::Ready(Ok(response)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(failure(error, this.connect_timeout))),
        }
    }
}

/// What a failed request becomes: a timeout when it was the connect timeout
/// that ran out, the client's own error otherwise.
fn failure(error: legacy::Error, connect_timeout: Option<Duration>) -> BoxError {
    match connect_timeout {
        Some(timeout) if error.is_connect() && timed_out(&error) => {
            Box::new(Error::timeout(timeout))
        }
        _ => Box::new(error),
    }
}

/// Whether anything in the chain of `error` is an I/O error that timed out.
fn timed_out(error: &(dyn StdError + 'static)) -> bool {
    let mut link = Some(error);
    while let Some(current) = link {
        if current
            .downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::TimedOut)
        {
            return true;
        }
        link = current.source();
    }
    false
}

/// The rustls configuration: TLS 1.2 and 1.3 with aws-lc-rs, the operating
/// system's roots, and `extra_roots` on top of them.
///
/// The crypto provider is named rather than taken from the process default,
/// so a second provider elsewhere in the program cannot change it. ALPN is
/// left empty: hyper-rustls sets it from the versions the connector enables,
/// and refuses a configuration that already has it.
fn tls_config(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(verifier_error)?;
    let config = if extra_roots.is_empty() {
        builder.with_platform_verifier().map_err(verifier_error)?.with_no_client_auth()
    } else {
        let verifier =
            Verifier::new_with_extra_roots(extra_roots, provider).map_err(verifier_error)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth()
    };
    Ok(config)
}

/// The error for a TLS configuration that cannot be built.
///
/// The verifier's text can quote a certificate the caller added or the
/// platform's own diagnostics, so it is escaped and bounded like any text
/// this SDK did not write.
fn verifier_error(error: rustls::Error) -> Error {
    Error::config(format!(
        "The TLS certificate verifier could not be built: {}.",
        text::bounded(&error, text::MAX_MESSAGE_CHARS)
    ))
}

#[cfg(test)]
#[path = "hyper_tests.rs"]
mod tests;
