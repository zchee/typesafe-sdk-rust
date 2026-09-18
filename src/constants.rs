//! The names and defaults the SDK's behaviour is pinned to.
//!
//! The environment variables a client reads, the request and response headers
//! it sets and looks for, the two API paths, and the defaults for the base
//! URL, the per-attempt deadline and the response size cap. They live in one
//! module because they are the crate's contract with the outside world: a
//! caller reading the documentation should be able to find every name the SDK
//! knows in one place, and a change to one of them is a change to that
//! contract rather than an edit somewhere in the middle of a request builder.

/// The response header carrying the server's identifier for a request.
pub(crate) const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

/// The non-standard millisecond-precision companion to `Retry-After`.
pub(crate) const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";
