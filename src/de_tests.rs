use std::fmt::Debug;

use http::HeaderValue;
use serde_json::json;

use super::*;
use crate::{ErrorKind, response::Answer};

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("../tests/fixtures/result.json");
const UNKNOWN_ANSWER_TYPE: &[u8] = include_bytes!("../tests/fixtures/unknown-answer-type.json");
const UNKNOWN_EXTRA_FIELDS: &[u8] = include_bytes!("../tests/fixtures/unknown-extra-fields.json");
const STRUCTURED_LEGEND: &[u8] = include_bytes!("../tests/fixtures/structured-legend.json");
const EMPTY_ANSWERS: &[u8] = include_bytes!("../tests/fixtures/empty-answers.json");

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-123"));
    headers
}

fn decode_as<A: AnswerSet>(body: &[u8], questions: usize) -> Result<SystemOneResponse<A>, Error> {
    let uri = Uri::from_static(ENDPOINT);
    decode_system_one(
        Bytes::copy_from_slice(body),
        StatusCode::OK,
        headers(),
        questions,
        Some((&Method::POST, &uri)),
    )
}

fn decode(body: &[u8]) -> SystemOneResponse {
    match decode_as::<Answers>(body, 3) {
        Ok(response) => response,
        Err(error) => panic!("{} did not decode: {error}", String::from_utf8_lossy(body)),
    }
}

/// The response-validation error a body fails with.
fn rejection<A: Debug + AnswerSet>(body: &[u8]) -> ResponseValidationError {
    match decode_as::<A>(body, 3) {
        Ok(response) => panic!(
            "{} decoded where it should have failed: {response:?}",
            String::from_utf8_lossy(body)
        ),
        Err(error) => match error.kind() {
            ErrorKind::ResponseValidation(failure) => failure.clone(),
            other => {
                panic!("{} failed with the wrong kind: {other:?}", String::from_utf8_lossy(body))
            }
        },
    }
}

/// The path a body fails at, through the runtime answer set.
fn failure_path(body: &serde_json::Value) -> String {
    let text = serde_json::to_vec(body).expect("the test body encodes");
    rejection::<Answers>(&text).field_path().to_owned()
}

/// `RESULT` with one more answer after the last one, written as given.
fn with_future_answer(answer: &str) -> String {
    let text = std::str::from_utf8(RESULT).expect("the fixture is UTF-8").trim_end();
    let open = text.strip_suffix("}}").expect("the fixture ends with the answers and the body");
    format!("{open},{answer}}}}}")
}

/// The `answers` object of `RESULT`, as the text the fixture spells it with.
fn result_answers() -> &'static str {
    let text = std::str::from_utf8(RESULT).expect("the fixture is UTF-8").trim_end();
    let (_, answers) = text.split_once(r#""answers":"#).expect("the fixture has answers");
    answers.strip_suffix('}').expect("the answers close the body")
}

/// A response body around `answers`, with the other members valid.
fn with_answers(answers: serde_json::Value) -> serde_json::Value {
    json!({"model": "test", "usage": {"input_tokens": 1, "output_tokens": 1}, "answers": answers})
}

// ------------------------------------------------------- the fixture

#[test]
fn the_result_fixture_decodes_to_one_answer_of_each_kind() {
    let response = decode(RESULT);

    assert_eq!(response.model(), "jev-latest");
    assert_eq!(*response.usage(), Usage::new(Some(12), Some(3)));
    let answers = response.answers();
    assert_eq!(answers.names().collect::<Vec<_>>(), ["spam", "tone", "quality"]);
    assert_eq!(answers.nouls().map(|(name, _)| name).collect::<Vec<_>>(), ["spam"]);
    assert_eq!(answers.choices().map(|(name, _)| name).collect::<Vec<_>>(), ["tone"]);
    assert_eq!(answers.scores().map(|(name, _)| name).collect::<Vec<_>>(), ["quality"]);

    let spam = answers.noul("spam").expect("spam is a noul answer");
    assert_eq!(spam.noul().to_bits(), 0.98_f64.to_bits());

    let tone = answers.choice("tone").expect("tone is a choice answer");
    assert_eq!(tone.choice(), "friendly");
    assert_eq!(tone.confidence().to_bits(), 0.9_f64.to_bits());
    assert_eq!(tone.probabilities().collect::<Vec<_>>(), [("friendly", 0.9), ("hostile", 0.1)]);

    let quality = answers.score("quality").expect("quality is a score answer");
    assert_eq!(quality.score().to_bits(), 1.7_f64.to_bits());
    assert_eq!(quality.confidence().to_bits(), 0.8_f64.to_bits());
    let legend: Vec<(u32, Option<&str>)> =
        quality.legend().map(|(level, description)| (level, description.as_text())).collect();
    assert_eq!(legend, [(0, Some("bad")), (1, Some("ok")), (2, Some("great"))]);
    assert_eq!(quality.probabilities().collect::<Vec<_>>(), [(0, 0.1), (1, 0.1), (2, 0.8)]);

    assert_eq!(response.meta().status(), StatusCode::OK);
    assert_eq!(response.meta().request_id(), Some("req-123"));
    assert_eq!(&response.meta().raw_body()[..], RESULT);
}

