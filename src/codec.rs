//! JSON encoding and decoding for the whole crate.
//!
//! This is the only module that names `sonic_rs`. No type of that crate
//! appears in any signature outside it, so replacing the codec is a change to
//! this file alone.
//!
//! Four things here are not what a plain `serde_json` wrapper would do:
//!
//! * **The encode buffer is retained per thread.** Before every string write
//!   the codec reserves `len * 6 + 35` bytes, so a buffer sized to the final
//!   body re-allocates on every call. [`encode_body`] writes into a scratch
//!   buffer this thread keeps between calls and copies the finished bytes into
//!   an exactly sized one.
//! * **Decoding runs a depth pre-scan first.** The codec has no recursion
//!   limit on the paths this crate uses, and it aborts the process rather than
//!   returning an error when the parser runs out of stack. A counter inside a
//!   `serde` `Visitor` cannot help, because the overflow happens inside the
//!   parser before the visitor is entered again; the guard has to be a pass
//!   over the raw bytes. See [`check_depth`].
//! * **Decode errors are rebuilt rather than forwarded.** The codec's own
//!   `Display` embeds a multi-line excerpt of the input, and `serde`'s
//!   type-mismatch messages quote the offending value; a `state` may carry
//!   personal data, so [`DecodeError`] keeps only a kind, a position and a
//!   field path.
//! * **Raw JSON has a path for this codec and a path for every other one.**
//!   Splicing text in unchanged, and capturing it on the way back, are both
//!   protocols private to this codec. [`RawJson`] is public and users will
//!   hand it to a codec of their own, so it also knows how to write itself out
//!   as ordinary data and to read ordinary data back. See [`serialize_raw`]
//!   and [`deserialize_raw`].

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    fmt,
    marker::PhantomData,
};

use bytes::Bytes;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de,
    de::{DeserializeSeed, IgnoredAny},
    ser,
    ser::{SerializeMap, SerializeSeq, SerializeStruct},
};
use thiserror::Error;

use crate::text::{Backslash, SafeText};

/// The deepest JSON nesting this crate will parse.
///
/// Anything deeper is rejected before a byte reaches the parser. The limit is
/// far below what any documented API response needs; it exists because the
/// parser's failure mode on deep input is a process abort.
pub(crate) const MAX_JSON_DEPTH: usize = 16;

// ------------------------------------------------------------------ errors

/// The reason a JSON document could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DecodeErrorKind {
    /// The document is nested deeper than the 16 levels this crate parses, and
    /// was rejected without being parsed.
    TooDeep,
    /// The bytes are not syntactically valid JSON, or they end early.
    Syntax,
    /// The document parses, but its shape does not match the expected type: a
    /// field is missing, or a value has the wrong type.
    Data,
}

/// A JSON document could not be decoded into the expected type.
///
/// The error deliberately carries no part of the input. A decoded document may
/// contain application state and therefore personal data, and both the codec's
/// own error text and `serde`'s type-mismatch messages quote the input. What
/// is kept is the kind, the position and the field path, which are derived
/// from the expected type rather than from the values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{detail}")]
#[non_exhaustive]
pub struct DecodeError {
    detail: Detail,
}

/// The rendered forms of [`DecodeError`], kept private so that the public type
/// can gain fields without breaking callers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
enum Detail {
    #[error("JSON input is nested deeper than the maximum of {}", MAX_JSON_DEPTH)]
    TooDeep,
    #[error("invalid JSON syntax at line {line} column {column}")]
    Syntax { line: usize, column: usize },
    #[error("unexpected JSON value at `{path}`, line {line} column {column}")]
    Data { path: Box<str>, line: usize, column: usize },
    /// The document does not have the shape the type expects, and the codec
    /// could say neither where nor why. Rendering an empty path as ``at ` ` ``
    /// would say less than saying nothing.
    #[error("the JSON document does not have the expected shape")]
    Opaque,
}

impl DecodeError {
    /// Which of the three failure classes this is.
    #[must_use]
    pub fn kind(&self) -> DecodeErrorKind {
        match self.detail {
            Detail::TooDeep => DecodeErrorKind::TooDeep,
            Detail::Syntax { .. } => DecodeErrorKind::Syntax,
            Detail::Data { .. } | Detail::Opaque => DecodeErrorKind::Data,
        }
    }

    /// The one-based line the parser stopped at, or 0 when the document was
    /// rejected before it was parsed.
    #[must_use]
    pub fn line(&self) -> usize {
        match self.detail {
            Detail::TooDeep | Detail::Opaque => 0,
            Detail::Syntax { line, .. } | Detail::Data { line, .. } => line,
        }
    }

    /// The one-based column the parser stopped at, or 0 when the document was
    /// rejected before it was parsed.
    #[must_use]
    pub fn column(&self) -> usize {
        match self.detail {
            Detail::TooDeep | Detail::Opaque => 0,
            Detail::Syntax { column, .. } | Detail::Data { column, .. } => column,
        }
    }

