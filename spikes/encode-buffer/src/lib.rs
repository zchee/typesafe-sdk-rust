//! Spike S6: the body-encode buffer strategies of plan section 3.3 step 1.
//!
//! sonic-rs reserves `value.len() * 6 + 35` bytes before every string write
//! (v0.5.10 `src/format.rs:266`, plain `Vec::reserve` in `src/writer.rs:78-82`),
//! so a buffer sized to the final body length re-allocates on every call and a
//! 1 MB `state` grows it past 6 MB. Each variant here is a different answer to
//! that, and the binary measures what each one costs on the second identical
//! call.
//!
//! The variants are in this library rather than in the binary so that the dhat
//! run and the divan run measure the same code. Timing must not go through an
//! instrumented allocator, which is why they are separate targets.

pub mod escape;
pub mod state;

use std::cell::{Cell, RefCell};

use bytes::{Bytes, BytesMut};

pub use crate::state::State;

/// The model name, already escaped, as the SDK will hold it.
pub const MODEL: &[u8] = b"\"jev-latest\"";

/// A prepared question set, already serialized: the bytes a `PreparedQuestions`
/// splices in without re-encoding. 297 bytes.
pub const QUESTIONS: &[u8] = br#"{"billing":{"type":"noul","instructions":"Is this about billing?","yes":"payments or invoices"},"tone":{"type":"choice","instructions":"What is the tone?","criteria":{"calm":"neutral or polite","angry":"annoyed or hostile"}},"urgency":{"type":"score","criteria":["can wait","this week","today"]}}"#;

const PREFIX: &[u8] = br#"{"state":"#;
const AFTER_STATE: &[u8] = br#","model":"#;
const AFTER_MODEL: &[u8] = br#","questions":"#;

/// Which strategy produced a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// (i) one `Vec<u8>` per call, sized from the previous call's post-encode
    /// `capacity()`.
    CapacityHint,
    /// (ii) exact-size two-pass escaping, scalar escaper. String states only.
    ExactTwoPass,
    /// (ii) with `json-escape-simd` in place of the scalar escaper.
    ExactTwoPassSimd,
    /// (iii) thread-local retained scratch with a decaying hint, then an exact
    /// copy into a right-sized body buffer. Shrinks to the 8x ceiling, which is
    /// the rule as the plan states it.
    RetainedScratch,
    /// (iii) with the shrink taken in ONE step down to the hint instead of down
    /// to the ceiling, so the shrink condition stops holding immediately.
    RetainedScratchOneShot,
    /// (iv) (ii) for string states, (iii) for everything else.
    Combined,
    /// (iv) with the one-shot shrink.
    CombinedOneShot,
}

/// How far a shrink goes once the scratch has outgrown its ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shrink {
    /// Down to `8 * hint`: the ceiling itself. The hint keeps decaying, so the
    /// capacity stays above the new ceiling and the next call shrinks again.
    ToCeiling,
    /// Down to `hint`: one re-allocation, after which `capacity > 8 * hint` is
    /// false until the hint has decayed by another factor of eight.
    ToHint,
}

