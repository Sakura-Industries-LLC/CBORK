// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Validate parsed CBOR against a compiled CDDL schema.
//!
//! The implementation is intentionally conservative: it prefers the compiler's
//! resolved constants where available, falls back to the AST for structural
//! rules, and reports unsupported shapes explicitly instead of silently
//! accepting them.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::map_identity,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_lifetimes,
    clippy::ptr_arg,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unnecessary_find_map
)]

use std::{cell::RefCell, collections::HashMap, fmt::Write as _, path::Path};

use cbork_abnf_parser::parse_abnf;
use cbork_cddl_compiler::{
    CompiledCDDL, DiagnosticLevel, EntryState, WrappedNode, child_text,
    literals::{
        byte::ByteLiteralBytes,
        regex::{RegexLiteral, RegexValidationError},
        text::TextLiteralBytes,
    },
    resolve_type2_leaf,
};
use cbork_edn::{Document, Float, MapEntry, Value};
use console::style;

use crate::{
    decode::{ColorKind, push_bracket, push_colored, push_dim, push_indent, read_input},
    diagnostics::{has_error_diagnostics, print_compiler_diagnostics},
};

thread_local! {
    static CURRENT_SOURCE_BYTES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static CURRENT_SCHEMA_NOTES: RefCell<Vec<SchemaNote>> = const { RefCell::new(Vec::new()) };
}

/// Schema annotation captured during validation for later rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaNote {
    /// Validation path where the note applies.
    path: Vec<PathStep>,
    /// Human-readable annotation text.
    text: String,
}

/// Validate a CDDL schema against a CBOR payload.
pub(crate) fn exec(
    schema_path: &Path,
    cbor_path: Option<&Path>,
    show_warnings: bool,
    detailed: bool,
    force_no_color: bool,
) -> bool {
    let schema_root = schema_path.parent();
    let compiled = match CompiledCDDL::compile(schema_path, schema_root) {
        Ok(compiled) => compiled,
        Err(err) => {
            println!(
                "{} {}:\n{}",
                console::Emoji::new("🚨", "Compile Error"),
                schema_path.display(),
                style(err).red()
            );
            return false;
        },
    };

    let warning_count = compiled
        .warnings
        .iter()
        .filter(|diagnostic| diagnostic.level == DiagnosticLevel::Warning)
        .count();

    if has_error_diagnostics(&compiled.warnings) {
        print_compiler_diagnostics(schema_path, &compiled.warnings, false);
        return false;
    }

    if warning_count > 0 {
        if show_warnings {
            print_compiler_diagnostics(schema_path, &compiled.warnings, false);
        } else {
            println!(
                "{}",
                style(format!("{warning_count} warnings detected")).yellow()
            );
        }
    }

    let input_path =
        cbor_path.map_or_else(|| "<stdin>".to_owned(), |path| path.display().to_string());
    let input = match read_input(cbor_path) {
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

    set_current_source_bytes(&input);
    clear_current_schema_notes();

    let Some(root_name) = root_rule_name(&compiled) else {
        println!(
            "{}",
            style(format!(
                "validation error: no root rule found in {}",
                schema_path.display()
            ))
            .red()
        );
        return false;
    };

    let definitions = collect_definitions(&compiled.complete_nodes);
    let issues = validate_document(&compiled, &definitions, &root_name, &document);
    let schema_notes = take_current_schema_notes();

    if issues.is_empty() {
        if detailed {
            let dump = render_validation_dump(
                schema_path,
                &input_path,
                &document,
                &schema_notes,
                None,
                !force_no_color,
            );
            if force_no_color {
                println!("{dump}");
            } else {
                println!("{}", style(dump).dim());
            }
        }
        println!("OK");
        return true;
    }

    println!(
        "{} {} -> {}",
        console::Emoji::new("🚨", "Errors"),
        schema_path.display(),
        input_path
    );
    let highlight = issues
        .first()
        .map(|issue| issue.path.as_slice())
        .unwrap_or(&[]);
    let dump = render_validation_dump(
        schema_path,
        &input_path,
        &document,
        &schema_notes,
        Some(highlight),
        !force_no_color,
    );
    print!("{dump}");

    for issue in &issues {
        println!(
            "{}",
            style(format!(
                "error: at {}: expected {}, found {}",
                format_path(&issue.path),
                issue.expected,
                issue.found
            ))
            .red()
        );
        if let Some(message) = &issue.message {
            println!("{}", style(format!("  {message}")).red());
        }
    }

    false
}

/// A validation mismatch collected during traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationIssue {
    /// The value location within the decoded document.
    path: Vec<PathStep>,
    /// What the schema expected.
    expected: String,
    /// What the CBOR value contained.
    found: String,
    /// Optional extra context.
    message: Option<String>,
}

impl ValidationIssue {
    /// Build a new issue.
    fn new(
        path: Vec<PathStep>,
        expected: impl Into<String>,
        found: impl Into<String>,
        message: impl Into<Option<String>>,
    ) -> Self {
        Self {
            path,
            expected: expected.into(),
            found: found.into(),
            message: message.into(),
        }
    }
}

/// A location inside the parsed CBOR tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathStep {
    /// Top-level document item.
    DocItem(usize),
    /// Array element.
    ArrayItem(usize),
    /// Map key.
    MapKey(usize),
    /// Map value.
    MapValue(usize),
    /// Tag payload.
    TagInner,
}

/// Validate the whole parsed document against the root schema rule.
fn validate_document(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    root_name: &str,
    document: &Document,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    if document.items().len() > 1 {
        issues.push(ValidationIssue::new(
            vec![PathStep::DocItem(1)],
            "a single top-level CBOR item",
            format!("{} top-level items", document.items().len()),
            Some("CBOR sequence validation is not yet implemented".to_owned()),
        ));
        return issues;
    }

    let Some(item) = document.items().first() else {
        issues.push(ValidationIssue::new(
            Vec::new(),
            "a CBOR item",
            "empty document",
            Some("no top-level CBOR item was parsed".to_owned()),
        ));
        return issues;
    };

    let mut path = vec![PathStep::DocItem(0)];
    validate_named_rule(
        compiled,
        definitions,
        root_name,
        item,
        &mut path,
        &mut issues,
    );

    issues
}

/// Validate one rule by name.
fn validate_named_rule(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    name: &str,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    record_schema_note_once(path, name.to_owned());

    if let Some(state) = compiled
        .resolved_types
        .iter()
        .find_map(|(entry_name, state)| {
            if entry_name == name && state.is_resolved() {
                Some(state)
            } else {
                None
            }
        })
    {
        validate_state(state, value, path, issues);
        return;
    }

    let Some(node) = definitions.get(name) else {
        if is_socket_name(name) {
            issues.push(ValidationIssue::new(
                path.clone(),
                format!("a value accepted by socket `{name}`"),
                format!("{value}"),
                Some("socket has no plugged definitions".to_owned()),
            ));
            return;
        }

        issues.push(ValidationIssue::new(
            path.clone(),
            format!("definition `{name}`"),
            format!("{value}"),
            Some("undefined rule reference".to_owned()),
        ));
        return;
    };

    validate_rule_node(compiled, definitions, node, value, path, issues);
}

/// Validate a rule definition node.
fn validate_rule_node(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    match node {
        WrappedNode::RuleLine { children, .. } => {
            if let Some(rhs) = find_rhs_node(children) {
                validate_schema_node(compiled, definitions, rhs, value, path, issues);
            } else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "a CDDL expression",
                    format!("{value}"),
                    Some("could not find the rule right-hand side".to_owned()),
                ));
            }
        },
        WrappedNode::Syntax { .. } => {
            validate_schema_node(compiled, definitions, node, value, path, issues);
        },
        _ => {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a schema rule",
                format!("{value}"),
                Some(format!("unsupported node kind {}", node.kind_label())),
            ));
        },
    }
}

