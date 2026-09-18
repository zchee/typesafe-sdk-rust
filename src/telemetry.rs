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
