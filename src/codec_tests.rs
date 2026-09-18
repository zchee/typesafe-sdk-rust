//! Tests for the codec.
#![expect(
    dead_code,
    reason = "the fixture types exist to be decoded into; their fields are never read back"
)]

use std::{
    panic::{self, AssertUnwindSafe},
    thread,
};

use proptest::prelude::*;
use serde::{Deserialize, de::IgnoredAny};

use super::*;

// ------------------------------------------------------------- fixtures

/// A response-shaped target, cut down to what the field-path tests need.
#[derive(Debug, Deserialize)]
struct Envelope {
    answers: Answers,
}

#[derive(Debug, Deserialize)]
struct Answers {
    spam: Noul,
    tone: Tone,
}

#[derive(Debug, Deserialize)]
struct Noul {
    noul: f64,
}

#[derive(Debug, Deserialize)]
struct Tone {
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct ModelList {
    models: Vec<Model>,
}

#[derive(Debug, Deserialize)]
struct Model {
    name: String,
}

/// A target that ignores everything but one field, so that the value of the
/// other field goes through the codec's skip path.
#[derive(Debug, Deserialize)]
struct Known {
    known: u32,
}

/// `[[[ ... ]]]`, `depth` levels deep.
fn nested_array(depth: usize) -> String {
    let mut text = String::with_capacity(depth * 2);
    text.push_str(&"[".repeat(depth));
    text.push_str(&"]".repeat(depth));
    text
}

/// Runs `body` on a thread with the 2 MiB stack a Tokio worker gets.
///
/// The depth tests have to hold on the stack the SDK actually runs on, and in
/// an unoptimized build the codec's skip path costs tens of kilobytes per
/// nesting level. A test that only passed on the main thread, which has 8 MiB,
/// would not prove anything about the worker threads.
fn on_worker_stack<F, T>(body: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(body)
        .expect("invariant: the test thread can be spawned")
        .join()
        .expect("the decode must finish on a 2 MiB stack instead of overflowing it")
}

// ----------------------------------------------------------- depth guard

#[test]
fn depth_at_the_limit_is_accepted_and_one_more_is_not() {
    let at_limit = nested_array(MAX_JSON_DEPTH);
    let over_limit = nested_array(MAX_JSON_DEPTH + 1);

    check_depth(at_limit.as_bytes()).expect("a document at the limit is accepted");
    decode::<IgnoredAny>(at_limit.as_bytes()).expect("a document at the limit decodes");

    let error = check_depth(over_limit.as_bytes()).expect_err("one level deeper is rejected");
    assert_eq!(error.kind(), DecodeErrorKind::TooDeep, "kind of {error}");
    assert_eq!(
        decode::<IgnoredAny>(over_limit.as_bytes()).expect_err("decoding rejects it too").kind(),
        DecodeErrorKind::TooDeep
    );
}

#[test]
fn a_hundred_thousand_levels_are_rejected_before_the_parser_sees_them() {
    // The parser has no recursion limit and aborts the process on input like
    // this, so reaching the assertion at all is the result being tested.
    let bare = nested_array(100_000);
    let in_a_field = format!(r#"{{"legend":{bare}}}"#);

    let kinds = on_worker_stack(move || {
        [
            decode::<IgnoredAny>(bare.as_bytes())
                .expect_err("a 100,000 level array is rejected")
                .kind(),
            decode::<IgnoredAny>(in_a_field.as_bytes())
                .expect_err("the same nested in an object field is rejected")
                .kind(),
        ]
    });

    assert_eq!(kinds, [DecodeErrorKind::TooDeep, DecodeErrorKind::TooDeep]);
}

#[test]
fn the_skip_path_holds_at_the_depth_limit_on_a_worker_stack() {
    // Depth 16 counting the enclosing object, so the ignored value is 15 deep.
    let document = format!(r#"{{"unknown":{},"known":7}}"#, nested_array(MAX_JSON_DEPTH - 1));

    let known = on_worker_stack(move || {
        decode::<Known>(document.as_bytes())
            .expect("an unknown field at the depth limit is skipped")
            .known
    });

    assert_eq!(known, 7);
}

#[test]
fn brackets_and_escapes_inside_strings_do_not_count_as_nesting() {
    let brackets = "[".repeat(MAX_JSON_DEPTH * 4) + &"{".repeat(MAX_JSON_DEPTH * 4);
    // `\"` ends no string and `\\` escapes only itself, so the `[` after
    // either of them is still inside the string value.
    let document = format!(r#"{{"a":"{brackets}","b":"\"[[[[","c":"\\","d":"\\\"[[[["}}"#);

    check_depth(document.as_bytes()).expect("only the enclosing object nests");

    let decoded = decode::<serde_json::Value>(document.as_bytes())
        .expect("the document is valid JSON at depth 2");
    assert_eq!(decoded["b"], "\"[[[[", "the escaped quote is part of the value: {decoded}");
    assert_eq!(decoded["c"], "\\", "the escaped backslash decodes to one");
    assert_eq!(decoded["d"], "\\\"[[[[");
}

#[test]
fn an_unterminated_string_cannot_hide_a_bracket_run() {
    // Everything after the opening quote is string content, so the run of
    // brackets never counts and the pre-scan hands the input to the parser,
    // which rejects it as truncated.
    let document = format!(r#"{{"a":"{}"#, "[".repeat(MAX_JSON_DEPTH * 10));

    check_depth(document.as_bytes()).expect("nothing inside the string nests");
    let error = decode::<IgnoredAny>(document.as_bytes()).expect_err("the input is truncated");
    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "kind of {error}");
}

// ------------------------------------------------------------ field paths

#[test]
fn a_missing_field_is_reported_at_its_own_path() {
    let document = br#"{"answers":{"spam":{},"tone":{"confidence":0.9}}}"#;

    let error = decode::<Envelope>(document).expect_err("`noul` is missing");

    assert_eq!(error.kind(), DecodeErrorKind::Data, "kind of {error}");
    assert_eq!(error.path(), "answers.spam.noul");
    assert!(error.line() > 0, "the position is kept: {error:?}");
}

#[test]
fn a_wrongly_typed_field_is_reported_at_its_own_path() {
    let document = br#"{"answers":{"spam":{"noul":0.98},"tone":{"confidence":"high"}}}"#;

    let error = decode::<Envelope>(document).expect_err("`confidence` is not a number");

    assert_eq!(error.kind(), DecodeErrorKind::Data, "kind of {error}");
    assert_eq!(error.path(), "answers.tone.confidence");
}

#[test]
fn a_sequence_element_is_reported_with_its_index() {
    let document = br#"{"models":[{"name":"jev-latest"},{"name":123}]}"#;

    let error = decode::<ModelList>(document).expect_err("the second name is a number");

    assert_eq!(error.kind(), DecodeErrorKind::Data, "kind of {error}");
    assert_eq!(error.path(), "models[1].name");
}

#[test]
fn a_decode_error_never_carries_the_input() {
    // A value the caller would not want in a log line. The `state` of a real
    // call is application data, and both the codec's own error text and
    // serde's type-mismatch messages quote what they were given.
    const SECRET: &str = "pentachlorophenol-42";

    let wrong_type = format!(r#"{{"answers":{{"spam":{{"noul":"{SECRET}"}}}}}}"#);
    let truncated = format!(r#"{{"answers":{{"spam":{{"noul":"{SECRET}"#);
    let deep = format!("{}{SECRET}", "[".repeat(MAX_JSON_DEPTH + 1));

    for document in [wrong_type, truncated, deep] {
        let error = decode::<Envelope>(document.as_bytes())
            .expect_err("every one of these documents fails");
        let rendered = error.to_string();
        let debugged = format!("{error:?}");

        assert!(!rendered.contains(SECRET), "Display leaked the input: {rendered}");
        assert!(!debugged.contains(SECRET), "Debug leaked the input: {debugged}");
    }
}

#[test]
fn a_syntax_error_keeps_its_position() {
    let error = decode::<Envelope>(br#"{"answers":}"#).expect_err("the value is missing");

    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "kind of {error}");
    assert_eq!(error.line(), 1);
    assert!(error.column() > 0, "the column is kept: {error:?}");
    assert_eq!(error.path(), "", "a syntax error has no field path");
}

// --------------------------------------------------------------- encoding

#[test]
fn encode_into_appends_instead_of_clearing() {
    let mut buffer = Vec::from(&br#"{"state":"#[..]);

    encode_into(&mut buffer, "a \"quoted\" state").expect("a string always encodes");
    buffer.extend_from_slice(br#","model":"#);
    write_json_string(&mut buffer, "jev-latest");
    buffer.push(b'}');

    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        r#"{"state":"a \"quoted\" state","model":"jev-latest"}"#
    );
}

#[test]
fn encode_body_returns_exactly_the_bytes_the_closure_wrote() {
    reset_scratch();

    let body = encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"state":"#);
        encode_into(buffer, "hello")?;
        buffer.extend_from_slice(br#","model":"jev-latest"}"#);
        Ok(())
    })
    .expect("the closure succeeds");

    assert_eq!(&body[..], br#"{"state":"hello","model":"jev-latest"}"#);
}

#[test]
fn encode_body_survives_a_nested_call() {
    reset_scratch();

    let outer = encode_body(|buffer| {
        let inner = encode_body(|nested| {
            nested.extend_from_slice(br#"{"inner":true}"#);
            Ok(())
        })?;
        buffer.extend_from_slice(br#"{"outer":"#);
        buffer.extend_from_slice(&inner);
        buffer.push(b'}');
        Ok(())
    })
    .expect("the nested call gets a buffer of its own");

    assert_eq!(&outer[..], br#"{"outer":{"inner":true}}"#);
}

#[test]
fn a_panicking_closure_does_not_poison_later_calls() {
    reset_scratch();

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        encode_body(|buffer| {
            buffer.extend_from_slice(b"partial");
            panic!("the caller's Serialize implementation gave up");
        })
    }));
    panic::set_hook(previous_hook);

    assert!(outcome.is_err(), "the panic propagates to the caller");

    let body = encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"after":"the panic"}"#);
        Ok(())
    })
    .expect("the next call gets a fresh buffer");
    assert_eq!(&body[..], br#"{"after":"the panic"}"#);
}

#[test]
fn a_repeated_body_reuses_the_retained_scratch() {
    reset_scratch();

    let state = "s".repeat(1024);
    for _ in 0..4 {
        let body = encode_body(|buffer| encode_into(buffer, state.as_str())).expect("encodes");
        assert_eq!(body.len(), state.len() + 2);
    }

    let capacity = scratch_capacity();
    let hint = scratch_hint();
    assert!(capacity >= state.len(), "the buffer is kept: {capacity}");
    assert!(
        capacity <= hint.saturating_mul(8),
        "retained {capacity} bytes against a hint of {hint}"
    );
}

#[test]
fn an_outlier_body_stops_pinning_the_scratch() {
    reset_scratch();

    let large = "L".repeat(1024 * 1024);
    let small = "s".repeat(1024);
    encode_body(|buffer| encode_into(buffer, large.as_str())).expect("encodes");
    for _ in 0..16 {
        encode_body(|buffer| encode_into(buffer, small.as_str())).expect("encodes");
    }

    let capacity = scratch_capacity();
    let hint = scratch_hint();
    assert!(
        capacity <= hint.saturating_mul(8),
        "after the outlier the scratch still holds {capacity} bytes against a hint of {hint}"
    );
    assert!(capacity < large.len(), "the megabyte buffer is gone: {capacity}");
}

// ------------------------------------------------------------- raw JSON

#[derive(Debug, Deserialize)]
struct Holder {
    raw: RawJson,
}

#[test]
fn raw_json_keeps_the_text_it_was_given() {
    // A `\u00e9` escape, an escaped quote, an escaped backslash, a line
    // separator and a negative zero: the codec may rewrite none of them.
    let document = r#"{"raw":{"text":"caf\u00e9 \"q\" \\ \u2028","list":[1,2,3],"n":-0.0}}"#;

    let holder: Holder = decode(document.as_bytes()).expect("the document decodes");
    assert_eq!(
        holder.raw.as_str(),
        r#"{"text":"caf\u00e9 \"q\" \\ \u2028","list":[1,2,3],"n":-0.0}"#
    );

    let mut buffer = Vec::new();
    encode_into(&mut buffer, &holder.raw).expect("raw text re-encodes");
    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        holder.raw.as_str(),
        "re-encoding must splice the text in rather than re-render it"
    );
}

#[test]
fn raw_json_round_trips_through_a_value() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Point {
        x: i32,
        label: String,
    }
    let point = Point { x: -3, label: "caf\u{e9}\u{2028}\u{1f600}".to_owned() };

    let raw = RawJson::from_value(&point).expect("the value encodes");
    assert_eq!(raw.deserialize::<Point>().expect("the text decodes"), point, "raw text {raw}");
    assert_eq!(raw, RawJson::from_value(&point).expect("encodes again"));
}

#[test]
fn raw_json_applies_the_depth_limit_when_it_is_read_back() {
    let mut value = serde_json::Value::from(1);
    for _ in 0..=MAX_JSON_DEPTH {
        value = serde_json::Value::Array(vec![value]);
    }

    let raw = RawJson::from_value(&value).expect("encoding is not depth limited");
    let error = raw.deserialize::<IgnoredAny>().expect_err("reading it back is");
    assert_eq!(error.kind(), DecodeErrorKind::TooDeep, "kind of {error}");
}

// ------------------------------------------------- differential behaviour

#[test]
fn a_negative_zero_loses_its_sign() {
    // An accepted divergence of this codec, asserted so that it is a recorded
    // property rather than a surprise: a literal negative zero decodes as a
    // positive one. A zero reached by underflow keeps its sign.
    let ours: f64 = decode(b"-0.0").expect("it parses");
    let reference: f64 = serde_json::from_slice(b"-0.0").expect("it parses there too");

    assert_eq!(ours.to_bits(), 0.0_f64.to_bits());
    assert_eq!(reference.to_bits(), (-0.0_f64).to_bits());

    let underflow: f64 = decode(b"-1e-400").expect("it parses");
    assert_eq!(underflow.to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn awkward_characters_encode_to_text_another_codec_reads_back() {
    let samples = [
        "\u{0}\u{1}\u{1f}",
        "\u{7f}",
        "\u{a0}",
        "\u{2028}\u{2029}",
        "\u{1f600}",
        "\"\\/",
        "caf\u{e9} \u{20ac} \u{2014}",
    ];

    for sample in samples {
        let mut buffer = Vec::new();
        encode_into(&mut buffer, sample).expect("a string always encodes");
        let reparsed: String = serde_json::from_slice(&buffer)
            .unwrap_or_else(|error| panic!("{error} for {buffer:?}"));
        assert_eq!(reparsed, sample, "encoded as {:?}", String::from_utf8_lossy(&buffer));
    }
}

proptest! {
    /// The encode side of the codec differential: whatever this codec writes
    /// for a string, an independent parser must read back unchanged.
    #[test]
    fn encoded_strings_reparse_identically(
        text in proptest::collection::vec(any::<char>(), 0..48).prop_map(String::from_iter)
    ) {
        let mut buffer = Vec::new();
        encode_into(&mut buffer, text.as_str()).expect("a string always encodes");
        let reparsed: String = serde_json::from_slice(&buffer)
            .unwrap_or_else(|error| panic!("{error} for {buffer:?}"));
        prop_assert_eq!(reparsed, text);
    }
}