/// Validate a schema subtree against a CBOR value.
fn validate_schema_node(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    if !matches!(node, WrappedNode::RuleLine { .. })
        && !matches!(path.last(), Some(PathStep::MapKey(_)))
    {
        record_schema_note_once(path, schema_summary(node));
    }

    if let Some(state) = resolve_type2_leaf(node, &compiled.resolved_types)
        && state.is_resolved()
    {
        validate_state(&state, value, path, issues);
        return;
    }

    let WrappedNode::Syntax {
        rule,
        children,
        text,
        ..
    } = node
    else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a CDDL type expression",
            format!("{value}"),
            Some("unsupported schema node".to_owned()),
        ));
        return;
    };

    match rule.as_str() {
        "type" => validate_type_choice(compiled, definitions, children, value, path, issues),
        "type1" => validate_type1(compiled, definitions, children, text, value, path, issues),
        "type2" => validate_type2(compiled, definitions, node, value, path, issues),
        "group" => validate_group(compiled, definitions, node, value, path, issues),
        "grpent" => validate_grpent(compiled, definitions, node, value, path, issues),
        "value" => {
            if let Some(expected) = parse_value_literal(node) {
                validate_state(&expected, value, path, issues);
            } else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "a literal value",
                    format!("{value}"),
                    Some("could not parse the literal".to_owned()),
                ));
            }
        },
        "memberkey" => {
            if let Some(inner) = children.iter().find(|child| {
                matches!(
                    child,
                    WrappedNode::Syntax { rule, .. }
                        if matches!(
                            rule.as_str(),
                            "value" | "type1" | "typename" | "groupname" | "bareword"
                        )
                )
            }) {
                if let WrappedNode::Syntax { rule, text, .. } = inner {
                    if rule == "bareword" {
                        validate_named_rule(
                            compiled,
                            definitions,
                            text.trim(),
                            value,
                            path,
                            issues,
                        );
                    } else {
                        validate_schema_node(compiled, definitions, inner, value, path, issues);
                    }
                }
            } else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "a member key",
                    format!("{value}"),
                    Some("could not interpret the member key".to_owned()),
                ));
            }
        },
        "typename" | "groupname" => {
            let name = text.trim();
            validate_named_rule(compiled, definitions, name, value, path, issues);
        },
        _ => {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a supported CDDL type",
                format!("{value}"),
                Some(format!("unsupported syntax rule `{rule}`")),
            ));
        },
    }
}

/// Validate a `type` node as an ordered set of alternatives.
fn validate_type_choice(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut branch_issues = Vec::new();
    let mut saw_branch = false;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "type1"
        {
            saw_branch = true;
            let mut local_issues = Vec::new();
            validate_schema_node(compiled, definitions, child, value, path, &mut local_issues);
            if local_issues.is_empty() {
                return;
            }
            branch_issues.push(local_issues);
        }
    }

    if saw_branch {
        let found = format!("{value}");
        let expected = "one of the listed alternatives";
        issues.push(ValidationIssue::new(
            path.clone(),
            expected,
            found,
            Some("none of the `type` alternatives matched".to_owned()),
        ));
        issues.extend(branch_issues.into_iter().flatten());
        return;
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a CDDL type alternative",
        format!("{value}"),
        Some("empty `type` node".to_owned()),
    ));
}

/// Validate a `type1` node, including control-operator forms.
fn validate_type1(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    text: &str,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut lhs: Option<&WrappedNode> = None;
    let mut op: Option<&str> = None;
    let mut rhs: Option<&WrappedNode> = None;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if lhs.is_none() => lhs = Some(child),
                "ctlop" => op = Some(child_text(child).trim()),
                "type2" => rhs = Some(child),
                _ => {},
            }
        }
    }

    if let (Some(lhs), Some(op), Some(rhs)) = (lhs, op, rhs) {
        validate_ctlop_value(compiled, definitions, lhs, op, rhs, value, path, issues);
        return;
    }

    if let Some(lhs) = lhs {
        validate_schema_node(compiled, definitions, lhs, value, path, issues);
        return;
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a `type1` expression",
        format!("{value}"),
        Some(format!("could not interpret `{text}`")),
    ));
}

/// Validate a `type2` node.
fn validate_type2(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let WrappedNode::Syntax { text, children, .. } = node else {
        return;
    };

    let trimmed = text.trim_start();

    if trimmed.starts_with('[') {
        validate_array_group(compiled, definitions, children, value, path, issues);
        return;
    }

    if trimmed.starts_with('{') {
        validate_map_group(compiled, definitions, children, value, path, issues);
        return;
    }

    if trimmed.starts_with('#') {
        validate_tag_or_simple(compiled, definitions, node, value, path, issues);
        return;
    }

    if trimmed.starts_with('(') {
        if let Some(inner) = children
            .iter()
            .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type"))
        {
            validate_schema_node(compiled, definitions, inner, value, path, issues);
        } else {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a parenthesized type expression",
                format!("{value}"),
                Some("empty parenthesized type".to_owned()),
            ));
        }
        return;
    }

    if trimmed.starts_with('~')
        && let Some(name) = children.iter().find_map(|child| {
            match child {
                WrappedNode::Syntax { rule, text, .. } if rule == "typename" => {
                    Some(text.trim().to_owned())
                },
                _ => None,
            }
        })
    {
        validate_named_rule(compiled, definitions, &name, value, path, issues);
        return;
    }

    if let Some(builtin) = builtin_type_name(trimmed) {
        validate_builtin(builtin, value, path, issues);
        return;
    }

    if let Some(name) = children.iter().find_map(|child| {
        match child {
            WrappedNode::Syntax { rule, text, .. } if rule == "typename" || rule == "groupname" => {
                Some(text.trim().to_owned())
            },
            _ => None,
        }
    }) {
        validate_named_rule(compiled, definitions, &name, value, path, issues);
        return;
    }

    if let Some(expected) = parse_value_literal(node) {
        validate_state(&expected, value, path, issues);
        return;
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a supported type",
        format!("{value}"),
        Some(format!("unsupported type syntax `{trimmed}`")),
    ));
}

/// Validate a `group` node.
fn validate_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let Value::Array(items) = value else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "an array",
            format!("{value}"),
            Some("group validation currently expects an array value".to_owned()),
        ));
        return;
    };

    let Some(group) = node_children_find(node, "group") else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a group",
            format!("{value}"),
            Some("missing group body".to_owned()),
        ));
        return;
    };

    let mut branch_issues = Vec::new();
    for grpchoice in group_children(group, "grpchoice") {
        let mut local_issues = Vec::new();
        if validate_grpchoice_array(
            compiled,
            definitions,
            grpchoice,
            items,
            path,
            &mut local_issues,
        ) {
            return;
        }
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "an array matching the schema group",
        format!("{value}"),
        Some("none of the group alternatives matched".to_owned()),
    ));
    issues.extend(branch_issues.into_iter().flatten());
}

/// Validate a `grpent` node.
fn validate_grpent(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(grpent) = find_grpent_body(node) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a group element",
            format!("{value}"),
            Some("missing grpent body".to_owned()),
        ));
        return;
    };

    validate_schema_node(compiled, definitions, grpent, value, path, issues);
}

/// Validate a tag or simple-value expression.
fn validate_tag_or_simple(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let WrappedNode::Syntax { text, .. } = node else {
        return;
    };
    let trimmed = text.trim_start();

    if trimmed.starts_with("#6") {
        let Value::Tag(actual_tag, inner_value) = value else {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a tagged CBOR item".to_owned(),
                format!("{value}"),
                Some("expected a tagged CBOR item".to_owned()),
            ));
            return;
        };

        if !head_number_matches_tag(compiled, definitions, node, *actual_tag, path, issues) {
            return;
        }

        if let Some(inner_type) = find_tag_inner_type(node) {
            record_schema_note_once(path, schema_summary(inner_type));
            path.push(PathStep::TagInner);
            validate_schema_node(compiled, definitions, inner_type, inner_value, path, issues);
            path.pop();
        } else {
            issues.push(ValidationIssue::new(
                path.clone(),
                "tag payload schema",
                format!("{inner_value}"),
                Some("missing tag inner schema".to_owned()),
            ));
        }
        return;
    }

    if trimmed.starts_with("#7") {
        if !head_number_matches_simple(compiled, definitions, node, value, path, issues) {
            return;
        }
        return;
    }

    if let Some(simple) = parse_simple_marker(trimmed) {
        match (simple, value) {
            ("null", Value::Null)
            | ("undefined", Value::Undefined)
            | ("true", Value::Bool(true))
            | ("false", Value::Bool(false)) => return,
            _ => {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    simple,
                    format!("{value}"),
                    Some("simple value did not match".to_owned()),
                ));
                return;
            },
        }
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a tagged or simple CBOR item",
        format!("{value}"),
        Some(format!("unsupported tag/simple expression `{trimmed}`")),
    ));
}

