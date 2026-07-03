// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Semantic documentation checks (step 9 of `crates/cbork/plan.md`).
//!
//! Implements the four cbork semantic checks documented in the plan:
//!
//! * File/module documentation must start with a level-1 Markdown heading.
//! * Definition documentation must start with a level-3 Markdown heading.
//! * Exported definitions must have documentation.
//! * Documented generic definitions must document every generic parameter.
//!
//! The internal-definition policy (`--doc-internal no|warn|yes`) is also
//! implemented here so the `--doc` flag can drive the whole semantic
//! pass in one place.
//!
//! The checks consume the existing [`crate::doc_block::DocScan`] and the
//! `user_nodes` from a [`crate::CompiledCDDL`], so they slot into the
//! existing CDDL lint pass without re-parsing anything.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use crate::{
    Diagnostic, DiagnosticLevel, SourceOrigin,
    doc_block::{DocBlock, DocScan},
    node::WrappedNode,
    symbols::rule_name,
};

/// Diagnostic codes emitted by the semantic doc-lint pass.
///
/// * `E030` — file/module documentation does not start with a level-1 heading.
/// * `E031` — definition documentation does not start with a level-3 heading.
/// * `E032` — exported definition has no documentation.
/// * `E033` — documented generic definition is missing a parameter description.
/// * `W040` — internal definition has no documentation (under `--doc-internal warn`).
/// * `E034` — internal definition has no documentation (under `--doc-internal yes`).
const CODE_FILE_DOC_MISSING_H1: &str = "E030";

/// Diagnostic code for a definition doc that does not start with a level-3 heading.
const CODE_DEF_DOC_MISSING_H3: &str = "E031";

/// Diagnostic code for an exported definition that has no documentation.
const CODE_EXPORTED_MISSING_DOCS: &str = "E032";

/// Diagnostic code for a documented generic definition missing a parameter description.
const CODE_GENERIC_PARAM_UNDOCUMENTED: &str = "E033";

/// Diagnostic code for an undocumented internal definition under `--doc-internal warn`.
const CODE_INTERNAL_MISSING_DOCS_WARN: &str = "W040";

/// Diagnostic code for an undocumented internal definition under `--doc-internal yes`.
const CODE_INTERNAL_MISSING_DOCS_ERROR: &str = "E034";

/// Policy for documentation of *internal* (non-exported) definitions,
/// selected by the `--doc-internal` CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocInternalPolicy {
    /// Do not require documentation for internal definitions. This is the
    /// default so enabling `--doc` does not force every private helper
    /// rule to be documented immediately.
    #[default]
    No,
    /// Warn when an internal definition has no documentation.
    Warn,
    /// Error when an internal definition has no documentation.
    Yes,
}

/// Configuration for the semantic doc-lint pass.
#[derive(Debug, Clone, Default)]
pub struct DocSemanticsConfig {
    /// Policy for internal (non-exported) definition documentation.
    pub doc_internal: DocInternalPolicy,
    /// Names of rules that the source has declared as part of its
    /// public library API via `;@ CBORK: Export`. Exported rules are
    /// required to have documentation; everything else follows the
    /// `--doc-internal` policy.
    pub exported_names: HashSet<String>,
}

/// Result of the semantic doc-lint pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocSemanticsReport {
    /// Diagnostics emitted by the semantic checks.
    pub diagnostics: Vec<Diagnostic>,
}

