use http::HeaderValue;
use serde_json::json;

use super::*;
use crate::{codec, de::decode_system_one};

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("../tests/fixtures/result.json");

fn decode(body: &[u8]) -> SystemOneResponse {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-export"));
    decode_system_one(Bytes::copy_from_slice(body), StatusCode::OK, headers, 3, None)
        .expect("the body decodes")
}

fn through_the_codec<T: Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    codec::encode_into(&mut out, value).expect("the value encodes");
    out
}

/// `test_response_serialization_excludes_http_metadata`, the System One half.
#[test]
fn serializing_a_response_writes_the_payload_and_not_the_http_metadata() {
    let response = decode(RESULT);
    let body: serde_json::Value = serde_json::from_slice(RESULT).expect("the fixture parses");
    // Iterating the typed views first must not change what is serialized.
    assert_eq!(response.answers().choices().count(), 1);
    assert_eq!(response.answers().scores().count(), 1);

    let through_serde_json = serde_json::to_value(&response).expect("the response serializes");
    assert_eq!(through_serde_json, body);
    let keys: Vec<&String> =
        through_serde_json.as_object().expect("a response is an object").keys().collect();
    assert_eq!(keys, ["answers", "model", "usage"]);

    let encoded = through_the_codec(&response);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&encoded).expect("JSON"), body);
    assert_eq!(
        std::str::from_utf8(&encoded).expect("UTF-8"),
        std::str::from_utf8(RESULT).expect("UTF-8").trim_end(),
        "this codec writes the fixture back byte for byte"
    );

    let restored = decode(&encoded);
    assert_eq!(restored.model(), response.model());
    assert_eq!(restored.usage(), response.usage());
    assert_eq!(restored.answers(), response.answers());
    assert_eq!(restored.meta().request_id(), Some("req-export"));
    assert_eq!(&response.meta().raw_body()[..], RESULT);
}

/// `test_answer_attributes_and_dictionary_types`.
#[test]
fn each_answer_serializes_with_its_type() {
    let noul = NoulAnswer::new(0.98);
    let choice = ChoiceAnswer::new("billing", 0.9, [("billing", 0.9), ("support", 0.1)]);

    assert_eq!(noul.noul(), 0.98);
    assert_eq!(choice.choice(), "billing");
    assert_eq!(choice.confidence(), 0.9);
    assert_eq!(choice.probabilities().collect::<Vec<_>>(), [("billing", 0.9), ("support", 0.1)]);
    assert_eq!(
        serde_json::to_value(noul).expect("serializes"),
        json!({"type": "noul", "noul": 0.98})
    );
    assert_eq!(
        serde_json::to_value(&choice).expect("serializes"),
        json!({"type": "choice", "choice": "billing", "confidence": 0.9, "probabilities": {"billing": 0.9, "support": 0.1}})
    );
    assert_eq!(
        String::from_utf8(through_the_codec(&choice)).expect("UTF-8"),
        r#"{"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.9,"support":0.1}}"#
    );
}

/// `test_response_preserves_nested_json`.
#[test]
fn a_structured_legend_description_serializes_as_data() {
    let description =
        Content::json(&json!({"examples": ["a", {"note": null}]})).expect("an object is content");
    let answer = ScoreAnswer::new(0.0, 1.0, [(0, description.clone())], [(0, 1.0)]);

    assert_eq!(answer.description(0), Some(&description));
    assert_eq!(answer.probability(0), Some(1.0));
    let exported = json!({
        "type": "score",
        "score": 0.0,
        "confidence": 1.0,
        "legend": {"0": {"examples": ["a", {"note": null}]}},
        "probabilities": {"0": 1.0},
    });
    assert_eq!(serde_json::to_value(&answer).expect("serializes through serde_json"), exported);
    let encoded = through_the_codec(&answer);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&encoded).expect("JSON"), exported);
    assert_eq!(
        String::from_utf8(encoded).expect("UTF-8"),
        r#"{"type":"score","score":0.0,"confidence":1.0,"legend":{"0":{"examples":["a",{"note":null}]}},"probabilities":{"0":1.0}}"#
    );

    let result = Answers::from_iter([("q", Answer::from(answer.clone()))]);
    assert_eq!(result.choices().count(), 0);
    assert_eq!(result.score("q"), Some(&answer));
}

#[test]
fn a_score_built_by_hand_is_sorted_by_level() {
    let answer = ScoreAnswer::new(
        1.5,
        0.5,
        [(2, Content::text("high")), (0, Content::text("low")), (1, Content::text("mid"))],
        [(1, 0.5), (2, 0.25), (0, 0.25)],
    );

    assert_eq!(answer.score(), 1.5);
    assert_eq!(answer.confidence(), 0.5);
    let legend: Vec<(u32, Option<&str>)> =
        answer.legend().map(|(level, description)| (level, description.as_text())).collect();
    assert_eq!(legend, [(0, Some("low")), (1, Some("mid")), (2, Some("high"))]);
    assert_eq!(answer.probabilities().collect::<Vec<_>>(), [(0, 0.25), (1, 0.5), (2, 0.25)]);
    assert_eq!(answer.probabilities().next_back(), Some((2, 0.25)));
    assert_eq!(answer.description(3), None);
    assert_eq!(answer.probability(5), None);
}

