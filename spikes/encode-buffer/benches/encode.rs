//! Wall-clock for the S6 variants. Report-only: no acceptance criterion of the
//! plan depends on a time, and this machine cannot count instructions.
//!
//! A separate target from the dhat binary on purpose. Timing measured through
//! an instrumented allocator is timing of the instrumentation.

use encode_buffer::{State, Variant};

fn main() {
    divan::main();
}

/// Every variant paired with every state, as `variant/state`.
const CASES: &[&str] = &[
    "i/s1k",
    "i/s64k",
    "i/s1m",
    "i/obj1m",
    "ii/s1k",
    "ii/s64k",
    "ii/s1m",
    "ii/obj1m",
    "ii-simd/s1k",
    "ii-simd/s64k",
    "ii-simd/s1m",
    "ii-simd/obj1m",
    "iii/s1k",
    "iii/s64k",
    "iii/s1m",
    "iii/obj1m",
    "iii-1shot/s1k",
    "iii-1shot/s64k",
    "iii-1shot/s1m",
    "iii-1shot/obj1m",
    "iv/s1k",
    "iv/s64k",
    "iv/s1m",
    "iv/obj1m",
    "iv-1shot/s1k",
    "iv-1shot/s64k",
    "iv-1shot/s1m",
    "iv-1shot/obj1m",
];

/// The timed call is a second identical call: the variants exist to make the
/// steady state cheap, so a first call would report set-up costs instead.
#[divan::bench(args = CASES)]
fn encode(bencher: divan::Bencher, case: &str) {
    let Some((variant_name, state_name)) = case.split_once('/') else {
        return;
    };
    let Some(variant) = Variant::parse(variant_name) else {
        return;
    };
    let Some(state) = State::parse(state_name) else {
        return;
    };

    // The warm-up is inside the input closure, which divan excludes from the
    // timed section, so every sample measures a steady-state call.
    bencher
        .with_inputs(|| {
            drop(variant.encode(&state).expect("the warm-up encode succeeds"));
        })
        .bench_values(|()| variant.encode(&state).expect("the measured encode succeeds"));
}
