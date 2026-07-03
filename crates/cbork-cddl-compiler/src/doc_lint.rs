// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Documentation linting pipeline (optional `--doc` pass).
//!
//! This module is the front end of the optional documentation linting
//! pass described in `crates/cbork/plan.md` § *Optional documentation
//! linting*. It glues together the three remaining pipeline steps:
//!
//! * **Step 6** — Transform safety validation. Reject doc comments that contain the
//!   reserved `CBORK CDDL FROM` marker prefix or that open a multiline HTML comment
//!   (`<!--`) without a matching `-->`. These would either collide with a generated
//!   splice marker or be swallowed by a generated splice marker in the synthetic
//!   Markdown.
//! * **Step 7** — `rumdl` integration. Load the user's `.rumdl.toml` via `rumdl`'s
//!   configuration discovery API, get the configured rule set, and run `rumdl` against
//!   the synthetic Markdown produced by [`transform_to_markdown`].
//! * **Step 8** — Diagnostic mapping. Translate each `rumdl` [`LintWarning`] back to a
//!   [`Diagnostic`] anchored on the original CDDL source: warnings on doc lines keep the
//!   original line plus a column offset for the stripped `;!` prefix; warnings on
//!   generated splice markers and wrapper blank lines are suppressed because they cannot
//!   represent a real user-facing issue.
//!
//! [`LintWarning`]: ::rumdl_lib::rule::LintWarning

use std::path::Path;

use ::rumdl_lib::{
    config::{Config, MarkdownFlavor, SourcedConfig},
    lint as rumdl_lint,
    rule::LintWarning,
    rules::all_rules,
};

use crate::{
    Diagnostic, DiagnosticLevel,
    doc_block::{DocBlock, scan_doc_blocks},
    transform::{SPLICE_MARKER_PREFIX, SyntheticLineKind, SyntheticMarkdown},
};

/// Reserved error codes emitted by the doc-lint pipeline.
const CODE_RESERVED_MARKER: &str = "E040";

/// Error code emitted when a doc block opens more `<!--` HTML
/// comments than it closes.
const CODE_UNCLOSED_HTML_COMMENT: &str = "E041";

/// Default `MarkdownFlavor` for cbork's documentation linting. The
/// CDDL doc-block transform does not need MkDocs-specific handling;
/// standard `CommonMark` is the right baseline.
const DEFAULT_RUMDL_FLAVOR: MarkdownFlavor = MarkdownFlavor::Standard;

// ---------------------------------------------------------------------
// Step 6: transform safety validation
// ---------------------------------------------------------------------

/// Result of [`validate_doc_source`]: the diagnostics emitted by the
/// safety check, in source order. The diagnostics are `Error`-level
/// because they describe a doc block that the transform cannot
/// faithfully round-trip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafetyReport {
    /// Diagnostics found in the source. Empty when the source is safe
    /// to transform.
    pub diagnostics: Vec<Diagnostic>,
}