    /// The path of the offending field, with dotted names and bracketed
    /// indices, for example `answers.tone.confidence` or `models[1].name`.
    ///
    /// The path of the document root is `.`, and it is empty when the failure
    /// carries no path at all.
    ///
    /// A name in the path may be an object key the input chose, so it is
    /// rendered safe to print: a control character or a format character that
    /// reorders or hides text is written as a Rust escape (`\n`, `\u{1b}`,
    /// `\u{202e}`), a backslash is written `\\` so that an escape cannot be
    /// mistaken for text, and other printable text, non-ASCII included, is kept
    /// as it is.
    /// Each name is cut at 128 characters and the whole path at 320, counted
    /// after escaping, and a cut is marked with U+2026.
    #[must_use]
    pub fn path(&self) -> &str {
        match &self.detail {
            Detail::TooDeep | Detail::Syntax { .. } | Detail::Opaque => "",
            Detail::Data { path, .. } => path,
        }
    }

    fn too_deep() -> Self {
        Self { detail: Detail::TooDeep }
    }
}

/// A value could not be encoded as JSON.
///
/// The message comes from the codec's serializer, which reports the failing
/// step - a map key that is not a string, a boolean or a number, or an error
/// returned by the value's own [`Serialize`] implementation - and never quotes
/// the value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("the value could not be encoded as JSON: {message}")]
#[non_exhaustive]
pub struct EncodeError {
    message: Box<str>,
}

impl EncodeError {
    /// The serializer's description of what went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Serialization errors carry no position and no input excerpt, so the
    /// codec's `Display` is safe to keep here. Decode errors are not, which is
    /// why [`DecodeError`] is rebuilt from parts instead.
    fn from_codec(error: sonic_rs::Error) -> Self {
        Self { message: error.to_string().into_boxed_str() }
    }
}

// ---------------------------------------------------------------- encoding

thread_local! {
    /// The buffer [`encode_body`] writes into, kept between calls.
    ///
    /// It is an `Option` because the buffer is taken out for the duration of a
    /// call rather than borrowed: the closure runs arbitrary `Serialize`
    /// implementations, and one of them encoding a body of its own would panic
    /// on a `RefCell` borrow held across it.
    static SCRATCH: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    /// The recent body size this thread encodes, decayed by a sixteenth per
    /// call so that a single large outlier stops pinning the buffer.
    static SCRATCH_HINT: Cell<usize> = const { Cell::new(0) };
}

/// The most encode scratch a thread keeps between calls: 8 MiB.
///
/// The codec reserves six times a string's length before writing it, so the
/// scratch a large string state leaves behind is six times the state, and a
/// thread would keep it until later calls on that same thread decayed it
/// away - never, on a thread that goes idle. A 64 MiB state left
/// 402,653,228 B on its thread. A scratch over this size is dropped after its
/// call instead, and a state that large grows it afresh on every call, as a
/// first call does. A 1 MB state, the largest one a call is budgeted for,
/// leaves 6,291,587 B, under the ceiling, so its calls keep reusing it.
const MAX_RETAINED_SCRATCH: usize = 8 * 1024 * 1024;

/// Appends the JSON form of `value` to `buf`.
///
/// The buffer is not cleared, which is what lets a request body be spliced out
/// of literal fragments and encoded values in one pass.
///
/// # Errors
///
/// Returns [`EncodeError`] when the value cannot be represented as JSON: a
/// map whose keys are neither strings, booleans nor numbers, or a
/// [`Serialize`] implementation that returns an error of its own. A non-finite
/// float is not one of these - it is written as `null`, which is what
/// `serde_json` does as well. The buffer may then hold a partial encoding of
/// that value, so a caller that reuses it has to truncate it.
pub(crate) fn encode_into<T>(buf: &mut Vec<u8>, value: &T) -> Result<(), EncodeError>
where
    T: Serialize + ?Sized,
{
    // The mark is what tells `RawJson` that the serializer about to run is
    // this crate's own, and so that raw text may be spliced in verbatim.
    let _inside = EncoderMark::enter();
    sonic_rs::to_writer(&mut *buf, value).map_err(EncodeError::from_codec)
}

/// Appends `text` to `buf` as a JSON string literal, quoted and escaped.
pub(crate) fn write_json_string(buf: &mut Vec<u8>, text: &str) {
    encode_into(buf, text).expect("invariant: encoding a string into a Vec cannot fail");
}

