//! Where a client's settings come from, and what makes them usable.
//!
//! Resolution is a pure function over a lookup closure rather than a reader of
//! the process environment, for two reasons: `std::env::set_var` is `unsafe` in
//! edition 2024, so a test that wanted to set a variable could not do it
//! without breaking the crate's `#![forbid(unsafe_code)]`; and the process
//! environment is global mutable state that two tests running in parallel
//! would fight over. The closure makes each case its own input.
//!
//! An explicit setting wins over the environment, an environment value is
//! trimmed, a blank one counts as unset, a trailing slash comes off the base
//! URL, and a deadline must be finite and above zero.
