//! Text this SDK did not write, made safe to print.
//!
//! A key the server chose ends up in a field path, a transport's error ends
//! up in a connection error's message, and either one ends up in a log line.
//! Such text keeps its printable characters, non-ASCII included, but a control
//! character, or a format character that reorders or hides the text around
//! it, is written as a Rust escape (`\n`, `\r`, `\t`, or `\u{1b}` for the
//! others), so that it cannot break a log line, recolour a terminal or
//! disguise itself. The whole text is capped at a number of characters,
//! counted after escaping; a cut never splits a character or an escape, and
//! is marked with U+2026.

use std::fmt::{self, Write as _};

/// What a backslash in the text becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backslash {
    /// Written as `\\`, so that a text spelling `\u{1b}` cannot be mistaken
    /// for one holding the character. Two characters, counted by the caps.
    Double,
    /// Written as it is, for text that another layer has already escaped
    /// once and would only be made harder to read by a second pass.
    Keep,
}

/// A capped rendering being built from fixed text and text this SDK did not
/// write.
pub(crate) struct SafeText {
    text: String,
    /// Characters written so far that count against `limit`.
    chars: usize,
    limit: usize,
    backslash: Backslash,
    /// Set once `limit` is reached; nothing is written after it.
    full: bool,
}

impl SafeText {
    /// An empty rendering capped at `limit` characters.
    pub(crate) fn new(limit: usize, backslash: Backslash) -> Self {
        Self::after(String::new(), limit, backslash)
    }

    /// A rendering that starts with `prefix`, which does not count against
    /// `limit`: the fixed start of a message, say, that is the SDK's own.
    pub(crate) fn after(prefix: String, limit: usize, backslash: Backslash) -> Self {
        Self { text: prefix, chars: 0, limit, backslash, full: false }
    }

    /// Text that comes from the SDK itself, written as it is but counted.
    pub(crate) fn fixed(&mut self, text: &str) {
        self.put(&text, text.chars().count());
    }

    /// Text the SDK did not write, escaped, and cut at `cap` characters of its
    /// own as well as at the whole rendering's limit.
    pub(crate) fn untrusted(&mut self, text: &str, cap: usize) {
        let mut written = 0usize;
        for character in text.chars() {
            let Some(len) = self.character(character, &mut written, cap) else {
                return;
            };
            written += len;
        }
    }

    /// The rendering.
    pub(crate) fn into_string(self) -> String {
        self.text
    }

    /// Writes one character of untrusted text, escaped, unless it would take
    /// its piece past `cap`: then the ellipsis is written instead and `None`
    /// returned. Returns the characters written, or `None` when writing
    /// should stop.
    fn character(&mut self, character: char, written: &mut usize, cap: usize) -> Option<usize> {
        // Rust lets a binding be declared here and assigned in only one arm,
        // so each escape lives long enough to be borrowed below.
        let short;
        let code;
        let (shown, len): (&dyn fmt::Display, usize) = match character {
            '\\' if self.backslash == Backslash::Keep => (&character, 1),
            // A backslash is the one printable character escaped: written
            // bare, a key spelling `\u{1b}` would read exactly like a key
            // holding a real ESC.
            '\\' | '\n' | '\r' | '\t' => {
                short = character.escape_default();
                (&short, short.len())
            }
            _ if character.is_control() || hides_text(character) => {
                code = character.escape_unicode();
                (&code, code.len())
            }
            _ => (&character, 1),
        };
        if written.saturating_add(len) > cap {
            self.put(&'\u{2026}', 1);
            return None;
        }
        self.put(shown, len).then_some(len)
    }

    /// Appends `piece`, which renders as `len` characters, whole - or, when
    /// it would cross the limit, the ellipsis instead, after which nothing
    /// more is written. Returns whether `piece` was written.
    fn put(&mut self, piece: &dyn fmt::Display, len: usize) -> bool {
        if self.full {
            return false;
        }
        if self.chars + len > self.limit {
            self.text.push('\u{2026}');
            self.full = true;
            return false;
        }
        write!(self.text, "{piece}").expect("invariant: writing to a String cannot fail");
        self.chars += len;
        true
    }
}

/// Whether `character` is a Unicode format character that reorders, joins or
/// hides the text around it: the bidirectional embeddings, overrides and
/// isolates, the zero-width characters, the byte-order mark, the line and
/// paragraph separators, the interlinear annotation marks and the invisible
/// tag characters. `char::is_control` covers none of them.
fn hides_text(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