#[test]
fn an_answer_of_an_unknown_type_is_dropped_and_stays_in_the_raw_body() {
    let text = with_future_answer(r#""future":{"type":"future"}"#);

    let response = decode(text.as_bytes());

    assert_eq!(response.answers().names().collect::<Vec<_>>(), ["spam", "tone", "quality"]);
    assert_eq!(response.answers().get("future"), None);
    assert_eq!(response.answers(), decode(RESULT).answers(), "only the future answer is dropped");
    let raw: serde_json::Value =
        serde_json::from_slice(response.meta().raw_body()).expect("the raw body is the JSON sent");
    assert_eq!(raw["answers"]["future"], json!({"type": "future"}));
}

#[test]
fn a_missing_noul_fails_at_its_full_path() {
    let failure =
        rejection::<Answers>(br#"{"model":"test","usage":{},"answers":{"spam":{"type":"noul"}}}"#);

    assert_eq!(failure.field_path(), "answers.spam.noul");
    assert_eq!(failure.message(), "Invalid response data at 'answers.spam.noul'.");
    assert_eq!(failure.decode_error().kind(), crate::DecodeErrorKind::Data);
    assert_eq!(failure.status(), StatusCode::OK);
    assert_eq!(failure.request_id(), Some("req-123"));
    assert_eq!(
        failure.to_string(),
        "POST https://api.typesafe.ai/v1/systemone: 200 Invalid response data at \
         'answers.spam.noul'. (request_id=req-123)"
    );
}

// ------------------------------------------- ported from test_responses.py

/// `test_malformed_response_raises_validation_error`, every row.
///
/// The bodies are spelled out rather than built with `json!`, whose map sorts
/// its keys: the Python test sends each answer with `type` first, as the API
/// does, and the order decides whether a member is read in place.
#[test]
fn a_malformed_response_fails_where_the_python_sdk_says_it_does() {
    let rows = [
        (r#"{}"#, "model"),
        (r#"{"n":{"type":"noul"}}"#, "answers.n.noul"),
        (r#"{"n":{"type":"noul","noul":"0.5"}}"#, "answers.n.noul"),
        (r#"{"c":{"type":"choice","choice":"a","probabilities":{}}}"#, "answers.c.confidence"),
        (r#"{"c":{"type":"choice","confidence":0.5,"probabilities":{}}}"#, "answers.c.choice"),
        (
            r#"{"s":{"type":"score","score":1.0,"confidence":1.0,"legend":[],"probabilities":{}}}"#,
            "answers.s.legend",
        ),
        (
            r#"{"s":{"type":"score","score":1.0,"confidence":1.0,"legend":{"x":"bad"},"probabilities":{}}}"#,
            "answers.s.legend.x",
        ),
        (r#"{"c":"not-a-mapping"}"#, "answers.c.type"),
    ];

    for (answers, path) in rows {
        let model = if path == "model" { "" } else { r#","model":"test""# };
        let text = format!(
            r#"{{"usage":{{"input_tokens":1,"output_tokens":1}},"answers":{answers}{model}}}"#
        );

        let failure = rejection::<Answers>(text.as_bytes());

        assert_eq!(failure.field_path(), path, "body {text}");
        assert_eq!(failure.status(), StatusCode::OK, "body {text}");
        assert_eq!(failure.request_id(), Some("req-123"), "body {text}");
        assert_eq!(failure.body(), text.as_bytes(), "body {text}");
        assert_eq!(
            failure.to_string(),
            format!(
                "POST https://api.typesafe.ai/v1/systemone: 200 Invalid response data at \
                 '{path}'. (request_id=req-123)"
            ),
            "body {text}"
        );
    }
}

/// A body whose answers object is `answers`, written as given.
fn around(answers: &str) -> String {
    format!(
        r#"{{"model":"test","usage":{{"input_tokens":1,"output_tokens":1}},"answers":{answers}}}"#
    )
}

/// `test_unknown_answer_type_ignored`.
#[test]
fn a_future_answer_type_is_skipped_rather_than_raised() {
    let response = decode(UNKNOWN_ANSWER_TYPE);

    assert_eq!(response.answers().names().collect::<Vec<_>>(), ["spam"]);
    assert_eq!(response.answers().noul("spam").map(NoulAnswer::noul), Some(0.9));
    let raw: serde_json::Value =
        serde_json::from_slice(response.meta().raw_body()).expect("the raw body parses");
    assert_eq!(raw["answers"]["mystery"]["type"], "aurora");
}

/// `test_unknown_extra_fields_tolerated`.
#[test]
fn members_this_version_does_not_know_are_ignored() {
    let response = decode(UNKNOWN_EXTRA_FIELDS);

    assert_eq!(response.answers().noul("spam").map(NoulAnswer::noul), Some(0.9));
    assert_eq!(*response.usage(), Usage::new(Some(1), Some(1)));
    assert_eq!(
        serde_json::to_value(response.usage()).expect("usage serializes"),
        json!({"input_tokens": 1, "output_tokens": 1})
    );
    let raw: serde_json::Value =
        serde_json::from_slice(response.meta().raw_body()).expect("the raw body parses");
    assert_eq!(raw["usage"]["billing_units"], 1);
}

/// `test_extra_body_shallow_override`: a legend description may be an object.
#[test]
fn a_structured_legend_description_is_kept_as_the_json_it_arrived_as() {
    let response = decode(STRUCTURED_LEGEND);

    let risk = response.answers().score("risk").expect("risk is a score answer");
    assert_eq!(risk.score().to_bits(), 0.0_f64.to_bits());
    assert_eq!(risk.confidence().to_bits(), 1.0_f64.to_bits());
    let description = risk.description(0).expect("level 0 has a description");
    assert_eq!(
        description.as_json().map(crate::RawJson::as_str),
        Some(r#"{"summary":"duplicated","examples":["charged twice"]}"#)
    );
    assert_eq!(risk.probabilities().collect::<Vec<_>>(), [(0, 1.0)]);
    assert_eq!(risk.description(1), None);
}

/// The response of `test_array_inputs`: no answers and no token counts.
#[test]
fn an_empty_answer_object_and_empty_usage_decode() {
    let response = decode(EMPTY_ANSWERS);

    assert!(response.answers().is_empty(), "{:?}", response.answers());
    assert_eq!(*response.usage(), Usage::new(None, None));
}

// ------------------------------------------------------ order independence

#[test]
fn an_answer_may_name_its_type_after_the_members_it_governs() {
    let reordered = br#"{"answers":{"spam":{"noul":0.98,"type":"noul"},"tone":{"probabilities":{"friendly":0.9,"hostile":0.1},"confidence":0.9,"choice":"friendly","type":"choice"},"quality":{"legend":{"2":"great","0":"bad","1":"ok"},"probabilities":{"2":0.8,"0":0.1,"1":0.1},"confidence":0.8,"score":1.7,"type":"score"}},"usage":{"output_tokens":3,"input_tokens":12},"model":"jev-latest"}"#;

    let response = decode(reordered);

    assert_eq!(response.answers(), decode(RESULT).answers());
    assert_eq!(response.model(), "jev-latest");
    assert_eq!(*response.usage(), Usage::new(Some(12), Some(3)));
}

/// A document with its answers in another order, the members of each answer in
/// another order, and an answer of a type this version does not know.
#[test]
fn a_shuffled_document_yields_the_same_answers_by_name() {
    let shuffled = br#"{"answers":{"quality":{"legend":{"2":"great","0":"bad","1":"ok"},"probabilities":{"2":0.8,"0":0.1,"1":0.1},"confidence":0.8,"score":1.7,"type":"score"},"future":{"horizon":"2027","type":"prediction"},"tone":{"probabilities":{"hostile":0.1,"friendly":0.9},"confidence":0.9,"choice":"friendly","type":"choice"},"spam":{"noul":0.98,"type":"noul"}},"usage":{"output_tokens":3,"input_tokens":12},"model":"jev-latest"}"#;

    let response = decode(shuffled);
    let reference = decode(RESULT);

    assert_eq!(response.answers().names().collect::<Vec<_>>(), ["quality", "tone", "spam"]);
    for name in ["spam", "quality"] {
        assert_eq!(response.answers().get(name), reference.answers().get(name), "{name}");
    }
    // A choice keeps its options in the order received, so only the set of
    // options is the same.
    let tone = response.answers().choice("tone").expect("tone is a choice answer");
    assert_eq!(tone.probabilities().collect::<Vec<_>>(), [("hostile", 0.1), ("friendly", 0.9)]);
    assert_eq!(tone.probability("friendly"), Some(0.9));
}

#[test]
fn a_future_type_may_reuse_a_member_name_with_another_shape_before_naming_itself() {
    let body = with_answers(json!({
        "later": {"noul": "high", "choice": 3, "legend": [1, 2], "probabilities": null, "score": {}, "type": "future"},
        "spam": {"type": "noul", "noul": 0.25},
    }));
    let text = serde_json::to_vec(&body).expect("the body encodes");

    let response = decode(&text);

    assert_eq!(response.answers().names().collect::<Vec<_>>(), ["spam"]);
    assert_eq!(response.answers().noul("spam").map(NoulAnswer::noul), Some(0.25));
}

/// The order the API writes answers in: `type` first. Every member after an
/// unknown type is skipped unread, whatever its name.
#[test]
fn a_future_type_named_first_is_skipped_whatever_its_members_look_like() {
    let text = around(
        r#"{"later":{"type":"future","noul":"high","choice":3,"confidence":[],"legend":[1,2],"probabilities":null,"score":{}},"spam":{"type":"noul","noul":0.25}}"#,
    );

    let response = decode(text.as_bytes());

    assert_eq!(response.answers().names().collect::<Vec<_>>(), ["spam"]);
    assert_eq!(response.answers().noul("spam").map(NoulAnswer::noul), Some(0.25));
}

#[test]
fn members_held_before_the_type_decode_as_that_type() {
    let body = with_answers(json!({
        "tone": {"choice": "a\"b", "probabilities": {"a\"b": 0.75, "c": 0.25}, "confidence": 0.5, "type": "choice"},
        "quality": {"probabilities": {"1": 0.5, "0": 0.5}, "legend": {"1": ["x"], "0": "low"}, "score": 0.5, "confidence": 1, "type": "score"},
    }));
    let text = serde_json::to_vec(&body).expect("the body encodes");

    let response = decode(&text);

    let tone = response.answers().choice("tone").expect("tone is a choice answer");
    assert_eq!(tone.choice(), "a\"b");
    assert_eq!(tone.confidence(), 0.5);
    assert_eq!(tone.probabilities().collect::<Vec<_>>(), [("a\"b", 0.75), ("c", 0.25)]);
    let quality = response.answers().score("quality").expect("quality is a score answer");
    assert_eq!(quality.probabilities().collect::<Vec<_>>(), [(0, 0.5), (1, 0.5)]);
    let legend: Vec<(u32, String)> = quality
        .legend()
        .map(|(level, description)| {
            let text = description.as_text().map_or_else(
                || description.as_json().map(|raw| raw.as_str().to_owned()),
                |text| Some(text.to_owned()),
            );
            (level, text.expect("a description is text or JSON"))
        })
        .collect();
    assert_eq!(legend, [(0, "low".to_owned()), (1, r#"["x"]"#.to_owned())]);
}

/// A member held as raw text is checked once its type is known, after the
/// walk has left it, so its failure is reported at the answer. When the type
/// comes first - which is how the API writes answers - the failure names the
/// member, as `a_malformed_response_fails_where_the_python_sdk_says_it_does`
/// shows.
#[test]
fn a_held_member_of_the_wrong_shape_fails_at_its_answer() {
    let rows = [
        (json!({"n": {"noul": "0.5", "type": "noul"}}), "answers.n"),
        (
            json!({"c": {"choice": 1, "confidence": 1, "probabilities": {}, "type": "choice"}}),
            "answers.c",
        ),
        (
            json!({"c": {"probabilities": [], "choice": "a", "confidence": 1, "type": "choice"}}),
            "answers.c",
        ),
        (
            json!({"s": {"probabilities": {"x": 1}, "legend": {}, "score": 1, "confidence": 1, "type": "score"}}),
            "answers.s",
        ),
        (
            json!({"s": {"legend": {"0": 7}, "probabilities": {}, "score": 1, "confidence": 1, "type": "score"}}),
            "answers.s",
        ),
        // A member that is missing is reported at its own path whatever the
        // order, because nothing had to be held for it.
        (json!({"n": {"other": 1, "type": "noul"}}), "answers.n.noul"),
        (
            json!({"c": {"choice": "a", "confidence": 1, "type": "choice"}}),
            "answers.c.probabilities",
        ),
        (
            json!({"s": {"score": 1, "confidence": 1, "legend": {}, "type": "score"}}),
            "answers.s.probabilities",
        ),
    ];

    for (answers, path) in rows {
        let body = with_answers(answers);
        assert_eq!(failure_path(&body), path, "body {body}");
    }
}

// ------------------------------------------------------------ answer shape

#[test]
fn an_answer_without_a_usable_type_fails_at_its_type() {
    let rows = [
        json!({"x": {"noul": 0.5}}),
        json!({"x": {"type": 5, "noul": 0.5}}),
        json!({"x": {"type": null}}),
        json!({"x": 5}),
        json!({"x": -5}),
        json!({"x": 0.5}),
        json!({"x": true}),
        json!({"x": null}),
        json!({"x": [1, 2]}),
        json!({"x": "noul"}),
    ];

    for answers in rows {
        let body = with_answers(answers);
        assert_eq!(failure_path(&body), "answers.x.type", "body {body}");
    }
}

#[test]
fn members_that_belong_to_another_kind_are_ignored() {
    // `type` first, so every member is judged against a known kind as it
    // arrives; the same members held until a late `type` are dropped as well.
    let type_first = around(
        r#"{"n":{"type":"noul","noul":1,"choice":{"not":"read"},"legend":5,"probabilities":"no","score":[]},"c":{"type":"choice","choice":"a","confidence":1,"probabilities":{"a":1},"noul":[],"score":"x","legend":null}}"#,
    );
    let type_last = around(
        r#"{"n":{"noul":1,"choice":{"not":"read"},"legend":5,"probabilities":"no","score":[],"type":"noul"},"c":{"choice":"a","confidence":1,"probabilities":{"a":1},"noul":[],"score":"x","legend":null,"type":"choice"}}"#,
    );

    for text in [type_first, type_last] {
        let response = decode(text.as_bytes());

        assert_eq!(response.answers().noul("n").map(NoulAnswer::noul), Some(1.0), "body {text}");
        assert_eq!(
            response.answers().choice("c").map(ChoiceAnswer::choice),
            Some("a"),
            "body {text}"
        );
    }
}

#[test]
fn an_answer_that_names_two_types_is_refused() {
    // `json!` keeps one of two equal keys, so the documents are spelled out.
    let texts = [
        r#"{"model":"m","usage":{},"answers":{"x":{"type":"score","probabilities":{"0":1},"type":"choice","choice":"a","confidence":1}}}"#,
        r#"{"model":"m","usage":{},"answers":{"x":{"type":"choice","probabilities":{"a":1},"type":"score","score":1,"confidence":1,"legend":{}}}}"#,
    ];

    for text in texts {
        let failure = rejection::<Answers>(text.as_bytes());
        assert_eq!(failure.field_path(), "answers.x", "body {text}");
        assert_eq!(failure.decode_error().kind(), crate::DecodeErrorKind::Data, "body {text}");
    }
}

#[test]
fn a_repeated_member_keeps_its_last_value() {
    let text = br#"{"model":"m","usage":{},"answers":{"n":{"type":"noul","noul":0.1,"noul":0.7}}}"#;

    let response = decode(text);

    assert_eq!(response.answers().noul("n").map(NoulAnswer::noul), Some(0.7));
}

#[test]
fn a_repeated_question_name_keeps_both_answers_and_finds_the_first() {
    let text = br#"{"model":"m","usage":{},"answers":{"n":{"type":"noul","noul":0.1},"n":{"type":"noul","noul":0.7}}}"#;

    let response = decode(text);

    assert_eq!(response.answers().len(), 2);
    assert_eq!(response.answers().noul("n").map(NoulAnswer::noul), Some(0.1));
}

// ------------------------------------------------ keys the server chose

/// A response body around `answers`, with each server-chosen key written as a
/// JSON string by `serde_json`, so that control characters in it arrive as
/// JSON escapes rather than as raw bytes in this file.
fn around_keys(answers: &str, keys: &[&str]) -> String {
    let mut text = answers.to_owned();
    for (index, key) in keys.iter().enumerate() {
        let encoded = serde_json::to_string(key).expect("a string encodes");
        text = text.replace(&format!("@{index}"), &encoded);
    }
    around(&text)
}

/// Asserts the whole rendering of a response-validation failure at `path`.
fn assert_rendered(failure: &ResponseValidationError, path: &str) {
    assert_eq!(failure.field_path(), path);
    assert_eq!(failure.message(), format!("Invalid response data at '{path}'."));
    assert_eq!(
        failure.to_string(),
        format!(
            "POST https://api.typesafe.ai/v1/systemone: 200 Invalid response data at '{path}'. \
             (request_id=req-123)"
        )
    );
    for rendered in [failure.to_string(), format!("{failure:?}")] {
        assert!(
            !rendered.bytes().any(|byte| byte < 0x20 || byte == 0x7f),
            "a control byte reached the text: {rendered:?}"
        );
        assert!(!rendered.contains('\u{202e}'), "a bidi override reached the text: {rendered:?}");
    }
}

#[test]
fn a_question_name_the_server_chose_is_echoed_but_made_safe_to_print() {
    // A plain name is echoed: which question failed is what the path is for.
    let plain =
        rejection::<Answers>(br#"{"model":"m","usage":{},"answers":{"SECRETH":{"type":"noul"}}}"#);
    assert_rendered(&plain, "answers.SECRETH.noul");

    // A name carrying a line break and a terminal colour sequence cannot
    // break the log line or recolour the terminal it is printed to.
    let text = around_keys(r#"{@0:{"type":"noul"}}"#, &["x\ny\u{1b}[31mz"]);
    let injected = rejection::<Answers>(text.as_bytes());
    assert_rendered(&injected, r"answers.x\ny\u{1b}[31mz.noul");

    // A bidi override is escaped; a printable non-ASCII name is not.
    let text = around_keys(r#"{@0:{"type":"noul"}}"#, &["ok\u{202e}gnp.exe"]);
    assert_rendered(&rejection::<Answers>(text.as_bytes()), r"answers.ok\u{202e}gnp.exe.noul");
    let text = around_keys(r#"{@0:{"type":"noul"}}"#, &["\u{54c1}\u{8cea}"]);
    assert_rendered(&rejection::<Answers>(text.as_bytes()), "answers.\u{54c1}\u{8cea}.noul");

    // 100 KB of name gives a message of bounded size.
    let huge = "n".repeat(100_000);
    let text = around_keys(r#"{@0:{"type":"noul"}}"#, &[&huge]);
    let failure = rejection::<Answers>(text.as_bytes());
    assert_rendered(&failure, &format!("answers.{}\u{2026}.noul", "n".repeat(128)));
    assert!(failure.to_string().len() < 300, "{} bytes", failure.to_string().len());
}

#[test]
fn legend_levels_and_choice_options_go_through_the_same_rendering() {
    let text = around_keys(
        r#"{"q":{"type":"score","score":1,"confidence":1,"legend":{@0:"x"},"probabilities":{}}}"#,
        &["\u{1b}[2J7"],
    );
    assert_rendered(&rejection::<Answers>(text.as_bytes()), r"answers.q.legend.\u{1b}[2J7");

    let text = around_keys(
        r#"{"c":{"type":"choice","choice":"a","confidence":1,"probabilities":{@0:"high"}}}"#,
        &["opt\r\nion\u{2066}"],
    );
    assert_rendered(
        &rejection::<Answers>(text.as_bytes()),
        r"answers.c.probabilities.opt\r\nion\u{2066}",
    );

    let long_level = "9".repeat(300);
    let text = around_keys(
        r#"{"q":{"type":"score","score":1,"confidence":1,"legend":{@0:"x"},"probabilities":{}}}"#,
        &[&long_level],
    );
    assert_rendered(
        &rejection::<Answers>(text.as_bytes()),
        &format!("answers.q.legend.{}\u{2026}", "9".repeat(128)),
    );
}

// ------------------------------------------------------------ score levels

#[test]
fn score_levels_are_integers_sorted_whatever_the_wire_order() {
    let type_first = around(
        r#"{"q":{"type":"score","score":5,"confidence":1,"legend":{"10":"top","2":"mid","0":"low"},"probabilities":{"10":0.5,"0":0.25,"2":0.25}}}"#,
    );
    let type_last = around(
        r#"{"q":{"probabilities":{"10":0.5,"0":0.25,"2":0.25},"legend":{"10":"top","2":"mid","0":"low"},"score":5,"confidence":1,"type":"score"}}"#,
    );

    for text in [type_first, type_last] {
        let response = decode(text.as_bytes());

        let q = response.answers().score("q").expect("q is a score answer");
        assert_eq!(
            q.legend().map(|(level, _)| level).collect::<Vec<_>>(),
            [0, 2, 10],
            "body {text}"
        );
        assert_eq!(
            q.probabilities().collect::<Vec<_>>(),
            [(0, 0.25), (2, 0.25), (10, 0.5)],
            "body {text}"
        );
        assert_eq!(q.description(10).and_then(Content::as_text), Some("top"), "body {text}");
        assert_eq!(q.probability(2), Some(0.25), "body {text}");
        assert_eq!(q.probability(3), None, "body {text}");
    }
}

#[test]
fn a_score_level_that_is_not_a_non_negative_integer_fails_at_its_key() {
    let rows = [
        (r#""legend":{"-1":"x"},"probabilities":{}"#, "answers.s.legend.-1"),
        (r#""legend":{"1.5":"x"},"probabilities":{}"#, "answers.s.legend.1.5"),
        (r#""legend":{"4294967296":"x"},"probabilities":{}"#, "answers.s.legend.4294967296"),
        (r#""legend":{},"probabilities":{"two":1}"#, "answers.s.probabilities.two"),
        (r#""probabilities":{"0":1},"legend":{"0":1}"#, "answers.s.legend.0"),
        (r#""legend":{"0":null},"probabilities":{}"#, "answers.s.legend.0"),
        (r#""legend":{},"probabilities":{"0":"1"}"#, "answers.s.probabilities.0"),
    ];

    for (members, path) in rows {
        let text =
            around(&format!(r#"{{"s":{{"type":"score","score":1,"confidence":1,{members}}}}}"#));
        assert_eq!(rejection::<Answers>(text.as_bytes()).field_path(), path, "body {text}");
    }
}

#[test]
fn a_level_named_twice_keeps_both_in_the_order_received() {
    let text = br#"{"model":"m","usage":{},"answers":{"s":{"type":"score","score":1,"confidence":1,"legend":{"1":"b","0":"a","1":"c"},"probabilities":{"0":0.5,"00":0.5}}}}"#;

    let response = decode(text);

    let s = response.answers().score("s").expect("s is a score answer");
    let legend: Vec<(u32, Option<&str>)> =
        s.legend().map(|(level, description)| (level, description.as_text())).collect();
    assert_eq!(legend, [(0, Some("a")), (1, Some("b")), (1, Some("c"))]);
    assert_eq!(s.probabilities().collect::<Vec<_>>(), [(0, 0.5), (0, 0.5)]);
}

// ------------------------------------------------------- the top level

#[test]
fn model_and_usage_are_required_and_answers_are_not() {
    let rows = [
        (json!({"usage": {}, "answers": {}}), "model"),
        (json!({"model": "m", "answers": {}}), "usage"),
        (json!({"model": 1, "usage": {}}), "model"),
        (json!({"model": "m", "usage": {"input_tokens": 1.5}}), "usage.input_tokens"),
        (json!({"model": "m", "usage": {"output_tokens": -1}}), "usage.output_tokens"),
        (json!({"model": "m", "usage": [], "answers": {}}), "usage"),
        (json!({"model": "m", "usage": {}, "answers": []}), "answers"),
        (json!({"model": "m", "usage": {}, "answers": null}), "answers"),
    ];
    for (body, path) in rows {
        assert_eq!(failure_path(&body), path, "body {body}");
    }

    let response = decode(br#"{"model":"m","usage":{"input_tokens":null},"extra":[1,{"a":2}]}"#);
    assert!(response.answers().is_empty());
    assert_eq!(*response.usage(), Usage::new(None, None));
}

/// The document root is reported as `.`, where the Python SDK reports an empty
/// path: the codec names the root that way for every type it decodes.
#[test]
fn a_body_that_is_not_an_object_fails_at_the_root() {
    for body in [&b"[]"[..], b"null", b"\"text\"", b"1"] {
        let failure = rejection::<Answers>(body);
        assert_eq!(failure.field_path(), ".", "body {}", String::from_utf8_lossy(body));
        assert_eq!(failure.message(), "Invalid response data at '.'.");
    }
}

#[test]
fn a_body_that_is_not_json_or_too_deep_fails_without_a_path() {
    let too_deep = format!(
        r#"{{"model":"m","usage":{{}},"answers":{{"s":{{"type":"score","score":1,"confidence":1,"probabilities":{{}},"legend":{{"0":{}{}}}}}}}}}"#,
        "[".repeat(13),
        "]".repeat(13)
    );
    let rows: [(&[u8], crate::DecodeErrorKind); 3] = [
        (b"{\"model\":", crate::DecodeErrorKind::Syntax),
        (br#"{"model":"m","usage":{}} {}"#, crate::DecodeErrorKind::Syntax),
        (too_deep.as_bytes(), crate::DecodeErrorKind::TooDeep),
    ];

    for (body, kind) in rows {
        let failure = rejection::<Answers>(body);
        assert_eq!(failure.decode_error().kind(), kind, "body {}", String::from_utf8_lossy(body));
        assert_eq!(failure.field_path(), "", "body {}", String::from_utf8_lossy(body));
        assert_eq!(failure.message(), "Invalid response data at ''.");
    }
}

#[test]
fn a_failure_without_an_endpoint_leaves_it_out() {
    let result = decode_system_one::<Answers>(
        Bytes::from_static(b"{}"),
        StatusCode::OK,
        HeaderMap::new(),
        0,
        None,
    );

    let error = result.expect_err("an empty object has no model");
    assert_eq!(error.to_string(), "200 Invalid response data at 'model'.");
}

// ------------------------------------------------------------- sizing

#[test]
fn the_answer_storage_is_sized_from_the_question_count() {
    let body = with_answers(serde_json::Value::Object(
        (0..20).map(|index| (format!("q{index}"), json!({"type": "noul", "noul": 0.5}))).collect(),
    ));
    let text = serde_json::to_vec(&body).expect("the body encodes");

    for (questions, capacity) in [(20, 20), (32, 32)] {
        let response = decode_as::<Answers>(&text, questions).expect("the body decodes");
        assert_eq!(response.answers().len(), 20);
        assert_eq!(response.answers().capacity(), capacity, "{questions} questions asked");
    }
    let unsized_ = decode_as::<Answers>(&text, 0).expect("the body decodes");
    assert!(unsized_.answers().capacity() >= 20, "{}", unsized_.answers().capacity());
}

#[test]
fn the_question_count_is_scoped_to_one_decode() {
    assert_eq!(EXPECTED_ANSWERS.get(), 0);
    {
        let _outer = ExpectedAnswers::enter(7);
        {
            let _inner = ExpectedAnswers::enter(3);
            assert_eq!(EXPECTED_ANSWERS.get(), 3);
        }
        assert_eq!(EXPECTED_ANSWERS.get(), 7);
        decode(RESULT);
        assert_eq!(EXPECTED_ANSWERS.get(), 7, "a decode puts the enclosing count back");
    }
    assert_eq!(EXPECTED_ANSWERS.get(), 0);
    assert!(rejection::<Answers>(b"{}").field_path() == "model");
    assert_eq!(EXPECTED_ANSWERS.get(), 0, "a failed decode puts it back too");
}

// ---------------------------------------------------- typed answer sets

/// A question set declared as a struct, with the [`AnswerSet`] implementation
/// the derive macro is to generate, written by hand: field dispatch on the
/// key, no map and no name string.
#[derive(Debug, Clone, PartialEq)]
struct Ticket {
    spam: NoulAnswer,
    tone: ChoiceAnswer,
    quality: ScoreAnswer,
}

enum TicketField {
    Spam,
    Tone,
    Quality,
    Other,
}

impl<'de> Deserialize<'de> for TicketField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;

        impl Visitor<'_> for FieldVisitor {
            type Value = TicketField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a question name")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<TicketField, E> {
                Ok(match value {
                    "spam" => TicketField::Spam,
                    "tone" => TicketField::Tone,
                    "quality" => TicketField::Quality,
                    _ => TicketField::Other,
                })
            }
        }

        deserializer.deserialize_str(FieldVisitor)
    }
}

impl AnswerSet for Ticket {
    fn deserialize_answers<'de, D>(deserializer: D, _: AnswerContext) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TicketVisitor;

        impl<'de> Visitor<'de> for TicketVisitor {
            type Value = Ticket;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the answers of a Ticket")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Ticket, M::Error>
            where
                M: MapAccess<'de>,
            {
                let (mut spam, mut tone, mut quality) = (None, None, None);
                while let Some(field) = map.next_key::<TicketField>()? {
                    match field {
                        TicketField::Spam => spam = Some(map.next_value()?),
                        TicketField::Tone => tone = Some(map.next_value()?),
                        TicketField::Quality => quality = Some(map.next_value()?),
                        TicketField::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(Ticket {
                    spam: spam.ok_or_else(|| de::Error::missing_field("spam"))?,
                    tone: tone.ok_or_else(|| de::Error::missing_field("tone"))?,
                    quality: quality.ok_or_else(|| de::Error::missing_field("quality"))?,
                })
            }
        }

        deserializer.deserialize_map(TicketVisitor)
    }
}

#[test]
fn a_struct_answer_set_decodes_each_answer_into_its_field() {
    let typed = decode_as::<Ticket>(RESULT, 3).expect("the fixture decodes as a Ticket");
    let runtime = decode(RESULT);

    assert_eq!(Some(&typed.answers().spam), runtime.answers().noul("spam"));
    assert_eq!(Some(&typed.answers().tone), runtime.answers().choice("tone"));
    assert_eq!(Some(&typed.answers().quality), runtime.answers().score("quality"));
    assert_eq!(typed.model(), "jev-latest");
    assert_eq!(typed.meta(), runtime.meta());

    let mut body: serde_json::Value = serde_json::from_slice(RESULT).expect("the fixture parses");
    body["answers"]["extra"] = json!({"type": "future"});
    let text = serde_json::to_vec(&body).expect("the body encodes");
    let with_extra = decode_as::<Ticket>(&text, 3).expect("an answer the struct lacks is ignored");
    assert_eq!(with_extra.answers(), typed.answers());
}

#[test]
fn a_struct_answer_set_reports_a_missing_or_mistyped_answer_by_name() {
    let mut body: serde_json::Value = serde_json::from_slice(RESULT).expect("the fixture parses");
    let quality = body["answers"]
        .as_object_mut()
        .and_then(|answers| answers.remove("quality"))
        .expect("the fixture has a quality answer");
    let text = serde_json::to_vec(&body).expect("the body encodes");
    assert_eq!(rejection::<Ticket>(&text).field_path(), "answers.quality");

    body["answers"]["quality"] = quality;
    body["answers"]["spam"] =
        json!({"type": "choice", "choice": "a", "confidence": 1, "probabilities": {}});
    let text = serde_json::to_vec(&body).expect("the body encodes");
    assert_eq!(rejection::<Ticket>(&text).field_path(), "answers.spam.type");

    body["answers"]["spam"] = json!({"noul": 0.5});
    let text = serde_json::to_vec(&body).expect("the body encodes");
    assert_eq!(rejection::<Ticket>(&text).field_path(), "answers.spam.type");

    body["answers"]["spam"] = json!({"noul": "0.5", "type": "noul"});
    let text = serde_json::to_vec(&body).expect("the body encodes");
    assert_eq!(
        rejection::<Ticket>(&text).field_path(),
        "answers.spam.noul",
        "a typed answer knows its kind before `type` arrives, so nothing is held"
    );

    let without_answers = br#"{"model":"m","usage":{}}"#;
    assert_eq!(rejection::<Ticket>(without_answers).field_path(), "spam");
}

/// Every clause of the `AnswerSet` contract, held against the struct set.
#[test]
fn a_struct_answer_set_keeps_the_answer_set_contract() {
    let result = result_answers();
    let open = result.strip_suffix('}').expect("the answers object closes");

    // Extra answers are skipped unread, whatever their kind or shape.
    let extras = format!(
        r#"{open},"extra":[1,2],"stranger":{{"type":"noul","noul":"not a number"}},"odd":{{"type":"future","legend":7}}}}"#
    );
    let typed =
        decode_as::<Ticket>(around(&extras).as_bytes(), 3).expect("extra answers are skipped");
    assert_eq!(
        typed.answers(),
        decode_as::<Ticket>(RESULT, 3).expect("the fixture decodes").answers()
    );

    // Answers in another order, and `type` after the data inside each one.
    let shuffled = around(
        r#"{"quality":{"legend":{"2":"great","0":"bad","1":"ok"},"probabilities":{"2":0.8,"0":0.1,"1":0.1},"confidence":0.8,"score":1.7,"type":"score"},"tone":{"probabilities":{"friendly":0.9,"hostile":0.1},"confidence":0.9,"choice":"friendly","type":"choice"},"spam":{"noul":0.98,"type":"noul"}}"#,
    );
    let reordered = decode_as::<Ticket>(shuffled.as_bytes(), 3).expect("any order decodes");
    assert_eq!(reordered.answers(), typed.answers());

    // Wrong kind, named after the data; a wrong shape names the member.
    let rows = [
        (r#"{"noul":0.5,"type":"choice"}"#, "answers.spam.type"),
        (r#""spam""#, "answers.spam.type"),
        (r#"{"type":"noul","noul":[]}"#, "answers.spam.noul"),
    ];
    for (spam, path) in rows {
        let (_, rest) = open.split_once(r#""tone":"#).expect("the fixture has a tone answer");
        let text = around(&format!(r#"{{"spam":{spam},"tone":{rest}}}"#));
        assert_eq!(rejection::<Ticket>(text.as_bytes()).field_path(), path, "body {text}");
    }

    // The input is one object.
    for answers in ["[]", "null", r#""answers""#] {
        let text = around(answers);
        assert_eq!(rejection::<Ticket>(text.as_bytes()).field_path(), "answers", "body {text}");
    }
}

// ------------------------------------------------ standalone deserializing

#[test]
fn each_answer_type_reads_its_own_object_through_any_codec() {
    let noul: NoulAnswer =
        serde_json::from_str(r#"{"noul": 0.5, "type": "noul", "unexpected": true}"#)
            .expect("a noul");
    assert_eq!(noul, NoulAnswer::new(0.5));

    let choice: ChoiceAnswer = serde_json::from_str(
        r#"{"type": "choice", "choice": "a", "confidence": 1.0, "probabilities": {"a": 1.0}}"#,
    )
    .expect("a choice");
    assert_eq!(choice, ChoiceAnswer::new("a", 1.0, [("a", 1.0)]));

    let score: ScoreAnswer = serde_json::from_str(
        r#"{"type": "score", "score": 0.0, "confidence": 1.0, "legend": {"0": "bad"}, "probabilities": {"0": 1.0}}"#,
    )
    .expect("a score");
    assert_eq!(score, ScoreAnswer::new(0.0, 1.0, [(0, Content::text("bad"))], [(0, 1.0)]));

    let answer: Answer = serde_json::from_str(r#"{"type": "noul", "noul": 1}"#).expect("an answer");
    assert_eq!(answer, Answer::Noul(NoulAnswer::new(1.0)));

    let answers: Answers = serde_json::from_str(result_answers()).expect("answers");
    assert_eq!(&answers, decode(RESULT).answers());
}

#[test]
fn a_standalone_answer_of_the_wrong_or_an_unknown_type_is_an_error() {
    let rows: [(&str, Result<(), String>); 5] = [
        (
            r#"{"type": "choice"}"#,
            Err("expected an answer of type `noul` at line 1 column 17".to_owned()),
        ),
        (
            r#"{"type": "future"}"#,
            Err("expected an answer of type `noul` at line 1 column 17".to_owned()),
        ),
        (r#"{"noul": 1}"#, Err("missing field `type` at line 1 column 11".to_owned())),
        (r#"[1]"#, Err("missing field `type` at line 1 column 1".to_owned())),
        (r#"{"type": "noul", "noul": 1}"#, Ok(())),
    ];
    for (text, expected) in rows {
        let result =
            serde_json::from_str::<NoulAnswer>(text).map(|_| ()).map_err(|error| error.to_string());
        assert_eq!(result, expected, "input {text}");
    }

    let choice = serde_json::from_str::<ChoiceAnswer>(r#"{"type": "score"}"#)
        .map_err(|error| error.to_string());
    assert_eq!(choice, Err("expected an answer of type `choice` at line 1 column 16".to_owned()));
    let choice = serde_json::from_str::<ChoiceAnswer>(r#"{"choice": "a"}"#)
        .map_err(|error| error.to_string());
    assert_eq!(choice, Err("missing field `type` at line 1 column 15".to_owned()));
    let score = serde_json::from_str::<ScoreAnswer>(r#"{"type": "noul"}"#)
        .map_err(|error| error.to_string());
    assert_eq!(score, Err("expected an answer of type `score` at line 1 column 15".to_owned()));
    let score =
        serde_json::from_str::<ScoreAnswer>(r#"{"score": 1}"#).map_err(|error| error.to_string());
    assert_eq!(score, Err("missing field `type` at line 1 column 12".to_owned()));

    let unknown = serde_json::from_str::<Answer>(r#"{"type": "future"}"#)
        .expect_err("a single answer of an unknown type has nothing to fall back to");
    assert_eq!(unknown.to_string(), "an answer of a type this version does not model");
}

#[test]
fn a_score_level_given_as_a_number_is_read_as_one() {
    let levels = LevelSeed.deserialize(de::value::U64Deserializer::<de::value::Error>::new(3));
    assert_eq!(levels, Ok(3));
    let too_large = LevelSeed
        .deserialize(de::value::U64Deserializer::<de::value::Error>::new(u64::from(u32::MAX) + 1));
    assert_eq!(
        too_large.map_err(|error| error.to_string()),
        Err("a score level is a non-negative integer".to_owned())
    );
}

#[test]
fn the_expectations_name_what_was_wanted() {
    let rows: [(&Render, &str); 10] = [
        (
            &|f| Visitor::expecting(&AnswersVisitor { capacity: 0 }, f),
            "an object of question name to answer",
        ),
        (
            &|f| Visitor::expecting(&AnswerSeed::<NoulAnswer> { name: "", target: PhantomData }, f),
            "an answer object",
        ),
        (&|f| Visitor::expecting(&KeyIn(&[]), f), "an object key"),
        (&|f| Visitor::expecting(&KindSeed { expected: None }, f), "an answer type name"),
        (&|f| Visitor::expecting(&TextSeed, f), "a string"),
        (&|f| Visitor::expecting(&LevelSeed, f), "a score level"),
        (&|f| Visitor::expecting(&NamedSeed, f), "an object of option name to probability"),
        (
            &|f| Visitor::expecting(&LevelsSeed { capacity: 0 }, f),
            "an object of score level to probability",
        ),
        (
            &|f| Visitor::expecting(&LegendSeed { capacity: 0 }, f),
            "an object of score level to description",
        ),
        (&|f| Visitor::expecting(&UsageVisitor, f), "an object of token counts"),
    ];
    for (expecting, text) in rows {
        assert_eq!(Expecting(expecting).to_string(), text);
    }
    assert_eq!(
        Expecting(&|f| Visitor::expecting(
            &EnvelopeVisitor::<Answers> { context: AnswerContext::default(), answers: PhantomData },
            f
        ))
        .to_string(),
        "a System One response"
    );
    assert_eq!(AnswerContext::new(4).expected_answers(), 4);
    assert_eq!(AnswerContext::default().expected_answers(), 0);
}

/// Writes a visitor's `expecting` text.
type Render = dyn Fn(&mut fmt::Formatter<'_>) -> fmt::Result;

/// Renders a visitor's `expecting` text.
struct Expecting<'a>(&'a Render);

impl fmt::Display for Expecting<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(formatter)
    }
}

// ----------------------------------------------------------------- tracing

#[cfg(feature = "tracing")]
mod warning {
    use std::sync::{Arc, Mutex};

    use tracing::{
        Event, Level, Metadata, Subscriber,
        field::{Field, Visit},
        span,
    };

    use super::*;

    /// One recorded event: its level and its fields rendered as text.
    type Recorded = (Level, Vec<(String, String)>);

    /// Every event recorded while it is the default subscriber.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Recorded>>>);

    struct Fields(Vec<(String, String)>);

    impl Visit for Fields {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_owned(), value.to_owned()));
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
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
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0
                .lock()
                .expect("the recorder is not poisoned")
                .push((*event.metadata().level(), fields.0));
        }

        fn enter(&self, _: &span::Id) {}

        fn exit(&self, _: &span::Id) {}
    }

    #[test]
    fn a_skipped_answer_is_warned_about_by_question_and_type_and_nothing_else() {
        let recorder = Recorder::default();
        let body = br#"{"model":"m","usage":{},"answers":{"mystery":{"type":"aur\u006fra","value":"secret-value"},"spam":{"type":"noul","noul":1}}}"#;

        let response = tracing::subscriber::with_default(recorder.clone(), || decode(body));

        assert_eq!(response.answers().names().collect::<Vec<_>>(), ["spam"]);
        let events = recorder.0.lock().expect("the recorder is not poisoned").clone();
        assert_eq!(events.len(), 1, "{events:?}");
        let (level, fields) = &events[0];
        assert_eq!(*level, Level::WARN);
        assert_eq!(
            fields,
            &[
                (
                    "message".to_owned(),
                    "ignoring an answer of a type this version does not model; the raw body still carries it"
                        .to_owned()
                ),
                ("question".to_owned(), "mystery".to_owned()),
                ("answer_type".to_owned(), "aurora".to_owned()),
            ]
        );
        assert!(!format!("{events:?}").contains("secret-value"), "{events:?}");
    }
}
