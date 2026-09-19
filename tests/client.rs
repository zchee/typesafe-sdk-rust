//! The client against a real loopback server: ports of the upstream
//! `tests/test_clients.py`, case by case, over HTTP/1.1, h2c and HTTP/2 over
//! TLS where the protocol can matter.
//!
//! Every upstream test is named in the doc comment of the Rust test that
//! ports it. A case the plan's deviation table (section 6) lists is tested for
//! the Rust behaviour, and the comment names the row.

use std::{
    error::Error as StdError,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{Request, Response, StatusCode, Uri, header::HOST};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper_util::{
    client::legacy::{self, connect::HttpConnector},
    rt::TokioExecutor,
};
use serde_json::json;
use test_support::{Protocol, RecordedRequest, TestResponse, TestServer};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::Notify,
};
use tower_service::Service;
use typesafe_sdk::{
    ApiError, ApiErrorKind, Body, Choice, Client, ClientBuilder, Content, Error, ErrorKind,
    HttpVersion, Noul, PreparedQuestions, Questions, RawQuestion, Score,
};

// ------------------------------------------------------------- fixtures

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

const PROTOCOLS: [Protocol; 3] = [Protocol::Http1, Protocol::H2c, Protocol::Http2Tls];

/// A JSON response with `status`.
fn json_response(status: StatusCode, body: impl Into<Bytes>) -> TestResponse {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response.headers_mut().insert("content-type", "application/json".parse().expect("valid"));
    response
}

/// A server answering every request with `200` and `body`.
async fn answering(protocol: Protocol, body: impl AsRef<[u8]>) -> TestServer {
    let body = Bytes::copy_from_slice(body.as_ref());
    TestServer::start(protocol, move |_| {
        let body = body.clone();
        async move { json_response(StatusCode::OK, body) }
    })
    .await
    .expect("the test server starts")
}

/// A builder for a client of `server`: trusting its certificate when it has
/// one, and speaking prior-knowledge HTTP/2 to an h2c server.
fn builder_for(server: &TestServer, protocol: Protocol) -> ClientBuilder {
    let mut builder = Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_model("jev-latest");
    if let Some(certificate) = server.certificate_der() {
        builder = builder.add_root_certificate(certificate.to_vec());
    }
    if protocol == Protocol::H2c {
        builder = builder.http_version(HttpVersion::Http2Only);
    }
    builder
}

fn client_for(server: &TestServer, protocol: Protocol) -> Client {
    builder_for(server, protocol).build().expect("the client builds")
}

/// One question, raw, as most upstream cases ask.
fn one_raw_question() -> PreparedQuestions {
    Questions::new()
        .raw("q", RawQuestion::new("noul").field("instructions", "?"))
        .prepare()
        .expect("the question prepares")
}

fn body_text(request: &RecordedRequest) -> &str {
    std::str::from_utf8(&request.body).expect("the SDK sends UTF-8")
}

/// Every value of `name` a request carried.
fn header<'a>(request: &'a RecordedRequest, name: &str) -> Vec<&'a str> {
    request
        .headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("a header value the test reads is text"))
        .collect()
}

/// The API error `error` must be.
fn api_error(error: &Error) -> &ApiError {
    match error.kind() {
        ErrorKind::Api(api) => api,
        other => panic!("expected an API error, got {other:?}: {error}"),
    }
}

// --------------------------------------------------------- round trip

/// Upstream `test_round_trip[dataclass|raw|mixed]`: the body, the endpoint,
/// the content type, and every answer of the fixture - over each protocol.
#[tokio::test]
async fn round_trip_sends_the_body_and_decodes_every_answer_kind() {
    let raw = Questions::new()
        .raw("spam", RawQuestion::new("noul").field("instructions", "Spam?"))
        .raw(
            "tone",
            RawQuestion::new("choice")
                .field("instructions", "Tone?")
                .field("criteria", json!({"friendly": null, "hostile": null})),
        )
        .raw(
            "quality",
            RawQuestion::new("score")
                .field("instructions", "Quality?")
                .field("criteria", ["bad", "ok", "great"]),
        );
    let typed = Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"));
    let mixed = Questions::new()
        .raw("spam", RawQuestion::new("noul").field("instructions", "Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"));
    let forms = [
        ("dataclass", typed.prepare().expect("prepares")),
        ("raw", raw.prepare().expect("prepares")),
        ("mixed", mixed.prepare().expect("prepares")),
    ];
    let expected_body = concat!(
        r#"{"state":{"document":"Hello "#,
        "\u{1f30d}",
        r#""},"model":"jev-latest","questions":{"spam":{"type":"noul","instructions":"Spam?"},"#,
        r#""tone":{"type":"choice","instructions":"Tone?","criteria":{"friendly":null,"hostile":null}},"#,
        r#""quality":{"type":"score","instructions":"Quality?","criteria":["bad","ok","great"]}}}"#,
    );
    let state = json!({"document": "Hello \u{1f30d}"});

    for protocol in PROTOCOLS {
        for (form, questions) in &forms {
            let server = answering(protocol, RESULT).await;
            let result = client_for(&server, protocol)
                .system_one(&state, questions)
                .send()
                .await
                .unwrap_or_else(|error| panic!("{protocol:?} {form}: {error}"));

            let [request] = &server.requests()[..] else {
                panic!("{protocol:?} {form}: exactly one request expected")
            };
            assert_eq!(request.method, "POST");
            assert_eq!(request.uri.path(), "/v1/systemone", "{protocol:?} {form}");
            assert_eq!(body_text(request), expected_body, "{protocol:?} {form}");
            assert_eq!(header(request, "content-type"), ["application/json"]);

            assert_eq!(result.model(), "jev-latest");
            assert_eq!(result.usage().input_tokens(), Some(12));
            assert_eq!(result.usage().output_tokens(), Some(3));
            let answers = result.answers();
            assert_eq!(answers.names().collect::<Vec<_>>(), ["spam", "tone", "quality"]);
            assert_eq!(answers.noul("spam").map(|answer| answer.noul()), Some(0.98));
            let tone = answers.choice("tone").expect("a choice answer");
            assert_eq!(tone.choice(), "friendly");
            assert_eq!(tone.confidence(), 0.9);
            assert_eq!(
                tone.probabilities().collect::<Vec<_>>(),
                [("friendly", 0.9), ("hostile", 0.1)]
            );
            let quality = answers.score("quality").expect("a score answer");
            assert_eq!(quality.score(), 1.7);
            assert_eq!(quality.confidence(), 0.8);
            let legend: Vec<_> =
                quality.legend().map(|(level, text)| (level, text.as_text())).collect();
            assert_eq!(legend, [(0, Some("bad")), (1, Some("ok")), (2, Some("great"))]);
            assert_eq!(quality.probabilities().collect::<Vec<_>>(), [(0, 0.1), (1, 0.1), (2, 0.8)]);
            assert_eq!(result.meta().status(), StatusCode::OK);
            assert_eq!(&result.meta().raw_body()[..], RESULT);
        }
    }
}