/// Builds one request body and hands it over as exactly sized [`Bytes`].
///
/// `fill` writes the whole body into a buffer this thread keeps between calls,
/// so a repeated call of the same shape allocates once: the copy into the
/// returned buffer. A buffer that grew past [`MAX_RETAINED_SCRATCH`] is not
/// kept. That buffer has `len == capacity`, which makes
/// `Bytes::from` a move rather than a copy and defers the shared-header
/// allocation to the first `clone`.
///
/// The scratch buffer is taken out of the thread-local for the duration of the
/// call. A `Serialize` implementation that calls this function again therefore
/// gets a buffer of its own instead of corrupting the outer one, and a panic
/// inside `fill` drops the buffer rather than leaving a damaged one behind.
///
/// # Errors
///
/// Returns whatever `fill` returns.
pub(crate) fn encode_body<F>(fill: F) -> Result<Bytes, EncodeError>
where
    F: FnOnce(&mut Vec<u8>) -> Result<(), EncodeError>,
{
    let mut scratch = SCRATCH.with(|cell| cell.borrow_mut().take()).unwrap_or_default();
    scratch.clear();

    // `to_vec` allocates exactly `len` bytes, so the body buffer is full and
    // `Bytes::from` takes it over without copying again.
    let body = fill(&mut scratch).map(|()| Bytes::from(scratch.as_slice().to_vec()));

    let hint = SCRATCH_HINT.get();
    let decayed = scratch.len().max(hint - hint / 16);
    SCRATCH_HINT.set(decayed);
    if scratch.capacity() > MAX_RETAINED_SCRATCH {
        // Not shrunk to the hint either: a hint this large is the body that
        // just passed the ceiling, and keeping it would hold the memory the
        // ceiling exists to release.
        scratch = Vec::new();
    } else if scratch.capacity() > decayed.saturating_mul(8) {
        // The shrink goes all the way down to the hint rather than to the
        // bound: stopping at the bound leaves the capacity exactly where the
        // still-decaying hint lowers it again, so every following call
        // re-allocates the whole buffer.
        scratch.shrink_to(decayed);
    }
    SCRATCH.with(|cell| cell.replace(Some(scratch)));

    body
}

/// Bytes the encode scratch of this thread is holding on to.
#[cfg(any(test, feature = "internals"))]
pub(crate) fn scratch_capacity() -> usize {
    SCRATCH.with(|cell| cell.borrow().as_ref().map_or(0, Vec::capacity))
}

/// The decayed size hint this thread carries.
#[cfg(any(test, feature = "internals"))]
pub(crate) fn scratch_hint() -> usize {
    SCRATCH_HINT.get()
}

/// Drops this thread's encode scratch and its size hint, so that the next call
/// starts from the state of a fresh thread.
#[cfg(any(test, feature = "internals"))]
pub(crate) fn reset_scratch() {
    SCRATCH.with(|cell| cell.replace(None));
    SCRATCH_HINT.set(0);
}

// ---------------------------------------------------------------- decoding

/// Rejects JSON nested deeper than [`MAX_JSON_DEPTH`].
///
/// This runs over the raw bytes, before any of them reach the parser, and it
/// only counts brackets outside string literals. It does not validate the
/// document: unbalanced or misplaced brackets are the parser's business.
///
/// # Errors
///
/// Returns a [`DecodeErrorKind::TooDeep`] error at the first bracket that
/// crosses the limit.
pub(crate) fn check_depth(json: &[u8]) -> Result<(), DecodeError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for &byte in json {
        if in_string {
            if escaped {
                // Every escape sequence this matters for is one byte long
                // (`\"` and `\\`); the four hex digits of `\uXXXX` contain no
                // quote or backslash, so skipping one byte is enough.
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(DecodeError::too_deep());
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    Ok(())
}

/// Checks that `bytes` are UTF-8, and hands them back as text.
///
/// Every decode starts here, before the depth pre-scan and before any byte
/// reaches the codec, because the codec cannot be trusted with anything
/// else: when it reads a string it takes the bytes as text without checking
/// them (a `debug_assert!` in its debug builds, nothing in its release
/// builds), and it reports a byte that is not UTF-8 only once the whole
/// document has been read - after a string holding one has already been
/// handed on as text. The codec is then given the checked text, which it does
/// not check a second time.
///
/// # Errors
///
/// Returns a [`DecodeErrorKind::Syntax`] error at the first byte that is not
/// part of a UTF-8 character: JSON text is UTF-8 (RFC 8259, section 8.1), so a
/// document holding such a byte is not JSON. The position counts bytes, as the
/// codec's own positions do.
fn as_text(bytes: &[u8]) -> Result<&str, DecodeError> {
    std::str::from_utf8(bytes).map_err(|failure| {
        let before = &bytes[..failure.valid_up_to()];
        let line = 1 + before.iter().filter(|&&byte| byte == b'\n').count();
        let line_start = before.iter().rposition(|&byte| byte == b'\n').map_or(0, |at| at + 1);
        let column = 1 + before.len() - line_start;
        DecodeError { detail: Detail::Syntax { line, column } }
    })
}

/// Decodes `bytes` into `T`.
///
/// The successful path is a single pass and pays nothing for error reporting.
/// A failure is decoded a second time through a path-tracking wrapper, which
/// is what turns a bare position into `answers.tone.confidence`.
///
/// # Errors
///
/// Returns [`DecodeError`] when the input is not UTF-8, is nested too deeply,
/// is not valid JSON, or does not have the shape `T` expects.
pub(crate) fn decode<'de, T>(bytes: &'de [u8]) -> Result<T, DecodeError>
where
    T: Deserialize<'de>,
{
    let text = as_text(bytes)?;
    check_depth(bytes)?;
    // `PhantomData<T>` is serde's own seed for "decode a `T`", which is what
    // lets the failure pass below serve this function and `decode_seed` alike.
    sonic_rs::from_str::<T>(text).map_err(|_| describe_failure(text, PhantomData::<T>))
}

/// Decodes `bytes` through `seed`, a decoder that carries state of its own -
/// how many answers to make room for, for instance - which a type's
/// `Deserialize` cannot receive.
///
/// Everything else is [`decode`]: the same UTF-8 check and depth pre-scan,
/// one pass on success that accepts exactly what `decode` accepts, and on
/// failure the same second, path-tracking pass. That second pass needs the
/// seed again, which is why it is `Clone`: a seed is consumed by the pass it
/// drives.
///
/// # Errors
///
/// Returns [`DecodeError`] when the input is not UTF-8, is nested too deeply,
/// is not valid JSON, or does not have the shape the seed expects.
pub(crate) fn decode_seed<'de, S>(bytes: &'de [u8], seed: S) -> Result<S::Value, DecodeError>
where
    S: DeserializeSeed<'de> + Clone,
{
    let text = as_text(bytes)?;
    check_depth(bytes)?;
    // The codec's entry point for a type checks that the value is followed by
    // nothing but whitespace; its deserializer does that only when asked, with
    // `end`.
    let decoded = {
        let mut deserializer = sonic_rs::Deserializer::from_str(text);
        seed.clone()
            .deserialize(&mut deserializer)
            .ok()
            .and_then(|value| deserializer.end().ok().map(|()| value))
    };
    decoded.ok_or_else(|| describe_failure(text, seed))
}

