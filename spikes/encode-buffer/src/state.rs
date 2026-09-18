//! The `state` values the S6 scenarios encode.

use serde::Serialize;

/// English-like filler with the punctuation that makes escaping non-trivial:
/// straight quotes, newlines and multi-byte characters.
const PARAGRAPH: &str = "The customer wrote: \"my invoice is wrong again\", and attached a café \
                         receipt for €12.50 — the third one this month.\nSupport replied within \
                         four minutes, marked the thread urgent and asked for the order id.\n";

/// A `state` argument in one of the two shapes the SDK has to encode.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum State {
    /// A plain string, which the two-pass variants can size exactly.
    Text(String),
    /// A structure, whose serialized length is not known before it is written.
    Object(ObjectState),
}

/// An object `state`: a short field beside a large one.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectState {
    pub subject: String,
    pub body: String,
}

impl State {
    /// Builds the scenario named on the command line and in the ledger.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "s1k" => Some(Self::Text(english_like(1024))),
            "s64k" => Some(Self::Text(english_like(65_536))),
            "s1m" => Some(Self::Text(english_like(1_048_576))),
            "obj1m" => Some(Self::Object(ObjectState {
                subject: english_like(64),
                body: english_like(1_048_576),
            })),
            _ => None,
        }
    }

    /// Bytes of `state` payload, for the tables in the ledger.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Object(object) => object.subject.len() + object.body.len(),
        }
    }
}

/// Exactly `target_len` bytes of [`PARAGRAPH`], cut on a character boundary and
/// padded with ASCII so every variant is measured on the same input size.
#[must_use]
pub fn english_like(target_len: usize) -> String {
    let mut text = String::with_capacity(target_len + PARAGRAPH.len());
    while text.len() < target_len {
        text.push_str(PARAGRAPH);
    }

    let mut cut = target_len;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    while text.len() < target_len {
        text.push('.');
    }
    text
}
