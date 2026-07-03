// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Diagnostic rendering helpers for the `cbork` CLI.

use std::{fmt::Write as _, ops::Range, path::Path};

use cbork_cddl_compiler::{Diagnostic, DiagnosticLevel, SubdiagKind};
use console::{Emoji, style};

use crate::{rfc, why};

/// Return `true` when any diagnostic is error-level.
#[must_use]
pub(crate) fn has_error_diagnostics(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.level == DiagnosticLevel::Error)
}

/// Print compiler diagnostics with annotated source snippets.
pub(crate) fn print_compiler_diagnostics(
    file_path: &Path,
    diagnostics: &[Diagnostic],
    show_why: bool,
) {
    if diagnostics.is_empty() {
        return;
    }

    let has_errors = has_error_diagnostics(diagnostics);
    let (emoji, fallback) = if has_errors {
        ("🚨", "Errors")
    } else {
        ("⚠️", "Warnings")
    };

    println!("{} {}:", Emoji::new(emoji, fallback), file_path.display());

    for diagnostic in diagnostics {
        let rendered = format_diagnostic(diagnostic);
        if diagnostic.level == DiagnosticLevel::Error {
            println!("{}", style(rendered).red());
        } else {
            println!("{}", style(rendered).yellow());
        }

        if show_why && let Some(entry) = why::find(diagnostic.code) {
            print!(
                "{}",
                style(rfc::render_citations("WHY", entry.citations)).cyan()
            );
        }
    }
}

/// Format one diagnostic with annotated source snippets.
fn format_diagnostic(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    let level = match diagnostic.level {
        DiagnosticLevel::Error => "error",
        DiagnosticLevel::Warning => "warning",
    };
    let _ = writeln!(out, "{level}[{}]: {}", diagnostic.code, diagnostic.message);

    if diagnostic.code == "W001" {
        if let Some(previous) = &diagnostic.previous_origin {
            write_annotated_origin(
                &mut out,
                "-->",
                &previous.source_path,
                previous.line,
                previous.column,
                "first defined here",
            );
        }

        if let (Some(path), Some(span)) = (&diagnostic.source_file, &diagnostic.span) {
            write_annotated_span(&mut out, ":::", path, span, Some("redundant here"), None);
        } else if let Some(path) = &diagnostic.source_file {
            let _ = writeln!(out, "  ::: {}", path.display());
            let _ = writeln!(out);
        }
    } else {
        if let (Some(path), Some(span)) = (&diagnostic.source_file, &diagnostic.span) {
            write_annotated_span(&mut out, "-->", path, span, Some("here"), None);
        } else if let Some(path) = &diagnostic.source_file {
            let _ = writeln!(out, "  --> {}", path.display());
            let _ = writeln!(out);
        }

        if let Some(previous) = &diagnostic.previous_origin {
            write_annotated_origin(
                &mut out,
                ":::",
                &previous.source_path,
                previous.line,
                previous.column,
                "first defined here",
            );
        }
    }

    if !diagnostic.related.is_empty() {
        write_related(&mut out, diagnostic.code, &diagnostic.related);
    }

    let _ = writeln!(out);
    out
}

/// Render a sequence of [`Subdiag`] annotations under a diagnostic.
///
/// For `.within` / `.and` diagnostics (detected by the presence of
/// [`SubdiagKind::Matched`] or [`SubdiagKind::Optional`] entries),
/// renders a single inline diff block. All other diagnostics render
/// each subdiag as a separate labelled block.
fn write_related(
    out: &mut String,
    code: &str,
    related: &[cbork_cddl_compiler::Subdiag],
) {
    if let Some(diff_start) = effective_schema_prefix_len(related)
        && let (Some(schema_related), Some(diff_related)) =
            (related.get(..diff_start), related.get(diff_start..))
        && !diff_related.is_empty()
        && is_diff_related(code, diff_related)
    {
        write_effective_schema_blocks(out, schema_related);
        write_diff_related(out, diff_related);
        return;
    }

    if is_diff_related(code, related) {
        write_diff_related(out, related);
    } else {
        write_legacy_related(out, related);
    }
}

/// Return the length of an initial LHS/RHS schema block pair.
fn effective_schema_prefix_len(related: &[cbork_cddl_compiler::Subdiag]) -> Option<usize> {
    match related {
        [lhs, rhs, ..] if lhs.kind == SubdiagKind::Lhs && rhs.kind == SubdiagKind::Rhs => Some(2),
        _ => None,
    }
}