impl SafetyReport {
    /// Returns `true` when the source is safe to transform.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Validate the captured pre-transform CDDL source for the two
/// safety conditions that the synthetic-Markdown transform cannot
/// recover from:
///
/// 1. A doc block whose concatenated text contains the reserved `CBORK CDDL FROM` marker
///    prefix. After the transform, this substring would collide with a generated splice
///    marker and the reverse transform would misinterpret the user's literal text as a
///    splice marker to be expanded.
/// 2. A doc block that opens an HTML comment (`<!--`) without a matching `-->`. Such a
///    comment would swallow a generated splice marker that the transform inserts after
///    the doc block.
#[must_use]
pub fn validate_doc_source(source_text: &str) -> SafetyReport {
    let scan = scan_doc_blocks(source_text);
    let mut diagnostics = Vec::new();

    for (block, _binding) in scan.iter() {
        if let Some(diag) = reserved_marker_diagnostic(block) {
            diagnostics.push(diag);
        }
        if let Some(diag) = unclosed_html_comment_diagnostic(block) {
            diagnostics.push(diag);
        }
    }

    SafetyReport { diagnostics }
}

/// Emit a diagnostic for a doc block that contains the reserved
/// `CBORK CDDL FROM` marker prefix.
fn reserved_marker_diagnostic(block: &DocBlock) -> Option<Diagnostic> {
    let joined: String = block
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    if !joined.contains(SPLICE_MARKER_PREFIX) {
        return None;
    }

    let span_start = block.start_line.saturating_sub(1);
    let span_end = block.end_line;
    Some(Diagnostic {
        code: CODE_RESERVED_MARKER,
        level: DiagnosticLevel::Error,
        message: format!(
            "documentation comment contains the reserved splice-marker prefix `{SPLICE_MARKER_PREFIX}`; \
             this would collide with a generated `<!-- CBORK CDDL FROM start-end -->` \
             marker, so the doc-lint transform refuses to run"
        ),
        source_file: None,
        span: Some(span_start..span_end),
        previous_origin: None,
        related: Vec::new(),
    })
}

/// Emit a diagnostic for a doc block that opens an HTML comment
/// without a matching `-->`. The transform would emit a generated
/// splice marker on the next non-doc line, and that marker would be
/// swallowed by the still-open HTML comment.
fn unclosed_html_comment_diagnostic(block: &DocBlock) -> Option<Diagnostic> {
    let joined: String = block
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let opens = joined.matches("<!--").count();
    let closes = joined.matches("-->").count();
    if opens <= closes {
        return None;
    }

    let span_start = block.start_line.saturating_sub(1);
    let span_end = block.end_line;
    Some(Diagnostic {
        code: CODE_UNCLOSED_HTML_COMMENT,
        level: DiagnosticLevel::Error,
        message: format!(
            "documentation comment opens {opens} HTML comment(s) but closes only \
             {closes}; an unclosed `<!-- ... -->` would swallow a generated splice \
             marker, so the doc-lint transform refuses to run"
        ),
        source_file: None,
        span: Some(span_start..span_end),
        previous_origin: None,
        related: Vec::new(),
    })
}

// ---------------------------------------------------------------------
// Step 7: rumdl integration
// ---------------------------------------------------------------------

/// Outcome of running `rumdl` against the synthetic Markdown.
#[derive(Debug, Clone)]
pub struct RumdlRun {
    /// `rumdl` warnings, in source order.
    pub warnings: Vec<LintWarning>,
    /// Optional `rumdl` config that was actually used (after
    /// discovery and validation). Useful for debugging.
    pub config_loaded: bool,
}

/// Error returned by [`lint_synthetic_markdown`] when the
/// `rumdl` pipeline fails (config errors, file-system errors, or
/// `rumdl` internal errors).
#[derive(Debug)]
pub struct RumdlError {
    /// Human-readable error message.
    pub message: String,
}

impl std::fmt::Display for RumdlError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RumdlError {}

/// Run `rumdl` against the synthetic Markdown produced by
/// [`transform_to_markdown`].
///
/// `config_path` is an optional explicit path to a `.rumdl.toml`
/// file. When `None`, cbork walks up from the directory containing
/// `source_path` looking for the nearest rumdl config (one of
/// `.rumdl.toml`, `rumdl.toml`, `.config/rumdl.toml`, or a
/// `[tool.rumdl]` section in `pyproject.toml`). If none is found,
/// cbork falls back to `rumdl`'s built-in discovery, which walks up
/// from the process current directory.
///
/// `source_path` is the CDDL file the synthetic Markdown was built
/// from; `rumdl` uses it for the file paths in cross-file rule
/// diagnostics.
///
/// # Errors
///
/// Returns [`RumdlError`] when the `rumdl` configuration cannot be
/// loaded, the config fails validation, or `rumdl` itself returns an
/// internal error during the lint pass.
pub fn lint_synthetic_markdown(
    synthetic: &SyntheticMarkdown,
    source_path: &Path,
    config_path: Option<&str>,
) -> Result<RumdlRun, RumdlError> {
    let (effective_config_path, config_loaded) =
        resolve_rumdl_config_path(config_path, source_path);
    let config = load_rumdl_config(effective_config_path.as_ref().and_then(|p| p.to_str()))?;
    // Apply the config's `disable` list via `filter_rules` so fixture-local
    // `.rumdl.toml` configs that disable noisy style rules actually take
    // effect. The lower-level `rumdl_lib::lint` does not filter rules
    // itself; it only uses `disable` for inline-config suppression.
    let all = all_rules(&config);
    let rules = ::rumdl_lib::rules::filter_rules(&all, &config.global);
    let warnings = rumdl_lint(
        &synthetic.text,
        &rules,
        false,
        DEFAULT_RUMDL_FLAVOR,
        Some(source_path.to_path_buf()),
        Some(&config),
    )
    .map_err(|e| {
        RumdlError {
            message: format!("rumdl lint failed: {e}"),
        }
    })?;
    Ok(RumdlRun {
        warnings,
        config_loaded,
    })
}

/// Resolve the rumdl config path that should be used for the lint.
///
/// Priority:
///
/// 1. The explicit `config_path` argument wins if it is `Some`.
/// 2. Otherwise, walk up from the directory containing `source_path` looking for the
///    first directory that has a `rumdl` config file in it. The walk stops at the first
///    hit so the file-local `.rumdl.toml` overrides any repository-level config above it.
/// 3. If neither of the above produces a hit, return `None` and let `rumdl` discover
///    config from the process current directory.
///
/// The returned tuple is `(Option<PathBuf>, bool)` where the boolean
/// reports whether *any* config was found (either explicit or
/// discovered). The boolean is exposed in [`RumdlRun::config_loaded`]
/// for `--why` output and tests.
fn resolve_rumdl_config_path(
    config_path: Option<&str>,
    source_path: &Path,
) -> (Option<std::path::PathBuf>, bool) {
    if let Some(path) = config_path {
        return (Some(std::path::PathBuf::from(path)), true);
    }
    if let Some(start) = source_path.parent()
        && let Some(found) = walk_up_for_rumdl_config(start)
    {
        return (Some(found), true);
    }
    (None, false)
}

/// Walk up from `start` looking for the first directory that contains
/// one of the rumdl-native config files. The walk stops at the first
/// hit so the nearest file-local config wins, and stops at the file
/// system root so it cannot escape into unrelated parent directories.
fn walk_up_for_rumdl_config(start: &Path) -> Option<std::path::PathBuf> {
    const RUMDL_CONFIG_FILES: &[&str] = &[
        ".rumdl.toml",
        "rumdl.toml",
        ".config/rumdl.toml",
        "pyproject.toml",
    ];

    let mut current = Some(start.to_path_buf());
    while let Some(dir) = current {
        for name in RUMDL_CONFIG_FILES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                if *name == "pyproject.toml" && !pyproject_declares_rumdl(&candidate) {
                    continue;
                }
                return Some(candidate);
            }
        }
        current = dir.parent().map(std::path::Path::to_path_buf);
    }
    None
}

