//! The questions a call asks, and the shapes the API accepts them in.
//!
//! A question is a noul (how true is this?), a choice (which of these?) or a
//! score (how much, on this scale?); a raw question carries a shape this
//! version of the SDK does not model, so a new question type on the server does
//! not need a new release here.
//!
//! A question set is validated and serialized once, and the bytes are reused
//! for every call that asks it. That is what keeps the per-call cost to
//! splicing one prepared fragment into the body instead of walking a structure
//! that has not changed since the last call.
