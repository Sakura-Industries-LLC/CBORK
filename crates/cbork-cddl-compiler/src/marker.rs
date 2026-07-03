// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Comment marker classification for documentation linting.
//!
//! CDDL supports three "special" comment markers that carry semantic meaning
//! beyond ordinary source comments:
//!
//! * `;!` — documentation comment marker (`crates/cbork/plan.md` § *Optional
//!   documentation linting*).
//! * `;@` — CBORK directive comment marker (e.g. `;@ CBORK: Library`).
//! * `;#` — module include/import directive comment marker (e.g. `;# include
//!   "./foo.cddl"`).
//!
//! These markers are only recognized on **standalone** comment lines:
//! lines where the marker appears after only leading whitespace.
//! Using one of these markers as a trailing comment (after non-whitespace
//! source on the same line) is a marker-misuse warning because the
//! directive or documentation block would otherwise be silently lost.
//!
//! This module provides:
//!
//! * [`CommentMarker`] — the three special marker variants.
//! * [`classify_comment_marker`] — pure classification of the marker text itself, without
//!   source context.
//! * [`MarkerPosition`] — the marker position relative to its source line.
//! * [`classify_comment_position`] — full classification combining the marker and its
//!   position on the source line.
//! * [`detect_marker_misuse`] — emit a W030 warning for every CDDL comment whose special
//!   marker is misused as a trailing comment.
//! * [`collect_marker_spacing_issues`] — emit a W036 warning for every standalone `;!`
//!   documentation comment that does not have a space after the marker.

use std::ops::Range;

use crate::{Diagnostic, DiagnosticLevel, SourceOrigin, WrappedNode, compiled::CompiledCDDL};

/// Semantic marker classification for a CDDL comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentMarker {
    /// `;!` documentation comment marker.
    Documentation,
    /// `;@` CBORK directive comment marker.
    CborkDirective,
    /// `;#` include/import directive comment marker.
    IncludeDirective,
}

impl CommentMarker {
    /// Returns the literal marker string (e.g. `";!"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CommentMarker::Documentation => ";!",
            CommentMarker::CborkDirective => ";@",
            CommentMarker::IncludeDirective => ";#",
        }
    }

    /// Returns a short description of the marker's semantic role.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            CommentMarker::Documentation => "documentation",
            CommentMarker::CborkDirective => "CBORK directive",
            CommentMarker::IncludeDirective => "include/import directive",
        }
    }
}

/// Position of a CDDL comment relative to its source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerPosition {
    /// The comment starts a logical block on the line:
    /// either at column 1 or after only leading whitespace.
    /// Carries the recognized marker when one is present.
    Standalone(Option<CommentMarker>),
    /// The comment appears after non-whitespace CDDL source on the
    /// same line. Carries the recognized marker when one is present;
    /// `None` for trailing ordinary `;` comments.
    Trailing(Option<CommentMarker>),
}

/// Classify the semantic marker carried by a CDDL comment text.
///
/// Returns `None` when the comment is an ordinary `;` comment with no
/// special marker. Leading whitespace before the marker is allowed;
/// any text after the marker is ignored by this pure helper.
#[must_use]
pub fn classify_comment_marker(text: &str) -> Option<CommentMarker> {
    let mut chars = text.chars();
    match chars.next()? {
        ';' => {
            match chars.next()? {
                '!' => Some(CommentMarker::Documentation),
                '@' => Some(CommentMarker::CborkDirective),
                '#' => Some(CommentMarker::IncludeDirective),
                _ => None,
            }
        },
        _ => None,
    }
}

/// Classify a CDDL comment's marker position using its comment text and
/// the source line that contains it.
///
/// `column` is the 1-based byte column of the leading `;` in the source
/// line. `source_line` is the full source line (without the trailing
/// newline).
#[must_use]
pub fn classify_comment_position(
    text: &str,
    source_line: &str,
    column: usize,
) -> MarkerPosition {
    let marker = classify_comment_marker(text);
    if preceded_by_non_whitespace(source_line, column) {
        MarkerPosition::Trailing(marker)
    } else {
        MarkerPosition::Standalone(marker)
    }
}

/// Returns `true` when the comment at `column` (1-based, pointing at `;`)
/// is preceded on `source_line` by at least one non-whitespace byte.
fn preceded_by_non_whitespace(
    source_line: &str,
    column: usize,
) -> bool {
    if column <= 1 {
        return false;
    }
    let prefix_end = column.saturating_sub(1).min(source_line.len());
    source_line
        .bytes()
        .take(prefix_end)
        .any(|byte| !byte.is_ascii_whitespace())
}

