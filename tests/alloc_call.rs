//! What one whole System One call costs in allocations, beyond what its
//! transport costs.
//!
//! The call goes through an in-memory transport that answers the upstream
//! `RESULT` fixture, so everything measured is the SDK's own work: the body
//! encode, the per-attempt header map, request assembly, reading the response
//! and decoding it. The transport's own allocations are measured separately,
//! by calling it directly with a request built beforehand, and subtracted.
//!
//! Scenario, as the budget pins it: a 1 KB string `state`, three questions,
//! no per-call headers, the default deadline, no `tracing` subscriber, the
//! second of two identical calls. Both runs consume the response body the
//! same way - frame by frame - and the SDK additionally keeps it.
//!
//! The budget is the frozen inventory without the retained retry body, which
//! the retry phase adds: encode 1 + header map 2 + request assembly 0 + the
//! queue of collected frames 1 + decode 14 = 18 blocks.
//!
//! One profiler exists per process, so all of it runs in a single test.

use std::{
    convert::Infallible,
    future::{Ready, ready},
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use http_body_util::BodyExt as _;
use tower_service::Service;
use typesafe_sdk::{
    __internals as sdk, Body, Choice, Client, Noul, PreparedQuestions, Questions, Score,
    response::Answers,
};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `RESULT` of `tests/test_clients.py:42-56`, as the upstream test sends it.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// The frozen per-call budget, without the retained retry body.
const MAX_BLOCKS: u64 = 18;

/// A transport that answers every request with the fixture, allocating the
/// same blocks whatever the request: one response header.
#[derive(Debug, Clone, Copy)]
struct InMemory;

impl Service<Request<Body>> for InMemory {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        drop(request);
        let mut response = Response::new(Body::from(Bytes::from_static(RESULT)));
        response.headers_mut().insert("x-typesafe-request-id", HeaderValue::from_static("req-1"));
        ready(Ok(response))
    }
}

/// The change in dhat's counters across one section.
#[derive(Clone, Copy, Debug)]
struct Measured {
    blocks: u64,
    bytes: u64,
}

fn measure<F, T>(body: F) -> (Measured, T)
where
    F: FnOnce() -> T,
{
    let before = dhat::HeapStats::get();
    let value = body();
    let after = dhat::HeapStats::get();
    (
        Measured {
            blocks: after.total_blocks - before.total_blocks,
            bytes: after.total_bytes - before.total_bytes,
        },
        value,
    )
}

/// The three questions the fixture answers.
fn questions() -> PreparedQuestions {
    Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"))
        .prepare()
        .expect("the questions prepare")
}

/// Calls the transport directly with `request` and reads the response the
/// way a caller of it would: frame by frame, keeping nothing.
async fn call_directly(request: Request<Body>) -> StatusCode {
    let response = InMemory.call(request).await.unwrap_or_else(|never| match never {});
    let (parts, mut body) = response.into_parts();
    while let Some(frame) = body.frame().await {
        drop(frame.unwrap_or_else(|never| match never {}));
    }
    parts.status
}

/// A request like the one the SDK sends, built outside the measurement.
fn prebuilt_request() -> Request<Body> {
    let mut request = Request::new(Body::from(Bytes::from_static(b"{\"state\":\"x\"}")));
    *request.method_mut() = Method::POST;
    *request.uri_mut() = Uri::from_static("http://127.0.0.1:9/v1/systemone");
    request
}

#[test]
fn a_whole_call_costs_its_own_steps_and_nothing_else() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the runtime builds");
    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build_with_service(InMemory)
        .expect("the client builds");
    let questions = questions();
    let state = "x".repeat(1024);

    let call = || {
        runtime
            .block_on(client.system_one(state.as_str(), &questions).send())
            .expect("the call succeeds")
    };
    let untimed = || {
        runtime
            .block_on(client.system_one(state.as_str(), &questions).no_timeout().send())
            .expect("the call succeeds")
    };

    // Warm-ups: the encode scratch, the promoted `Bytes` of the endpoint and
    // the header values, the timer wheel, the event callsites.
    drop(call());
    drop(untimed());
    assert_eq!(runtime.block_on(call_directly(prebuilt_request())), StatusCode::OK);

    let (whole, response) = measure(call);
    let (whole_untimed, untimed_response) = measure(untimed);
    let request = prebuilt_request();
    let (direct, status) = measure(|| runtime.block_on(call_directly(request)));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.answers().len(), 3);
    assert_eq!(untimed_response.answers(), response.answers());
    drop((response, untimed_response));

    // The steps on their own, through the same code the call runs.
    let (encode, body) = measure(|| {
        sdk::encode_body(|buffer| {
            buffer.extend_from_slice(br#"{"state":"#);
            sdk::encode_into(buffer, state.as_str())?;
            buffer.extend_from_slice(br#","model":"jev-latest","questions":"#);
            buffer.extend_from_slice(b"{}");
            buffer.push(b'}');
            Ok(())
        })
        .expect("the body encodes")
    });
    drop(body);
    let mut headers = HeaderMap::with_capacity(6);
    for name in ["authorization", "accept", "user-agent", "x-typesafe-sdk", "x-typesafe-runtime"] {
        headers.insert(name, HeaderValue::from_static("value"));
    }
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    // The first clone moves the names' bytes into shared storage, once; the
    // client's own map went through that during the warm-up call.
    drop(headers.clone());
    let (header_clone, cloned) = measure(|| headers.clone());
    drop(cloned);
    let fixture = Bytes::from_static(RESULT);
    drop(sdk::decode_system_one::<Answers>(fixture.clone(), StatusCode::OK, HeaderMap::new(), 3));
    let (decode, decoded) =
        measure(|| sdk::decode_system_one::<Answers>(fixture, StatusCode::OK, HeaderMap::new(), 3));
    drop(decoded);

    let sdk_blocks = whole.blocks - direct.blocks;
    let sdk_untimed = whole_untimed.blocks - direct.blocks;
    println!("whole call               blocks={:>3} bytes={:>6}", whole.blocks, whole.bytes);
    println!(
        "whole call, no deadline  blocks={:>3} bytes={:>6}",
        whole_untimed.blocks, whole_untimed.bytes
    );
    println!("transport called directly blocks={:>3} bytes={:>6}", direct.blocks, direct.bytes);
    println!(
        "SDK's own                blocks={sdk_blocks:>3} (budget {MAX_BLOCKS}); without a deadline {sdk_untimed}"
    );
    println!("  encode                 blocks={:>3} bytes={:>6}", encode.blocks, encode.bytes);
    println!(
        "  header map clone       blocks={:>3} bytes={:>6}",
        header_clone.blocks, header_clone.bytes
    );
    println!("  decode                 blocks={:>3} bytes={:>6}", decode.blocks, decode.bytes);
    let itemized = i128::from(encode.blocks + header_clone.blocks + decode.blocks);
    println!(
        "  the rest               blocks={:>3} (request assembly, frame queue, deadline)",
        i128::from(sdk_blocks) - itemized
    );

    assert!(
        sdk_blocks <= MAX_BLOCKS,
        "a call costs {sdk_blocks} blocks of its own, over the budget of {MAX_BLOCKS}"
    );
}
