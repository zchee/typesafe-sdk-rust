use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use hyper_util::client::legacy::Client;

use super::*;

/// Builds a plain hyper-util pooled client over cleartext TCP. `http2_only`
/// selects h2c with prior knowledge, which is what makes a cold fan-out share
/// one connection: hyper-util only takes its single-connection lock when it
/// already knows the version is HTTP/2.
fn cleartext_client(
    http2_only: bool,
) -> Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>> {
    let mut builder = Client::builder(TokioExecutor::new());
    builder.http2_only(http2_only);
    builder.build_http()
}

/// Builds a pooled client that speaks HTTP/2 over TLS and trusts exactly the
/// certificates in `roots`. hyper-rustls fills in `alpn_protocols` from
/// `enable_http2`, and panics if the config already lists any, so the config
/// handed to it must leave that field empty.
fn tls_client(
    roots: rustls::RootCertStore,
) -> Result<
    Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
    rustls::Error,
> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_only()
        .enable_http2()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

fn text_response(status: StatusCode, body: &'static str) -> TestResponse {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    response
}

async fn read_body(response: Response<Incoming>) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("a response served by the test server has a readable body")
        .to_bytes()
}

#[tokio::test]
async fn http1_round_trip_is_recorded() {
    let server = TestServer::start(Protocol::Http1, |request| async move {
        assert_eq!(request.body, Bytes::from_static(br#"{"ping":true}"#));
        text_response(StatusCode::OK, r#"{"pong":true}"#)
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let request = Request::post(server.url("/v1/systemone"))
        .header("content-type", "application/json")
        .header("x-team", "billing")
        .body(Full::new(Bytes::from_static(br#"{"ping":true}"#)))
        .expect("the request parts are valid");
    let response = cleartext_client(false)
        .request(request)
        .await
        .expect("the HTTP/1.1 request reaches the test server");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(read_body(response).await, Bytes::from_static(br#"{"pong":true}"#));

    let recorded = server.requests();
    assert_eq!(recorded.len(), 1, "exactly one request was served");
    assert_eq!(server.request_count(), 1);
    let first = &recorded[0];
    assert_eq!(first.method, Method::POST);
    assert_eq!(first.uri.path(), "/v1/systemone");
    assert_eq!(first.version, Version::HTTP_11);
    assert_eq!(first.body, Bytes::from_static(br#"{"ping":true}"#));
    assert_eq!(
        first.headers.get("x-team").map(http::HeaderValue::as_bytes),
        Some(b"billing".as_slice()),
        "caller headers reach the recorder unchanged",
    );
    assert_eq!(server.accepted_connections(), 1);
}

#[tokio::test]
async fn h2c_multiplexes_fifty_requests_over_one_connection() {
    const REQUESTS: usize = 50;

    let server =
        TestServer::start(Protocol::H2c, |_request| async { text_response(StatusCode::OK, "ok") })
            .await
            .expect("an h2c server binds on loopback");

    // One client, cold: nothing has connected yet when all 50 calls start.
    let client = cleartext_client(true);
    let mut calls = Vec::with_capacity(REQUESTS);
    for index in 0..REQUESTS {
        let client = client.clone();
        let url = server.url(&format!("/v1/models?call={index}"));
        calls.push(tokio::spawn(async move {
            let request = Request::get(url)
                .body(Full::new(Bytes::new()))
                .expect("the request parts are valid");
            client.request(request).await
        }));
    }

    for call in calls {
        let response = call
            .await
            .expect("no request task panicked")
            .expect("every h2c request reaches the test server");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), Version::HTTP_2);
    }

    assert_eq!(server.request_count(), REQUESTS);
    assert_eq!(
        server.accepted_connections(),
        1,
        "50 concurrent h2c requests are multiplexed over a single connection",
    );
}

#[tokio::test]
async fn tls_client_that_trusts_the_certificate_negotiates_http2() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");

    let certificate = server.certificate_der().expect("a TLS server exposes its certificate");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).expect("the generated certificate is a usable trust anchor");

    let request = Request::get(server.url("/v1/models"))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    let response = tls_client(roots)
        .expect("the client TLS configuration is valid")
        .request(request)
        .await
        .expect("a client trusting the server certificate completes the handshake");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.version(),
        Version::HTTP_2,
        "ALPN selected h2 rather than falling back to HTTP/1.1",
    );
    assert_eq!(server.request_count(), 1);
    assert_eq!(server.accepted_connections(), 1);
}

#[tokio::test]
async fn tls_client_without_the_certificate_fails_the_handshake() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");

    let request = Request::get(server.url("/v1/models"))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    // An empty root store trusts nothing, so the self-signed certificate has no
    // path to an anchor.
    let error = tls_client(rustls::RootCertStore::empty())
        .expect("the client TLS configuration is valid")
        .request(request)
        .await
        .expect_err("a client trusting nothing must not complete the handshake");

    assert!(
        error.is_connect(),
        "the failure is a connect-time TLS failure, not an HTTP response: {error}",
    );
    assert_eq!(server.request_count(), 0, "a rejected handshake never reaches the handler",);
    assert_eq!(
        server.accepted_connections(),
        1,
        "the TCP connection is still counted, because accept precedes the handshake",
    );
}

#[tokio::test]
async fn handler_can_be_stateful_and_can_delay() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&attempts);

    let server = TestServer::start(Protocol::Http1, move |_request| {
        let attempts = Arc::clone(&attempts);
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                // Holding the first answer back is how a per-attempt deadline
                // is exercised without any clock injection.
                tokio::time::sleep(Duration::from_millis(50)).await;
                text_response(StatusCode::SERVICE_UNAVAILABLE, "retry")
            } else {
                text_response(StatusCode::OK, "ok")
            }
        }
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let client = cleartext_client(false);
    let mut statuses = Vec::new();
    for _ in 0..2 {
        let request = Request::get(server.url("/v1/models"))
            .body(Full::new(Bytes::new()))
            .expect("the request parts are valid");
        let response = client.request(request).await.expect("the request reaches the test server");
        statuses.push(response.status());
    }

    assert_eq!(
        statuses,
        vec![StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK],
        "the handler answered the two attempts differently",
    );
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn dropping_the_server_releases_the_port() {
    let server = TestServer::start(Protocol::Http1, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");
    let addr = server.addr();
    drop(server);

    // Rebinding the exact port proves the listener is gone rather than merely
    // idle. A short retry covers the scheduler not having run the abort yet.
    let mut rebound = None;
    for _ in 0..50 {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                rebound = Some(listener);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    assert!(rebound.is_some(), "the port {addr} was still held after the server was dropped",);
}

#[tokio::test]
async fn every_protocol_starts_a_server_of_its_scheme() {
    for protocol in Protocol::ALL {
        let server =
            TestServer::start(protocol, |_request| async { text_response(StatusCode::OK, "ok") })
                .await
                .expect("a server binds on loopback");
        let tls = protocol == Protocol::Http2Tls;
        let scheme = if tls { "https://" } else { "http://" };
        assert!(server.base_url().starts_with(scheme), "{protocol:?}: {server:?}");
        assert_eq!(server.certificate_der().is_some(), tls, "{protocol:?}");
    }
}

#[tokio::test]
async fn a_counted_handler_is_told_which_request_it_answers() {
    let server = TestServer::start_nth(Protocol::Http1, |n, request| {
        json_response(StatusCode::OK, format!("{n} {:?}", request.header_values("x-team")))
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let client = cleartext_client(false);
    for expected in [r#"1 ["a", "b"]"#, r#"2 ["a", "b"]"#] {
        let request = Request::get(format!("{}/v1/models", server.base_url()))
            .header("x-team", "a")
            .header("x-team", "b")
            .body(Full::new(Bytes::new()))
            .expect("the request parts are valid");
        let response = client.request(request).await.expect("the request reaches the test server");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).map(HeaderValue::as_bytes),
            Some(b"application/json".as_slice()),
        );
        assert_eq!(read_body(response).await, expected.as_bytes());
    }
    assert_eq!(server.requests()[1].header_values("x-team"), ["a", "b"]);
    assert_eq!(server.requests()[1].header_values("x-absent"), Vec::<&str>::new());
}

#[tokio::test]
async fn a_raw_server_answers_every_connection_with_its_bytes() {
    let reply = b"SSH-2.0-OpenSSH_9.9\r\n\r\n";
    let addr = raw_server(reply).await.expect("a raw server binds on loopback");
    assert!(addr.ip().is_loopback(), "{addr}");
    for connection in 1..=2 {
        let mut stream = TcpStream::connect(addr).await.expect("the raw server accepts");
        stream.write_all(b"GET / HTTP/1.1\r\n\r\n").await.expect("the request is written");
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.expect("the raw server closes after its reply");
        assert_eq!(answer, reply, "connection {connection}");
    }
}
