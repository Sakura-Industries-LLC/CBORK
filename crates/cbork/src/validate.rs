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

use std::{
    cell::RefCell,
    collections::HashMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use cbork_abnf_parser::parse_abnf;
use cbork_cddl_compiler::{
    CompiledCDDL, DiagnosticLevel, EntryState, MetaData, WrappedNode, build_resolution, child_text,
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
    static CURRENT_VALIDATION_WARNINGS: RefCell<Vec<ValidationWarning>> = const { RefCell::new(Vec::new()) };
}

/// Schema annotation captured during validation for later rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaNote {
    /// Validation path where the note applies.
    path: Vec<PathStep>,
    /// Human-readable annotation text.
    text: String,
}

/// Non-failing validation warning captured during traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationWarning {
    /// Validation path where the warning applies.
    path: Vec<PathStep>,
    /// Human-readable warning text.
    text: String,
}

/// Validate a CDDL schema against a CBOR payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec(
    schema_path: &Path,
    cbor_path: Option<&Path>,
    show_warnings: bool,
    detailed: bool,
    type_name: Option<&str>,
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
    clear_current_validation_warnings();

    let root_name = match resolve_validation_root(&compiled, schema_path, type_name) {
        Ok(name) => name,
        Err(error) => {
            println!(
                "{}",
                style(format_root_selection_error(&error, schema_path)).red()
            );
            return false;
        },
    };

    let definitions = collect_definitions(&compiled.complete_nodes);
    let issues = validate_document(&compiled, &definitions, &root_name, &document);
    let schema_notes = take_current_schema_notes();
    let validation_warnings = take_current_validation_warnings();

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
        } else {
            println!(
                "{} {} {} : OK",
                schema_path.display(),
                style("==").dim(),
                input_path,
            );
        }
        print_validation_warnings(&validation_warnings);
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
    print_validation_warnings(&validation_warnings);

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

/// Numeric value used by ordering control-operator validation.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NumericValue {
    /// Integer value.
    Integer(i128),
    /// Floating-point value.
    Float(f64),
}

impl std::fmt::Display for NumericValue {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Integer(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
        }
    }
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
            let warning_len = validation_warning_count();
            let note_len = schema_note_count();
            validate_schema_node(compiled, definitions, child, value, path, &mut local_issues);
            if local_issues.is_empty() {
                return;
            }
            truncate_validation_warnings(warning_len);
            truncate_current_schema_notes(note_len);
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
        if let Some(best) = best_issue_branch(branch_issues) {
            issues.extend(best);
        }
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
        let warning_len = validation_warning_count();
        let note_len = schema_note_count();
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
        truncate_validation_warnings(warning_len);
        truncate_current_schema_notes(note_len);
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "an array matching the schema group",
        format!("{value}"),
        Some("none of the group alternatives matched".to_owned()),
    ));
    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
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
        let warning_len = validation_warning_count();
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
        truncate_validation_warnings(warning_len);
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "an array matching the schema",
        format!("{value}"),
        Some("none of the array alternatives matched".to_owned()),
    ));
    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
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
        let warning_len = validation_warning_count();
        let note_len = schema_note_count();
        if validate_grpchoice_map(
            compiled,
            definitions,
            grpchoice,
            entries,
            &mut used,
            path,
            &mut local_issues,
            true,
        ) {
            return;
        }
        truncate_validation_warnings(warning_len);
        truncate_current_schema_notes(note_len);
        branch_issues.push(local_issues);
    }

    issues.push(ValidationIssue::new(
        path.clone(),
        "a map matching the schema",
        format!("{value}"),
        Some("no map alternative matched".to_owned()),
    ));
    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
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
        op if is_compression_ctlop(op) => {
            record_validation_warning_once(
                path,
                format!("compression operator `{op}` is not checked during validation yet"),
            );
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
        ".lt" | ".le" | ".gt" | ".ge" => {
            validate_ordering_rhs(compiled, rhs, op, value, path, issues);
        },
        ".eq" | ".ne" => {
            validate_equality_rhs(compiled, rhs, op, value, path, issues);
        },
        ".default" | ".within" | ".feature" => {
            // Defaults affect omitted values, and `.within` is a compile-time
            // subtype constraint. `.feature` is advisory in cbork because
            // validation has no feature selection context. If the LHS matched,
            // runtime validation has nothing else to check for these operators.
        },
        ".x-enc" | ".x-hash" => {
            // Plain `.x-enc` and `.x-hash` are annotations on the carrier bytes here.
            // The LHS validation above already proved the value is acceptable
            // as a byte string; checking the wrapped value requires algorithm/key
            // context that CDDL does not carry.
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

/// Validate numeric ordering control operators such as `.ge`.
fn validate_ordering_rhs(
    compiled: &CompiledCDDL,
    rhs: &WrappedNode,
    op: &str,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let expected = resolve_number_rhs(compiled, rhs).or_else(|| parse_number_from_node(rhs));
    let Some(expected) = expected else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a numeric comparison bound",
            format!("{value}"),
            Some(format!("{op} RHS did not resolve to a number")),
        ));
        return false;
    };

    let Some(actual) = value_to_number(value) else {
        issues.push(ValidationIssue::new(
            path.clone(),
            format!("number {op} {expected}"),
            format!("{value}"),
            Some("ordering validation requires a numeric value".to_owned()),
        ));
        return false;
    };

    let ok = match op {
        ".lt" => numeric_lt(actual, expected),
        ".le" => numeric_le(actual, expected),
        ".gt" => numeric_gt(actual, expected),
        ".ge" => numeric_ge(actual, expected),
        _ => false,
    };

    if !ok {
        issues.push(ValidationIssue::new(
            path.clone(),
            format!("number {op} {expected}"),
            actual.to_string(),
            Some("ordering constraint failed".to_owned()),
        ));
    }
    ok
}

