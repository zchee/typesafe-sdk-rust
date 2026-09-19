//! The questions of a set as the JSON the SDK sends, produced while the
//! caller's crate compiles.
//!
//! The bytes must equal what the runtime `Questions::prepare` produces for the
//! same questions, so the layout here copies it exactly: members in the order
//! `type`, `instructions`, `criteria`; a noul's criteria in the order `true`,
//! `false`, and left out when neither is set; an option without a description
//! as `null`; nothing between tokens. The runtime writes strings with the
//! SDK's codec, and [`push_string`] escapes exactly as that codec does.

use crate::parse::{Question, QuestionField};

/// A question set as the runtime's `PreparedQuestions` holds it: the JSON
/// object, then every name unescaped and back to back, and where each name
/// ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    pub(crate) buf: String,
    pub(crate) json_len: usize,
    pub(crate) name_ends: Vec<usize>,
}

/// Serializes the fields' questions in declaration order.
pub(crate) fn prepare(fields: &[QuestionField]) -> Prepared {
    let mut buf = String::from("{");
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            buf.push(',');
        }
        push_string(&mut buf, &field.name);
        buf.push(':');
        push_question(&mut buf, &field.question);
    }
    buf.push('}');
    let json_len = buf.len();
    let name_ends = fields
        .iter()
        .map(|field| {
            buf.push_str(&field.name);
            buf.len()
        })
        .collect();
    Prepared { buf, json_len, name_ends }
}

fn push_question(buf: &mut String, question: &Question) {
    match question {
        Question::Noul { instructions, yes, no } => {
            buf.push_str(r#"{"type":"noul""#);
            push_instructions(buf, instructions.as_deref());
            if yes.is_some() || no.is_some() {
                buf.push_str(r#","criteria":{"#);
                if let Some(yes) = yes {
                    buf.push_str(r#""true":"#);
                    push_string(buf, yes);
                }
                if let Some(no) = no {
                    if yes.is_some() {
                        buf.push(',');
                    }
                    buf.push_str(r#""false":"#);
                    push_string(buf, no);
                }
                buf.push('}');
            }
            buf.push('}');
        }
        Question::Choice { instructions, options } => {
            buf.push_str(r#"{"type":"choice""#);
            push_instructions(buf, instructions.as_deref());
            buf.push_str(r#","criteria":{"#);
            for (index, (name, description)) in options.iter().enumerate() {
                if index > 0 {
                    buf.push(',');
                }
                push_string(buf, name);
                buf.push(':');
                match description {
                    Some(description) => push_string(buf, description),
                    None => buf.push_str("null"),
                }
            }
            buf.push_str("}}");
        }
        Question::Score { instructions, levels } => {
            buf.push_str(r#"{"type":"score""#);
            push_instructions(buf, instructions.as_deref());
            buf.push_str(r#","criteria":["#);
            for (index, level) in levels.iter().enumerate() {
                if index > 0 {
                    buf.push(',');
                }
                push_string(buf, level);
            }
            buf.push_str("]}");
        }
    }
}

fn push_instructions(buf: &mut String, instructions: Option<&str>) {
    if let Some(instructions) = instructions {
        buf.push_str(r#","instructions":"#);
        push_string(buf, instructions);
    }
}

/// Appends `text` as a JSON string literal, escaped as the SDK's codec
/// (sonic-rs) escapes it: `"` and `\` with a backslash; backspace, tab, line
/// feed, form feed and carriage return by their short escapes; every other
/// character below U+0020 as `\u00xx` in lowercase hexadecimal; everything
/// else, U+007F and every non-ASCII character included, as it is.
pub(crate) fn push_string(out: &mut String, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            control @ '\u{00}'..='\u{1F}' => {
                // A `char` below U+0020 is one byte, so its two hex digits are
                // its high and low nibble.
                let byte = control as usize;
                out.push_str("\\u00");
                out.push(char::from(HEX[byte >> 4]));
                out.push(char::from(HEX[byte & 0xF]));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