impl DocSemanticsReport {
    /// Returns `true` when the source has no doc-lint semantic issues.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Run the semantic doc-lint pass on a compiled CDDL source.
///
/// `user_nodes` is the AST from [`crate::CompiledCDDL::user_nodes`].
/// `scan` is the output of [`crate::doc_block::scan_doc_blocks`] on the
/// captured pre-transform source text. `source_text` is the original
/// CDDL text and is used to translate 1-based line numbers to byte
/// offsets so the rendered diagnostic carets line up with the user's
/// editor. `source_path` is the path attached to every diagnostic.
/// The function is pure and does not perform any I/O.
#[must_use]
pub fn check_doc_semantics(
    source_text: &str,
    source_path: &Path,
    user_nodes: &[WrappedNode],
    scan: &DocScan,
    config: &DocSemanticsConfig,
) -> DocSemanticsReport {
    let line_offsets = compute_line_offsets(source_text);
    let definitions = collect_top_level_definitions(user_nodes);

    let mut definitions_by_line: HashMap<usize, TopLevelDefinition> = HashMap::new();
    for def in &definitions {
        definitions_by_line.insert(def.line, def.clone());
    }

    let mut diagnostics = Vec::new();
    check_doc_blocks(
        scan,
        &definitions_by_line,
        &line_offsets,
        source_path,
        &mut diagnostics,
    );
    check_definition_coverage(
        &definitions,
        scan,
        config,
        &line_offsets,
        source_path,
        &mut diagnostics,
    );

    DocSemanticsReport { diagnostics }
}

/// A trimmed-down view of a top-level CDDL rule definition that is
/// useful for the semantic doc-lint pass.
#[derive(Debug, Clone)]
struct TopLevelDefinition {
    /// Rule name as it appears in source.
    name: String,
    /// 1-based source line of the rule definition.
    line: usize,
    /// Generic parameter names declared on the rule head, in the
    /// order they appear in the source. Empty for non-generic rules.
    generic_params: Vec<String>,
}

/// Walk every doc block in `scan` and append semantic diagnostics
/// for each block. A block that is both file-level and bound to a
/// definition receives *both* the file-doc heading check and the
/// definition-doc heading / generic-param checks.
fn check_doc_blocks(
    scan: &DocScan,
    definitions_by_line: &HashMap<usize, TopLevelDefinition>,
    line_offsets: &[usize],
    source_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (block, binding) in scan.iter() {
        let text = block_text(block);
        if binding.is_file_level {
            check_file_doc_heading(block, &text, line_offsets, source_path, diagnostics);
        }
        let Some(definition_line) = binding.definition_line else {
            continue;
        };
        check_definition_doc_heading(
            block,
            &text,
            definition_line,
            line_offsets,
            source_path,
            diagnostics,
        );
        check_documented_generic_params(
            block,
            &text,
            definition_line,
            definitions_by_line,
            line_offsets,
            source_path,
            diagnostics,
        );
    }
}

/// Emit `E030` when the file-level doc block does not start with a
/// level-1 heading.
fn check_file_doc_heading(
    block: &DocBlock,
    text: &str,
    line_offsets: &[usize],
    source_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if starts_with_heading_level(text, 1) {
        return;
    }
    let span = line_range_to_byte_range(line_offsets, block.start_line, block.end_line);
    diagnostics.push(Diagnostic {
        code: CODE_FILE_DOC_MISSING_H1,
        level: DiagnosticLevel::Warning,
        message: "file/module documentation must start with a level-1 Markdown heading (`# `)"
            .to_owned(),
        source_file: Some(source_path.to_path_buf()),
        span: Some(span),
        previous_origin: None,
        related: Vec::new(),
    });
}

/// Emit `E031` when a definition's doc block does not start with a
/// level-3 heading.
fn check_definition_doc_heading(
    block: &DocBlock,
    text: &str,
    definition_line: usize,
    line_offsets: &[usize],
    source_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if starts_with_heading_level(text, 3) {
        return;
    }
    let span = line_range_to_byte_range(line_offsets, block.start_line, block.end_line);
    diagnostics.push(Diagnostic {
        code: CODE_DEF_DOC_MISSING_H3,
        level: DiagnosticLevel::Warning,
        message: "definition documentation must start with a level-3 Markdown heading (`### `)"
            .to_owned(),
        source_file: Some(source_path.to_path_buf()),
        span: Some(span),
        previous_origin: Some(SourceOrigin::new(
            source_path.to_path_buf(),
            definition_line,
            1,
        )),
        related: Vec::new(),
    });
}

/// Emit one `E033` for every generic parameter on a definition whose
/// doc block is present but does not mention the parameter by name.
fn check_documented_generic_params(
    block: &DocBlock,
    text: &str,
    definition_line: usize,
    definitions_by_line: &HashMap<usize, TopLevelDefinition>,
    line_offsets: &[usize],
    source_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(definition) = definitions_by_line.get(&definition_line) else {
        return;
    };
    if definition.generic_params.is_empty() {
        return;
    }
    for param in &definition.generic_params {
        if doc_mentions_parameter(text, param) {
            continue;
        }
        let span = line_range_to_byte_range(line_offsets, block.start_line, block.end_line);
        diagnostics.push(Diagnostic {
            code: CODE_GENERIC_PARAM_UNDOCUMENTED,
            level: DiagnosticLevel::Warning,
            message: format!(
                "documented generic definition `{name}` is missing a description of \
                 parameter `{param}` in its documentation comment",
                name = definition.name,
                param = param,
            ),
            source_file: Some(source_path.to_path_buf()),
            span: Some(span),
            previous_origin: Some(SourceOrigin::new(
                source_path.to_path_buf(),
                definition_line,
                1,
            )),
            related: Vec::new(),
        });
    }
}

/// Walk every top-level definition and emit a diagnostic when the
/// definition has no doc block: an error-level `E032` for exported
/// definitions, plus a `W040` or `E034` for internal ones when the
/// `--doc-internal` policy says so. The default `--doc-internal no`
/// policy is silent for internal definitions.
fn check_definition_coverage(
    definitions: &[TopLevelDefinition],
    scan: &DocScan,
    config: &DocSemanticsConfig,
    line_offsets: &[usize],
    source_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for def in definitions {
        if doc_block_covers_line(scan, def.line) {
            continue;
        }
        let is_exported = config.exported_names.contains(&def.name);
        if is_exported {
            push_uncovered_definition_diagnostic(
                diagnostics,
                CODE_EXPORTED_MISSING_DOCS,
                DiagnosticLevel::Warning,
                def,
                line_offsets,
                source_path,
                "exported definition `{name}` has no documentation comment",
            );
            continue;
        }
        match config.doc_internal {
            DocInternalPolicy::No => {},
            DocInternalPolicy::Warn => {
                push_uncovered_definition_diagnostic(
                    diagnostics,
                    CODE_INTERNAL_MISSING_DOCS_WARN,
                    DiagnosticLevel::Warning,
                    def,
                    line_offsets,
                    source_path,
                    "internal definition `{name}` has no documentation comment \
                     (--doc-internal warn)",
                );
            },
            DocInternalPolicy::Yes => {
                push_uncovered_definition_diagnostic(
                    diagnostics,
                    CODE_INTERNAL_MISSING_DOCS_ERROR,
                    DiagnosticLevel::Error,
                    def,
                    line_offsets,
                    source_path,
                    "internal definition `{name}` has no documentation comment \
                     (--doc-internal yes)",
                );
            },
        }
    }
}

/// Push a single "definition `X` has no documentation" diagnostic. The
/// `message` template uses `{name}` as the place to substitute the
/// definition name, so the three call sites above can share the
/// span/message boilerplate without repeating `let span = ...`.
fn push_uncovered_definition_diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    code: &'static str,
    level: DiagnosticLevel,
    def: &TopLevelDefinition,
    line_offsets: &[usize],
    source_path: &Path,
    message: &str,
) {
    let span = line_range_to_byte_range(line_offsets, def.line, def.line.saturating_add(1));
    diagnostics.push(Diagnostic {
        code,
        level,
        message: message.replace("{name}", &def.name),
        source_file: Some(source_path.to_path_buf()),
        span: Some(span),
        previous_origin: None,
        related: Vec::new(),
    });
}

