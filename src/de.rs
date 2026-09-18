//! Reading an answer set in one pass.
//!
//! The decoder walks the wire format directly instead of building a value and
//! then interpreting it: an answer's kind is known from the member that names
//! it, so the visitor that reads it can be chosen before its contents are
//! parsed, and a score's integer level keys become integers without a string
//! ever existing.
//!
//! The field path an error reports is built as the walk descends, which is why
//! a failure deep in the answers can name the field it failed at rather than
//! the object that contained it.
