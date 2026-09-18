//! Tests for `Content`.

use std::mem;

use serde::Deserialize;

use super::*;
use crate::DecodeErrorKind;

/// One field of a response, which is where content arrives from the wire.
#[derive(Debug, Deserialize)]
struct Row<'a> {
    #[serde(borrow)]
    value: Content<'a>,
}

fn row(document: &[u8]) -> Result<Row<'_>, DecodeError> {
    codec::decode::<Row<'_>>(document)
}

fn encoded(content: &Content<'_>) -> String {
    let mut buffer = Vec::new();
    codec::encode_into(&mut buffer, content).expect("content always encodes");
    String::from_utf8(buffer).expect("the codec emits UTF-8")
}

#[test]
fn content_stays_small() {
    // One is held per legend entry and per question field, so the size is part
    // of the decode budget. Both shapes fit in the space of a `Cow<str>`: the
    // raw-JSON pointer is narrower, and the discriminant rides in the spare
    // values of the `Cow` tag rather than adding a word.
    assert_eq!(
        mem::size_of::<Content<'_>>(),
        mem::size_of::<Cow<'_, str>>(),
        "Content is {} bytes against {} for a Cow",
        mem::size_of::<Content<'_>>(),
        mem::size_of::<Cow<'_, str>>()
    );
    assert_eq!(mem::size_of::<Content<'_>>(), 24);
}

#[test]
fn a_string_decodes_as_text_and_borrows_the_input() {
    let document = br#"{"value":"neutral or polite"}"#;

    let decoded = row(document).expect("a string is valid content");

    assert_eq!(decoded.value.as_text(), Some("neutral or polite"));
    assert_eq!(decoded.value.as_json(), None);

    let borrowed = decoded.value.as_text().expect("it is text").as_ptr().addr();
    let start = document.as_ptr().addr();
    assert!(
        (start..start + document.len()).contains(&borrowed),
        "text without escapes must point into the response buffer, not a copy"
    );
}

#[test]
fn an_escaped_string_decodes_to_its_value() {
    let document = br#"{"value":"caf\u00e9 \"quoted\" \\ \u2028"}"#;

    let decoded = row(document).expect("escapes are valid content");

    assert_eq!(decoded.value.as_text(), Some("caf\u{e9} \"quoted\" \\ \u{2028}"));
}

#[test]
fn an_object_and_an_array_keep_their_text() {
    let object = br#"{"value":{"label":"ok","weight":2}}"#;
    let array = br#"{"value":["great","best"]}"#;

    let decoded_object = row(object).expect("an object is valid content");
    let decoded_array = row(array).expect("an array is valid content");

    assert_eq!(decoded_object.value.as_text(), None);
    assert_eq!(
        decoded_object.value.as_json().map(RawJson::as_str),
        Some(r#"{"label":"ok","weight":2}"#)
    );
    assert_eq!(decoded_array.value.as_json().map(RawJson::as_str), Some(r#"["great","best"]"#));
}

#[test]
fn every_shape_re_encodes_to_what_it_came_from() {
    for (document, field) in [
        (&br#"{"value":"neutral or polite"}"#[..], r#""neutral or polite""#),
        (&br#"{"value":"caf\u00e9 \"quoted\" \\"}"#[..], "\"caf\u{e9} \\\"quoted\\\" \\\\\""),
        (&br#"{"value":{"label":"ok","weight":2}}"#[..], r#"{"label":"ok","weight":2}"#),
        (&br#"{"value":["great","best"]}"#[..], r#"["great","best"]"#),
    ] {
        let decoded = row(document).expect("valid content");
        assert_eq!(
            encoded(&decoded.value),
            field,
            "round trip of {}",
            String::from_utf8_lossy(document)
        );
    }
}

#[test]
fn a_raw_value_is_spliced_in_rather_than_re_rendered() {
    let document = br#"{"value":{"b":1,"a":[ ],"n":-0.0}}"#;

    let decoded = row(document).expect("an object is valid content");

    assert_eq!(
        encoded(&decoded.value),
        r#"{"b":1,"a":[ ],"n":-0.0}"#,
        "key order, the space and the sign of the zero all survive"
    );
}

#[test]
fn numbers_booleans_and_null_are_refused() {
    for document in [
        &br#"{"value":42}"#[..],
        &br#"{"value":-0.5}"#[..],
        &br#"{"value":true}"#[..],
        &br#"{"value":false}"#[..],
        &br#"{"value":null}"#[..],
    ] {
        let error = row(document).expect_err("only string, object and array are content");
        assert_eq!(
            error.kind(),
            DecodeErrorKind::Data,
            "{} gave {error}",
            String::from_utf8_lossy(document)
        );
        assert_eq!(error.path(), "value");
    }

    assert_eq!(Content::json(&42).expect_err("a number is not content"), ContentError::Shape);
    assert_eq!(Content::json(&true).expect_err("a boolean is not content"), ContentError::Shape);
    assert_eq!(
        Content::json(&Option::<u8>::None).expect_err("null is not content"),
        ContentError::Shape
    );
}

#[test]
fn a_value_that_encodes_to_a_string_is_text() {
    let from_value = Content::json(&"payments or invoices").expect("a string is content");

    assert_eq!(from_value, Content::text("payments or invoices"));
    assert_eq!(from_value.as_text(), Some("payments or invoices"));
}

#[test]
fn a_structured_value_becomes_raw_json() {
    #[derive(Debug, serde::Serialize)]
    struct Criterion {
        label: &'static str,
        weight: u8,
    }

    let content =
        Content::json(&Criterion { label: "ok", weight: 2 }).expect("a struct is content");

    assert_eq!(content.as_json().map(RawJson::as_str), Some(r#"{"label":"ok","weight":2}"#));
    assert_eq!(encoded(&content), r#"{"label":"ok","weight":2}"#);
}

#[test]
fn borrowed_text_can_be_detached_from_its_input() {
    let owned = {
        let document = String::from(r#"{"value":"neutral or polite"}"#);
        let decoded = row(document.as_bytes()).expect("a string is valid content");
        decoded.value.into_owned()
    };

    assert_eq!(owned.as_text(), Some("neutral or polite"));
}

#[test]
fn text_is_escaped_on_the_way_out() {
    let content = Content::text("a \"quoted\" \\ caf\u{e9}");

    assert_eq!(encoded(&content), "\"a \\\"quoted\\\" \\\\ caf\u{e9}\"");
}
