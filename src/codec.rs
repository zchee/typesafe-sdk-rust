//! JSON encoding and decoding for the whole crate.
//!
//! This is the only module that names `sonic_rs`. No type of that crate
//! appears in any signature outside it, so replacing the codec is a change to
//! this file alone.
//!
//! Three things here are not what a plain `serde_json` wrapper would do:
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

// `#[expect]` cannot be used for these: with the `internals` feature the
// wrappers in `crate::__internals` do reach every item below, so an
// expectation would be unfulfilled - and therefore warn - in exactly the
// configuration where the items are used.
#![cfg_attr(not(feature = "internals"), allow(dead_code))]

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    fmt,
};

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

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
    /// The document is nested deeper than [`MAX_JSON_DEPTH`] and was rejected
    /// without being parsed.
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
}

impl DecodeError {
    /// Which of the three failure classes this is.
    #[must_use]
    pub fn kind(&self) -> DecodeErrorKind {
        match self.detail {
            Detail::TooDeep => DecodeErrorKind::TooDeep,
            Detail::Syntax { .. } => DecodeErrorKind::Syntax,
            Detail::Data { .. } => DecodeErrorKind::Data,
        }
    }

    /// The one-based line the parser stopped at, or 0 when the document was
    /// rejected before it was parsed.
    #[must_use]
    pub fn line(&self) -> usize {
        match self.detail {
            Detail::TooDeep => 0,
            Detail::Syntax { line, .. } | Detail::Data { line, .. } => line,
        }
    }

    /// The one-based column the parser stopped at, or 0 when the document was
    /// rejected before it was parsed.
    #[must_use]
    pub fn column(&self) -> usize {
        match self.detail {
            Detail::TooDeep => 0,
            Detail::Syntax { column, .. } | Detail::Data { column, .. } => column,
        }
    }

