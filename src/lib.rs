//! Async Rust SDK for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! The crate is published as `typesafe-sdk-rust` because `typesafe-sdk` is
//! already taken on crates.io; the library it builds is `typesafe_sdk`, so
//! callers write `use typesafe_sdk::...`.
//!
//! # Asking questions
//!
//! A call asks a set of named questions about a state. The set is built once,
//! validated and serialized by [`Questions::prepare`], and the resulting
//! [`PreparedQuestions`] is reused by every call that asks it. The client that
//! sends it arrives in a later release; this release provides the question
//! set, the response types and their decoder.
//!
//! ```
//! use typesafe_sdk::{Choice, Noul, Questions, Score};
//!
//! let prepared = Questions::new()
//!     .noul("billing", Noul::new().instructions("Is this about billing?"))
//!     .choice("tone", Choice::new(["calm", "angry"]).instructions("What is the tone?"))
//!     .score("urgency", Score::new(["can wait", "this week", "today"]))
//!     .prepare()?;
//!
//! assert_eq!(prepared.len(), 3);
//! assert_eq!(prepared.names().collect::<Vec<_>>(), ["billing", "tone", "urgency"]);
//! # Ok::<(), typesafe_sdk::Error>(())
//! ```
//!
//! # Runtime requirements
//!
//! Every network operation is `async` and expects a [Tokio] runtime whose
//! **time driver is enabled** (`#[tokio::main]`, or a `Builder` with
//! `enable_time()` / `enable_all()`). Per-attempt deadlines and HTTP/2
//! keep-alive both arm timers, and Tokio panics when a timer is created on a
//! runtime without that driver.
//!
//! # Safety
//!
//! The crate is `#![forbid(unsafe_code)]`. Dependencies that use `unsafe`
//! internally are confined to single modules so that swapping one out is a
//! local change.
//!
//! [Tokio]: https://docs.rs/tokio

#![forbid(unsafe_code)]

pub mod client;
mod codec;
pub mod config;
pub mod constants;
pub mod content;
pub mod de;
pub mod error;
pub mod models;
pub mod question;
pub mod request;
pub mod response;
pub mod retry;
pub mod telemetry;
pub mod transport;

#[cfg(feature = "internals")]
#[doc(hidden)]
pub mod __internals;

pub use crate::{
    codec::{DecodeError, DecodeErrorKind, EncodeError, RawJson},
    content::{Content, ContentError},
    de::{AnswerContext, AnswerSet},
    error::{ApiError, ApiErrorKind, Error, ErrorKind, ResponseValidationError},
    models::{ListModelsResponse, ModelMetadata},
    question::{Choice, Noul, PreparedQuestions, Question, Questions, RawQuestion, Score},
    response::{
        Answer, Answers, ChoiceAnswer, NoulAnswer, ResponseMeta, ScoreAnswer, SystemOneResponse,
        Usage,
    },
};
