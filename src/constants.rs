//! The names and defaults the SDK's behaviour is pinned to.
//!
//! The environment variables a client reads, the request and response headers
//! it sets and looks for, the two API paths, and the defaults for the base
//! URL, the per-attempt deadline and the response size cap. They live in one
//! module because they are the crate's contract with the outside world: a
//! caller reading the documentation should be able to find every name the SDK
//! knows in one place, and a change to one of them is a change to that
//! contract rather than an edit somewhere in the middle of a request builder.

use std::{sync::LazyLock, time::Duration};

use http::{HeaderName, HeaderValue};

/// The response header carrying the server's identifier for a request.
pub(crate) const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

/// The non-standard millisecond-precision companion to `Retry-After`.
pub(crate) const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";

// ------------------------------------------------- environment and defaults

/// The environment variable a client reads its API key from when none is
/// passed explicitly.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// The environment variable a client reads its base URL from when none is
/// passed explicitly.
pub const BASE_URL_ENV: &str = "TYPESAFE_BASE_URL";

/// The environment variable a client reads its default model from when none
/// is passed explicitly.
pub const DEFAULT_MODEL_ENV: &str = "TYPESAFE_DEFAULT_MODEL";

/// The API root a client talks to when neither the caller nor
/// [`BASE_URL_ENV`] names one.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// The model a request names when neither the call, the client nor
/// [`DEFAULT_MODEL_ENV`] names one.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// The deadline each attempt of a request gets when the caller sets none.
///
/// It bounds one attempt from the first byte sent to the last byte received,
/// not the whole call: a retried call can take several of these.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response body a client reads before giving up on it, in bytes.
///
/// A body is collected in memory before it is decoded, so without a cap a
/// broken or hostile endpoint could make one call hold as much memory as it
/// cared to send.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

// ------------------------------------------------------------ API paths

/// The path of the System One endpoint, appended to the base URL.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const SYSTEM_ONE_PATH: &str = "/v1/systemone";

/// The path of the model listing endpoint, appended to the base URL.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const MODELS_PATH: &str = "/v1/models";

// ------------------------------------------------------ request headers
//
// `http` keeps header names lower-cased, so these are spelled that way; on the
// wire HTTP/1.1 header names are case-insensitive and HTTP/2 requires lower
// case, so nothing is lost. The names `http` already defines (`authorization`,
// `accept`, `content-type`, `user-agent`) are used from `http::header` directly
// rather than restated here.

/// The header naming the SDK and its version on every request.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const SDK_HEADER: HeaderName = HeaderName::from_static("x-typesafe-sdk");

/// The header naming the language runtime, operating system and architecture
/// on every request.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const RUNTIME_HEADER: HeaderName = HeaderName::from_static("x-typesafe-runtime");

/// The header counting how many times a request has been retried, set by the
/// SDK on retries only. A caller-supplied one is dropped.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const RETRY_COUNT_HEADER: HeaderName = HeaderName::from_static("x-typesafe-retry-count");

/// The media type of every request body and of every response the SDK accepts.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const JSON_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("application/json");

/// The header names whose values are credentials, and so are never logged.
///
/// Lower-cased, because that is how `http` stores every name. A name that
/// merely contains `token` or `secret` is treated the same way; that rule lives
/// with the redaction that applies it.
#[cfg_attr(not(test), expect(dead_code, reason = "header logging arrives with the transport"))]
pub(crate) const SECRET_HEADERS: [&str; 6] =
    ["authorization", "proxy-authorization", "x-api-key", "api-key", "cookie", "set-cookie"];

// ------------------------------------------------------ SDK identification

/// What the SDK calls itself in `User-Agent` and [`SDK_HEADER`], as
/// `typesafe-sdk-rust/<version>`.
///
/// Deliberately not the official Python SDK's `typesafe-sdk/<version>`: the
/// server may count or treat SDKs by this value, and a port must not be
/// mistaken for the SDK it is a port of.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) const SDK_IDENTIFIER: HeaderValue =
    HeaderValue::from_static(concat!("typesafe-sdk-rust/", env!("CARGO_PKG_VERSION")));

/// What the SDK sends in [`RUNTIME_HEADER`], as `rust (<os>; <arch>)`.
///
/// `std::env::consts` holds the target the crate was compiled for, such as
/// `macos` and `aarch64`. Those are constants but not literals, and `concat!`
/// takes only literals, so the value is built on first use and kept for the
/// life of the process.
#[cfg_attr(not(test), expect(dead_code, reason = "request assembly arrives with the transport"))]
pub(crate) static RUNTIME_IDENTIFIER: LazyLock<HeaderValue> = LazyLock::new(|| {
    let text = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    HeaderValue::from_str(&text)
        .expect("invariant: target OS and architecture names are printable ASCII")
});

#[cfg(test)]
#[path = "constants_tests.rs"]
mod tests;
