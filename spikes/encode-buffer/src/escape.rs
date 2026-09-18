//! A scalar JSON string escaper that can size its output exactly.
//!
//! The point of the two-pass variant is a buffer that is allocated once at the
//! final length, so the escaper must be able to answer "how long will this be"
//! before it writes anything. `json-escape-simd` cannot: its kernels do
//! speculative full-register stores and force the caller's buffer to hold
//! `len * 6 + 35` spare bytes, which is the very reservation the variant exists
//! to avoid.

/// Length of `value` written as a JSON string, including the two quotes.
#[must_use]
pub fn escaped_len(value: &str) -> usize {
    let mut len = 2;
    for byte in value.as_bytes() {
        len += match byte {
            b'"' | b'\\' | 0x08 | 0x0c | b'\n' | b'\r' | b'\t' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
    }
    len
}

/// Appends `value` to `out` as a JSON string, including the two quotes.
///
/// Iterating over bytes rather than chars is correct because every byte of a
/// multi-byte UTF-8 sequence is >= 0x80, so none of them can collide with an
/// ASCII character that needs escaping; they are copied through untouched.
pub fn write_escaped(value: &str, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    out.push(b'"');
    let bytes = value.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x0c => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1f => {
                out.extend_from_slice(&bytes[start..index]);
                out.extend_from_slice(b"\\u00");
                out.push(HEX[usize::from(byte >> 4)]);
                out.push(HEX[usize::from(byte & 0x0f)]);
                start = index + 1;
                continue;
            }
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..index]);
        out.extend_from_slice(escape);
        start = index + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}
