//! What decoding one response costs in allocations.
//!
//! The budget is stated in dhat's `total_blocks` and `total_bytes` deltas, and
//! it is measured on the identical decodes after one warm-up of the same
//! shape, exactly as the encode budget is: [`support::RUNS`] of them, held to
//! the stable minimum, for the reason `support` gives.
//!
//! The fixture is the upstream `RESULT` (three answers: a noul, a choice, a
//! score). Every block it costs is one the answer representation asks for -
//! decoding it into a fully borrowed type costs none - so the count below is a
//! count of `Vec`s and of strings too long to be stored inline. The names (the
//! model, the question names, the choice's pick and option names) are stored
//! inline up to 24 bytes on a 64-bit target, and every name in the fixture is
//! shorter; the level descriptions are `Content`, which owns a `String`:
//!
//! | item | blocks |
//! | --- | ---: |
//! | model name (inline) | 0 |
//! | answers vector, sized from the question count | 1 |
//! | three question names (inline) | 0 |
//! | the choice's pick (inline) | 0 |
//! | the choice's probabilities vector | 1 |
//! | the choice's two option names (inline) | 0 |
//! | the score's legend vector | 1 |
//! | the score's three level descriptions | 3 |
//! | the score's probabilities vector | 1 |
//! | **total** | **7** |
//!
//! One profiler exists per process, so all of it runs in a single test.

// A test target of the root package is found without a manifest entry, and the
// manifest is not this file's to change; gating the whole file on the feature
// is what a `required-features` entry would otherwise do.
#![cfg(feature = "internals")]

mod support;

use std::{collections::HashMap, fmt};

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use serde::{
    Deserialize,
    de::{self, Deserializer, IgnoredAny, MapAccess, Visitor},
};
use typesafe_sdk::{
    __internals as sdk,
    de::{AnswerContext, AnswerSet},
    response::{Answers, ChoiceAnswer, NoulAnswer, ScoreAnswer, SystemOneResponse},
};

use crate::support::{Measured, measure_min};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `RESULT` of `tests/test_clients.py:42-56`, as the upstream test sends it.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// The number of questions the request asked.
const QUESTIONS: usize = 3;

/// The decode budget: blocks, bytes, and the ratio to the comparator's blocks.
/// The block count is exact, so one more `String` or `Vec` fails it. The byte
/// bound is the measured 578 bytes plus 12.5%, rounded down, close to the
/// headroom the first budget had (700 over a measured 626, 11.8%), so a
/// regression of 100 bytes that adds no block fails it.
const MAX_BLOCKS: u64 = 7;
const MAX_BYTES: u64 = 650;
const MAX_RATIO_TO_NAIVE: f64 = 0.7;

/// Decodes the fixture once through `decode` to warm up and reports what each
/// identical decode after it costs. The response and the inputs are made
/// outside the measured section: a response body arrives as `Bytes` and the
/// headers as a `HeaderMap`, and both are moved in, not copied.
fn second_decode<T, F>(label: &str, decode: F) -> Measured
where
    T: fmt::Debug,
    F: Fn(Bytes, HeaderMap) -> T,
{
    drop(decode(Bytes::from_static(RESULT), HeaderMap::new()));

    let (measured, value) = measure_min(
        label,
        || (Bytes::from_static(RESULT), HeaderMap::new()),
        |(body, headers)| decode(body, headers),
    );
    println!("{label:<44} blocks={:>3} bytes={:>5}", measured.blocks, measured.bytes);
    drop(value);
    measured
}

// ------------------------------------------------------ naive comparator

/// A comparator that is not a strawman: the same codec, answers internally
/// tagged by `type` in a `HashMap<String, _>`, maps for every container.
#[expect(dead_code, reason = "the comparator is decoded to be measured and is never read")]
#[derive(Debug, Deserialize)]
struct NaiveResponse {
    model: String,
    usage: NaiveUsage,
    answers: HashMap<String, NaiveAnswer>,
}

