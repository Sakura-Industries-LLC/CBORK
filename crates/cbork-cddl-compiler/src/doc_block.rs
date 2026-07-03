// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Documentation block scanner.
//!
//! Scans the captured pre-transform CDDL text line by line and builds
//! "documentation blocks" from contiguous runs of standalone `;!` lines.
//! This is the front end of the optional documentation linting pass
//! (`cbork lint --doc`).
//!
//! # Binding model
//!
//! The scanner follows the association rules from
//! `crates/cbork/plan.md` § *Documentation binding model*:
//!
//! * A documentation block is a contiguous run of `;!` lines.
//! * Blank lines or CDDL definitions break documentation contiguity.
//! * `;@` directive comments, include/import comments, and regular `;` comments do not
//!   break documentation contiguity.
//! * A documentation block documents the next CDDL definition when no blank line or prior
//!   CDDL definition separates the block from that definition.
//! * Directives placed between a documentation block and a definition still apply
//!   normally and do not steal the documentation.
//! * Regular comments placed between a documentation block and a definition remain source
//!   comments and do not become documentation.
//! * The first documentation block before any other non-whitespace source content is the
//!   file/module-level documentation block.
//!
//! # Inline doc comments inside multi-line rules
//!
//! A `;!` comment that sits between `{` and `}` (or `[` and `]`) on its
//! own line inside a CDDL rule body is still a doc line. Such a
//! comment is part of the rule's Markdown documentation, not an
//! ordinary source comment. The only `;!` lines that are *not* doc
//! lines are the trailing ones handled by the marker-misuse check
//! in [`crate::marker`] — `rule = 1  ;! trailing comment` is just a
//! regular CDDL comment that happens to start with `;!`.

use std::ops::Range;

/// A single documentation line — the text of a `;!` line with the
/// `;!` marker stripped, preserving indentation after the marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLine {
    /// 1-based source line number of the original `;!` line.
    pub line: usize,
    /// Text after the stripped `;!` marker (whitespace after `;!` is
    /// preserved verbatim so the Markdown content is reproduced exactly).
    pub text: String,
}

/// A contiguous run of standalone `;!` lines (and the doc lines that
/// they contain, with their original source line numbers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocBlock {
    /// 1-based source line number of the first `;!` line in the block.
    pub start_line: usize,
    /// 1-based source line number of the last `;!` line in the block.
    pub end_line: usize,
    /// The doc lines in source order.
    pub lines: Vec<DocLine>,
}

/// Where a documentation block is bound to.
///
/// `is_file_level` is `true` for the first doc block in the file.
/// `definition_line` is `Some(n)` when the block documents the CDDL
/// definition on source line `n` (1-based).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocBinding {
    /// Whether this block is the file/module-level documentation block.
    pub is_file_level: bool,
    /// The 1-based source line of the definition this block documents,
    /// or `None` when the block is orphan.
    pub definition_line: Option<usize>,
}

/// Result of [`scan_doc_blocks`]: every documentation block in the
/// source file together with its binding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocScan {
    /// Documentation blocks in source order.
    pub blocks: Vec<DocBlock>,
    /// Binding for each block, in the same order as `blocks`.
    pub bindings: Vec<DocBinding>,
}

impl DocScan {
    /// Returns `true` when no documentation blocks were found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns the number of documentation blocks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Iterate `(block, binding)` pairs in source order.
    pub fn iter(&self) -> impl Iterator<Item = (&DocBlock, &DocBinding)> {
        self.blocks.iter().zip(self.bindings.iter())
    }
}

/// Classification of a single source line for the purposes of doc-block
/// contiguity and binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClass {
    /// Standalone `;!` documentation line (after leading whitespace).
    DocLine,
    /// Other standalone comment line (`;`, `;@`, `;#`).
    OtherComment,
    /// Whitespace-only line.
    Blank,
    /// Anything else: a CDDL definition line (or a continuation of one).
    Definition,
}

/// Classify a single source line by its leading content.
///
/// The classifier only inspects characters; the AST is not consulted.
/// A `;!` comment that is physically inside a multi-line CDDL construct
/// (e.g. between `{` and `}`) will still be classified as [`LineClass::DocLine`]
/// because this text-based scanner cannot see brace nesting.
pub fn classify_line(line: &str) -> LineClass {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        LineClass::Blank
    } else if trimmed.starts_with(";!") {
        LineClass::DocLine
    } else if trimmed.starts_with(';') {
        LineClass::OtherComment
    } else {
        LineClass::Definition
    }
}

