//! The three response representations AC-P0 compares.
//!
//! [`Answers`] is a prototype of the representation plan section 3.3 step 7
//! describes: a hand-written single-pass visitor, order independent, answers
//! vector pre-sized from the number of questions asked, unknown answer types
//! skipped, score levels read as integers straight out of the object keys.
//!
//! [`NaiveAnswers`] is the comparator plan section 5 pins down so it cannot be
//! a strawman: the same codec, `#[serde(tag = "type")]` answers in a
//! `HashMap<String, _>`, maps everywhere.
//!
//! [`Ticket`] is the derived typed form of the same three answers, which is
//! what `#[derive(QuestionSet)]` will generate.

use std::{collections::HashMap, fmt};

use serde::{
    Deserialize,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};

// ---------------------------------------------------------------- shared

/// `{"input_tokens": 12, "output_tokens": 3}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A level label, which the API documents as a string but may widen later.
#[derive(Debug, Clone, PartialEq)]
pub enum LegendValue {
    /// The documented case: a JSON string.
    Text(String),
    /// Anything else, kept as JSON text so an unknown shape is not lost.
    Raw(String),
}

/// `{"friendly": 0.9, "hostile": 0.1}` as pairs, in wire order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NamedProbabilities(pub Vec<(String, f64)>);

/// `{"0": 0.1, "1": 0.1, "2": 0.8}` as pairs, keys read as integers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LevelProbabilities(pub Vec<(u32, f64)>);

/// `{"0": "bad", "1": "ok", "2": "great"}` as pairs, keys read as integers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Legend(pub Vec<(u32, LegendValue)>);

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NoulAnswer {
    pub noul: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: NamedProbabilities,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoreAnswer {
    pub score: f64,
    pub confidence: f64,
    pub legend: Legend,
    pub probabilities: LevelProbabilities,
}

/// One answer of a known type.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Noul(NoulAnswer),
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
}

// ------------------------------------------------- container deserializers

impl<'de> Deserialize<'de> for NamedProbabilities {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = NamedProbabilities;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of option name to probability")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(2));
                while let Some((name, probability)) = map.next_entry::<String, f64>()? {
                    pairs.push((name, probability));
                }
                Ok(NamedProbabilities(pairs))
            }
        }
        deserializer.deserialize_map(V)
    }
}

impl<'de> Deserialize<'de> for LevelProbabilities {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LevelProbabilities;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of score level to probability")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(3));
                while let Some((level, probability)) = map.next_entry::<Level, f64>()? {
                    pairs.push((level.0, probability));
                }
                Ok(LevelProbabilities(pairs))
            }
        }
        deserializer.deserialize_map(V)
    }
}

impl<'de> Deserialize<'de> for Legend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Legend;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of score level to label")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(3));
                while let Some(level) = map.next_key::<Level>()? {
                    pairs.push((level.0, map.next_value_seed(LegendValueSeed)?));
                }
                Ok(Legend(pairs))
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// A score level read straight out of a JSON object key.
///
/// The key arrives as text, so the integer is parsed from `&str` and nothing is
/// allocated for it; a codec that hands the key over as a number reaches
/// `visit_u64` instead and the result is the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Level(u32);

impl<'de> Deserialize<'de> for Level {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Level;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a score level")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Level, E> {
                value.parse().map(Level).map_err(de::Error::custom)
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Level, E> {
                u32::try_from(value).map(Level).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_str(V)
    }
}

/// Reads a legend value as text when it is a JSON string, and as JSON text
/// otherwise.
///
/// The `Raw` rendering is normalized rather than byte-exact: it is built from
/// the decoded value, so whitespace and number formatting are the renderer's.
/// The documented shape is a string, so the fixture never reaches that arm.
struct LegendValueSeed;