/// Validate equality control operators such as `.eq` and `.ne`.
fn validate_equality_rhs(
    compiled: &CompiledCDDL,
    rhs: &WrappedNode,
    op: &str,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let expected = resolve_value_rhs(compiled, rhs).or_else(|| parse_value_from_node(rhs));
    let Some(expected) = expected else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "a literal equality operand",
            format!("{value}"),
            Some(format!("{op} RHS did not resolve to a literal value")),
        ));
        return false;
    };

    let equal = values_equal(value, &expected);
    let ok = match op {
        ".eq" => equal,
        ".ne" => !equal,
        _ => false,
    };

    if !ok {
        issues.push(ValidationIssue::new(
            path.clone(),
            format!("value {op} {expected}"),
            format!("{value}"),
            Some("equality constraint failed".to_owned()),
        ));
    }
    ok
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

/// Count recorded schema notes.
fn schema_note_count() -> usize {
    CURRENT_SCHEMA_NOTES.with(|slot| slot.borrow().len())
}

/// Truncate recorded schema notes after a failed speculative branch.
fn truncate_current_schema_notes(len: usize) {
    CURRENT_SCHEMA_NOTES.with(|slot| slot.borrow_mut().truncate(len));
}

/// Record a validation warning for a path if the same warning is not already present.
fn record_validation_warning_once(
    path: &[PathStep],
    text: String,
) {
    if text.trim().is_empty() {
        return;
    }

    CURRENT_VALIDATION_WARNINGS.with(|slot| {
        let mut warnings = slot.borrow_mut();
        if warnings
            .iter()
            .any(|warning| warning.path == path && warning.text == text)
        {
            return;
        }
        warnings.push(ValidationWarning {
            path: path.to_vec(),
            text,
        });
    });
}

/// Print non-failing validation warnings.
fn print_validation_warnings(warnings: &[ValidationWarning]) {
    for warning in warnings {
        println!(
            "{}",
            style(format!(
                "warning: at {}: {}",
                format_path(&warning.path),
                warning.text
            ))
            .yellow()
        );
    }
}

/// Take all recorded validation warnings.
fn take_current_validation_warnings() -> Vec<ValidationWarning> {
    CURRENT_VALIDATION_WARNINGS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Clear the recorded validation warnings.
fn clear_current_validation_warnings() {
    CURRENT_VALIDATION_WARNINGS.with(|slot| slot.borrow_mut().clear());
}

/// Current number of recorded validation warnings.
fn validation_warning_count() -> usize {
    CURRENT_VALIDATION_WARNINGS.with(|slot| slot.borrow().len())
}

/// Truncate validation warnings back to a previous count.
fn truncate_validation_warnings(len: usize) {
    CURRENT_VALIDATION_WARNINGS.with(|slot| slot.borrow_mut().truncate(len));
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
#[cfg(test)]
fn root_rule_name(compiled: &CompiledCDDL) -> Option<String> {
    compiled
        .user_nodes
        .iter()
        .find_map(top_level_rule_signature)
        .map(|(name, _)| name)
}

/// Extract the top-level rule base name from a rule line.
fn top_level_rule_name(node: &WrappedNode) -> Option<String> {
    top_level_rule_signature(node).map(|(name, _)| name)
}

/// Extract `(base_name, is_generic)` from a top-level rule line.
///
/// The base name stops at the first space, `<`, or tab so it captures
/// the head of a possibly generic LHS (`wrapper<t>` → `wrapper`). The
/// boolean is `true` when the LHS contains a `<...>` generic parameter
/// list, which is what `--type` must reject.
fn top_level_rule_signature(node: &WrappedNode) -> Option<(String, bool)> {
    let WrappedNode::RuleLine { text, .. } = node else {
        return None;
    };

    let lhs = text
        .split_once('=')
        .map_or(text.as_str(), |(lhs, _)| lhs)
        .trim();
    if lhs.ends_with('/') {
        return None;
    }
    let lhs = lhs.strip_suffix(':').map_or(lhs, str::trim);
    let is_generic = lhs.contains('<');
    let name: String = lhs
        .chars()
        .take_while(|ch| !matches!(ch, ' ' | '<' | '\t'))
        .collect();
    Some((name, is_generic))
}

/// Reason `--type` selection failed.
///
/// All variants carry the requested type name so the CLI can name it
/// in the error message. `ImportedOrIncluded` additionally carries the
/// origin path of the rule we found so the user can see where the
/// colliding definition lives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootSelectionError {
    /// `--type` contained generic syntax such as `wrapper<t>`.
    InvalidName {
        /// The raw string the user passed.
        requested: String,
    },
    /// The requested rule exists in the schema but is generic; generic
    /// templates cannot be selected as the validation root.
    Generic {
        /// The base name the user requested.
        requested: String,
    },
    /// The requested rule is not declared directly in the schema file
    /// passed to `validate`. The optional origin points at where it
    /// was found (include, import, postlude).
    NotInPrimarySchema {
        /// The base name the user requested.
        requested: String,
        /// Where the rule was found, when known.
        origin: Option<PathBuf>,
        /// Why the rule is not selectable: included, imported, or from
        /// the standard postlude.
        source: RootSource,
    },
    /// The requested rule does not exist anywhere in the schema.
    Missing {
        /// The base name the user requested.
        requested: String,
    },
    /// The schema has no top-level rule at all and `--type` was not
    /// supplied.
    NoRootRule,
}

/// What kind of non-primary origin supplied the rule the user picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    /// `;# include` material.
    Included,
    /// `;# import` material.
    Imported,
    /// Standard postlude definitions (e.g. `uint`, `tstr`).
    Postlude,
}