/// Validate an array group against a CBOR array.
fn validate_array_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let Value::Array(items) = value else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "an array",
            format!("{value}"),
            Some("expected a CBOR array".to_owned()),
        ));
        return;
    };

    let Some(group) = children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
    else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "an array group",
            format!("{value}"),
            Some("missing array group".to_owned()),
        ));
        return;
    };

    let mut branch_issues = Vec::new();
    for grpchoice in group_children(group, "grpchoice") {
        let mut local_issues = Vec::new();
        if validate_grpchoice_array(
            compiled,
            definitions,
            grpchoice,
            items,
            path,
            &mut local_issues,
        ) {
            return;
        }
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "an array matching the schema",
        format!("{value}"),
        Some("none of the array alternatives matched".to_owned()),
    ));
    issues.extend(branch_issues.into_iter().flatten());
}

/// Validate a map group against a CBOR map.
fn validate_map_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let Value::Map(entries) = value else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a map",
            format!("{value}"),
            Some("expected a CBOR map".to_owned()),
        ));
        return;
    };

    let Some(group) = children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
    else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a map group",
            format!("{value}"),
            Some("missing map group".to_owned()),
        ));
        return;
    };

    let mut branch_issues = Vec::new();
    for grpchoice in group_children(group, "grpchoice") {
        let mut used = vec![false; entries.len()];
        let mut local_issues = Vec::new();
        if validate_grpchoice_map(
            compiled,
            definitions,
            grpchoice,
            entries,
            &mut used,
            path,
            &mut local_issues,
        ) {
            return;
        }
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a map matching the schema",
        format!("{value}"),
        Some("no map alternative matched".to_owned()),
    ));
    issues.extend(branch_issues.into_iter().flatten());
}

/// Validate a `type1` control-operator expression.
fn validate_ctlop_value(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    lhs: &WrappedNode,
    op: &str,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut lhs_issues = Vec::new();
    validate_schema_node(compiled, definitions, lhs, value, path, &mut lhs_issues);
    if !lhs_issues.is_empty() {
        issues.extend(lhs_issues);
        return;
    }

    match op {
        ".size" => {
            let expected =
                resolve_integer_rhs(compiled, rhs).or_else(|| parse_integer_from_node(rhs));
            let Some(expected) = expected else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "an integer size",
                    format!("{value}"),
                    Some("size RHS did not resolve to an integer".to_owned()),
                ));
                return;
            };

            let actual = value_len(value);
            let Some(actual) = actual else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("size {expected}"),
                    format!("{value}"),
                    Some("value has no size".to_owned()),
                ));
                return;
            };

            let size = expected;
            let Ok(expected) = usize::try_from(size) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "a non-negative size",
                    size.to_string(),
                    Some("size RHS must be non-negative".to_owned()),
                ));
                return;
            };

            if actual != expected {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("size {expected}"),
                    format!("size {actual}"),
                    Some("size constraint failed".to_owned()),
                ));
            }
        },
        ".regexp" => {
            let _ = validate_regex_rhs(compiled, rhs, value, path, issues);
        },
        ".abnf" | ".abnfb" | ".x-enc.abnf" | ".x-enc.abnfb" | ".x-hash.abnf" | ".x-hash.abnfb" => {
            let _ = validate_abnf_rhs(compiled, rhs, value, path, issues);
        },
        ".json" => {
            if let Some(text) = value_to_text(value) {
                if TextLiteralBytes::from_bytes(text.as_bytes().to_vec())
                    .validate_json()
                    .is_err()
                {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        "valid JSON text",
                        format!("{value}"),
                        Some("JSON validation failed".to_owned()),
                    ));
                }
            } else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "JSON text",
                    format!("{value}"),
                    Some("value is not text".to_owned()),
                ));
            }
        },
        ".cbor" => {
            let _ = validate_embedded_cbor(compiled, definitions, rhs, value, path, issues, false);
        },
        ".cborseq" => {
            let _ = validate_embedded_cbor(compiled, definitions, rhs, value, path, issues, true);
        },
        ".dtrm" => {
            let _ = validate_deterministic_serialization(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                false,
            );
        },
        ".dtrmseq" => {
            let _ = validate_deterministic_serialization(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                true,
            );
        },
        _ => {
            issues.push(ValidationIssue::new(
                path.clone(),
                format!("supported control operator {op}"),
                format!("{value}"),
                Some("operator validation is not implemented yet".to_owned()),
            ));
        },
    }
}

/// Finds the inner type node of a CDDL tag construct.
fn find_tag_inner_type(node: &WrappedNode) -> Option<&WrappedNode> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };

    children
        .iter()
        .rev()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type"))
}

/// Validate an embedded CBOR payload from a `bstr`.
fn validate_embedded_cbor(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
    allow_sequence: bool,
) -> bool {
    validate_embedded_cbor_with_serialization(
        compiled,
        definitions,
        rhs,
        value,
        path,
        issues,
        allow_sequence,
        false,
    )
}

/// Validate an embedded CBOR payload from a `bstr`.
fn validate_embedded_cbor_with_serialization(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
    allow_sequence: bool,
    deterministic: bool,
) -> bool {
    let start_len = issues.len();
    let Some(bytes) = value_to_bytes(value) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "bytes",
            format!("{value}"),
            Some("embedded CBOR operators expect a byte string".to_owned()),
        ));
        return false;
    };

    let document = match Document::parse(bytes) {
        Ok(document) => document,
        Err(error) => {
            issues.push(ValidationIssue::new(
                path.clone(),
                "embedded CBOR",
                format!("{value}"),
                Some(format!("failed to parse embedded CBOR: {error}")),
            ));
            return false;
        },
    };

    if deterministic {
        match document.to_deterministic_bytes() {
            Ok(encoded) if encoded == bytes => {},
            Ok(_) => {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "deterministic CBOR",
                    render_bytes(bytes),
                    Some("embedded CBOR was not deterministically encoded".to_owned()),
                ));
                return false;
            },
            Err(error) => {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "deterministic CBOR",
                    render_bytes(bytes),
                    Some(format!("failed to re-encode embedded CBOR: {error}")),
                ));
                return false;
            },
        }
    }

    if !allow_sequence && document.items().len() != 1 {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a single embedded CBOR item",
            format!("{} top-level item(s)", document.items().len()),
            Some("embedded CBOR was not a single item".to_owned()),
        ));
        return false;
    }

    let previous_source = set_current_source_bytes(bytes);
    if allow_sequence {
        for (index, item) in document.items().iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(PathStep::TagInner);
            child_path.push(PathStep::ArrayItem(index));
            validate_schema_node(compiled, definitions, rhs, item, &mut child_path, issues);
        }
        restore_current_source_bytes(previous_source);
        return issues.len() == start_len;
    }

    let Some(item) = document.items().first() else {
        restore_current_source_bytes(previous_source);
        return false;
    };

    let mut child_path = path.clone();
    child_path.push(PathStep::TagInner);
    validate_schema_node(compiled, definitions, rhs, item, &mut child_path, issues);
    restore_current_source_bytes(previous_source);
    issues.len() == start_len
}

/// Validate a deterministic serialization controller.
fn validate_deterministic_serialization(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
    allow_sequence: bool,
) -> bool {
    let start_len = issues.len();

    if value_to_bytes(value).is_some() {
        return validate_embedded_cbor_with_serialization(
            compiled,
            definitions,
            rhs,
            value,
            path,
            issues,
            allow_sequence,
            true,
        );
    }

    let source = current_source_bytes();
    if source.is_empty() {
        issues.push(ValidationIssue::new(
            path.clone(),
            "deterministic CBOR input",
            format!("{value}"),
            Some("no source bytes were available for deterministic comparison".to_owned()),
        ));
        return false;
    }

    let encoded = match value.to_deterministic_bytes() {
        Ok(encoded) => encoded,
        Err(error) => {
            issues.push(ValidationIssue::new(
                path.clone(),
                "deterministic CBOR",
                format!("{value}"),
                Some(format!("failed to re-encode CBOR: {error}")),
            ));
            return false;
        },
    };

    if encoded != source {
        issues.push(ValidationIssue::new(
            path.clone(),
            "deterministic CBOR",
            format!("{value}"),
            Some("encoded bytes did not match deterministic re-encoding".to_owned()),
        ));
        return false;
    }

    validate_schema_node(compiled, definitions, rhs, value, path, issues);
    issues.len() == start_len
}

