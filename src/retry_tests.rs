//! Tests for the backoff schedule and the seconds-to-`Duration` conversion.
//!
//! Expected values are the upstream SDK's own test tables, or were computed by
//! running its `_backoff` (and Python's `round` and `math.ldexp`) under
//! CPython with the same inputs. Floating-point results are compared by their
//! bits, so a sign of zero or a last-place difference fails the test.

use super::*;

/// The upstream `RetryPolicy` defaults: `backoff_initial`, `backoff_max` and
/// `backoff_jitter`.
const INITIAL: f64 = 0.5;
const MAX: f64 = 5.0;
const JITTER: f64 = 0.25;

/// The largest `f64` below 1, the top of the range a random draw comes from.
const DRAW_BELOW_ONE: f64 = 1.0 - f64::EPSILON / 2.0;

#[track_caller]
fn assert_same_f64(got: f64, want: f64, context: &str) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{context}: got {got:?} (bits {:#018x}), want {want:?} (bits {:#018x})",
        got.to_bits(),
        want.to_bits(),
    );
}

#[test]
fn default_schedule_doubles_then_caps_as_upstream_test_backoff_dates_cap_and_jitter() {
    let table = [(1, 0.5), (2, 1.0), (3, 2.0), (4, 4.0), (5, 5.0), (20, 5.0)];
    for (attempt, want) in table {
        let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        assert_same_f64(got, want, &format!("attempt {attempt}, draw 0"));
    }
}

#[test]
fn a_full_draw_removes_the_whole_jitter_share_as_upstream() {
    // Upstream patches `random.random` to return 1.0, outside its real range,
    // to reach the largest reduction: 0.5 * (1 - 0.25).
    let got = backoff_seconds(1, INITIAL, MAX, JITTER, 1.0);
    assert_same_f64(got, 0.375, "attempt 1, draw 1.0");
}

#[test]
fn extreme_values_match_upstream_test_backoff_extreme_values() {
    let table = [
        // A delay far below a millisecond rounds to zero.
        (1e-300, 1e300, 1, 0.0),
        // 2000 doublings of 1e-300 would overflow; the log2 cap test stops them.
        (1e-300, 1e300, 2000, 1e300),
        (1e308, 1e308, 1, 1e308),
        // 0.5 is above the cap, and the cap rounds up to 0.001; the result
        // never exceeds the un-jittered delay, so it is the cap itself.
        (0.5, 0.0006, 1, 0.0006),
    ];
    for (initial, max, attempt, want) in table {
        let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
        assert_same_f64(got, want, &format!("initial {initial:e}, max {max:e}, attempt {attempt}"));
    }
}

#[test]
fn zero_initial_or_max_disables_backoff_with_a_positive_zero() {
    let cases = [
        ("zero initial", 0.0, MAX),
        ("zero max", INITIAL, 0.0),
        ("negative-zero initial", -0.0, MAX),
        ("negative-zero max", INITIAL, -0.0),
    ];
    for (name, initial, max) in cases {
        for attempt in [0, 1, 7, u32::MAX] {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.5);
            assert_same_f64(got, 0.0, &format!("{name}, attempt {attempt}"));
        }
    }
}

#[test]
fn attempt_zero_follows_the_formula_and_gives_half_the_initial_delay() {
    let got = backoff_seconds(0, INITIAL, MAX, JITTER, 0.0);
    assert_same_f64(got, 0.25, "attempt 0, draw 0");
}

#[test]
fn the_draw_and_jitter_bounds_match_cpython() {
    let cases = [
        ("draw 0 leaves the delay whole", 1, JITTER, 0.0, 0.5),
        ("the largest draw below 1 rounds to the full reduction", 1, JITTER, DRAW_BELOW_ONE, 0.375),
        ("jitter 0 ignores the draw", 1, 0.0, DRAW_BELOW_ONE, 0.5),
        ("jitter 1 with a draw near 1 leaves almost nothing", 1, 1.0, DRAW_BELOW_ONE, 0.0),
        ("jitter 1 with a draw of one half halves the delay", 3, 1.0, 0.5, 1.0),
        ("a middle draw", 2, JITTER, 0.5, 0.875),
        ("a draw that needs rounding", 4, JITTER, 0.3, 3.7),
    ];
    for (name, attempt, jitter, draw, want) in cases {
        let got = backoff_seconds(attempt, INITIAL, MAX, jitter, draw);
        assert_same_f64(got, want, name);
    }
}

