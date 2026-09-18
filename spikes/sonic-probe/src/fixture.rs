//! The JSON documents every S1 scenario decodes, and the probe types they
//! decode into.
//!
//! [`RESULT`] is the upstream Python SDK's response fixture
//! (`tests/test_clients.py:42-56`) written out as compact JSON, which is the
//! shape the live API returns. The probe types below borrow every string out
//! of that buffer and hold no owned collection, so decoding them allocates
//! only what the codec itself allocates; that is the constant `C` of AC-P2.

// Every probe field is written by the decode and read only through `Debug`,
// which dead-code analysis does not count as a use. The types exist to give
// the codec a shape to fill, so the whole module opts out at once.
#![expect(dead_code, reason = "probe fields are populated by the decode, never read")]

use serde::Deserialize;

/// The upstream `RESULT` fixture, compact.
pub const RESULT: &[u8] = br#"{"model":"jev-latest","usage":{"input_tokens":12,"output_tokens":3},"answers":{"spam":{"type":"noul","noul":0.98},"tone":{"type":"choice","choice":"friendly","confidence":0.9,"probabilities":{"friendly":0.9,"hostile":0.1}},"quality":{"type":"score","score":1.7,"confidence":0.8,"legend":{"0":"bad","1":"ok","2":"great"},"probabilities":{"0":0.1,"1":0.1,"2":0.8}}}}"#;

/// [`RESULT`] with `answers.spam.noul` removed.
pub const RESULT_MISSING_NOUL: &[u8] = br#"{"model":"jev-latest","usage":{"input_tokens":12,"output_tokens":3},"answers":{"spam":{"type":"noul"},"tone":{"type":"choice","choice":"friendly","confidence":0.9,"probabilities":{"friendly":0.9,"hostile":0.1}},"quality":{"type":"score","score":1.7,"confidence":0.8,"legend":{"0":"bad","1":"ok","2":"great"},"probabilities":{"0":0.1,"1":0.1,"2":0.8}}}}"#;

/// A `GET /v1/models` payload whose second entry has a numeric `name`.
pub const MODELS_BAD_NAME: &[u8] = br#"{"models":[{"name":"jev-latest","description":"Fast model","release_date":"2026-08-01"},{"name":123,"description":"Older model","release_date":"2026-01-01"}]}"#;

/// The whole response, borrowed. No `String`, no `Vec`, no map: every field is
/// a `&str`, an `f64` or an integer, so the only allocations left in a decode
/// are the codec's own.
#[derive(Debug, Deserialize)]
pub struct BorrowedResponse<'a> {
    pub model: &'a str,
    pub usage: BorrowedUsage,
    #[serde(borrow)]
    pub answers: BorrowedAnswers<'a>,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedAnswers<'a> {
    #[serde(borrow)]
    pub spam: BorrowedNoul<'a>,
    #[serde(borrow)]
    pub tone: BorrowedChoice<'a>,
    #[serde(borrow)]
    pub quality: BorrowedScore<'a>,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedNoul<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub noul: f64,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedChoice<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub choice: &'a str,
    pub confidence: f64,
    pub probabilities: ChoiceProbabilities,
}

/// The fixture's two options, named so that the decode needs no map.
#[derive(Debug, Deserialize)]
pub struct ChoiceProbabilities {
    pub friendly: f64,
    pub hostile: f64,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedScore<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub score: f64,
    pub confidence: f64,
    #[serde(borrow)]
    pub legend: BorrowedLegend<'a>,
    pub probabilities: ScoreProbabilities,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedLegend<'a> {
    #[serde(rename = "0")]
    pub zero: &'a str,
    #[serde(rename = "1")]
    pub one: &'a str,
    #[serde(rename = "2")]
    pub two: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct ScoreProbabilities {
    #[serde(rename = "0")]
    pub zero: f64,
    #[serde(rename = "1")]
    pub one: f64,
    #[serde(rename = "2")]
    pub two: f64,
}

/// `GET /v1/models`, borrowed apart from the vector itself.
#[derive(Debug, Deserialize)]
pub struct BorrowedModels<'a> {
    #[serde(borrow)]
    pub models: Vec<BorrowedModelCard<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct BorrowedModelCard<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub release_date: &'a str,
}

/// A score level, parsed from the `&str` form the codec hands a map key.
///
/// The SDK will need this shape rather than `u32` directly if the codec
/// refuses to reinterpret an object key as an integer: a newtype can accept
/// the string and parse it, at the cost of one visitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LevelKey(pub u32);

impl<'de> Deserialize<'de> for LevelKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = LevelKey;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a decimal score level in a JSON string")
            }

            fn visit_str<E>(self, value: &str) -> Result<LevelKey, E>
            where
                E: serde::de::Error,
            {
                value.parse().map(LevelKey).map_err(serde::de::Error::custom)
            }

            // A codec that hands an object key over as an integer rather than
            // as text reaches this arm instead, so the newtype works either
            // way and the scenario reports which arm ran.
            fn visit_u64<E>(self, value: u64) -> Result<LevelKey, E>
            where
                E: serde::de::Error,
            {
                u32::try_from(value).map(LevelKey).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// A struct holding a single borrowed string, used to test whether a value
/// containing escape sequences can still be borrowed.
#[derive(Debug, Deserialize)]
pub struct BorrowedField<'a> {
    pub s: &'a str,
}

/// The same field as a `Cow`, which can fall back to owning the unescaped form.
#[derive(Debug, Deserialize)]
pub struct CowField<'a> {
    #[serde(borrow)]
    pub s: std::borrow::Cow<'a, str>,
}

/// A control target for S1(c): the same fixture decoded into owned strings and
/// maps. Its block count is not a budget, it only shows that a reported zero
/// for the borrowed probe is a measurement and not a broken harness.
#[derive(Debug, Deserialize)]
pub struct OwnedControl {
    pub model: String,
    pub usage: BorrowedUsage,
    pub answers: std::collections::HashMap<String, sonic_rs::Value>,
}
