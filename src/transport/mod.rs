//! The seam between this crate and whatever sends the bytes.
//!
//! The SDK drives a `tower`-shaped service rather than an HTTP client, so a
//! caller can put their own stack underneath it - a recorder, a proxy, an
//! in-memory harness - without this crate knowing about it, and the default
//! stack is one implementation of that seam rather than a hard dependency.
//!
//! The request body is this crate's own type implementing `http-body`, not a
//! type borrowed from a pre-1.0 crate, so the public signature does not commit
//! a caller to a version of somebody else's dependency.
