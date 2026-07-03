// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! CDDL-to-Markdown transform.
//!
//! Builds the synthetic Markdown document that the optional
//! documentation linting pass feeds to `rumdl` for linting and fixing.
//!
//! The transform walks the captured pre-transform CDDL source line by
//! line and produces:
//!
//! * A synthetic Markdown string suitable for `rumdl` to consume.
//! * A `lines` map that records, for every output line, whether it originated from a CDDL
//!   doc comment, from a generated splice marker, or from a generated blank-line wrapper.
//!
//! # Transform rules
//!
//! * Each contiguous standalone `;!` block is emitted as Markdown after the `;!` marker
//!   is stripped and common leading space is removed from non-blank lines.
//! * Every contiguous run of non-doc CDDL lines (definitions, regular comments, `;@` and
//!   `;#` directives, blank lines) is collapsed into a single splice marker of the form
//!   `<!-- CBORK CDDL FROM start-end -->` where `start` and `end` are the inclusive
//!   1-based source line numbers that the marker covers.
//! * Whitespace-only spans that sit between separate doc blocks are still part of the
//!   non-doc span — they are folded into the splice marker so the Markdown engine cannot
//!   accidentally merge the two doc blocks.
//! * One blank line is generated above and below every splice marker. Those blank lines
//!   are flagged as [`SyntheticLineKind::GeneratedBlank`] so the reverse transform can
//!   strip them along with the marker.

use crate::doc_block::{LineClass, classify_line, strip_doc_marker};

/// Reserved prefix for splice markers. Doc comments that contain this
/// prefix must be rejected by the transform-safety validation pass.
pub const SPLICE_MARKER_PREFIX: &str = "CBORK CDDL FROM ";

/// A single line in the synthetic Markdown output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticLine {
    /// 1-based line number in the synthetic Markdown output.
    pub output_line: usize,
    /// The text content of this line, without the trailing newline.
    pub text: String,
    /// What this line represents and where it came from.
    pub kind: SyntheticLineKind,
}

/// What a synthetic Markdown line represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntheticLineKind {
    /// A line that originated from a standalone `;!` doc comment.
    /// `text` is the doc comment with the `;!` marker stripped.
    DocLine {
        /// 1-based source line number of the original `;!` line.
        source_line: usize,
        /// Number of stripped characters before synthetic column 1.
        ///
        /// This includes source indentation before `;!`, the two-byte
        /// marker itself, and any common block indent removed from the
        /// Markdown text.
        source_column_offset: usize,
    },
    /// A generated splice marker, e.g. `<!-- CBORK CDDL FROM 12-27 -->`.
    SpliceMarker {
        /// 1-based source line where the covered CDDL span starts.
        span_start: usize,
        /// 1-based source line where the covered CDDL span ends (inclusive).
        span_end: usize,
    },
    /// A generated blank line that wraps a splice marker. The blank
    /// line above the marker is at `output_line - 1`; the blank line
    /// below it is at `output_line + 1`. Both are removed together
    /// with the marker by the reverse transform.
    GeneratedBlank,
}

/// Result of [`transform_to_markdown`]: the synthetic Markdown text
/// plus a per-output-line map back to the original CDDL source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticMarkdown {
    /// Synthetic Markdown text with `\n` line terminators.
    pub text: String,
    /// Per-output-line map in source order, with 1-based line numbers.
    pub lines: Vec<SyntheticLine>,
}

impl SyntheticMarkdown {
    /// Returns `true` when no splice markers or doc lines were emitted,
    /// i.e. the source contained only non-doc content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Returns the number of synthetic output lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Iterate over the synthetic lines in output order.
    pub fn iter(&self) -> impl Iterator<Item = &SyntheticLine> {
        self.lines.iter()
    }
}