    /// The path of the offending field, with dotted names and bracketed
    /// indices, for example `answers.tone.confidence` or `models[1].name`.
    ///
    /// The path of the document root is `.`, and it is empty when the failure
    /// carries no path at all.
    #[must_use]
    pub fn path(&self) -> &str {
        match &self.detail {
            Detail::TooDeep | Detail::Syntax { .. } => "",
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
/// step (a non-finite float, or an error returned by the value's own
/// [`Serialize`] implementation) and never quotes the value.
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

/// Appends the JSON form of `value` to `buf`.
///
/// The buffer is not cleared, which is what lets a request body be spliced out
/// of literal fragments and encoded values in one pass.
///
/// # Errors
///
/// Returns [`EncodeError`] when the value cannot be represented as JSON, for
/// example a non-finite float. The buffer may then hold a partial encoding of
/// that value, so a caller that reuses it has to truncate it.
pub(crate) fn encode_into<T>(buf: &mut Vec<u8>, value: &T) -> Result<(), EncodeError>
where
    T: Serialize + ?Sized,
{
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
/// returned buffer. That buffer has `len == capacity`, which makes
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
    // The shrink goes all the way down to the hint rather than to the ceiling:
    // stopping at the ceiling leaves the capacity exactly at a bound that the
    // still-decaying hint lowers again, so every following call re-allocates
    // the whole buffer.
    if scratch.capacity() > decayed.saturating_mul(8) {
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

/// Decodes `bytes` into `T`.
///
/// The successful path is a single pass and pays nothing for error reporting.
/// A failure is decoded a second time through a path-tracking wrapper, which
/// is what turns a bare position into `answers.tone.confidence`.
///
/// # Errors
///
/// Returns [`DecodeError`] when the input is nested too deeply, is not valid
/// JSON, or does not have the shape `T` expects.
pub(crate) fn decode<'de, T>(bytes: &'de [u8]) -> Result<T, DecodeError>
where
    T: Deserialize<'de>,
{
    check_depth(bytes)?;
    sonic_rs::from_slice::<T>(bytes).map_err(|_| describe_failure::<T>(bytes))
}

/// Re-runs a failed decode with path tracking and turns the result into a
/// [`DecodeError`] that carries no part of the input.
fn describe_failure<'de, T>(bytes: &'de [u8]) -> DecodeError
where
    T: Deserialize<'de>,
{
    let mut deserializer = sonic_rs::Deserializer::from_slice(bytes);
    let Err(tracked) = serde_path_to_error::deserialize::<_, T>(&mut deserializer) else {
        // Both passes read the same bytes with the same type, so this is
        // unreachable in practice; reporting a shapeless data error is still
        // better than claiming success the caller cannot use.
        return DecodeError { detail: Detail::Data { path: Box::from(""), line: 0, column: 0 } };
    };

    let inner = tracked.inner();
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
    let mut path = tracked.path().to_string();
    if let Some(field) = missing_field_name(&inner.to_string()) {
        if path == "." {
            path = field.to_owned();
        } else {
            path.push('.');
            path.push_str(field);
        }
    }

    DecodeError { detail: Detail::Data { path: path.into_boxed_str(), line, column } }
}

/// Extracts `noul` from ``missing field `noul` at line 1 column 101``.
fn missing_field_name(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("missing field `")?;
    let end = rest.find('`')?;
    Some(&rest[..end])
}

// ------------------------------------------------------------- raw JSON

/// Reads the next value as its raw JSON text, without interpreting it.
///
/// The text is borrowed from the input whenever the codec can do so, and owned
/// when it cannot - which is the case for a string that contains escape
/// sequences.
///
/// # Errors
///
/// Returns the deserializer's own error. Note that the raw capture relies on a
/// protocol private to this codec: another deserializer, `serde_json` for
/// instance, reports an unexpected newtype struct instead.
pub(crate) fn deserialize_raw<'de, D>(deserializer: D) -> Result<Cow<'de, str>, D::Error>
where
    D: Deserializer<'de>,
{
    sonic_rs::LazyValue::deserialize(deserializer).map(|value| value.as_raw_cow())
}

/// Writes raw JSON text through `serializer` verbatim.
///
/// # Errors
///
/// Returns the serializer's own error.
fn serialize_raw<S>(text: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    // The raw value has to be handed to the codec as its own lazy type for the
    // text to be spliced in rather than re-encoded, and that type can only be
    // built by parsing. The parse is a skip over the text: it allocates
    // nothing and validates what the constructors already guaranteed.
    let lazy = sonic_rs::from_str::<sonic_rs::LazyValue<'_>>(text)
        .map_err(|error| serde::ser::Error::custom(EncodeError::from_codec(error)))?;
    lazy.serialize(serializer)
}

/// An owned piece of JSON text that travels through the SDK unchanged.
///
/// It holds whatever value it was built from - an object, an array or a
/// scalar - as the text the wire carried, so a response field of a shape this
/// version does not know survives a decode and can be read later with
/// [`deserialize`](RawJson::deserialize). Two values are equal when their text
/// is equal, which means equality is textual: `{"a":1}` and `{ "a": 1 }` are
/// different values.
///
/// # Serialization
///
/// The text is spliced into the output verbatim when this crate's codec is the
/// serializer, which is the case for everything the SDK sends. Handing a
/// `RawJson` to another serializer, `serde_json` for instance, yields a
/// single-field object instead, because the verbatim splice is a protocol
/// private to the codec; use [`as_str`](RawJson::as_str) in that case.
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
    /// # Errors
    ///
    /// Returns [`DecodeError`] when the text does not have the shape `T`
    /// expects, or when it is nested deeper than the decoder allows - which a
    /// value built by [`from_value`](RawJson::from_value) can be, since
    /// encoding is not depth limited.
    pub fn deserialize<'de, T>(&'de self) -> Result<T, DecodeError>
    where
        T: Deserialize<'de>,
    {
        decode(self.text.as_bytes())
    }

    /// Wraps text the caller has already had validated by the codec.
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
