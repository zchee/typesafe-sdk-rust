//! T0.6: the itemized allocation inventory AC-P0 asks for.
//!
//! One step of plan section 3.3 per scenario, one scenario per process run,
//! every number taken on the second identical call after one warm-up of the
//! same shape.

mod answers;

use std::{env, error::Error, process::ExitCode};

use answers::{AskedFor, NaiveResponse, Response, TypedResponse};
use bytes::Bytes;
use encode_buffer::{State, Variant};
use http::{
    HeaderMap, HeaderValue, Method, Request, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, USER_AGENT},
};
use http_body_util::{BodyExt as _, Full};

// A plain wrapper type, so declaring it as the global allocator stays safe code.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The upstream `RESULT` fixture (`tests/test_clients.py:42-56`), compact.
const RESULT: &[u8] = br#"{"model":"jev-latest","usage":{"input_tokens":12,"output_tokens":3},"answers":{"spam":{"type":"noul","noul":0.98},"tone":{"type":"choice","choice":"friendly","confidence":0.9,"probabilities":{"friendly":0.9,"hostile":0.1}},"quality":{"type":"score","score":1.7,"confidence":0.8,"legend":{"0":"bad","1":"ok","2":"great"},"probabilities":{"0":0.1,"1":0.1,"2":0.8}}}}"#;

/// The same answers with the fields of every object in a different order, the
/// answers themselves in a different order, and one answer of a type this
/// version does not know.
const RESULT_REORDERED: &[u8] = br#"{"answers":{"quality":{"legend":{"2":"great","0":"bad","1":"ok"},"probabilities":{"2":0.8,"0":0.1,"1":0.1},"confidence":0.8,"score":1.7,"type":"score"},"future":{"horizon":"2027","type":"prediction"},"tone":{"probabilities":{"hostile":0.1,"friendly":0.9},"confidence":0.9,"choice":"friendly","type":"choice"},"spam":{"noul":0.98,"type":"noul"}},"usage":{"output_tokens":3,"input_tokens":12},"model":"jev-latest"}"#;

/// The number of questions asked, which is what sizes the answers vector.
const ASKED_FOR: usize = 3;

