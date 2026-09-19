use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

use super::*;
use crate::codec;

/// Short and long, single- and multi-byte, and text that JSON must escape.
const SAMPLES: [&str; 8] = [
    "",
    "tone",
    "exactly_twenty_four_byte",
    "twenty_five_bytes_exactly",
    "a question name that is well past the inline limit of a name",
    "\u{8cea}\u{554f}\u{306e}\u{540d}\u{524d}",
    "\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{8cea}\u{554f}\u{306e}\u{540d}",
    "tab\there, a \"quote\", a backslash \\ and a control \u{0001}",
];

#[test]
fn a_name_holds_the_text_it_was_made_from_whatever_it_was_made_from() {
    for text in SAMPLES {
        let from_str = Name::from(text);
        let from_string = Name::from(text.to_owned());
        let from_borrowed = Name::from(Cow::Borrowed(text));
        let from_owned = Name::from(Cow::<str>::Owned(text.to_owned()));

        assert_eq!(from_str.as_str(), text, "from &str: {text:?}");
        assert_eq!(from_string.as_str(), text, "from String: {text:?}");
        assert_eq!(from_borrowed.as_str(), text, "from a borrowed Cow: {text:?}");
        assert_eq!(from_owned.as_str(), text, "from an owned Cow: {text:?}");
        assert_eq!(from_str, from_string, "{text:?}");
        assert_eq!(from_str.clone(), from_owned, "{text:?}");
    }
    assert_ne!(Name::from("tone"), Name::from("tones"));
    assert_ne!(Name::from("exactly_twenty_four_byte"), Name::from("exactly_twenty_four_bytes"));
}

#[test]
fn a_name_prints_and_serializes_as_the_string_it_replaced() {
    for text in SAMPLES {
        let name = Name::from(text);
        let string = text.to_owned();

        assert_eq!(format!("{name:?}"), format!("{string:?}"), "Debug of {text:?}");
        assert_eq!(format!("{name:#?}"), format!("{string:#?}"), "alternate Debug of {text:?}");
        assert_eq!(
            serde_json::to_string(&name).expect("a name serializes"),
            serde_json::to_string(&string).expect("a string serializes"),
            "serde_json of {text:?}"
        );
        let (mut through_codec, mut string_through_codec) = (Vec::new(), Vec::new());
        codec::encode_into(&mut through_codec, &name).expect("a name encodes");
        codec::encode_into(&mut string_through_codec, &string).expect("a string encodes");
        assert_eq!(through_codec, string_through_codec, "the codec's output for {text:?}");
    }
}

#[test]
fn a_name_decodes_from_a_json_string_escaped_or_not() {
    for text in SAMPLES {
        let json = serde_json::to_string(text).expect("a string serializes");
        let through_codec: Name = codec::decode(json.as_bytes()).expect("a JSON string decodes");
        let through_serde_json: Name = serde_json::from_str(&json).expect("a JSON string decodes");
        assert_eq!(through_codec.as_str(), text, "the codec, from {json}");
        assert_eq!(through_serde_json.as_str(), text, "serde_json, from {json}");
    }
}

#[test]
fn a_value_that_is_not_a_string_is_refused_as_a_string_refuses_it() {
    for json in ["12", "null", "true", "[\"a\"]", "{\"a\":1}"] {
        let as_name = codec::decode::<Name>(json.as_bytes()).expect_err("not a string");
        let as_string = codec::decode::<String>(json.as_bytes()).expect_err("not a string");
        assert_eq!(as_name.to_string(), as_string.to_string(), "the error for {json}");
        assert_eq!(as_name.kind(), as_string.kind(), "the error kind for {json}");

        let as_name = serde_json::from_str::<Name>(json).expect_err("not a string");
        let as_string = serde_json::from_str::<String>(json).expect_err("not a string");
        assert_eq!(as_name.to_string(), as_string.to_string(), "serde_json's error for {json}");
    }
}

/// Every Rust file of the package, outside `target`.
fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// `src/name.rs` is the one file that names the small-string crate, so that
/// replacing the crate stays a change to that file alone. The check reads
/// every Rust file of this package - library, tests, benches and the derive
/// crate - because an integration test or a bench can name a dependency of
/// the library too.
#[test]
fn only_this_module_names_the_small_string_crate() {
    // Spelled in two pieces so this file does not name it either.
    let needle = concat!("compact", "_str");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed = root.join("src").join("name.rs");

    let mut files = Vec::new();
    for dir in ["src", "tests", "benches", "crates"] {
        let dir = root.join(dir);
        if dir.is_dir() {
            rust_files(&dir, &mut files);
        }
    }
    assert!(files.contains(&allowed), "the scan did not see {}", allowed.display());
    assert!(files.len() > 20, "the scan found only {} files: {files:?}", files.len());

    let offenders: Vec<String> = files
        .iter()
        .filter(|path| **path != allowed)
        .filter_map(|path| {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let lines: Vec<String> = text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(needle))
                .map(|(number, line)| format!("{}:{}: {line}", path.display(), number + 1))
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        })
        .collect();
    assert!(offenders.is_empty(), "only src/name.rs may name {needle}:\n{}", offenders.join("\n"));

    let own = fs::read_to_string(&allowed).expect("src/name.rs reads");
    assert!(own.contains(needle), "src/name.rs no longer names {needle}; update this check");
}