/// Return `true` when `path` (expected to be a `pyproject.toml`)
/// declares a `[tool.rumdl]` section. A `pyproject.toml` that does
/// not declare rumdl config is *not* a rumdl config file even if it
/// lives in the same directory as the source.
fn pyproject_declares_rumdl(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|content| content.contains("[tool.rumdl]"))
}

/// Load and validate a `rumdl` configuration. When `config_path` is
/// `None`, `rumdl`'s discovery API walks the tree from the process
/// current directory looking for `.rumdl.toml`.
fn load_rumdl_config(config_path: Option<&str>) -> Result<Config, RumdlError> {
    let sourced = SourcedConfig::load_with_discovery(config_path, None, config_path.is_none())
        .map_err(|e| {
            RumdlError {
                message: format!("failed to load rumdl config: {e}"),
            }
        })?;

    let registry = ::rumdl_lib::config::default_registry();
    let validated = sourced.validate(registry).map_err(|e| {
        RumdlError {
            message: format!("invalid rumdl config: {e}"),
        }
    })?;
    let _validation_warnings = &validated.validation_warnings;
    Ok(Config::from(validated))
}

/// Apply `rumdl` auto-fixes to the synthetic Markdown in memory.
///
/// The fixed Markdown still needs to be reverse-transformed by the
/// caller; this function only runs `rumdl`'s fix pass.
///
/// # Errors
///
/// Returns [`RumdlError`] when `rumdl`'s fix-apply helper fails (for
/// example, when a fix spans an invalid offset in the synthetic
/// Markdown).
pub fn apply_rumdl_fixes(
    synthetic: &SyntheticMarkdown,
    warnings: &[LintWarning],
) -> Result<String, RumdlError> {
    ::rumdl_lib::utils::fix_utils::apply_warning_fixes(&synthetic.text, warnings).map_err(|e| {
        RumdlError {
            message: format!("rumdl fix apply failed: {e}"),
        }
    })
}

// ---------------------------------------------------------------------
// Step 8: diagnostic mapping
// ---------------------------------------------------------------------