/// Resolve the validation root for one `cbork validate` run.
///
/// With `type_name = None` this returns the natural first top-level
/// rule declared in the primary schema, or `Err(NoRootRule)` if the
/// schema has none.
///
/// With `type_name = Some(name)` this only accepts a concrete
/// (non-generic) top-level rule whose `origin.source_path` matches the
/// primary schema path passed to `CompiledCDDL::compile`. Rules that
/// only arrive through include/import/postlude are rejected; when such
/// a rule is the only match, the error includes the rule's origin so
/// the user can see where it came from.
fn resolve_validation_root(
    compiled: &CompiledCDDL,
    schema_path: &Path,
    type_name: Option<&str>,
) -> Result<String, RootSelectionError> {
    let canonical_schema = canonicalize_primary_path(compiled, schema_path);

    let primary_matches: Vec<&WrappedNode> = compiled
        .user_nodes
        .iter()
        .filter(|node| matches!(node, WrappedNode::RuleLine { .. }))
        .filter(|node| {
            canonical_schema
                .as_ref()
                .is_none_or(|cs| node.origin().source_path == *cs)
        })
        .collect();

    match type_name {
        None => {
            primary_matches
                .iter()
                .find_map(|node| top_level_rule_signature(node))
                .map(|(name, _)| name)
                .ok_or(RootSelectionError::NoRootRule)
        },
        Some(requested_raw) => {
            let requested = requested_raw.trim();
            if requested.is_empty() {
                return Err(RootSelectionError::InvalidName {
                    requested: requested_raw.to_owned(),
                });
            }
            if requested.contains(['<', '>', ' ']) {
                return Err(RootSelectionError::InvalidName {
                    requested: requested_raw.to_owned(),
                });
            }

            // Same-name match in the primary schema, split by generic-ness.
            let mut primary_concrete: Vec<&WrappedNode> = Vec::new();
            let mut primary_generic: Vec<&WrappedNode> = Vec::new();
            for node in &primary_matches {
                if let Some((name, is_generic)) = top_level_rule_signature(node)
                    && name == requested
                {
                    if is_generic {
                        primary_generic.push(*node);
                    } else {
                        primary_concrete.push(*node);
                    }
                }
            }

            if !primary_generic.is_empty() {
                return Err(RootSelectionError::Generic {
                    requested: requested.to_owned(),
                });
            }

            match primary_concrete.len() {
                1 => Ok(requested.to_owned()),
                0 => {
                    Err(lookup_root_selection_error(
                        compiled,
                        canonical_schema.as_deref(),
                        requested,
                    ))
                },
                _ => {
                    // Multiple concrete same-name rules in the primary
                    // file: defer to the compiler's existing
                    // diagnostics by picking the first match. The plan
                    // says not to add bespoke ambiguity handling unless
                    // silent wrong-rule selection is otherwise
                    // possible; in practice the compiler will already
                    // have raised a diagnostic for this.
                    Ok(requested.to_owned())
                },
            }
        },
    }
}

/// Canonicalize the primary schema path the way the compiler saw it.
///
/// `CompiledCDDL::compile` normalizes the path it stores in node
/// origins; mirror that here so equality comparisons against
/// `node.origin().source_path` line up regardless of how the caller
/// spelled the path on the command line.
fn canonicalize_primary_path(
    compiled: &CompiledCDDL,
    schema_path: &Path,
) -> Option<PathBuf> {
    if let Some(node) = compiled
        .user_nodes
        .iter()
        .find(|node| matches!(node, WrappedNode::RuleLine { .. }))
    {
        return Some(node.origin().source_path.clone());
    }
    std::fs::canonicalize(schema_path).ok()
}

/// Build a missing-root error, enriched with where the same name
/// appears (if anywhere) when it is not in the primary schema.
fn lookup_root_selection_error(
    compiled: &CompiledCDDL,
    canonical_primary: Option<&Path>,
    requested: &str,
) -> RootSelectionError {
    let mut postlude_origin = false;
    let mut other_origin: Option<(RootSource, PathBuf)> = None;

    for node in &compiled.complete_nodes {
        let WrappedNode::RuleLine { .. } = node else {
            continue;
        };
        let Some((name, _)) = top_level_rule_signature(node) else {
            continue;
        };
        if name != requested {
            continue;
        }
        if canonical_primary.is_some_and(|cs| node.origin().source_path == cs) {
            // Already handled by the primary pass; this is the
            // duplicate-concrete case.
            continue;
        }
        if node.metadata().contains(&MetaData::StandardPostlude) {
            postlude_origin = true;
            continue;
        }
        // Heuristic: a node's containing file determines whether it
        // came from include vs import. `CompiledCDDL` does not expose
        // that bit on the node itself, so use the source file's
        // directory as a hint: files that live next to the primary
        // schema are usually `include`d; files anywhere else are
        // usually `import`ed.
        let source = classify_other_origin(
            canonical_primary.and_then(|p| p.parent()),
            node.origin().source_path.as_path(),
        );
        let path = node.origin().source_path.clone();
        match other_origin.as_ref() {
            Some((existing, existing_path)) if existing_path == &path => {},
            _ => {
                other_origin = Some((source, path));
            },
        }
    }

    if let Some((source, origin)) = other_origin {
        return RootSelectionError::NotInPrimarySchema {
            requested: requested.to_owned(),
            origin: Some(origin),
            source,
        };
    }
    if postlude_origin {
        return RootSelectionError::NotInPrimarySchema {
            requested: requested.to_owned(),
            origin: None,
            source: RootSource::Postlude,
        };
    }
    RootSelectionError::Missing {
        requested: requested.to_owned(),
    }
}

/// Decide whether a non-primary origin is more likely an `include` or
/// an `import`.
///
/// `CompiledCDDL` does not tag nodes with their directive of origin;
/// we fall back to a directory heuristic. Files that live next to the
/// primary schema (same parent directory) are usually `include`d;
/// files anywhere else are usually `import`ed.
fn classify_other_origin(
    primary_parent: Option<&Path>,
    path: &Path,
) -> RootSource {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return RootSource::Imported;
    };
    if file_name.contains("postlude") || file_name.contains("stdlib") {
        return RootSource::Postlude;
    }
    if primary_parent.is_some_and(|p| path.parent() == Some(p)) {
        RootSource::Included
    } else {
        RootSource::Imported
    }
}