/// Diff diagnostics use only line-level classifications.
fn is_diff_related(
    code: &str,
    related: &[cbork_cddl_compiler::Subdiag],
) -> bool {
    let has_diff_only_kinds = related.iter().all(|s| {
        matches!(
            s.kind,
            SubdiagKind::Matched
                | SubdiagKind::Unmatched
                | SubdiagKind::Optional
                | SubdiagKind::Note
        )
    });
    let has_concrete_diff = related.iter().any(|s| {
        matches!(
            s.kind,
            SubdiagKind::Matched | SubdiagKind::Optional | SubdiagKind::Unmatched
        )
    });

    // Some v1 diff conflicts are pathless/unmapped and therefore
    // arrive as Note-only streams. For E030 these are still diff
    // output, not legacy note blocks.
    let has_e030_note_diff = code == "E030" && related.len() > 1;

    !related.is_empty() && has_diff_only_kinds && (has_concrete_diff || has_e030_note_diff)
}

/// Render the effective schemas that a diff compares.
fn write_effective_schema_blocks(
    out: &mut String,
    related: &[cbork_cddl_compiler::Subdiag],
) {
    for subdiag in related {
        let label = match subdiag.kind {
            SubdiagKind::Lhs => "EFFECTIVE LHS",
            SubdiagKind::Rhs => "EFFECTIVE RHS",
            _ => continue,
        };
        let _ = writeln!(out, "   = {label}:");
        if subdiag.snippet.is_empty() {
            let _ = writeln!(out, "       (empty)");
        } else {
            for line in subdiag.snippet.lines() {
                let _ = writeln!(out, "       {line}");
            }
        }
        if let Some(origin) = &subdiag.origin {
            let _ = writeln!(
                out,
                "       ; from {}:{}:{}",
                origin.source_path.display(),
                origin.line,
                origin.column
            );
        }
    }
}

/// Legacy rendering: one labelled block per subdiag.
fn write_legacy_related(
    out: &mut String,
    related: &[cbork_cddl_compiler::Subdiag],
) {
    for subdiag in related {
        let label = match subdiag.kind {
            SubdiagKind::Lhs => "LHS",
            SubdiagKind::Rhs => "RHS",
            SubdiagKind::Matched => "MATCHED",
            SubdiagKind::Unmatched => "UNMATCHED",
            SubdiagKind::Optional => "OPTIONAL",
            SubdiagKind::FoldedFrom => "FOLDED",
            SubdiagKind::Note => "NOTE",
        };
        let _ = writeln!(out, "   = {label}:");
        if subdiag.snippet.is_empty() {
            let _ = writeln!(out, "       (empty)");
        } else {
            for line in subdiag.snippet.lines() {
                let _ = writeln!(out, "       {line}");
            }
        }
        if let Some(origin) = &subdiag.origin {
            let _ = writeln!(
                out,
                "       ; from {}:{}:{}",
                origin.source_path.display(),
                origin.line,
                origin.column
            );
        }
    }
}

/// Inline diff rendering for `.within` / `.and` diagnostics.
///
/// Renders a single `= DIFF:` block with each subdiag on its own
/// line, prefixed by a compact label that indicates the line's
/// classification in the subtype check:
///
/// ```text
///    = DIFF:
///        ==  <matched line>
///        --  <rejected / required-missing line>
///        ??  <optional RHS entry>
///        !!  <pathless conflict summary>
///            <context line>
/// ```
fn write_diff_related(
    out: &mut String,
    related: &[cbork_cddl_compiler::Subdiag],
) {
    let _ = writeln!(out, "   = DIFF:");
    for subdiag in related {
        let label = match subdiag.kind {
            SubdiagKind::Matched => "==",
            SubdiagKind::Unmatched => "--",
            SubdiagKind::Optional => "??",
            SubdiagKind::Note if looks_like_reason(&subdiag.snippet) => "!!",
            SubdiagKind::Note => "  ",
            _ => continue,
        };
        if subdiag.snippet.is_empty() {
            let _ = writeln!(out, "       {label}  (empty)");
        } else {
            for line in subdiag.snippet.lines() {
                let _ = writeln!(out, "       {label}  {line}");
            }
        }
    }
}

/// Heuristic for v1 pathless/unmapped conflict notes.
fn looks_like_reason(snippet: &str) -> bool {
    let trimmed = snippet.trim_start();
    trimmed.starts_with("map[")
        || trimmed.starts_with("array[")
        || trimmed.starts_with("choice[")
        || trimmed.starts_with("control(")
        || trimmed.contains(" not subtype ")
        || trimmed.contains(" no matching ")
        || trimmed.contains(" has no matching ")
        || trimmed.contains("different structure")
}