#[expect(dead_code, reason = "the comparator is decoded to be measured and is never read")]
#[derive(Debug, Deserialize)]
struct NaiveUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[expect(dead_code, reason = "the comparator is decoded to be measured and is never read")]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum NaiveAnswer {
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

// ------------------------------------------------- a struct answer set

/// The three answers as a struct, read through the [`AnswerSet`]
/// implementation a derived question set gets: field dispatch on the key, no
/// map, no name strings.
#[derive(Debug)]
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
                        TicketField::Spam if spam.is_none() => spam = Some(map.next_value()?),
                        TicketField::Tone if tone.is_none() => tone = Some(map.next_value()?),
                        TicketField::Quality if quality.is_none() => {
                            quality = Some(map.next_value()?);
                        }
                        // An answer the struct has no field for, or a later
                        // answer to a question already read: the first answer
                        // of a name is the one kept.
                        _ => {
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

// ------------------------------------------------------------------ test

#[test]
fn decoding_the_three_answer_fixture_stays_within_its_budget() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let answers = second_decode("SystemOneResponse<Answers>", |body, headers| {
        sdk::decode_system_one::<Answers>(body, StatusCode::OK, headers, QUESTIONS)
            .expect("the fixture decodes")
    });
    let naive = second_decode("naive: #[serde(tag)] answers in a HashMap", |_, _| {
        sdk::decode::<NaiveResponse>(RESULT).expect("the fixture decodes as the comparator")
    });
    let typed = second_decode("SystemOneResponse<Ticket>, field dispatch", |body, headers| {
        let response: SystemOneResponse<Ticket> =
            sdk::decode_system_one(body, StatusCode::OK, headers, QUESTIONS)
                .expect("the fixture decodes as a Ticket");
        response
    });

    let ratio = answers.blocks as f64 / naive.blocks as f64;
    println!(
        "budget: blocks {} <= {MAX_BLOCKS}, bytes {} <= {MAX_BYTES}, ratio to naive {ratio:.2} <= \
         {MAX_RATIO_TO_NAIVE}; struct set {} < {}",
        answers.blocks, answers.bytes, typed.blocks, answers.blocks
    );

    assert!(
        answers.blocks <= MAX_BLOCKS,
        "decoding into Answers cost {} blocks, over the budget of {MAX_BLOCKS}",
        answers.blocks
    );
    assert!(
        answers.bytes <= MAX_BYTES,
        "decoding into Answers allocated {} bytes, over the budget of {MAX_BYTES}",
        answers.bytes
    );
    assert!(
        ratio <= MAX_RATIO_TO_NAIVE,
        "decoding into Answers cost {} blocks against the comparator's {}: a ratio of {ratio:.2}",
        answers.blocks,
        naive.blocks
    );
    assert!(
        typed.blocks < answers.blocks,
        "a struct answer set cost {} blocks, not fewer than Answers' {}",
        typed.blocks,
        answers.blocks
    );

    // The cheaper decode has to be the same decode: each field holds the
    // answer the lookup finds under its name.
    let runtime: SystemOneResponse<Answers> = sdk::decode_system_one(
        Bytes::from_static(RESULT),
        StatusCode::OK,
        HeaderMap::new(),
        QUESTIONS,
    )
    .expect("the fixture decodes");
    let ticket: SystemOneResponse<Ticket> = sdk::decode_system_one(
        Bytes::from_static(RESULT),
        StatusCode::OK,
        HeaderMap::new(),
        QUESTIONS,
    )
    .expect("the fixture decodes as a Ticket");
    assert_eq!(Some(&ticket.answers().spam), runtime.answers().noul("spam"));
    assert_eq!(Some(&ticket.answers().tone), runtime.answers().choice("tone"));
    assert_eq!(Some(&ticket.answers().quality), runtime.answers().score("quality"));
}