/// Upstream `test_extra_body_shallow_override`: last write wins, `model` is
/// replaced where it stands, `null` is sent. Member order is the wire order.
#[tokio::test]
async fn extra_body_shallow_override() {
    let server = answering(Protocol::Http1, RESULT).await;
    let questions = one_raw_question();
    client_for(&server, Protocol::Http1)
        .system_one("hi", &questions)
        .model("call-model")
        .extra_body("model", "override-model")
        .extra_body("beam_width", &4)
        .extra_body("nullable", &None::<u8>)
        .send()
        .await
        .expect("the call succeeds");

    assert_eq!(
        body_text(&server.requests()[0]),
        r#"{"state":"hi","model":"override-model","questions":{"q":{"type":"noul","instructions":"?"}},"beam_width":4,"nullable":null}"#
    );
}

/// A value whose `Serialize` fails, as upstream's `object()` does.
struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("an object() the encoder cannot write"))
    }
}

/// Upstream `test_unserializable_request_body_raises`.
#[tokio::test]
async fn unserializable_request_body_raises_before_the_network() {
    let server = answering(Protocol::Http1, RESULT).await;
    let questions = one_raw_question();
    let error = client_for(&server, Protocol::Http1)
        .system_one("x", &questions)
        .extra_body("bad", &Unserializable)
        .send()
        .await
        .expect_err("the body cannot be encoded");

    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    let prefix = r#"The request body could not be encoded as JSON: the extra member "bad": "#;
    let rendered = error.to_string();
    let reason = rendered.strip_prefix(prefix).unwrap_or_else(|| panic!("{rendered}"));
    assert!(reason.contains("an object() the encoder cannot write"), "{rendered}");
    assert_eq!(server.request_count(), 0, "an unencodable body reached the network");
}

/// Upstream `test_raw_question_passthrough`: fields the SDK does not model
/// travel as written.
#[tokio::test]
async fn raw_question_passthrough() {
    let server = answering(Protocol::Http1, RESULT).await;
    let questions = Questions::new()
        .raw(
            "q",
            RawQuestion::new("noul")
                .field("instructions", "Spam?")
                .field("weight", 3)
                .field("nested", json!({"k": null})),
        )
        .raw(
            "choice",
            RawQuestion::new("choice").field("criteria", json!({"a": null})).field("weight", 2),
        )
        .raw("score", RawQuestion::new("score").field("criteria", ["good"]).field("weight", 1))
        .prepare()
        .expect("the questions prepare");
    client_for(&server, Protocol::Http1)
        .system_one("hi", &questions)
        .send()
        .await
        .expect("the call succeeds");

    assert_eq!(
        body_text(&server.requests()[0]),
        concat!(
            r#"{"state":"hi","model":"jev-latest","questions":{"#,
            r#""q":{"type":"noul","instructions":"Spam?","weight":3,"nested":{"k":null}},"#,
            r#""choice":{"type":"choice","criteria":{"a":null},"weight":2},"#,
            r#""score":{"type":"score","criteria":["good"],"weight":1}}}"#,
        )
    );
}

