//! S2a: TLS, ALPN and extra roots, without any credential.
//!
//! Three questions, in order: does the client the SDK will build reach the live
//! API over HTTP/2 and get a 403 with no `Authorization` header; does adding
//! the TestServer's self-signed certificate as an extra root reach the
//! TestServer over HTTP/2 while STILL reaching the live API; and does the same
//! TestServer refuse a client that has not added it.
//!
//! No API key is read, set or sent. A 403 is the expected and sufficient
//! answer from the live endpoint.

use std::{error::Error, time::Duration};

use http::{Request, Version};
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use rustls::pki_types::CertificateDer;
use test_support::{Protocol, TestServer};

use crate::tls;

/// The live endpoint, contacted WITHOUT an API key.
const LIVE_MODELS_URL: &str = "https://api.typesafe.ai/v1/models";

pub async fn run() -> Result<(), Box<dyn Error>> {
    println!("# S2a TLS, ALPN and extra roots (macOS only; Linux and Windows remain unproven)");
    println!();

    println!("## (1) live API through the plain platform verifier, no Authorization header");
    probe_live(Vec::new()).await?;

    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        http::Response::new(http_body_util::Full::new(Bytes::from_static(b"{\"models\":[]}")))
    })
    .await?;
    let certificate = server.certificate_der().ok_or("the TLS server has a certificate")?;

    println!();
    println!("## (3) TestServer WITHOUT its certificate as an extra root");
    probe_test_server(&server, Vec::new()).await;

    println!();
    println!("## (2) TestServer WITH its certificate as an extra root");
    probe_test_server(&server, vec![certificate.clone()]).await;

    println!();
    println!(
        "## (2) the SAME extra-roots config against the live API: extra roots must ADD to the OS store"
    );
    probe_live(vec![certificate]).await?;

    Ok(())
}

/// One unauthenticated `GET /v1/models` against the live endpoint.
async fn probe_live(extra_roots: Vec<CertificateDer<'static>>) -> Result<(), Box<dyn Error>> {
    let label = if extra_roots.is_empty() {
        "platform verifier"
    } else {
        "platform verifier + 1 extra root"
    };

    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls::client_config(extra_roots)?)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(connector);

    let request = Request::get(LIVE_MODELS_URL)
        .header("accept", "application/json")
        .header("user-agent", "typesafe-sdk-rust-spike/0.0.0")
        .body(Empty::new())?;

    let response = tokio::time::timeout(Duration::from_secs(20), client.request(request)).await??;
    let status = response.status();
    let version = response.version();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await?.to_bytes();

    println!("config={label}");
    println!("status={} version={version:?} http2={}", status.as_u16(), version == Version::HTTP_2);
    println!("body={}", String::from_utf8_lossy(&body));
    println!("response headers:");
    let mut names: Vec<_> = headers.iter().map(|(name, value)| (name.as_str(), value)).collect();
    names.sort_by_key(|(name, _)| *name);
    for (name, value) in names {
        println!("  {name}: {}", String::from_utf8_lossy(value.as_bytes()));
    }
    Ok(())
}

/// One request against the loopback TLS TestServer.
async fn probe_test_server(server: &TestServer, extra_roots: Vec<CertificateDer<'static>>) {
    let label = if extra_roots.is_empty() {
        "no extra root"
    } else {
        "extra root = the server's certificate"
    };

    let config = match tls::client_config(extra_roots) {
        Ok(config) => config,
        Err(error) => {
            println!("config={label} outcome=could not build the client config: {error}");
            return;
        }
    };
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(connector);

    let request = match Request::get(server.url("/v1/models")).body(Empty::new()) {
        Ok(request) => request,
        Err(error) => {
            println!("config={label} outcome=could not build the request: {error}");
            return;
        }
    };

    let before = server.accepted_connections();
    match tokio::time::timeout(Duration::from_secs(10), client.request(request)).await {
        Ok(Ok(response)) => {
            let version = response.version();
            println!(
                "config={label} outcome=Ok status={} version={version:?} http2={} accepted_connections=+{}",
                response.status().as_u16(),
                version == Version::HTTP_2,
                server.accepted_connections() - before,
            );
        }
        Ok(Err(error)) => {
            // The handshake failure arrives as a connector error, and its
            // source chain is where rustls' reason lives.
            println!("config={label} outcome=Err {error}");
            let mut source: Option<&dyn Error> = error.source();
            while let Some(inner) = source {
                println!("  caused by: {inner}");
                source = inner.source();
            }
            println!("  accepted_connections=+{}", server.accepted_connections() - before);
        }
        Err(_) => println!("config={label} outcome=timed out after 10 s"),
    }
}