impl Variant {
    /// Parses the name used on the command line and in the ledger.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "i" => Some(Self::CapacityHint),
            "ii" => Some(Self::ExactTwoPass),
            "ii-simd" => Some(Self::ExactTwoPassSimd),
            "iii" => Some(Self::RetainedScratch),
            "iii-1shot" => Some(Self::RetainedScratchOneShot),
            "iv" => Some(Self::Combined),
            "iv-1shot" => Some(Self::CombinedOneShot),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::CapacityHint => "i",
            Self::ExactTwoPass => "ii",
            Self::ExactTwoPassSimd => "ii-simd",
            Self::RetainedScratch => "iii",
            Self::RetainedScratchOneShot => "iii-1shot",
            Self::Combined => "iv",
            Self::CombinedOneShot => "iv-1shot",
        }
    }

    /// Encodes one request body.
    ///
    /// # Errors
    ///
    /// Returns the codec's error if the state cannot be serialized.
    pub fn encode(self, state: &State) -> Result<Bytes, sonic_rs::Error> {
        match self {
            Self::CapacityHint => capacity_hint(state),
            Self::ExactTwoPass => exact_two_pass(state, Escaper::Scalar),
            Self::ExactTwoPassSimd => exact_two_pass(state, Escaper::Simd),
            Self::RetainedScratch => retained_scratch(state, Shrink::ToCeiling),
            Self::RetainedScratchOneShot => retained_scratch(state, Shrink::ToHint),
            Self::Combined => match state {
                State::Text(_) => exact_two_pass(state, Escaper::Scalar),
                State::Object(_) => retained_scratch(state, Shrink::ToCeiling),
            },
            Self::CombinedOneShot => match state {
                State::Text(_) => exact_two_pass(state, Escaper::Scalar),
                State::Object(_) => retained_scratch(state, Shrink::ToHint),
            },
        }
    }

    /// Bytes this variant is holding on to after a call, per thread.
    ///
    /// Variant (i) hands its buffer out as the body, so it retains nothing.
    /// The two-pass variants retain nothing for a string state but do fall
    /// through to the scratch for an object state, so they are asked the same
    /// question as the scratch variants rather than answered with a constant.
    #[must_use]
    pub fn retained_capacity(self) -> usize {
        match self {
            Self::CapacityHint => 0,
            Self::ExactTwoPass
            | Self::ExactTwoPassSimd
            | Self::RetainedScratch
            | Self::RetainedScratchOneShot
            | Self::Combined
            | Self::CombinedOneShot => scratch_capacity(),
        }
    }

    /// The decayed size hint this variant carries, per thread.
    #[must_use]
    pub fn hint(self) -> usize {
        match self {
            Self::CapacityHint => CAPACITY_HINT.get(),
            Self::ExactTwoPass
            | Self::ExactTwoPassSimd
            | Self::RetainedScratch
            | Self::RetainedScratchOneShot
            | Self::Combined
            | Self::CombinedOneShot => SCRATCH_HINT.get(),
        }
    }
}

/// Appends the whole body to `out`, letting the codec grow it as it likes.
fn encode_into(out: &mut Vec<u8>, state: &State) -> Result<(), sonic_rs::Error> {
    out.extend_from_slice(PREFIX);
    match state {
        State::Text(text) => sonic_rs::to_writer(&mut *out, text)?,
        State::Object(object) => sonic_rs::to_writer(&mut *out, object)?,
    }
    out.extend_from_slice(AFTER_STATE);
    out.extend_from_slice(MODEL);
    out.extend_from_slice(AFTER_MODEL);
    out.extend_from_slice(QUESTIONS);
    out.push(b'}');
    Ok(())
}

// ------------------------------------------------------------- variant (i)

thread_local! {
    static CAPACITY_HINT: Cell<usize> = const { Cell::new(0) };
}

/// One fresh `Vec<u8>` per call, pre-sized from what the last call ended up
/// needing. The hint tracks `capacity()`, not `len()`, so it carries the
/// codec's 6x reservation forward and the second call does not re-allocate.
/// The body handed out is that same buffer, so nothing is retained.
fn capacity_hint(state: &State) -> Result<Bytes, sonic_rs::Error> {
    let mut buffer = Vec::with_capacity(CAPACITY_HINT.get());
    encode_into(&mut buffer, state)?;
    CAPACITY_HINT.set(buffer.capacity());
    Ok(Bytes::from(buffer))
}

// ------------------------------------------------------------ variant (ii)

#[derive(Clone, Copy)]
enum Escaper {
    Scalar,
    Simd,
}

/// Two passes over a string state: measure the escaped length, allocate exactly
/// that much, write once. An object state cannot be measured without
/// serializing it, so it falls through to the retained scratch.
fn exact_two_pass(state: &State, escaper: Escaper) -> Result<Bytes, sonic_rs::Error> {
    let State::Text(text) = state else {
        return retained_scratch(state, Shrink::ToCeiling);
    };

    let total = PREFIX.len()
        + escape::escaped_len(text)
        + AFTER_STATE.len()
        + MODEL.len()
        + AFTER_MODEL.len()
        + QUESTIONS.len()
        + 1;

    let mut buffer = Vec::with_capacity(total);
    buffer.extend_from_slice(PREFIX);
    match escaper {
        Escaper::Scalar => escape::write_escaped(text, &mut buffer),
        // This call reserves `len * 6 + 35` past the current end whatever the
        // caller did, so the exact sizing above is thrown away: the variant is
        // measured to show that, not because it can work.
        Escaper::Simd => json_escape_simd::escape_into(text, &mut buffer),
    }
    buffer.extend_from_slice(AFTER_STATE);
    buffer.extend_from_slice(MODEL);
    buffer.extend_from_slice(AFTER_MODEL);
    buffer.extend_from_slice(QUESTIONS);
    buffer.push(b'}');

    Ok(Bytes::from(buffer))
}