impl<'de> DeserializeSeed<'de> for LegendValueSeed {
    type Value = LegendValue;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for LegendValueSeed {
    type Value = LegendValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a level label")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<LegendValue, E> {
        Ok(LegendValue::Text(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<LegendValue, E> {
        Ok(LegendValue::Text(value))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<LegendValue, E> {
        Ok(LegendValue::Raw(value.to_string()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<LegendValue, E> {
        Ok(LegendValue::Raw(value.to_string()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<LegendValue, E> {
        Ok(LegendValue::Raw(value.to_string()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<LegendValue, E> {
        Ok(LegendValue::Raw(value.to_string()))
    }

    fn visit_unit<E: de::Error>(self) -> Result<LegendValue, E> {
        Ok(LegendValue::Raw("null".to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LegendValue, A::Error> {
        let mut rendered = String::from("[");
        while let Some(element) = seq.next_element_seed(LegendValueSeed)? {
            if rendered.len() > 1 {
                rendered.push(',');
            }
            push_rendered(&mut rendered, &element);
        }
        rendered.push(']');
        Ok(LegendValue::Raw(rendered))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LegendValue, A::Error> {
        let mut rendered = String::from("{");
        while let Some(key) = map.next_key::<String>()? {
            if rendered.len() > 1 {
                rendered.push(',');
            }
            push_json_string(&mut rendered, &key);
            rendered.push(':');
            let value = map.next_value_seed(LegendValueSeed)?;
            push_rendered(&mut rendered, &value);
        }
        rendered.push('}');
        Ok(LegendValue::Raw(rendered))
    }
}

fn push_rendered(out: &mut String, value: &LegendValue) {
    match value {
        LegendValue::Text(text) => push_json_string(out, text),
        LegendValue::Raw(raw) => out.push_str(raw),
    }
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => out.push_str(&format!("\\u{:04x}", control as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
}

// ----------------------------------------------- the prototype (step 6)

/// The answers of one response, in wire order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answers(pub Vec<(String, Answer)>);

/// The whole response in the planned owned representation.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub model: String,
    pub usage: Usage,
    pub answers: Answers,
}

/// How many answers the caller asked for, used to size the answers vector
/// exactly once instead of growing it.
#[derive(Debug, Clone, Copy)]
pub struct AskedFor(pub usize);

impl<'de> DeserializeSeed<'de> for AskedFor {
    type Value = Response;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Response, D::Error> {
        deserializer.deserialize_map(ResponseVisitor(self))
    }
}

struct ResponseVisitor(AskedFor);

impl<'de> Visitor<'de> for ResponseVisitor {
    type Value = Response;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a System One response")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Response, A::Error> {
        let mut model = None;
        let mut usage = None;
        let mut answers = None;

        // The response fields may arrive in any order, so each one is taken as
        // it appears and the check for completeness happens at the end.
        while let Some(field) = map.next_key::<&str>()? {
            match field {
                "model" => model = Some(map.next_value::<String>()?),
                "usage" => usage = Some(map.next_value::<Usage>()?),
                "answers" => answers = Some(map.next_value_seed(AnswersSeed(self.0))?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        Ok(Response {
            model: model.ok_or_else(|| de::Error::missing_field("model"))?,
            usage: usage.ok_or_else(|| de::Error::missing_field("usage"))?,
            answers: answers.unwrap_or_default(),
        })
    }
}

struct AnswersSeed(AskedFor);

impl<'de> DeserializeSeed<'de> for AnswersSeed {
    type Value = Answers;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Answers, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for AnswersSeed {
    type Value = Answers;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object of question name to answer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Answers, A::Error> {
        // One allocation for the whole vector, sized from what was asked.
        let mut answers = Vec::with_capacity(self.0.0);
        while let Some(name) = map.next_key::<String>()? {
            match map.next_value_seed(AnswerSeed)? {
                Some(answer) => answers.push((name, answer)),
                // An answer of a type this version does not know is dropped,
                // exactly as `_prepare_system_one_response` does upstream. The
                // raw body still carries it.
                None => drop(name),
            }
        }
        Ok(Answers(answers))
    }
}

/// One answer, dispatched on its `type` field wherever that field appears.
struct AnswerSeed;

impl<'de> DeserializeSeed<'de> for AnswerSeed {
    type Value = Option<Answer>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for AnswerSeed {
    type Value = Option<Answer>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an answer object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        // Every field is taken as it arrives and held in a local, so `type` may
        // come last without the object being buffered into a value tree, which
        // is exactly what `#[serde(tag = "type")]` would do instead.
        let mut kind: Option<AnswerKind> = None;
        let mut noul = None;
        let mut choice = None;
        let mut confidence = None;
        let mut score = None;
        let mut named_probabilities: Option<NamedProbabilities> = None;
        let mut level_probabilities: Option<LevelProbabilities> = None;
        let mut legend: Option<Legend> = None;

        while let Some(field) = map.next_key::<&str>()? {
            match field {
                "type" => kind = Some(map.next_value::<AnswerKind>()?),
                "noul" => noul = Some(map.next_value::<f64>()?),
                "choice" => choice = Some(map.next_value::<String>()?),
                "confidence" => confidence = Some(map.next_value::<f64>()?),
                "score" => score = Some(map.next_value::<f64>()?),
                "legend" => legend = Some(map.next_value::<Legend>()?),
                // The probabilities map is keyed by option name for a choice
                // and by level for a score. Which one applies is known only
                // once `type` has been seen, so both readings are kept and the
                // one the type does not need is never built: the decision is
                // made here from `kind`, which is `None` only when `type` comes
                // later, and then the score reading is retried from the named
                // form at dispatch.
                "probabilities" => match kind {
                    Some(AnswerKind::Score) => {
                        level_probabilities = Some(map.next_value::<LevelProbabilities>()?);
                    }
                    _ => named_probabilities = Some(map.next_value::<NamedProbabilities>()?),
                },
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let answer = match kind {
            Some(AnswerKind::Noul) => Answer::Noul(NoulAnswer {
                noul: noul.ok_or_else(|| de::Error::missing_field("noul"))?,
            }),
            Some(AnswerKind::Choice) => Answer::Choice(ChoiceAnswer {
                choice: choice.ok_or_else(|| de::Error::missing_field("choice"))?,
                confidence: confidence.ok_or_else(|| de::Error::missing_field("confidence"))?,
                probabilities: named_probabilities.unwrap_or_default(),
            }),
            Some(AnswerKind::Score) => {
                let probabilities = match (level_probabilities, named_probabilities) {
                    (Some(levels), _) => levels,
                    // `type` arrived after `probabilities`, so the keys were
                    // read as names and are converted here. The fixture puts
                    // `type` first, so this path costs nothing in the measured
                    // case, and it is what keeps the visitor order independent.
                    (None, Some(named)) => LevelProbabilities(
                        named
                            .0
                            .into_iter()
                            .map(|(name, probability)| {
                                name.parse()
                                    .map(|level| (level, probability))
                                    .map_err(de::Error::custom)
                            })
                            .collect::<Result<Vec<_>, A::Error>>()?,
                    ),
                    (None, None) => LevelProbabilities::default(),
                };
                Answer::Score(ScoreAnswer {
                    score: score.ok_or_else(|| de::Error::missing_field("score"))?,
                    confidence: confidence.ok_or_else(|| de::Error::missing_field("confidence"))?,
                    legend: legend.unwrap_or_default(),
                    probabilities,
                })
            }
            // An unknown or missing type is not an error: the answer is
            // dropped and the caller can still recover it from the raw body.
            Some(AnswerKind::Unknown) | None => return Ok(None),
        };
        Ok(Some(answer))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerKind {
    Noul,
    Choice,
    Score,
    Unknown,
}

impl<'de> Deserialize<'de> for AnswerKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = AnswerKind;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an answer type")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<AnswerKind, E> {
                Ok(match value {
                    "noul" => AnswerKind::Noul,
                    "choice" => AnswerKind::Choice,
                    "score" => AnswerKind::Score,
                    _ => AnswerKind::Unknown,
                })
            }
        }
        deserializer.deserialize_str(V)
    }
}

// --------------------------------------------- the naive comparator (7)

/// The comparator plan section 5 specifies: internally tagged answers in a
/// `HashMap`, with maps for every container.
///
/// Its fields are written by the decode and never read: the comparator exists
/// to be measured, not to be consumed.
#[expect(dead_code, reason = "the comparator is measured, never read")]
#[derive(Debug, Deserialize)]
pub struct NaiveResponse {
    pub model: String,
    pub usage: Usage,
    pub answers: HashMap<String, NaiveAnswer>,
}

#[expect(dead_code, reason = "the comparator is measured, never read")]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NaiveAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        confidence: f64,
        probabilities: HashMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        legend: HashMap<String, String>,
        probabilities: HashMap<String, f64>,
    },
}

// ------------------------------------------- the derived typed form (8)

/// What `#[derive(QuestionSet)]` will generate: field dispatch instead of a
/// map, with the same containers as the prototype so that the comparison is
/// about dispatch and nothing else.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Ticket {
    pub spam: NoulAnswer,
    pub tone: ChoiceAnswer,
    pub quality: ScoreAnswer,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TypedResponse {
    pub model: String,
    pub usage: Usage,
    pub answers: Ticket,
}
