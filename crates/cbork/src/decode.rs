// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Decode raw CBOR into a rendered EDN/CDN-style dump.

use std::{
    cell::Cell,
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
    try_cbor_bstr: bool,
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

    let limits = EmbedLimits::default();
    println!(
        "{}",
        render_dump(&header, &document, pretty, !no_color, try_cbor_bstr, limits),
    );

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
    try_cbor_bstr: bool,
    limits: EmbedLimits,
) -> String {
    let mut output = String::new();
    if color {
        push_colored(&mut output, header, ColorKind::Header, true);
        push_dim(&mut output, " ->\n", true);
    } else {
        output.push_str(header);
        output.push_str(" ->\n");
    }
    output.push_str(&render_document(
        document,
        pretty,
        color,
        try_cbor_bstr,
        limits,
    ));
    output
}

/// Render a parsed document into a colored or plain string.
pub(crate) fn render_document(
    document: &Document,
    pretty: bool,
    color: bool,
    try_cbor_bstr: bool,
    limits: EmbedLimits,
) -> String {
    let mut output = String::new();
    reset_render_counters();
    for (index, item) in document.items().iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        render_value(item, &mut output, pretty, color, 0, try_cbor_bstr, limits);
    }
    output
}

/// Render a single CBOR value recursively.
#[allow(clippy::too_many_lines)]
pub(crate) fn render_value(
    value: &Value,
    output: &mut String,
    pretty: bool,
    color: bool,
    indent: usize,
    try_cbor_bstr: bool,
    limits: EmbedLimits,
) {
    if try_cbor_bstr && let Value::Bytes(bytes) = value {
        // Empty byte strings are extremely common in real payloads (e.g.
        // empty COSE unprotected headers). Even with `--try-cbor-bstr`,
        // they must stay as `h''` rather than render as `<<>>`, because
        // they are usually ordinary data and the empty `<<>>` literal
        // is reserved for explicit `.cborseq`/`.prefpseq`/`.dtrmseq`
        // contexts in schema-aware dumps.
        if bytes.is_empty() {
            push_colored(output, render_bytes(bytes), ColorKind::Bytes, color);
            return;
        }
        if let Some(inner) = try_parse_cbor_bytes(bytes) {
            match try_charge_embed(bytes.len(), limits) {
                EmbedBudget::Expanded => {
                    render_embedded_bstr_value(&inner, output, pretty, color, indent, limits);
                    release_embed_depth();
                    return;
                },
                EmbedBudget::LimitReached => {
                    // Fall back to the EDN-literals draft's `h'...'` form.
                    push_colored(output, render_bytes(bytes), ColorKind::Bytes, color);
                    return;
                },
            }
        }
        push_colored(output, render_bytes(bytes), ColorKind::Bytes, color);
        return;
    }

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
                    render_value(
                        item,
                        output,
                        pretty,
                        color,
                        child_indent,
                        try_cbor_bstr,
                        limits,
                    );
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
                    render_value(item, output, pretty, color, indent, try_cbor_bstr, limits);
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
                    render_map_entry(
                        entry,
                        output,
                        pretty,
                        color,
                        child_indent,
                        try_cbor_bstr,
                        limits,
                    );
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
                    render_map_entry(entry, output, pretty, color, indent, try_cbor_bstr, limits);
                }
                push_bracket(output, "}", color, depth);
            }
        },
        Value::Tag(tag, value) => {
            let depth = indent / 2;
            push_colored(output, tag.to_string(), ColorKind::Tag, color);
            push_bracket(output, "(", color, depth);
            render_value(value, output, pretty, color, indent, try_cbor_bstr, limits);
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
    try_cbor_bstr: bool,
    limits: EmbedLimits,
) {
    render_value(
        &entry.key,
        output,
        pretty,
        color,
        indent,
        try_cbor_bstr,
        limits,
    );
    push_dim(output, ": ", color);
    render_value(
        &entry.value,
        output,
        pretty,
        color,
        indent,
        try_cbor_bstr,
        limits,
    );
}

/// Attempt to parse a byte string as one or more CBOR items, returning
/// the parsed `Document` only when the bytes form a complete sequence.
fn try_parse_cbor_bytes(bytes: &[u8]) -> Option<Document> {
    Document::parse(bytes).ok()
}