/// Transform the captured pre-transform CDDL source into the synthetic
/// Markdown document consumed by `rumdl`.
///
/// `source_text` should be the full pre-transform CDDL source (not a
/// synthetic substring) so the splice-marker line ranges match the
/// user's editor.
///
/// A trailing newline at the end of the source does not produce an
/// extra empty line; the transform splits on `\n` and ignores the
/// final empty element that arises from a terminating newline.
#[must_use]
pub fn transform_to_markdown(source_text: &str) -> SyntheticMarkdown {
    let mut lines: Vec<&str> = source_text.split('\n').collect();
    if lines.last().is_some_and(|s| s.is_empty()) {
        lines.pop();
    }
    let classes: Vec<LineClass> = lines.iter().map(|l| classify_line(l)).collect();

    let mut output_text: Vec<String> = Vec::new();
    let mut output_kinds: Vec<SyntheticLineKind> = Vec::new();

    let mut idx = 0usize;
    while idx < lines.len() {
        match classes.get(idx).copied() {
            Some(LineClass::DocLine) => {
                let mut doc_lines = Vec::new();
                while matches!(classes.get(idx).copied(), Some(LineClass::DocLine)) {
                    doc_lines.push((
                        idx.saturating_add(1),
                        lines.get(idx).copied().unwrap_or_default(),
                    ));
                    idx = idx.saturating_add(1);
                }

                let stripped: Vec<String> = doc_lines
                    .iter()
                    .map(|(_, line)| strip_doc_marker(line))
                    .collect();
                let common_indent = common_leading_space_indent(&stripped);

                for ((source_line, original_line), text) in doc_lines.iter().zip(stripped) {
                    let leading_ws_len = original_line
                        .len()
                        .saturating_sub(original_line.trim_start().len());
                    output_text.push(dedent_doc_line(&text, common_indent));
                    output_kinds.push(SyntheticLineKind::DocLine {
                        source_line: *source_line,
                        source_column_offset: leading_ws_len
                            .saturating_add(2)
                            .saturating_add(common_indent),
                    });
                }
            },
            Some(_) => {
                let span_start = idx.saturating_add(1);
                while let Some(c) = classes.get(idx).copied() {
                    if c == LineClass::DocLine {
                        break;
                    }
                    idx = idx.saturating_add(1);
                }
                let span_end = idx;

                push_splice_marker(&mut output_text, &mut output_kinds, span_start, span_end);
            },
            None => break,
        }
    }

    let text = output_text.join("\n");
    let lines_out: Vec<SyntheticLine> = output_text
        .into_iter()
        .zip(output_kinds)
        .enumerate()
        .map(|(i, (text, kind))| {
            SyntheticLine {
                output_line: i.saturating_add(1),
                text,
                kind,
            }
        })
        .collect();

    SyntheticMarkdown {
        text,
        lines: lines_out,
    }
}

/// Return the minimum leading-space indent across non-blank doc lines.
fn common_leading_space_indent(lines: &[String]) -> usize {
    let mut min = usize::MAX;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        min = min.min(line.bytes().take_while(|b| *b == b' ').count());
    }

    if min == usize::MAX { 0 } else { min }
}

/// Remove `common_indent` leading spaces from a doc line when present.
fn dedent_doc_line(
    line: &str,
    common_indent: usize,
) -> String {
    if line.trim().is_empty() {
        return String::new();
    }

    let remove = line
        .bytes()
        .take_while(|b| *b == b' ')
        .count()
        .min(common_indent);
    debug_assert!(line.is_char_boundary(remove));
    line.get(remove..)
        .map_or_else(String::new, ToOwned::to_owned)
}

/// Emit a blank line, then the splice marker for the covered span,
/// then another blank line. All three lines are pushed onto the
/// output buffers in order.
fn push_splice_marker(
    output_text: &mut Vec<String>,
    output_kinds: &mut Vec<SyntheticLineKind>,
    span_start: usize,
    span_end: usize,
) {
    output_text.push(String::new());
    output_kinds.push(SyntheticLineKind::GeneratedBlank);

    output_text.push(format!(
        "<!-- {SPLICE_MARKER_PREFIX}{span_start}-{span_end} -->"
    ));
    output_kinds.push(SyntheticLineKind::SpliceMarker {
        span_start,
        span_end,
    });

    output_text.push(String::new());
    output_kinds.push(SyntheticLineKind::GeneratedBlank);
}

/// Returns the splice-marker span covered by `kind`, if `kind` is a
/// [`SyntheticLineKind::SpliceMarker`]. Convenience helper for
/// downstream reverse-transform code and for tests.
#[must_use]
pub fn splice_span(kind: &SyntheticLineKind) -> Option<(usize, usize)> {
    match kind {
        SyntheticLineKind::SpliceMarker {
            span_start,
            span_end,
        } => Some((*span_start, *span_end)),
        _ => None,
    }
}