#[test]
fn jitter_never_raises_the_delay_above_the_exponential() {
    for attempt in 0..=8 {
        let exponential = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        for step in 0..=100 {
            let draw = f64::from(step) / 100.0 * DRAW_BELOW_ONE;
            let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, draw);
            assert!(
                got <= exponential && got >= exponential * (1.0 - JITTER) - 0.0005,
                "attempt {attempt}, draw {draw}: {got} is outside [{} - rounding, {exponential}]",
                exponential * (1.0 - JITTER),
            );
        }
    }
}

#[test]
fn rounding_is_python_round_half_even_on_the_exact_value_not_scaled_rounding() {
    // 1.0005 is stored as 1.000499999999999989..., which Python rounds down;
    // scaling by 1000 first gives exactly 1000.5, which `f64::round` rounds up.
    let got = backoff_seconds(1, 1.0005, MAX, JITTER, 0.0);
    assert_same_f64(got, 1.0, "initial 1.0005, draw 0");
    // 0.125 * (1 - 0.5 * 1) is 0.0625 exactly, a true tie: Python keeps the
    // even digit, 0.062, where half-away-from-zero would give 0.063.
    let got = backoff_seconds(1, 0.125, MAX, 1.0, 0.5);
    assert_same_f64(got, 0.062, "initial 0.125, jitter 1, draw 0.5");
}

#[test]
fn round_to_millis_matches_cpython_round() {
    let cases = [
        (0.0625, 0.062),
        (2.0625, 2.062),
        (0.4375, 0.438),
        (1.0005, 1.0),
        (0.0005, 0.001),
        (0.000_499_9, 0.0),
        (0.1875, 0.188),
        (-0.0004, -0.0),
        (-0.0, -0.0),
        (4.5285770979303905e271, 4.5285770979303905e271),
        // The widest text the stack buffer holds: 16 integer digits, a sign,
        // a point and three decimals.
        (-4_503_599_627_370_495.5, -4_503_599_627_370_495.5),
        (4_503_599_627_370_496.0, 4_503_599_627_370_496.0),
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    ];
    for (x, want) in cases {
        assert_same_f64(round_to_millis(x), want, &format!("round({x:?}, 3)"));
    }
    assert!(round_to_millis(f64::NAN).is_nan(), "round(nan, 3) is nan");
}

#[test]
fn the_default_schedule_never_decreases_and_settles_at_the_cap() {
    let mut previous = 0.0;
    for attempt in 0..=100 {
        let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        assert!(got >= previous, "attempt {attempt}: {got} is below the previous delay {previous}");
        previous = got;
    }
    assert_same_f64(previous, MAX, "attempt 100");
}

#[test]
fn extreme_ranges_stay_finite_monotonic_and_exact_through_every_doubling() {
    // The widest ranges an f64 allows: the smallest subnormal up to the
    // largest finite value, and upstream's 1e-300 up to 1e300. Doubling
    // either with `initial * 2f64.powi(exponent)` overflows: 2^1992 is not
    // representable, although 1e-300 times it is.
    for (initial, max) in [(f64::from_bits(1), f64::MAX), (1e-300, 1e300)] {
        let mut previous = 0.0;
        for attempt in 0..=2200 {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
            let context = format!("initial {initial:e}, max {max:e}, attempt {attempt}");
            assert!(got.is_finite() && got <= max, "{context}: {got} is infinite or above the cap");
            assert!(got >= previous, "{context}: {got} is below the previous delay {previous}");
            // Above 2^52 rounding is the identity, so an uncapped delay must be
            // exactly twice the one before it.
            if previous >= 4_503_599_627_370_496.0 && got < max {
                assert_same_f64(got, previous * 2.0, &context);
            }
            previous = got;
        }
        assert_same_f64(previous, max, &format!("initial {initial:e}, max {max:e}, attempt 2200"));
    }
}