/// Validate a `regexp` control operator.
fn validate_regex_rhs(
    compiled: &CompiledCDDL,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let pattern = resolve_text_rhs(compiled, rhs).or_else(|| parse_text_from_node(rhs));
    let Some(pattern) = pattern else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a regex pattern",
            format!("{value}"),
            Some("regex RHS did not resolve to text".to_owned()),
        ));
        return false;
    };

    let Ok(regex) = RegexLiteral::parse(pattern.as_bytes()) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a valid regex pattern",
            pattern.clone(),
            Some("regex RHS was invalid".to_owned()),
        ));
        return false;
    };

    match value {
        Value::Text(text) => {
            match regex.validate_text(text) {
                Ok(()) => true,
                Err(RegexValidationError::Mismatch) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("text matching {pattern}"),
                        format!("{value}"),
                        Some("regular expression mismatch".to_owned()),
                    ));
                    false
                },
                Err(RegexValidationError::InvalidUTF8) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("text matching {pattern}"),
                        format!("{value}"),
                        Some("text was not valid UTF-8".to_owned()),
                    ));
                    false
                },
            }
        },
        Value::Bytes(bytes) => {
            match regex.validate_bytes(bytes) {
                Ok(()) => true,
                Err(RegexValidationError::Mismatch) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("bytes matching {pattern}"),
                        format!("{value}"),
                        Some("regular expression mismatch".to_owned()),
                    ));
                    false
                },
                Err(RegexValidationError::InvalidUTF8) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("UTF-8 bytes matching {pattern}"),
                        format!("{value}"),
                        Some("byte string was not valid UTF-8".to_owned()),
                    ));
                    false
                },
            }
        },
        _ => {
            issues.push(ValidationIssue::new(
                path.clone(),
                format!("text or bytes matching {pattern}"),
                format!("{value}"),
                Some("regex validation requires text or bytes".to_owned()),
            ));
            false
        },
    }
}

/// Validate an ABNF controller RHS.
fn validate_abnf_rhs(
    compiled: &CompiledCDDL,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let pattern = resolve_text_rhs(compiled, rhs).or_else(|| parse_text_from_node(rhs));
    let Some(pattern) = pattern else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "an ABNF pattern",
            format!("{value}"),
            Some("ABNF RHS did not resolve to text".to_owned()),
        ));
        return false;
    };

    let Ok(document) = parse_abnf(&pattern) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "valid ABNF",
            pattern.clone(),
            Some("ABNF parsing failed".to_owned()),
        ));
        return false;
    };

    match value {
        Value::Text(text) => {
            match document.validate_text(text) {
                Ok(()) => true,
                Err(error) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("text matching ABNF {pattern}"),
                        format!("{value}"),
                        Some(error.to_string()),
                    ));
                    false
                },
            }
        },
        Value::Bytes(bytes) => {
            match document.validate_bytes(bytes) {
                Ok(()) => true,
                Err(error) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        format!("bytes matching ABNF {pattern}"),
                        format!("{value}"),
                        Some(error.to_string()),
                    ));
                    false
                },
            }
        },
        _ => {
            issues.push(ValidationIssue::new(
                path.clone(),
                format!("text or bytes matching ABNF {pattern}"),
                format!("{value}"),
                Some("ABNF validation requires text or bytes".to_owned()),
            ));
            false
        },
    }
}

/// Validate a resolved semantic state against a CBOR value.
fn validate_state(
    state: &EntryState,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) {
    match state {
        EntryState::Integer(expected) => {
            let Some(found) = value_to_i128(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("integer {expected}"),
                    format!("{value}"),
                    Some("value was not an integer".to_owned()),
                ));
                return;
            };

            if found != *expected {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("integer {expected}"),
                    found.to_string(),
                    Some("integer value did not match".to_owned()),
                ));
            }
        },
        EntryState::Float(expected) => {
            let Some(found) = value_to_f64(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("float {expected}"),
                    format!("{value}"),
                    Some("value was not a float".to_owned()),
                ));
                return;
            };

            if (found - *expected).abs() > f64::EPSILON {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("float {expected}"),
                    found.to_string(),
                    Some("float value did not match".to_owned()),
                ));
            }
        },
        EntryState::Text(expected) => {
            let Some(found) = value_to_text(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("text {:?}", String::from_utf8_lossy(expected.as_ref())),
                    format!("{value}"),
                    Some("value was not text".to_owned()),
                ));
                return;
            };

            if found.as_bytes() != expected.as_ref() {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("text {:?}", String::from_utf8_lossy(expected.as_ref())),
                    format!("{value}"),
                    Some("text literal did not match".to_owned()),
                ));
            }
        },
        EntryState::Bytes(expected) => {
            let Some(found) = value_to_bytes(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("bytes {}", render_bytes(expected.as_ref())),
                    format!("{value}"),
                    Some("value was not bytes".to_owned()),
                ));
                return;
            };

            if found != expected.as_ref() {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("bytes {}", render_bytes(expected.as_ref())),
                    render_bytes(found),
                    Some("byte literal did not match".to_owned()),
                ));
            }
        },
        EntryState::Regex(regex) => {
            let ok = match value {
                Value::Text(text) => regex.validate_text(text).is_ok(),
                Value::Bytes(bytes) => regex.validate_bytes(bytes).is_ok(),
                _ => false,
            };
            if !ok {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("value matching regex {regex}"),
                    format!("{value}"),
                    Some("regular expression did not match".to_owned()),
                ));
            }
        },
        EntryState::Abnf(document)
        | EntryState::EncAbnf(document)
        | EntryState::HashAbnf(document) => {
            let ok = match value {
                Value::Text(text) => document.validate_text(text).is_ok(),
                Value::Bytes(bytes) => document.validate_bytes(bytes).is_ok(),
                _ => false,
            };
            if !ok {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "value matching ABNF",
                    format!("{value}"),
                    Some("ABNF validation failed".to_owned()),
                ));
            }
        },
        EntryState::CompressionAbnf { document, .. } => {
            // The compression operator is currently a *narrowing
            // annotation* at the schema boundary; the literal payload
            // is still a byte string, so we just verify the value
            // matches the underlying ABNF document.  Future binary
            // validation can reverse the compression and re-validate
            // the inner payload against the controller.
            let ok = match value {
                Value::Text(text) => document.validate_text(text).is_ok(),
                Value::Bytes(bytes) => document.validate_bytes(bytes).is_ok(),
                _ => false,
            };
            if !ok {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "value matching compression annotation ABNF",
                    format!("{value}"),
                    Some("ABNF validation failed".to_owned()),
                ));
            }
        },
        EntryState::RangeInt {
            exclusive,
            min,
            max,
        } => {
            let Some(found) = value_to_i128(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("integer in range {min}..{max}"),
                    format!("{value}"),
                    Some("value was not an integer".to_owned()),
                ));
                return;
            };

            let in_range = if *exclusive {
                found > *min && found < *max
            } else {
                found >= *min && found <= *max
            };
            if !in_range {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("integer in range {min}..{max}"),
                    found.to_string(),
                    Some("integer range check failed".to_owned()),
                ));
            }
        },
        EntryState::RangeFloat {
            exclusive,
            min,
            max,
        } => {
            let Some(found) = value_to_f64(value) else {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("float in range {min}..{max}"),
                    format!("{value}"),
                    Some("value was not a float".to_owned()),
                ));
                return;
            };

            let in_range = if *exclusive {
                found > *min && found < *max
            } else {
                found >= *min && found <= *max
            };
            if !in_range {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    format!("float in range {min}..{max}"),
                    found.to_string(),
                    Some("float range check failed".to_owned()),
                ));
            }
        },
        EntryState::Unresolved | EntryState::Pruned => {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a resolved schema entry",
                format!("{value}"),
                Some("schema entry was not resolved".to_owned()),
            ));
        },
    }
}

/// Determine whether a validation path matches the highlighted path.
fn is_highlighted(
    path: &[PathStep],
    highlight: Option<&[PathStep]>,
) -> bool {
    highlight.is_some_and(|highlight| path == highlight)
}

