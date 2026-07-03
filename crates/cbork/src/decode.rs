// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Decode raw CBOR into a rendered EDN/CDN-style dump.

use std::{
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::Path,
};

use cbork_edn::{Document, Float, MapEntry, Value};
use console::style;

/// Decode raw CBOR input and print a rendered dump.
pub(crate) fn exec(
    path: Option<&Path>,
    no_color: bool,
    pretty: bool,
) -> bool {
    let header = path.map_or_else(|| "<stdin>".to_owned(), |path| path.display().to_string());
    let input = match read_input(path) {
        Ok(input) => input,
        Err(error) => {
            println!("{}", style(format!("decode error: {error}")).red());
            return false;
        },
    };

    let document = match Document::parse(&input) {
        Ok(document) => document,
        Err(error) => {
            println!("{}", style(format!("decode error: {error}")).red());
            return false;
        },
    };

    println!("{}", render_dump(&header, &document, pretty, !no_color));

    true
}

/// Read CBOR input from a file or from standard input.
pub(crate) fn read_input(path: Option<&Path>) -> io::Result<Vec<u8>> {
    if let Some(path) = path {
        fs::read(path)
    } else {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// Render a full decode dump including a source header.
pub(crate) fn render_dump(
    header: &str,
    document: &Document,
    pretty: bool,
    color: bool,
) -> String {
    let mut output = String::new();
    if color {
        push_colored(&mut output, header, ColorKind::Header, true);
        push_dim(&mut output, " ->\n", true);
    } else {
        output.push_str(header);
        output.push_str(" ->\n");
    }
    output.push_str(&render_document(document, pretty, color));
    output
}

/// Render a parsed document into a colored or plain string.
pub(crate) fn render_document(
    document: &Document,
    pretty: bool,
    color: bool,
) -> String {
    let mut output = String::new();
    for (index, item) in document.items().iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        render_value(item, &mut output, pretty, color, 0);
    }
    output
}

/// Render a single CBOR value recursively.
pub(crate) fn render_value(
    value: &Value,
    output: &mut String,
    pretty: bool,
    color: bool,
    indent: usize,
) {
    match value {
        Value::Integer(value) => push_colored(output, value.to_string(), ColorKind::Number, color),
        Value::Float(value) => {
            match value {
                Float::F16(value) | Float::F32(value) => {
                    push_colored(output, value.to_string(), ColorKind::Float, color);
                },
                Float::F64(value) => {
                    push_colored(output, value.to_string(), ColorKind::Float, color);
                },
            }
        },
        Value::Bool(value) => push_colored(output, value.to_string(), ColorKind::Keyword, color),
        Value::Null => push_colored(output, "null", ColorKind::Keyword, color),
        Value::Undefined => push_colored(output, "undefined", ColorKind::Keyword, color),
        Value::Simple(value) => {
            push_colored(output, format!("simple({value})"), ColorKind::Simple, color);
        },
        Value::Bytes(value) => push_colored(output, render_bytes(value), ColorKind::Bytes, color),
        Value::Text(value) => push_colored(output, format!("{value:?}"), ColorKind::Text, color),
        Value::Array(values) => {
            let depth = indent / 2;
            if pretty {
                let child_indent = indent.saturating_add(2);
                let last_index = values.len().saturating_sub(1);
                push_bracket(output, "[", color, depth);
                if values.is_empty() {
                    push_bracket(output, "]", color, depth);
                    return;
                }

                push_dim(output, "\n", color);
                for (index, item) in values.iter().enumerate() {
                    push_indent(output, child_indent);
                    render_value(item, output, pretty, color, child_indent);
                    if index != last_index {
                        push_dim(output, ",", color);
                    }
                    push_dim(output, "\n", color);
                }
                push_indent(output, indent);
                push_bracket(output, "]", color, depth);
            } else {
                push_bracket(output, "[", color, depth);
                for (index, item) in values.iter().enumerate() {
                    if index > 0 {
                        push_dim(output, ", ", color);
                    }
                    render_value(item, output, pretty, color, indent);
                }
                push_bracket(output, "]", color, depth);
            }
        },
        Value::Map(entries) => {
            let depth = indent / 2;
            if pretty {
                let child_indent = indent.saturating_add(2);
                let last_index = entries.len().saturating_sub(1);
                push_bracket(output, "{", color, depth);
                if entries.is_empty() {
                    push_bracket(output, "}", color, depth);
                    return;
                }

                push_dim(output, "\n", color);
                for (index, entry) in entries.iter().enumerate() {
                    push_indent(output, child_indent);
                    render_map_entry(entry, output, pretty, color, child_indent);
                    if index != last_index {
                        push_dim(output, ",", color);
                    }
                    push_dim(output, "\n", color);
                }
                push_indent(output, indent);
                push_bracket(output, "}", color, depth);
            } else {
                push_bracket(output, "{", color, depth);
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        push_dim(output, ", ", color);
                    }
                    render_map_entry(entry, output, pretty, color, indent);
                }
                push_bracket(output, "}", color, depth);
            }
        },
        Value::Tag(tag, value) => {
            let depth = indent / 2;
            push_colored(output, tag.to_string(), ColorKind::Tag, color);
            push_bracket(output, "(", color, depth);
            render_value(value, output, pretty, color, indent);
            push_bracket(output, ")", color, depth);
        },
    }
}