/// Re-runs a failed decode with path tracking and turns the result into a
/// [`DecodeError`] that carries no part of the input.
///
/// `text` is what [`as_text`] returned, so it is read without a second check.
fn describe_failure<'de, S>(text: &'de str, seed: S) -> DecodeError
where
    S: DeserializeSeed<'de>,
{
    let mut deserializer = sonic_rs::Deserializer::from_str(text);
    let mut track = serde_path_to_error::Track::new();
    let failure = seed
        .deserialize(serde_path_to_error::Deserializer::new(&mut deserializer, &mut track))
        .err();
    let Some(inner) = failure else {
        // The tracked pass reads one value and stops there; unlike the first
        // pass it never looks at what follows it. So a document with anything
        // but whitespace after its value parses here and failed there, and
        // asking the deserializer to finish is the only way to get the
        // position of the byte the first pass tripped on.
        return match deserializer.end() {
            Err(trailing) => DecodeError {
                detail: Detail::Syntax { line: trailing.line(), column: trailing.column() },
            },
            // Both passes read the same bytes with the same type and
            // disagreed on whether they parse at all. Nothing about the
            // input can be reported beyond that disagreement.
            Ok(()) => DecodeError { detail: Detail::Opaque },
        };
    };
    let path = track.path();

    let line = inner.line();
    let column = inner.column();
    let category = inner.classify();

    if matches!(category, sonic_rs::error::Category::Syntax | sonic_rs::error::Category::Eof) {
        return DecodeError { detail: Detail::Syntax { line, column } };
    }

    // `serde` reports a missing field at the struct that misses it, and names
    // the field only in the message. The message itself is not kept - a
    // type-mismatch message quotes the offending value - but the field name in
    // it comes from the target type, so appending it is safe and gives a
    // missing and a wrongly typed field the same shape of path.
    let message = inner.to_string();
    let path = render_path(&path, missing_field_name(&message));

    DecodeError { detail: Detail::Data { path: path.into_boxed_str(), line, column } }
}

/// The most characters one name in a field path is rendered with before it is
/// cut and marked with an ellipsis.
///
/// A name in a path can be a key the server chose - a question name, a legend
/// level, a choice option - so it is bounded like any other server text in an
/// error. The bound is well above any name a caller would give a question.
const MAX_PATH_SEGMENT_CHARS: usize = 128;

/// The most characters a whole field path is rendered with before it is cut
/// and marked with an ellipsis.
///
/// It holds the deepest path the response schema has,
/// `answers.<name>.probabilities.<option>`, with both names at their own cap.
const MAX_PATH_CHARS: usize = 320;

/// Renders a field path the way `serde_path_to_error` does - dotted names,
/// bracketed indices, `.` for the root - with every name made safe to print.
///
/// A name keeps its printable text, non-ASCII included. A control character,
/// or a format character that reorders or hides the text around it, is
/// written as a Rust escape (`\n`, `\r`, `\t`, or `\u{1b}` for the others), so
/// that a key cannot break a log line, recolour a terminal or disguise itself;
/// a backslash is written `\\`, so that no escape can be mistaken for text.
/// Each name is capped at [`MAX_PATH_SEGMENT_CHARS`] characters and the whole
/// path at [`MAX_PATH_CHARS`], counted after escaping; a cut never splits a
/// character or an escape, and is marked with U+2026.
fn render_path(path: &serde_path_to_error::Path, missing: Option<&str>) -> String {
    use serde_path_to_error::Segment;

    let mut out = SafeText::new(MAX_PATH_CHARS, Backslash::Double);
    // A name is preceded by a dot unless it starts the path; an index never
    // is. Whether it starts the path cannot be read off the text, because a
    // key may be the empty string.
    let mut first = true;
    for segment in path {
        match segment {
            Segment::Seq { index } => out.fixed(&format!("[{index}]")),
            Segment::Map { key } | Segment::Enum { variant: key } => {
                if !first {
                    out.fixed(".");
                }
                out.untrusted(key, MAX_PATH_SEGMENT_CHARS);
            }
            Segment::Unknown => out.fixed(if first { "?" } else { ".?" }),
        }
        first = false;
    }
    match missing {
        Some(field) => {
            if !first {
                out.fixed(".");
            }
            out.untrusted(field, MAX_PATH_SEGMENT_CHARS);
        }
        None if first => out.fixed("."),
        None => {}
    }
    out.into_string()
}