/// Outcome of [`map_rumdl_diagnostics`]: the `rumdl` warnings have
/// been translated to cbork [`Diagnostic`]s anchored on the
/// original CDDL source lines and columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MappedDiagnostics {
    /// Diagnostics that target user-authored CDDL source lines.
    pub diagnostics: Vec<Diagnostic>,
    /// `rumdl` warnings that landed on generated splice markers or
    /// wrapper blank lines and were suppressed. Useful for tests and
    /// for `--why` output that explains the missing diagnostic.
    pub suppressed: Vec<SuppressedWarning>,
}

/// A `rumdl` warning that did not translate to a CDDL diagnostic,
/// plus the reason it was suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressedWarning {
    /// 1-based line number in the synthetic Markdown.
    pub synthetic_line: usize,
    /// Why the warning was suppressed.
    pub reason: SuppressionReason,
}

/// Reason a `rumdl` warning was suppressed during mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    /// Warning landed on a `<!-- CBORK CDDL FROM ... -->` splice
    /// marker, which is generated by cbork and not user-authored.
    SpliceMarker,
    /// Warning landed on a generated blank line that wraps a splice
    /// marker. Internal transform bug if it represents a real issue.
    GeneratedBlank,
}

/// Map `rumdl` warnings onto CDDL source positions using the line map
/// recorded by [`transform_to_markdown`].
///
/// `source_path` is the file path the resulting diagnostics are
/// attributed to. `source_text` is the original CDDL text used to
/// compute the column offset for doc-line diagnostics.
#[must_use]
pub fn map_rumdl_diagnostics(
    warnings: Vec<LintWarning>,
    synthetic: &SyntheticMarkdown,
    source_text: &str,
    source_path: &Path,
) -> MappedDiagnostics {
    let source_lines = source_text.split('\n').collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    let mut suppressed = Vec::new();

    for warning in warnings {
        if let Some(diag) = map_one_warning(&warning, synthetic, &source_lines, source_path) {
            diagnostics.push(diag);
        } else {
            suppressed.push(build_suppressed(&warning, synthetic));
        }
    }

    MappedDiagnostics {
        diagnostics,
        suppressed,
    }
}

/// Map a single `rumdl` warning to a [`Diagnostic`] on the original
/// CDDL source, or return `None` when the warning points at a
/// generated splice marker or wrapper blank line.
fn map_one_warning(
    warning: &LintWarning,
    synthetic: &SyntheticMarkdown,
    source_lines: &[&str],
    source_path: &Path,
) -> Option<Diagnostic> {
    let line = synthetic.lines.get(warning.line.saturating_sub(1))?;
    match &line.kind {
        SyntheticLineKind::DocLine {
            source_line,
            source_column_offset,
        } => {
            Some(build_doc_line_diagnostic(
                warning,
                *source_line,
                *source_column_offset,
                source_lines,
                source_path,
            ))
        },
        SyntheticLineKind::SpliceMarker { .. } | SyntheticLineKind::GeneratedBlank => None,
    }
}

/// Build a [`Diagnostic`] anchored on the original CDDL source line
/// backing a `SyntheticLineKind::DocLine`.
fn build_doc_line_diagnostic(
    warning: &LintWarning,
    source_line: usize,
    source_column_offset: usize,
    source_lines: &[&str],
    source_path: &Path,
) -> Diagnostic {
    let column = warning.column.saturating_add(source_column_offset).max(1);
    let end_column = warning
        .end_column
        .saturating_add(source_column_offset)
        .max(column);

    let span_start_byte = line_column_to_byte_offset(source_lines, source_line, column);
    let span_end_byte = line_column_to_byte_offset(source_lines, source_line, end_column);

    Diagnostic {
        code: rule_code(warning),
        level: severity_to_level(warning.severity),
        message: warning.message.clone(),
        source_file: Some(source_path.to_path_buf()),
        span: Some(span_start_byte..span_end_byte),
        previous_origin: None,
        related: Vec::new(),
    }
}