#[test]
fn doubling_matches_cpython_ldexp_near_and_at_the_cap() {
    let subnormal = f64::from_bits(1);
    let cases = [
        (1900, 1e-300, 1e300, 4.5285770979303905e271),
        (1992, 1e-300, 1e300, 2.2424427642075284e299),
        // log2(1e300) - log2(1e-300) is 1993.157: exponents up to 1993 double.
        (1994, 1e-300, 1e300, 8.969771056830114e299),
        (1995, 1e-300, 1e300, 1e300),
        (2000, subnormal, f64::MAX, 2.83625966735417e278),
        // log2(f64::MAX) rounds to 1024, so the cap is at exponent 2098:
        // exponent 2097 is 2^-1074 * 2^2097 = 2^1023.
        (2098, subnormal, f64::MAX, 8.98846567431158e307),
        (2099, subnormal, f64::MAX, f64::MAX),
    ];
    for (attempt, initial, max, want) in cases {
        let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
        assert_same_f64(got, want, &format!("initial {initial:e}, max {max:e}, attempt {attempt}"));
    }
}

#[test]
fn attempts_beyond_i32_max_reach_the_cap_without_overflow() {
    let beyond_i32 = 1_u32 << 31;
    for attempt in [beyond_i32, beyond_i32 + 1, u32::MAX] {
        for (initial, max) in [(INITIAL, MAX), (1e-300, 1e300), (f64::from_bits(1), f64::MAX)] {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
            assert_same_f64(
                got,
                max,
                &format!("initial {initial:e}, max {max:e}, attempt {attempt}"),
            );
        }
    }
}

#[test]
fn inputs_a_policy_rejects_do_not_panic() {
    // An infinite cap never caps, so the doubling saturates to infinity; NaN
    // propagates. The retry policy rejects both before they reach here.
    let got = backoff_seconds(u32::MAX, INITIAL, f64::INFINITY, JITTER, 0.0);
    assert_same_f64(got, f64::INFINITY, "infinite max, attempt u32::MAX");
    assert!(backoff_seconds(1, f64::NAN, MAX, JITTER, 0.0).is_nan(), "NaN initial gives NaN");
    assert_eq!(seconds_to_duration(got), Duration::MAX, "an infinite delay saturates");
}

#[test]
fn scalbn_matches_cpython_ldexp() {
    let cases = [
        (1e-300, 1992, 4.484885528415057e299),
        (f64::from_bits(1), 2097, 8.98846567431158e307),
        (f64::from_bits(1), 2098, f64::INFINITY),
        (1.0, 1023, 8.98846567431158e307),
        (1.0, 1024, f64::INFINITY),
        (1.0, i32::MAX, f64::INFINITY),
        (1.0, -1022, 2.2250738585072014e-308),
        // Results in the subnormal range are rounded once, ties to even.
        (1.5, -1074, 1e-323),
        (3.0, -1075, 1e-323),
        (0.75, -1073, 1e-323),
        (1.0, -1076, 0.0),
        (1e300, -2000, 8.709809816217217e-303),
        (f64::MAX, i32::MIN, 0.0),
        (1e-310, -10, 9.765625e-314),
        (1e-310, 100, 1.2676506002282255e-280),
        (-0.0, 5, -0.0),
    ];
    for (x, n, want) in cases {
        assert_same_f64(scalbn(x, n), want, &format!("ldexp({x:e}, {n})"));
    }
}

#[test]
fn seconds_to_duration_saturates_and_maps_nan_and_negatives_to_zero() {
    let cases = [
        ("zero", 0.0, Duration::ZERO),
        ("negative zero", -0.0, Duration::ZERO),
        ("one nanosecond", 1e-9, Duration::from_nanos(1)),
        ("a backoff delay", 0.375, Duration::from_millis(375)),
        ("the default cap", 5.0, Duration::from_secs(5)),
        ("1e300 seconds, beyond Duration::MAX", 1e300, Duration::MAX),
        ("infinity", f64::INFINITY, Duration::MAX),
        ("NaN", f64::NAN, Duration::ZERO),
        ("minus one", -1.0, Duration::ZERO),
        ("negative infinity", f64::NEG_INFINITY, Duration::ZERO),
    ];
    for (name, seconds, want) in cases {
        assert_eq!(seconds_to_duration(seconds), want, "{name}: {seconds:?} s");
    }
}