const SDK_HEADER: HeaderName = HeaderName::from_static("x-typesafe-sdk");
const RUNTIME_HEADER: HeaderName = HeaderName::from_static("x-typesafe-runtime");

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let scenario = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    match scenario.as_str() {
        "steps" => scenario_steps()?,
        "decode" => scenario_decode(),
        "verify" => scenario_verify()?,
        other => {
            eprintln!("unknown scenario {other:?}");
            eprintln!("scenarios: steps decode verify");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The change in dhat's counters across one section.
#[derive(Clone, Copy)]
struct Measured {
    blocks: u64,
    bytes: u64,
}

impl Measured {
    fn around<F, T>(body: F) -> (Self, T)
    where
        F: FnOnce() -> T,
    {
        let before = dhat::HeapStats::get();
        let value = body();
        let after = dhat::HeapStats::get();
        (
            Self {
                blocks: after.total_blocks - before.total_blocks,
                bytes: after.total_bytes - before.total_bytes,
            },
            value,
        )
    }
}

fn row(step: &str, what: &str, measured: Measured) {
    println!("{step:<6} {what:<46} {:>7} {:>9}", measured.blocks, measured.bytes);
}

// ----------------------------------------------------- steps 1 to 5

fn scenario_steps() -> Result<(), Box<dyn Error>> {
    println!("# AC-P0 steps 1-5, second identical call");
    println!("{:<6} {:<46} {:>7} {:>9}", "step", "item", "blocks", "bytes");

    let state = State::parse("s1k").ok_or("state s1k")?;
    // The S6 winner: the retained thread-local scratch with a one-shot shrink.
    let variant = Variant::RetainedScratchOneShot;
    let uri: Uri = "https://api.typesafe.ai/v1/systemone".parse()?;
    let base = base_headers()?;

    let profiler = dhat::Profiler::builder().testing().build();

    // Warm-up: one identical pass through every step.
    {
        let body = variant.encode(&state)?;
        let retained = body.clone();
        let headers = base.clone();
        let request = build_request(&uri, headers, body.clone());
        drop((retained, request));
        let collected = Full::new(body).collect();
        drop(futures_block_on(collected));
    }

    let (encode, body) = Measured::around(|| variant.encode(&state));
    let body = body?;
    row("1", "body encode, 1 KB string state (S6 winner)", encode);
    println!("       body_len={} retained_scratch={}", body.len(), variant.retained_capacity());

    let (retain, retained) = Measured::around(|| body.clone());
    row("2", "retain the body for retry (Bytes clone)", retain);

    let (header_clone, headers) = Measured::around(|| base.clone());
    row("3", "clone the base HeaderMap (6 headers)", header_clone);

    // The header map is moved in, not cloned again: cloning it here would
    // report step 3's cost a second time under step 4's name.
    let (assemble, request) = Measured::around(|| build_request(&uri, headers, body.clone()));
    row("4", "build http::Request from a pre-parsed Uri", assemble);
    println!("       (the Bytes clone of step 2 is included; the Uri clone is the rest)");

    let (uri_clone, cloned_uri) = Measured::around(|| uri.clone());
    row("4a", "clone the pre-parsed Uri, second time", uri_clone);
    drop(cloned_uri);

    // A Uri is backed by `Bytes`, so its FIRST clone allocates the shared
    // header once per Uri and every clone after that is free. The client parses
    // one Uri per endpoint at build time, so this is paid twice in a process,
    // not per request; it is measured here so the budget can say so.
    let fresh: Uri = String::from("https://api.typesafe.ai/v1/models").parse()?;
    let (first_clone, cloned) = Measured::around(|| fresh.clone());
    row("4b", "clone a freshly parsed Uri, FIRST time", first_clone);
    let (second_clone, cloned_again) = Measured::around(|| fresh.clone());
    row("4c", "clone that same Uri, second time", second_clone);
    drop((cloned, cloned_again, fresh));

    let response_body = Full::new(Bytes::from_static(RESULT));
    let (collect, collected) = Measured::around(|| futures_block_on(response_body.collect()));
    let collected = collected?.to_bytes();
    row("5", "BodyExt::collect + to_bytes, single frame", collect);

    drop((retained, request, collected, body));
    drop(profiler);
    Ok(())
}

/// The base header map a client builds once and clones per attempt.
fn base_headers() -> Result<HeaderMap, Box<dyn Error>> {
    let mut headers = HeaderMap::with_capacity(6);
    // A placeholder, not a credential: this spike never reads a key.
    let mut authorization = HeaderValue::from_static("Bearer ts_placeholder_not_a_real_key");
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("typesafe-sdk-rust/0.1.0"));
    headers.insert(SDK_HEADER, HeaderValue::from_static("typesafe-sdk-rust/0.1.0"));
    headers.insert(RUNTIME_HEADER, HeaderValue::from_static("rust (macos; aarch64)"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

fn build_request(uri: &Uri, headers: HeaderMap, body: Bytes) -> Request<Full<Bytes>> {
    let mut request = Request::new(Full::new(body));
    *request.method_mut() = Method::POST;
    *request.uri_mut() = uri.clone();
    *request.headers_mut() = headers;
    request
}

/// Drives a future to completion on the current thread.
///
/// `BodyExt::collect` on a `Full` body is ready at its first poll, so a real
/// runtime would only add its own allocations to the measurement.
fn futures_block_on<F: Future>(future: F) -> F::Output {
    use std::{
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
}

// ----------------------------------------------------- steps 6 to 8

fn scenario_decode() {
    println!("# AC-P0 steps 6-8, decoding the RESULT fixture, second identical call");
    println!("{:<6} {:<46} {:>7} {:>9}", "step", "item", "blocks", "bytes");

    let profiler = dhat::Profiler::builder().testing().build();

    // Warm-up of all three, so no first-touch cost lands on a measured row.
    drop(decode_prototype());
    drop(decode_naive());
    drop(decode_typed());

    let (prototype, value) = Measured::around(decode_prototype);
    row("6", "prototype visitor -> Vec<(name, Answer)>", prototype);
    drop(value);

    let (naive, value) = Measured::around(decode_naive);
    row("7", "naive: #[serde(tag)] enum in HashMap", naive);
    drop(value);

    let (typed, value) = Measured::around(decode_typed);
    row("8", "derived typed struct (QuestionSet shape)", typed);
    drop(value);

    drop(profiler);

    println!();
    println!("# the plan's budget questions");
    // C is 0 blocks and 0 bytes, measured in S1(c).
    println!(
        "(6) <= 14 + C blocks, with C = 0: {} <= 14 -> {}",
        prototype.blocks,
        prototype.blocks <= 14
    );
    let ceiling = 0.7 * naive.blocks as f64;
    println!(
        "(6) <= 0.7 x (7): {} <= {:.1} -> {}",
        prototype.blocks,
        ceiling,
        prototype.blocks as f64 <= ceiling
    );
    println!(
        "(8) < (6): {} < {} -> {}",
        typed.blocks,
        prototype.blocks,
        typed.blocks < prototype.blocks
    );
}

fn decode_prototype() -> Response {
    let mut deserializer = sonic_rs::Deserializer::from_slice(RESULT);
    let value = serde::de::DeserializeSeed::deserialize(AskedFor(ASKED_FOR), &mut deserializer)
        .expect("the fixture matches the prototype representation");
    std::hint::black_box(value)
}

fn decode_naive() -> NaiveResponse {
    let value = sonic_rs::from_slice::<NaiveResponse>(RESULT)
        .expect("the fixture matches the naive representation");
    std::hint::black_box(value)
}

fn decode_typed() -> TypedResponse {
    let value = sonic_rs::from_slice::<TypedResponse>(RESULT)
        .expect("the fixture matches the derived representation");
    std::hint::black_box(value)
}

// ------------------------------------------------------- correctness

/// The prototype visitor has to produce the same value as serde_json does for
/// the same document, and it has to be indifferent to field order and tolerant
/// of an answer type it does not know.
fn scenario_verify() -> Result<(), Box<dyn Error>> {
    println!("# the prototype visitor against serde_json, same fixture");

    let sonic_ordered = decode_with_sonic(RESULT)?;
    let json_ordered = decode_with_serde_json(RESULT)?;
    println!("wire order:     sonic == serde_json -> {}", sonic_ordered == json_ordered);

    let sonic_reordered = decode_with_sonic(RESULT_REORDERED)?;
    let json_reordered = decode_with_serde_json(RESULT_REORDERED)?;
    println!("shuffled order: sonic == serde_json -> {}", sonic_reordered == json_reordered);

    // Every container keeps wire order by design, so a document whose keys
    // arrived in another order yields the same pairs in another order. The
    // comparison normalizes before asking whether the CONTENT is the same.
    println!(
        "shuffled order yields the same answers, compared as sets -> {}",
        normalized(&sonic_ordered) == normalized(&sonic_reordered)
    );
    println!(
        "shuffled order yields the same answers, compared in order -> {}",
        sonic_ordered.answers.0 == sonic_reordered.answers.0
    );
    println!(
        "model and usage unchanged -> {}",
        sonic_ordered.model == sonic_reordered.model
            && sonic_ordered.usage == sonic_reordered.usage
    );

    let names: Vec<&str> =
        sonic_reordered.answers.0.iter().map(|(name, _)| name.as_str()).collect();
    println!("answers kept from the shuffled document = {names:?}");
    println!("the unknown \"future\" answer was dropped -> {}", !names.contains(&"future"));
    println!("answer count = {} (asked for {ASKED_FOR})", sonic_reordered.answers.0.len());

    println!();
    println!("{:#?}", sonic_ordered.answers);
    Ok(())
}

/// The same response with every container sorted, so that two decodes of the
/// same data in different wire orders can be compared for content.
fn normalized(response: &Response) -> Vec<(String, answers::Answer)> {
    use answers::Answer;
    let mut answers: Vec<(String, Answer)> = response
        .answers
        .0
        .iter()
        .map(|(name, answer)| {
            let mut answer = answer.clone();
            match &mut answer {
                Answer::Noul(_) => {}
                Answer::Choice(choice) => {
                    choice.probabilities.0.sort_by(|a, b| a.0.cmp(&b.0));
                }
                Answer::Score(score) => {
                    score.legend.0.sort_by_key(|entry| entry.0);
                    score.probabilities.0.sort_by_key(|entry| entry.0);
                }
            }
            (name.clone(), answer)
        })
        .collect();
    answers.sort_by(|left, right| left.0.cmp(&right.0));
    answers
}

fn decode_with_sonic(input: &[u8]) -> Result<Response, Box<dyn Error>> {
    let mut deserializer = sonic_rs::Deserializer::from_slice(input);
    Ok(serde::de::DeserializeSeed::deserialize(AskedFor(ASKED_FOR), &mut deserializer)?)
}

fn decode_with_serde_json(input: &[u8]) -> Result<Response, Box<dyn Error>> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    Ok(serde::de::DeserializeSeed::deserialize(AskedFor(ASKED_FOR), &mut deserializer)?)
}
