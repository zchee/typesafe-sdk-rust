//! Spike S6 measurements: one scenario per process run, under dhat.

use std::{env, error::Error, process::ExitCode};

use encode_buffer::{Conversion, State, Variant, escape, state::english_like};

// A plain wrapper type, so declaring it as the global allocator is safe code
// and this crate keeps `unsafe_code` forbidden.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("variant") => scenario_variant(&args[1..])?,
        Some("mixed") => scenario_mixed(&args[1..])?,
        Some("bytes-convert") => scenario_bytes_convert(),
        Some("verify") => scenario_verify()?,
        other => {
            eprintln!("unknown scenario {other:?}");
            eprintln!(
                "scenarios:\n  \
                 variant <i|ii|ii-simd|iii|iii-1shot|iv|iv-1shot> <s1k|s64k|s1m|obj1m>\n  \
                 mixed <i|ii|ii-simd|iii|iii-1shot|iv|iv-1shot>\n  \
                 bytes-convert\n  \
                 verify"
            );
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The change in dhat's counters across one section.
struct Measured {
    blocks: u64,
    bytes: u64,
}

impl Measured {
    fn around<F, T>(body: F) -> (Self, T)
    where
        F: FnOnce() -> T,
    {
        let before = dhat::HeapStats::get();
        let value = body();
        let after = dhat::HeapStats::get();
        (
            Self {
                blocks: after.total_blocks - before.total_blocks,
                bytes: after.total_bytes - before.total_bytes,
            },
            value,
        )
    }
}

// --------------------------------------------------- one variant, one state

fn scenario_variant(args: &[String]) -> Result<(), Box<dyn Error>> {
    let variant = args
        .first()
        .and_then(|name| Variant::parse(name))
        .ok_or("first argument must be a variant name")?;
    let state_name = args.get(1).map(String::as_str).ok_or("second argument must be a state")?;
    let state = State::parse(state_name).ok_or("unknown state")?;

    let profiler = dhat::Profiler::builder().testing().build();

    // First call: pays for whatever the codec and the strategy set up once.
    let warm = variant.encode(&state)?;
    let body_len = warm.len();
    drop(warm);

    let (measured, body) = Measured::around(|| variant.encode(&state));
    let body = body?;
    let retained = variant.retained_capacity();
    let hint = variant.hint();
    drop(body);
    drop(profiler);

    println!(
        "variant={} state={state_name} state_len={} body_len={body_len} blocks={} bytes={} \
         retained_capacity={retained} hint={hint} bytes_over_body={:.2} retained_over_hint={:.2}",
        variant.name(),
        state.payload_len(),
        measured.blocks,
        measured.bytes,
        ratio(measured.bytes, body_len as u64),
        ratio(retained as u64, hint as u64),
    );
    Ok(())
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 { 0.0 } else { numerator as f64 / denominator as f64 }
}

// -------------------------------------------------- the mixed-size sequence

/// One 1 MB call followed by sixteen 1 KB calls, to show the hint decaying.
fn scenario_mixed(args: &[String]) -> Result<(), Box<dyn Error>> {
    let variant = args
        .first()
        .and_then(|name| Variant::parse(name))
        .ok_or("first argument must be a variant name")?;

    let large = State::parse("s1m").ok_or("state s1m")?;
    let small = State::parse("s1k").ok_or("state s1k")?;

    let profiler = dhat::Profiler::builder().testing().build();
    // One warm-up of each size, so the reported numbers are steady-state for
    // the strategy rather than first-touch costs of the allocator.
    drop(variant.encode(&small)?);
    drop(variant.encode(&large)?);
    encode_buffer::reset();

    println!("variant={} sequence=1MB then 16x1KB", variant.name());
    println!(
        "{:>4} {:>10} {:>8} {:>10} {:>12} {:>12}",
        "call", "state", "blocks", "bytes", "retained", "hint"
    );

    for index in 0..17 {
        let state = if index == 0 { &large } else { &small };
        let label = if index == 0 { "1MB" } else { "1KB" };
        let (measured, body) = Measured::around(|| variant.encode(state));
        drop(body?);
        println!(
            "{index:>4} {label:>10} {:>8} {:>10} {:>12} {:>12}",
            measured.blocks,
            measured.bytes,
            variant.retained_capacity(),
            variant.hint(),
        );
    }

    drop(profiler);
    Ok(())
}

// ------------------------------------------------- Vec/BytesMut -> Bytes

/// What it costs to hand a finished buffer to the body type, and what the
/// first `clone()` of that body costs on top.
fn scenario_bytes_convert() {
    println!("# S6 body-type conversion, 1 KiB and 1 MiB payloads");
    println!(
        "{:<16} {:>9} {:>8} {:>9} {:>8} {:>9} {:>8} {:>9}",
        "conversion",
        "len",
        "src_cap",
        "cv_blocks",
        "cv_bytes",
        "cl1_blocks",
        "cl1_bytes",
        "cl2_blocks"
    );

    let profiler = dhat::Profiler::builder().testing().build();

    for len in [1024usize, 1_048_576] {
        for conversion in [Conversion::VecExact, Conversion::VecSlack, Conversion::BytesMutFreeze] {
            // Warm up this exact shape once, then measure the second one.
            {
                let source = conversion.build(len);
                let body = source.freeze();
                let clone = body.clone();
                drop((body, clone));
            }

            let source = conversion.build(len);
            let capacity = source.capacity();
            let (convert, body) = Measured::around(|| source.freeze());
            let (first_clone, clone_one) = Measured::around(|| body.clone());
            let (second_clone, clone_two) = Measured::around(|| body.clone());

            println!(
                "{:<16} {len:>9} {capacity:>8} {:>9} {:>8} {:>10} {:>9} {:>10}",
                format!("{conversion:?}"),
                convert.blocks,
                convert.bytes,
                first_clone.blocks,
                first_clone.bytes,
                second_clone.blocks,
            );
            drop((body, clone_one, clone_two));
        }
    }

    drop(profiler);
}

// ------------------------------------------------------------- correctness

/// Every variant must produce the same bytes, and the scalar escaper must agree
/// with the codec: a faster variant that encodes differently is not a variant.
fn scenario_verify() -> Result<(), Box<dyn Error>> {
    println!("# S6 correctness: all variants agree, and the scalar escaper matches sonic-rs");

    for state_name in ["s1k", "s64k", "s1m", "obj1m"] {
        let state = State::parse(state_name).ok_or("unknown state")?;
        let mut reference: Option<Vec<u8>> = None;
        for variant in [
            Variant::CapacityHint,
            Variant::ExactTwoPass,
            Variant::ExactTwoPassSimd,
            Variant::RetainedScratch,
            Variant::RetainedScratchOneShot,
            Variant::Combined,
            Variant::CombinedOneShot,
        ] {
            encode_buffer::reset();
            let body = variant.encode(&state)?.to_vec();
            match &reference {
                None => reference = Some(body),
                Some(expected) => {
                    if *expected != body {
                        println!("state={state_name} variant={} MISMATCH", variant.name());
                        return Err("variants disagree".into());
                    }
                }
            }
        }
        let len = reference.as_ref().map_or(0, Vec::len);
        println!("state={state_name} all_variants_agree=true body_len={len}");
    }

    // The escaper is checked on the characters that actually differ between
    // implementations, not only on the English-like filler.
    let cases = [
        english_like(1024),
        "plain".to_owned(),
        "quote \" backslash \\ slash /".to_owned(),
        "control \u{0}\u{1}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{1f}".to_owned(),
        "del \u{7f} nbsp \u{a0} bmp \u{2028}\u{2029} astral \u{1f600}".to_owned(),
        "é€—".to_owned(),
    ];
    for case in &cases {
        let mine = {
            let mut buffer = Vec::new();
            escape::write_escaped(case, &mut buffer);
            buffer
        };
        let theirs = sonic_rs::to_string(case)?.into_bytes();
        let simd = {
            let mut buffer = Vec::new();
            json_escape_simd::escape_into(case, &mut buffer);
            buffer
        };
        let predicted = escape::escaped_len(case);
        println!(
            "case_len={} scalar_matches_sonic={} simd_matches_sonic={} escaped_len_exact={}",
            case.len(),
            mine == theirs,
            simd == theirs,
            predicted == mine.len(),
        );
        if mine != theirs {
            println!("  scalar={:?}", String::from_utf8_lossy(&mine));
            println!("  sonic ={theirs:?}", theirs = String::from_utf8_lossy(&theirs));
        }
        if simd != theirs {
            println!("  simd  ={:?}", String::from_utf8_lossy(&simd));
        }
    }

    Ok(())
}
