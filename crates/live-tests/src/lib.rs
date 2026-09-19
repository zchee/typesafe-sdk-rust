//! What the tests against the live TypeSafe API share.
//!
//! **These tests make real, billed calls.** With `TYPESAFE_API_KEY` in the
//! environment, `cargo test --workspace`, `cargo nextest run --workspace` or
//! any command naming `-p typesafe-sdk-rust-live-tests` sends requests to the
//! live API on that key's account. That is why this crate is a workspace
//! member (so its code is linted and compiled) but not a default member: a
//! plain `cargo test` or `cargo nextest run` never builds or runs it. Run it
//! on purpose, with a key meant for it.
//!
//! The tests themselves are in `tests/live.rs`. Each one builds its client
//! through [`live_client`], which panics when `TYPESAFE_API_KEY` is unset, so
//! a run without a key fails with a message naming the variable instead of
//! passing without having asked the API anything.

use std::time::Duration;

use typesafe_sdk::{Client, constants::API_KEY_ENV};

/// The deadline of one live request: the Python SDK's live tests give their
/// clients 120 seconds, far more than a System One answer takes.
pub const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// A client for the live API, configured from the environment as
/// [`Client::from_env`] configures one, with [`LIVE_TIMEOUT`] as its
/// deadline.
///
/// # Panics
///
/// When the client cannot be built - above all when `TYPESAFE_API_KEY` is
/// unset or empty. The panic carries the SDK's own configuration error, which
/// names the variable.
#[must_use]
pub fn live_client() -> Client {
    match Client::builder().timeout(LIVE_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => panic!(
            "the live tests need {API_KEY_ENV}; they fail without it rather than skip: {error}"
        ),
    }
}