/// Render a `RootSelectionError` as a user-facing message.
fn format_root_selection_error(
    error: &RootSelectionError,
    schema_path: &Path,
) -> String {
    match error {
        RootSelectionError::InvalidName { requested } => {
            format!(
                "validation error: --type {requested:?} must be a plain rule name; \
                 generic syntax like `wrapper<t>` is not selectable"
            )
        },
        RootSelectionError::Generic { requested } => {
            format!(
                "validation error: --type {requested:?} names a generic rule template; \
                 generic rule templates cannot be selected as the validation root"
            )
        },
        RootSelectionError::NotInPrimarySchema {
            requested,
            origin: Some(origin),
            source,
        } => {
            let verb = match source {
                RootSource::Included => "included in",
                RootSource::Imported => "imported from",
                RootSource::Postlude => "supplied by the standard postlude in",
            };
            format!(
                "validation error: --type {requested:?} can only select a rule declared \
                 directly in {schema}; rule `{requested}` is {verb} {origin}",
                schema = schema_path.display(),
                origin = origin.display(),
            )
        },
        RootSelectionError::NotInPrimarySchema {
            requested,
            origin: None,
            source: RootSource::Postlude,
        } => {
            format!(
                "validation error: --type {requested:?} can only select a rule declared \
                 directly in {schema}; rule `{requested}` is supplied by the standard \
                 postlude and cannot be selected as the validation root",
                schema = schema_path.display(),
            )
        },
        RootSelectionError::NotInPrimarySchema { requested, .. } => {
            format!(
                "validation error: --type {requested:?} can only select a rule declared \
                 directly in {schema}; rule `{requested}` is not declared in that file",
                schema = schema_path.display(),
            )
        },
        RootSelectionError::Missing { requested } => {
            format!(
                "validation error: --type {requested:?} does not name any rule in {schema}",
                schema = schema_path.display(),
            )
        },
        RootSelectionError::NoRootRule => {
            format!(
                "validation error: no root rule found in {}",
                schema_path.display()
            )
        },
    }
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
                if item_index < items.len()
                    && let Some(item) = items.get(item_index)
                {
                    let mut child_path = path.to_owned();
                    child_path.push(PathStep::ArrayItem(item_index));
                    record_schema_note_once(&child_path, schema_summary(body));
                    let before = issues.len();
                    let warning_len = validation_warning_count();
                    validate_schema_node(
                        compiled,
                        definitions,
                        body,
                        item,
                        &mut child_path,
                        issues,
                    );
                    if issues.len() == before {
                        item_index = item_index.saturating_add(1);
                    } else {
                        issues.truncate(before);
                        truncate_validation_warnings(warning_len);
                    }
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
    require_all_used: bool,
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

        if memberkey.is_none() {
            if !validate_nested_map_group_entry(
                compiled,
                definitions,
                body,
                occur,
                entries,
                used,
                path,
                issues,
            ) {
                return false;
            }
            continue;
        }

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

    if require_all_used && used.iter().any(|used| !*used) {
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

/// Validate a no-memberkey map `grpent` as a nested group entry.
fn validate_nested_map_group_entry(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    body: &WrappedNode,
    occur: &str,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    match occur {
        "?" => {
            let _ = try_validate_map_group_body(
                compiled,
                definitions,
                body,
                entries,
                used,
                path,
                issues,
            );
            true
        },
        "+" | "*" => {
            let mut count = 0usize;
            while try_validate_map_group_body(
                compiled,
                definitions,
                body,
                entries,
                used,
                path,
                issues,
            ) {
                count = count.saturating_add(1);
            }

            if occur == "+" && count == 0 {
                issues.push(ValidationIssue::new(
                    path.to_owned(),
                    "one or more matching map group entries",
                    "no match",
                    Some("required repeated map group entry was missing".to_owned()),
                ));
                return false;
            }

            true
        },
        _ => {
            if try_validate_map_group_body(compiled, definitions, body, entries, used, path, issues)
            {
                true
            } else {
                issues.push(ValidationIssue::new(
                    path.to_owned(),
                    "a matching map group entry",
                    "no match",
                    Some("required map group entry was missing".to_owned()),
                ));
                false
            }
        },
    }
}

/// Try a nested map group body, rolling back speculative state on failure.
fn try_validate_map_group_body(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    body: &WrappedNode,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let used_before = used.to_vec();
    let issue_len = issues.len();
    let warning_len = validation_warning_count();
    let note_len = schema_note_count();

    if validate_map_group_body(compiled, definitions, body, entries, used, path, issues)
        && used != used_before.as_slice()
    {
        return true;
    }

    used.copy_from_slice(&used_before);
    issues.truncate(issue_len);
    truncate_validation_warnings(warning_len);
    truncate_current_schema_notes(note_len);
    false
}

/// Validate a schema node that appears where CDDL expects a map group entry.
fn validate_map_group_body(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    body: &WrappedNode,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    match body {
        WrappedNode::RuleLine { children, .. } => {
            if let Some(rhs) = find_rhs_node(children) {
                return validate_map_group_body(
                    compiled,
                    definitions,
                    rhs,
                    entries,
                    used,
                    path,
                    issues,
                );
            }
            if let Some(group) = node_children_find(body, "group") {
                return validate_map_group_body(
                    compiled,
                    definitions,
                    group,
                    entries,
                    used,
                    path,
                    issues,
                );
            }
        },
        WrappedNode::Syntax {
            rule,
            children,
            text,
            ..
        } => {
            return match rule.as_str() {
                "grpent" => {
                    if let Some(group) = node_children_find(body, "group") {
                        validate_map_group_body(
                            compiled,
                            definitions,
                            group,
                            entries,
                            used,
                            path,
                            issues,
                        )
                    } else {
                        false
                    }
                },
                "group" => {
                    validate_map_group_node(
                        compiled,
                        definitions,
                        body,
                        entries,
                        used,
                        path,
                        issues,
                    )
                },
                "type" => {
                    validate_map_type_choice(
                        compiled,
                        definitions,
                        children,
                        entries,
                        used,
                        path,
                        issues,
                    )
                },
                "type1" => {
                    validate_map_type1(compiled, definitions, children, entries, used, path, issues)
                },
                "type2" => {
                    validate_map_type2(
                        compiled,
                        definitions,
                        body,
                        children,
                        entries,
                        used,
                        path,
                        issues,
                    )
                },
                "typename" | "groupname" => {
                    validate_named_map_group(
                        compiled,
                        definitions,
                        text.trim(),
                        entries,
                        used,
                        path,
                        issues,
                    )
                },
                _ => false,
            };
        },
        _ => {},
    }

    false
}

/// Validate a concrete `group` node in map-entry context.
fn validate_map_group_node(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    group: &WrappedNode,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let mut branch_issues = Vec::new();
    for grpchoice in group_children(group, "grpchoice") {
        let mut local_used = used.to_vec();
        let mut local_issues = Vec::new();
        let warning_len = validation_warning_count();
        let note_len = schema_note_count();
        if validate_grpchoice_map(
            compiled,
            definitions,
            grpchoice,
            entries,
            &mut local_used,
            path,
            &mut local_issues,
            false,
        ) && local_used != *used
        {
            used.copy_from_slice(&local_used);
            return true;
        }
        truncate_validation_warnings(warning_len);
        truncate_current_schema_notes(note_len);
        branch_issues.push(local_issues);
    }

    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
    false
}

/// Validate map group alternatives.
fn validate_map_type_choice(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let mut saw_branch = false;
    let mut branch_issues = Vec::new();
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "type1"
        {
            saw_branch = true;
            let mut local_used = used.to_vec();
            let mut local_issues = Vec::new();
            let warning_len = validation_warning_count();
            let note_len = schema_note_count();
            if validate_map_group_body(
                compiled,
                definitions,
                child,
                entries,
                &mut local_used,
                path,
                &mut local_issues,
            ) && local_used != *used
            {
                used.copy_from_slice(&local_used);
                return true;
            }
            truncate_validation_warnings(warning_len);
            truncate_current_schema_notes(note_len);
            branch_issues.push(local_issues);
        }
    }

    if !saw_branch {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "a map group alternative",
            "empty type",
            Some("empty map group type choice".to_owned()),
        ));
    } else if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
    false
}

