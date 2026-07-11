// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Capture and render ABNF match traces as CDN comment blocks.
//!
//! Successful `.abnfb` and `.abnf` validations run the ABNF parser
//! twice: once to confirm the boolean result, and once more to capture
//! the selected match tree (rule path plus byte offsets). The trace is
//! stored alongside the validation result, keyed by the byte-string or
//! text-string path. The detailed dump renderer looks up the trace and
//! appends an indented `// ...` comment block immediately below the
//! value, per `rfc/draft-ietf-cbor-edn-literals-25.txt` section 2.2.
//!
//! The renderer is intentionally compact: it only shows the start rule
//! and the children that share a non-trivial span with the consumed
//! input. Terminal numeric/char-val children are skipped because they
//! would produce thousands of identical lines for byte sequences
//! matched by `OCTET = %x00-FF` and similar productions. The original
//! value line is preserved exactly as the normal CDN renderer emits
//! it; only the comments below it are new.

use std::{cell::RefCell, collections::HashMap, fmt::Write as _};

use cbork_abnf_parser::AbnfMatch;

use crate::validate::PathStep;

thread_local! {
    /// Successful ABNF match traces keyed by the validation path of
    /// the byte-string or text-string value. Cleared at the start of
    /// each `render_validation_dump` call.
    static ABNF_TRACES: RefCell<HashMap<Vec<PathStep>, AbnfTrace>> =
        RefCell::new(HashMap::new());
}

/// A captured ABNF match trace plus the original input bytes so the
/// renderer can format byte and text spans uniformly.
pub(crate) struct AbnfTrace {
    /// The original input bytes that were validated.
    pub(crate) input: Vec<u8>,
    /// The selected match tree produced by the ABNF parser.
    pub(crate) trace: AbnfMatch,
}

/// Reset the trace map at the start of each render pass.
pub(crate) fn reset_traces() {
    ABNF_TRACES.with(|slot| slot.borrow_mut().clear());
}

/// Reset the trace map at the start of each `exec` call so successive
/// validation runs do not leak traces.
pub(crate) fn reset_traces_for_exec() {
    reset_traces();
}

/// Record a trace for the given path. Returns an error if the path is
/// empty.
pub(crate) fn record_trace(
    path: &[PathStep],
    input: &[u8],
    trace: AbnfMatch,
) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("ABNF match trace path is empty");
    }
    ABNF_TRACES.with(|slot| {
        slot.borrow_mut().insert(path.to_vec(), AbnfTrace {
            input: input.to_vec(),
            trace,
        });
    });
    Ok(())
}

/// Return the trace for the given path, if one was recorded.
pub(crate) fn get_trace(path: &[PathStep]) -> Option<AbnfTrace> {
    ABNF_TRACES.with(|slot| slot.borrow().get(path).cloned())
}

impl Clone for AbnfTrace {
    fn clone(&self) -> Self {
        Self {
            input: self.input.clone(),
            trace: self.trace.clone(),
        }
    }
}

/// Append a CDN comment block describing the captured ABNF match trace
/// to `output`. The original value line emitted by the normal CDN
/// renderer is not touched; the caller is responsible for emitting it
/// before calling this helper.
pub(crate) fn append_breakdown_comments(
    path: &[PathStep],
    output: &mut String,
    indent: usize,
    color: bool,
) {
    let Some(trace) = get_trace(path) else {
        return;
    };
    let mut lines = Vec::new();
    collect_breakdown_lines(&trace.input, &trace.trace, 0, &mut lines, true);
    for line in lines {
        output.push('\n');
        for _ in 0..indent {
            output.push(' ');
        }
        if color {
            let _ = write!(output, "{}", console::style("// ABNF: ").dim());
            let _ = write!(output, "{}", console::style(line).dim());
        } else {
            output.push_str("// ABNF: ");
            output.push_str(&line);
        }
    }
}

/// Walk the match trace and produce one human-readable line per
/// informative rule node. Lines describe the start rule, the directly
/// selected child rules, and the byte or text span each one consumed.
fn collect_breakdown_lines(
    input: &[u8],
    node: &AbnfMatch,
    depth: usize,
    lines: &mut Vec<String>,
    is_root: bool,
) {
    let span_len = node.end().saturating_sub(node.start());
    if !is_root {
        // Skip terminal numeric/char-val-style rules that contribute a
        // single byte; they would otherwise dominate the breakdown
        // for large inputs.
        if span_len <= 1 && node.children().is_empty() {
            return;
        }
    }
    if is_root || !node.children().is_empty() {
        let span = format_byte_or_text_span(input, node.start(), node.end());
        let indented_name = "  ".repeat(depth);
        lines.push(format!("{}{} = {}", indented_name, node.rule(), span));
    }
    for child in node.children() {
        collect_breakdown_lines(input, child, depth.saturating_add(1), lines, false);
    }
}

/// Format an input byte slice span as a CDN literal: a text-quoted
/// slice when every byte is printable UTF-8, otherwise `h'...'` hex.
fn format_byte_or_text_span(
    input: &[u8],
    start: usize,
    end: usize,
) -> String {
    let Some(span) = input.get(start..end) else {
        return String::new();
    };
    if span.is_empty() {
        return "\"\"".to_owned();
    }
    if let Ok(text) = std::str::from_utf8(span)
        && text.chars().all(|ch| !ch.is_control())
    {
        return format!("{text:?}");
    }
    let mut out = String::with_capacity(span.len().saturating_mul(3).saturating_add(4));
    out.push_str("h'");
    for (index, byte) in span.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out.push('\'');
    out
}