/// Returns the original CDDL source line backing `kind`, if any.
#[must_use]
pub fn source_line(kind: &SyntheticLineKind) -> Option<usize> {
    match kind {
        SyntheticLineKind::DocLine { source_line, .. } => Some(*source_line),
        SyntheticLineKind::SpliceMarker { span_start, .. } => Some(*span_start),
        SyntheticLineKind::GeneratedBlank => None,
    }
}

// ---------------------------------------------------------------------
// Reverse transform (step 11 of `crates/cbork/plan.md`)
// ---------------------------------------------------------------------

/// Error produced by [`reverse_transform`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReverseTransformError {
    /// The fixed synthetic Markdown contained a different set of
    /// `<!-- CBORK CDDL FROM start-end -->` splice markers than the
    /// original synthetic Markdown. The error carries diagnostics the
    /// caller can surface to the user.
    SpliceMarkerIntegrity {
        /// Description of what changed (deletion, duplication,
        /// reordering, or span change).
        detail: String,
    },
}

impl std::fmt::Display for ReverseTransformError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::SpliceMarkerIntegrity { detail } => {
                write!(f, "splice-marker integrity violation: {detail}")
            },
        }
    }
}

impl std::error::Error for ReverseTransformError {}

/// Reverse the CDDL-to-Markdown transform: take the `rumdl`-fixed
/// synthetic Markdown in memory and the original pre-transform CDDL
/// source, and reconstruct a fixed CDDL document where:
///
/// * Doc comment lines (`;!`) are rewritten from the fixed Markdown.
/// * Non-doc CDDL spans are restored from the original source byte-for-byte using the
///   splice-marker line table.
/// * Generated blank-line wrappers around splice markers are removed.
/// * Multiple consecutive blank lines in the final output are collapsed to one blank
///   line.
///
/// The function never re-reads the on-disk file — the original CDDL
/// comes from `original_source` (in-memory capture).
///
/// # Errors
///
/// Returns [`ReverseTransformError::SpliceMarkerIntegrity`] when the
/// fixed synthetic Markdown has a different set of splice markers
/// than the original synthetic Markdown. The caller must refuse the
/// fix in that case.
pub fn reverse_transform(
    fixed_synthetic: &str,
    original_source: &str,
    original_synthetic: &SyntheticMarkdown,
) -> Result<String, ReverseTransformError> {
    verify_splice_markers(fixed_synthetic, original_synthetic)?;

    let mut source_lines: Vec<&str> = original_source.split('\n').collect();
    if source_lines.last().is_some_and(|s| s.is_empty()) {
        source_lines.pop();
    }

    // Filter the fixed synthetic to non-blank lines.  Rumdl can
    // add or remove blank lines in Markdown, but its fixes never
    // change the splice-marker spans or the number of non-blank
    // doc-content lines (verified above).  By filtering both
    // sides to their non-blank content we can align them
    // one-to-one.
    let fixed_non_blanks: Vec<&str> = fixed_synthetic
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>();
    let original_entries: Vec<&SyntheticLine> = original_synthetic
        .lines
        .iter()
        .filter(|l| {
            !matches!(l.kind, SyntheticLineKind::GeneratedBlank) && !l.text.trim().is_empty()
        })
        .collect();

    if fixed_non_blanks.len() != original_entries.len() {
        return Err(ReverseTransformError::SpliceMarkerIntegrity {
            detail: format!(
                "expected {} non-blank entries in the fixed synthetic, found {}",
                original_entries.len(),
                fixed_non_blanks.len()
            ),
        });
    }

    let mut output = String::new();
    for (fixed_line, original_line) in fixed_non_blanks.iter().zip(original_entries.iter()) {
        match &original_line.kind {
            SyntheticLineKind::DocLine { .. } => {
                output.push_str(";! ");
                output.push_str(fixed_line);
                output.push('\n');
            },
            SyntheticLineKind::SpliceMarker {
                span_start,
                span_end,
            } => {
                for i in *span_start..=*span_end {
                    if let Some(line) = source_lines.get(i.saturating_sub(1)) {
                        output.push_str(line);
                        output.push('\n');
                    }
                }
            },
            SyntheticLineKind::GeneratedBlank => {
                // Already filtered above; reachable only if the enum
                // changes and this arm is added for exhaustiveness.
            },
        }
    }

    Ok(trim_trailing_whitespace(&collapse_blank_lines(&output)))
}