/// Annotated source location extracted from a file.
struct SourceExcerpt {
    /// One-based line number.
    line: usize,
    /// One-based starting column.
    column_start: usize,
    /// One-based ending column (inclusive).
    column_end: usize,
    /// Source text for the selected line.
    line_text: String,
}

/// Render an annotated span block.
fn write_annotated_span(
    out: &mut String,
    marker: &str,
    path: &Path,
    span: &Range<usize>,
    label: Option<&str>,
    message: Option<&str>,
) {
    match excerpt_from_span(path, span) {
        Some(excerpt) => write_excerpt(out, marker, path, &excerpt, label, message),
        None => {
            let _ = writeln!(out, "  {marker} {}", path.display());
        },
    }
}

/// Render an annotated point-location block.
fn write_annotated_origin(
    out: &mut String,
    marker: &str,
    path: &Path,
    line: usize,
    column: usize,
    label: &str,
) {
    match excerpt_from_origin(path, line, column) {
        Some(excerpt) => write_excerpt(out, marker, path, &excerpt, Some(label), None),
        None => {
            let _ = writeln!(out, "  {marker} {}:{line}:{column}", path.display());
        },
    }
}

/// Write a clippy-style excerpt block.
fn write_excerpt(
    out: &mut String,
    marker: &str,
    path: &Path,
    excerpt: &SourceExcerpt,
    label: Option<&str>,
    message: Option<&str>,
) {
    let line_width = excerpt.line.to_string().len();
    let _ = writeln!(
        out,
        "  {marker} {}:{}:{}",
        path.display(),
        excerpt.line,
        excerpt.column_start
    );
    let _ = writeln!(out, "  {:>line_width$} |", "", line_width = line_width);
    let _ = writeln!(
        out,
        "  {:>line_width$} | {}",
        excerpt.line,
        excerpt.line_text,
        line_width = line_width
    );

    let padding = " ".repeat(excerpt.column_start.saturating_sub(1));
    let underline_len = excerpt
        .column_end
        .saturating_sub(excerpt.column_start)
        .saturating_add(1)
        .max(1);
    let carets = "^".repeat(underline_len);

    match (label, message) {
        (Some(label), Some(message)) => {
            let _ = writeln!(
                out,
                "  {:>line_width$} | {}{} {}: {}",
                "",
                padding,
                carets,
                label,
                message,
                line_width = line_width
            );
        },
        (Some(label), None) => {
            let _ = writeln!(
                out,
                "  {:>line_width$} | {}{} {}",
                "",
                padding,
                carets,
                label,
                line_width = line_width
            );
        },
        (None, Some(message)) => {
            let _ = writeln!(
                out,
                "  {:>line_width$} | {}{} {}",
                "",
                padding,
                carets,
                message,
                line_width = line_width
            );
        },
        (None, None) => {
            let _ = writeln!(
                out,
                "  {:>line_width$} | {}{}",
                "",
                padding,
                carets,
                line_width = line_width
            );
        },
    }

    let _ = writeln!(out, "  {:>line_width$} |", "", line_width = line_width);
}

/// Resolve a byte span to a single-line source excerpt.
fn excerpt_from_span(
    path: &Path,
    span: &Range<usize>,
) -> Option<SourceExcerpt> {
    let source = std::fs::read_to_string(path).ok()?;
    let start = byte_offset_to_line_column(&source, span.start)?;
    let end_offset = span.end.saturating_sub(1).max(span.start);
    let mut end = byte_offset_to_line_column(&source, end_offset)?;

    if end.line != start.line {
        end.column = source
            .lines()
            .nth(start.line.saturating_sub(1))?
            .chars()
            .count()
            .saturating_add(1);
    }

    let line_text = source.lines().nth(start.line.saturating_sub(1))?.to_owned();

    Some(SourceExcerpt {
        line: start.line,
        column_start: start.column,
        column_end: end.column.max(start.column),
        line_text,
    })
}

/// Resolve a line/column location to a single-line source excerpt.
fn excerpt_from_origin(
    path: &Path,
    line: usize,
    column: usize,
) -> Option<SourceExcerpt> {
    let source = std::fs::read_to_string(path).ok()?;
    let line_text = source.lines().nth(line.saturating_sub(1))?.to_owned();
    Some(SourceExcerpt {
        line,
        column_start: column,
        column_end: column,
        line_text,
    })
}

/// One-based line/column pair.
struct LineColumn {
    /// One-based line number.
    line: usize,
    /// One-based column number.
    column: usize,
}

