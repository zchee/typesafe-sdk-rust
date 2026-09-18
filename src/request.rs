//! Building one request and sending it.
//!
//! The body is assembled by splicing fragments of finished JSON - the encoded
//! state, the pre-escaped model name, the prepared questions - rather than by
//! building a value and encoding it, so a call costs one pass over the state
//! and nothing over anything else.
//!
//! The builder ends in a plain `async fn`, not an `IntoFuture`: on the pinned
//! toolchain an unboxed `IntoFuture` needs an unstable associated type, so the
//! choice is between a boxed future on every call and a method call the caller
//! writes. The method call is free.
