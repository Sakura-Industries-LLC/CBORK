// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Structural CDDL pretty printer.
//!
//! This is a pure formatter: it parses a CDDL document and re-emits it
//! with canonical spacing and line breaks. It is agnostic of how the
//! document was produced (rendered, hand-written, or flattened) and
//! performs no semantic transformation: the same document always
//! formats to the same bytes.

use cbork_cddl_parser::{cddl, parse_cddl};

use crate::{
    node::WrappedNode,
    preprocessor::{inject_directives, process_ast},
};

/// Format a complete CDDL document canonically.
///
/// Returns the formatted text, or the input unchanged if it does not
/// parse (so callers never lose output).
#[must_use]
pub fn pretty_print(text: &str) -> String {
    let Ok(pairs) = parse_cddl(text) else {
        return text.to_owned();
    };
    let Ok(top) = process_ast(pairs) else {
        return text.to_owned();
    };
    let top: Vec<_> = top
        .into_iter()
        .filter(|p| matches!(p.as_rule(), cddl::Rule::line | cddl::Rule::COMMENT))
        .collect();
    let Ok(nodes) = inject_directives(std::path::Path::new("<pretty>"), &top, text) else {
        return text.to_owned();
    };
    let mut out = String::new();
    for node in &nodes {
        emit_top(node, &mut out);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Emit one top-level element (a rule line or a standalone comment).
fn emit_top(
    node: &WrappedNode,
    out: &mut String,
) {
    match node {
        WrappedNode::RuleLine { children, text, .. } => {
            emit_rule(children, 0, out);
            out.push('\n');
            // Trailing comments live in the line's text but outside the
            // `expr` subtree; preserve them so formatting never drops
            // a comment.
            if let Some(expr) = children.iter().find(|c| syntax_rule(c) == Some("expr")) {
                let expr_text = node_text(expr).trim();
                let full = text.trim();
                if let Some(rest) = full.strip_prefix(expr_text) {
                    for line in rest.lines() {
                        let line = line.trim();
                        if line.starts_with(';') {
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
            }
        },
        WrappedNode::Comment { text, .. } => {
            out.push_str(text.trim());
            out.push('\n');
        },
        _ => {},
    }
}

/// Emit `name = RHS` (or `name := RHS`, `name /= RHS`) with the RHS
/// laid out beneath the head when it spans multiple lines.
fn emit_rule(
    children: &[WrappedNode],
    indent: usize,
    out: &mut String,
) {
    // The parser wraps every rule in an `expr` node (`typename ~
    // genericparm? ~ assignt ~ type`); unwrap it to inspect the parts.
    let parts: &[WrappedNode] = children
        .iter()
        .find(|c| syntax_rule(c) == Some("expr"))
        .and_then(syntax_children)
        .unwrap_or(children);
    let mut name = String::new();
    let mut op = "=";
    let mut rhs: Option<&WrappedNode> = None;
    let mut past_lhs = false;
    for c in parts {
        if let WrappedNode::Syntax { rule, text, .. } = c {
            match rule.as_str() {
                "typename" | "groupname" => {
                    if !past_lhs {
                        text.trim().clone_into(&mut name);
                    }
                },
                "assignt" | "assigng" => {
                    past_lhs = true;
                    op = text.trim();
                },
                "type" | "type1" | "type2" | "grpent" if past_lhs => {
                    rhs = Some(c);
                },
                "genericparm" => name.push_str(text.trim()),
                _ => {},
            }
        }
    }
    indent_line(indent, out);
    out.push_str(&name);
    out.push(' ');
    out.push_str(op);
    out.push(' ');
    if let Some(rhs) = rhs {
        emit_type(rhs, indent, out);
    }
}

/// Write `indent` levels of two-space indentation to `out`.
fn indent_line(
    indent: usize,
    out: &mut String,
) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

/// Emit a `type` node: a choice renders one arm per line.
fn emit_type(
    node: &WrappedNode,
    indent: usize,
    out: &mut String,
) {
    // A group rule body (`name = (...)` with `=`... actually
    // `groupname = grpent`): emit the entry structure directly.
    if syntax_rule(node) == Some("grpent") {
        emit_grpent(node, indent, out);
        return;
    }
    let Some(children) = syntax_children(node) else {
        out.push_str(node_text(node).trim());
        return;
    };
    let type1s: Vec<&WrappedNode> = children
        .iter()
        .filter(|c| syntax_rule(c) == Some("type1"))
        .collect();
    if type1s.len() > 1 {
        let mut first = true;
        for t1 in type1s {
            if !first {
                out.push_str(" /\n");
                indent_line(indent.saturating_add(1), out);
            }
            first = false;
            emit_type1(t1, indent.saturating_add(1), out);
        }
    } else if let Some(t1) = type1s.first() {
        emit_type1(t1, indent, out);
    } else if let Some(first) = children.first() {
        emit_type(first, indent, out);
    } else {
        out.push_str(node_text(node).trim());
    }
}

/// Emit a `type1`: an optional ctlop/rangeop chain, or a single `type2`.
fn emit_type1(
    node: &WrappedNode,
    indent: usize,
    out: &mut String,
) {
    let Some(children) = syntax_children(node) else {
        out.push_str(node_text(node).trim());
        return;
    };
    let type2s: Vec<&WrappedNode> = children
        .iter()
        .filter(|c| syntax_rule(c) == Some("type2"))
        .collect();
    let mut operator: Option<&str> = None;
    for c in children {
        if syntax_rule(c).is_some_and(|r| r == "ctlop" || r == "rangeop") {
            operator = Some(node_text(c).trim());
        }
    }
    if type2s.is_empty() {
        out.push_str(node_text(node).trim());
        return;
    }
    let Some(&left) = type2s.first() else {
        return;
    };
    if let Some(op) = operator {
        emit_type2(left, indent, out);
        out.push(' ');
        out.push_str(op);
        out.push(' ');
        if let Some(right) = type2s.get(1) {
            emit_type2(right, indent, out);
        } else {
            emit_type2(left, indent, out);
        }
    } else {
        emit_type2(left, indent, out);
    }
}

/// Emit a `type2`: delimited groups expand to multi-line blocks; tags,
/// generics, and atoms stay verbatim.
fn emit_type2(
    node: &WrappedNode,
    indent: usize,
    out: &mut String,
) {
    let Some(children) = syntax_children(node) else {
        out.push_str(node_text(node).trim());
        return;
    };
    let text = node_text(node).trim();
    let open = text.chars().next().filter(|c| matches!(c, '{' | '[' | '('));
    let close = text
        .chars()
        .next_back()
        .filter(|c| matches!(c, '}' | ']' | ')'));
    if let (Some(open), Some(close)) = (open, close) {
        // `{ ... }` and `[ ... ]` are always groups. A `( ... )` may be
        // a group or a parenthesized type choice; a parenthesized type
        // keeps its parens (they affect ctlop/choice binding) and is
        // handled by the inner-type path below.
        let has_type_child = children
            .iter()
            .any(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2"));
        if (open != '(' || !has_type_child)
            && let Some(group) = children.iter().find(|c| syntax_rule(c) == Some("group"))
        {
            emit_block(open, group, close, indent, out);
            return;
        }
        // A parenthesized type choice: `(` + choice + `)`. The parens
        // are kept when they carry structure (a choice or a ctlop
        // expression needs its operand scope), and dropped when they
        // are redundant — a single bare operand (`(tstr)`) is the same
        // as `tstr`, so the canonical form strips the parens. Without
        // this normalization, a renderer path that emits the parens
        // and one that does not would disagree on the second render.
        if open == '('
            && has_type_child
            && let Some(inner) = children.iter().find(|c| {
                matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2")
            })
        {
            let mut inner_out = String::new();
            emit_type(inner, indent, &mut inner_out);
            let inner_text = inner_out.trim();
            let redundant = !inner_text.contains('/')
                && !inner_text.contains('\n')
                && !inner_text.contains("..")
                && !inner_text.contains(" .")
                && !inner_text.starts_with('{')
                && !inner_text.starts_with('[')
                && !inner_text.starts_with('&');
            if redundant {
                out.push_str(inner_text);
            } else {
                out.push('(');
                out.push_str(inner_text);
                out.push(')');
            }
            return;
        }
    }
    // Tags (`#6.37(type)`), generics, and atoms: format the inner type
    // if present, otherwise keep the text.
    if let Some(inner) = children.iter().find(|c| {
        matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2")
    }) {
        let Some(head) = leading_head(text) else {
            emit_type(inner, indent, out);
            return;
        };
        out.push_str(&head);
        out.push('(');
        emit_type(inner, indent, out);
        out.push(')');
        return;
    }
    out.push_str(text);
}

/// Emit a `{ ... }`, `[ ... ]`, or `( ... )` group with one entry per
/// line.
fn emit_block(
    open: char,
    group: &WrappedNode,
    close: char,
    indent: usize,
    out: &mut String,
) {
    out.push(open);
    out.push('\n');
    let entries = group_entries(group);
    let separator = group_separator(group);
    if text_has_comment(node_text(group)) {
        // Comments inside a group are consumed as whitespace by the
        // parser and are absent from the tree; preserve the block's
        // original text so formatting never drops a comment.
        out.push_str(node_text(group).trim_end());
        out.push('\n');
    } else if entries.is_empty() {
        // `[]` / `{}` / `()` — no content, no marker (a `//` token is
        // not valid CDDL).
    } else {
        for (i, entry) in entries.iter().enumerate() {
            indent_line(indent.saturating_add(1), out);
            let before = out.len();
            emit_grpent(entry, indent.saturating_add(1), out);
            let is_last = i.saturating_add(1) == entries.len();
            // A trailing `;` comment must come after the separator. Only
            // a single-line entry can carry a trailing comment: `;`
            // inside a nested block (an `&(...)` enum, a multi-line
            // choice) is part of the structure and must not be split.
            let emitted = out.get(before..).unwrap_or_default().to_owned();
            if is_last {
                out.push(',');
                out.push('\n');
            } else if let Some(comment_idx) = emitted.find(';') {
                // Only a single-line entry can carry a trailing comment:
                // `;` inside a nested block (an `&(...)` enum, a
                // multi-line choice) is part of the structure.
                if !emitted.contains('\n') {
                    let (code, comment) = emitted.split_at(comment_idx);
                    out.truncate(before);
                    out.push_str(code.trim_end());
                    out.push_str(separator);
                    out.push(' ');
                    out.push_str(comment.trim());
                    out.push('\n');
                    continue;
                }
                out.push_str(separator);
                out.push('\n');
            } else {
                out.push_str(separator);
                out.push('\n');
            }
        }
    }
    indent_line(indent, out);
    out.push(close);
}

/// Emit one group entry: occurrences, member keys, and the value.
fn emit_grpent(
    node: &WrappedNode,
    indent: usize,
    out: &mut String,
) {
    let Some(children) = syntax_children(node) else {
        out.push_str(node_text(node).trim());
        return;
    };
    let mut occur: Option<&str> = None;
    let mut memberkey: Option<&WrappedNode> = None;
    let mut types: Vec<&WrappedNode> = Vec::new();
    let mut ctlop: Option<&str> = None;
    for c in children {
        match syntax_rule(c) {
            Some("occur") => occur = Some(node_text(c).trim()),
            Some("memberkey") => memberkey = Some(c),
            Some("type" | "type1" | "type2") => types.push(c),
            Some("ctlop") => ctlop = Some(node_text(c).trim()),
            _ => {},
        }
    }
    if let Some(o) = occur {
        out.push_str(o);
        out.push(' ');
    }
    if let Some(mk) = memberkey {
        emit_memberkey(mk, out);
        out.push(' ');
    }
    match (types.as_slice(), ctlop) {
        ([lhs, rhs], Some(op)) => {
            emit_type(lhs, indent, out);
            out.push(' ');
            out.push_str(op);
            out.push(' ');
            emit_type(rhs, indent, out);
        },
        ([single], _) => emit_type(single, indent, out),
        // A grpent whose only structure is a parenthesized group
        // (`(a => int, ...)`, optionally occurrence-wrapped) has no
        // type child; the parens are the group's delimiters.
        _ => {
            if let Some(group) = children.iter().find(|c| syntax_rule(c) == Some("group")) {
                emit_block('(', group, ')', indent, out);
                return;
            }
            out.push_str(node_text(node).trim());
        },
    }
}

/// Emit a member key: `key =>` / `key :` with the operator text.
fn emit_memberkey(
    node: &WrappedNode,
    out: &mut String,
) {
    let text = node_text(node).trim();
    let key = text
        .strip_suffix("=>")
        .or_else(|| text.strip_suffix(':'))
        .or_else(|| text.strip_suffix('~'))
        .unwrap_or(text);
    out.push_str(key.trim());
    if text.ends_with("=>") {
        out.push_str(" =>");
    } else if text.ends_with(':') {
        out.push(':');
    } else if text.ends_with('~') {
        out.push_str(" ~");
    }
}

/// Return the syntax children of a `Syntax` node, if any.
fn syntax_children(node: &WrappedNode) -> Option<&[WrappedNode]> {
    match node {
        WrappedNode::Syntax { children, .. } => Some(children),
        _ => None,
    }
}

/// Return the node's source text verbatim (empty for nodes without text).
fn node_text(node: &WrappedNode) -> &str {
    match node {
        WrappedNode::Syntax { text, .. } | WrappedNode::RuleLine { text, .. } => text,
        _ => "",
    }
}

/// Return the syntax rule name of a `Syntax` node, if any.
fn syntax_rule(node: &WrappedNode) -> Option<&str> {
    match node {
        WrappedNode::Syntax { rule, .. } => Some(rule.as_str()),
        _ => None,
    }
}

/// True if `text` contains a CDDL comment (`;`) outside string
/// literals.
fn text_has_comment(text: &str) -> bool {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_double {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'\'' {
                in_single = false;
            }
            continue;
        }
        match byte {
            b'"' => in_double = true,
            b'\'' => in_single = true,
            b';' => return true,
            _ => {},
        }
    }
    false
}

/// Determine a group's entry separator.
///
/// CDDL group entries are always comma-separated (the grammar's
/// `grpchoice = (grpent optcom)*`; `/` only appears inside a value
/// choice or between grpchoices via `//`). A text scan for `/` would
/// misfire on member values like `6 / 17`, so the separator is
/// unconditionally the comma.
fn group_separator(_group: &WrappedNode) -> &'static str {
    ","
}

/// Collect the grpent entries of a group node (skipping commas).
fn group_entries(group: &WrappedNode) -> Vec<&WrappedNode> {
    let mut out = Vec::new();
    let Some(children) = syntax_children(group) else {
        return out;
    };
    for c in children {
        if let WrappedNode::Syntax { rule, .. } = c {
            match rule.as_str() {
                "grpent" => out.push(c),
                "group" | "grpchoice" => out.extend(group_entries(c)),
                _ => {},
            }
        }
    }
    out
}

/// Extract the text before a `(`/`[`/`{` opener (a tag head like
/// `#6.37`), if the node text has a balanced opener.
fn leading_head(text: &str) -> Option<String> {
    let idx = text
        .char_indices()
        .find(|(_, c)| matches!(c, '(' | '[' | '{'))?
        .0;
    let head = text.get(..idx).unwrap_or_default().trim();
    if head.is_empty() {
        None
    } else {
        Some(head.to_owned())
    }
}