/// Convert a byte offset in UTF-8 source to a one-based line/column.
fn byte_offset_to_line_column(
    source: &str,
    offset: usize,
) -> Option<LineColumn> {
    if offset > source.len() {
        return None;
    }

    let mut line = 1usize;
    let mut column = 1usize;

    for (idx, ch) in source.char_indices() {
        if idx >= offset {
            return Some(LineColumn { line, column });
        }

        if ch == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }

    Some(LineColumn { line, column })
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        path::{Path, PathBuf},
    };

    use cbork_cddl_compiler::{Diagnostic, DiagnosticLevel, SubdiagKind};

    use super::{format_diagnostic, has_error_diagnostics, print_compiler_diagnostics};

    #[test]
    fn diagnostics_respect_mixed_severity_levels() {
        let diagnostics = vec![
            Diagnostic {
                code: "W001",
                level: DiagnosticLevel::Warning,
                message: "redundant definition".to_owned(),
                source_file: Some(PathBuf::from("warning.cddl")),
                span: None,
                previous_origin: None,
                related: Vec::new(),
            },
            Diagnostic {
                code: "E014",
                level: DiagnosticLevel::Error,
                message: "conflicting definition".to_owned(),
                source_file: Some(PathBuf::from("error.cddl")),
                span: None,
                previous_origin: None,
                related: Vec::new(),
            },
        ];

        assert!(has_error_diagnostics(&diagnostics));

        let rendered = diagnostics
            .iter()
            .map(format_diagnostic)
            .collect::<String>();
        assert!(rendered.contains("warning[W001]: redundant definition"));
        assert!(rendered.contains("error[E014]: conflicting definition"));
    }

    #[test]
    fn diagnostics_render_why_without_panicking() {
        let diagnostics = vec![Diagnostic {
            code: "E016",
            level: DiagnosticLevel::Error,
            message: "undefined reference".to_owned(),
            source_file: Some(PathBuf::from("error.cddl")),
            span: None,
            previous_origin: None,
            related: Vec::new(),
        }];

        print_compiler_diagnostics(Path::new("error.cddl"), &diagnostics, true);
    }

    #[test]
    fn redundant_definition_renders_first_occurrence_before_redundant_one() {
        let dir = std::env::temp_dir().join("cbork_diagnostics_test");
        std::fs::create_dir_all(&dir).expect("temp diagnostics dir should exist");
        let first_path = dir.join("first.cddl");
        let current_path = dir.join("current.cddl");
        std::fs::File::create(&first_path)
            .and_then(|mut file| file.write_all(b"ttl = 0..10\n"))
            .expect("first file should be written");
        std::fs::File::create(&current_path)
            .and_then(|mut file| file.write_all(b"ttl = 0..10\n"))
            .expect("current file should be written");

        let diagnostic = Diagnostic {
            code: "W001",
            level: DiagnosticLevel::Warning,
            message: "redundant definition".to_owned(),
            source_file: Some(current_path.clone()),
            span: Some(0..11),
            previous_origin: Some(cbork_cddl_compiler::SourceOrigin::new(
                first_path.clone(),
                1,
                1,
            )),
            related: Vec::new(),
        };

        let rendered = format_diagnostic(&diagnostic);
        let first_idx = rendered
            .find(&format!("--> {}:1:1", first_path.display()))
            .expect("first occurrence should be rendered first");
        let redundant_idx = rendered
            .find(&format!("::: {}:1:1", current_path.display()))
            .expect("redundant occurrence should be rendered second");
        assert!(first_idx < redundant_idx, "{rendered}");
        assert!(rendered.contains("first defined here"));
        assert!(rendered.contains("redundant here"));
    }

    #[test]
    fn subdiags_render_lhs_and_rhs_blocks() {
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: ".within subtype check failed".to_owned(),
            source_file: Some(PathBuf::from("file.cddl")),
            span: Some(0..10),
            previous_origin: None,
            related: vec![
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Lhs,
                    snippet: "{ ed25519 => bstr, 5 => bstr }".to_owned(),
                    origin: None,
                },
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Rhs,
                    snippet: "{ 1 => int / tstr, * cose.label => any }".to_owned(),
                    origin: None,
                },
            ],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(rendered.contains("= LHS:"), "{rendered}");
        assert!(rendered.contains("= RHS:"), "{rendered}");
        assert!(
            rendered.contains("ed25519 => bstr"),
            "LHS snippet should be present: {rendered}"
        );
        assert!(
            rendered.contains("cose.label => any"),
            "RHS snippet should be present: {rendered}"
        );
    }

    #[test]
    fn empty_subdiag_renders_empty_placeholder() {
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: "no snippet".to_owned(),
            source_file: None,
            span: None,
            previous_origin: None,
            related: vec![cbork_cddl_compiler::Subdiag {
                kind: SubdiagKind::Note,
                snippet: String::new(),
                origin: None,
            }],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(rendered.contains("= NOTE:"), "{rendered}");
        assert!(rendered.contains("(empty)"), "{rendered}");
    }

    #[test]
    fn subdiag_origin_renders_provenance_line() {
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: "with origin".to_owned(),
            source_file: None,
            span: None,
            previous_origin: None,
            related: vec![cbork_cddl_compiler::Subdiag {
                kind: SubdiagKind::FoldedFrom,
                snippet: "42".to_owned(),
                origin: Some(cbork_cddl_compiler::SourceOrigin::new(
                    PathBuf::from("constants.cddl"),
                    7,
                    1,
                )),
            }],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(rendered.contains("= FOLDED:"), "{rendered}");
        assert!(rendered.contains("; from constants.cddl:7:1"), "{rendered}");
    }

    #[test]
    fn diff_subdiags_render_in_order() {
        // Matched, Unmatched, and Optional diff subdiags must render
        // under a single = DIFF: block in the correct order.
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: ".within subtype check failed".to_owned(),
            source_file: Some(PathBuf::from("file.cddl")),
            span: Some(0..10),
            previous_origin: None,
            related: vec![
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Matched,
                    snippet: "1 => int".to_owned(),
                    origin: None,
                },
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Unmatched,
                    snippet: "2.5 => 'test'".to_owned(),
                    origin: None,
                },
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Optional,
                    snippet: "? 3 => bool".to_owned(),
                    origin: None,
                },
            ],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(
            rendered.contains("= DIFF:"),
            "missing DIFF header: {rendered}"
        );
        let ok_pos = rendered.find("==  1 => int").expect("expected == label");
        let conflict_pos = rendered
            .find("--  2.5 => 'test'")
            .expect("expected -- label");
        let optional_pos = rendered.find("??  ? 3 => bool").expect("expected ?? label");
        assert!(
            ok_pos < conflict_pos,
            "== should appear before --: {rendered}"
        );
        assert!(
            conflict_pos < optional_pos,
            "-- should appear before ??: {rendered}"
        );
        assert!(
            rendered.contains("2.5 => 'test'"),
            "Unmatched snippet missing: {rendered}"
        );
        assert!(
            rendered.contains("? 3 => bool"),
            "Optional snippet missing: {rendered}"
        );
    }

    #[test]
    fn diff_conflict_includes_reason() {
        // An Unmatched subdiag whose snippet carries a reason (via the
        // `text ; reason` encoding) must render the reason visibly.
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: ".within subtype check failed".to_owned(),
            source_file: Some(PathBuf::from("file.cddl")),
            span: Some(0..10),
            previous_origin: None,
            related: vec![cbork_cddl_compiler::Subdiag {
                kind: SubdiagKind::Unmatched,
                snippet: "3 => bool  ; LHS has a required key not accepted by the RHS".to_owned(),
                origin: None,
            }],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(rendered.contains("= DIFF:"), "{rendered}");
        assert!(rendered.contains("--  3 => bool"), "{rendered}");
        assert!(
            rendered.contains("LHS has a required key not accepted by the RHS"),
            "reason should appear after the conflict line: {rendered}"
        );
    }

    #[test]
    fn legacy_lhs_rhs_not_detected_as_diff() {
        // Legacy LHS/RHS subdiags must NOT be rendered as a diff. They
        // must keep their individual = LHS: / = RHS: blocks.
        let diagnostic = Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Error,
            message: ".within subtype check failed".to_owned(),
            source_file: Some(PathBuf::from("file.cddl")),
            span: Some(0..10),
            previous_origin: None,
            related: vec![
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Lhs,
                    snippet: "{ 1 => int }".to_owned(),
                    origin: None,
                },
                cbork_cddl_compiler::Subdiag {
                    kind: SubdiagKind::Rhs,
                    snippet: "{ 2 => tstr }".to_owned(),
                    origin: None,
                },
            ],
        };
        let rendered = format_diagnostic(&diagnostic);
        assert!(
            !rendered.contains("DIFF"),
            "legacy LHS/RHS should not render as DIFF: {rendered}"
        );
        assert!(rendered.contains("= LHS:"), "{rendered}");
        assert!(rendered.contains("= RHS:"), "{rendered}");
    }
}