/// Render a validation dump with optional highlighting.
fn render_validation_dump(
    schema_path: &Path,
    input_path: &str,
    document: &Document,
    notes: &[SchemaNote],
    highlight: Option<&[PathStep]>,
    color: bool,
) -> String {
    let mut output = String::new();
    if color {
        push_colored(
            &mut output,
            format!("{} -> {}", schema_path.display(), input_path),
            ColorKind::Header,
            color,
        );
        push_dim(&mut output, "\n", color);
    } else {
        let _ = writeln!(output, "{} -> {}", schema_path.display(), input_path);
    }

    for (index, item) in document.items().iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let path = [PathStep::DocItem(index)];
        render_value_with_highlight(item, &mut output, color, 0, &path, highlight, notes);
    }

    if !output.ends_with('\n') {
        output.push('\n');
    }

    output
}

/// Render one CBOR value with an optional highlight.
fn render_value_with_highlight(
    value: &Value,
    output: &mut String,
    color: bool,
    indent: usize,
    path: &[PathStep],
    highlight: Option<&[PathStep]>,
    notes: &[SchemaNote],
) {
    let node_highlight = is_highlighted(path, highlight);
    if let Some(note) = schema_note_for_path(notes, path) {
        render_annotation(output, &note, color, node_highlight);
    }

    match value {
        Value::Integer(value) => {
            render_token(
                output,
                value.to_string(),
                ColorKind::Number,
                color,
                node_highlight,
            );
        },
        Value::Float(value) => {
            match value {
                Float::F16(value) | Float::F32(value) => {
                    render_token(
                        output,
                        value.to_string(),
                        ColorKind::Float,
                        color,
                        node_highlight,
                    );
                },
                Float::F64(value) => {
                    render_token(
                        output,
                        value.to_string(),
                        ColorKind::Float,
                        color,
                        node_highlight,
                    );
                },
            }
        },
        Value::Bool(value) => {
            render_token(
                output,
                value.to_string(),
                ColorKind::Keyword,
                color,
                node_highlight,
            );
        },
        Value::Null => {
            render_token(output, "null", ColorKind::Keyword, color, node_highlight);
        },
        Value::Undefined => {
            render_token(
                output,
                "undefined",
                ColorKind::Keyword,
                color,
                node_highlight,
            );
        },
        Value::Simple(value) => {
            render_token(
                output,
                format!("simple({value})"),
                ColorKind::Simple,
                color,
                node_highlight,
            );
        },
        Value::Bytes(value) => {
            render_token(
                output,
                render_bytes(value),
                ColorKind::Bytes,
                color,
                node_highlight,
            );
        },
        Value::Text(value) => {
            render_token(
                output,
                format!("{value:?}"),
                ColorKind::Text,
                color,
                node_highlight,
            );
        },
        Value::Array(values) => {
            let depth = indent / 2;
            push_bracket(output, "[", color, depth);
            if values.is_empty() {
                push_bracket(output, "]", color, depth);
                return;
            }

            push_dim(output, "\n", color);
            push_indent(output, indent.saturating_add(2));
            for (index, item) in values.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(PathStep::ArrayItem(index));
                render_value_with_highlight(
                    item,
                    output,
                    color,
                    indent.saturating_add(2),
                    &child_path,
                    highlight,
                    notes,
                );
                if index + 1 < values.len() {
                    render_punct(output, ",", color, false);
                }
                if index + 1 < values.len() {
                    push_dim(output, "\n", color);
                    push_indent(output, indent.saturating_add(2));
                }
            }
            push_dim(output, "\n", color);
            push_indent(output, indent);
            push_bracket(output, "]", color, depth);
        },
        Value::Map(entries) => {
            let depth = indent / 2;
            push_bracket(output, "{", color, depth);
            if entries.is_empty() {
                push_bracket(output, "}", color, depth);
                return;
            }

            push_dim(output, "\n", color);
            push_indent(output, indent.saturating_add(2));
            for (index, entry) in entries.iter().enumerate() {
                render_map_entry_with_highlight(
                    entry,
                    output,
                    color,
                    indent.saturating_add(2),
                    path,
                    index,
                    highlight,
                    notes,
                );
                let mut value_path = path.to_vec();
                value_path.push(PathStep::MapValue(index));
                if index + 1 < entries.len() {
                    render_punct(output, ",", color, false);
                }
                if index + 1 < entries.len() {
                    push_dim(output, "\n", color);
                    push_indent(output, indent.saturating_add(2));
                }
            }
            push_dim(output, "\n", color);
            push_indent(output, indent);
            push_bracket(output, "}", color, depth);
        },
        Value::Tag(tag, inner) => {
            let depth = indent / 2;
            let rendered = tag.to_string();
            if node_highlight {
                render_token(output, rendered, ColorKind::Tag, color, true);
                push_bracket(output, "(", color, depth);
                let mut child_path = path.to_vec();
                child_path.push(PathStep::TagInner);
                render_value_with_highlight(
                    inner,
                    output,
                    color,
                    indent,
                    &child_path,
                    highlight,
                    notes,
                );
                push_bracket(output, ")", color, depth);
            } else {
                render_token(output, rendered, ColorKind::Tag, color, false);
                push_bracket(output, "(", color, depth);
                let mut child_path = path.to_vec();
                child_path.push(PathStep::TagInner);
                render_value_with_highlight(
                    inner,
                    output,
                    color,
                    indent,
                    &child_path,
                    highlight,
                    notes,
                );
                push_bracket(output, ")", color, depth);
            }
        },
    }
}

/// Render one map entry with highlight support.
fn render_map_entry_with_highlight(
    entry: &MapEntry,
    output: &mut String,
    color: bool,
    indent: usize,
    path: &[PathStep],
    entry_index: usize,
    highlight: Option<&[PathStep]>,
    notes: &[SchemaNote],
) {
    let key_path = {
        let mut path = path.to_vec();
        path.push(PathStep::MapKey(entry_index));
        path
    };
    let value_path = {
        let mut path = path.to_vec();
        path.push(PathStep::MapValue(entry_index));
        path
    };
    render_value_with_highlight(
        &entry.key, output, color, indent, &key_path, highlight, notes,
    );
    render_punct(output, ": ", color, is_highlighted(path, highlight));
    render_value_with_highlight(
        &entry.value,
        output,
        color,
        indent,
        &value_path,
        highlight,
        notes,
    );
}

/// Render one token with optional highlight.
fn render_token<T: std::fmt::Display>(
    output: &mut String,
    text: T,
    kind: ColorKind,
    color: bool,
    highlight: bool,
) {
    if color && highlight {
        let _ = write!(output, "{}", style(text).red().bold());
    } else {
        push_colored(output, text, kind, color);
    }
}

/// Render punctuation with optional highlight.
fn render_punct(
    output: &mut String,
    text: &str,
    color: bool,
    highlight: bool,
) {
    if color && highlight {
        let _ = write!(output, "{}", style(text).red().bold());
    } else {
        push_dim(output, text, color);
    }
}

/// Render a schema annotation prefix.
fn render_annotation(
    output: &mut String,
    text: &str,
    color: bool,
    _highlight: bool,
) {
    if color {
        let _ = write!(output, "{} ", style(format!("/{text}/")).dim());
    } else {
        let _ = write!(output, "/{text}/ ");
    }
}

/// Record a schema note for a path if there is no existing note.
fn record_schema_note_once(
    path: &[PathStep],
    text: String,
) {
    if text.trim().is_empty() {
        return;
    }

    CURRENT_SCHEMA_NOTES.with(|slot| {
        let mut notes = slot.borrow_mut();
        if notes.iter().any(|note| note.path == path) {
            return;
        }
        notes.push(SchemaNote {
            path: path.to_vec(),
            text,
        });
    });
}

