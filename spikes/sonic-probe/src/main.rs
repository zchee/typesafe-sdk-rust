//! Spike S1 (sonic-rs semantics) and S5 (how dhat accounts for re-allocation).
//!
//! One scenario per process run. dhat attributes every allocation in the
//! process to a single profiler, so a scenario that reports blocks and bytes
//! has to be the only measured work in its run; the scenario name is an
//! argument and `run.sh` invokes the binary once per scenario. The nesting
//! scenarios need a process of their own for a second reason: a stack overflow
//! is one of their possible answers, and the shell records the exit status.

mod fixture;
mod nesting;
mod stats;

use std::{env, error::Error, process::ExitCode};

use serde::de::IgnoredAny;

use crate::{
    fixture::{
        BorrowedField, BorrowedModels, BorrowedResponse, CowField, LevelKey, MODELS_BAD_NAME,
        OwnedControl, RESULT, RESULT_MISSING_NOUL,
    },
    stats::Measured,
};

// dhat's allocator is a plain wrapper type; declaring it as the global
// allocator is safe code, which is what lets this crate keep `unsafe_code`
// forbidden. When no profiler is live it forwards straight to the system
// allocator, so the non-dhat scenarios pay only an atomic load per allocation.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let scenario = args.first().map(String::as_str).unwrap_or("help");

    match scenario {
        "a-path" => scenario_a_path(),
        "b-borrow" => scenario_b_borrow(),
        "e-append" => scenario_e_append()?,
        "f-float" => scenario_f_float(),
        "c-borrowed" => scenario_c(Decoded::Borrowed),
        "c-ignoredany" => scenario_c(Decoded::Ignored),
        "c-owned" => scenario_c(Decoded::Owned),
        "s5-realloc" => scenario_s5_realloc(),
        "nest" => return nesting::run(&args[1..]),
        other => {
            eprintln!("unknown scenario {other:?}");
            eprintln!(
                "scenarios: a-path b-borrow e-append f-float c-borrowed c-ignoredany c-owned s5-realloc \
                 nest <shape> <depth> <stack-kib>"
            );
            return Ok(ExitCode::from(2));
        }
    }

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------- S1 (a)

/// Does `serde_path_to_error` track field paths over `sonic_rs::Deserializer`?
fn scenario_a_path() {
    println!("# S1(a) serde_path_to_error over sonic_rs::Deserializer (slice input)");

    report_path::<BorrowedResponse<'_>>("answers.spam.noul removed", RESULT_MISSING_NOUL);
    report_path::<BorrowedModels<'_>>("models[1].name is a number", MODELS_BAD_NAME);

    println!();
    println!("# same inputs through serde_json, for comparison");
    report_path_serde_json::<BorrowedResponse<'_>>(
        "answers.spam.noul removed",
        RESULT_MISSING_NOUL,
    );
    report_path_serde_json::<BorrowedModels<'_>>("models[1].name is a number", MODELS_BAD_NAME);

    println!();
    println!("# the same failures WITHOUT serde_path_to_error, to show what is lost");
    match sonic_rs::from_slice::<BorrowedResponse<'_>>(RESULT_MISSING_NOUL) {
        Ok(_) => println!("bare/sonic answers.spam.noul removed: unexpectedly Ok"),
        Err(error) => println!("bare/sonic answers.spam.noul removed: {error}"),
    }
    match sonic_rs::from_slice::<BorrowedModels<'_>>(MODELS_BAD_NAME) {
        Ok(_) => println!("bare/sonic models[1].name: unexpectedly Ok"),
        Err(error) => println!("bare/sonic models[1].name: {error}"),
    }
}

fn report_path<'de, T>(case: &str, input: &'de [u8])
where
    T: serde::Deserialize<'de> + std::fmt::Debug,
{
    let mut deserializer = sonic_rs::Deserializer::from_slice(input);
    match serde_path_to_error::deserialize::<_, T>(&mut deserializer) {
        Ok(value) => println!("sonic  {case}: unexpectedly Ok: {value:?}"),
        Err(error) => {
            println!("sonic  {case}: path={:?} inner={}", error.path().to_string(), error.inner());
        }
    }
}

