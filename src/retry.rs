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

use std::{io::Write as _, time::Duration};

/// The delay, in seconds, before the attempt after attempt number `attempt`
/// failed.
///
/// The delay starts at `initial`, doubles with each attempt and stops growing
/// at `max`; `jitter` then removes a random share of it, up to `jitter` of the
/// whole, so that clients which failed together do not retry together. `draw`
/// is that random number, in `[0, 1)`, taken as an argument so the delay is a
/// pure function of its inputs. A zero `initial` or `max` disables backoff.
///
/// Attempts are numbered from 1, the first try. `attempt` 0 is not a number
/// the retry loop produces; it follows the same arithmetic and gives half of
/// `initial`, which keeps the schedule monotonic from 0 upwards. The type is
/// `u32` because every value converts exactly into `f64` and `i64`, which the
/// cap test and the doubling depend on, and no call is retried four billion
/// times.
///
/// The cap is tested in log2 space, before any doubling, so no intermediate
/// value overflows however large `attempt` is; the doubling itself is an exact
/// `ldexp`, not `initial * 2^exponent`, whose power of two alone would
/// overflow before the product came back under `max`.
///
/// The result is rounded to milliseconds and never exceeds the delay before
/// jitter: a delay that rounds up past a sub-millisecond `max` is `max`.
/// Rounding is exact decimal rounding of the binary value with ties to even,
/// the rule of Python's `round(delay, 3)`, rather than scaling by 1000 and
/// rounding the product: that shortcut rounds `1.0005` (stored just below it)
/// up to `1.001`, and the exact tie `0.0625` up to `0.063`, where this and
/// Python give `1.0` and `0.062`.
///
/// Non-finite or negative inputs are rejected where a retry policy is built,
/// so none is checked here; none of them panics.
#[cfg_attr(not(test), expect(dead_code, reason = "the retry loop arrives with the client"))]
pub(crate) fn backoff_seconds(attempt: u32, initial: f64, max: f64, jitter: f64, draw: f64) -> f64 {
    if initial == 0.0 || max == 0.0 {
        return 0.0;
    }
    // The subtraction is exact: every `u32` is representable in an `f64`.
    let exponential = if f64::from(attempt) - 1.0 >= max.log2() - initial.log2() {
        max
    } else {
        // Reaching here bounds the exponent by the log2 distance between two
        // finite values, under 2100, so the conversion only saturates when
        // `max` is infinite, where `scalbn` overflows to infinity either way.
        let exponent = i32::try_from(i64::from(attempt) - 1).unwrap_or(i32::MAX);
        scalbn(initial, exponent)
    };
    let delay = exponential * (1.0 - draw * jitter);
    exponential.min(round_to_millis(delay))
}

/// Converts a delay in seconds into a [`Duration`], saturating instead of
/// failing.
///
/// NaN, zero and negative values become [`Duration::ZERO`]; a value too large
/// for a `Duration`, infinity included, becomes [`Duration::MAX`].
#[cfg_attr(not(test), expect(dead_code, reason = "the retry loop arrives with the client"))]
pub(crate) fn seconds_to_duration(seconds: f64) -> Duration {
    if seconds.is_nan() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    // `from_secs_f64` would panic on overflow; the fallible form reports it,
    // and a positive, non-NaN value can only fail by being too large.
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

/// `x * 2^n`, correctly rounded: C's `scalbn`, which std does not provide.
///
/// This is musl's algorithm. A power of two outside the normal range of an
/// `f64` cannot be written as one value, so a large `n` is applied in steps of
/// `2^1023`; while scaling down, each step is `2^-1022` times `2^53`, which
/// keeps the intermediate value normal so the result is rounded only once.
/// `n` is clamped after two steps, where any finite nonzero `x` has already
/// overflowed or underflowed.
#[cfg_attr(not(test), expect(dead_code, reason = "the retry loop arrives with the client"))]
fn scalbn(x: f64, n: i32) -> f64 {
    const TWO_POW_1023: f64 = f64::from_bits(0x7FE0_0000_0000_0000);
    const TWO_POW_MINUS_969: f64 = f64::from_bits(0x0360_0000_0000_0000);
    let mut y = x;
    let mut n = n;
    if n > 1023 {
        y *= TWO_POW_1023;
        n -= 1023;
        if n > 1023 {
            y *= TWO_POW_1023;
            n = (n - 1023).min(1023);
        }
    } else if n < -1022 {
        y *= TWO_POW_MINUS_969;
        n += 1022 - 53;
        if n < -1022 {
            y *= TWO_POW_MINUS_969;
            n = (n + 1022 - 53).max(-1022);
        }
    }
    // `n` is now in `[-1022, 1023]`, so the biased exponent `1023 + n` is in
    // `[1, 2046]`: a normal power of two built directly from its bits.
    y * f64::from_bits(u64::from((0x3FF + n).unsigned_abs()) << 52)
}

/// `x` rounded to three decimal places, as Python's `round(x, 3)`.
///
/// Formatting with a precision rounds the exact decimal expansion of the
/// binary value, ties to even, and parsing back picks the nearest `f64`: the
/// same two steps CPython's `round` takes. From `2^52` up every `f64` is an
/// integer, so larger values, infinities and NaN are returned unchanged, which
/// also bounds the text to 21 bytes and keeps it on the stack.
#[cfg_attr(not(test), expect(dead_code, reason = "the retry loop arrives with the client"))]
fn round_to_millis(x: f64) -> f64 {
    const INTEGRAL_FROM: f64 = 4_503_599_627_370_496.0; // 2^52
    if x.is_nan() || x.abs() >= INTEGRAL_FROM {
        return x;
    }
    let mut buf = [0_u8; 24];
    // Writing into `&mut [u8]` advances the slice past what was written, so
    // the length left over tells how much was.
    let unused = {
        let mut rest = &mut buf[..];
        write!(rest, "{x:.3}")
            .expect("invariant: a value below 2^52 needs at most 21 bytes at three decimals");
        rest.len()
    };
    let written = buf.len() - unused;
    std::str::from_utf8(&buf[..written])
        .expect("invariant: formatted digits are ASCII")
        .parse()
        .expect("invariant: a formatted finite f64 parses back")
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
