//! What one request body costs in allocations.
//!
//! The budget is stated in dhat's `total_blocks` and `total_bytes` deltas
//! rather than in live bytes, because every re-allocation is charged as one
//! block whose byte contribution is the full new size - which is exactly what
//! makes a buffer that grows inside a call visible here.
//!
//! Everything is measured on the **second** identical call. The first call of
//! a shape fills the thread's encode scratch; the SDK's steady state is what
//! follows it.
//!
//! One profiler exists per process, so all of it runs in a single test.

use bytes::Bytes;
use serde::Serialize;
use typesafe_sdk::__internals as codec;

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The model name as the SDK holds it: already a JSON string literal.
const MODEL: &[u8] = b"\"jev-latest\"";

/// A prepared question set, already serialized. 297 bytes, spliced in without
/// being encoded again.
const QUESTIONS: &[u8] = br#"{"billing":{"type":"noul","instructions":"Is this about billing?","yes":"payments or invoices"},"tone":{"type":"choice","instructions":"What is the tone?","criteria":{"calm":"neutral or polite","angry":"annoyed or hostile"}},"urgency":{"type":"score","criteria":["can wait","this week","today"]}}"#;

/// An object `state`, the shape whose length cannot be known before it is
/// encoded.
#[derive(Serialize)]
struct Ticket<'a> {
    subject: &'a str,
    body: &'a str,
}

/// The change in dhat's counters across one section.
#[derive(Clone, Copy)]
struct Measured {
    blocks: u64,
    bytes: u64,
}

fn measure<F, T>(body: F) -> (Measured, T)
where
    F: FnOnce() -> T,
{
    let before = dhat::HeapStats::get();
    let value = body();
    let after = dhat::HeapStats::get();
    (
        Measured {
            blocks: after.total_blocks - before.total_blocks,
            bytes: after.total_bytes - before.total_bytes,
        },
        value,
    )
}

/// `{"state":<state>,"model":"jev-latest","questions":<prepared>}`.
fn encode_body<T>(state: &T) -> Bytes
where
    T: Serialize + ?Sized,
{
    codec::encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"state":"#);
        codec::encode_into(buffer, state)?;
        buffer.extend_from_slice(br#","model":"#);
        buffer.extend_from_slice(MODEL);
        buffer.extend_from_slice(br#","questions":"#);
        buffer.extend_from_slice(QUESTIONS);
        buffer.push(b'}');
        Ok(())
    })
    .expect("the body encodes")
}

/// Text of roughly `len` bytes that the encoder has real work to do on: a
/// quote to escape and a multi-byte character to copy.
fn filler(len: usize) -> String {
    const PATTERN: &str = "The ticket said \"it broke\" again, caf\u{e9} closed. ";
    let mut text = PATTERN.repeat(len.div_ceil(PATTERN.len()));
    while !text.is_char_boundary(len) {
        text.push(' ');
    }
    text.truncate(len);
    text
}

/// Encodes the same body twice and reports what the second call cost.
fn steady_state<F>(label: &str, encode: F) -> Measured
where
    F: Fn() -> Bytes,
{
    drop(encode());
    let (measured, body) = measure(&encode);

    let allowance = (body.len() as f64 * 1.05) as u64 + 4096;
    println!(
        "{label:<14} body={:>9} blocks={} bytes={:>9} allowance={allowance:>9} \
         scratch={} hint={}",
        body.len(),
        measured.blocks,
        measured.bytes,
        codec::scratch_capacity(),
        codec::scratch_hint()
    );

    assert_eq!(
        measured.blocks, 1,
        "{label}: a repeated body must cost the one copy into the request buffer"
    );
    assert!(
        measured.bytes <= allowance,
        "{label}: allocated {} bytes for a body of {}",
        measured.bytes,
        body.len()
    );
    assert!(
        codec::scratch_capacity() <= codec::scratch_hint().saturating_mul(8),
        "{label}: the retained scratch is {} bytes against a hint of {}",
        codec::scratch_capacity(),
        codec::scratch_hint()
    );

    measured
}

#[test]
fn a_repeated_request_body_costs_one_block() {
    let _profiler = dhat::Profiler::builder().testing().build();

    for (label, len) in
        [("1 KB string", 1024), ("64 KB string", 64 * 1024), ("1 MB string", 1024 * 1024)]
    {
        codec::reset_scratch();
        let state = filler(len);
        steady_state(label, || encode_body(state.as_str()));
    }

    codec::reset_scratch();
    let subject = filler(64);
    let long_body = filler(1024 * 1024);
    let ticket = Ticket { subject: &subject, body: &long_body };
    steady_state("1 MB object", || encode_body(&ticket));

    mixed_sizes_return_to_one_block();
}

/// One large body followed by sixteen small ones.
///
/// The scratch is retained, so the small calls start out allocating nothing
/// while holding on to megabytes. The size hint decays a sixteenth per call
/// until the buffer is more than eight times too large, and the single shrink
/// that follows goes all the way down to the hint - not to the eight-times
/// ceiling, which the still-decaying hint would cross again on the very next
/// call, re-allocating the whole buffer every time.
fn mixed_sizes_return_to_one_block() {
    codec::reset_scratch();

    let large = filler(1024 * 1024);
    let small = filler(1024);

    let (first, large_body) = measure(|| encode_body(large.as_str()));
    println!(
        "mixed: 1 MB     body={:>9} blocks={} bytes={:>9} scratch={}",
        large_body.len(),
        first.blocks,
        first.bytes,
        codec::scratch_capacity()
    );

    let mut shrinks = 0;
    let mut last = Measured { blocks: 0, bytes: 0 };
    for call in 1..=16 {
        let (measured, body) = measure(|| encode_body(small.as_str()));
        println!(
            "mixed: 1 KB #{call:<2} body={:>9} blocks={} bytes={:>9} scratch={} hint={}",
            body.len(),
            measured.blocks,
            measured.bytes,
            codec::scratch_capacity(),
            codec::scratch_hint()
        );
        assert!(
            measured.blocks <= 2,
            "call {call} cost {} blocks; only a shrink may add one",
            measured.blocks
        );
        if measured.blocks == 2 {
            shrinks += 1;
        }
        last = measured;
    }

    assert!(
        shrinks <= 1,
        "the scratch shrank {shrinks} times; shrinking to the ceiling instead of \
         to the hint is what makes it repeat"
    );
    assert_eq!(last.blocks, 1, "after the outlier the per-call cost must be back to one block");
    assert!(
        codec::scratch_capacity() <= codec::scratch_hint().saturating_mul(8),
        "the scratch still holds {} bytes against a hint of {}",
        codec::scratch_capacity(),
        codec::scratch_hint()
    );
    assert!(
        codec::scratch_capacity() < large.len(),
        "the megabyte buffer is still retained: {} bytes",
        codec::scratch_capacity()
    );
}
