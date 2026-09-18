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
    assert_eq!(raw.decode::<Point>().expect("the text decodes"), point, "raw text {raw}");
    assert_eq!(raw, RawJson::from_value(&point).expect("encodes again"));
}

#[test]
fn raw_json_applies_the_depth_limit_when_it_is_read_back() {
    let mut value = serde_json::Value::from(1);
    for _ in 0..=MAX_JSON_DEPTH {
        value = serde_json::Value::Array(vec![value]);
    }

    let raw = RawJson::from_value(&value).expect("encoding is not depth limited");
    let error = raw.decode::<IgnoredAny>().expect_err("reading it back is");
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

// ------------------------------------------------- raw JSON, other codecs

/// A response-shaped value with a raw field, which is the shape a user gets
/// when they serialize a response with a codec of their own.
#[derive(Debug, Deserialize, Serialize)]
struct Envelope2 {
    name: String,
    raw: RawJson,
}

/// The four numbers the transcode has to carry without changing their value:
/// an exponent far from 1, a decimal fraction no binary float holds exactly,
/// and both integer extremes, which do not fit in an `f64`.
#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Numbers {
    tiny: f64,
    tenth: f64,
    largest: u64,
    smallest: i64,
}

const NUMBERS: &str =
    r#"{"tiny":1e-7,"tenth":0.1,"largest":18446744073709551615,"smallest":-9223372036854775808}"#;

/// Encodes with this crate's codec, which is the serializer the SDK uses.
fn sdk_encoded<T: Serialize + ?Sized>(value: &T) -> String {
    let mut buffer = Vec::new();
    encode_into(&mut buffer, value).expect("the value encodes");
    String::from_utf8(buffer).expect("the codec emits UTF-8")
}

#[test]
fn another_codec_gets_json_data_and_never_the_splice_token() {
    let document = br#"{"name":"legend","raw":{"b":[1,{"c":"caf\u00e9"}],"a":null}}"#;
    let envelope: Envelope2 = decode(document).expect("the document decodes");

    let foreign = serde_json::to_string(&envelope).expect("another codec writes it as data");

    assert!(
        !foreign.contains("$sonic_rs"),
        "a private protocol of this codec reached another one: {foreign}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&foreign).expect("the output is JSON"),
        serde_json::json!({"name": "legend", "raw": {"b": [1, {"c": "caf\u{e9}"}], "a": null}}),
        "transcoded as {foreign}"
    );
}