/// Parse the `<!-- CBORK CDDL FROM start-end -->` markers from
/// `text` and return the `(start, end)` pairs in source order.
///
/// Used by the integrity check to compare the fixed and original
/// synthetic Markdown documents.
#[must_use]
pub fn find_splice_markers(text: &str) -> Vec<(usize, usize)> {
    let mut markers = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(body) = trimmed
            .strip_prefix("<!-- CBORK CDDL FROM ")
            .and_then(|rest| rest.strip_suffix(" -->"))
            && let Some((s, e)) = body.split_once('-')
        {
            let start = s.parse::<usize>();
            let end = e.parse::<usize>();
            if let (Ok(start), Ok(end)) = (start, end) {
                markers.push((start, end));
            }
        }
    }
    markers
}

/// Compare the splice markers in the fixed synthetic Markdown with
/// the original synthetic Markdown. Returns `Ok(())` when the
/// markers are identical in count, order, and span. Returns
/// `Err(ReverseTransformError::SpliceMarkerIntegrity)` otherwise so
/// the caller can refuse the fix.
fn verify_splice_markers(
    fixed_synthetic: &str,
    original_synthetic: &SyntheticMarkdown,
) -> Result<(), ReverseTransformError> {
    let fixed = find_splice_markers(fixed_synthetic);
    let original = find_splice_markers(&original_synthetic.text);

    if fixed.len() != original.len() {
        return Err(ReverseTransformError::SpliceMarkerIntegrity {
            detail: format!(
                "expected {} splice markers, found {}",
                original.len(),
                fixed.len()
            ),
        });
    }

    for (idx, (fc, oc)) in fixed.iter().zip(original.iter()).enumerate() {
        if fc != oc {
            return Err(ReverseTransformError::SpliceMarkerIntegrity {
                detail: format!("splice marker {idx} changed: expected {oc:?}, got {fc:?}"),
            });
        }
    }

    Ok(())
}

/// Collapse every run of two or more consecutive blank (whitespace-only)
/// lines into a single blank line.
///
/// The plan requires this as the *final* `--fix` output policy, running
/// only after the reverse transform has reconstructed valid CDDL.
#[must_use]
pub fn collapse_blank_lines(text: &str) -> String {
    let mut output = String::new();
    let mut prev_blank = false;
    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if !prev_blank {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push('\n');
                prev_blank = true;
            }
        } else {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(line);
            output.push('\n');
            prev_blank = false;
        }
    }
    output
}