/// Take all recorded schema notes.
fn take_current_schema_notes() -> Vec<SchemaNote> {
    CURRENT_SCHEMA_NOTES.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Clear the recorded schema notes.
fn clear_current_schema_notes() {
    CURRENT_SCHEMA_NOTES.with(|slot| slot.borrow_mut().clear());
}

/// Get a note for a particular path.
fn schema_note_for_path(
    notes: &[SchemaNote],
    path: &[PathStep],
) -> Option<String> {
    let mut seen = Vec::new();
    for note in notes.iter().filter(|note| note.path == path) {
        if !seen.iter().any(|existing: &String| existing == &note.text) {
            seen.push(note.text.clone());
        }
    }

    if seen.is_empty() {
        None
    } else {
        Some(seen.join("; "))
    }
}

/// Render a concise schema summary for a node.
fn schema_summary(node: &WrappedNode) -> String {
    match node {
        WrappedNode::RuleLine { text, .. } => {
            top_level_rule_name(node).unwrap_or_else(|| text.trim().to_owned())
        },
        WrappedNode::Syntax { rule, text, .. } => {
            match rule.as_str() {
                "memberkey" => memberkey_summary(text),
                "typename" | "groupname" | "bareword" => text.trim().to_owned(),
                _ => text.trim().to_owned(),
            }
        },
        WrappedNode::Directive { source_comment, .. } => source_comment.trim().to_owned(),
        WrappedNode::Comment { text, .. } => text.trim().to_owned(),
        WrappedNode::ModuleStart { text, .. } | WrappedNode::ModuleEnd { text, .. } => {
            text.trim().to_owned()
        },
    }
}

/// Summarize a member key expression.
fn memberkey_summary(text: &str) -> String {
    let trimmed = text.trim();
    if let Some((lhs, _rhs)) = trimmed.split_once("=>") {
        return lhs.trim().trim_end_matches(':').to_owned();
    }
    if let Some((lhs, _rhs)) = trimmed.split_once(':') {
        return lhs.trim().to_owned();
    }
    trimmed.to_owned()
}

/// Format a validation path for human-readable output.
fn format_path(path: &[PathStep]) -> String {
    if matches!(path, [PathStep::DocItem(0)]) {
        return "root".to_owned();
    }

    let mut out = String::from("root");
    for step in path {
        match step {
            PathStep::DocItem(index) => {
                let _ = write!(out, "[{index}]");
            },
            PathStep::ArrayItem(index) => {
                let _ = write!(out, "[{index}]");
            },
            PathStep::MapKey(index) => {
                let _ = write!(out, ".key[{index}]");
            },
            PathStep::MapValue(index) => {
                let _ = write!(out, ".value[{index}]");
            },
            PathStep::TagInner => out.push_str(".tag"),
        }
    }
    out
}

/// Find the first top-level rule name in a compiled schema.
fn root_rule_name(compiled: &CompiledCDDL) -> Option<String> {
    compiled.user_nodes.iter().find_map(top_level_rule_name)
}

/// Extract the top-level rule name from a rule line.
fn top_level_rule_name(node: &WrappedNode) -> Option<String> {
    let WrappedNode::RuleLine { text, .. } = node else {
        return None;
    };

    let lhs = text
        .split_once('=')
        .map_or(text.as_str(), |(lhs, _)| lhs)
        .trim();
    Some(
        lhs.chars()
            .take_while(|ch| !matches!(ch, ' ' | '<' | '\t'))
            .collect(),
    )
}

/// Collect all rule definitions from the compiled tree.
fn collect_definitions<'a>(nodes: &'a [WrappedNode]) -> HashMap<String, &'a WrappedNode> {
    let mut defs = HashMap::new();
    collect_definitions_nested(nodes, &mut defs);
    defs
}

/// Recursively collect definitions from a node slice.
fn collect_definitions_nested<'a>(
    nodes: &'a [WrappedNode],
    defs: &mut HashMap<String, &'a WrappedNode>,
) {
    for node in nodes {
        if let Some(name) = top_level_rule_name(node) {
            defs.insert(name, node);
        }

        match node {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                collect_definitions_nested(children, defs);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Find the RHS schema node inside a rule line.
fn find_rhs_node<'a>(children: &'a [WrappedNode]) -> Option<&'a WrappedNode> {
    children.iter().find_map(|child| {
        match child {
            WrappedNode::Syntax { rule, .. }
                if matches!(rule.as_str(), "type" | "group" | "type1" | "grpent") =>
            {
                Some(child)
            },
            WrappedNode::Syntax { children, .. }
            | WrappedNode::RuleLine { children, .. }
            | WrappedNode::Directive { children, .. } => find_rhs_node(children),
            _ => None,
        }
    })
}

/// Find a child syntax node with the given rule.
fn node_children_find<'a>(
    node: &'a WrappedNode,
    rule_name: &str,
) -> Option<&'a WrappedNode> {
    match node {
        WrappedNode::Syntax { children, .. }
        | WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. } => {
            children.iter().find_map(|child| {
                if let WrappedNode::Syntax { rule, .. } = child
                    && rule == rule_name
                {
                    return Some(child);
                }
                node_children_find(child, rule_name)
            })
        },
        _ => None,
    }
}

/// Collect nodes of a particular syntax rule.
fn group_children<'a>(
    node: &'a WrappedNode,
    rule_name: &str,
) -> Vec<&'a WrappedNode> {
    match node {
        WrappedNode::Syntax { children, .. }
        | WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. } => {
            children
                .iter()
                .filter(
                    |child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == rule_name),
                )
                .collect()
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => Vec::new(),
    }
}

/// Find the payload node of a `grpent`.
fn find_grpent_body(node: &WrappedNode) -> Option<&WrappedNode> {
    match node {
        WrappedNode::Syntax { rule, children, .. } if rule == "grpent" => {
            children.iter().find_map(|child| {
                match child {
                    WrappedNode::Syntax { rule, .. }
                        if matches!(rule.as_str(), "type" | "group") =>
                    {
                        Some(child)
                    },
                    _ => None,
                }
            })
        },
        _ => None,
    }
}

/// Validate one array `grpchoice`.
fn validate_grpchoice_array(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    grpchoice: &WrappedNode,
    items: &[Value],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let mut item_index = 0usize;
    let Some(grpent_nodes) = extract_grpent_nodes(grpchoice) else {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "a group choice",
            "unrecognized group structure",
            Some("could not extract group elements".to_owned()),
        ));
        return false;
    };

    for (element_index, grpent) in grpent_nodes.iter().enumerate() {
        let occur = grpent_occurrence(grpent);
        let Some(body) = find_grpent_body(grpent) else {
            issues.push(ValidationIssue::new(
                path.to_owned(),
                "a group element body",
                "missing body",
                Some("could not locate the group element payload".to_owned()),
            ));
            return false;
        };

        match occur {
            "?" => {
                if item_index < items.len() {
                    if let Some(item) = items.get(item_index) {
                        let mut child_path = path.to_owned();
                        child_path.push(PathStep::ArrayItem(item_index));
                        record_schema_note_once(&child_path, schema_summary(body));
                        validate_schema_node(
                            compiled,
                            definitions,
                            body,
                            item,
                            &mut child_path,
                            issues,
                        );
                    }
                    item_index = item_index.saturating_add(1);
                }
            },
            "+" | "*" => {
                if occur == "+" && item_index >= items.len() {
                    issues.push(ValidationIssue::new(
                        path.to_owned(),
                        "one or more array items",
                        "empty array",
                        Some(format!(
                            "group element {element_index} required at least one item"
                        )),
                    ));
                    return false;
                }
                while item_index < items.len() {
                    if let Some(item) = items.get(item_index) {
                        let mut child_path = path.to_owned();
                        child_path.push(PathStep::ArrayItem(item_index));
                        record_schema_note_once(&child_path, schema_summary(body));
                        validate_schema_node(
                            compiled,
                            definitions,
                            body,
                            item,
                            &mut child_path,
                            issues,
                        );
                    }
                    item_index = item_index.saturating_add(1);
                }
            },
            _ => {
                if item_index >= items.len() {
                    issues.push(ValidationIssue::new(
                        path.to_owned(),
                        "more array items",
                        "end of array",
                        Some(format!("missing array item for element {element_index}")),
                    ));
                    return false;
                }
                if let Some(item) = items.get(item_index) {
                    let mut child_path = path.to_owned();
                    child_path.push(PathStep::ArrayItem(item_index));
                    record_schema_note_once(&child_path, schema_summary(body));
                    validate_schema_node(
                        compiled,
                        definitions,
                        body,
                        item,
                        &mut child_path,
                        issues,
                    );
                }
                item_index = item_index.saturating_add(1);
            },
        }
    }

    if item_index != items.len() {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "no trailing array items",
            format!("{} extra item(s)", items.len().saturating_sub(item_index)),
            Some("array had trailing elements".to_owned()),
        ));
        return false;
    }

    issues.is_empty()
}