/// Reconstruct a doc block's Markdown text from the per-line content.
///
/// Lines are joined with `\n` because the synthetic transform preserves
/// one doc line per output line. Empty lines inside a doc block are
/// preserved verbatim.
fn block_text(block: &DocBlock) -> String {
    let mut out = String::new();
    for (idx, line) in block.lines.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&line.text);
    }
    out
}

/// Returns `true` when the first non-blank line of `text` is a Markdown
/// heading at exactly `level` `#` characters followed by a space.
///
/// `### foo` is a level-3 heading. `## foo` is level-2. The leading
/// space after the `#` characters is required so we do not match
/// `#foo` (a literal `#` followed by text).
fn starts_with_heading_level(
    text: &str,
    level: usize,
) -> bool {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let expected_hashes = "#".repeat(level);
        return trimmed
            .strip_prefix(&expected_hashes)
            .is_some_and(|rest| rest.starts_with(' '));
    }
    false
}

/// Returns `true` when `text` mentions the generic parameter `name`
/// as a whole word (delimited by non-word characters or string
/// boundaries).
///
/// The match is intentionally word-based rather than a plain substring
/// search so that a single-letter parameter name like `a` is not
/// accidentally satisfied by every mention of "a" inside ordinary
/// English words ("a", "any", "pair", etc.). The user's prose
/// still drives the description; the match is just strict enough to
/// reject incidental occurrences.
fn doc_mentions_parameter(
    text: &str,
    name: &str,
) -> bool {
    if name.is_empty() {
        return true;
    }
    text.lines().any(|line| {
        line.split(|c: char| !is_word_char(c))
            .any(|word| word == name)
    })
}