/// Extracts `noul` from ``missing field `noul` at line 1 column 101``.
fn missing_field_name(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("missing field `")?;
    let end = rest.find('`')?;
    Some(&rest[..end])
}

// ------------------------------------------------------------- raw JSON

/// The struct name sonic-rs reads as "the one field below is JSON text
/// already, write it out unchanged".
///
/// The constant is private to that crate, so the name is spelled out here
/// rather than reached by serializing a `sonic_rs::LazyValue`: building one of
/// those needs a parse of the text, and the parser has no recursion limit, so
/// that would put a process abort on the outbound path for a deeply nested
/// value. Should a later sonic-rs rename the token, the splice degrades into
/// an ordinary one-field object and the round-trip tests fail on it.
const SPLICE_TOKEN: &str = "$sonic_rs::LazyValue";

thread_local! {
    /// How many [`encode_into`] calls this thread is inside.
    ///
    /// A counter rather than a flag, because a `Serialize` implementation the
    /// encoder reaches may encode a value of its own.
    static INSIDE_SDK_ENCODER: Cell<u32> = const { Cell::new(0) };
}

/// Marks this thread as being inside the SDK's serializer while it lives.
///
/// serde offers no way to ask a `Serializer` which implementation it is, and
/// the verbatim splice below is a protocol only this crate's codec
/// understands. The mark is therefore set where the codec's serializer is
/// built - [`encode_into`], the single place in the crate that builds one -
/// and read by [`serialize_raw`].
struct EncoderMark;

impl EncoderMark {
    fn enter() -> Self {
        INSIDE_SDK_ENCODER.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }

    /// Whether the value being serialized on this thread is on its way into
    /// the SDK's own encoder.
    fn is_set() -> bool {
        INSIDE_SDK_ENCODER.with(|depth| depth.get() > 0)
    }
}

impl Drop for EncoderMark {
    fn drop(&mut self) {
        INSIDE_SDK_ENCODER.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Writes raw JSON text through `serializer`.
///
/// Inside this crate's encoder the text is spliced in byte for byte. Through
/// any other serializer it is streamed out as ordinary JSON data instead, so
/// that a value which travels through, say, `serde_json` carries the data it
/// holds rather than a protocol this crate's codec invented.
///
/// # Errors
///
/// Returns the serializer's own error, and a too-deep error when a value on
/// the transcoding path is nested deeper than [`MAX_JSON_DEPTH`].
fn serialize_raw<S>(text: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if EncoderMark::is_set() { splice(text, serializer) } else { transcode(text, serializer) }
}

/// Hands `text` to this crate's codec for a verbatim splice.
///
/// Nothing parses the text here: a `RawJson` only ever holds text the codec
/// produced or captured, so the splice writes bytes the codec has already
/// accepted once.
fn splice<S>(text: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut raw = serializer.serialize_struct(SPLICE_TOKEN, 1)?;
    raw.serialize_field(SPLICE_TOKEN, text)?;
    raw.end()
}

/// Streams the JSON value in `text` into a serializer that is not this crate's
/// codec.
///
/// Nothing is buffered and no value tree is built: every value read out of
/// `text` is handed straight to `serializer`. The data is preserved; its
/// spelling is not, because the target serializer chooses its own number
/// format and drops the insignificant whitespace the text may carry.
///
/// # Errors
///
/// Returns a too-deep error when `text` is nested deeper than
/// [`MAX_JSON_DEPTH`], and otherwise whatever `serializer` returns.
fn transcode<S>(text: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    // Both the transcode and the parser driving it descend one stack frame per
    // nesting level. The cap the decode path uses bounds that recursion, which
    // is what stops a `RawJson` built by `RawJson::from_value` from an
    // arbitrarily deep value from overflowing the stack here; the splice path
    // above does not read the text at all and so needs no cap.
    check_depth(text.as_bytes()).map_err(ser::Error::custom)?;

    let mut source = sonic_rs::Deserializer::from_str(text);
    Transcoder::new(&mut source).serialize(serializer)
}

/// The prefix that marks a serializer error on its way out through the
/// deserializer driving a transcode.
const REFUSED: &str = "the JSON writer refused a value: ";

/// Wraps a serializer error so that it survives the trip out through the
/// deserializer. The two halves of a transcode share no error type, so the
/// message is all that can cross.
fn writer_refused<E, D>(error: E) -> D
where
    E: fmt::Display,
    D: de::Error,
{
    de::Error::custom(format_args!("{REFUSED}{error}"))
}

/// Turns the error a transcode comes back with into a serializer error.
///
/// Only a message [`writer_refused`] marked is passed on. Anything else was
/// raised by the parser, whose `Display` embeds an excerpt of what it was
/// reading, and the text of a `RawJson` may be application data.
fn transcode_failed<E, S>(error: E) -> S
where
    E: fmt::Display,
    S: ser::Error,
{
    let rendered = error.to_string();
    match rendered.split_once(REFUSED) {
        Some((_, message)) => ser::Error::custom(message),
        None => ser::Error::custom("the stored JSON text could not be read back"),
    }
}

/// A `Serialize` that writes whatever one deserializer yields.
///
/// serde drives serializing from the value side and deserializing from the
/// visitor side, so a transcode has to hand the serializer something that
/// implements `Serialize` and pulls from a deserializer when it is asked to
/// write. That single call consumes the deserializer, which is why it sits in
/// a `RefCell<Option<_>>` rather than being held by value.
struct Transcoder<D> {
    source: RefCell<Option<D>>,
}

impl<D> Transcoder<D> {
    fn new(source: D) -> Self {
        Self { source: RefCell::new(Some(source)) }
    }
}

impl<'de, D> Serialize for Transcoder<D>
where
    D: Deserializer<'de>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(source) = self.source.borrow_mut().take() else {
            return Err(ser::Error::custom("a raw JSON value can be written only once"));
        };
        source.deserialize_any(TranscodeVisitor { serializer }).map_err(transcode_failed)
    }
}