/// Build a [`SuppressedWarning`] for a `rumdl` warning that landed
/// on a generated wrapper line in the synthetic Markdown.
fn build_suppressed(
    warning: &LintWarning,
    synthetic: &SyntheticMarkdown,
) -> SuppressedWarning {
    let line = synthetic.lines.get(warning.line.saturating_sub(1));
    let reason = match line.map(|l| &l.kind) {
        // A warning that points at a generated blank line wrapper
        // is reported as such; everything else (splice marker, or
        // a line beyond the synthetic output) is treated as the
        // splice-marker case.
        Some(SyntheticLineKind::GeneratedBlank) => SuppressionReason::GeneratedBlank,
        Some(SyntheticLineKind::SpliceMarker { .. } | SyntheticLineKind::DocLine { .. }) | None => {
            SuppressionReason::SpliceMarker
        },
    };
    SuppressedWarning {
        synthetic_line: warning.line,
        reason,
    }
}

/// Promote a `rumdl` rule name (e.g. `"MD041"`) into a `'static str`
/// suitable for cbork's [`Diagnostic::code`]. Leaks a small amount
/// of memory per warning; acceptable because diagnostics are
/// short-lived.
fn rule_code(warning: &LintWarning) -> &'static str {
    match warning.rule_name.as_deref() {
        Some(name) => Box::leak(name.to_owned().into_boxed_str()),
        None => "MD000",
    }
}

/// Map a `rumdl` [`Severity`] to cbork's [`DiagnosticLevel`]. `Info`
/// collapses to `Warning` because cbork has no `Info` tier.
fn severity_to_level(severity: ::rumdl_lib::rule::Severity) -> DiagnosticLevel {
    use ::rumdl_lib::rule::Severity;
    match severity {
        Severity::Error => DiagnosticLevel::Error,
        Severity::Warning | Severity::Info => DiagnosticLevel::Warning,
    }
}

/// Convert a 1-based (line, column) in the CDDL source into a byte
/// offset from the start of the file. Used to build diagnostic spans
/// that line up with the user's editor.
fn line_column_to_byte_offset(
    source_lines: &[&str],
    line: usize,
    column: usize,
) -> usize {
    let mut offset = 0usize;
    for (idx, text) in source_lines.iter().enumerate() {
        if idx.saturating_add(1) == line {
            // Columns are 1-based character offsets; convert to a
            // byte offset that respects UTF-8 boundaries.
            return offset.saturating_add(char_column_to_byte_offset(text, column));
        }
        offset = offset.saturating_add(text.len()).saturating_add(1);
    }
    offset
}