/// Upstream `test_question_schema_validation_is_left_to_api`: a raw question
/// the API will refuse is sent as written, and the 422 comes back mapped.
#[tokio::test]
async fn question_schema_validation_is_left_to_api() {
    let server = TestServer::start(Protocol::Http1, |_| async {
        json_response(StatusCode::UNPROCESSABLE_ENTITY, r#"{"detail":"Invalid question"}"#)
    })
    .await
    .expect("the test server starts");
    let rows = [
        (RawQuestion::new("noul").field("instructions", 1), r#"{"type":"noul","instructions":1}"#),
        (
            RawQuestion::new("choice").field("criteria", ["invalid", "shape"]),
            r#"{"type":"choice","criteria":["invalid","shape"]}"#,
        ),
    ];
    for (index, (question, sent)) in rows.into_iter().enumerate() {
        let questions = Questions::new().raw("q", question).prepare().expect("it prepares");
        let error = client_for(&server, Protocol::Http1)
            .system_one("x", &questions)
            .send()
            .await
            .expect_err("the server refuses it");

        assert_eq!(api_error(&error).kind(), ApiErrorKind::UnprocessableEntity);
        assert_eq!(
            error.to_string(),
            format!("POST {}/v1/systemone: 422 Invalid question", server.base_url())
        );
        let body = body_text(&server.requests()[index]).to_owned();
        assert!(body.ends_with(&format!(r#""questions":{{"q":{sent}}}}}"#)), "{body}");
    }
}

/// Upstream `test_rich_descriptions`: structured content in the questions and
/// in the answer's legend.
#[tokio::test]
async fn rich_descriptions() {
    const ANSWER: &[u8] = br#"{"model":"custom","usage":{"input_tokens":1,"output_tokens":1},"answers":{"risk":{"type":"score","score":0,"confidence":1,"legend":{"0":{"summary":"duplicated","examples":["charged twice"]}},"probabilities":{"0":1}}}}"#;
    let server = answering(Protocol::Http1, ANSWER).await;
    let criteria = json!({"summary": "duplicated", "examples": ["charged twice"]});
    let structured = || Content::json(&criteria).expect("an object is content");
    let questions = Questions::new()
        .raw(
            "duplicate",
            RawQuestion::new("noul")
                .field("instructions", json!({"question": "Duplicate?"}))
                .field("criteria", json!({"true": criteria})),
        )
        .choice(
            "team",
            Choice::new(["billing", "other"]).option("billing", structured()).instructions("Team?"),
        )
        .score("risk", Score::new([structured()]).instructions("Risk?"))
        .prepare()
        .expect("the questions prepare");
    let result = client_for(&server, Protocol::Http1)
        .system_one("a ticket", &questions)
        .model("custom")
        .send()
        .await
        .expect("the call succeeds");

    let legend = result.answers().score("risk").and_then(|risk| risk.description(0));
    let decoded: serde_json::Value =
        legend.and_then(Content::as_json).expect("a structured legend").decode().expect("JSON");
    assert_eq!(decoded, criteria);
    // `serde_json::json!` sorts keys, so the object is written in that order.
    assert_eq!(
        body_text(&server.requests()[0]),
        concat!(
            r#"{"state":"a ticket","model":"custom","questions":{"#,
            r#""duplicate":{"type":"noul","instructions":{"question":"Duplicate?"},"criteria":{"true":{"examples":["charged twice"],"summary":"duplicated"}}},"#,
            r#""team":{"type":"choice","instructions":"Team?","criteria":{"billing":{"examples":["charged twice"],"summary":"duplicated"},"other":null}},"#,
            r#""risk":{"type":"score","instructions":"Risk?","criteria":[{"examples":["charged twice"],"summary":"duplicated"}]}}}"#,
        )
    );
}

/// Upstream `test_validation_before_network`: questions that cannot be sent
/// never become a request, because they cannot become a `PreparedQuestions`.
#[test]
fn validation_before_network() {
    let empty = Questions::new().prepare().expect_err("no questions");
    assert!(matches!(empty.kind(), ErrorKind::InvalidRequest));
    assert_eq!(empty.to_string(), "At least one question is required.");

    let no_levels = Questions::new()
        .score("rating", Score::new(Vec::<&str>::new()).instructions("?"))
        .prepare()
        .expect_err("a score with no levels");
    assert!(matches!(no_levels.kind(), ErrorKind::InvalidRequest));
    assert!(no_levels.to_string().contains(r#""rating" has no criteria"#), "{no_levels}");
}

// ---------------------------------------------------------- API errors

/// Upstream `test_error_mapping`: every status to its class, with body,
/// request id, headers, message and `Retry-After`, over each protocol.
#[tokio::test]
async fn error_mapping() {
    let rows = [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::PermissionDenied),
        (404, ApiErrorKind::NotFound),
        (422, ApiErrorKind::UnprocessableEntity),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::InternalServer),
        (503, ApiErrorKind::InternalServer),
        (408, ApiErrorKind::Other),
        (409, ApiErrorKind::Other),
        (302, ApiErrorKind::Other),
    ];
    let body = r#"{"detail":{"message":"Server explanation"}}"#;
    for protocol in PROTOCOLS {
        for (status, kind) in rows {
            let server = TestServer::start(protocol, move |_| async move {
                let mut response =
                    json_response(StatusCode::from_u16(status).expect("valid"), body);
                let headers = response.headers_mut();
                headers.insert("x-typesafe-request-id", "req_123".parse().expect("valid"));
                headers.insert("retry-after-ms", "125".parse().expect("valid"));
                response
            })
            .await
            .expect("the test server starts");

            let error =
                client_for(&server, protocol).models().list().send().await.expect_err("refused");
            let api = api_error(&error);
            assert_eq!(api.kind(), kind, "{protocol:?} {status}");
            assert_eq!(api.status().as_u16(), status);
            assert_eq!(api.body(), body.as_bytes());
            assert_eq!(api.request_id(), Some("req_123"));
            assert_eq!(api.headers()["retry-after-ms"], "125");
            assert_eq!(api.retry_after(), Some(Duration::from_millis(125)));
            assert_eq!(
                error.to_string(),
                format!(
                    "GET {}/v1/models: {status} Server explanation (request_id=req_123)",
                    server.base_url()
                )
            );
            assert_eq!(server.request_count(), 1, "a failure status is not retried here");
        }
    }
}

/// Upstream `test_error_messages`: where the message is read from.
#[tokio::test]
async fn error_messages() {
    let rows: [(&str, &str); 8] = [
        (r#"{"error":"error","message":"message","detail":"detail"}"#, "error"),
        (r#"{"error":{"message":"nested error"},"message":"message"}"#, "nested error"),
        (r#"{"message":"message","detail":"detail"}"#, "message"),
        (r#"{"detail":"detail"}"#, "detail"),
        (r#"{"detail":{"message":"nested detail"}}"#, "nested detail"),
        (
            r#"{"detail":[{"loc":["body","questions","q","score","criteria",0],"msg":"Invalid"},{"msg":"Missing"},{}]}"#,
            "questions.q.score.criteria.0: Invalid; Missing",
        ),
        ("plain text", "plain text"),
        (r#"{"unexpected": true}"#, r#"{"unexpected":true}"#),
    ];
    for (body, message) in rows {
        let server = TestServer::start(Protocol::Http1, move |_| async move {
            json_response(StatusCode::BAD_REQUEST, body)
        })
        .await
        .expect("the test server starts");
        let error =
            client_for(&server, Protocol::Http1).models().list().send().await.expect_err("refused");
        assert_eq!(api_error(&error).kind(), ApiErrorKind::BadRequest);
        assert_eq!(
            error.to_string(),
            format!("GET {}/v1/models: 400 {message}", server.base_url()),
            "body {body}"
        );
    }
}

// ------------------------------------------------------ transport errors

/// A base URL with nothing listening at it.
async fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let address = listener.local_addr().expect("its address");
    drop(listener);
    format!("http://{address}")
}

/// A server that reads one request and answers every connection with
/// `reply`, then closes it.
async fn raw_server(reply: impl AsRef<[u8]>) -> String {
    let reply = Bytes::copy_from_slice(reply.as_ref());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let address = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0; 4096];
            // The reply goes out once the request has arrived; how much of it
            // one read returns does not matter to the client.
            let _ = stream.read(&mut buffer).await;
            let _ = stream.write_all(&reply).await;
            let _ = stream.shutdown().await;
        }
    });
    format!("http://{address}")
}

/// Asserts that `error` is a connection error whose message is its cause's
/// chain of messages, and returns that message.
fn connection_message(error: &Error) -> String {
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    let chain: Vec<String> =
        std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| {
            (*cause).source()
        })
        .map(ToString::to_string)
        .collect();
    let rendered = error.to_string();
    assert_eq!(rendered, format!("Connection error: {}", chain.join(": ")));
    rendered
}

/// The first cause under `error` of type `T`.
fn cause<T: StdError + 'static>(error: &Error) -> Option<&T> {
    std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| (*cause).source())
        .find_map(|cause| cause.downcast_ref::<T>())
}

/// Upstream `test_transport_errors[ConnectError, ReadError,
/// RemoteProtocolError]`: every one is a connection error with the
/// transport's own error as its cause.
#[tokio::test]
async fn transport_errors_are_connection_errors_with_their_cause() {
    let client = |url: &str| {
        Client::builder().api_key("test-key").base_url(url).build().expect("the client builds")
    };

    // Nothing listening.
    let error = client(&closed_port().await).models().list().send().await.expect_err("refused");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (Connect): "), "{message}");
    let refused = cause::<std::io::Error>(&error).expect("an I/O error under it");
    assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);

    // The response ends before its body does.
    let url = raw_server(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n{\"models\"").await;
    let error = client(&url).models().list().send().await.expect_err("a truncated body");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: "), "{message}");
    assert!(cause::<hyper::Error>(&error).is_some(), "{error:?}");

    // Not HTTP at all.
    let url = raw_server(b"SSH-2.0-OpenSSH_9.9\r\n\r\n").await;
    let error = client(&url).models().list().send().await.expect_err("not HTTP");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (SendRequest): "), "{message}");
    assert!(cause::<hyper::Error>(&error).is_some_and(hyper::Error::is_parse), "{error:?}");
}

/// A transport error that says whatever it likes: a newline, an ANSI
/// colour, a right-to-left override, and 100,000 characters.
#[derive(Debug)]
struct Loud;

impl std::fmt::Display for Loud {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "bad\nline \u{1b}[31mred \u{202e}rtl {}", "x".repeat(100_000))
    }
}

impl StdError for Loud {}

/// A transport that fails every request with [`Loud`].
#[derive(Debug, Clone, Copy)]
struct Failing;

impl Service<Request<Body>> for Failing {
    type Response = Response<Body>;
    type Error = Loud;
    type Future = std::future::Ready<Result<Response<Body>, Loud>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Loud>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        std::future::ready(Err(Loud))
    }
}