/// Look up the source line that contains `origin.line` (1-based) in
/// `source_text`. Returns the line without the trailing newline.
///
/// Returns `None` when the line is out of range.
#[must_use]
pub fn source_line_for<'a>(
    source_text: &'a str,
    origin: &SourceOrigin,
) -> Option<&'a str> {
    let zero_based = origin.line.checked_sub(1)?;
    source_text.lines().nth(zero_based)
}

/// Returns `true` when `text` carries one of the special CDDL comment
/// markers (`;!`, `;@`, or `;#`) and the comment is positioned after
/// non-whitespace CDDL source on the same source line.
///
/// Such comments are recognized only as ordinary source comments and
/// must never bind documentation or apply CBORK/include directives.
#[must_use]
pub fn is_trailing_marker_comment(
    text: &str,
    origin: &SourceOrigin,
    source_text: &str,
) -> bool {
    let Some(source_line) = source_line_for(source_text, origin) else {
        return false;
    };
    matches!(
        classify_comment_position(text, source_line, origin.column),
        MarkerPosition::Trailing(Some(_))
    )
}

/// Emit a W030 marker-misuse warning for each comment whose special
/// marker appears as a trailing comment.
///
/// This detection runs as part of the normal CDDL lint pass and does not
/// require `--doc`. A trailing `;@` or `;#` would otherwise silently
/// fail to apply the directive, so the warning is always worth emitting.
pub fn detect_marker_misuse(
    compiled: &mut CompiledCDDL,
    source_text: &str,
) {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    collect_marker_misuse(&compiled.user_nodes, source_text, &mut diagnostics);
    compiled.warnings.extend(diagnostics);
}

/// Walk the wrapped AST and collect spacing diagnostics for standalone
/// `;!` documentation comment lines whose marker is not followed by a
/// single space.
///
/// A standalone `;!` line of the form:
///
/// ```text
/// ;! Title
/// ```
///
/// is the canonical CDDL documentation-comment form. The marker
/// itself is a two-byte sequence (`;` then `!`), and the text after it
/// must begin with at least one space so downstream tooling can split
/// the marker from the body. A line where the marker appears alone,
/// or where only whitespace follows the marker, is valid and represents
/// a blank line inside the documentation block.
///
/// Trailing `;!` comments (`rule = 1  ;! comment`) are intentionally
/// excluded: those belong to the W030 marker-misuse path and are not
/// documentation comments.
///
/// Returns the diagnostics in source order. The caller decides where
/// these diagnostics land; the check only fires under `--doc`.
#[must_use]
pub fn collect_marker_spacing_issues(
    nodes: &[WrappedNode],
    source_text: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    walk_marker_spacing(nodes, source_text, &mut diagnostics);
    diagnostics
}

/// Walk every comment in `nodes` and run [`doc_marker_spacing_diagnostic`]
/// on each comment that looks like a documentation marker.
fn walk_marker_spacing(
    nodes: &[WrappedNode],
    source_text: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            WrappedNode::Comment {
                text, span, origin, ..
            } => {
                if let Some(diagnostic) =
                    doc_marker_spacing_diagnostic(text, origin, source_text, span.clone())
                {
                    diagnostics.push(diagnostic);
                }
            },
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                walk_marker_spacing(children, source_text, diagnostics);
            },
            WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Build a [`Diagnostic`] for a standalone `;!` line whose marker is
/// followed immediately by a non-whitespace byte. Returns `None` for
/// regular `;` / `;@` / `;#` comments, for trailing `;!` comments
/// (which belong to the W030 path), for the bare-marker `;!` form, and
/// for any line whose marker is followed by at least one whitespace
/// byte.
fn doc_marker_spacing_diagnostic(
    text: &str,
    origin: &SourceOrigin,
    source_text: &str,
    span: Range<usize>,
) -> Option<Diagnostic> {
    if classify_comment_marker(text) != Some(CommentMarker::Documentation) {
        return None;
    }
    let source_line = source_line_for(source_text, origin)?;
    // Trailing `;!` comments are W030 territory; the spacing rule does
    // not apply.  When the comment is preceded by non-whitespace CDDL
    // source on the same line, the marker is a regular comment marker.
    if preceded_by_non_whitespace(source_line, origin.column) {
        return None;
    }
    // Skip past the two marker bytes (`;` and `!`). If there is no
    // third byte or the third byte is whitespace, the marker is on its
    // own (blank-line) form, which is valid.
    let mut chars = text.chars();
    let _semi = chars.next()?;
    let _bang = chars.next()?;
    match chars.next() {
        None => None,
        Some(next_char) if next_char.is_whitespace() => None,
        Some(next_char) => {
            Some(Diagnostic {
                code: "W036",
                level: DiagnosticLevel::Warning,
                message: format!(
                    "documentation marker `;!` at {}:{}:{} is not followed by a space; \
                 insert a single space between `;!` and the comment text (`;!{next_char}` \
                 must read `;! {next_char}`) so the doc-block transform can split the \
                 marker from the body",
                    origin.source_path.display(),
                    origin.line,
                    origin.column,
                ),
                source_file: Some(origin.source_path.clone()),
                span: Some(span),
                previous_origin: None,
                related: Vec::new(),
            })
        },
    }
}