/// Strip the standalone `;!` marker from a doc line and return the
/// remaining Markdown text.
///
/// Only the two-character `;!` marker is removed. The leading
/// whitespace before the marker is also discarded because it is not
/// part of the Markdown content; any whitespace between the marker and
/// the user's text is preserved verbatim as the "indentation after
/// the marker".
pub fn strip_doc_marker(line: &str) -> String {
    let trimmed = line.trim_start();
    // The caller only invokes this on lines classified as `DocLine`,
    // which guarantees `trimmed` begins with `;!`. Advance one
    // character at a time so multi-byte boundaries are respected.
    let mut chars = trimmed.chars();
    let _semi = chars.next();
    let _bang = chars.next();
    chars.as_str().to_owned()
}

/// Scan `source_text` line by line and return all documentation blocks
/// with their bindings.
///
/// The function is line-based and operates on a single pre-transform
/// CDDL buffer. Pass the full file contents (not a synthetic substring)
/// so that source line numbers in the result match the user's editor.
#[must_use]
pub fn scan_doc_blocks(source_text: &str) -> DocScan {
    let lines: Vec<&str> = source_text.split('\n').collect();
    let classes: Vec<LineClass> = lines.iter().map(|l| classify_line(l)).collect();

    let mut scan = DocScan::default();
    let mut idx = 0usize;
    let mut found_first_block = false;
    let mut saw_definition_before_first_block = false;

    while let Some(class) = classes.get(idx).copied() {
        if class != LineClass::DocLine {
            // Track definitions that appear *before* the first doc
            // block so we can mark that block as the file-level doc.
            if !found_first_block && class == LineClass::Definition {
                saw_definition_before_first_block = true;
            }
            idx = idx.saturating_add(1);
            continue;
        }

        let mut doc_lines: Vec<DocLine> = Vec::new();

        // Collect doc lines until the contiguity rule breaks:
        // blank lines and CDDL definitions end the block; other
        // comments are transparent.
        while let Some(inner) = classes.get(idx).copied() {
            if inner == LineClass::Blank || inner == LineClass::Definition {
                break;
            }
            if inner == LineClass::DocLine {
                let line_number = idx.saturating_add(1);
                let text = strip_doc_marker(lines.get(idx).copied().unwrap_or_default());
                doc_lines.push(DocLine {
                    line: line_number,
                    text,
                });
            }
            idx = idx.saturating_add(1);
        }

        let Some(first_line) = doc_lines.first() else {
            // Empty block should not happen because we entered this
            // branch on a DocLine, but guard against future drift.
            continue;
        };
        let start_line = first_line.line;
        let end_line = doc_lines.last().map_or(start_line, |l| l.line);

        let definition_line = find_binding_definition(&classes, idx);
        let binding = DocBinding {
            is_file_level: false,
            definition_line,
        };

        scan.blocks.push(DocBlock {
            start_line,
            end_line,
            lines: doc_lines,
        });
        scan.bindings.push(binding);
        found_first_block = true;

        // `idx` already points at the first non-transparent line
        // (Blank, Definition, or EOF). The outer loop will move past
        // it on the next iteration.
    }

    // The first doc block is the file/module-level doc only when no
    // CDDL definition precedes it.
    if !saw_definition_before_first_block && let Some(first) = scan.bindings.first_mut() {
        first.is_file_level = true;
    }

    scan
}

/// Search forward from `start_idx` for the next CDDL definition that the
/// documentation block can bind to.
///
/// Returns the 1-based source line of the definition, or `None` when
/// the binding attempt is interrupted by a blank line, runs off the
/// end of the file, or hits another `;!` line that would extend the
/// block (treated as a break for already-collected blocks).
fn find_binding_definition(
    classes: &[LineClass],
    start_idx: usize,
) -> Option<usize> {
    let mut idx = start_idx;
    while let Some(class) = classes.get(idx).copied() {
        match class {
            LineClass::Blank | LineClass::DocLine => return None,
            LineClass::Definition => return Some(idx.saturating_add(1)),
            LineClass::OtherComment => {
                idx = idx.saturating_add(1);
            },
        }
    }
    None
}

