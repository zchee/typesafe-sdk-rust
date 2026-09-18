//! What a call answers with.
//!
//! The response type is generic in its answers from the start, so the map of
//! answers a runtime question set produces and the struct a derived one
//! produces are the same type at different parameters rather than two types
//! that drift apart.
//!
//! The received bytes are kept beside the decoded answers. An answer of a kind
//! this version does not model is dropped by the decoder and is still there in
//! the raw body, so a caller is never left with no way to read what the server
//! actually said.