fn report_path_serde_json<'de, T>(case: &str, input: &'de [u8])
where
    T: serde::Deserialize<'de> + std::fmt::Debug,
{
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    match serde_path_to_error::deserialize::<_, T>(&mut deserializer) {
        Ok(value) => println!("serde_json {case}: unexpectedly Ok: {value:?}"),
        Err(error) => {
            println!(
                "serde_json {case}: path={:?} inner={}",
                error.path().to_string(),
                error.inner()
            );
        }
    }
}

// ---------------------------------------------------------------- S1 (b)

/// Borrowed `&'de str` fields and integer map keys.
fn scenario_b_borrow() {
    println!("# S1(b) borrowed &str fields from a &[u8] input");

    let plain: &[u8] = br#"{"s":"friendly"}"#;
    let escaped: &[u8] = br#"{"s":"a\"b\nc\u00e9"}"#;

    report_borrow(plain, "no escape sequences");
    report_borrow(escaped, "with escape sequences");

    println!();
    println!("# the same two inputs into Cow<str>");
    report_cow(plain, "no escape sequences");
    report_cow(escaped, "with escape sequences");

    println!();
    println!("# S1(b) integer map keys");
    let levels: &[u8] = br#"{"0":0.1,"1":0.9}"#;

    match sonic_rs::from_slice::<std::collections::HashMap<u32, f64>>(levels) {
        Ok(map) => {
            let mut pairs: Vec<_> = map.into_iter().collect();
            pairs.sort_by_key(|(key, _)| *key);
            println!("sonic HashMap<u32, f64>: Ok {pairs:?}");
        }
        Err(error) => println!("sonic HashMap<u32, f64>: Err {error}"),
    }
    match sonic_rs::from_slice::<std::collections::BTreeMap<u32, f64>>(levels) {
        Ok(map) => println!("sonic BTreeMap<u32, f64>: Ok {map:?}"),
        Err(error) => println!("sonic BTreeMap<u32, f64>: Err {error}"),
    }
    match sonic_rs::from_slice::<std::collections::BTreeMap<LevelKey, f64>>(levels) {
        Ok(map) => println!("sonic BTreeMap<LevelKey, f64>: Ok {map:?}"),
        Err(error) => println!("sonic BTreeMap<LevelKey, f64>: Err {error}"),
    }

    match serde_json::from_slice::<std::collections::BTreeMap<u32, f64>>(levels) {
        Ok(map) => println!("serde_json BTreeMap<u32, f64>: Ok {map:?}"),
        Err(error) => println!("serde_json BTreeMap<u32, f64>: Err {error}"),
    }
    match serde_json::from_slice::<std::collections::BTreeMap<LevelKey, f64>>(levels) {
        Ok(map) => println!("serde_json BTreeMap<LevelKey, f64>: Ok {map:?}"),
        Err(error) => println!("serde_json BTreeMap<LevelKey, f64>: Err {error}"),
    }
}

/// Reports whether the decoded `&str` points into `input` itself.
fn report_borrow(input: &[u8], case: &str) {
    match sonic_rs::from_slice::<BorrowedField<'_>>(input) {
        Ok(value) => {
            println!(
                "sonic &str {case}: Ok {:?} borrowed={}",
                value.s,
                points_into(input, value.s)
            );
        }
        Err(error) => println!("sonic &str {case}: Err {error}"),
    }
    match serde_json::from_slice::<BorrowedField<'_>>(input) {
        Ok(value) => println!(
            "serde_json &str {case}: Ok {:?} borrowed={}",
            value.s,
            points_into(input, value.s)
        ),
        Err(error) => println!("serde_json &str {case}: Err {error}"),
    }
}

fn report_cow(input: &[u8], case: &str) {
    match sonic_rs::from_slice::<CowField<'_>>(input) {
        Ok(value) => {
            let owned = matches!(value.s, std::borrow::Cow::Owned(_));
            println!(
                "sonic Cow<str> {case}: Ok {:?} owned={owned} borrowed={}",
                value.s,
                points_into(input, &value.s)
            );
        }
        Err(error) => println!("sonic Cow<str> {case}: Err {error}"),
    }
}

/// Address arithmetic only, so a borrowed slice can be recognised without
/// dereferencing anything and without `unsafe`.
fn points_into(haystack: &[u8], needle: &str) -> bool {
    let start = haystack.as_ptr().addr();
    let end = start + haystack.len();
    let at = needle.as_ptr().addr();
    at >= start && at + needle.len() <= end
}

