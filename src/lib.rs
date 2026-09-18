//! Async Rust SDK for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! The crate is published as `typesafe-sdk-rust` because `typesafe-sdk` is
//! already taken on crates.io; the library it builds is `typesafe_sdk`, so
//! callers write `use typesafe_sdk::...`.
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

mod codec;
pub mod content;

#[cfg(feature = "internals")]
#[doc(hidden)]
pub mod __internals;

pub use crate::{
    codec::{DecodeError, DecodeErrorKind, EncodeError, RawJson},
    content::{Content, ContentError},
};