/// Returns `true` for ASCII letters, digits, and underscore. These are
/// the characters that form identifiers in CDDL and Markdown code
/// spans.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Returns `true` when any doc block in `scan` binds to the given
/// 1-based source line.
fn doc_block_covers_line(
    scan: &DocScan,
    line: usize,
) -> bool {
    scan.iter()
        .any(|(_, binding)| binding.definition_line == Some(line))
}

/// Build a 1-based table of byte offsets for the start of each line in
/// `source_text`. `line_offsets[1]` is the byte offset of line 1,
/// `line_offsets[2]` of line 2, etc. Index 0 is unused.
fn compute_line_offsets(source_text: &str) -> Vec<usize> {
    let mut offsets = vec![0_usize; 1];
    for (idx, byte) in source_text.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(idx.saturating_add(1));
        }
    }
    offsets
}

/// Convert an inclusive 1-based `start..=end` line range to a byte
/// offset range. Returns an empty range for out-of-bounds input.
fn line_range_to_byte_range(
    line_offsets: &[usize],
    start_line: usize,
    end_line: usize,
) -> std::ops::Range<usize> {
    if start_line == 0 || start_line > line_offsets.len() {
        return 0..0;
    }
    let start = line_offsets
        .get(start_line.saturating_sub(1))
        .copied()
        .unwrap_or(0);
    let end_byte = if end_line < line_offsets.len() {
        line_offsets.get(end_line).copied().unwrap_or(usize::MAX)
    } else {
        // Last line: use a value safely past the end of the source.
        // The diagnostic renderer's `write_annotated_span` clamps
        // the end offset to the source line length, so over-shooting
        // is safe.
        usize::MAX
    };
    start..end_byte
}

/// Walk `user_nodes` and collect every top-level rule definition
/// together with its 1-based source line and its generic parameter
/// names. Generic parameter names are extracted from the rule head's
/// `genericparm` syntax node, falling back to a textual split of the
/// `<...>` argument when the AST does not expose the names
/// individually.
fn collect_top_level_definitions(user_nodes: &[WrappedNode]) -> Vec<TopLevelDefinition> {
    let mut definitions = Vec::new();
    let mut seen_names = HashSet::new();
    for node in user_nodes {
        let WrappedNode::RuleLine {
            children, origin, ..
        } = node
        else {
            continue;
        };
        let Some(name) = rule_name(node) else {
            continue;
        };
        if !seen_names.insert(name.clone()) {
            continue;
        }
        let line = origin.line;
        let generic_params = collect_generic_params(children);
        definitions.push(TopLevelDefinition {
            name,
            line,
            generic_params,
        });
    }
    definitions
}