/// Validate one map `grpchoice`.
fn validate_grpchoice_map(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    grpchoice: &WrappedNode,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let Some(grpent_nodes) = extract_grpent_nodes(grpchoice) else {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "a group choice",
            "unrecognized group structure",
            Some("could not extract group elements".to_owned()),
        ));
        return false;
    };

    for grpent in grpent_nodes {
        let Some(body) = find_grpent_body(grpent) else {
            issues.push(ValidationIssue::new(
                path.to_owned(),
                "a group element body",
                "missing body",
                Some("could not locate the group element payload".to_owned()),
            ));
            return false;
        };
        let memberkey = find_memberkey(grpent);
        let occur = grpent_occurrence(grpent);

        let Some((entry_index, entry)) =
            find_matching_map_entry(compiled, definitions, memberkey, entries, used)
        else {
            if occur == "?" {
                continue;
            }
            issues.push(ValidationIssue::new(
                path.to_owned(),
                "a matching map entry",
                "no match",
                Some("required map entry was missing".to_owned()),
            ));
            return false;
        };

        let mut key_path = path.to_owned();
        key_path.push(PathStep::MapKey(entry_index));
        let mut value_path = path.to_owned();
        value_path.push(PathStep::MapValue(entry_index));
        if let Some(key_node) = memberkey {
            record_schema_note_once(&key_path, schema_summary(key_node));
            validate_schema_node(
                compiled,
                definitions,
                key_node,
                &entry.key,
                &mut key_path,
                issues,
            );
        }
        record_schema_note_once(&value_path, schema_summary(body));
        validate_schema_node(
            compiled,
            definitions,
            body,
            &entry.value,
            &mut value_path,
            issues,
        );
        if let Some(slot) = used.get_mut(entry_index) {
            *slot = true;
        }
    }

    if used.iter().any(|used| !*used) {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "no trailing map entries",
            "unmatched map entry",
            Some("map had extra entries".to_owned()),
        ));
        return false;
    }

    issues.is_empty()
}

/// Find the first map entry that matches a member key.
fn find_matching_map_entry<'a>(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    memberkey: Option<&WrappedNode>,
    entries: &'a [MapEntry],
    used: &[bool],
) -> Option<(usize, &'a MapEntry)> {
    entries
        .iter()
        .enumerate()
        .find(|(index, entry)| {
            if used.get(*index).copied().unwrap_or(false) {
                return false;
            }

            match memberkey {
                Some(key_schema) => {
                    let mut temp_issues = Vec::new();
                    let mut path = Vec::new();
                    validate_schema_node(
                        compiled,
                        definitions,
                        key_schema,
                        &entry.key,
                        &mut path,
                        &mut temp_issues,
                    );
                    temp_issues.is_empty()
                },
                None => true,
            }
        })
        .map(|(index, entry)| (index, entry))
}

/// Extract the member-key node from a group element.
fn find_memberkey(node: &WrappedNode) -> Option<&WrappedNode> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "grpent" {
        return None;
    }

    children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "memberkey"))
}

/// Extract a `grpent`'s occurrence modifier.
fn grpent_occurrence(node: &WrappedNode) -> &str {
    let WrappedNode::Syntax { rule, text, .. } = node else {
        return "";
    };
    if rule != "grpent" {
        return "";
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with('?') {
        "?"
    } else if trimmed.starts_with('+') {
        "+"
    } else if trimmed.starts_with('*') {
        "*"
    } else {
        ""
    }
}

/// Extract `grpent` nodes from a group choice.
fn extract_grpent_nodes(node: &WrappedNode) -> Option<Vec<&WrappedNode>> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "grpchoice" {
        return None;
    }

    Some(
        children
            .iter()
            .filter(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "grpent"))
            .collect(),
    )
}

/// Parse a simple-value marker such as `null` or `true`.
fn parse_simple_marker(text: &str) -> Option<&str> {
    match text.trim() {
        "null" | "true" | "false" | "undefined" => Some(text.trim()),
        _ => None,
    }
}

/// Returns `true` if the name is a CDDL socket (starts with `$`).
fn is_socket_name(name: &str) -> bool {
    name.starts_with('$')
}

/// Checks whether a definition's numeric head value matches a given CBOR tag.
fn head_number_matches_tag(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    actual_tag: u64,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let Some(head) = node_children_find(node, "head_number") else {
        return true;
    };

    let tag_value = Value::Integer(actual_tag.into());
    let mut local_issues = Vec::new();
    if let Some(schema) = node_children_find(head, "type") {
        validate_schema_node(
            compiled,
            definitions,
            schema,
            &tag_value,
            path,
            &mut local_issues,
        );
    } else {
        let trimmed = child_text(head).trim();
        if let Ok(expected) = trimmed.parse::<u64>()
            && expected == actual_tag
        {
            return true;
        }
        local_issues.push(ValidationIssue::new(
            path.clone(),
            format!("tag {trimmed}"),
            format!("tag {actual_tag}"),
            Some("tag number did not match".to_owned()),
        ));
    }

    if local_issues.is_empty() {
        return true;
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        format!("a tag matching {}", child_text(head).trim()),
        format!("tag {actual_tag}"),
        Some("tag number did not match the `head_number` constraint".to_owned()),
    ));
    issues.extend(local_issues);
    false
}

/// Checks whether a definition's numeric head value matches a simple value.
fn head_number_matches_simple(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let Some(code) = simple_value_code(value) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a major type 7 item".to_owned(),
            format!("{value}"),
            Some("expected a simple value or float".to_owned()),
        ));
        return false;
    };

    let Some(head) = node_children_find(node, "head_number") else {
        return true;
    };

    let head_value = Value::Integer(code.into());
    let mut local_issues = Vec::new();
    if let Some(schema) = node_children_find(head, "type") {
        validate_schema_node(
            compiled,
            definitions,
            schema,
            &head_value,
            path,
            &mut local_issues,
        );
    } else {
        let trimmed = child_text(head).trim();
        if let Ok(expected) = trimmed.parse::<u8>()
            && expected == code
        {
            return true;
        }
        local_issues.push(ValidationIssue::new(
            path.clone(),
            format!("simple({trimmed})"),
            format!("simple({code})"),
            Some("simple value did not match".to_owned()),
        ));
    }

    if local_issues.is_empty() {
        return true;
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        format!("a simple value matching {}", child_text(head).trim()),
        format!("{value}"),
        Some("simple value did not match the `head_number` constraint".to_owned()),
    ));
    issues.extend(local_issues);
    false
}

/// Extracts the simple value code (0-255) from a CDDL value if applicable.
fn simple_value_code(value: &Value) -> Option<u8> {
    match value {
        Value::Bool(false) => Some(20),
        Value::Bool(true) => Some(21),
        Value::Null => Some(22),
        Value::Undefined => Some(23),
        Value::Float(Float::F16(_)) => Some(25),
        Value::Float(Float::F32(_)) => Some(26),
        Value::Float(Float::F64(_)) => Some(27),
        Value::Simple(value) => Some(*value),
        _ => None,
    }
}

/// Parse a literal value node into an entry state.
fn parse_value_literal(node: &WrappedNode) -> Option<EntryState> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            return match rule.as_str() {
                "uint" | "int" => text.trim().parse::<i128>().ok().map(EntryState::Integer),
                "intfloat" | "number" => {
                    let trimmed = text.trim();
                    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
                        trimmed.parse::<f64>().ok().map(EntryState::Float)
                    } else {
                        trimmed.parse::<i128>().ok().map(EntryState::Integer)
                    }
                },
                "text" => {
                    TextLiteralBytes::parse(text.as_bytes())
                        .ok()
                        .map(EntryState::Text)
                },
                "bytes" => {
                    ByteLiteralBytes::parse(text.as_bytes())
                        .ok()
                        .map(EntryState::Bytes)
                },
                _ => None,
            };
        }
    }
    None
}