/// Strip trailing whitespace from every line and reduce whitespace-only
/// lines to empty lines.
///
/// This is the `--fix` post-process pass for normalizing CDDL source
/// after the reverse transform has reconstructed it. It runs only on
/// the final reverse-transformed output; the synthetic Markdown and
/// the splice-marker restoration are never fed whitespace-trimmed
/// input because that would shift the splice-marker line map.
///
/// Trailing newline handling matches [`collapse_blank_lines`]: a single
/// trailing newline is preserved when the input ends with one (matching
/// the standard "POSIX text file" convention), and the final empty
/// element produced by a terminating `\n` is dropped. This keeps the
/// post-process output shape identical to what callers expect from
/// `collapse_blank_lines`.
///
/// Leading whitespace is intentionally preserved so `;!` doc-block
/// indentation is left alone.
#[must_use]
pub fn trim_trailing_whitespace(text: &str) -> String {
    let mut output = String::new();
    let ends_with_newline = text.ends_with('\n');
    let lines: Vec<&str> = if ends_with_newline {
        // Mirror `collapse_blank_lines`: split on `\n` after dropping the
        // terminator so the trailing empty element does not produce an
        // extra blank line in the output.
        let trimmed_len = text.len().saturating_sub("\n".len());
        let body = text.get(..trimmed_len).unwrap_or("");
        body.split('\n').collect()
    } else {
        text.split('\n').collect()
    };
    for line in lines {
        let trimmed = line.trim_end();
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(trimmed);
        output.push('\n');
    }
    if !ends_with_newline && !output.is_empty() && output.ends_with('\n') {
        // No trailing newline on the input means we should not emit
        // one either; trim the surplus from the last emitted line.
        output.pop();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_produces_empty_synthetic() {
        let synthetic = transform_to_markdown("");
        assert!(synthetic.is_empty());
        assert_eq!(synthetic.text, "");
    }

    #[test]
    fn source_with_no_doc_lines_is_pure_splice_marker() {
        let source = "rule = 1\n";
        let synthetic = transform_to_markdown(source);

        assert_eq!(synthetic.lines.len(), 3);
        assert_eq!(synthetic.lines[0].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.lines[1].kind, SyntheticLineKind::SpliceMarker {
            span_start: 1,
            span_end: 1,
        });
        assert_eq!(synthetic.lines[2].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.text, "\n<!-- CBORK CDDL FROM 1-1 -->\n");
    }

    #[test]
    fn doc_line_strips_marker_and_common_indent() {
        let source = ";! # Title\nrule = 1\n";
        let synthetic = transform_to_markdown(source);

        assert_eq!(synthetic.lines.len(), 4);
        assert_eq!(synthetic.lines[0].kind, SyntheticLineKind::DocLine {
            source_line: 1,
            source_column_offset: 3,
        });
        assert_eq!(synthetic.lines[0].text, "# Title");
        assert_eq!(synthetic.lines[1].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.lines[2].kind, SyntheticLineKind::SpliceMarker {
            span_start: 2,
            span_end: 2,
        });
        assert_eq!(synthetic.lines[3].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.text, "# Title\n\n<!-- CBORK CDDL FROM 2-2 -->\n");
    }

    #[test]
    fn doc_block_dedents_numbered_list() {
        let source = "\
;! # Title
;!
;! 1. Concatenate the UTF-8 key bytes.
;! 2. Hash the result.
rule = 1
";
        let synthetic = transform_to_markdown(source);

        assert!(
            synthetic
                .text
                .starts_with("# Title\n\n1. Concatenate the UTF-8 key bytes.\n2. Hash the result.")
        );
        assert_eq!(
            synthetic
                .lines
                .iter()
                .filter_map(|line| {
                    match line.kind {
                        SyntheticLineKind::DocLine {
                            source_line,
                            source_column_offset,
                        } => Some((source_line, source_column_offset, line.text.as_str())),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>(),
            vec![
                (1, 3, "# Title"),
                (2, 3, ""),
                (3, 3, "1. Concatenate the UTF-8 key bytes."),
                (4, 3, "2. Hash the result."),
            ]
        );
    }

    #[test]
    fn intervening_non_doc_lines_become_one_splice_marker() {
        let source = "\
;! # Title
; regular comment
;@ CBORK: Library
rule = 1
trailing = 2
";
        let synthetic = transform_to_markdown(source);

        // One doc line covers `;! # Title` on line 1. The remaining
        // lines 2..=5 form a single non-doc span with one splice
        // marker.
        assert_eq!(synthetic.lines.len(), 4);
        assert_eq!(synthetic.lines[0].kind, SyntheticLineKind::DocLine {
            source_line: 1,
            source_column_offset: 3,
        });
        assert_eq!(synthetic.lines[1].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.lines[2].kind, SyntheticLineKind::SpliceMarker {
            span_start: 2,
            span_end: 5,
        });
        assert_eq!(synthetic.lines[3].kind, SyntheticLineKind::GeneratedBlank);
    }

    #[test]
    fn whitespace_only_span_between_doc_blocks_is_a_splice_marker() {
        let source = "\
;! # First block
rule_a = 1


;! # Second block
rule_b = 2
";
        let synthetic = transform_to_markdown(source);

        // Doc lines: line 1, line 5. The whitespace-only lines 3..=4
        // belong to the splice marker for the first block so the
        // Markdown engine cannot merge the two doc blocks.
        let doc_lines: Vec<_> = synthetic
            .lines
            .iter()
            .filter_map(|l| {
                match l.kind {
                    SyntheticLineKind::DocLine { source_line, .. } => Some(source_line),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(doc_lines, vec![1, 5]);

        let splice_spans: Vec<_> = synthetic
            .lines
            .iter()
            .filter_map(|l| splice_span(&l.kind))
            .collect();
        assert_eq!(splice_spans, vec![(2, 4), (6, 6)]);
    }

    #[test]
    fn two_adjacent_doc_lines_share_no_splice_marker() {
        let source = ";! # A\n;! # B\nrule = 1\n";
        let synthetic = transform_to_markdown(source);

        // Two doc lines (no intervening non-doc content) followed by
        // the splice marker for the trailing rule.
        assert_eq!(synthetic.lines.len(), 5);
        assert_eq!(synthetic.lines[0].kind, SyntheticLineKind::DocLine {
            source_line: 1,
            source_column_offset: 3,
        });
        assert_eq!(synthetic.lines[1].kind, SyntheticLineKind::DocLine {
            source_line: 2,
            source_column_offset: 3,
        });
        assert_eq!(synthetic.lines[2].kind, SyntheticLineKind::GeneratedBlank);
        assert_eq!(synthetic.lines[3].kind, SyntheticLineKind::SpliceMarker {
            span_start: 3,
            span_end: 3,
        });
        assert_eq!(synthetic.lines[4].kind, SyntheticLineKind::GeneratedBlank);
    }

    #[test]
    fn inline_doc_comments_inside_a_map_body_become_distinct_blocks() {
        // The opening `rule = {`, the field-value line, and the
        // closing `}` are non-doc content; the inline `;!` comment is
        // a doc line.
        let source = "\
rule = {
  ;! # field description
  \"name\" => tstr,
}
";
        let synthetic = transform_to_markdown(source);

        let doc_lines: Vec<_> = synthetic
            .lines
            .iter()
            .filter_map(|l| {
                match l.kind {
                    SyntheticLineKind::DocLine { source_line, .. } => Some(source_line),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(doc_lines, vec![2]);

        let splice_spans: Vec<_> = synthetic
            .lines
            .iter()
            .filter_map(|l| splice_span(&l.kind))
            .collect();
        // Two non-doc spans: line 1 (the opening `rule = {`) and
        // lines 3..=4 (the field value and the closing brace).
        assert_eq!(splice_spans, vec![(1, 1), (3, 4)]);
    }

    #[test]
    fn splice_marker_format_is_stable() {
        let synthetic = transform_to_markdown("rule = 1\n");
        assert!(synthetic.text.contains("<!-- CBORK CDDL FROM 1-1 -->"));
        assert!(synthetic.text.starts_with('\n'));
        assert!(synthetic.text.ends_with('\n'));
    }

    #[test]
    fn output_lines_are_one_indexed_in_order() {
        let synthetic = transform_to_markdown(";! # Title\nrule = 1\n");
        for (i, line) in synthetic.lines.iter().enumerate() {
            assert_eq!(line.output_line, i.saturating_add(1));
        }
    }

    #[test]
    fn only_doc_content_emits_no_splice_marker() {
        let source = ";! # Title\n;! Description\n";
        let synthetic = transform_to_markdown(source);

        assert_eq!(synthetic.lines.len(), 2);
        assert!(
            synthetic
                .lines
                .iter()
                .all(|l| matches!(l.kind, SyntheticLineKind::DocLine { .. }))
        );
    }

    #[test]
    fn source_line_helper_extracts_backing_source() {
        let synthetic = transform_to_markdown(";! # Title\nrule = 1\n");
        for line in &synthetic.lines {
            match &line.kind {
                SyntheticLineKind::DocLine {
                    source_line: src, ..
                } => {
                    assert_eq!(super::source_line(&line.kind), Some(*src));
                },
                SyntheticLineKind::SpliceMarker { span_start, .. } => {
                    assert_eq!(super::source_line(&line.kind), Some(*span_start));
                },
                SyntheticLineKind::GeneratedBlank => {
                    assert_eq!(super::source_line(&line.kind), None);
                },
            }
        }
    }

    // -----------------------------------------------------------------
    // Reverse transform tests (step 11)
    // -----------------------------------------------------------------

    #[test]
    fn reverse_transform_roundtrips_unmodified_source() {
        let source = ";! # Title\n;! Description\nrule = 1\n";
        let original = transform_to_markdown(source);
        let fixed = reverse_transform(&original.text, source, &original)
            .expect("reverse should succeed on an unmodified synthetic");
        assert_eq!(fixed, source);
    }

    #[test]
    fn reverse_transform_roundtrips_source_with_blank_lines() {
        let source = ";! # Title\n\nrule = 1\n";
        let original = transform_to_markdown(source);
        let fixed = reverse_transform(&original.text, source, &original)
            .expect("reverse should succeed on an unmodified synthetic");
        assert_eq!(fixed, source);
    }

    #[test]
    fn reverse_transform_roundtrips_multi_line_cddl_rule() {
        let source = "\
;! # Title
rule = {
  field: tstr,
}
";
        let original = transform_to_markdown(source);
        let fixed = reverse_transform(&original.text, source, &original)
            .expect("reverse should succeed on an unmodified synthetic");
        assert_eq!(fixed, source);
    }

    #[test]
    fn reverse_transform_roundtrips_two_doc_blocks() {
        let source = "\
;! # File title
rule_a = 1

;! ### definition
rule_b = 2
";
        let original = transform_to_markdown(source);
        let fixed =
            reverse_transform(&original.text, source, &original).expect("reverse should succeed");
        assert_eq!(fixed, source);
    }

    #[test]
    fn reverse_transform_preserves_non_doc_spans_byte_for_byte() {
        let source = "\
;! # Title
;@ CBORK: Library
;  multi-line
;  regular comment
rule = 1
";
        let original = transform_to_markdown(source);
        let fixed =
            reverse_transform(&original.text, source, &original).expect("reverse should succeed");
        assert_eq!(fixed, source);
    }

    #[test]
    fn reverse_transform_doc_fix_applies_to_doc_lines_only() {
        let source = "\
;! # Title
;! Description
rule = 1
";
        let original = transform_to_markdown(source);
        // The fixed Markdown has a different title; the CDDL rule must
        // still come through byte-for-byte.
        let fixed_md = "# Fixed Title\nDescription\n\n<!-- CBORK CDDL FROM 3-3 -->\n";
        let fixed = reverse_transform(fixed_md, source, &original).expect("reverse should succeed");
        let expected = ";! # Fixed Title\n;! Description\nrule = 1\n";
        assert_eq!(fixed, expected);
    }

    #[test]
    fn reverse_transform_refuses_on_deleted_splice_marker() {
        let source = ";! # Title\nrule = 1\n";
        let original = transform_to_markdown(source);
        // Remove the splice marker line entirely.
        let fixed_md = " # Title\n";
        let err = reverse_transform(fixed_md, source, &original)
            .expect_err("should reject when a splice marker is deleted");
        assert!(
            err.to_string()
                .contains("splice-marker integrity violation")
        );
    }

    #[test]
    fn reverse_transform_refuses_on_changed_splice_marker_span() {
        let source = ";! # Title\nrule = 1\n";
        let original = transform_to_markdown(source);
        // Change the span end from 2 to 3.
        let fixed_md = " # Title\n\n<!-- CBORK CDDL FROM 2-3 -->\n";
        let err = reverse_transform(fixed_md, source, &original)
            .expect_err("should reject when a splice marker span changes");
        assert!(err.to_string().contains("splice-marker integrity"));
    }

    #[test]
    fn reverse_transform_refuses_on_reordered_markers() {
        let source = "\
;! # A
rule_a = 1

;! # B
rule_b = 2
";
        let original = transform_to_markdown(source);
        // Swap the two splice markers.
        let fixed_md = "\
 # A


<!-- CBORK CDDL FROM 6-6 -->


 # B


<!-- CBORK CDDL FROM 3-3 -->

";
        let err = reverse_transform(fixed_md, source, &original)
            .expect_err("should reject when splice markers are reordered");
        assert!(err.to_string().contains("splice-marker"));
    }

    #[test]
    fn collapse_blank_lines_collapses_runs_to_one() {
        let input = "first\n\n\n\nsecond\n\n";
        assert_eq!(collapse_blank_lines(input), "first\n\nsecond\n\n");
    }

    #[test]
    fn collapse_blank_lines_preserves_single_blank_lines() {
        let input = "first\n\nsecond\n";
        assert_eq!(collapse_blank_lines(input), "first\n\nsecond\n");
    }

    #[test]
    fn collapse_blank_lines_handles_no_blanks() {
        let input = "first\nsecond\n";
        assert_eq!(collapse_blank_lines(input), "first\nsecond\n");
    }

    #[test]
    fn trim_trailing_whitespace_strips_each_line() {
        let input = ";! x   \nrule = 1\t\n\n";
        assert_eq!(trim_trailing_whitespace(input), ";! x\nrule = 1\n\n");
    }

    #[test]
    fn trim_trailing_whitespace_reduces_whitespace_only_lines() {
        let input = ";! x\n   \t  \nrule = 1\n";
        assert_eq!(trim_trailing_whitespace(input), ";! x\n\nrule = 1\n");
    }

    #[test]
    fn trim_trailing_whitespace_preserves_leading_whitespace() {
        // Doc-block indentation must survive; only the trailing side
        // is trimmed.
        let input = "    ;! indented   \n        child   \n";
        assert_eq!(
            trim_trailing_whitespace(input),
            "    ;! indented\n        child\n"
        );
    }

    #[test]
    fn trim_trailing_whitespace_idempotent_on_clean_input() {
        let input = ";! # Title\n\n;! Description\nrule = 1\n";
        let first = trim_trailing_whitespace(input);
        let second = trim_trailing_whitespace(&first);
        assert_eq!(first, input);
        assert_eq!(second, input);
    }

    #[test]
    fn trim_trailing_whitespace_handles_empty_input() {
        // An empty file stays empty; an input that is just a single
        // newline round-trips to a single newline.
        assert_eq!(trim_trailing_whitespace(""), "");
        assert_eq!(trim_trailing_whitespace("\n"), "\n");
    }

    #[test]
    fn reverse_transform_strips_trailing_whitespace_after_fix() {
        // --doc --fix must end with stripped-trailing-whitespace
        // output even when the original source has trailing tabs and
        // spaces. The reverse transform preserves the original
        // non-doc CDDL spans byte-for-byte but the post-process
        // trim pass is responsible for normalizing them.
        let source = ";! # Title   \n\nrule = 1  \n";
        let original = transform_to_markdown(source);
        let fixed_md = &original.text;
        let result =
            reverse_transform(fixed_md, source, &original).expect("reverse transform must succeed");
        assert!(!result.contains("   "), "trailing spaces must be stripped");
        assert!(!result.contains('\t'), "trailing tabs must be stripped");
        assert!(result.contains("rule = 1\n"), "CDDL rule must survive");
    }

    #[test]
    fn reverse_transform_replaces_whitespace_only_lines() {
        // A whitespace-only line in the original source must come out
        // as a truly empty line after --fix, not as `   \n`.
        let source = ";! # Title\n   \nrule = 1\n";
        let original = transform_to_markdown(source);
        let fixed_md = &original.text;
        let result =
            reverse_transform(fixed_md, source, &original).expect("reverse transform must succeed");
        assert!(
            !result.contains("   \n"),
            "whitespace-only line must be trimmed"
        );
    }

    #[test]
    fn find_splice_markers_extracts_ordered_pairs() {
        let md = "\n\n<!-- CBORK CDDL FROM 1-5 -->\n\n<!-- CBORK CDDL FROM 10-15 -->\n";
        assert_eq!(find_splice_markers(md), vec![(1, 5), (10, 15)]);
    }

    #[test]
    fn find_splice_markers_ignores_other_html_comments() {
        let md = "<!-- not a splice marker -->\n<!-- CBORK CDDL FROM 1-5 -->\n";
        assert_eq!(find_splice_markers(md), vec![(1, 5)]);
    }

    #[test]
    fn reverse_transform_handles_blank_doc_lines_in_original() {
        // A fixture where the doc block contains blank `;!` lines
        // (just the marker with no content). The fixed synthetic has
        // rumdl-removed blanks, so the non-blank alignment must still
        // match after both sides filter blank-content lines.
        let source = "\
;! # Title
;!
;! Description
rule = 1
";
        let original = transform_to_markdown(source);
        // Fixed synthetic: rumdl removed the blank line and made the
        // heading non-indented.
        let fixed_md = "# Title\nDescription\n\n<!-- CBORK CDDL FROM 4-4 -->\n";
        let result = reverse_transform(fixed_md, source, &original)
            .expect("should succeed even when rumdl removes blank doc lines");
        assert_eq!(result, ";! # Title\n;! Description\nrule = 1\n");
    }

    #[test]
    fn reverse_transform_handles_rumdl_blank_line_insertion() {
        // Rumdl may ADD blank lines (e.g. MD022). The non-blank
        // alignment must still work because both sides drop
        // blank-content lines.
        let source = "\
;! # Title
;! Description
rule = 1
";
        let original = transform_to_markdown(source);
        // Fixed: rumdl added blank line between heading and body.
        let fixed_md = "# Title\n\nDescription\n\n<!-- CBORK CDDL FROM 3-3 -->\n";
        let result = reverse_transform(fixed_md, source, &original)
            .expect("should succeed when rumdl adds blank lines");
        // The reconstructed CDDL preserves content; blank-line collapse
        // reduces the added blank to a single blank if it lands between
        // ;! doc lines.
        assert!(result.contains(";! # Title"));
        assert!(result.contains(";! Description"));
        assert!(result.contains("rule = 1"));
    }
}