// ----------------------------------------------------------- variant (iii)

thread_local! {
    static SCRATCH: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    static SCRATCH_HINT: Cell<usize> = const { Cell::new(0) };
}

/// Encodes into a buffer this thread keeps between calls, then copies the exact
/// bytes into a right-sized body buffer.
///
/// The scratch is TAKEN out of the thread-local rather than borrowed: the
/// caller's `Serialize` implementation runs while the buffer is in hand and may
/// itself encode a body, and a `RefCell` borrow held across that would panic.
fn retained_scratch(state: &State, shrink: Shrink) -> Result<Bytes, sonic_rs::Error> {
    let mut scratch = SCRATCH.with(|cell| cell.borrow_mut().take()).unwrap_or_default();
    scratch.clear();

    let outcome = encode_into(&mut scratch, state);
    // `to_vec` allocates exactly `len` bytes, so the body buffer has
    // `len == capacity` and `Bytes::from` takes it over without copying again.
    let body = outcome.map(|()| Bytes::from(scratch.as_slice().to_vec()));

    // The hint decays by a sixteenth per call, so one large outlier stops
    // pinning the buffer after a few dozen small calls instead of for ever.
    let hint = SCRATCH_HINT.get();
    let decayed = scratch.len().max(hint - hint / 16);
    SCRATCH_HINT.set(decayed);
    let ceiling = decayed.saturating_mul(8);
    if scratch.capacity() > ceiling {
        scratch.shrink_to(match shrink {
            Shrink::ToCeiling => ceiling,
            Shrink::ToHint => decayed,
        });
    }
    SCRATCH.with(|cell| cell.replace(Some(scratch)));

    body
}

/// Bytes the retained scratch is holding on this thread.
#[must_use]
pub fn scratch_capacity() -> usize {
    SCRATCH.with(|cell| cell.borrow().as_ref().map_or(0, Vec::capacity))
}

/// Drops this thread's retained scratch and resets every hint, so that a
/// scenario starts from the same state as a fresh thread.
pub fn reset() {
    SCRATCH.with(|cell| cell.replace(None));
    SCRATCH_HINT.set(0);
    CAPACITY_HINT.set(0);
}

// ------------------------------------------------- body-type conversions

/// How a finished buffer becomes the `Bytes` the request body holds.
#[derive(Debug, Clone, Copy)]
pub enum Conversion {
    /// `Bytes::from(vec)` where the vector is exactly full.
    VecExact,
    /// `Bytes::from(vec)` where the vector has spare capacity, which is what a
    /// codec-grown buffer looks like.
    VecSlack,
    /// `BytesMut::freeze()`.
    BytesMutFreeze,
}

impl Conversion {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "vec-exact" => Some(Self::VecExact),
            "vec-slack" => Some(Self::VecSlack),
            "bytesmut-freeze" => Some(Self::BytesMutFreeze),
            _ => None,
        }
    }

    /// Builds a buffer of `len` bytes in the shape this conversion describes.
    #[must_use]
    pub fn build(self, len: usize) -> Source {
        match self {
            Self::VecExact => Source::Vec(vec![b'x'; len]),
            Self::VecSlack => {
                let mut buffer = Vec::with_capacity(len * 6 + 35);
                buffer.resize(len, b'x');
                Source::Vec(buffer)
            }
            Self::BytesMutFreeze => {
                let mut buffer = BytesMut::with_capacity(len);
                buffer.resize(len, b'x');
                Source::BytesMut(buffer)
            }
        }
    }
}

/// A buffer waiting to be turned into `Bytes`.
#[derive(Debug)]
pub enum Source {
    Vec(Vec<u8>),
    BytesMut(BytesMut),
}

impl Source {
    #[must_use]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Vec(buffer) => buffer.capacity(),
            Self::BytesMut(buffer) => buffer.capacity(),
        }
    }

    #[must_use]
    pub fn freeze(self) -> Bytes {
        match self {
            Self::Vec(buffer) => Bytes::from(buffer),
            Self::BytesMut(buffer) => buffer.freeze(),
        }
    }
}