/// Validate a `type1` node in map-entry context.
fn validate_map_type1(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let mut lhs: Option<&WrappedNode> = None;
    let mut has_ctlop = false;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if lhs.is_none() => lhs = Some(child),
                "ctlop" => has_ctlop = true,
                _ => {},
            }
        }
    }

    if has_ctlop {
        return false;
    }

    if let Some(lhs) = lhs {
        return validate_map_group_body(compiled, definitions, lhs, entries, used, path, issues);
    }

    false
}

/// Validate a `type2` node in map-entry context.
fn validate_map_type2(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    children: &[WrappedNode],
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    if let Some(group) = children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
    {
        return validate_map_group_body(compiled, definitions, group, entries, used, path, issues);
    }

    if let Some(name) = children.iter().find_map(|child| {
        match child {
            WrappedNode::Syntax { rule, text, .. } if rule == "typename" || rule == "groupname" => {
                Some(text.trim().to_owned())
            },
            _ => None,
        }
    }) {
        return validate_named_map_group(compiled, definitions, &name, entries, used, path, issues);
    }

    if let Some(group) = node_children_find(node, "group") {
        return validate_map_group_body(compiled, definitions, group, entries, used, path, issues);
    }

    false
}

/// Validate a named rule as a map group entry.
fn validate_named_map_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    name: &str,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    if let Some(node) = definitions.get(name) {
        return validate_map_group_body(compiled, definitions, node, entries, used, path, issues);
    }

    let resolution = build_resolution(&compiled.complete_nodes);
    let plugs = resolution.plugs_for(name);
    if !plugs.is_empty() {
        let mut branch_issues = Vec::new();
        for plug in plugs {
            let mut local_used = used.to_vec();
            let mut local_issues = Vec::new();
            let warning_len = validation_warning_count();
            let note_len = schema_note_count();
            if validate_map_group_body(
                compiled,
                definitions,
                plug,
                entries,
                &mut local_used,
                path,
                &mut local_issues,
            ) && local_used != *used
            {
                used.copy_from_slice(&local_used);
                return true;
            }
            truncate_validation_warnings(warning_len);
            truncate_current_schema_notes(note_len);
            branch_issues.push(local_issues);
        }

        if let Some(best) = best_issue_branch(branch_issues) {
            issues.extend(best);
        }
        return false;
    }

    if is_socket_name(name) {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            format!("a map entry accepted by socket `{name}`"),
            "no match",
            Some("socket has no plugged definitions".to_owned()),
        ));
    } else {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            format!("definition `{name}`"),
            "no match",
            Some("undefined rule reference".to_owned()),
        ));
    }
    false
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

/// Pick the most useful failed branch to display after an alternative summary.
fn best_issue_branch(branches: Vec<Vec<ValidationIssue>>) -> Option<Vec<ValidationIssue>> {
    branches
        .into_iter()
        .filter(|branch| !branch.is_empty())
        .min_by_key(|branch| {
            let unsupported = branch
                .iter()
                .filter(|issue| {
                    issue
                        .message
                        .as_deref()
                        .is_some_and(|message| message.contains("not implemented"))
                })
                .count();
            (unsupported, branch.len())
        })
}