/// Hands every value it is shown straight to `serializer`.
struct TranscodeVisitor<S> {
    serializer: S,
}

impl<'de, S> de::Visitor<'de> for TranscodeVisitor<S>
where
    S: Serializer,
{
    type Value = S::Ok;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        self.serializer.serialize_bool(value).map_err(writer_refused)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        self.serializer.serialize_i64(value).map_err(writer_refused)
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Self::Value, E> {
        self.serializer.serialize_i128(value).map_err(writer_refused)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        self.serializer.serialize_u64(value).map_err(writer_refused)
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Self::Value, E> {
        self.serializer.serialize_u128(value).map_err(writer_refused)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        self.serializer.serialize_f64(value).map_err(writer_refused)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.serializer.serialize_str(value).map_err(writer_refused)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.serializer.serialize_unit().map_err(writer_refused)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.serializer.serialize_none().map_err(writer_refused)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut sequence =
            self.serializer.serialize_seq(access.size_hint()).map_err(writer_refused)?;
        while access.next_element_seed(TranscodeElement { sequence: &mut sequence })?.is_some() {}
        sequence.end().map_err(writer_refused)
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut map = self.serializer.serialize_map(access.size_hint()).map_err(writer_refused)?;
        loop {
            let key = access.next_key_seed(TranscodeKey { map: &mut map })?;
            if key.is_none() {
                break;
            }
            access.next_value_seed(TranscodeValue { map: &mut map })?;
        }
        map.end().map_err(writer_refused)
    }
}

/// Writes the element the deserializer is positioned on into `sequence`.
struct TranscodeElement<'s, S> {
    sequence: &'s mut S,
}

impl<'de, S> de::DeserializeSeed<'de> for TranscodeElement<'_, S>
where
    S: SerializeSeq,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.sequence.serialize_element(&Transcoder::new(deserializer)).map_err(writer_refused)
    }
}

/// Writes the key the deserializer is positioned on into `map`.
struct TranscodeKey<'s, S> {
    map: &'s mut S,
}

impl<'de, S> de::DeserializeSeed<'de> for TranscodeKey<'_, S>
where
    S: SerializeMap,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.map.serialize_key(&Transcoder::new(deserializer)).map_err(writer_refused)
    }
}

/// Writes the value the deserializer is positioned on into `map`.
struct TranscodeValue<'s, S> {
    map: &'s mut S,
}

impl<'de, S> de::DeserializeSeed<'de> for TranscodeValue<'_, S>
where
    S: SerializeMap,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.map.serialize_value(&Transcoder::new(deserializer)).map_err(writer_refused)
    }
}

/// Rejects text that is not exactly one complete JSON value.
///
/// A [`RawJson`] is written into a request body without being read again, so
/// text carrying a second value would splice that value - a key of the
/// caller's choosing, say - into the enclosing object, and text that is not
/// JSON at all would make the whole body unparseable. Every value this codec
/// captures is one value already; what needs guarding is a string handed over
/// by a deserializer that answered the raw-text request with whatever the
/// caller had put in it.
///
/// The scan allocates nothing on the accepting path: the depth pre-scan reads
/// the bytes, and the parser then skips one value and checks that only
/// whitespace follows.
fn one_json_value<E: de::Error>(text: &str) -> Result<(), E> {
    decode::<IgnoredAny>(text.as_bytes()).map_err(de::Error::custom)?;
    Ok(())
}