/// Render an embedded-CBOR byte string inside an `<<...>>` wrapper using
/// the generic (schema-free) renderer. Used by `cbork decode --try-cbor-bstr`.
fn render_embedded_bstr_value(
    document: &Document,
    output: &mut String,
    pretty: bool,
    color: bool,
    indent: usize,
    limits: EmbedLimits,
) {
    let depth = indent / 2;
    push_bracket(output, "<<", color, depth);
    let items = document.items();
    if items.is_empty() {
        push_bracket(output, ">>", color, depth);
        return;
    }
    let inner_indent = indent.saturating_add(2);
    push_dim(output, "\n", color);
    let last = items.len().saturating_sub(1);
    reset_sequence_counter();
    for (index, item) in items.iter().enumerate() {
        if !try_charge_sequence_item(limits) {
            // Sequence too long; close out the wrapper with a trailing
            // diagnostic and fall back rather than truncating silently.
            push_indent(output, inner_indent);
            push_colored(
                output,
                format!(
                    "... ({} more item(s) truncated)",
                    items.len().saturating_sub(index)
                ),
                ColorKind::Simple,
                color,
            );
            push_dim(output, "\n", color);
            push_indent(output, indent);
            push_bracket(output, ">>", color, depth);
            return;
        }
        push_indent(output, inner_indent);
        render_value(item, output, pretty, color, inner_indent, true, limits);
        if index != last {
            push_dim(output, ",", color);
        }
        push_dim(output, "\n", color);
    }
    push_indent(output, indent);
    push_bracket(output, ">>", color, depth);
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

/// Resource limits for embedded-CBOR rendering.
///
/// The schema-aware validation renderer and the schema-free `--try-cbor-bstr`
/// renderer share the same limits. They are intentionally generous so that
/// ordinary multi-level COSE payloads, the three-level regression fixture,
/// and large header arrays are not artificially truncated; they exist to
/// prevent malicious input from exhausting the stack or memory. Exceeding a
/// limit falls back to the EDN-literals draft's `h'...'` byte-string
/// diagnostic with no further descent into the embedded bytes, so the
/// outer dump remains informative and the parser never crashes.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub(crate) struct EmbedLimits {
    /// Maximum number of nested `<<...>>` wrappers allowed before the
    /// renderer falls back to raw `h'...'` for the offending payload.
    pub depth: usize,
    /// Maximum total bytes the renderer will decode from embedded CBOR
    /// byte strings in a single render pass.
    pub embedded_bytes: usize,
    /// Maximum number of items the renderer will render for an embedded
    /// CBOR sequence (`<<...>>`).
    pub sequence_items: usize,
}

impl Default for EmbedLimits {
    fn default() -> Self {
        // Generous defaults: 32 levels of nesting handles the three-level
        // regression plus normal COSE nesting; 16 MiB covers very large
        // header maps; 4096 items covers the most aggressive sequence
        // formats known today.
        Self {
            depth: 32,
            embedded_bytes: 16 * 1024 * 1024,
            sequence_items: 4096,
        }
    }
}

thread_local! {
    /// Mutable counters shared by the recursive embedded renderer.
    /// Initialized to zero by `reset_render_counters` before each render pass.
    static RENDER_DEPTH: Cell<usize> = const { Cell::new(0) };
    static RENDER_EMBEDDED_BYTES: Cell<usize> = const { Cell::new(0) };
    static RENDER_SEQUENCE_ITEMS: Cell<usize> = const { Cell::new(0) };
}

/// Reset the resource counters at the start of a render pass.
pub(crate) fn reset_render_counters() {
    RENDER_DEPTH.set(0);
    RENDER_EMBEDDED_BYTES.set(0);
    RENDER_SEQUENCE_ITEMS.set(0);
}

/// Outcome of an attempt to expand an embedded-CBOR byte string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbedBudget {
    /// The embedded bytes were rendered inside the `<<...>>` wrapper.
    Expanded,
    /// A resource limit was reached; fall back to raw `h'...'` bytes.
    LimitReached,
}

/// Check whether expanding an embedded payload of `bytes.len()` bytes at
/// the current depth would exceed the configured limits. Increments the
/// counters and returns `Expanded` on success, `LimitReached` otherwise.
pub(crate) fn try_charge_embed(
    bytes_len: usize,
    limits: EmbedLimits,
) -> EmbedBudget {
    let current_depth = RENDER_DEPTH.get();
    if current_depth >= limits.depth {
        return EmbedBudget::LimitReached;
    }
    let next_depth = current_depth.saturating_add(1);
    RENDER_DEPTH.set(next_depth);
    RENDER_EMBEDDED_BYTES.set(RENDER_EMBEDDED_BYTES.get().saturating_add(bytes_len));
    if RENDER_EMBEDDED_BYTES.get() > limits.embedded_bytes {
        return EmbedBudget::LimitReached;
    }
    EmbedBudget::Expanded
}

/// Charge a sequence item to the sequence-items counter. Returns true if
/// the sequence item is within the budget.
pub(crate) fn try_charge_sequence_item(limits: EmbedLimits) -> bool {
    let next = RENDER_SEQUENCE_ITEMS.get().saturating_add(1);
    RENDER_SEQUENCE_ITEMS.set(next);
    next <= limits.sequence_items
}

