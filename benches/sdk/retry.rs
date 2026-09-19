//! B4: reading `Retry-After`.
//!
//! The parse is reached the way a caller reaches it: an API error from a
//! real call through the in-memory transport, then `ApiError::retry_after`,
//! which reads `retry-after-ms`, then `Retry-After` as seconds or as an HTTP
//! date measured against the system clock. The error is made once, outside
//! the timed section.
//!
//! How this can mislead: `retry_after` reads the system clock on every call,
//! so the HTTP-date case includes a `SystemTime::now()`; the other two read
//! it too and never use it. Inside the retry loop the SDK passes the clock
//! reading it already has.

use std::pin::pin;

use divan::{Bencher, black_box};
use http::StatusCode;
use typesafe_sdk::{ApiError, ErrorKind, RetryPolicy};

use crate::{
    service::{InMemory, client},
    support::{questions, text},
};

/// The three spellings: milliseconds, seconds, and an HTTP date.
fn header(spelling: &str) -> (&'static str, &'static str) {
    match spelling {
        "ms" => ("retry-after-ms", "1500"),
        "seconds" => ("retry-after", "2"),
        "date" => ("retry-after", "Fri, 01 Jan 2100 00:00:00 GMT"),
        other => panic!("no spelling {other}"),
    }
}

/// A 429 carrying `header`, as the error a call returns.
fn rate_limited(header: (&'static str, &'static str)) -> ApiError {
    let client = client(InMemory::failing(StatusCode::TOO_MANY_REQUESTS, header));
    let questions = questions();
    let state = text(64);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the runtime builds");
    let call = client
        .system_one(state.as_str(), &questions)
        .retry(RetryPolicy::default().max_retries(0))
        .send();
    let error = runtime.block_on(pin!(call)).expect_err("a 429 is an error");
    match error.kind() {
        ErrorKind::Api(api) => api.clone(),
        other => panic!("a 429 is an API error, not {other:?}"),
    }
}

#[divan::bench(args = ["ms", "seconds", "date"])]
fn retry_after(bencher: Bencher<'_, '_>, spelling: &str) {
    let error = rate_limited(header(spelling));
    assert!(error.retry_after().is_some(), "{spelling}: {:?} gives a wait", header(spelling));
    bencher.bench_local(|| black_box(&error).retry_after());
}
