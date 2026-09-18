//! When a failed attempt is worth repeating, and how long to wait first.
//!
//! The delay is a pure function of the attempt number and a random draw, so
//! the schedule can be asserted exactly rather than observed; the clock and the
//! sleep are a seam a test fills with a fake, so a retry test costs no wall
//! time and cannot be flaky.
//!
//! A server that says how long to wait is obeyed however long it asks for.
//! Retrying stops when the next delay would carry the call past its budget, and
//! the failure the caller gets is the last one, unchanged.