/// Resolve the RHS of a control operator to text if possible.
fn resolve_text_rhs(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<String> {
    let state = resolve_type2_leaf(node, &compiled.resolved_types)?;
    match state {
        EntryState::Text(text) => String::from_utf8(text.as_ref().to_vec()).ok(),
        EntryState::Bytes(bytes) => String::from_utf8(bytes.as_ref().to_vec()).ok(),
        _ => None,
    }
}

/// Resolve the RHS of a control operator to an integer if possible.
fn resolve_integer_rhs(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<i128> {
    let state = resolve_type2_leaf(node, &compiled.resolved_types)?;
    match state {
        EntryState::Integer(value) => Some(value),
        _ => None,
    }
}

/// Parse an integer literal from a node.
fn parse_integer_from_node(node: &WrappedNode) -> Option<i128> {
    match parse_value_literal(node)? {
        EntryState::Integer(value) => Some(value),
        _ => None,
    }
}

/// Parse text from a literal node.
fn parse_text_from_node(node: &WrappedNode) -> Option<String> {
    let EntryState::Text(text) = parse_value_literal(node)? else {
        return None;
    };
    String::from_utf8(text.as_ref().to_vec()).ok()
}

/// Convert a CBOR value to an integer if possible.
fn value_to_i128(value: &Value) -> Option<i128> {
    match value {
        Value::Integer(int) => Some((*int).into()),
        _ => None,
    }
}

/// Convert a CBOR value to a float if possible.
fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(Float::F16(value) | Float::F32(value)) => Some(f64::from(*value)),
        Value::Float(Float::F64(value)) => Some(*value),
        _ => None,
    }
}

/// Convert a CBOR value to text if possible.
fn value_to_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) => Some(text.as_str()),
        _ => None,
    }
}

/// Convert a CBOR value to bytes if possible.
fn value_to_bytes(value: &Value) -> Option<&[u8]> {
    match value {
        Value::Bytes(bytes) => Some(bytes.as_slice()),
        _ => None,
    }
}

/// Determine the size of a CBOR value.
fn value_len(value: &Value) -> Option<usize> {
    match value {
        Value::Bytes(bytes) => Some(bytes.len()),
        Value::Text(text) => Some(text.len()),
        Value::Array(values) => Some(values.len()),
        Value::Map(entries) => Some(entries.len()),
        _ => None,
    }
}

/// Validate a builtin type name.
fn validate_builtin(
    builtin: &str,
    value: &Value,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) {
    let ok = match builtin {
        "any" => true,
        "bool" => matches!(value, Value::Bool(_)),
        "null" => matches!(value, Value::Null),
        "undefined" => matches!(value, Value::Undefined),
        "text" | "tstr" => matches!(value, Value::Text(_)),
        "bytes" | "bstr" => matches!(value, Value::Bytes(_)),
        "int" => matches!(value, Value::Integer(_)),
        "uint" => matches!(value, Value::Integer(int) if i128::from(*int) >= 0),
        "nint" => matches!(value, Value::Integer(int) if i128::from(*int) < 0),
        "number" => matches!(value, Value::Integer(_) | Value::Float(_)),
        "float" => matches!(value, Value::Float(_)),
        _ => false,
    };

    if !ok {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            builtin,
            format!("{value}"),
            Some("builtin type check failed".to_owned()),
        ));
    }
}

/// Determine whether a syntax name is a builtin CDDL type.
fn builtin_type_name(text: &str) -> Option<&'static str> {
    match text.trim() {
        "any" => Some("any"),
        "bool" => Some("bool"),
        "null" => Some("null"),
        "undefined" => Some("undefined"),
        "text" => Some("text"),
        "tstr" => Some("tstr"),
        "bytes" => Some("bytes"),
        "bstr" => Some("bstr"),
        "int" => Some("int"),
        "uint" => Some("uint"),
        "nint" => Some("nint"),
        "number" => Some("number"),
        "float" => Some("float"),
        _ => None,
    }
}

/// Return a concise CBOR display value for bytes.
fn render_bytes(bytes: &[u8]) -> String {
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

/// Set the current source bytes for deterministic serialization checks.
fn set_current_source_bytes(bytes: &[u8]) -> Vec<u8> {
    CURRENT_SOURCE_BYTES.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), bytes.to_vec()))
}

/// Restore the previous source bytes after a nested validation scope.
fn restore_current_source_bytes(previous: Vec<u8>) {
    CURRENT_SOURCE_BYTES.with(|slot| *slot.borrow_mut() = previous);
}

/// Fetch the current source bytes.
fn current_source_bytes() -> Vec<u8> {
    CURRENT_SOURCE_BYTES.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        path::{Path, PathBuf},
    };

    use cbork_cddl_compiler::CompiledCDDL;
    use cbork_edn::Document;

    use super::{
        PathStep, SchemaNote, collect_definitions, exec, render_validation_dump, root_rule_name,
        validate_document,
    };

    fn write_temp_file(
        name: &str,
        content: &[u8],
    ) -> PathBuf {
        let dir = std::env::temp_dir().join("cbork_validate_test");
        std::fs::create_dir_all(&dir).expect("temp validate dir should exist");
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("temp validate file should be created");
        file.write_all(content)
            .expect("temp validate file should be written");
        path
    }

    fn validate_schema_bytes(
        schema_name: &str,
        schema: &[u8],
        cbor: &[u8],
    ) -> Vec<super::ValidationIssue> {
        let schema = write_temp_file(schema_name, schema);
        let compiled =
            CompiledCDDL::compile(&schema, None::<&Path>).expect("schema should compile");
        let root = root_rule_name(&compiled).expect("schema should have root rule");
        let definitions = collect_definitions(&compiled.complete_nodes);
        let document = Document::parse(cbor).expect("CBOR should parse");
        validate_document(&compiled, &definitions, &root, &document)
    }

    #[test]
    fn validate_succeeds_for_matching_integer() {
        let schema = write_temp_file("schema_ok.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_ok.cbor", &[0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, true));
    }

    #[test]
    fn validate_fails_for_mismatched_integer() {
        let schema = write_temp_file("schema_fail.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_fail.cbor", &[0x02]);

        assert!(!exec(&schema, Some(&cbor), false, false, true));
    }

    #[test]
    fn validate_accepts_named_tag_head_number() {
        let issues = validate_schema_bytes(
            "named_tag_head_number.cddl",
            b"root = #6.<tag-num>(int)\ntag-num = 1000\n",
            &[0xD9, 0x03, 0xE8, 0x01],
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_simple_head_number_float16() {
        let issues = validate_schema_bytes("simple_head_number.cddl", b"root = #7.<25>\n", &[
            0xF9, 0x00, 0x00,
        ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_braced_unicode_text_literal() {
        let issues = validate_schema_bytes(
            "unicode_braced_text.cddl",
            "root = \"\\u{41}\"\n".as_bytes(),
            &[0x61, 0x41],
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_treats_missing_socket_as_empty_choice() {
        let issues = validate_schema_bytes("missing_socket.cddl", b"root = $missing\n", &[0x01]);

        assert!(
            !issues.is_empty(),
            "missing socket should not match arbitrary input"
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.as_deref() == Some("socket has no plugged definitions")),
            "{issues:#?}"
        );
        assert!(
            !issues
                .iter()
                .any(|issue| issue.message.as_deref() == Some("undefined rule reference")),
            "{issues:#?}"
        );
    }

    #[test]
    fn validation_dump_places_commas_and_schema_notes_inline() {
        let document =
            Document::parse(&[0x82, 0x02, 0xA1, 0x01, 0x03]).expect("document should parse");
        let notes = vec![
            SchemaNote {
                path: vec![PathStep::DocItem(0)],
                text: "root".to_owned(),
            },
            SchemaNote {
                path: vec![PathStep::DocItem(0), PathStep::ArrayItem(1)],
                text: "payload".to_owned(),
            },
            SchemaNote {
                path: vec![
                    PathStep::DocItem(0),
                    PathStep::ArrayItem(1),
                    PathStep::MapKey(0),
                ],
                text: "alg".to_owned(),
            },
            SchemaNote {
                path: vec![
                    PathStep::DocItem(0),
                    PathStep::ArrayItem(1),
                    PathStep::MapValue(0),
                ],
                text: "alg: tstr".to_owned(),
            },
        ];

        let rendered = render_validation_dump(
            Path::new("schema.cddl"),
            "input.cbor",
            &document,
            &notes,
            None,
            false,
        );

        assert!(
            rendered.contains("/root/ [\n  2,\n  /payload/ {\n    /alg/ 1: /alg: tstr/ 3\n  }\n]",)
        );
    }
}
