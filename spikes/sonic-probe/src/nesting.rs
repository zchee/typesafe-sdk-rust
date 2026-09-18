//! S1(d): what a deeply nested document does to each decode path.
//!
//! Every shape runs in its own process because a stack overflow is one of the
//! possible answers and it terminates the process rather than returning. The
//! caller reads the exit status: 0 with a printed verdict means the path
//! returned, a signal means it did not.

// The three holder structs below exist to drive a decode; their one field is
// written by the codec and never read back.
#![expect(dead_code, reason = "holder fields are populated by the decode, never read")]

use std::{error::Error, process::ExitCode};

use serde::{Deserialize, de::IgnoredAny};

/// A `legend` value holding whatever the nested document is, decoded as raw
/// JSON the way the SDK keeps an unrecognised level label.
#[derive(Debug, Deserialize)]
struct LegendLazy {
    legend: sonic_rs::OwnedLazyValue,
}

/// The same holder, decoding the nested document into the codec's DOM.
#[derive(Debug, Deserialize)]
struct LegendValue {
    legend: sonic_rs::Value,
}

/// The same holder, skipping the nested document entirely.
#[derive(Debug, Deserialize)]
struct LegendIgnored {
    legend: IgnoredAny,
}

/// `nest <shape> <depth> <stack-kib>`.
///
/// `stack-kib` of 0 runs on the process's main thread, which macOS gives 8 MiB.
/// Any other value spawns a thread with that stack, which is how a tokio worker
/// thread (2 MiB by default) can be modelled.
pub fn run(args: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let shape = args.first().map(String::as_str).unwrap_or("value");
    let depth: usize = args.get(1).map_or(Ok(100_000), |value| value.parse())?;
    let stack_kib: usize = args.get(2).map_or(Ok(0), |value| value.parse())?;

    let document = build(shape, depth);
    println!("shape={shape} depth={depth} stack_kib={stack_kib} input_len={}", document.len());

    if stack_kib == 0 {
        decode(shape, &document);
        return Ok(ExitCode::SUCCESS);
    }

    let shape = shape.to_owned();
    let handle = std::thread::Builder::new()
        .stack_size(stack_kib * 1024)
        .spawn(move || decode(&shape, &document))?;
    match handle.join() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(_) => {
            println!("verdict=thread panicked");
            Ok(ExitCode::from(1))
        }
    }
}

/// `[[[...]]]` at `depth`, either bare or as the value of a `legend` field.
fn build(shape: &str, depth: usize) -> Vec<u8> {
    let mut document = Vec::with_capacity(depth * 2 + 32);
    let wrapped = shape.ends_with("-legend");
    if wrapped {
        document.extend_from_slice(br#"{"legend":"#);
    }
    document.resize(document.len() + depth, b'[');
    document.resize(document.len() + depth, b']');
    if wrapped {
        document.push(b'}');
    }
    document
}

fn decode(shape: &str, document: &[u8]) {
    match shape {
        "value" => verdict(sonic_rs::from_slice::<sonic_rs::Value>(document)),
        "ignored" => verdict(sonic_rs::from_slice::<IgnoredAny>(document)),
        "lazy" => verdict(sonic_rs::from_slice::<sonic_rs::OwnedLazyValue>(document)),
        "value-legend" => verdict(sonic_rs::from_slice::<LegendValue>(document)),
        "ignored-legend" => verdict(sonic_rs::from_slice::<LegendIgnored>(document)),
        "lazy-legend" => verdict(sonic_rs::from_slice::<LegendLazy>(document)),
        // Routed through sonic's Deserializer but built with serde's data model,
        // which is the only sonic path that passes through `with_depth_limit`.
        "serde-value" => verdict(sonic_rs::from_slice::<serde_json::Value>(document)),
        "serde-json-value" => verdict(serde_json::from_slice::<serde_json::Value>(document)),
        "serde-json-ignored" => verdict(serde_json::from_slice::<IgnoredAny>(document)),
        other => println!("verdict=unknown shape {other:?}"),
    }
}

fn verdict<T, E: std::fmt::Display>(outcome: Result<T, E>) {
    match outcome {
        Ok(value) => {
            // The value is dropped inside the guarded section: a recursive
            // Drop is its own way to overflow the stack, and hiding it behind
            // the report would misattribute the crash.
            drop(value);
            println!("verdict=Ok (parsed and dropped)");
        }
        Err(error) => println!("verdict=Err {error}"),
    }
}