/// Return the list of generic parameter names declared on the rule
/// head. Looks for `genericparm` children first; falls back to parsing
/// the textual `<a, b, c>` argument when the AST does not surface
/// each name as a separate child.
fn collect_generic_params(children: &[WrappedNode]) -> Vec<String> {
    for child in children {
        let WrappedNode::Syntax {
            rule,
            text,
            children: inner,
            ..
        } = child
        else {
            continue;
        };
        if rule != "expr" {
            continue;
        }
        for expr_child in inner {
            let WrappedNode::Syntax {
                rule,
                text: gp_text,
                children: gp_children,
                ..
            } = expr_child
            else {
                continue;
            };
            if rule == "genericparm" {
                let mut names = Vec::new();
                for gp in gp_children {
                    let WrappedNode::Syntax {
                        rule,
                        text: id_text,
                        ..
                    } = gp
                    else {
                        continue;
                    };
                    if rule == "id" {
                        names.push(id_text.trim().to_owned());
                    }
                }
                if !names.is_empty() {
                    return names;
                }
                let trimmed = gp_text.trim().trim_start_matches('<').trim_end_matches('>');
                for part in trimmed.split(',') {
                    let name = part.trim();
                    if !name.is_empty() {
                        names.push(name.to_owned());
                    }
                }
                if !names.is_empty() {
                    return names;
                }
            }
            let _ = text;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        doc_block::{DocBinding, DocLine, DocScan},
        scan_doc_blocks,
    };

    fn make_block(
        start: usize,
        end: usize,
        lines: &[&str],
    ) -> DocBlock {
        DocBlock {
            start_line: start,
            end_line: end,
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    DocLine {
                        line: start.saturating_add(i),
                        text: (*t).to_owned(),
                    }
                })
                .collect(),
        }
    }

    fn make_scan(blocks: Vec<(DocBlock, DocBinding)>) -> DocScan {
        let (blocks, bindings): (Vec<_>, Vec<_>) = blocks.into_iter().unzip();
        DocScan { blocks, bindings }
    }

    /// Build a fully-typed `user_nodes` for tests by parsing a real
    /// CDDL snippet. The caller picks the file name and adds any
    /// definitions they want exercised.
    fn parse_user_nodes(src: &str) -> Vec<WrappedNode> {
        use cbork_cddl_parser::parse_cddl;

        use crate::preprocessor::{inject_directives, process_ast};

        let pairs = parse_cddl(src).expect("parse test CDDL");
        let pairs = process_ast(pairs).expect("process test AST");
        inject_directives(std::path::Path::new("<test>"), &pairs, src).expect("inject directives")
    }

    #[test]
    fn starts_with_heading_level_matches_exact_level() {
        assert!(starts_with_heading_level("# Title", 1));
        assert!(!starts_with_heading_level("## Title", 1));
        assert!(starts_with_heading_level("### Title", 3));
        assert!(!starts_with_heading_level("## Title", 3));
        assert!(!starts_with_heading_level("#Title", 1));
    }

    #[test]
    fn starts_with_heading_level_skips_blank_leading_lines() {
        assert!(starts_with_heading_level("\n\n# Title\nbody", 1));
        assert!(!starts_with_heading_level("\n\nbody without heading", 1));
    }

    #[test]
    fn doc_mentions_parameter_uses_word_match() {
        assert!(doc_mentions_parameter(
            "The `value` parameter holds a T.",
            "value"
        ));
        assert!(!doc_mentions_parameter(
            "The key parameter holds a T.",
            "value"
        ));
        // The substring "a" inside "pair" must not satisfy a search
        // for the single-letter parameter "a".
        assert!(!doc_mentions_parameter("### pair", "a"));
        // "a" as a standalone word does count as a mention.
        assert!(doc_mentions_parameter("a is the first value", "a"));
        assert!(doc_mentions_parameter("`a` is the first value", "a"));
    }

    #[test]
    fn file_doc_missing_h1_emits_e030() {
        let scan = make_scan(vec![(
            make_block(1, 2, &["## A subsection", "body"]),
            DocBinding {
                is_file_level: true,
                definition_line: None,
            },
        )]);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &[], &scan, &config);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_FILE_DOC_MISSING_H1),
            "expected E030, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn file_doc_with_h1_is_clean() {
        let scan = make_scan(vec![(make_block(1, 1, &["# File title"]), DocBinding {
            is_file_level: true,
            definition_line: None,
        })]);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &[], &scan, &config);
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_FILE_DOC_MISSING_H1),
            "h1 file doc must not emit E030, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn file_doc_h2_section_inside_h1_is_clean() {
        // h2 is allowed inside file docs as sectioning.
        let scan = make_scan(vec![(
            make_block(1, 3, &["# File title", "", "## A section"]),
            DocBinding {
                is_file_level: true,
                definition_line: None,
            },
        )]);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &[], &scan, &config);
        assert!(
            report.is_clean(),
            "h1+h2 file doc must be clean: {report:#?}"
        );
    }

    #[test]
    fn definition_doc_missing_h3_emits_e031() {
        let scan = make_scan(vec![(make_block(1, 2, &["# Title", "body"]), DocBinding {
            is_file_level: false,
            definition_line: Some(4),
        })]);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &[], &scan, &config);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_DEF_DOC_MISSING_H3),
            "expected E031, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn definition_doc_with_h3_is_clean() {
        let scan = make_scan(vec![(make_block(1, 1, &["### rule"]), DocBinding {
            is_file_level: false,
            definition_line: Some(4),
        })]);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &[], &scan, &config);
        assert!(
            report.is_clean(),
            "h3 definition doc must be clean: {report:#?}"
        );
    }

    #[test]
    fn exported_definition_without_docs_emits_e032() {
        let nodes = parse_user_nodes("widget = 1\n");
        let mut exported_names = HashSet::new();
        exported_names.insert("widget".to_owned());
        let config = DocSemanticsConfig {
            exported_names,
            ..Default::default()
        };
        let report = check_doc_semantics("", Path::new(""), &nodes, &DocScan::default(), &config);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_EXPORTED_MISSING_DOCS),
            "expected E032, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn exported_definition_with_docs_is_clean() {
        let source = "\
;! ### widget
widget = 1
";
        let nodes = parse_user_nodes(source);
        let scan = scan_doc_blocks(source);
        let mut exported_names = HashSet::new();
        exported_names.insert("widget".to_owned());
        let config = DocSemanticsConfig {
            exported_names,
            ..Default::default()
        };
        let report = check_doc_semantics(source, Path::new(""), &nodes, &scan, &config);
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_EXPORTED_MISSING_DOCS),
            "documented export must not emit E032, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn internal_definition_with_no_docs_silent_under_default_policy() {
        let nodes = parse_user_nodes("helper = 1\n");
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &nodes, &DocScan::default(), &config);
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_INTERNAL_MISSING_DOCS_WARN),
            "internal def must not warn under `--doc-internal no`, got {:#?}",
            report.diagnostics
        );
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_INTERNAL_MISSING_DOCS_ERROR),
            "internal def must not error under `--doc-internal no`, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn internal_definition_with_no_docs_warns_under_warn_policy() {
        let nodes = parse_user_nodes("helper = 1\n");
        let config = DocSemanticsConfig {
            doc_internal: DocInternalPolicy::Warn,
            ..Default::default()
        };
        let report = check_doc_semantics("", Path::new(""), &nodes, &DocScan::default(), &config);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_INTERNAL_MISSING_DOCS_WARN),
            "expected W040 under `--doc-internal warn`, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn internal_definition_with_no_docs_errors_under_yes_policy() {
        let nodes = parse_user_nodes("helper = 1\n");
        let config = DocSemanticsConfig {
            doc_internal: DocInternalPolicy::Yes,
            ..Default::default()
        };
        let report = check_doc_semantics("", Path::new(""), &nodes, &DocScan::default(), &config);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_INTERNAL_MISSING_DOCS_ERROR),
            "expected E034 under `--doc-internal yes`, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn documented_generic_missing_parameter_emits_e033() {
        let source = "\
;! ### pair
pair<a, b> = [a, b]
";
        let nodes = parse_user_nodes(source);
        let scan = scan_doc_blocks(source);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &nodes, &scan, &config);
        let e033: Vec<_> = report
            .diagnostics
            .iter()
            .filter(|d| d.code == CODE_GENERIC_PARAM_UNDOCUMENTED)
            .collect();
        assert_eq!(
            e033.len(),
            2,
            "expected two E033 diagnostics, got: {e033:?}"
        );
        assert!(e033.iter().any(|d| d.message.contains("`a`")));
        assert!(e033.iter().any(|d| d.message.contains("`b`")));
    }

    #[test]
    fn documented_generic_with_all_parameters_is_clean() {
        let source = "\
;! ### pair
;! The `a` and `b` parameters are paired values.
pair<a, b> = [a, b]
";
        let nodes = parse_user_nodes(source);
        let scan = scan_doc_blocks(source);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &nodes, &scan, &config);
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == CODE_GENERIC_PARAM_UNDOCUMENTED),
            "fully-documented generic must not emit E033, got {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn non_generic_documented_definition_is_clean() {
        let source = "\
;! # File title

;! ### simple
simple = 1
";
        let nodes = parse_user_nodes(source);
        let scan = scan_doc_blocks(source);
        let config = DocSemanticsConfig::default();
        let report = check_doc_semantics("", Path::new(""), &nodes, &scan, &config);
        assert!(
            report.is_clean(),
            "non-generic doc must be clean: {report:#?}"
        );
    }

    #[test]
    fn doc_block_covers_line_returns_true_for_matching_definition_line() {
        let scan = make_scan(vec![(make_block(1, 1, &["### widget"]), DocBinding {
            is_file_level: false,
            definition_line: Some(5),
        })]);
        assert!(doc_block_covers_line(&scan, 5));
        assert!(!doc_block_covers_line(&scan, 6));
    }

    #[test]
    fn block_text_joins_lines_with_newlines() {
        let block = make_block(1, 3, &["# Title", "", "body"]);
        assert_eq!(block_text(&block), "# Title\n\nbody");
    }
}
