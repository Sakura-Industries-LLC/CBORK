// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! A CDDL AST preprocessor.
//!
//! - Validates the root rule of the AST to be a `cddl` rule.
//! - Passes through all children without filtering so `COMMENT` nodes (when enabled by
//!   the grammar) are preserved in parse order.
//! - Injects parsed module directives into the AST.

use anyhow::{anyhow, ensure};
use cbork_cddl_parser::cddl;
use pest::iterators::Pair;

use crate::node::{SourceOrigin, WrappedNode};

/// Processes the AST: validates the root rule and returns the raw `line`
/// / `COMMENT` pairs.
///
/// # Errors
///
/// Returns an error if the root rule is missing or not a `cddl` rule.
pub fn process_ast(ast: Vec<Pair<'_, cddl::Rule>>) -> anyhow::Result<Vec<Pair<'_, cddl::Rule>>> {
    validate_root(ast, cddl::Rule::cddl)
}

/// Validate the root rule and return all `line` children as-is (do not
/// filter). Each `line` pair wraps either an `expr` or a `COMMENT` node,
/// preserving interleaved parse order for downstream processing.
fn validate_root(
    ast: Vec<Pair<'_, cddl::Rule>>,
    root_rule: cddl::Rule,
) -> anyhow::Result<Vec<Pair<'_, cddl::Rule>>> {
    let mut ast_iter = ast.into_iter();
    let ast_root = ast_iter.next().ok_or(anyhow!("Empty AST."))?;
    ensure!(
        ast_root.as_rule() == root_rule && ast_iter.next().is_none(),
        "AST must have only one root rule, which must be a `{root_rule:?}` rule."
    );
    Ok(ast_root.into_inner().collect())
}

// ---------------------------------------------------------------------------
// Directive injection pass
// ---------------------------------------------------------------------------

/// Inject parsed module directives into the AST alongside their surrounding
/// nodes.
///
/// This pass scans COMMENT nodes for module directive comments (`;# ...`),
/// parses them using the directive parser, and emits `ModuleStart`,
/// `Directive`, and `ModuleEnd` wrapper nodes in their place.
/// Non-directive comments are preserved as-is.
///
/// The returned node list preserves original source order.
///
/// # Errors
///
/// Returns an error if directive parsing fails. The error message reports
/// the parse failure and the line number within the comment block.
pub fn inject_directives(
    source_path: &std::path::Path,
    pairs: &[Pair<'_, cddl::Rule>],
    source_text: &str,
) -> anyhow::Result<Vec<WrappedNode>> {
    // The pairs may come from a different input than `source_text`
    // (the postlude is injected with an empty source string); the
    // cursor must scan the pairs' actual input to stay in bounds.
    let input = pairs.first().map_or("", |pair| pair.as_span().get_input());
    let mut cursor = LineColCursor::new(input);
    let nodes = build_nodes(
        source_path,
        pairs
            .iter()
            .filter(|pair| matches!(pair.as_rule(), cddl::Rule::line | cddl::Rule::COMMENT))
            .cloned(),
        &mut cursor,
    )?;
    inject_directives_into_nodes(nodes, source_text)
}

/// Tracks line/column positions while walking pest pairs in source
/// order.
///
/// pest's `Position::line_col()` is O(position) per call, which makes
/// node construction quadratic for large inputs (a deeply nested
/// 94 KB schema took ~3 s in `build_nodes`). Walking the source forward
/// once, byte by byte, computes every line/column in O(n) total with
/// the same semantics as pest (CRLF counts as one newline; columns
/// count characters).
/// Walks the source forward while pest pairs are consumed in source
/// order, computing (line, column) for every node start in O(n) total.
struct LineColCursor<'a> {
    /// The full input the pairs were parsed from.
    source: &'a str,
    /// Byte offset the cursor has advanced to.
    pos: usize,
    /// 1-based line at `pos`.
    line: usize,
    /// 1-based column at `pos`.
    column: usize,
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "Counters track a single forward pass; `target` is a char boundary from pest"
)]
impl<'a> LineColCursor<'a> {
    /// Create a cursor at the start of `source`.
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            column: 1,
        }
    }

    /// Advance to `target` (a char boundary) and return its 1-based
    /// (line, column).
    fn advance_to(
        &mut self,
        target: usize,
    ) -> (usize, usize) {
        let slice = &self.source[self.pos..target];
        let mut chars = slice.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                        self.line += 1;
                        self.column = 1;
                    } else {
                        self.column += 1;
                    }
                },
                '\n' => {
                    self.line += 1;
                    self.column = 1;
                },
                _ => self.column += 1,
            }
        }
        self.pos = target;
        (self.line, self.column)
    }
}