/// Reads the next value as its raw JSON text, without interpreting it.
///
/// This crate's codec answers the request with the text of the value as the
/// wire carried it, borrowed from the input whenever it can be - which it
/// cannot when the value is a string holding escape sequences. Any other
/// deserializer does not know the request and passes itself on instead; the
/// value is then read as ordinary JSON and rendered back to compact text, so
/// what comes out holds the same data rather than failing.
///
/// A format that neither knows the request nor forwards itself, but answers it
/// with a bare string, cannot be told apart from the codec's own raw-text
/// answer, so the string is read as JSON text rather than as a JSON string.
/// Such text is checked before it is accepted: one complete JSON value is
/// taken at face value, and anything else - a second value after the first,
/// or text that is not JSON - is refused rather than carried into a request
/// body. No JSON codec behaves that way, so in practice this guards a string
/// a caller supplied by hand.
///
/// # Errors
///
/// Returns the deserializer's own error, and a too-deep error when a document
/// read through a foreign deserializer nests deeper than [`MAX_JSON_DEPTH`].
pub(crate) fn deserialize_raw<'de, D>(deserializer: D) -> Result<Cow<'de, str>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_newtype_struct(SPLICE_TOKEN, RawTextVisitor)
}

/// The two ways a value arrives at [`deserialize_raw`].
struct RawTextVisitor;

impl<'de> de::Visitor<'de> for RawTextVisitor {
    type Value = Cow<'de, str>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    /// This crate's codec, handing over the raw text of the value.
    ///
    /// The check is redundant for that codec, which never hands over anything
    /// else, and is what holds the invariant for a deserializer that answers
    /// the raw-text request with a string of the caller's own. The borrow
    /// survives it.
    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        one_json_value(value)?;
        Ok(Cow::Borrowed(value))
    }

    /// The same, for text the codec had to rebuild because it holds escapes.
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        one_json_value(value)?;
        Ok(Cow::Owned(value.to_owned()))
    }

    /// Any other deserializer: it does not know the request above, so it hands
    /// itself over and the value is read as data.
    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut out = Vec::new();
        deserializer.deserialize_any(Render { out: &mut out, depth: 0 })?;
        Ok(Cow::Owned(rendered_text(out)))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Cow::Borrowed(if value { "true" } else { "false" }))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        render_scalar(&value)
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Self::Value, E> {
        render_scalar(&value)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        render_scalar(&value)
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Self::Value, E> {
        render_scalar(&value)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        render_scalar(&value)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Cow::Borrowed("null"))
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Cow::Borrowed("null"))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.visit_newtype_struct(deserializer)
    }

    fn visit_seq<A>(self, access: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut out = Vec::new();
        de::Visitor::visit_seq(Render { out: &mut out, depth: 0 }, access)?;
        Ok(Cow::Owned(rendered_text(out)))
    }

    fn visit_map<A>(self, access: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut out = Vec::new();
        de::Visitor::visit_map(Render { out: &mut out, depth: 0 }, access)?;
        Ok(Cow::Owned(rendered_text(out)))
    }
}

/// Encodes one scalar on its own, for a value that reached
/// [`RawTextVisitor`] without any surrounding structure.
fn render_scalar<'de, T, E>(value: &T) -> Result<Cow<'de, str>, E>
where
    T: Serialize + ?Sized,
    E: de::Error,
{
    let mut out = Vec::new();
    encode_into(&mut out, value).map_err(de::Error::custom)?;
    Ok(Cow::Owned(rendered_text(out)))
}

/// The bytes a render wrote, as text.
fn rendered_text(out: Vec<u8>) -> String {
    String::from_utf8(out).expect("invariant: the renderer emits UTF-8")
}

/// Writes one JSON value, read from a deserializer that is not this crate's
/// codec, into `out` as compact JSON text.
///
/// It is both the seed that a container hands to its elements and the visitor
/// that writes them, so a nested document costs one stack frame per level and
/// no intermediate value.
struct Render<'b> {
    out: &'b mut Vec<u8>,
    depth: usize,
}

/// The depth one level in, or a too-deep error at the cap.
fn one_level_in<E: de::Error>(depth: usize) -> Result<usize, E> {
    if depth >= MAX_JSON_DEPTH {
        return Err(de::Error::custom(DecodeError::too_deep()));
    }
    Ok(depth + 1)
}

impl<'de> de::DeserializeSeed<'de> for Render<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> de::Visitor<'de> for Render<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        self.out.extend_from_slice(if value { b"true" } else { b"false" });
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        encode_into(self.out, &value).map_err(de::Error::custom)
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Self::Value, E> {
        encode_into(self.out, &value).map_err(de::Error::custom)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        encode_into(self.out, &value).map_err(de::Error::custom)
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Self::Value, E> {
        encode_into(self.out, &value).map_err(de::Error::custom)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        encode_into(self.out, &value).map_err(de::Error::custom)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        write_json_string(self.out, value);
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let inner = one_level_in(self.depth)?;
        let out = self.out;
        out.push(b'[');
        let mut first = true;
        loop {
            let element = RenderElement { out: &mut *out, depth: inner, first };
            if access.next_element_seed(element)?.is_none() {
                break;
            }
            first = false;
        }
        out.push(b']');
        Ok(())
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let inner = one_level_in(self.depth)?;
        let out = self.out;
        out.push(b'{');
        let mut first = true;
        loop {
            let key = RenderKey { out: &mut *out, first };
            if access.next_key_seed(key)?.is_none() {
                break;
            }
            out.push(b':');
            access.next_value_seed(Render { out: &mut *out, depth: inner })?;
            first = false;
        }
        out.push(b'}');
        Ok(())
    }
}