#[test]
fn the_same_value_splices_verbatim_through_this_codec() {
    // The exact bytes, spacing and key order of the raw field survive, which
    // is what the other codec is allowed to re-render and this one is not.
    let document = br#"{"name":"legend","raw":{ "b":[1, 2],"a":-0.0 }}"#;
    let envelope: Envelope2 = decode(document).expect("the document decodes");

    assert_eq!(sdk_encoded(&envelope), r#"{"name":"legend","raw":{ "b":[1, 2],"a":-0.0 }}"#);
}

#[test]
fn a_raw_value_round_trips_through_another_codec_with_the_same_data() {
    for text in [
        r#"{"label":"ok","weight":2,"tags":["a","b"],"none":null,"flag":true}"#,
        r#"[1,[2,[3,[]]],{"k":"v"}]"#,
        r#""a \"quoted\" caf\u00e9""#,
        "17",
    ] {
        let original = RawJson::from_text(text.to_owned());

        let written = serde_json::to_string(&original).expect("it writes as data");
        let read_back: RawJson = serde_json::from_str(&written).expect("it reads back");

        assert!(!written.contains("$sonic_rs"), "token leaked for {text}: {written}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(read_back.as_str())
                .expect("the text is JSON"),
            serde_json::from_str::<serde_json::Value>(text).expect("so is the original"),
            "{text} came back as {read_back}"
        );
    }
}

#[test]
fn numbers_keep_their_value_across_the_transcode() {
    let raw = RawJson::from_text(NUMBERS.to_owned());
    let expected = Numbers { tiny: 1e-7, tenth: 0.1, largest: u64::MAX, smallest: i64::MIN };

    let written = serde_json::to_string(&raw).expect("it writes as data");
    let decoded: Numbers = serde_json::from_str(&written).expect("the numbers read back");

    assert_eq!(decoded.tiny.to_bits(), expected.tiny.to_bits(), "written as {written}");
    assert_eq!(decoded.tenth.to_bits(), expected.tenth.to_bits(), "written as {written}");
    assert_eq!(decoded.largest, u64::MAX, "written as {written}");
    assert_eq!(decoded.smallest, i64::MIN, "written as {written}");

    // The same value through this codec is not re-rendered at all.
    assert_eq!(sdk_encoded(&raw), NUMBERS);
}

#[test]
fn a_negative_zero_loses_its_sign_on_the_transcode_path_only() {
    // The recorded divergence of this codec, now reachable one step further
    // out: the transcode reads the number with this parser, so a literal
    // negative zero comes out positive. The splice never reads it.
    let raw = RawJson::from_text("-0.0".to_owned());

    assert_eq!(serde_json::to_string(&raw).expect("it writes as data"), "0.0");
    assert_eq!(sdk_encoded(&raw), "-0.0");
}

#[test]
fn a_value_too_deep_to_transcode_is_refused_rather_than_aborting() {
    let at_limit = RawJson::from_text(nested_array(MAX_JSON_DEPTH));
    let over_limit = RawJson::from_text(nested_array(MAX_JSON_DEPTH + 1));
    // Far past any stack: the splice path must not read this, and the
    // transcode path must reject it on the byte scan rather than recurse.
    let absurd = RawJson::from_text(nested_array(100_000));

    let outcomes = on_worker_stack(move || {
        (
            serde_json::to_string(&at_limit).expect("the limit itself transcodes"),
            serde_json::to_string(&over_limit).map(|_| ()).map_err(|error| error.to_string()),
            serde_json::to_string(&absurd).map(|_| ()).map_err(|error| error.to_string()),
            sdk_encoded(&absurd),
        )
    });

    assert_eq!(outcomes.0, nested_array(MAX_JSON_DEPTH));
    assert!(outcomes.1.is_err(), "one level past the cap must be refused: {outcomes:?}");
    assert!(outcomes.2.is_err(), "100,000 levels must be refused: {outcomes:?}");
    assert_eq!(outcomes.3, nested_array(100_000), "the splice never reads the text");
}

#[test]
fn a_document_too_deep_to_render_is_refused_by_the_other_codec_path() {
    // Under the other codec's own recursion limit, so the cap that bites here
    // is this crate's.
    let document = nested_array(MAX_JSON_DEPTH + 1);

    let error = serde_json::from_str::<RawJson>(&document).expect_err("the document is too deep");

    assert!(
        error.to_string().contains("nested deeper"),
        "the depth cap has to be the reason: {error}"
    );
    assert_eq!(
        serde_json::from_str::<RawJson>(&nested_array(MAX_JSON_DEPTH))
            .expect("the limit itself is fine")
            .as_str(),
        nested_array(MAX_JSON_DEPTH)
    );
}

/// A writer that gives up part way, so that an error raised by the other
/// codec has to travel back out through the parser driving the transcode.
struct ShortWriter {
    remaining: usize,
}

impl std::io::Write for ShortWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(std::io::Error::other("the sink is full"));
        }
        let taken = buf.len().min(self.remaining);
        self.remaining -= taken;
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failure_in_the_other_codec_is_reported_and_carries_no_input() {
    const SECRET: &str = "pentachlorophenol-42";
    let raw = RawJson::from_text(format!(r#"{{"nested":[{{"state":"{SECRET}"}}]}}"#));

    let error = serde_json::to_writer(ShortWriter { remaining: 4 }, &raw)
        .expect_err("the sink refuses the value");

    let rendered = error.to_string();
    assert!(rendered.contains("the sink is full"), "the writer's reason is kept: {rendered}");
    assert!(!rendered.contains(SECRET), "the failure leaked the text: {rendered}");
}

#[test]
fn a_wrapper_around_this_codec_forwards_the_splice() {
    // `serde_path_to_error::Serializer` is not this crate's serializer, but it
    // passes every call through to the one it wraps. Whether a splice happens
    // is decided by the encoder that is running, not by the type in hand, so
    // the raw text reaches this codec and is written out unchanged.
    let raw = RawJson::from_text(r#"{ "b":[1, 2],"a":-0.0 }"#.to_owned());

    let mut track = serde_path_to_error::Track::new();
    let mut buffer = Vec::new();
    {
        let _inside = EncoderMark::enter();
        let mut codec = sonic_rs::Serializer::new(&mut buffer);
        raw.serialize(serde_path_to_error::Serializer::new(&mut codec, &mut track))
            .expect("the wrapper forwards the splice");
    }
    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        r#"{ "b":[1, 2],"a":-0.0 }"#
    );

    // The same wrapper outside the SDK's encoder gets data instead, which is
    // what a caller assembling a serializer of their own sees.
    let mut track = serde_path_to_error::Track::new();
    let mut buffer = Vec::new();
    let mut codec = sonic_rs::Serializer::new(&mut buffer);
    raw.serialize(serde_path_to_error::Serializer::new(&mut codec, &mut track))
        .expect("outside the encoder it transcodes");
    assert_eq!(String::from_utf8(buffer).expect("the codec emits UTF-8"), r#"{"b":[1,2],"a":0.0}"#);
}

#[test]
fn a_deserializer_that_ignores_the_request_still_yields_the_value() {
    // These forward every request to `deserialize_any`, so the value arrives
    // at the visitor directly instead of through the newtype struct. It is the
    // route a non-self-describing format takes.
    use serde::de::{
        IntoDeserializer,
        value::{Error as ValueError, MapDeserializer, SeqDeserializer, U64Deserializer},
    };

    // Spelled through the trait: the inherent `RawJson::deserialize` would
    // shadow it.
    let number = <RawJson as Deserialize>::deserialize(U64Deserializer::<ValueError>::new(
        9_007_199_254_740_993,
    ))
    .expect("a bare number arrives as data");
    assert_eq!(number.as_str(), "9007199254740993");

    let sequence = <RawJson as Deserialize>::deserialize(SeqDeserializer::<_, ValueError>::new(
        [1_u64, 2, 3].into_iter(),
    ))
    .expect("a bare sequence arrives as data");
    assert_eq!(sequence.as_str(), "[1,2,3]");

    let map = <RawJson as Deserialize>::deserialize(MapDeserializer::<_, ValueError>::new(
        [("we\"ird", 1_u64), ("b", 2)].into_iter().map(|(k, v)| (k, v.into_deserializer())),
    ))
    .expect("a bare map arrives as data");
    assert_eq!(map.as_str(), r#"{"we\"ird":1,"b":2}"#);
}

// --------------------------------------------- the one-JSON-value invariant

/// Reads one raw capture, which is what a response type does for a field it
/// keeps as text.
#[derive(Debug, Deserialize)]
struct RawRow<'a> {
    #[serde(borrow, deserialize_with = "deserialize_raw")]
    value: Cow<'a, str>,
}

#[test]
fn text_that_is_not_one_json_value_never_becomes_raw_json() {
    use serde::de::value::{BorrowedStrDeserializer, Error as ValueError, StrDeserializer};

    // The first of these is the dangerous one: spliced into a request body it
    // would add a key of the caller's choosing to the enclosing object. The
    // others would make the body unparseable. The refusal that reaches the
    // caller is a `serde` error carrying the rendered `DecodeError`, so the
    // kind is asserted through what it renders as.
    for (text, expected) in [
        (r#"{"a":1}, "model": "evil""#, "invalid JSON syntax at line 1 column 8"),
        ("{not json", "invalid JSON syntax at line 1 column 2"),
        ("hello", "invalid JSON syntax at line 1 column 1"),
        ("", "invalid JSON syntax at line 1 column 1"),
        ("  ", "invalid JSON syntax at line 1 column 2"),
    ] {
        let borrowed =
            <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(text));
        let owned = <RawJson as Deserialize>::deserialize(StrDeserializer::<ValueError>::new(text));

        let error = borrowed.expect_err(text).to_string();
        assert!(error.contains(expected), "{text:?} was refused as {error}");
        assert!(
            owned.expect_err(text).to_string().contains(expected),
            "the owned route refused {text:?} differently"
        );
        assert!(!error.contains("evil"), "the refusal quoted the input: {error}");
        assert!(!error.contains("not json"), "the refusal quoted the input: {error}");
    }

    // A string that does hold exactly one value is taken at face value, which
    // is the raw-text protocol this codec relies on.
    let accepted = <RawJson as Deserialize>::deserialize(
        BorrowedStrDeserializer::<ValueError>::new(r#"{"a":1}"#),
    )
    .expect("one complete value is raw JSON text");
    assert_eq!(accepted.as_str(), r#"{"a":1}"#);
}

#[test]
fn a_raw_capture_past_the_depth_cap_is_refused() {
    use serde::de::value::{BorrowedStrDeserializer, Error as ValueError};

    let deep = nested_array(MAX_JSON_DEPTH + 1);
    let at_limit = nested_array(MAX_JSON_DEPTH);

    let error =
        <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(&deep))
            .expect_err("one level past the cap is refused");
    assert!(error.to_string().contains("nested deeper"), "{error}");

    assert_eq!(
        <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(
            &at_limit
        ))
        .expect("the limit itself is fine")
        .as_str(),
        at_limit
    );
}

#[test]
fn the_check_leaves_the_borrow_on_the_sdk_decode_path() {
    let document = br#"{"value":{"label":"ok","weight":2}}"#;

    let row: RawRow<'_> = decode(document).expect("the document decodes");

    assert!(
        matches!(row.value, Cow::Borrowed(_)),
        "the raw capture stopped borrowing the response buffer: {:?}",
        row.value
    );
    let captured = row.value.as_ptr().addr();
    let start = document.as_ptr().addr();
    assert!(
        (start..start + document.len()).contains(&captured),
        "the text must point into the response buffer, not a copy"
    );
    assert_eq!(&*row.value, r#"{"label":"ok","weight":2}"#);
}

// ------------------------------------------------------ non-finite floats

#[test]
fn a_non_finite_float_encodes_as_null_exactly_as_the_reference_codec_does() {
    // Pinned rather than asserted as desirable: this codec and `serde_json`
    // agree on `null`, the upstream Python SDK writes the bare words `NaN` and
    // `Infinity`, which are not JSON at all, and the plan says both should
    // refuse. The behaviour is recorded here so that a change to it is a test
    // failure rather than a surprise on the wire.
    #[derive(Debug, Serialize)]
    struct Scores {
        score: f64,
        ratio: f32,
    }

    for (value, expected) in [(f64::NAN, "null"), (f64::INFINITY, "null"), (-f64::INFINITY, "null")]
    {
        let ours = RawJson::from_value(&value).expect("a non-finite float encodes");
        assert_eq!(ours.as_str(), expected);
        assert_eq!(serde_json::to_string(&value).expect("so it does there"), expected);
    }

    let scores = Scores { score: f64::NAN, ratio: f32::NEG_INFINITY };
    assert_eq!(
        RawJson::from_value(&scores).expect("a struct of them encodes").as_str(),
        r#"{"score":null,"ratio":null}"#
    );
    assert_eq!(
        serde_json::to_string(&scores).expect("and there too"),
        r#"{"score":null,"ratio":null}"#
    );

    // The render path a foreign deserializer takes reaches the same writer.
    use serde::de::value::{Error as ValueError, F64Deserializer};
    assert_eq!(
        <RawJson as Deserialize>::deserialize(F64Deserializer::<ValueError>::new(f64::NAN))
            .expect("it renders")
            .as_str(),
        "null"
    );
}

#[test]
fn an_encode_error_is_a_key_the_writer_cannot_spell_or_a_value_that_refuses() {
    // The two failures the rustdoc of `EncodeError` names. A non-finite float
    // is not one of them, which is why it is not mentioned there.
    let tuple_keys = std::collections::BTreeMap::from([((1_u8, 2_u8), 3_u8)]);
    let error = RawJson::from_value(&tuple_keys).expect_err("a tuple is not a JSON key");
    assert!(error.message().contains("key"), "{error}");
    assert!(serde_json::to_string(&tuple_keys).is_err(), "the reference codec refuses it too");

    struct Refuses;
    impl Serialize for Refuses {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("this value declines to be encoded"))
        }
    }
    let error = RawJson::from_value(&Refuses).expect_err("the value refuses");
    assert_eq!(error.message(), "this value declines to be encoded");
}

#[test]
fn bytes_after_the_value_are_a_syntax_error_at_the_byte_that_follows_it() {
    // The failure path deserializes a second time to build the field path, and
    // that pass stops at the end of the value rather than at the end of the
    // input. A document with anything behind its value parses on that pass and
    // failed on the first, so the position has to come from finishing the
    // deserializer, not from the pass itself.
    let trailing_number = decode::<u64>(b"1 2").expect_err("a second value is not one value");
    assert_eq!(trailing_number.kind(), DecodeErrorKind::Syntax, "{trailing_number}");
    assert_eq!(trailing_number.line(), 1);
    assert_eq!(trailing_number.column(), 3, "the column points at the second value");
    assert_eq!(trailing_number.path(), "");

    let injected = decode::<IgnoredAny>(br#"{"a":1}, "model": "evil""#)
        .expect_err("a key behind the value is not one value");
    assert_eq!(injected.kind(), DecodeErrorKind::Syntax, "{injected}");
    assert_eq!(injected.line(), 1);
    assert_eq!(injected.column(), 8, "the column points at the comma");
    assert!(!injected.to_string().contains("evil"), "the error quoted the input: {injected}");

    let after_a_struct =
        decode::<Envelope>(br#"{"answers":{"spam":{"noul":0.9},"tone":{"confidence":0.4}}}x"#)
            .expect_err("a stray byte is not whitespace");
    assert_eq!(after_a_struct.kind(), DecodeErrorKind::Syntax, "{after_a_struct}");
    assert_eq!(after_a_struct.line(), 1);
    assert_eq!(after_a_struct.column(), 60, "the column points at the stray byte");
}

#[test]
fn a_data_error_without_a_path_says_so_rather_than_printing_an_empty_one() {
    // Reached only when the two decode passes disagree about whether the
    // document parses at all, which no input produces today; the rendering is
    // asserted here because it is what a caller would see if one did.
    let opaque = DecodeError { detail: Detail::Opaque };

    assert_eq!(opaque.kind(), DecodeErrorKind::Data);
    assert_eq!(opaque.path(), "");
    assert_eq!(opaque.line(), 0);
    assert_eq!(opaque.column(), 0);
    assert_eq!(opaque.to_string(), "the JSON document does not have the expected shape");
    assert!(!opaque.to_string().contains("``"), "an empty path must not be printed");
}
