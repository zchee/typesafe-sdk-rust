//! What the SDK reports about itself while it works.
//!
//! The crate emits events and never installs a subscriber: choosing where logs
//! go is the application's decision, and a library that made it would take it
//! away from every other library in the process.
//!
//! A state may carry personal data and a header may carry a credential, so a
//! body is reported by its length at the ordinary level and in full only at the
//! most verbose one, and the headers that carry secrets are redacted wherever
//! they are printed.

// The events that print headers arrive with the transport; until then only the
// tests call in here. `expect` rather than `allow`, so the attribute fails the
// gate by itself once a real caller exists.
#![cfg_attr(not(test), expect(dead_code, reason = "header logging arrives with the transport"))]

use std::fmt;

use http::{HeaderMap, HeaderName, HeaderValue};

use crate::constants::SECRET_HEADERS;

/// What a secret header value is printed as.
const REDACTED: &str = "***";

/// A view of `headers` that prints every secret value as `***`.
///
/// A header is secret when its name is one of the credential headers
/// (`authorization`, `proxy-authorization`, `x-api-key`, `api-key`, `cookie`,
/// `set-cookie`), when its name contains `token` or `secret`, or when its
/// value is flagged sensitive. The first two rules are the Python SDK's; the
/// third is this port's own, so a value the SDK or a caller marked sensitive
/// stays hidden under any name.
///
/// Nothing is copied: the view borrows the map and redacts as it writes, so a
/// log event that is filtered out costs nothing beyond building the view.
pub(crate) fn redact(headers: &HeaderMap) -> RedactedHeaders<'_> {
    RedactedHeaders(headers)
}

/// Headers as the logs show them. See [`redact`].
#[derive(Clone, Copy)]
pub(crate) struct RedactedHeaders<'a>(&'a HeaderMap);

impl RedactedHeaders<'_> {
    /// Each header in the map's order, a name repeated once per value, with
    /// its value or `None` when that value is secret.
    fn entries(&self) -> impl Iterator<Item = (&HeaderName, Option<&HeaderValue>)> {
        self.0.iter().map(|(name, value)| (name, (!is_secret(name, value)).then_some(value)))
    }
}

impl fmt::Debug for RedactedHeaders<'_> {
    /// `{"name": "value", "authorization": "***"}`, each value in the quoted,
    /// escaped form `HeaderValue`'s own `Debug` uses.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_map()
            .entries(self.entries().map(|(name, value)| (name, Shown(value))))
            .finish()
    }
}

impl fmt::Display for RedactedHeaders<'_> {
    /// `{name: value, authorization: ***}`, each value as plain text when it
    /// is UTF-8 and in its quoted, escaped `Debug` form when it is not.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("{")?;
        for (index, (name, value)) in self.entries().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{name}: ")?;
            match value {
                None => formatter.write_str(REDACTED)?,
                Some(value) => match value.to_str() {
                    Ok(text) => formatter.write_str(text)?,
                    // Only visible ASCII passes `to_str`; a value holding any
                    // other byte is printed escaped, so it cannot put raw
                    // bytes into a log line.
                    Err(_) => write!(formatter, "{value:?}")?,
                },
            }
        }
        formatter.write_str("}")
    }
}

/// One header value in a `Debug` map: the value, or the redaction marker.
struct Shown<'a>(Option<&'a HeaderValue>);

impl fmt::Debug for Shown<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => fmt::Debug::fmt(value, formatter),
            None => fmt::Debug::fmt(REDACTED, formatter),
        }
    }
}

/// Whether the value of this header must not be printed.
///
/// `http` stores every header name lower-cased, which is what makes the
/// comparisons here case-insensitive without lower-casing anything.
fn is_secret(name: &HeaderName, value: &HeaderValue) -> bool {
    let name = name.as_str();
    value.is_sensitive()
        || SECRET_HEADERS.contains(&name)
        || name.contains("token")
        || name.contains("secret")
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