/// One element of an array, with the comma that precedes it.
///
/// The separator is written here rather than in the loop because whether there
/// is another element is only known once the deserializer has been asked for
/// it, and asking is what renders it.
struct RenderElement<'b> {
    out: &'b mut Vec<u8>,
    depth: usize,
    first: bool,
}

impl<'de> de::DeserializeSeed<'de> for RenderElement<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if !self.first {
            self.out.push(b',');
        }
        deserializer.deserialize_any(Render { out: self.out, depth: self.depth })
    }
}

/// One key of an object, with the comma that precedes it. A JSON key is always
/// a string, so anything else is a type error rather than a rendered value.
struct RenderKey<'b> {
    out: &'b mut Vec<u8>,
    first: bool,
}

impl<'de> de::DeserializeSeed<'de> for RenderKey<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if !self.first {
            self.out.push(b',');
        }
        deserializer.deserialize_str(self)
    }
}

impl<'de> de::Visitor<'de> for RenderKey<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object key")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        write_json_string(self.out, value);
        Ok(())
    }
}

/// An owned piece of JSON text that travels through the SDK unchanged.
///
/// It holds whatever value it was built from - an object, an array or a
/// scalar - as the text the wire carried, so a response field of a shape this
/// version does not know survives a decode and can be read later with
/// [`decode`](RawJson::decode). Two values are equal when their text
/// is equal, which means equality is textual: `{"a":1}` and `{ "a": 1 }` are
/// different values.
///
/// # Invariant
///
/// The text is always exactly one complete JSON value whose structure is
/// valid. Nothing builds a `RawJson` without the codec having established
/// that, because the text is written into a request body unread: a second
/// value in it would become a field of the enclosing object. Escapes inside
/// strings are passed through unchecked: a `\u` escape with bad hex digits,
/// or a lone surrogate half, is kept as it was written, and a strict server
/// refuses the request that carries it. String boundaries are found the same
/// way either way, so such an escape cannot end a string early.
///
/// # Serialization
///
/// Inside the SDK the text is spliced into the request body byte for byte:
/// key order, spacing and the exact spelling of every number are what the
/// caller or the server wrote. Through any other serializer - `serde_json`,
/// for instance - the value is written out as ordinary JSON data instead, so
/// the data survives while its spelling may not: numbers are re-rendered by
/// that serializer and insignificant whitespace is dropped.
///
/// Which of the two happens is decided by whether the SDK's own encoder is
/// running on this thread, not by the type of the serializer, because serde
/// offers no way to ask a serializer what it is. A `RawJson` handed to a
/// foreign serializer from inside a caller's own `Serialize` implementation
/// while the SDK is encoding a request is therefore spliced rather than
/// transcoded; use [`as_str`](RawJson::as_str) there.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RawJson {
    text: Box<str>,
}

impl RawJson {
    /// Encodes `value` and keeps the resulting JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when the value cannot be represented as JSON.
    pub fn from_value<T>(value: &T) -> Result<Self, EncodeError>
    where
        T: Serialize + ?Sized,
    {
        let mut buffer = Vec::new();
        encode_into(&mut buffer, value)?;
        Ok(Self::from_text(String::from_utf8(buffer).expect("invariant: the codec emits UTF-8")))
    }

    /// The JSON text, exactly as it was received or encoded.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Decodes the held text into `T`.
    ///
    /// It is `decode` rather than `deserialize` so that it does not shadow
    /// [`Deserialize::deserialize`], which a caller reaches for when they read
    /// a `RawJson` out of a document with a codec of their own.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when the text does not have the shape `T`
    /// expects, or when it is nested deeper than the decoder allows - which a
    /// value built by [`from_value`](RawJson::from_value) can be, since
    /// encoding is not depth limited.
    pub fn decode<'de, T>(&'de self) -> Result<T, DecodeError>
    where
        T: Deserialize<'de>,
    {
        decode(self.text.as_bytes())
    }

    /// Wraps text the codec has already established to be one JSON value:
    /// what [`encode_into`] wrote, or what [`deserialize_raw`] captured and
    /// checked.
    pub(crate) fn from_text(text: String) -> Self {
        Self { text: text.into_boxed_str() }
    }
}

impl fmt::Debug for RawJson {
    /// Prints the JSON text itself; the default derive would wrap it in a
    /// struct with one field and quote it a second time.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl fmt::Display for RawJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl Serialize for RawJson {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_raw(&self.text, serializer)
    }
}

impl<'de> Deserialize<'de> for RawJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_raw(deserializer).map(|raw| Self::from_text(raw.into_owned()))
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