/// Walk the wrapped AST, collecting marker-misuse warnings for any
/// comment that is not at the start of its line.
pub(crate) fn collect_marker_misuse(
    nodes: &[WrappedNode],
    source_text: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            WrappedNode::Comment {
                text, span, origin, ..
            } => {
                if let Some(diagnostic) =
                    comment_marker_misuse_diagnostic(text, origin, source_text, span.clone())
                {
                    diagnostics.push(diagnostic);
                }
            },
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                collect_marker_misuse(children, source_text, diagnostics);
            },
            WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Build a marker-misuse diagnostic for a single comment if applicable.
fn comment_marker_misuse_diagnostic(
    text: &str,
    origin: &SourceOrigin,
    source_text: &str,
    span: Range<usize>,
) -> Option<Diagnostic> {
    let marker = classify_comment_marker(text)?;
    let source_line = source_line_for(source_text, origin)?;
    if !preceded_by_non_whitespace(source_line, origin.column) {
        return None;
    }
    Some(Diagnostic {
        code: "W030",
        level: DiagnosticLevel::Warning,
        message: format!(
            "special comment marker `{}` used as a trailing comment at {}:{}:{}; \
             it is treated as an ordinary CDDL comment, not as {}; \
             move the marker to its own line so it can bind documentation, \
             apply a CBORK directive, or apply an include/import directive",
            marker.as_str(),
            origin.source_path.display(),
            origin.line,
            origin.column,
            marker.description(),
        ),
        source_file: Some(origin.source_path.clone()),
        span: Some(span),
        previous_origin: None,
        related: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_marker_documentation() {
        assert_eq!(
            classify_comment_marker(";! # Title"),
            Some(CommentMarker::Documentation)
        );
    }

    #[test]
    fn classify_marker_cbork_directive() {
        assert_eq!(
            classify_comment_marker(";@ CBORK: Library"),
            Some(CommentMarker::CborkDirective)
        );
    }

    #[test]
    fn classify_marker_include_directive() {
        assert_eq!(
            classify_comment_marker(";# include \"./foo.cddl\""),
            Some(CommentMarker::IncludeDirective)
        );
    }

    #[test]
    fn classify_marker_regular_comment_is_none() {
        assert_eq!(classify_comment_marker(" ordinary comment"), None);
    }

    #[test]
    fn classify_marker_unrelated_text_is_none() {
        assert_eq!(classify_comment_marker("hello world"), None);
    }

    #[test]
    fn classify_position_standalone_marker() {
        let line = ";! # File title";
        assert_eq!(
            classify_comment_position(";! # File title", line, 1),
            MarkerPosition::Standalone(Some(CommentMarker::Documentation))
        );
    }

    #[test]
    fn classify_position_standalone_after_indent() {
        let line = "    ;! indented marker";
        assert_eq!(
            classify_comment_position(";! indented marker", line, 5),
            MarkerPosition::Standalone(Some(CommentMarker::Documentation))
        );
    }

    #[test]
    fn classify_position_trailing_marker() {
        let line = "rule = 1  ;! trailing marker";
        assert_eq!(
            classify_comment_position(";! trailing marker", line, 12),
            MarkerPosition::Trailing(Some(CommentMarker::Documentation))
        );
    }

    #[test]
    fn classify_position_trailing_regular_is_trailing_without_marker() {
        let line = "rule = 1  ; ordinary trailing comment";
        assert_eq!(
            classify_comment_position("; ordinary trailing comment", line, 12),
            MarkerPosition::Trailing(None)
        );
    }

    #[test]
    fn classify_position_standalone_regular_is_standalone_without_marker() {
        let line = "; ordinary standalone comment";
        assert_eq!(
            classify_comment_position("; ordinary standalone comment", line, 1),
            MarkerPosition::Standalone(None)
        );
    }

    #[test]
    fn preceded_by_non_whitespace_detects_after_text() {
        assert!(preceded_by_non_whitespace("rule = 1  ;! x", 12));
    }

    #[test]
    fn preceded_by_non_whitespace_false_for_indent() {
        assert!(!preceded_by_non_whitespace("    ;! x", 5));
    }

    #[test]
    fn preceded_by_non_whitespace_false_for_column_one() {
        assert!(!preceded_by_non_whitespace(";! x", 1));
    }

    #[test]
    fn source_line_for_returns_correct_line() {
        let source = "first line\n;! second\nthird line\n";
        let origin = SourceOrigin::new("test.cddl".into(), 2, 1);
        assert_eq!(source_line_for(source, &origin), Some(";! second"));
    }

    #[test]
    fn source_line_for_returns_none_for_out_of_range() {
        let source = "only one line\n";
        let origin = SourceOrigin::new("test.cddl".into(), 5, 1);
        assert_eq!(source_line_for(source, &origin), None);
    }

    #[test]
    fn spacing_warns_when_dash_follows_doc_marker() {
        let source = ";!-Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        let diag = doc_marker_spacing_diagnostic(";!-Title", &origin, source, 0..3).expect("diag");
        assert_eq!(diag.code, "W036");
        assert!(diag.message.contains("`;!-`"), "got: {}", diag.message);
    }

    #[test]
    fn spacing_warns_when_letter_follows_doc_marker() {
        let source = ";!Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        let diag = doc_marker_spacing_diagnostic(";!Title", &origin, source, 0..6).expect("diag");
        assert_eq!(diag.code, "W036");
        assert!(
            diag.message.contains("`;!T`"),
            "diagnostic message should include the bad form, got: {diag:?}"
        );
    }

    #[test]
    fn spacing_accepts_canonical_space_form() {
        let source = ";! # Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        assert!(doc_marker_spacing_diagnostic(";! # Title", &origin, source, 0..9).is_none());
    }

    #[test]
    fn spacing_accepts_marker_only_line() {
        let source = ";!\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        assert!(doc_marker_spacing_diagnostic(";!", &origin, source, 0..2).is_none());
    }

    #[test]
    fn spacing_accepts_marker_with_trailing_whitespace() {
        let source = ";!   \n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        assert!(doc_marker_spacing_diagnostic(";!   ", &origin, source, 0..5).is_none());
    }

    #[test]
    fn spacing_accepts_multiple_spaces_after_marker() {
        let source = ";!    Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        assert!(
            doc_marker_spacing_diagnostic(";!    Title", &origin, source, 0..11).is_none(),
            "any whitespace count is valid"
        );
    }

    #[test]
    fn spacing_skips_trailing_doc_comments() {
        // A trailing `;!X` belongs to the W030 path; the W036 spacing
        // rule is only for standalone marker lines.
        let source = "rule = 1  ;!Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 12);
        assert!(
            doc_marker_spacing_diagnostic(";!Title", &origin, source, 11..18).is_none(),
            "trailing doc comments must not trigger W036"
        );
    }

    #[test]
    fn spacing_skips_indented_standalone_doc_marker() {
        let source = "    ;!Title\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 5);
        // The column 5 marker is preceded only by whitespace, so it
        // counts as standalone; the missing space still warns.
        let diag = doc_marker_spacing_diagnostic(";!Title", &origin, source, 4..10).expect("diag");
        assert_eq!(diag.code, "W036");
    }

    #[test]
    fn spacing_skips_non_doc_markers() {
        let source = ";@CBORK: Library\n";
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        assert!(
            doc_marker_spacing_diagnostic(";@CBORK: Library", &origin, source, 0..17).is_none(),
            "only `;!` is checked; other markers are out of scope"
        );
    }

    #[test]
    fn collect_marker_spacing_walks_nested_children() {
        // Build a synthetic AST tree by hand so we do not need a real
        // CDDL compile pass to exercise the walker.
        let source = "rule = 1\n  ;!Body\n";
        let origin = SourceOrigin::new("test.cddl".into(), 2, 3);
        let inner = WrappedNode::Comment {
            text: ";!Body".into(),
            span: 11..16,
            origin: origin.clone(),
            metadata: Vec::new(),
        };
        let rule = WrappedNode::RuleLine {
            text: "rule = 1".into(),
            span: 0..8,
            origin: SourceOrigin::new("test.cddl".into(), 1, 1),
            children: vec![inner],
            metadata: Vec::new(),
        };
        let diagnostics = collect_marker_spacing_issues(&[rule], source);
        assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
        assert_eq!(diagnostics[0].code, "W036");
    }
}