/// Text a caller's own transport puts in its error reaches the message
/// escaped and cut 200 characters after `Connection error: `; the error
/// itself, whole, is the cause.
#[tokio::test]
async fn a_transport_error_of_any_text_is_escaped_and_cut_in_the_message() {
    let client = Client::builder()
        .api_key("test-key")
        .base_url("https://api.typesafe.ai")
        .build_with_service(Failing)
        .expect("the client builds");
    let error = client.models().list().send().await.expect_err("the transport fails");

    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    let rendered = error.to_string();
    assert_eq!(
        rendered,
        format!(
            "Connection error: bad\\nline \\u{{1b}}[31mred \\u{{202e}}rtl {}\u{2026}",
            "x".repeat(164)
        )
    );
    // 18 characters of prefix, 200 of the transport's text, the mark.
    assert_eq!(rendered.chars().count(), 18 + 200 + 1);
    for shown in [rendered.clone(), format!("{rendered:?}")] {
        assert!(!shown.bytes().any(|byte| byte < 0x20 || byte == 0x7f), "{shown:?}");
        assert!(!shown.contains('\u{202e}'), "{shown:?}");
    }
    let cause = error.source().and_then(|cause| cause.downcast_ref::<Loud>());
    assert_eq!(cause.map(ToString::to_string), Some(Loud.to_string()));
}

