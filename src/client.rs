//! The client: what a caller holds, clones and shares.
//!
//! A client owns its transport and is cheap to clone, so passing one to every
//! task is the intended use rather than something to work around with a shared
//! reference. The API key enters as a secret and is kept only as the finished
//! `Authorization` header value, marked sensitive, so no formatting of the
//! client or its configuration can print it.