/// Convert a 1-based character column within `line` to a byte offset.
fn char_column_to_byte_offset(
    line: &str,
    column: usize,
) -> usize {
    if column == 0 {
        return 0;
    }
    for (char_idx, (byte_idx, _)) in line.char_indices().enumerate() {
        if char_idx.saturating_add(1) == column {
            return byte_idx;
        }
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_clean_source_returns_no_diagnostics() {
        let source = ";! # Title\nrule = 1\n";
        let report = validate_doc_source(source);
        assert!(report.is_clean());
    }

    #[test]
    fn validate_rejects_reserved_marker_prefix_in_doc_block() {
        let source = "\
;! # Title
;! CBORK CDDL FROM 99-99
rule = 1
";
        let report = validate_doc_source(source);
        assert!(!report.is_clean());
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].code, CODE_RESERVED_MARKER);
    }

    #[test]
    fn validate_rejects_unclosed_html_comment() {
        let source = "\
;! # Title
;! <!-- not closed
rule = 1
";
        let report = validate_doc_source(source);
        assert!(!report.is_clean());
        let diag = report
            .diagnostics
            .iter()
            .find(|d| d.code == CODE_UNCLOSED_HTML_COMMENT)
            .expect("unclosed HTML comment diagnostic");
        assert!(diag.message.contains("opens 1 HTML comment"));
    }

    #[test]
    fn validate_accepts_closed_html_comment() {
        let source = "\
;! <!-- closed -->
rule = 1
";
        let report = validate_doc_source(source);
        assert!(report.is_clean());
    }

    #[test]
    fn validate_accepts_multiline_balanced_html_comment() {
        let source = "\
;! <!-- start
;! middle
;! end -->
rule = 1
";
        let report = validate_doc_source(source);
        assert!(report.is_clean());
    }

    #[test]
    fn validate_rejects_two_unclosed_html_comments() {
        let source = "\
;! <!-- a
;! <!-- b
rule = 1
";
        let report = validate_doc_source(source);
        let diag = report
            .diagnostics
            .iter()
            .find(|d| d.code == CODE_UNCLOSED_HTML_COMMENT)
            .expect("unclosed HTML comment diagnostic");
        assert!(diag.message.contains("opens 2 HTML comment"));
    }

    #[test]
    fn reserved_prefix_detection_ignores_cddl_source_outside_doc() {
        // The literal `CBORK CDDL FROM` substring is reserved only
        // inside doc comments; a CDDL rule body that contains the
        // same text must not trigger the diagnostic.
        let source = "rule = 1 ; CBORK CDDL FROM 99-99\n";
        let report = validate_doc_source(source);
        assert!(report.is_clean());
    }

    #[test]
    fn char_column_to_byte_offset_handles_ascii() {
        assert_eq!(char_column_to_byte_offset("hello", 1), 0);
        assert_eq!(char_column_to_byte_offset("hello", 3), 2);
        assert_eq!(char_column_to_byte_offset("hello", 6), 5);
    }

    #[test]
    fn char_column_to_byte_offset_handles_utf8() {
        // This line has 5 chars but 6 bytes because the second char
        // is two bytes in UTF-8.
        let line = "h\u{00e9}llo";
        assert_eq!(char_column_to_byte_offset(line, 1), 0);
        assert_eq!(char_column_to_byte_offset(line, 2), 1);
        assert_eq!(char_column_to_byte_offset(line, 3), 3);
    }

    #[test]
    fn char_column_to_byte_offset_handles_out_of_range() {
        assert_eq!(char_column_to_byte_offset("hello", 100), 5);
        assert_eq!(char_column_to_byte_offset("hello", 0), 0);
    }

    #[test]
    fn map_diagnostics_suppresses_splice_marker_warnings() {
        let source = "rule = 1\n";
        let synthetic = crate::transform_to_markdown(source);
        let warning = LintWarning {
            message: "test".into(),
            line: synthetic
                .lines
                .iter()
                .position(|l| matches!(l.kind, SyntheticLineKind::SpliceMarker { .. }))
                .map_or(1, |i| i + 1),
            column: 1,
            end_line: 1,
            end_column: 5,
            severity: ::rumdl_lib::rule::Severity::Warning,
            fix: None,
            rule_name: Some("MD000".into()),
        };

        let mapped = map_rumdl_diagnostics(vec![warning], &synthetic, source, Path::new("x.cddl"));
        assert!(mapped.diagnostics.is_empty());
        assert_eq!(mapped.suppressed.len(), 1);
        assert_eq!(mapped.suppressed[0].reason, SuppressionReason::SpliceMarker);
    }

    #[test]
    fn map_diagnostics_suppresses_generated_blank_warnings() {
        let source = "rule = 1\n";
        let synthetic = crate::transform_to_markdown(source);
        let warning = LintWarning {
            message: "test".into(),
            line: 1, // the blank line above the splice marker
            column: 1,
            end_line: 1,
            end_column: 1,
            severity: ::rumdl_lib::rule::Severity::Warning,
            fix: None,
            rule_name: Some("MD000".into()),
        };

        let mapped = map_rumdl_diagnostics(vec![warning], &synthetic, source, Path::new("x.cddl"));
        assert!(mapped.diagnostics.is_empty());
        assert_eq!(mapped.suppressed.len(), 1);
        assert_eq!(
            mapped.suppressed[0].reason,
            SuppressionReason::GeneratedBlank
        );
    }

    #[test]
    fn map_diagnostics_keeps_doc_line_warnings_with_column_offset() {
        let source = "    ;! # field docs\nrule = 1\n";
        let synthetic = crate::transform_to_markdown(source);
        let doc_line = synthetic
            .lines
            .iter()
            .find_map(|l| {
                match &l.kind {
                    SyntheticLineKind::DocLine { source_line, .. } => Some(*source_line),
                    _ => None,
                }
            })
            .expect("synthetic has a doc line");

        let warning = LintWarning {
            message: "MD test".into(),
            line: synthetic
                .lines
                .iter()
                .position(|l| matches!(l.kind, SyntheticLineKind::DocLine { .. }))
                .map_or(1, |i| i + 1),
            column: 2,
            end_line: 1,
            end_column: 4,
            severity: ::rumdl_lib::rule::Severity::Warning,
            fix: None,
            rule_name: Some("MD041".into()),
        };

        let mapped = map_rumdl_diagnostics(vec![warning], &synthetic, source, Path::new("x.cddl"));

        assert_eq!(mapped.diagnostics.len(), 1);
        let diag = &mapped.diagnostics[0];
        assert_eq!(diag.code, "MD041");
        assert_eq!(diag.message, "MD test");
        // Original line is `    ;! # field docs` with 4 leading
        // spaces. The transform also removes the common single space
        // after `;!`, so synthetic column 2 must map to original
        // column 2 + 4 (leading ws) + 2 (`;!`) + 1 (dedent) = 9.
        let source_lines: Vec<_> = source.lines().collect();
        let expected_start = line_column_to_byte_offset(&source_lines, doc_line, 9);
        let expected_end = line_column_to_byte_offset(&source_lines, doc_line, 11);
        assert_eq!(diag.span, Some(expected_start..expected_end));
        assert!(diag.previous_origin.is_none());
    }

    #[test]
    fn severity_to_level_maps_error_to_error() {
        assert_eq!(
            severity_to_level(::rumdl_lib::rule::Severity::Error),
            DiagnosticLevel::Error
        );
        assert_eq!(
            severity_to_level(::rumdl_lib::rule::Severity::Warning),
            DiagnosticLevel::Warning
        );
        assert_eq!(
            severity_to_level(::rumdl_lib::rule::Severity::Info),
            DiagnosticLevel::Warning
        );
    }

    // -----------------------------------------------------------------
    // Config discovery tests.
    //
    // These tests prove that `resolve_rumdl_config_path` walks up
    // from the CDDL source file's directory looking for a rumdl
    // config, instead of falling back to rumdl's CWD-based discovery.
    // The plan requires this so that a fixture-local `.rumdl.toml`
    // controls the lint pass without depending on the directory the
    // user happened to run `cbork lint` from.

    use std::fs;

    /// Build a temporary directory tree shaped like
    /// `<root>/<chain>/<leaf>/<file>` plus an optional `.rumdl.toml`
    /// placed in one of the ancestors. Returns the directory tree root
    /// and the path to the leaf file.
    fn build_tree(layout: &[(&str, Option<&str>)]) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "cbork_rumdl_discovery_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&root).expect("create temp root");
        let mut chain: std::path::PathBuf = root.clone();
        for (rel, content) in layout {
            chain = chain.join(rel);
            fs::create_dir_all(&chain).expect("create intermediate dir");
            if let Some(body) = content {
                fs::write(chain.join(".rumdl.toml"), body).expect("write .rumdl.toml");
            }
        }
        let file = chain.join("fixture.cddl");
        fs::write(&file, ";! # Title\nrule = 1\n").expect("write fixture");
        (root, file)
    }

    #[test]
    fn walk_up_finds_rumdl_config_in_same_directory() {
        let body = "# rumdl config for the walk-up test\n";
        let (root, file) = build_tree(&[("a/b/c", Some(body))]);
        let found = walk_up_for_rumdl_config(file.parent().unwrap());
        let _dropped = fs::remove_dir_all(&root);
        assert_eq!(
            found.unwrap(),
            file.parent().unwrap().join(".rumdl.toml"),
            "walk-up must find the file-local .rumdl.toml"
        );
    }

    #[test]
    fn walk_up_finds_nearest_rumdl_config() {
        // Two `.rumdl.toml` files: the deeper one wins because the
        // walk stops at the first hit.
        let outer = "# outer\n";
        let inner = "# inner\n";
        let (root, file) = build_tree(&[("proj", Some(outer)), ("proj/sub/dir", Some(inner))]);
        let found = walk_up_for_rumdl_config(file.parent().unwrap());
        let _dropped = fs::remove_dir_all(&root);
        assert_eq!(
            found.unwrap(),
            file.parent().unwrap().join(".rumdl.toml"),
            "the nearest .rumdl.toml must win over a parent one"
        );
    }

    #[test]
    fn walk_up_returns_none_when_no_rumdl_config_exists() {
        let (root, file) = build_tree(&[("only/deep/path", None)]);
        let found = walk_up_for_rumdl_config(file.parent().unwrap());
        let _dropped = fs::remove_dir_all(&root);
        assert!(
            found.is_none(),
            "walk-up must return None when no .rumdl.toml exists"
        );
    }

    #[test]
    fn pyproject_without_rumdl_section_is_ignored() {
        let (root, file) = build_tree(&[("proj", Some("# no tool.rumdl here\n"))]);
        // Rename the file to pyproject.toml so the walk hits it.
        let pyproject = file.parent().unwrap().join(".rumdl.toml");
        fs::rename(&pyproject, file.parent().unwrap().join("pyproject.toml")).unwrap();
        let found = walk_up_for_rumdl_config(file.parent().unwrap());
        let _dropped = fs::remove_dir_all(&root);
        assert!(
            found.is_none(),
            "a pyproject.toml without a `[tool.rumdl]` section must not count as a rumdl config"
        );
    }

    #[test]
    fn pyproject_with_tool_rumdl_section_is_accepted() {
        let (root, file) = build_tree(&[("proj", Some("[tool.rumdl]\nenable = true\n"))]);
        let pyproject = file.parent().unwrap().join(".rumdl.toml");
        fs::rename(&pyproject, file.parent().unwrap().join("pyproject.toml")).unwrap();
        let found = walk_up_for_rumdl_config(file.parent().unwrap());
        let _dropped = fs::remove_dir_all(&root);
        assert_eq!(
            found.unwrap(),
            file.parent().unwrap().join("pyproject.toml"),
            "a pyproject.toml with `[tool.rumdl]` must be accepted"
        );
    }

    #[test]
    fn resolve_rumdl_config_path_prefers_explicit_path() {
        let body = "# explicit\n";
        let (root, file) = build_tree(&[("a", Some(body))]);
        let explicit = file.parent().unwrap().join(".rumdl.toml");
        let (resolved, loaded) = resolve_rumdl_config_path(Some(explicit.to_str().unwrap()), &file);
        let _dropped = fs::remove_dir_all(&root);
        assert_eq!(resolved.as_deref(), Some(explicit.as_path()));
        assert!(loaded, "explicit path counts as loaded");
    }

    #[test]
    fn resolve_rumdl_config_path_walks_up_from_source_directory() {
        let body = "# fixture-local\n";
        let (root, file) = build_tree(&[("a/b/c", Some(body))]);
        let (resolved, loaded) = resolve_rumdl_config_path(None, &file);
        let _dropped = fs::remove_dir_all(&root);
        assert_eq!(
            resolved.as_deref(),
            Some(file.parent().unwrap().join(".rumdl.toml").as_path()),
            "walk-up from the CDDL file's directory must surface a file-local .rumdl.toml"
        );
        assert!(loaded);
    }

    #[test]
    fn resolve_rumdl_config_path_returns_none_when_no_config_anywhere() {
        let (root, file) = build_tree(&[("a/b/c", None)]);
        let (resolved, loaded) = resolve_rumdl_config_path(None, &file);
        let _dropped = fs::remove_dir_all(&root);
        assert!(
            resolved.is_none(),
            "no walk-up config must hand back None so rumdl falls back to its own discovery"
        );
        assert!(!loaded);
    }

    #[test]
    fn fixture_local_rumdl_config_controls_lint_pass() {
        // Build a fixture tree that contains a `.rumdl.toml` which
        // disables the noisy MD013 (line-length) rule. A fixture
        // whose synthetic Markdown would normally trigger MD013
        // must produce zero MD013 warnings when the fixture-local
        // config is in effect. This is the regression test the plan
        // calls out as required for Step 7.
        let root = std::env::temp_dir().join(format!(
            "cbork_rumdl_fixture_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let dir = root.join("project");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(".rumdl.toml"), "[lint]\ndisable = [\"MD013\"]\n")
            .expect("write .rumdl.toml");
        // A doc block with one very long line should trigger MD013
        // under the default config but be silenced by the
        // fixture-local override.
        let cddl_path = dir.join("wide.cddl");
        let long_line = ";! ".to_owned() + &"x".repeat(200);
        fs::write(&cddl_path, format!("{long_line}\nrule = 1\n")).expect("write fixture");

        let synthetic = crate::transform_to_markdown(&fs::read_to_string(&cddl_path).unwrap());
        let rumdl_run = lint_synthetic_markdown(&synthetic, &cddl_path, None)
            .expect("rumdl must run on the fixture");
        let md013: Vec<_> = rumdl_run
            .warnings
            .iter()
            .filter(|w| w.rule_name.as_deref() == Some("MD013"))
            .collect();
        let _dropped = fs::remove_dir_all(&root);
        assert!(
            md013.is_empty(),
            "fixture-local .rumdl.toml must silence MD013, got: {md013:?}"
        );
        assert!(
            rumdl_run.config_loaded,
            "config_loaded must report true when a config is found"
        );
    }
}