// ---------------------------------------------------------------- S1 (e)

/// Does `to_writer` append to a non-empty `Vec<u8>` instead of clearing it?
fn scenario_e_append() -> Result<(), Box<dyn Error>> {
    println!("# S1(e) sonic_rs::to_writer into a non-empty Vec<u8>");

    let mut buffer = br#"{"state":"#.to_vec();
    let before = buffer.len();
    sonic_rs::to_writer(&mut buffer, &"a \"quoted\" state")?;
    buffer.extend_from_slice(br#","model":"#);
    sonic_rs::to_writer(&mut buffer, &"jev-latest")?;
    buffer.push(b'}');

    let text = String::from_utf8(buffer)?;
    println!("prefix_len_before_first_write = {before}");
    println!("result = {text}");

    // Proof that the splice is a valid document, not only a plausible string.
    let round_trip: serde_json::Value = serde_json::from_str(&text)?;
    println!("reparsed_by_serde_json = {round_trip}");

    // A second writer shape the body encoder may end up using.
    let mut owned = Vec::new();
    sonic_rs::to_writer(&mut owned, &[1u8, 2, 3])?;
    println!("empty_vec_result = {}", String::from_utf8(owned)?);

    Ok(())
}

// ---------------------------------------------------------------- S1 (f)

/// Are decoded f64 values bit-identical to `str::parse::<f64>()`?
fn scenario_f_float() {
    println!("# S1(f) float parsing, compared by f64::to_bits");
    println!("{:<34} {:>18} {:>18} {:>18} agree", "literal", "sonic", "str::parse", "serde_json");

    let literals = [
        "0.1",
        "1e-7",
        "0.30000000000000004",
        "5e-324",
        "-0.0",
        "-0",
        "-0.0e5",
        "-1e-400",
        "1e-400",
        "1.7976931348623157e308",
        "1.234567890123456789012345678901",
        "123456789012345678901234567890",
        "1e309",
        "-1e309",
        "0.000000000000000000000000000001",
    ];

    for literal in literals {
        let sonic = sonic_rs::from_str::<f64>(literal).map(f64::to_bits);
        let native = literal.parse::<f64>().map(f64::to_bits);
        let json = serde_json::from_str::<f64>(literal).map(f64::to_bits);

        let agree = match (&sonic, &native, &json) {
            (Ok(a), Ok(b), Ok(c)) => {
                if a == b && b == c {
                    "yes"
                } else {
                    "NO"
                }
            }
            _ => "n/a",
        };

        println!(
            "{literal:<34} {:>18} {:>18} {:>18} {agree}",
            show(&sonic),
            show(&native),
            show(&json),
        );
    }

    println!();
    println!("# negative zero inside a document, not as a bare scalar");
    for document in [r#"{"s":-0.0}"#, r#"[-0.0]"#] {
        let sonic = sonic_rs::from_str::<serde_json::Value>(document).map(|v| v.to_string());
        let json = serde_json::from_str::<serde_json::Value>(document).map(|v| v.to_string());
        println!("{document:<14} sonic_to_value={sonic:?} serde_json_to_value={json:?}");
    }
    let document = r#"{"s":-0.0}"#;
    let sonic = sonic_rs::from_str::<NegZero>(document).map(|value| value.s.to_bits());
    let json = serde_json::from_str::<NegZero>(document).map(|value| value.s.to_bits());
    println!("{document:<14} sonic_bits={} serde_json_bits={}", show(&sonic), show(&json));

    println!();
    println!("# the same values rendered back, to show the f64 each parse produced");
    for literal in literals {
        match (sonic_rs::from_str::<f64>(literal), literal.parse::<f64>()) {
            (Ok(sonic), Ok(native)) => {
                println!("{literal:<34} sonic={sonic:?} native={native:?}");
            }
            (sonic, native) => {
                println!("{literal:<34} sonic={sonic:?} native={native:?}");
            }
        }
    }
}

/// A single `f64` field, so the negative-zero case can be checked in the shape
/// the SDK actually decodes: a struct field, not a bare scalar.
#[derive(serde::Deserialize)]
struct NegZero {
    s: f64,
}

fn show<E>(value: &Result<u64, E>) -> String {
    match value {
        Ok(bits) => format!("{bits:#018x}"),
        Err(_) => "err".to_owned(),
    }
}

// ---------------------------------------------------------------- S1 (c)

#[derive(Clone, Copy)]
enum Decoded {
    Borrowed,
    Ignored,
    Owned,
}

/// The constant `C` of AC-P2: what a decode of the fixture costs when the
/// target itself owns nothing.
fn scenario_c(kind: Decoded) {
    let label = match kind {
        Decoded::Borrowed => "borrowed probe type",
        Decoded::Ignored => "serde::de::IgnoredAny",
        Decoded::Owned => "an owned control type (String and HashMap)",
    };
    println!("# S1(c) constant C: decoding RESULT into {label}");

    let profiler = dhat::Profiler::builder().testing().build();

    // The measured call is the second one: the first pays for anything the
    // codec initialises lazily on its first use in the process.
    decode_once(kind);

    let measured = Measured::around(|| decode_once(kind));
    drop(profiler);

    measured.print("decode");
    println!("input_len = {}", RESULT.len());
}

fn decode_once(kind: Decoded) {
    match kind {
        Decoded::Borrowed => {
            let value = sonic_rs::from_slice::<BorrowedResponse<'_>>(RESULT)
                .expect("the fixture matches the borrowed probe type");
            std::hint::black_box(&value);
        }
        Decoded::Ignored => {
            let value =
                sonic_rs::from_slice::<IgnoredAny>(RESULT).expect("the fixture is valid JSON");
            std::hint::black_box(&value);
        }
        Decoded::Owned => {
            let value = sonic_rs::from_slice::<OwnedControl>(RESULT)
                .expect("the fixture matches the owned control type");
            std::hint::black_box(&value);
        }
    }
}

// ---------------------------------------------------------------- S5

/// How dhat accounts for a `Vec<u8>` that grows, in place and moved.
fn scenario_s5_realloc() {
    println!("# S5 dhat accounting for Vec<u8> growth");

    let profiler = dhat::Profiler::builder().testing().build();

    // Warm up the allocator so the first measured step is not paying for a
    // fresh arena.
    let warm = Vec::<u8>::with_capacity(4096);
    std::hint::black_box(&warm);
    drop(warm);

    let mut buffer: Vec<u8> = Vec::new();
    let fresh = Measured::around(|| {
        buffer = vec![0u8; 1024];
    });
    fresh.print("fresh vec![0u8; 1024]");
    println!("  capacity = {} ptr = {:#x}", buffer.capacity(), buffer.as_ptr().addr());

    // A ladder of growths: small blocks come out of size classes that usually
    // cannot absorb the next size in place, large ones are page-backed and
    // often can. Which one happens is exactly what the ledger has to state.
    for target in [4096usize, 65_536, 1_048_576, 8_388_608] {
        let before_ptr = buffer.as_ptr().addr();
        let before_capacity = buffer.capacity();
        let additional = target - buffer.len();

        let measured = Measured::around(|| {
            buffer.reserve(additional);
            buffer.resize(target, 0u8);
        });

        let after_ptr = buffer.as_ptr().addr();
        measured.print(&format!("grow {before_capacity} -> {target}"));
        println!(
            "  capacity = {} ptr = {:#x} moved = {}",
            buffer.capacity(),
            after_ptr,
            before_ptr != after_ptr
        );
    }

    // A realloc that neither grows nor moves, to show what dhat charges for the
    // call itself rather than for the extra memory.
    let before_capacity = buffer.capacity();
    let measured = Measured::around(|| {
        buffer.shrink_to(1024);
    });
    measured.print(&format!("shrink_to(1024) while len == capacity == {before_capacity}"));
    println!("  capacity = {} ptr = {:#x}", buffer.capacity(), buffer.as_ptr().addr());

    // A shrink that really gives memory back, which is what the retained-scratch
    // variant of S6 does when its buffer outgrows the decayed hint.
    let before_capacity = buffer.capacity();
    let before_ptr = buffer.as_ptr().addr();
    let measured = Measured::around(|| {
        buffer.truncate(1024);
        buffer.shrink_to_fit();
    });
    measured.print(&format!("truncate + shrink_to_fit from capacity {before_capacity}"));
    println!(
        "  capacity = {} ptr = {:#x} moved = {}",
        buffer.capacity(),
        buffer.as_ptr().addr(),
        before_ptr != buffer.as_ptr().addr()
    );

    drop(buffer);
    drop(profiler);
}