/// Build a recursive `RuleLine` node from a top-level `line` pair.
fn build_line_node(
    source_path: &std::path::Path,
    pair: Pair<'_, cddl::Rule>,
    cursor: &mut LineColCursor<'_>,
) -> anyhow::Result<WrappedNode> {
    let span = pair.as_span();
    let (line, column) = cursor.advance_to(span.start());
    let text = pair.as_str().to_owned();
    let children = build_children(source_path, pair.into_inner(), cursor)?;

    Ok(WrappedNode::RuleLine {
        text,
        span: span.start()..span.end(),
        origin: SourceOrigin::new(source_path.to_path_buf(), line, column),
        children,
        metadata: Vec::new(),
    })
}

/// Convert a slice of pest pairs into wrapped nodes.
fn build_nodes<'a, I>(
    source_path: &std::path::Path,
    pairs: I,
    cursor: &mut LineColCursor<'_>,
) -> anyhow::Result<Vec<WrappedNode>>
where
    I: IntoIterator<Item = Pair<'a, cddl::Rule>>,
{
    pairs
        .into_iter()
        .map(|pair| build_node(source_path, pair, cursor))
        .collect()
}

/// Convert a pest `Pairs` iterator into wrapped child nodes.
fn build_children(
    source_path: &std::path::Path,
    pairs: pest::iterators::Pairs<'_, cddl::Rule>,
    cursor: &mut LineColCursor<'_>,
) -> anyhow::Result<Vec<WrappedNode>> {
    build_nodes(source_path, pairs, cursor)
}

/// Convert a single nested pest pair into a wrapped node.
fn build_node(
    source_path: &std::path::Path,
    pair: Pair<'_, cddl::Rule>,
    cursor: &mut LineColCursor<'_>,
) -> anyhow::Result<WrappedNode> {
    let rule = pair.as_rule();
    let span = pair.as_span();
    let (line, column) = cursor.advance_to(span.start());
    let text = pair.as_str().to_owned();

    Ok(match rule {
        cddl::Rule::line => build_line_node(source_path, pair, cursor)?,
        cddl::Rule::COMMENT => {
            WrappedNode::Comment {
                text,
                span: span.start()..span.end(),
                origin: SourceOrigin::new(source_path.to_path_buf(), line, column),
                metadata: Vec::new(),
            }
        },
        _ => {
            WrappedNode::Syntax {
                rule: format!("{rule:?}"),
                text,
                span: span.start()..span.end(),
                origin: SourceOrigin::new(source_path.to_path_buf(), line, column),
                children: build_children(source_path, pair.into_inner(), cursor)?,
                metadata: Vec::new(),
            }
        },
    })
}