/// Whether a control operator belongs to the deferred compression-validation family.
fn is_compression_ctlop(op: &str) -> bool {
    matches!(
        op,
        ".x-compressed"
            | ".x-compressed.abnf"
            | ".x-compressed.abnfb"
            | ".x-brotli"
            | ".x-brotli.abnf"
            | ".x-brotli.abnfb"
            | ".x-zstd"
            | ".x-zstd.abnf"
            | ".x-zstd.abnfb"
            | ".x-gzip"
            | ".x-gzip.abnf"
            | ".x-gzip.abnfb"
            | ".x-deflate"
            | ".x-deflate.abnf"
            | ".x-deflate.abnfb"
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

/// Resolve the RHS of a control operator to a number if possible.
fn resolve_number_rhs(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<NumericValue> {
    let state = resolve_type2_leaf(node, &compiled.resolved_types)?;
    match state {
        EntryState::Integer(value) => Some(NumericValue::Integer(value)),
        EntryState::Float(value) => Some(NumericValue::Float(value)),
        _ => None,
    }
}

/// Resolve the RHS of an equality control operator to a value if possible.
fn resolve_value_rhs(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<Value> {
    let state = resolve_type2_leaf(node, &compiled.resolved_types)?;
    entry_state_to_value(&state)
}

/// Parse an integer literal from a node.
fn parse_integer_from_node(node: &WrappedNode) -> Option<i128> {
    match parse_value_literal(node)? {
        EntryState::Integer(value) => Some(value),
        _ => None,
    }
}

/// Parse a literal value from a node.
fn parse_value_from_node(node: &WrappedNode) -> Option<Value> {
    let state = parse_value_literal(node)?;
    entry_state_to_value(&state)
}

/// Parse a numeric literal from a node.
fn parse_number_from_node(node: &WrappedNode) -> Option<NumericValue> {
    match parse_value_literal(node)? {
        EntryState::Integer(value) => Some(NumericValue::Integer(value)),
        EntryState::Float(value) => Some(NumericValue::Float(value)),
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

/// Convert a resolved entry state into a CBOR value for equality comparison.
fn entry_state_to_value(state: &EntryState) -> Option<Value> {
    match state {
        EntryState::Integer(value) => (*value).try_into().ok().map(Value::Integer),
        EntryState::Float(value) => Some(Value::Float(Float::F64(*value))),
        EntryState::Text(value) => {
            String::from_utf8(value.as_ref().to_vec())
                .ok()
                .map(Value::Text)
        },
        EntryState::Bytes(value) => Some(Value::Bytes(value.as_ref().to_vec())),
        _ => None,
    }
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

/// Convert a CBOR value to a numeric comparison value if possible.
fn value_to_number(value: &Value) -> Option<NumericValue> {
    match value {
        Value::Integer(int) => Some(NumericValue::Integer(i128::from(*int))),
        Value::Float(Float::F16(value) | Float::F32(value)) => {
            Some(NumericValue::Float(f64::from(*value)))
        },
        Value::Float(Float::F64(value)) => Some(NumericValue::Float(*value)),
        _ => None,
    }
}

/// Compare two numeric values using CDDL ordering semantics.
fn numeric_lt(
    lhs: NumericValue,
    rhs: NumericValue,
) -> bool {
    match (lhs, rhs) {
        (NumericValue::Integer(lhs), NumericValue::Integer(rhs)) => lhs < rhs,
        (lhs, rhs) => numeric_as_f64(lhs) < numeric_as_f64(rhs),
    }
}

/// Compare two numeric values using CDDL ordering semantics.
fn numeric_le(
    lhs: NumericValue,
    rhs: NumericValue,
) -> bool {
    match (lhs, rhs) {
        (NumericValue::Integer(lhs), NumericValue::Integer(rhs)) => lhs <= rhs,
        (lhs, rhs) => numeric_as_f64(lhs) <= numeric_as_f64(rhs),
    }
}

/// Compare two numeric values using CDDL ordering semantics.
fn numeric_gt(
    lhs: NumericValue,
    rhs: NumericValue,
) -> bool {
    match (lhs, rhs) {
        (NumericValue::Integer(lhs), NumericValue::Integer(rhs)) => lhs > rhs,
        (lhs, rhs) => numeric_as_f64(lhs) > numeric_as_f64(rhs),
    }
}

/// Compare two numeric values using CDDL ordering semantics.
fn numeric_ge(
    lhs: NumericValue,
    rhs: NumericValue,
) -> bool {
    match (lhs, rhs) {
        (NumericValue::Integer(lhs), NumericValue::Integer(rhs)) => lhs >= rhs,
        (lhs, rhs) => numeric_as_f64(lhs) >= numeric_as_f64(rhs),
    }
}

/// Compare CBOR values for equality-control validation.
fn values_equal(
    lhs: &Value,
    rhs: &Value,
) -> bool {
    match (value_to_number(lhs), value_to_number(rhs)) {
        (Some(lhs), Some(rhs)) => numeric_eq(lhs, rhs),
        _ => lhs == rhs,
    }
}

/// Compare two numeric values for equality.
fn numeric_eq(
    lhs: NumericValue,
    rhs: NumericValue,
) -> bool {
    match (lhs, rhs) {
        (NumericValue::Integer(lhs), NumericValue::Integer(rhs)) => lhs == rhs,
        (lhs, rhs) => numeric_as_f64(lhs) == numeric_as_f64(rhs),
    }
}

/// Convert a numeric value for mixed integer/float comparisons.
#[allow(clippy::cast_precision_loss)]
fn numeric_as_f64(value: NumericValue) -> f64 {
    match value {
        NumericValue::Integer(value) => value as f64,
        NumericValue::Float(value) => value,
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
        PathStep, RootSelectionError, SchemaNote, clear_current_schema_notes, collect_definitions,
        exec, format_root_selection_error, render_validation_dump, resolve_validation_root,
        root_rule_name, set_current_source_bytes, take_current_schema_notes,
        take_current_validation_warnings, validate_document,
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
        validate_schema_bytes_with_warnings(schema_name, schema, cbor).0
    }

    fn validate_schema_bytes_with_warnings(
        schema_name: &str,
        schema: &[u8],
        cbor: &[u8],
    ) -> (Vec<super::ValidationIssue>, Vec<super::ValidationWarning>) {
        let schema = write_temp_file(schema_name, schema);
        let compiled =
            CompiledCDDL::compile(&schema, None::<&Path>).expect("schema should compile");
        let root = root_rule_name(&compiled).expect("schema should have root rule");
        let definitions = collect_definitions(&compiled.complete_nodes);
        let document = Document::parse(cbor).expect("CBOR should parse");
        set_current_source_bytes(cbor);
        clear_current_schema_notes();
        super::clear_current_validation_warnings();
        let issues = validate_document(&compiled, &definitions, &root, &document);
        let warnings = take_current_validation_warnings();
        (issues, warnings)
    }

    fn validate_schema_bytes_with_dump(
        schema_name: &str,
        schema: &[u8],
        cbor: &[u8],
    ) -> (Vec<super::ValidationIssue>, String) {
        let schema = write_temp_file(schema_name, schema);
        let compiled =
            CompiledCDDL::compile(&schema, None::<&Path>).expect("schema should compile");
        let root = root_rule_name(&compiled).expect("schema should have root rule");
        let definitions = collect_definitions(&compiled.complete_nodes);
        let document = Document::parse(cbor).expect("CBOR should parse");
        set_current_source_bytes(cbor);
        clear_current_schema_notes();
        super::clear_current_validation_warnings();
        let issues = validate_document(&compiled, &definitions, &root, &document);
        let notes = take_current_schema_notes();
        let dump = render_validation_dump(&schema, "input.cbor", &document, &notes, None, false);
        (issues, dump)
    }

    #[test]
    fn validate_succeeds_for_matching_integer() {
        let schema = write_temp_file("schema_ok.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_ok.cbor", &[0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, None, true));
    }

    #[test]
    fn validate_fails_for_mismatched_integer() {
        let schema = write_temp_file("schema_fail.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_fail.cbor", &[0x02]);

        assert!(!exec(&schema, Some(&cbor), false, false, None, true));
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
    fn validate_map_group_socket_entry_matches_whole_entry() {
        let schema = br"
root = {
  -19 => bstr .size 32
  one-pq-private-key
}
one-pq-private-key //= (-48 => bstr .size 32)
one-pq-private-key //= (-49 => bstr .size 32)
one-pq-private-key //= (-50 => bstr .size 32)
";
        let mut cbor = vec![0xA2, 0x32, 0x58, 0x20];
        cbor.extend([0x61; 32]);
        cbor.extend([0x38, 0x30, 0x58, 0x20]);
        cbor.extend([0x62; 32]);

        let issues = validate_schema_bytes("map_group_socket_entry.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validation_dump_discards_failed_alternative_labels() {
        let schema = br"
root = [ signature, { bad => bstr .size 2 } ] / [ private_key, { ml-dsa-65 => bstr .size 1 } ]
signature = 2
private_key = 2
bad = -48
ml-dsa-65 = -49
";
        let cbor = [0x82, 0x02, 0xA1, 0x38, 0x30, 0x41, 0xAA];

        let (issues, dump) =
            validate_schema_bytes_with_dump("failed_alternative_labels.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("/private_key/ 2"), "{dump}");
        assert!(dump.contains("/ml-dsa-65/ -49"), "{dump}");
        assert!(!dump.contains("/signature/ 2"), "{dump}");
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
    fn validate_accepts_argon2id_style_hash_and_ordering_ctlops() {
        let schema = br"
root = any .dtrm (tagged / untagged)
tagged = #6.33001(untagged)
untagged = ([tag] / [tag, options])
tag = (bstr .x-hash any) .size 32
options = {
  ? 1 => memcost,
  ? 2 => timecost,
  ? 3 => parallelism,
  ? 4 => bstr .size 16,
}
memcost = (uint .ge 8192) .default 8192
timecost = (uint .ge 1) .default 1
parallelism = (uint .ge 1) .default 1
";
        let cbor = [
            0x82, 0x58, 0x20, 0x84, 0x02, 0x3C, 0x41, 0x9A, 0x7D, 0x47, 0xE5, 0x35, 0xF1, 0x6D,
            0xA0, 0x33, 0x78, 0xD9, 0x5D, 0xE4, 0xA2, 0xDD, 0xBB, 0x37, 0xF8, 0x5D, 0x2A, 0xCB,
            0xAA, 0x21, 0xEA, 0xD6, 0xED, 0x88, 0x9C, 0xA4, 0x01, 0x1A, 0x00, 0x01, 0x00, 0x00,
            0x02, 0x03, 0x03, 0x04, 0x04, 0x50, 0xAD, 0x2B, 0xC5, 0xDF, 0x31, 0xAF, 0xDC, 0xB8,
            0x58, 0xBE, 0x98, 0x15, 0x4B, 0xC0, 0xFF, 0x38,
        ];

        let issues = validate_schema_bytes("argon2id_style.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_optional_array_element_does_not_consume_mismatch() {
        let issues =
            validate_schema_bytes("optional_array_backtracks.cddl", b"root = [ ? 1, 2 ]\n", &[
                0x81, 0x02,
            ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_reports_only_best_failed_alternative_details() {
        let issues = validate_schema_bytes("compact_type_choice.cddl", b"root = ([1] / [2])\n", &[
            0x81, 0x03,
        ]);
        let integer_mismatches = issues
            .iter()
            .filter(|issue| issue.message.as_deref() == Some("integer value did not match"))
            .count();

        assert_eq!(integer_mismatches, 1, "{issues:#?}");
    }

    #[test]
    fn validate_treats_x_enc_as_plain_annotation() {
        let issues =
            validate_schema_bytes("x_enc_annotation.cddl", b"root = bstr .x-enc any\n", &[
                0x43, b'a', b'b', b'c',
            ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_checks_eq_and_ne_control_operators() {
        let eq_issues = validate_schema_bytes("eq_operator.cddl", b"root = int .eq 2\n", &[0x02]);
        let ne_issues = validate_schema_bytes("ne_operator.cddl", b"root = int .ne 2\n", &[0x03]);

        assert!(eq_issues.is_empty(), "{eq_issues:#?}");
        assert!(ne_issues.is_empty(), "{ne_issues:#?}");
    }

    #[test]
    fn validate_rejects_failed_eq_control_operator() {
        let issues = validate_schema_bytes("eq_operator_fail.cddl", b"root = int .eq 2\n", &[0x03]);

        assert!(
            issues
                .iter()
                .any(|issue| issue.message.as_deref() == Some("equality constraint failed")),
            "{issues:#?}"
        );
    }

    #[test]
    fn validate_treats_feature_as_advisory() {
        let issues = validate_schema_bytes(
            "feature_operator.cddl",
            b"root = 1 .feature \"draft\"\n",
            &[0x01],
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_warns_but_does_not_fail_for_compression_operators() {
        let (issues, warnings) = validate_schema_bytes_with_warnings(
            "compression_deferred.cddl",
            b"root = bstr .x-brotli.abnf \"abc\"\n",
            &[0x43, b'a', b'b', b'c'],
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(
            warnings.iter().any(|warning| {
                warning
                    .text
                    .contains("compression operator `.x-brotli.abnf`")
            }),
            "{warnings:#?}"
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

    // Plan 008 — `--type` selection tests.

    fn write_temp_dir_tree(relative: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("cbork_validate_type_test")
            .join(relative.join("/"));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("temp dir tree should exist");
        dir
    }

    fn write_cddl(
        dir: &Path,
        name: &str,
        content: &[u8],
    ) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("cddl file should be written");
        path
    }

    fn write_cbor(
        dir: &Path,
        name: &str,
        bytes: &[u8],
    ) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("cbor file should be written");
        path
    }

    fn cbor_integer(value: u8) -> Vec<u8> {
        // CBOR major type 0 (unsigned integer). Values 0..=23 fit in
        // the low 5 bits of the initial byte; values 24..=255 need an
        // additional 1-byte payload (`0x18 <value>`).
        if value <= 23 {
            vec![value]
        } else {
            vec![0x18, value]
        }
    }

    #[test]
    fn type_override_none_uses_natural_root() {
        let dir = write_temp_dir_tree(&["natural_root"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = 1\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(1));

        assert!(exec(&schema, Some(&cbor), false, false, None, true));
    }

    #[test]
    fn type_override_picks_later_local_rule() {
        let dir = write_temp_dir_tree(&["later_rule"]);
        // Root references payload so neither rule is unreferenced.
        let schema = write_cddl(&dir, "schema.cddl", b"root = payload\npayload = 2\n");
        let matching = write_cbor(&dir, "match.cbor", &cbor_integer(2));
        let mismatching = write_cbor(&dir, "miss.cbor", &cbor_integer(1));

        assert!(exec(
            &schema,
            Some(&matching),
            false,
            false,
            Some("payload"),
            true
        ));
        assert!(!exec(
            &schema,
            Some(&mismatching),
            false,
            false,
            Some("payload"),
            true
        ));
    }

    #[test]
    fn type_override_unknown_name_fails() {
        let dir = write_temp_dir_tree(&["unknown"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = 1\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(1));

        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("missing"),
            true
        ));
    }

    #[test]
    fn type_override_generic_template_fails() {
        let dir = write_temp_dir_tree(&["generic"]);
        let schema = write_cddl(&dir, "schema.cddl", b"wrapper<t> = [t]\npayload = 1\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(1));

        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("wrapper"),
            true
        ));
        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("wrapper<t>"),
            true
        ));
    }

    #[test]
    fn type_override_included_only_rule_fails() {
        let dir = write_temp_dir_tree(&["included_only"]);
        write_cddl(&dir, "helper.cddl", b"shared = 5\n");
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b";# include \"helper.cddl\"\nroot = 1\n",
        );
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(1));

        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("shared"),
            true
        ));
    }

    #[test]
    fn type_override_can_reference_included_helpers() {
        let dir = write_temp_dir_tree(&["ref_include"]);
        write_cddl(&dir, "helper.cddl", b"helper = uint\n");
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = payload\n;# include \"helper.cddl\"\npayload = helper\n",
        );
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(42));

        assert!(exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("payload"),
            true
        ));
    }

    #[test]
    fn type_override_detailed_dump_uses_selected_root() {
        let dir = write_temp_dir_tree(&["detailed"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = payload\npayload = 2\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(2));

        let compiled = CompiledCDDL::compile(&schema, None::<&Path>).expect("schema compiles");
        let root = resolve_validation_root(&compiled, &schema, Some("payload"))
            .expect("payload is selectable");
        assert_eq!(root, "payload");

        // Detailed dump still passes through `exec`.
        assert!(exec(
            &schema,
            Some(&cbor),
            false,
            true,
            Some("payload"),
            true
        ));
    }

    #[test]
    fn root_selection_error_messages_cover_each_variant() {
        let dir = write_temp_dir_tree(&["errors"]);
        let schema = write_cddl(&dir, "schema.cddl", b"wrapper<t> = [t]\n");

        let err = resolve_validation_root(
            &CompiledCDDL::compile(&schema, None::<&Path>).expect("schema compiles"),
            &schema,
            Some("wrapper"),
        )
        .expect_err("generic rule must be rejected");
        assert!(matches!(err, RootSelectionError::Generic { .. }));

        let rendered = format_root_selection_error(&err, &schema);
        assert!(rendered.contains("generic rule template"));
        assert!(rendered.contains("wrapper"));

        let invalid = RootSelectionError::InvalidName {
            requested: "foo<t>".to_owned(),
        };
        let rendered = format_root_selection_error(&invalid, &schema);
        assert!(rendered.contains("generic syntax"));

        let missing = RootSelectionError::Missing {
            requested: "absent".to_owned(),
        };
        let rendered = format_root_selection_error(&missing, &schema);
        assert!(rendered.contains("absent"));
        assert!(rendered.contains("does not name any rule"));
    }

    #[test]
    fn type_override_postlude_rule_is_rejected() {
        // `uint` is a standard-postlude rule; selecting it must fail
        // even though it appears in `complete_nodes`.
        let dir = write_temp_dir_tree(&["postlude"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = uint\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(0));

        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            Some("uint"),
            true
        ));
    }
}
