//! Link-level probe over the authorized runtime dependency set.
//!
//! Prints one JSON object describing what each authorized crate reports on
//! this host. Building it proves the pinned versions and feature sets compose;
//! running `cargo deny check` against its lock file reports the transitive
//! license, advisory and duplicate-version situation of the shipping SDK.

#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http_body::Body as _;
use rustls_platform_verifier::BuilderVerifierExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;

/// Error type of this probe, present so that `thiserror` is exercised.
#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("{what} is not usable on this host: {detail}")]
    Unusable { what: &'static str, detail: String },
}

#[derive(Debug, Serialize)]
struct Report {
    uri_authority: String,
    body_size_hint: Option<u64>,
    body_bytes: usize,
    http_versions: [String; 2],
    tls_provider: &'static str,
    tls_cipher_suites: usize,
    tls_alpn_preset: usize,
    tls_alpn_enabled_by_connector: [&'static str; 2],
    connector_is_service: bool,
    runtime_threads: usize,
    http_date: String,
    random_nonce: u64,
    secret_len: usize,
}

fn build_report() -> Result<Report, ProbeError> {
    // `http`: the SDK parses each endpoint once at build time, so the probe
    // parses one the same way rather than formatting a string per call.
    let uri: http::Uri =
        "https://api.typesafe.ai/v1/systemone".parse().map_err(|e: http::uri::InvalidUri| {
            ProbeError::Unusable { what: "http::Uri", detail: e.to_string() }
        })?;
    let uri_authority = uri
        .authority()
        .ok_or_else(|| ProbeError::Unusable {
            what: "http::Uri",
            detail: "absolute URI parsed without an authority".to_owned(),
        })?
        .to_string();

    // `bytes` + `http-body-util`: the shape the request body will take.
    let payload = Bytes::from_static(br#"{"state":"probe","questions":{}}"#);
    let body_bytes = payload.len();
    let body = http_body_util::Full::new(payload);
    let body_size_hint = body.size_hint().exact();

    let http_versions =
        [format!("{:?}", hyper::Version::HTTP_11), format!("{:?}", hyper::Version::HTTP_2)];

    // `rustls` + `rustls-platform-verifier`: the OS trust store decides which
    // roots are anchors, so nothing here is compiled in. Installing the
    // provider explicitly means a later `ring` arriving in the graph cannot
    // make `ClientConfig::builder()` ambiguous.
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let tls_cipher_suites = provider.cipher_suites.len();
    // `install_default` consumes the provider and hands it back inside an `Arc`
    // when one was already installed, which is not an error here.
    let _ = provider.install_default();
    let tls_config = rustls::ClientConfig::builder()
        .with_platform_verifier()
        .map_err(|e| ProbeError::Unusable {
            what: "rustls_platform_verifier",
            detail: e.to_string(),
        })?
        .with_no_client_auth();
    // hyper-rustls sets `alpn_protocols` itself from the `enable_*` calls below
    // and panics ("ALPN protocols should not be pre-defined") if the config
    // already carries any, so the list must stay empty here.
    let tls_alpn_preset = tls_config.alpn_protocols.len();

    // `hyper-rustls` + `hyper-util`: the connector the pooled client drives.
    // `enable_http2()` is what makes ALPN offer h2 at all, and it is reachable
    // only because the manifest turns the non-default `http2` feature on.
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    // A connector is only usable by hyper-util if it is a tower service over
    // `Uri`; asserting the bound here is what pins `tower-service` into the
    // graph.
    let connector_is_service = is_uri_service(&connector);
    let _client: hyper_util::client::legacy::Client<_, http_body_util::Full<Bytes>> =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .pool_timer(hyper_util::rt::TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .build(connector);

    // `tokio`: constructed, not entered; the probe does no I/O.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| ProbeError::Unusable { what: "tokio runtime", detail: e.to_string() })?;
    let runtime_threads = runtime.metrics().num_workers();

    // `httpdate`: the `Retry-After` HTTP-date form.
    let http_date = httpdate::fmt_http_date(SystemTime::UNIX_EPOCH);

    // `fastrand`: retry jitter.
    let random_nonce = fastrand::u64(..);

    // `secrecy`: the API key never leaves a `SecretString` except to build one
    // header value.
    let secret = SecretString::from("probe-key");
    let secret_len = secret.expose_secret().len();

    Ok(Report {
        uri_authority,
        body_size_hint,
        body_bytes,
        http_versions,
        tls_provider: "aws-lc-rs",
        tls_cipher_suites,
        tls_alpn_preset,
        tls_alpn_enabled_by_connector: ["h2", "http/1.1"],
        connector_is_service,
        runtime_threads,
        http_date,
        random_nonce,
        secret_len,
    })
}

/// Reports whether `S` is the tower service shape hyper-util's pooled client
/// requires of a connector.
fn is_uri_service<S>(_service: &S) -> bool
where
    S: tower_service::Service<http::Uri>,
{
    true
}

fn main() -> Result<(), ProbeError> {
    tracing::debug!("probing the authorized runtime dependency set");
    let report = build_report()?;
    let json = sonic_rs::to_string_pretty(&report)
        .map_err(|e| ProbeError::Unusable { what: "sonic_rs", detail: e.to_string() })?;
    println!("{json}");
    Ok(())
}