/// Recursively inject module directives into a fully built wrapped AST.
///
/// `source_text` is the full pre-transform CDDL source. It is used only
/// to classify the position of `;#`-style marker comments so that
/// trailing markers (after non-whitespace CDDL source on the same
/// line) are preserved as ordinary comments and never interpreted as
/// include/import directives.
#[allow(
    clippy::too_many_lines,
    reason = "Recursive AST traversal; the size matches the node enum."
)]
fn inject_directives_into_nodes(
    nodes: Vec<WrappedNode>,
    source_text: &str,
) -> anyhow::Result<Vec<WrappedNode>> {
    use cbork_cddl_parser::modules::parse_directives;

    use crate::marker::is_trailing_marker_comment;

    let mut out = Vec::new();

    for node in nodes {
        match node {
            WrappedNode::Comment {
                text,
                span,
                origin,
                metadata,
            } => {
                if is_trailing_marker_comment(&text, &origin, source_text) {
                    out.push(WrappedNode::Comment {
                        text,
                        span,
                        origin,
                        metadata,
                    });
                    continue;
                }
                let directives =
                    parse_directives(&text).map_err(|e| anyhow!("directive parse error: {e}"))?;

                if directives.is_empty() {
                    out.push(WrappedNode::Comment {
                        text,
                        span,
                        origin,
                        metadata,
                    });
                } else {
                    for dir in directives {
                        out.push(WrappedNode::ModuleStart {
                            text: WrappedNode::module_start_marker(&dir),
                            origin: origin.clone(),
                            metadata: Vec::new(),
                        });
                        out.push(WrappedNode::Directive {
                            directive: dir,
                            source_comment: text.clone(),
                            span: span.clone(),
                            origin: origin.clone(),
                            children: Vec::new(),
                            metadata: Vec::new(),
                        });
                        if let Some(WrappedNode::Directive { directive: d, .. }) = out.last() {
                            out.push(WrappedNode::ModuleEnd {
                                text: WrappedNode::module_end_marker(d),
                                origin: origin.clone(),
                                metadata: Vec::new(),
                            });
                        }
                    }
                }
            },
            WrappedNode::RuleLine {
                text,
                span,
                origin,
                children,
                metadata,
            } => {
                let children = inject_directives_into_nodes(children, source_text)?;
                out.push(WrappedNode::RuleLine {
                    text,
                    span,
                    origin,
                    children,
                    metadata,
                });
            },
            WrappedNode::Syntax {
                rule,
                text,
                span,
                origin,
                children,
                metadata,
            } => {
                let children = inject_directives_into_nodes(children, source_text)?;
                out.push(WrappedNode::Syntax {
                    rule,
                    text,
                    span,
                    origin,
                    children,
                    metadata,
                });
            },
            WrappedNode::Directive {
                directive,
                source_comment,
                span,
                origin,
                children,
                metadata,
            } => {
                let children = inject_directives_into_nodes(children, source_text)?;
                out.push(WrappedNode::Directive {
                    directive,
                    source_comment,
                    span,
                    origin,
                    children,
                    metadata,
                });
            },
            other => out.push(other),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cbork_cddl_parser::{
        modules::{Directive, FileName},
        parse_cddl, parse_postlude,
    };

    use super::*;

    #[test]
    fn comments_preserved_in_parse_order() {
        let input = "; top comment\nfoo = bar\n; between\nbaz = qux\n";
        let ast = parse_cddl(input).unwrap();
        let ast = process_ast(ast).unwrap();
        let nodes = inject_directives(Path::new("test.cddl"), &ast, input).unwrap();

        let mut comment_count = 0usize;
        let mut line_count = 0usize;
        collect_kind_counts(&nodes, &mut line_count, &mut comment_count);

        assert!(line_count >= 2, "expected at least 2 rule lines");
        assert!(comment_count >= 2, "expected at least 2 COMMENT nodes");
    }

    #[test]
    fn inject_import_directive() {
        let input = ";# import rfc9052\nfoo = bar\n";
        let ast = parse_cddl(input).unwrap();
        let ast = process_ast(ast).unwrap();
        let nodes = inject_directives(Path::new("test.cddl"), &ast, input).unwrap();

        assert!(matches!(nodes[0], WrappedNode::ModuleStart { .. }));
        assert!(matches!(nodes[1], WrappedNode::Directive { .. }));
        assert!(matches!(nodes[2], WrappedNode::ModuleEnd { .. }));
    }

    #[test]
    fn inject_preserves_non_directive_comments() {
        let input = "; regular comment\nfoo = bar\n";
        let ast = parse_cddl(input).unwrap();
        let ast = process_ast(ast).unwrap();
        let nodes = inject_directives(Path::new("test.cddl"), &ast, input).unwrap();

        assert!(
            matches!(nodes[0], WrappedNode::Comment { .. }),
            "expected Comment, got {:?}",
            nodes[0].kind_label()
        );
    }

    #[test]
    fn inject_import_as_directive() {
        let input = ";# import rfc9052 as cose\nfoo = bar\n";
        let ast = parse_cddl(input).unwrap();
        let ast = process_ast(ast).unwrap();
        let nodes = inject_directives(Path::new("test.cddl"), &ast, input).unwrap();

        if let WrappedNode::Directive { ref directive, .. } = nodes[1] {
            assert_eq!(directive, &Directive::ImportAs {
                filename: FileName::WellKnown("rfc9052".to_owned()),
                alias: "cose".to_owned()
            });
        } else {
            panic!("expected Directive");
        }
    }

    #[test]
    fn parse_postlude_is_available() {
        let postlude = parse_postlude().unwrap();
        assert!(!postlude.is_empty());
    }

    fn collect_kind_counts(
        nodes: &[WrappedNode],
        line_count: &mut usize,
        comment_count: &mut usize,
    ) {
        for node in nodes {
            match node {
                WrappedNode::RuleLine { children, .. } => {
                    *line_count = line_count.wrapping_add(1);
                    collect_kind_counts(children, line_count, comment_count);
                },
                WrappedNode::Comment { .. } => {
                    *comment_count = comment_count.wrapping_add(1);
                },
                WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
                    collect_kind_counts(children, line_count, comment_count);
                },
                WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
            }
        }
    }
}