#[test]
fn insertion_by_level_is_stable_for_a_repeated_level() {
    let mut entries = Vec::new();
    for (level, tag) in [(3, 'a'), (1, 'b'), (3, 'c'), (0, 'd'), (1, 'e')] {
        insert_by_level(&mut entries, level, tag);
    }

    assert_eq!(entries, [(0, 'd'), (1, 'b'), (1, 'e'), (3, 'a'), (3, 'c')]);
}

#[test]
fn answers_are_looked_up_by_name_and_filtered_by_kind_without_copying() {
    let answers = Answers::from_iter([
        ("spam", Answer::from(NoulAnswer::new(0.1))),
        ("tone", Answer::from(ChoiceAnswer::new("calm", 0.8, [("calm", 0.8), ("angry", 0.2)]))),
        ("urgency", Answer::from(ScoreAnswer::new(1.0, 0.7, [], [(1, 1.0)]))),
        ("billing", Answer::from(NoulAnswer::new(0.9))),
    ]);

    assert_eq!(answers.len(), 4);
    assert!(!answers.is_empty());
    assert_eq!(answers.names().collect::<Vec<_>>(), ["spam", "tone", "urgency", "billing"]);
    assert_eq!(answers.names().next_back(), Some("billing"));
    assert_eq!(
        answers.nouls().map(|(name, answer)| (name, answer.noul())).collect::<Vec<_>>(),
        [("spam", 0.1), ("billing", 0.9)]
    );
    assert_eq!(answers.choices().map(|(name, _)| name).collect::<Vec<_>>(), ["tone"]);
    assert_eq!(answers.scores().map(|(name, _)| name).collect::<Vec<_>>(), ["urgency"]);
    assert_eq!(answers.iter().len(), 4);

    assert_eq!(answers.noul("billing").map(NoulAnswer::noul), Some(0.9));
    assert_eq!(answers.choice("tone").and_then(|tone| tone.probability("angry")), Some(0.2));
    assert_eq!(answers.choice("tone").and_then(|tone| tone.probability("bored")), None);
    assert_eq!(answers.score("urgency").map(ScoreAnswer::score), Some(1.0));
    // A name of another kind, or no such name, is `None` rather than a panic.
    assert_eq!(answers.noul("tone"), None);
    assert_eq!(answers.choice("urgency"), None);
    assert_eq!(answers.score("spam"), None);
    assert_eq!(answers.get("missing"), None);

    let tone = answers.get("tone").expect("tone is there");
    assert_eq!((tone.as_noul(), tone.as_score()), (None, None));
    let urgency = answers.get("urgency").expect("urgency is there");
    assert_eq!((urgency.as_noul(), urgency.as_choice()), (None, None));
    let spam = answers.get("spam").expect("spam is there");
    assert_eq!((spam.as_choice(), spam.as_score()), (None, None));

    assert!(Answers::default().is_empty());
    assert_eq!(serde_json::to_value(Answers::default()).expect("serializes"), json!({}));
}

#[test]
fn usage_counts_are_optional_and_serialize_as_null_when_absent() {
    let usage = Usage::new(Some(120), None);

    assert_eq!(usage.input_tokens(), Some(120));
    assert_eq!(usage.output_tokens(), None);
    assert_eq!(
        serde_json::to_value(usage).expect("serializes"),
        json!({"input_tokens": 120, "output_tokens": null})
    );
    assert_eq!(Usage::default(), Usage::new(None, None));
}

#[test]
fn the_meta_keeps_the_http_response_and_prints_none_of_its_contents() {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-7"));
    headers.insert("set-cookie", HeaderValue::from_static("session=secret-cookie"));
    let meta =
        ResponseMeta::new(StatusCode::OK, headers.clone(), Bytes::from_static(b"{\"secret\":1}"));

    assert_eq!(meta.status(), StatusCode::OK);
    assert_eq!(meta.headers(), &headers);
    assert_eq!(meta.request_id(), Some("req-7"));
    assert_eq!(&meta.raw_body()[..], b"{\"secret\":1}");
    let rendered = format!("{meta:?}");
    assert_eq!(
        rendered,
        "ResponseMeta { status: 200, request_id: Some(\"req-7\"), headers: <2 headers>, body: <12 bytes> }"
    );
    assert!(!rendered.contains("secret"), "{rendered}");

    let (status, parts_headers, body) = meta.clone().into_parts();
    assert_eq!(
        (status, &parts_headers, &body[..]),
        (StatusCode::OK, &headers, &b"{\"secret\":1}"[..])
    );
}

#[test]
fn a_request_id_that_is_absent_or_not_text_is_none() {
    let absent = ResponseMeta::new(StatusCode::OK, HeaderMap::new(), Bytes::new());
    assert_eq!(absent.request_id(), None);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-typesafe-request-id",
        HeaderValue::from_bytes(b"req-\xff").expect("an opaque header value is valid"),
    );
    let opaque = ResponseMeta::new(StatusCode::OK, headers, Bytes::new());
    assert_eq!(opaque.request_id(), None);
}

#[test]
fn a_response_gives_up_its_answers_and_compares_whole() {
    let response = decode(RESULT);
    let copy = response.clone();

    assert_eq!(copy, response);
    let answers = copy.into_answers();
    assert_eq!(&answers, response.answers());

    let other = SystemOneResponse::from_parts(
        response.model().to_owned(),
        *response.usage(),
        answers,
        ResponseMeta::new(StatusCode::OK, HeaderMap::new(), response.meta().raw_body().clone()),
    );
    assert_ne!(other, response, "the HTTP metadata is part of equality");
}
