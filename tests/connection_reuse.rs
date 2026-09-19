//! How many connections a client opens: one, however many requests it sends
//! and however many of them start together.
//!
//! Every server here is HTTP/2 over TLS with a certificate the client trusts
//! through `add_root_certificate`, and its URL is an IP literal, so no second
//! socket is raced across resolved addresses and the count the server keeps
//! is the client's pool alone. The client uses its default HTTP version,
//! which for an `https` base URL is HTTP/2 only.

use bytes::Bytes;
use http::{Response, StatusCode, Version};
use http_body_util::Full;
use test_support::{Protocol, TestServer};
use typesafe_sdk::{Client, Noul, PreparedQuestions, Questions};

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

async fn tls_server() -> TestServer {
    TestServer::start(Protocol::Http2Tls, |request| async move {
        let body: &'static [u8] =
            if request.method == http::Method::POST { RESULT } else { br#"{"models":[]}"# };
        let mut response = Response::new(Full::new(Bytes::from_static(body)));
        *response.status_mut() = StatusCode::OK;
        response
    })
    .await
    .expect("the test server starts")
}

fn client_for(server: &TestServer) -> Client {
    let certificate = server.certificate_der().expect("a TLS server has a certificate");
    Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .add_root_certificate(certificate.to_vec())
        .build()
        .expect("the client builds")
}

fn questions() -> PreparedQuestions {
    Questions::new().noul("spam", Noul::new().instructions("Spam?")).prepare().expect("prepares")
}

/// Sends `count` System One calls at once, each from a task of its own.
async fn concurrently(client: &Client, questions: &PreparedQuestions, count: usize) {
    let mut tasks = Vec::with_capacity(count);
    for index in 0..count {
        let (client, questions) = (client.clone(), questions.clone());
        tasks.push(tokio::spawn(async move {
            client
                .system_one("hello", &questions)
                .send()
                .await
                .map(|response| response.answers().len())
                .map_err(|error| format!("call {index}: {error}"))
        }));
    }
    for task in tasks {
        let answers = task.await.expect("the task ran").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(answers, 3);
    }
}

/// Every request the server saw arrived over HTTP/2.
fn assert_all_http2(server: &TestServer) {
    let versions: Vec<Version> = server.requests().iter().map(|request| request.version).collect();
    assert!(versions.iter().all(|version| *version == Version::HTTP_2), "{versions:?}");
}

/// AC-P4 (a): 100 calls one after another, then 100 at once, over exactly
/// one connection, all of them HTTP/2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_then_concurrent_calls_share_one_connection() {
    let server = tls_server().await;
    let client = client_for(&server);
    let questions = questions();

    for index in 0..100 {
        client
            .system_one("hello", &questions)
            .send()
            .await
            .unwrap_or_else(|error| panic!("sequential call {index}: {error}"));
    }
    assert_eq!(server.accepted_connections(), 1, "after 100 sequential calls");

    concurrently(&client, &questions, 100).await;
    assert_eq!(server.request_count(), 200);
    assert_eq!(server.accepted_connections(), 1, "after 100 concurrent calls");
    assert_all_http2(&server);
    println!("AC-P4 (a): 200 calls, {} connection(s)", server.accepted_connections());
}

/// AC-P4 (b): 64 calls started together on a client that has no connection
/// yet open exactly one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cold_fan_out_opens_one_connection() {
    let server = tls_server().await;
    let client = client_for(&server);

    concurrently(&client, &questions(), 64).await;
    assert_eq!(server.request_count(), 64);
    assert_eq!(server.accepted_connections(), 1);
    assert_all_http2(&server);
    println!(
        "AC-P4 (b): 64 cold concurrent calls, {} connection(s)",
        server.accepted_connections()
    );
}

/// AC-P4 (c): after `warm_up`, 64 calls started together open no new
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_warm_up_a_fan_out_opens_no_new_connection() {
    let server = tls_server().await;
    let client = client_for(&server);

    client.warm_up().await.expect("the warm-up succeeds");
    let after_warm_up = server.accepted_connections();
    assert_eq!(after_warm_up, 1);

    concurrently(&client, &questions(), 64).await;
    assert_eq!(server.request_count(), 65);
    assert_eq!(server.accepted_connections() - after_warm_up, 0);
    assert_all_http2(&server);
    println!(
        "AC-P4 (c): 64 concurrent calls after warm_up, {} new connection(s)",
        server.accepted_connections() - after_warm_up
    );
}