/// Return the inclusive source line range for `block`.
///
/// Useful for diagnostic rendering and source mapping.
#[must_use]
pub fn doc_block_range(block: &DocBlock) -> Range<usize> {
    block.start_line..block.end_line.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_blank_line() {
        assert_eq!(classify_line(""), LineClass::Blank);
        assert_eq!(classify_line("   "), LineClass::Blank);
        assert_eq!(classify_line("\t\t"), LineClass::Blank);
    }

    #[test]
    fn classify_doc_line_with_leading_whitespace() {
        assert_eq!(classify_line(";! # Title"), LineClass::DocLine);
        assert_eq!(classify_line("    ;! indented"), LineClass::DocLine);
        assert_eq!(classify_line(";!"), LineClass::DocLine);
    }

    #[test]
    fn classify_other_comment() {
        assert_eq!(classify_line("; regular"), LineClass::OtherComment);
        assert_eq!(classify_line(";@ CBORK: Library"), LineClass::OtherComment);
        assert_eq!(
            classify_line(";# include \"./x.cddl\""),
            LineClass::OtherComment
        );
    }

    #[test]
    fn classify_definition() {
        assert_eq!(classify_line("rule = 1"), LineClass::Definition);
        assert_eq!(
            classify_line("rule = 1  ;! trailing doc"),
            LineClass::Definition
        );
        assert_eq!(classify_line("  nested: tstr,"), LineClass::Definition);
    }

    #[test]
    fn classify_inline_doc_comment_inside_brace_block() {
        // A `;!` line that sits between `{` and `}` on its own line is
        // documentation for the enclosing rule's fields. The scanner
        // must classify it as a doc line so its Markdown reaches the
        // renderer.
        assert_eq!(
            classify_line("  ;! # field description"),
            LineClass::DocLine
        );
        assert_eq!(
            classify_line("    ;!    indented field doc"),
            LineClass::DocLine
        );
        // The closing `}` is still a definition line, not a doc line.
        assert_eq!(classify_line("}"), LineClass::Definition);
    }

    #[test]
    fn scan_inline_doc_comments_inside_a_map_body() {
        // Inline `;!` comments are picked up as doc lines even when
        // they live between the `{` and `}` of a map body. The block
        // is interrupted by the first non-comment content (`"name":`)
        // so the inline docs form their own block.
        let source = "\
rule = {
  ;! # field documentation
  ;! describes the `name` field
  \"name\" => tstr,
  ;! describes the `age` field
  ? \"age\" => uint,
}
";
        let scan = scan_doc_blocks(source);

        // Two doc blocks: one per field. Both bind to the enclosing
        // `rule = { ... }` definition because no blank line separates
        // them from the rule's opening brace — the only intervening
        // content is the field value lines themselves, which are
        // Definition-classified and therefore break the block but the
        // *second* block (the `age` field) still binds forward to the
        // closing brace's definition start.
        assert_eq!(scan.blocks.len(), 2, "got blocks = {:?}", scan.blocks);
        assert_eq!(scan.blocks[0].lines.len(), 2);
        assert_eq!(scan.blocks[0].lines[0].text, " # field documentation");
        assert_eq!(scan.blocks[0].lines[1].text, " describes the `name` field");
        assert_eq!(scan.blocks[1].lines.len(), 1);
        assert_eq!(scan.blocks[1].lines[0].text, " describes the `age` field");
    }

    #[test]
    fn strip_doc_marker_preserves_indentation() {
        assert_eq!(strip_doc_marker(";! # Title"), " # Title");
        assert_eq!(strip_doc_marker(";!"), "");
        assert_eq!(strip_doc_marker("    ;!   indented"), "   indented");
    }

    #[test]
    fn scan_empty_source() {
        let scan = scan_doc_blocks("");
        assert!(scan.is_empty());
    }

    #[test]
    fn scan_no_doc_blocks() {
        let source = "; just a comment\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert!(scan.is_empty());
    }

    #[test]
    fn scan_single_doc_block_binds_to_definition() {
        let source = ";! # Title\n;! Description\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert_eq!(scan.blocks[0].start_line, 1);
        assert_eq!(scan.blocks[0].end_line, 2);
        assert_eq!(scan.blocks[0].lines.len(), 2);
        assert_eq!(scan.blocks[0].lines[0].text, " # Title");
        assert_eq!(scan.blocks[0].lines[1].text, " Description");
        assert!(scan.bindings[0].is_file_level);
        assert_eq!(scan.bindings[0].definition_line, Some(3));
    }

    #[test]
    fn scan_doc_block_breaks_on_blank_line() {
        let source = ";! # Title\n\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert_eq!(scan.blocks[0].end_line, 1);
        assert!(!scan.bindings[0].is_file_level || scan.bindings[0].definition_line.is_none());
        // Block ends at line 1, then a blank line at line 2 breaks the
        // binding to `rule = 1` on line 3.
        assert_eq!(scan.bindings[0].definition_line, None);
    }

    #[test]
    fn scan_doc_block_skips_intervening_comments() {
        let source =
            ";! # Title\n; regular comment\n;@ CBORK: Library\n; another comment\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert_eq!(scan.blocks[0].end_line, 1);
        assert!(scan.bindings[0].is_file_level);
        assert_eq!(scan.bindings[0].definition_line, Some(5));
    }

    #[test]
    fn scan_doc_block_skips_intervening_include_directive() {
        // `;#` is converted to a `Directive` node by the preprocessor,
        // but in the raw source text the line still begins with `;#`.
        // The text-based scanner must still treat it as a transparent
        // other-comment line.
        let source = ";! # Title\n;# include \"./lib.cddl\"\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert!(scan.bindings[0].is_file_level);
        assert_eq!(scan.bindings[0].definition_line, Some(3));
    }

    #[test]
    fn scan_two_doc_blocks_separated_by_blank_line() {
        let source = ";! # File title\n\n;! ### definition\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 2);

        assert!(scan.bindings[0].is_file_level);
        assert_eq!(scan.bindings[0].definition_line, None);

        assert!(!scan.bindings[1].is_file_level);
        assert_eq!(scan.bindings[1].definition_line, Some(4));
    }

    #[test]
    fn scan_two_doc_blocks_separated_by_definition() {
        let source = ";! # File title\nrule_a = 1\n;! ### definition\nrule_b = 2\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 2);

        assert!(scan.bindings[0].is_file_level);
        // Block 1 is followed by `rule_a = 1` on line 2 with no blank
        // line in between, so it binds.
        assert_eq!(scan.bindings[0].definition_line, Some(2));

        assert!(!scan.bindings[1].is_file_level);
        assert_eq!(scan.bindings[1].definition_line, Some(4));
    }

    #[test]
    fn scan_orphan_doc_block_at_end_of_file() {
        let source = "rule = 1\n;! ### orphan docs\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert!(!scan.bindings[0].is_file_level);
        assert_eq!(scan.bindings[0].definition_line, None);
    }

    #[test]
    fn scan_doc_block_after_definition_does_not_become_file_level() {
        let source = "rule_a = 1\n;! ### orphan\nrule_b = 2\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert!(
            !scan.bindings[0].is_file_level,
            "a doc block that appears after a definition cannot be the file-level doc"
        );
        assert_eq!(scan.bindings[0].definition_line, Some(3));
    }

    #[test]
    fn scan_preserves_blank_line_separation_after_comments() {
        // Doc block → regular comment → blank line → definition.
        // The blank line breaks the binding even though a comment was
        // between the doc block and the blank.
        let source = ";! # Title\n; trailing regular comment\n\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 1);
        assert_eq!(scan.bindings[0].definition_line, None);
    }

    #[test]
    fn scan_file_level_doc_with_internal_blank_line_becomes_two_blocks() {
        let source = ";! # File title\n;!\n\n;! ## Section\n\n;! ### Definition\nrule = 1\n";
        let scan = scan_doc_blocks(source);
        assert_eq!(scan.blocks.len(), 3);
        assert!(scan.bindings[0].is_file_level);
        assert!(!scan.bindings[1].is_file_level);
        assert!(!scan.bindings[2].is_file_level);
    }

    #[test]
    fn doc_block_range_returns_inclusive_range() {
        let block = DocBlock {
            start_line: 3,
            end_line: 5,
            lines: vec![],
        };
        assert_eq!(doc_block_range(&block), 3..6);
    }
}