/// A TLS server whose certificate the client was not told to trust.
#[tokio::test]
async fn an_untrusted_certificate_is_a_connection_error_naming_the_certificate() {
    let server = answering(Protocol::Http2Tls, br#"{"models":[]}"#).await;
    let client =
        Client::builder().api_key("test-key").base_url(server.base_url()).build().expect("builds");

    let error = client.models().list().send().await.expect_err("the certificate is not trusted");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (Connect): "), "{message}");
    // `io::Error::source` skips the error it wraps, and the connector wraps
    // the TLS error in two of them, so it is reached through `get_ref`.
    let mut wrapped = cause::<std::io::Error>(&error).and_then(|io| io.get_ref());
    while let Some(io) = wrapped.and_then(|inner| inner.downcast_ref::<std::io::Error>()) {
        wrapped = io.get_ref();
    }
    let tls = wrapped
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .unwrap_or_else(|| panic!("the TLS error under {error:#?}"));
    assert!(matches!(tls, rustls::Error::InvalidCertificate(_)), "{tls:?}");
    assert_eq!(server.request_count(), 0);
}

/// A server whose handler holds every response until the test releases it,
/// and says when a request has arrived.
struct Held {
    server: TestServer,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

async fn held(protocol: Protocol, body: &'static [u8]) -> Held {
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (on_arrival, on_release) = (Arc::clone(&arrived), Arc::clone(&release));
    let server = TestServer::start(protocol, move |_| {
        let (arrived, release) = (Arc::clone(&on_arrival), Arc::clone(&on_release));
        async move {
            arrived.notify_one();
            release.notified().await;
            json_response(StatusCode::OK, body)
        }
    })
    .await
    .expect("the test server starts");
    Held { server, arrived, release }
}

/// Upstream `test_transport_errors[ConnectTimeout, ReadTimeout]`: an attempt
/// past its deadline is a timeout carrying that deadline. (Section 6 row "per-
/// attempt TOTAL deadline": one deadline covers the whole attempt, not each
/// httpx phase.)
#[tokio::test]
async fn an_attempt_past_its_deadline_is_a_timeout_with_that_deadline() {
    for protocol in PROTOCOLS {
        let held = held(protocol, br#"{"models":[]}"#).await;
        let error = client_for(&held.server, protocol)
            .models()
            .list()
            .timeout(Duration::from_millis(125))
            .send()
            .await
            .expect_err("the response is held back");

        assert!(
            matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_millis(125)),
            "{protocol:?}: {error:?}"
        );
        assert_eq!(error.to_string(), "Request timed out (timeout=0.125s).");
        assert!(error.source().is_none());
        assert_eq!(held.server.request_count(), 1, "{protocol:?}: the request reached the server");
        held.release.notify_waiters();
    }
}

/// Upstream `test_system_one_timeout_override` (and `test_invalid_timeout`'s
/// per-call half): a call's deadline replaces the client's for that call
/// only, `no_timeout` removes it, and zero is refused before the network.
#[tokio::test]
async fn system_one_timeout_override() {
    let held = held(Protocol::Http1, RESULT).await;
    let client = builder_for(&held.server, Protocol::Http1)
        .timeout(Duration::from_millis(100))
        .build()
        .expect("builds");
    let questions = one_raw_question();

    // The client's own deadline fails a held call.
    let error = client.system_one("hello", &questions).send().await.expect_err("held past 100 ms");
    assert!(
        matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_millis(100)),
        "{error:?}"
    );
    held.release.notify_waiters();

    // A longer per-call deadline, and no deadline, outlast a hold three times
    // as long as the client's deadline, which that deadline would not allow.
    for (label, request) in [
        ("timeout(5s)", client.system_one("hello", &questions).timeout(Duration::from_secs(5))),
        ("no_timeout()", client.system_one("hello", &questions).no_timeout()),
    ] {
        let release = async {
            held.arrived.notified().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            held.release.notify_one();
        };
        let (response, ()) = tokio::join!(request.send(), release);
        let response = response.unwrap_or_else(|error| panic!("{label}: {error}"));
        assert_eq!(response.model(), "jev-latest", "{label}");
    }

    let error = client
        .system_one("hello", &questions)
        .timeout(Duration::ZERO)
        .send()
        .await
        .expect_err("a zero deadline");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    assert_eq!(error.to_string(), "timeout must be a positive, finite number of seconds.");
    assert_eq!(held.server.request_count(), 3, "the zero deadline never reached the network");
}

// ------------------------------------------------------- response limit

/// The 16 MiB cap of section 6 ("no response size limit" row), set lower: a
/// body over it is not read past it, whatever says how long it is.
#[tokio::test]
async fn a_response_over_the_limit_is_refused() {
    let big = Bytes::from(vec![b' '; 4096]);
    for protocol in [Protocol::Http1, Protocol::H2c] {
        // A success response whose declared length is over the limit: its
        // own kind, naming the limit, with no cause.
        let server = answering(protocol, big.clone()).await;
        let client =
            builder_for(&server, protocol).max_response_bytes(1024).build().expect("builds");
        let error = client.models().list().send().await.expect_err("over the limit");
        assert_too_large(&error, 1024);

        // A failure response keeps its status and headers, and no body.
        let failing = big.clone();
        let server = TestServer::start(protocol, move |_| {
            let body = failing.clone();
            async move {
                let mut response = json_response(StatusCode::SERVICE_UNAVAILABLE, body);
                response.headers_mut().insert("retry-after", "3".parse().expect("valid"));
                response
            }
        })
        .await
        .expect("the test server starts");
        let client =
            builder_for(&server, protocol).max_response_bytes(1024).build().expect("builds");
        let error = client.models().list().send().await.expect_err("over the limit");
        let api = api_error(&error);
        assert_eq!(api.kind(), ApiErrorKind::InternalServer);
        assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.body(), b"");
        assert_eq!(api.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(
            error.to_string(),
            format!(
                "GET {}/v1/models: 503 The response body exceeded the limit of 1024 bytes and was not read.",
                server.base_url()
            )
        );
    }

    // A body at the limit is read.
    let at_limit: &'static [u8] = br#"{"models":[]}"#;
    let server = answering(Protocol::Http1, at_limit).await;
    let client = builder_for(&server, Protocol::Http1)
        .max_response_bytes(at_limit.len())
        .build()
        .expect("builds");
    let response = client.models().list().send().await.expect("exactly at the limit");
    assert!(response.models().is_empty());
}

/// The error a success response over `limit` must be.
fn assert_too_large(error: &Error, limit: usize) {
    assert!(
        matches!(error.kind(), ErrorKind::ResponseTooLarge { limit: at } if *at == limit),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        format!("The response body exceeded the limit of {limit} bytes and was not read.")
    );
    assert!(error.source().is_none(), "{error:?}");
}

/// A `content-length` over the limit is refused before a byte of the body is
/// read - even when the server then sends far less than it declared.
#[tokio::test]
async fn a_declared_length_over_the_limit_is_refused_whatever_follows_it() {
    let url = raw_server(b"HTTP/1.1 200 OK\r\ncontent-length: 5000\r\n\r\n{\"models\":[]}").await;
    let client = Client::builder()
        .api_key("test-key")
        .base_url(url)
        .max_response_bytes(1024)
        .build()
        .expect("builds");
    let error = client.models().list().send().await.expect_err("declared over the limit");
    assert_too_large(&error, 1024);
}

/// A body with no declared length is cut off by the limit as it streams.
#[tokio::test]
async fn a_streamed_response_over_the_limit_is_refused_as_it_arrives() {
    // One chunk of 0x400 = 1024 bytes, and no `content-length`.
    let reply = format!(
        "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n400\r\n{}\r\n0\r\n\r\n",
        " ".repeat(1024)
    );
    let url = raw_server(reply).await;
    let client = Client::builder()
        .api_key("test-key")
        .base_url(url)
        .max_response_bytes(100)
        .build()
        .expect("builds");
    let error = client.models().list().send().await.expect_err("over the limit");
    assert_too_large(&error, 100);
}

// -------------------------------------------------------------- headers

/// A transport that forwards every request to a loopback server over plain
/// HTTP/1.1, keeping the host it was asked for in `Host`: how a request to a
/// host this test cannot resolve still reaches a real server.
#[derive(Clone)]
struct Forward {
    inner: legacy::Client<HttpConnector, Body>,
    /// The server's `host:port`.
    target: Uri,
    /// Every URI the SDK handed over, in order.
    asked: Arc<Mutex<Vec<(http::Method, Uri)>>>,
}

impl Forward {
    fn to(server: &TestServer) -> Self {
        Self {
            inner: legacy::Client::builder(TokioExecutor::new()).build_http(),
            target: server.base_url().parse().expect("a URL"),
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> Vec<(http::Method, Uri)> {
        self.asked.lock().expect("not poisoned").clone()
    }
}

impl Service<Request<Body>> for Forward {
    type Response = Response<Incoming>;
    type Error = legacy::Error;
    type Future = legacy::ResponseFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), legacy::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let asked = request.uri().clone();
        self.asked.lock().expect("not poisoned").push((request.method().clone(), asked.clone()));
        let authority = asked.authority().expect("the SDK sends an absolute URI").as_str();
        request.headers_mut().insert(HOST, authority.parse().expect("a host is a header value"));
        let mut parts = asked.into_parts();
        parts.scheme = Some(http::uri::Scheme::HTTP);
        parts.authority = self.target.authority().cloned();
        *request.uri_mut() = Uri::from_parts(parts).expect("the parts came from a URI");
        self.inner.request(request)
    }
}

/// The five headers upstream protects, each set to something wrong.
const PROTECTED: [(&str, &str); 5] = [
    ("authorization", "injected-secret"),
    ("accept", "text/plain"),
    ("user-agent", "wrong"),
    ("x-typesafe-sdk", "wrong"),
    ("x-typesafe-runtime", "wrong"),
];

/// A server answering the fixture with a credential and a request id among
/// its headers.
async fn logging_server(protocol: Protocol) -> TestServer {
    TestServer::start(protocol, |_| async {
        let mut response = json_response(StatusCode::OK, RESULT);
        let headers = response.headers_mut();
        headers.insert("set-cookie", "response-secret".parse().expect("valid"));
        headers.insert("x-typesafe-request-id", "req_log".parse().expect("valid"));
        response
    })
    .await
    .expect("the test server starts")
}

/// Sets every header upstream's case sets, on the client and on the call, and
/// sends one request.
async fn send_upstream_headers(builder: ClientBuilder) {
    let mut builder = builder.timeout(Duration::from_secs(7));
    for (name, value) in PROTECTED {
        builder = builder.default_header(name, value);
    }
    let client = builder
        .default_header("X-Team", "default")
        .default_header("X-Default", "kept")
        .default_header("X-API-Key", "key-secret")
        .default_header("cookie", "cookie-secret")
        .build()
        .expect("the client builds");
    let questions = one_raw_question();
    let mut request = client.system_one("hello", &questions).timeout(Duration::from_secs(2));
    for (name, value) in PROTECTED {
        request = request.header(name, value);
    }
    request
        .header("x-team", "call")
        .header("x-typesafe-retry-count", "99")
        .header("content-type", "wrong")
        .send()
        .await
        .expect("the call succeeds");
}

/// What the server must have recorded for [`send_upstream_headers`].
fn assert_upstream_headers(request: &RecordedRequest, context: &str) {
    let sdk = format!("typesafe-sdk-rust/{}", env!("CARGO_PKG_VERSION"));
    let runtime = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    assert_eq!(header(request, "authorization"), ["Bearer test-key"], "{context}");
    assert_eq!(header(request, "accept"), ["application/json"], "{context}");
    assert_eq!(header(request, "user-agent"), [sdk.as_str()], "{context}");
    assert_eq!(header(request, "x-typesafe-sdk"), [sdk.as_str()], "{context}");
    assert_eq!(header(request, "x-typesafe-runtime"), [runtime.as_str()], "{context}");
    assert!(request.headers.get("x-typesafe-retry-count").is_none(), "{context}");
    assert_eq!(header(request, "x-team"), ["call"], "{context}");
    assert_eq!(header(request, "x-default"), ["kept"], "{context}");
    assert_eq!(header(request, "content-type"), ["application/json"], "{context}");
    // A secret default is sent; it is kept out of logs, not off the wire.
    assert_eq!(header(request, "x-api-key"), ["key-secret"], "{context}");
    assert_eq!(header(request, "cookie"), ["cookie-secret"], "{context}");
}

/// Upstream `test_headers_timeout_and_logging` - the headers half, AC-F5:
/// the protected headers and `Content-Type` cannot be overridden, a caller's
/// `x-typesafe-retry-count: 99` is not sent, and a base URL of
/// `<root>/prefix///` sends to `<root>/prefix/v1/systemone`. Asserted on what
/// the server recorded, over each protocol.
#[tokio::test]
async fn headers_timeout_and_logging() {
    for protocol in PROTOCOLS {
        let server = logging_server(protocol).await;
        let builder =
            builder_for(&server, protocol).base_url(format!("{}/prefix///", server.base_url()));
        send_upstream_headers(builder).await;

        let [request] = &server.requests()[..] else { panic!("{protocol:?}: one request") };
        assert_eq!(request.uri.path(), "/prefix/v1/systemone", "{protocol:?}");
        assert_upstream_headers(request, &format!("{protocol:?}"));
    }
}

/// AC-F5's URL exactly: `https://example.test/prefix///` sends to
/// `https://example.test/prefix/v1/systemone`. The host cannot be resolved
/// here, so a forwarding transport carries the request to the loopback
/// server, which records the path and the `Host` it was sent.
#[tokio::test]
async fn the_upstream_base_url_with_a_prefix_and_trailing_slashes() {
    let server = logging_server(Protocol::Http1).await;
    let forward = Forward::to(&server);
    let mut builder = Client::builder()
        .api_key("test-key")
        .default_model("jev-latest")
        .base_url("https://example.test/prefix///")
        .timeout(Duration::from_secs(7));
    for (name, value) in PROTECTED {
        builder = builder.default_header(name, value);
    }
    let client = builder
        .default_header("X-Team", "default")
        .default_header("X-Default", "kept")
        .default_header("X-API-Key", "key-secret")
        .default_header("cookie", "cookie-secret")
        .build_with_service(forward.clone())
        .expect("the client builds");
    let questions = one_raw_question();
    let mut request = client.system_one("hello", &questions).timeout(Duration::from_secs(2));
    for (name, value) in PROTECTED {
        request = request.header(name, value);
    }
    request
        .header("x-team", "call")
        .header("x-typesafe-retry-count", "99")
        .header("content-type", "wrong")
        .send()
        .await
        .expect("the call succeeds");

    assert_eq!(
        forward.asked(),
        [(http::Method::POST, Uri::from_static("https://example.test/prefix/v1/systemone"))]
    );
    let [request] = &server.requests()[..] else { panic!("one request") };
    assert_eq!(request.uri.path(), "/prefix/v1/systemone");
    assert_eq!(header(request, "host"), ["example.test"]);
    assert_upstream_headers(request, "example.test");
}

/// Upstream `test_http_client_settings`, as section 6 has it ("`http_client=`
/// / `transport=`" row): a transport of the caller's own carries every
/// request, with the SDK's headers and the call's.
#[tokio::test]
async fn a_custom_transport_carries_every_request_with_the_sdk_headers() {
    let server = TestServer::start(Protocol::Http1, |request: RecordedRequest| async move {
        let body: &'static [u8] =
            if request.method == http::Method::POST { RESULT } else { br#"{"models":[]}"# };
        json_response(StatusCode::OK, body)
    })
    .await
    .expect("the test server starts");
    let forward = Forward::to(&server);
    let client = Client::builder()
        .api_key("test-key")
        .base_url("https://api.typesafe.ai")
        .default_header("x-sdk-default", "sdk")
        .default_header("x-call", "sdk")
        .build_with_service(forward.clone())
        .expect("the client builds");

    let models = client.models().list().header("x-call", "call").send().await.expect("listed");
    assert!(models.models().is_empty());
    let questions = one_raw_question();
    client.system_one("x", &questions).header("x-call", "call").send().await.expect("answered");

    let asked: Vec<_> =
        forward.asked().into_iter().map(|(method, uri)| format!("{method} {uri}")).collect();
    assert_eq!(
        asked,
        ["GET https://api.typesafe.ai/v1/models", "POST https://api.typesafe.ai/v1/systemone"]
    );
    for request in server.requests() {
        assert_eq!(header(&request, "authorization"), ["Bearer test-key"]);
        assert_eq!(header(&request, "accept"), ["application/json"]);
        assert_eq!(header(&request, "x-sdk-default"), ["sdk"]);
        assert_eq!(header(&request, "x-call"), ["call"]);
        let content_type =
            if request.method == http::Method::POST { vec!["application/json"] } else { vec![] };
        assert_eq!(header(&request, "content-type"), content_type, "{}", request.method);
    }
}

/// A transport that counts how many copies of it are alive.
struct Counted {
    forward: Forward,
    alive: Arc<AtomicUsize>,
}

impl Counted {
    fn new(forward: Forward, alive: &Arc<AtomicUsize>) -> Self {
        alive.fetch_add(1, Ordering::SeqCst);
        Self { forward, alive: Arc::clone(alive) }
    }
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        Self::new(self.forward.clone(), &self.alive)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.alive.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Service<Request<Body>> for Counted {
    type Response = Response<Incoming>;
    type Error = legacy::Error;
    type Future = legacy::ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), legacy::Error>> {
        self.forward.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        self.forward.call(request)
    }
}

/// Upstream `test_supplied_network_resources_closed`,
/// `test_owned_http_client_closed` and
/// `test_exceptional_context_closes_http_client`, as section 6 has them
/// ("explicit `close()`/context managers" row): a client owns its transport by
/// value, and dropping the last clone of the client drops it - after success
/// and after failure alike.
#[tokio::test]
async fn the_last_clone_of_a_client_drops_its_transport() {
    let server = answering(Protocol::Http1, br#"{"models":[]}"#).await;
    let alive = Arc::new(AtomicUsize::new(0));
    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://api.test")
        .build_with_service(Counted::new(Forward::to(&server), &alive))
        .expect("the client builds");
    let clone = client.clone();
    assert_eq!(alive.load(Ordering::SeqCst), 1, "a client clone shares the transport");

    clone.models().list().send().await.expect("listed");
    let failure = clone.models().list().timeout(Duration::ZERO).send().await;
    assert!(failure.is_err_and(|error| matches!(error.kind(), ErrorKind::InvalidRequest)));
    assert_eq!(alive.load(Ordering::SeqCst), 1, "each call's clone of the transport is gone");

    drop(client);
    assert_eq!(alive.load(Ordering::SeqCst), 1, "another clone still holds it");
    drop(clone);
    assert_eq!(alive.load(Ordering::SeqCst), 0, "the last clone dropped it");
}

/// Upstream `test_task_cancellation_closes_context` and
/// `test_cancellation_propagates`: dropping the future of a call in flight
/// cancels it - one request reached the server, nothing more is sent - and
/// the client goes on working.
#[tokio::test]
async fn dropping_a_call_in_flight_cancels_it() {
    for protocol in PROTOCOLS {
        let held = held(protocol, br#"{"models":[]}"#).await;
        let client = client_for(&held.server, protocol);
        {
            let call = client.models().list().no_timeout().send();
            tokio::pin!(call);
            tokio::select! {
                result = &mut call => panic!("{protocol:?}: the held call returned {result:?}"),
                () = held.arrived.notified() => {}
            }
        }
        assert_eq!(held.server.request_count(), 1, "{protocol:?}");

        held.release.notify_one();
        let release = async {
            held.arrived.notified().await;
            held.release.notify_one();
        };
        let (listed, ()) = tokio::join!(client.models().list().send(), release);
        assert!(listed.expect("the client still works").models().is_empty());
        assert_eq!(held.server.request_count(), 2, "{protocol:?}");
    }
}

// -------------------------------------------------------------- logging

/// The logging half of upstream `test_headers_timeout_and_logging` and
/// `test_secret_headers_redacted`: no credential reaches an event, whichever
/// side of the exchange carried it; the request id and the body do, the body
/// at `TRACE` only (section 6 row "DEBUG logs full bodies").
#[cfg(feature = "tracing")]
mod logging {
    use std::fmt::{self, Write as _};

    use tracing::{
        Event, Level, Metadata, Subscriber,
        field::{Field, Visit},
        span,
    };

    use super::*;

    /// Every event recorded while it is the default subscriber, as lines of
    /// text: level, target, then each field.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<(Level, String)>>>);

    impl Recorder {
        fn text(&self) -> String {
            let events = self.0.lock().expect("not poisoned");
            events.iter().map(|(level, line)| format!("{level} {line}\n")).collect()
        }

        /// This crate's events at `level`; hyper's own are left out.
        fn at(&self, level: Level) -> Vec<String> {
            let events = self.0.lock().expect("not poisoned");
            events
                .iter()
                .filter(|(at, line)| *at == level && line.starts_with("typesafe_sdk "))
                .map(|(_, line)| line.clone())
                .collect()
        }
    }

    struct Line(String);

    impl Visit for Line {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            write!(self.0, " {}={value:?}", field.name()).expect("a String takes any write");
        }
    }

    impl Subscriber for Recorder {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }

        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut line = Line(event.metadata().target().to_owned());
            event.record(&mut line);
            self.0.lock().expect("not poisoned").push((*event.metadata().level(), line.0));
        }

        fn enter(&self, _: &span::Id) {}

        fn exit(&self, _: &span::Id) {}
    }

    /// Every event is recorded, hyper's included, so the secrets are checked
    /// against everything a subscriber would see.
    #[tokio::test]
    async fn no_credential_reaches_an_event_and_the_body_waits_for_trace() {
        let recorder = Recorder::default();
        let _default = tracing::subscriber::set_default(recorder.clone());

        let server = logging_server(Protocol::Http1).await;
        send_upstream_headers(builder_for(&server, Protocol::Http1)).await;

        let text = recorder.text();
        for secret in
            ["test-key", "injected-secret", "key-secret", "cookie-secret", "response-secret"]
        {
            assert!(!text.contains(secret), "{secret} reached the events:\n{text}");
        }
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("req_log"), "{text}");
        assert!(text.contains("x-default"), "{text}");

        let debug = recorder.at(Level::DEBUG);
        assert_eq!(debug.len(), 2, "one event each way: {debug:#?}");
        assert!(debug[0].contains("message=sending request"), "{}", debug[0]);
        assert!(debug[0].contains("method=POST"), "{}", debug[0]);
        assert!(
            debug[0].contains(&format!("endpoint={}/v1/systemone", server.base_url())),
            "{}",
            debug[0]
        );
        let sent = server.requests()[0].body.len();
        assert!(debug[0].contains(&format!("body_len={sent}")), "{}", debug[0]);
        assert!(debug[1].contains("message=received response"), "{}", debug[1]);
        assert!(debug[1].contains("status=200"), "{}", debug[1]);
        assert!(debug[1].contains("request_id=req_log "), "{}", debug[1]);
        assert!(debug.iter().all(|line| !line.contains("hello")), "a body at DEBUG: {debug:#?}");

        let trace = recorder.at(Level::TRACE);
        assert_eq!(trace.len(), 2, "{trace:#?}");
        assert!(trace[0].contains(r#"body={"state":"hello","#), "{}", trace[0]);
        assert!(trace[1].contains(r#"body={"model":"jev-latest","#), "{}", trace[1]);
    }

    /// The `INFO` line an attempt's single `INFO` event renders as, without
    /// the target.
    fn only_info_line(recorder: &Recorder) -> String {
        let lines = recorder.at(Level::INFO);
        let [line] = &lines[..] else { panic!("one INFO line expected: {lines:#?}") };
        line.strip_prefix("typesafe_sdk message=").unwrap_or_else(|| panic!("{line}")).to_owned()
    }

    /// `<prefix><digits>ms<suffix>`, and nothing else.
    fn assert_timed(line: &str, prefix: &str, suffix: &str) {
        let millis = line
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .and_then(|rest| rest.strip_suffix("ms"))
            .unwrap_or_else(|| panic!("{line:?} is not {prefix:?}<n>ms{suffix:?}"));
        assert!(millis.bytes().all(|byte| byte.is_ascii_digit()) && !millis.is_empty(), "{line}");
    }

    /// Upstream `test_logger_level_controls_output`'s INFO summary, and its
    /// failure line: one `INFO` event per attempt, as upstream words it, for
    /// a response of any status and for an attempt that got none. No INFO or
    /// DEBUG event carries a body, a header value or an error's own message.
    #[tokio::test]
    async fn every_attempt_gets_one_info_line_and_no_body_reaches_info_or_debug() {
        // A success.
        let recorder = Recorder::default();
        let default = tracing::subscriber::set_default(recorder.clone());
        let server = logging_server(Protocol::Http1).await;
        let questions = one_raw_question();
        client_for(&server, Protocol::Http1)
            .system_one("secret-state", &questions)
            .send()
            .await
            .expect("answered");
        let base = server.base_url();
        assert_timed(
            &only_info_line(&recorder),
            &format!("POST {base}/v1/systemone <- 200 in "),
            " (request req_log)",
        );
        let quiet = [recorder.at(Level::INFO), recorder.at(Level::DEBUG)].concat();
        assert!(quiet.iter().all(|line| !line.contains("secret-state")), "{quiet:#?}");
        drop(default);

        // A failure status, with a body that says something private.
        let recorder = Recorder::default();
        let default = tracing::subscriber::set_default(recorder.clone());
        let server = TestServer::start(Protocol::Http1, |_| async {
            json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"message":"secret-body"}"#)
        })
        .await
        .expect("the test server starts");
        let error =
            client_for(&server, Protocol::Http1).models().list().send().await.expect_err("503");
        assert_eq!(api_error(&error).message(), "secret-body");
        assert_timed(
            &only_info_line(&recorder),
            &format!("GET {}/v1/models <- 503 in ", server.base_url()),
            " (request -)",
        );
        let quiet = [recorder.at(Level::INFO), recorder.at(Level::DEBUG)].concat();
        assert!(quiet.iter().all(|line| !line.contains("secret-body")), "{quiet:#?}");
        drop(default);

        // No response: a timeout, a refused connection, a body over the limit.
        let held = held(Protocol::Http1, br#"{"models":[]}"#).await;
        let refused = closed_port().await;
        let big = answering(Protocol::Http1, vec![b' '; 4096]).await;
        let cases: [(ClientBuilder, &str); 3] = [
            (
                builder_for(&held.server, Protocol::Http1).timeout(Duration::from_millis(50)),
                "timeout",
            ),
            (Client::builder().api_key("test-key").base_url(refused.as_str()), "connection error"),
            (builder_for(&big, Protocol::Http1).max_response_bytes(1024), "response too large"),
        ];
        for (builder, word) in cases {
            let recorder = Recorder::default();
            let _default = tracing::subscriber::set_default(recorder.clone());
            let client = builder.build().expect("the client builds");
            let error = client.models().list().send().await.expect_err(word);
            let endpoint = format!("GET {}/v1/models", client_base(&client));
            assert_eq!(only_info_line(&recorder), format!("{endpoint} <- {word}"));
            // The DEBUG failure event names the kind by the same word and
            // carries no message: a connection error's message is the
            // transport's chain, which can hold text the server chose.
            let debug = recorder.at(Level::DEBUG);
            let failure = debug.last().expect("a DEBUG failure event");
            assert!(failure.contains(&format!("failure=\"{word}\"")), "{failure}");
            assert!(!failure.contains(&error.to_string()), "{failure}");
        }
        held.release.notify_waiters();
    }

    /// The base URL a client sends to, read back from its `Debug`, which
    /// prints its endpoints as an error names them.
    fn client_base(client: &Client) -> String {
        let debug = format!("{client:?}");
        let start = debug.find("\"GET ").expect("the models endpoint") + "\"GET ".len();
        let end = debug[start..].find("/v1/models").expect("the models path") + start;
        debug[start..end].to_owned()
    }

    /// Upstream `test_secret_headers_redacted`, the nine spellings of a secret
    /// header, on the request (a client default) and on the response, for a
    /// success and a failure.
    #[tokio::test]
    async fn every_secret_header_spelling_is_redacted_both_ways() {
        let names = [
            "Authorization",
            "Proxy-Authorization",
            "X-API-Key",
            "API-Key",
            "Cookie",
            "Set-Cookie",
            "X-Access-Token",
            "X-Client-Secret",
            "x-MiXeD-ToKeN",
        ];
        for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
            for name in names {
                let recorder = Recorder::default();
                let _default = tracing::subscriber::set_default(recorder.clone());
                let server = TestServer::start(Protocol::Http1, move |_| async move {
                    let body: &'static [u8] = if status.is_success() {
                        br#"{"models":[]}"#
                    } else {
                        br#"{"message":"failure"}"#
                    };
                    let mut response = json_response(status, body);
                    let headers = response.headers_mut();
                    headers.insert(
                        http::HeaderName::from_bytes(name.as_bytes()).expect("valid"),
                        "response-credential".parse().expect("valid"),
                    );
                    headers.insert("x-visible", "response-visible".parse().expect("valid"));
                    response
                })
                .await
                .expect("the test server starts");
                let client = Client::builder()
                    .api_key("auth-credential")
                    .base_url(server.base_url())
                    .default_header(name, "request-credential")
                    .default_header("x-visible", "request-visible")
                    .build()
                    .expect("the client builds");
                let outcome = client.models().list().send().await;
                assert_eq!(outcome.is_ok(), status.is_success(), "{name} {status}");

                let text = recorder.text();
                assert!(text.contains("request-visible"), "{name} {status}:\n{text}");
                assert!(text.contains("response-visible"), "{name} {status}:\n{text}");
                assert!(text.contains("***"), "{name} {status}:\n{text}");
                for secret in ["auth-credential", "request-credential", "response-credential"] {
                    assert!(!text.contains(secret), "{name} {status}: {secret} reached\n{text}");
                }
            }
        }
    }
}