/// Render a single map entry.
pub(crate) fn render_map_entry(
    entry: &MapEntry,
    output: &mut String,
    pretty: bool,
    color: bool,
    indent: usize,
) {
    render_value(&entry.key, output, pretty, color, indent);
    push_dim(output, ": ", color);
    render_value(&entry.value, output, pretty, color, indent);
}

/// Render a byte string as CDN-style `h''` output.
pub(crate) fn render_bytes(bytes: &[u8]) -> String {
    let mut rendered = String::from("h'");
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            rendered.push(' ');
        }
        let _ = write!(&mut rendered, "{byte:02x}");
    }
    rendered.push('\'');
    rendered
}

/// Append dimmed punctuation or separators.
pub(crate) fn push_dim(
    output: &mut String,
    text: &str,
    color: bool,
) {
    if color {
        let _ = write!(output, "{}", style(text).dim());
    } else {
        output.push_str(text);
    }
}

/// Append a nesting-aware structural bracket.
pub(crate) fn push_bracket(
    output: &mut String,
    text: &str,
    color: bool,
    depth: usize,
) {
    if !color {
        output.push_str(text);
        return;
    }

    let styled = match depth % 6 {
        0 => style(text).green(),
        1 => style(text).magenta(),
        2 => style(text).blue(),
        3 => style(text).cyan(),
        4 => style(text).yellow(),
        _ => style(text).red(),
    };
    let _ = write!(output, "{styled}");
}

/// Append indentation spaces.
pub(crate) fn push_indent(
    output: &mut String,
    width: usize,
) {
    for _ in 0..width {
        output.push(' ');
    }
}

/// Append a colored scalar or token.
pub(crate) fn push_colored<T: std::fmt::Display>(
    output: &mut String,
    text: T,
    kind: ColorKind,
    color: bool,
) {
    if color {
        let styled = match kind {
            ColorKind::Number => style(text).blue(),
            ColorKind::Float | ColorKind::Simple => style(text).magenta(),
            ColorKind::Keyword => style(text).yellow(),
            ColorKind::Text => style(text).green(),
            ColorKind::Bytes => style(text).cyan(),
            ColorKind::Tag => style(text).cyan().bold(),
            ColorKind::Header => style(text).bold(),
        };
        let _ = write!(output, "{styled}");
    } else {
        let _ = write!(output, "{text}");
    }
}

/// Token categories for colorized rendering.
#[derive(Clone, Copy)]
pub(crate) enum ColorKind {
    /// Numeric literals and floats.
    Number,
    /// Floating-point literals.
    Float,
    /// Keyword-like literals such as `true`, `false`, `null`, and `undefined`.
    Keyword,
    /// Text string literals.
    Text,
    /// Byte string literals.
    Bytes,
    /// `simple(...)` literal values.
    Simple,
    /// CBOR tags.
    Tag,
    /// Source label for decode output.
    Header,
}

#[cfg(test)]
mod tests {
    use cbork_edn::parse;
    use console::set_colors_enabled;

    use super::{render_document, render_dump};

    #[test]
    fn plain_and_colored_rendering_diverge() {
        set_colors_enabled(true);
        let document = parse(&[0x01, 0x61, 0x61, 0x42, 0x01, 0x02]).expect("parse");
        let plain = render_document(&document, false, false);
        let colored = render_document(&document, false, true);

        assert_eq!(plain, "1\n\"a\"\nh'01 02'");
        assert!(colored.contains("\u{1b}["));
        assert_ne!(plain, colored);
    }

    #[test]
    fn pretty_rendering_breaks_nested_structures() {
        let document = parse(&[
            0x82, 0x02, 0xA2, 0x01, 0x58, 0x20, 0x43, 0x13, 0x4D, 0x68, 0x8B, 0xB8, 0xB0, 0x7D,
            0xFC, 0x2D, 0x9B, 0xC9, 0xC6, 0x73, 0x85, 0x7A, 0xE6, 0x11, 0xCD, 0xE1, 0x6E, 0x29,
            0xAF, 0x2B, 0xA5, 0xD0, 0xE4, 0xB9, 0xB8, 0xF4, 0x5B, 0x83, 0x02, 0x58, 0x20, 0x67,
            0x12, 0x4A, 0xDD, 0x8E, 0xC1, 0xFD, 0x40, 0xDF, 0xFB, 0xEB, 0xF5, 0x16, 0x04, 0x1E,
            0x71, 0xE6, 0x3D, 0x0A, 0x61, 0xFD, 0xF1, 0xC1, 0xC4, 0xF1, 0x63, 0x3C, 0xEE, 0xB6,
            0xB3, 0xE8, 0x77,
        ])
        .expect("parse");

        let pretty = render_document(&document, true, false);

        assert_eq!(
            pretty,
            "[\n  2,\n  {\n    1: h'43 13 4d 68 8b b8 b0 7d fc 2d 9b c9 c6 73 85 7a e6 11 cd e1 6e 29 af 2b a5 d0 e4 b9 b8 f4 5b 83',\n    2: h'67 12 4a dd 8e c1 fd 40 df fb eb f5 16 04 1e 71 e6 3d 0a 61 fd f1 c1 c4 f1 63 3c ee b6 b3 e8 77'\n  }\n]"
        );
    }

    #[test]
    fn dump_prefixes_source_label() {
        let document = parse(&[0x01]).expect("parse");
        let dump = render_dump("input.cbor", &document, false, false);

        assert_eq!(dump, "input.cbor ->\n1");
    }
}