/// Decrement the depth counter after rendering an embedded payload.
pub(crate) fn release_embed_depth() {
    let current = RENDER_DEPTH.get();
    RENDER_DEPTH.set(current.saturating_sub(1));
}

/// Reset only the sequence-items counter so a new top-level embedded
/// sequence starts fresh without resetting the depth and bytes counters.
pub(crate) fn reset_sequence_counter() {
    RENDER_SEQUENCE_ITEMS.set(0);
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

    use super::{EmbedLimits, render_document, render_dump};

    #[test]
    fn plain_and_colored_rendering_diverge() {
        set_colors_enabled(true);
        let document = parse(&[0x01, 0x61, 0x61, 0x42, 0x01, 0x02]).expect("parse");
        let limits = EmbedLimits::default();
        let plain = render_document(&document, false, false, false, limits);
        let colored = render_document(&document, false, true, false, limits);

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

        let limits = EmbedLimits::default();
        let pretty = render_document(&document, true, false, false, limits);

        assert_eq!(
            pretty,
            "[\n  2,\n  {\n    1: h'43 13 4d 68 8b b8 b0 7d fc 2d 9b c9 c6 73 85 7a e6 11 cd e1 6e 29 af 2b a5 d0 e4 b9 b8 f4 5b 83',\n    2: h'67 12 4a dd 8e c1 fd 40 df fb eb f5 16 04 1e 71 e6 3d 0a 61 fd f1 c1 c4 f1 63 3c ee b6 b3 e8 77'\n  }\n]"
        );
    }

    #[test]
    fn dump_prefixes_source_label() {
        let document = parse(&[0x01]).expect("parse");
        let limits = EmbedLimits::default();
        let dump = render_dump("input.cbor", &document, false, false, false, limits);

        assert_eq!(dump, "input.cbor ->\n1");
    }

    #[test]
    fn try_cbor_bstr_decodes_inner_cbor_byte_string() {
        // Build a COSE-like map: `{"protected": h'a10126', "unprotected": {}}`
        // where the bytes `a1 01 26` are the CBOR encoding of `{1: -7}`.
        let document = parse(&[
            0xA2, 0x69, b'p', b'r', b'o', b't', b'e', b'c', b't', b'e', b'd', 0x43, 0xA1, 0x01,
            0x26, 0x6B, b'u', b'n', b'p', b'r', b'o', b't', b'e', b'c', b't', b'e', b'd', 0xA0,
        ])
        .expect("parse");

        let limits = EmbedLimits::default();
        let without_flag = render_document(&document, false, false, false, limits);
        let with_flag = render_document(&document, false, false, true, limits);

        // Without `--try-cbor-bstr` the byte string is preserved.
        assert!(without_flag.contains("h'a1 01 26'"), "{without_flag}");
        assert!(!without_flag.contains("<<"), "{without_flag}");

        // With `--try-cbor-bstr` the byte string is rendered as an
        // embedded-CBOR literal showing the decoded `{1: -7}`.
        assert!(with_flag.contains("<<"), "{with_flag}");
        assert!(with_flag.contains(">>"), "{with_flag}");
        assert!(with_flag.contains("1: -7"), "{with_flag}");
        assert!(!with_flag.contains("h'a1 01 26'"), "{with_flag}");
    }

    #[test]
    fn try_cbor_bstr_leaves_non_cbor_bytes_untouched() {
        // A byte string that does not parse as CBOR must stay raw.
        // `0x43 0xFF 0xFF 0xFF` is a 3-byte string whose bytes are not
        // a valid CBOR item.
        let document = parse(&[0x43, 0xFF, 0xFF, 0xFF]).expect("parse");
        let limits = EmbedLimits::default();
        let with_flag = render_document(&document, false, false, true, limits);
        assert!(with_flag.contains("h'ff ff ff'"), "{with_flag}");
        assert!(!with_flag.contains("<<"), "{with_flag}");
    }

    #[test]
    fn try_cbor_bstr_keeps_empty_byte_string_as_raw() {
        // `0x40` is an empty byte string. Even with `--try-cbor-bstr`,
        // an empty bstr must stay raw because empty embedded sequences
        // are reserved for explicit `.cborseq`/`.prefpseq`/`.dtrmseq`
        // contexts in schema-aware dumps.
        let document = parse(&[0x40]).expect("parse");
        let limits = EmbedLimits::default();
        let with_flag = render_document(&document, false, false, true, limits);
        assert!(with_flag.contains("h''"), "{with_flag}");
        assert!(!with_flag.contains("<<"), "{with_flag}");
    }
}
