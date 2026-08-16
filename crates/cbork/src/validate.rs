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

use cbork_abnf_parser::{AbnfMatch, parse_abnf};
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
    decode::{
        ColorKind, EmbedBudget, EmbedLimits, push_bracket, push_colored, push_dim, push_indent,
        read_input, release_embed_depth, reset_render_counters, reset_sequence_counter,
        try_charge_embed, try_charge_sequence_item,
    },
    diagnostics::{has_error_diagnostics, print_compiler_diagnostics},
    render_abnf_breakdown,
};

thread_local! {
    static CURRENT_SOURCE_BYTES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static CURRENT_SOURCE_PATH: RefCell<Vec<PathStep>> = const { RefCell::new(Vec::new()) };
    static CURRENT_SCHEMA_NOTES: RefCell<Vec<SchemaNote>> = const { RefCell::new(Vec::new()) };
    static CURRENT_VALIDATION_WARNINGS: RefCell<Vec<ValidationWarning>> = const { RefCell::new(Vec::new()) };
    static CURRENT_EMBEDDED_CBOR_HINTS: RefCell<Vec<EmbeddedCborHint>> = const { RefCell::new(Vec::new()) };
    static CURRENT_SERIALIZATION_FLOOR: RefCell<Option<u8>> =
        const { RefCell::new(None) };
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

/// The CBOR serialization control operator that produced an embedded payload.
///
/// The schema-aware renderer uses this to pick a single-item vs. sequence
/// presentation and to carry the `.dtrm`/`.prefp` validation behavior
/// separately from rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbeddedCborOperator {
    /// `.cbor` — single embedded CBOR item.
    Cbor,
    /// `.cborseq` — one or more embedded CBOR items.
    CborSeq,
    /// `.prefp` — single embedded CBOR item (preferred-plus encoding).
    Prefp,
    /// `.prefpseq` — one or more embedded CBOR items (preferred-plus encoding).
    PrefpSeq,
    /// `.dtrm` — single deterministically-encoded embedded CBOR item.
    Dtrm,
    /// `.dtrmseq` — one or more deterministically-encoded embedded CBOR items.
    DtrmSeq,
}

impl EmbeddedCborOperator {
    /// Return true when this operator permits more than one top-level item
    /// inside the embedded byte string.
    fn allows_sequence(self) -> bool {
        matches!(self, Self::CborSeq | Self::PrefpSeq | Self::DtrmSeq,)
    }

    /// Return true when this operator requires deterministic encoding
    /// (`draft-ietf-cbor-serialization-06` Section 5).
    fn requires_deterministic(self) -> bool {
        matches!(self, Self::Dtrm | Self::DtrmSeq)
    }

    /// Return true when this operator requires preferred-plus encoding
    /// (`draft-ietf-cbor-serialization-06` Section 4), either as its own
    /// check (`.prefp`/`.prefpseq`) or as a strict superset (`.dtrm`/
    /// `.dtrmseq`).
    fn requires_preferred_plus(self) -> bool {
        matches!(
            self,
            Self::Prefp | Self::PrefpSeq | Self::Dtrm | Self::DtrmSeq,
        )
    }
}

/// Hint that an embedded-CBOR byte string at a known path has been parsed
/// and can be rendered in its decoded form during the detailed dump.
///
/// The retained `Document` preserves every top-level item, so the renderer
/// can choose between single-item and sequence presentation without
/// re-parsing the raw bytes. Original bytes are still recoverable from the
/// `Value::Bytes` the validator originally handed off.
#[derive(Debug, Clone)]
struct EmbeddedCborHint {
    /// Path of the outer byte-string field.
    path: Vec<PathStep>,
    /// Serialization operator that produced the byte string.
    operator: EmbeddedCborOperator,
    /// Parsed embedded payload.
    document: Document,
}

/// Validate a CDDL schema against a CBOR payload.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)]
pub(crate) fn exec(
    schema_path: &Path,
    cbor_path: Option<&Path>,
    show_warnings: bool,
    detailed: bool,
    fails: bool,
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

    set_current_source_context(&input, &[]);
    clear_current_schema_notes();
    clear_current_validation_warnings();
    clear_current_embedded_cbor_hints();
    crate::render_abnf_breakdown::reset_traces_for_exec();

    let root_name = match resolve_validation_root(&compiled, schema_path, type_name) {
        Ok((name, _node)) => name,
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
    let embedded_hints = take_current_embedded_cbor_hints();

    if issues.is_empty() {
        if fails {
            let dump = render_validation_dump(
                schema_path,
                &input_path,
                &document,
                &RenderContext::new(&schema_notes, &embedded_hints),
                None,
                !force_no_color,
            );
            if force_no_color {
                println!("{dump}");
            } else {
                println!("{}", style(dump).dim());
            }
            println!(
                "{}:{} {} {} : ERROR",
                schema_path.display(),
                root_name,
                style("==").dim(),
                input_path,
            );
            print_validation_warnings(&validation_warnings);
            return false;
        }

        if detailed {
            let dump = render_validation_dump(
                schema_path,
                &input_path,
                &document,
                &RenderContext::new(&schema_notes, &embedded_hints),
                None,
                !force_no_color,
            );
            if force_no_color {
                println!("{dump}");
            } else {
                println!("{}", style(dump).dim());
            }
        }
        println!(
            "{}:{} {} {} : OK",
            schema_path.display(),
            root_name,
            style("==").dim(),
            input_path,
        );
        print_validation_warnings(&validation_warnings);
        return true;
    }

    if fails {
        if detailed {
            print_validation_failure(
                schema_path,
                &input_path,
                &document,
                &schema_notes,
                &embedded_hints,
                &issues,
                &validation_warnings,
                force_no_color,
            );
        }
        println!(
            "{}:{} {} {} : OK",
            schema_path.display(),
            root_name,
            style("!=").dim(),
            input_path,
        );
        if !detailed {
            print_validation_warnings(&validation_warnings);
        }
        return true;
    }

    print_validation_failure(
        schema_path,
        &input_path,
        &document,
        &schema_notes,
        &embedded_hints,
        &issues,
        &validation_warnings,
        force_no_color,
    );
    false
}

/// Print validation mismatch diagnostics.
fn print_validation_failure(
    schema_path: &Path,
    input_path: &str,
    document: &Document,
    schema_notes: &[SchemaNote],
    embedded_hints: &[EmbeddedCborHint],
    issues: &[ValidationIssue],
    validation_warnings: &[ValidationWarning],
    force_no_color: bool,
) {
    println!(
        "{} {} -> {}",
        console::Emoji::new("🚨", "Errors"),
        schema_path.display(),
        input_path
    );
    let unique_issues = unique_validation_issues(issues);
    let highlight = unique_issues
        .first()
        .map(|issue| issue.path.as_slice())
        .unwrap_or(&[]);
    let dump = render_validation_dump(
        schema_path,
        input_path,
        document,
        &RenderContext::new(schema_notes, embedded_hints),
        Some(highlight),
        !force_no_color,
    );
    print!("{dump}");
    print_validation_warnings(validation_warnings);

    for issue in unique_issues {
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
}

/// Keep validation output concise by removing exact duplicate issues.
fn unique_validation_issues(issues: &[ValidationIssue]) -> Vec<&ValidationIssue> {
    let mut unique = Vec::new();
    for issue in issues {
        if unique.contains(&issue) {
            continue;
        }
        unique.push(issue);
    }
    unique
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PathStep {
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
    /// Top-level item inside an embedded CBOR payload (single-item or sequence).
    ///
    /// Distinct from `ArrayItem` and `TagInner` so the renderer can identify
    /// items inside a `<<...>>` wrapper without colliding with array indices
    /// or ordinary tag payloads at the same path level.
    EmbeddedItem(usize),
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

    if let Some(rhs) = root_cbor_sequence_controller(definitions, root_name) {
        let sequence = Value::Array(document.items().to_vec());
        let mut path = vec![PathStep::DocItem(0)];
        validate_schema_node(
            compiled,
            definitions,
            rhs,
            &sequence,
            &mut path,
            &mut issues,
        );
        rewrite_cbor_sequence_schema_notes(definitions, rhs);
        normalize_cbor_sequence_issues(&mut issues, document);
        return issues;
    }

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

/// Rewrite synthetic array notes into top-level CBOR sequence notes.
fn rewrite_cbor_sequence_schema_notes(
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
) {
    let root_note = SchemaNote {
        path: Vec::new(),
        text: format!(
            "CBOR sequence {}",
            cbor_sequence_controller_summary(definitions, rhs)
        ),
    };
    let labels = cbor_sequence_item_labels(definitions, rhs);

    CURRENT_SCHEMA_NOTES.with(|slot| {
        let original = std::mem::take(&mut *slot.borrow_mut());
        let mut rewritten = Vec::with_capacity(original.len().saturating_add(labels.len() + 1));
        rewritten.push(root_note);

        for (index, label) in labels.into_iter().enumerate() {
            rewritten.push(SchemaNote {
                path: vec![PathStep::DocItem(index)],
                text: label,
            });
        }

        for mut note in original {
            if note.path == [PathStep::DocItem(0)] {
                continue;
            }
            if let [PathStep::DocItem(0), PathStep::ArrayItem(index), rest @ ..] =
                note.path.as_slice()
            {
                let mut path = vec![PathStep::DocItem(*index)];
                path.extend_from_slice(rest);
                note.path = path;
            }
            if !rewritten.iter().any(|existing| existing.path == note.path) {
                rewritten.push(note);
            }
        }

        *slot.borrow_mut() = rewritten;
    });
}

/// Summarize a CBOR sequence controller without exposing synthetic array syntax.
fn cbor_sequence_controller_summary(
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
) -> String {
    cbor_sequence_controller_reference(rhs)
        .or_else(|| {
            let labels = cbor_sequence_item_labels(definitions, rhs);
            (!labels.is_empty()).then(|| labels.join(", "))
        })
        .unwrap_or_else(|| schema_summary(rhs))
}

/// Return the single named controller inside `[ name ]`, when present.
fn cbor_sequence_controller_reference(node: &WrappedNode) -> Option<String> {
    let WrappedNode::Syntax {
        rule,
        text,
        children,
        ..
    } = node
    else {
        return None;
    };
    if rule == "type2" && text.trim_start().starts_with('[') {
        let group = children
            .iter()
            .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))?;
        let grpchoice = group_children(group, "grpchoice").into_iter().next()?;
        let grpent_nodes = extract_grpent_nodes(grpchoice)?;
        if grpent_nodes.len() == 1 {
            return grpent_nodes
                .first()
                .and_then(|grpent| find_grpent_body(grpent))
                .and_then(named_reference);
        }
    }
    None
}

/// Collect display labels for top-level CBOR sequence items.
fn cbor_sequence_item_labels(
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
) -> Vec<String> {
    cbor_sequence_item_labels_in_namespace(definitions, node, None)
}

/// Collect display labels for top-level CBOR sequence items.
fn cbor_sequence_item_labels_in_namespace(
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    namespace: Option<&str>,
) -> Vec<String> {
    match node {
        WrappedNode::RuleLine { children, .. } => {
            find_rhs_node(children).map_or_else(Vec::new, |rhs| {
                cbor_sequence_item_labels_in_namespace(definitions, rhs, namespace)
            })
        },
        WrappedNode::Syntax {
            rule,
            children,
            text,
            ..
        } if rule == "type" => {
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type1"))
                .map_or_else(Vec::new, |child| {
                    cbor_sequence_item_labels_in_namespace(definitions, child, namespace)
                })
        },
        WrappedNode::Syntax { rule, children, .. } if rule == "type1" => {
            if let Some((lhs, op, _rhs)) = control_operator_parts(children)
                && op == ".within"
            {
                return cbor_sequence_item_labels_in_namespace(definitions, lhs, namespace);
            }
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type2"))
                .map_or_else(Vec::new, |child| {
                    cbor_sequence_item_labels_in_namespace(definitions, child, namespace)
                })
        },
        WrappedNode::Syntax {
            rule,
            children,
            text,
            ..
        } if rule == "type2" && text.trim_start().starts_with('[') => {
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
                .map_or_else(Vec::new, |group| {
                    cbor_sequence_item_labels_in_namespace(definitions, group, namespace)
                })
        },
        WrappedNode::Syntax { rule, text, .. } if rule == "type2" => {
            let name = text.trim();
            resolve_definition_in_namespace(definitions, name, namespace).map_or_else(
                Vec::new,
                |(resolved_name, node)| {
                    cbor_sequence_item_labels_in_namespace(
                        definitions,
                        node,
                        definition_namespace(&resolved_name),
                    )
                },
            )
        },
        WrappedNode::Syntax { rule, .. } if rule == "group" => {
            let Some(grpchoice) = group_children(node, "grpchoice").into_iter().next() else {
                return Vec::new();
            };
            collect_grpchoice_sequence_labels(definitions, grpchoice, namespace)
        },
        WrappedNode::Syntax { rule, .. } if rule == "grpent" => {
            find_grpent_body(node).map_or_else(Vec::new, |body| {
                cbor_sequence_item_labels_in_namespace(definitions, body, namespace)
            })
        },
        WrappedNode::Syntax { rule, text, .. } if rule == "typename" || rule == "groupname" => {
            let name = text.trim();
            resolve_definition_in_namespace(definitions, name, namespace).map_or_else(
                Vec::new,
                |(resolved_name, node)| {
                    cbor_sequence_item_labels_in_namespace(
                        definitions,
                        node,
                        definition_namespace(&resolved_name),
                    )
                },
            )
        },
        _ => {
            named_reference(node)
                .and_then(|name| resolve_definition_in_namespace(definitions, &name, namespace))
                .map_or_else(Vec::new, |(resolved_name, node)| {
                    cbor_sequence_item_labels_in_namespace(
                        definitions,
                        node,
                        definition_namespace(&resolved_name),
                    )
                })
        },
    }
}

/// Collect display labels from one group choice.
fn collect_grpchoice_sequence_labels(
    definitions: &HashMap<String, &WrappedNode>,
    grpchoice: &WrappedNode,
    namespace: Option<&str>,
) -> Vec<String> {
    let Some(grpent_nodes) = extract_grpent_nodes(grpchoice) else {
        return Vec::new();
    };
    let mut labels = Vec::new();
    for grpent in grpent_nodes {
        if let Some(memberkey) = find_memberkey(grpent) {
            labels.push(memberkey_summary(child_text(memberkey)));
            continue;
        }
        let Some(body) = find_grpent_body(grpent) else {
            continue;
        };
        let nested = cbor_sequence_item_labels_in_namespace(definitions, body, namespace);
        if nested.is_empty() {
            labels.push(schema_summary(body));
        } else {
            labels.extend(nested);
        }
    }
    labels
}

/// Resolve a CDDL rule name, preferring the current import namespace for bare references.
fn resolve_definition_in_namespace<'a>(
    definitions: &'a HashMap<String, &WrappedNode>,
    name: &str,
    namespace: Option<&str>,
) -> Option<(String, &'a WrappedNode)> {
    if let Some(namespace) = namespace
        && !name.contains('.')
    {
        let qualified = format!("{namespace}.{name}");
        if let Some(node) = definitions.get(qualified.as_str()) {
            return Some((qualified, *node));
        }
    }

    definitions.get(name).map(|node| (name.to_owned(), *node))
}

/// Return the namespace prefix for a qualified imported definition name.
fn definition_namespace(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(namespace, _rule)| namespace)
}

/// Return the named reference held by a syntax subtree, if any.
fn named_reference(node: &WrappedNode) -> Option<String> {
    node_children_find(node, "typename")
        .or_else(|| node_children_find(node, "groupname"))
        .and_then(|node| {
            match node {
                WrappedNode::Syntax { text, .. } => Some(text.trim().to_owned()),
                _ => None,
            }
        })
}

/// Split a `type1` control operator into `(lhs, op, rhs)`.
fn control_operator_parts(children: &[WrappedNode]) -> Option<(&WrappedNode, &str, &WrappedNode)> {
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

    Some((lhs?, op?, rhs?))
}

/// Rewrite diagnostics from the internal sequence-as-array representation.
fn normalize_cbor_sequence_issues(
    issues: &mut [ValidationIssue],
    document: &Document,
) {
    let sequence_found = cbor_sequence_found(document);
    for issue in issues {
        issue.expected = cbor_sequence_text(&issue.expected);
        issue.found = match issue.found.as_str() {
            found if found == format!("{}", Value::Array(document.items().to_vec())) => {
                sequence_found.clone()
            },
            "end of array" => "end of CBOR sequence".to_owned(),
            found => cbor_sequence_text(found),
        };
        if let Some(message) = &mut issue.message {
            *message = cbor_sequence_text(message);
        }
    }
}

/// Render a concise description of the top-level CBOR sequence.
fn cbor_sequence_found(document: &Document) -> String {
    match document.items().len() {
        0 => "empty CBOR sequence".to_owned(),
        1 => {
            document.items().first().map_or_else(
                || "empty CBOR sequence".to_owned(),
                |item| format!("1-item CBOR sequence: {item}"),
            )
        },
        len => format!("{len}-item CBOR sequence"),
    }
}

/// Convert internal array wording to user-facing CBOR sequence wording.
fn cbor_sequence_text(text: &str) -> String {
    text.replace(
        "one of the listed group alternatives",
        "one of the listed CBOR sequence group alternatives",
    )
    .replace(
        "spliced array group alternatives",
        "CBOR sequence group alternatives",
    )
    .replace("spliced array group", "CBOR sequence group")
    .replace("array item sequence", "CBOR sequence")
    .replace("array item(s)", "CBOR sequence item(s)")
    .replace("array items", "CBOR sequence items")
    .replace("array item", "CBOR sequence item")
    .replace(
        "an array matching the schema",
        "a CBOR sequence matching the schema",
    )
    .replace("no matching item sequence", "no matching CBOR sequence")
    .replace("array alternatives", "CBOR sequence alternatives")
    .replace("array group", "CBOR sequence group")
    .replace("end of array", "end of CBOR sequence")
    .replace("trailing array", "trailing CBOR sequence")
    .replace("empty array", "empty CBOR sequence")
}

/// Return the controller for a root-level `any .cborseq` or `any .dtrmseq`.
fn root_cbor_sequence_controller<'a>(
    definitions: &'a HashMap<String, &WrappedNode>,
    root_name: &str,
) -> Option<&'a WrappedNode> {
    let root = definitions.get(root_name)?;
    let rhs = rule_rhs_or_self(root)?;
    sequence_controller_from_node(rhs)
}

/// Find a sequence controller only when the LHS is the permissive `any` carrier.
fn sequence_controller_from_node(node: &WrappedNode) -> Option<&WrappedNode> {
    match node {
        WrappedNode::Syntax { rule, children, .. } if rule == "type" => {
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type1"))
                .and_then(sequence_controller_from_node)
        },
        WrappedNode::Syntax { rule, children, .. } if rule == "type1" => {
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

            if matches!(op, Some(".cborseq" | ".dtrmseq")) && lhs.is_some_and(is_any_type2) {
                rhs
            } else {
                None
            }
        },
        _ => None,
    }
}

/// Return true when a `type2` node is the builtin `any` carrier.
fn is_any_type2(node: &WrappedNode) -> bool {
    let WrappedNode::Syntax { rule, text, .. } = node else {
        return false;
    };
    rule == "type2" && text.trim() == "any"
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
                if let WrappedNode::Syntax { rule, .. } = inner {
                    if rule == "bareword" {
                        // A bareword member key (e.g. `foo` in
                        // `{ foo: uint }`) names a literal text-string
                        // key, not a schema. The match against the
                        // CBOR map's key already happened in
                        // `find_matching_map_entry`; do not re-validate
                        // the key value against a rule with this name.
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
        ".abnf" | ".abnfb" => {
            let _ = validate_abnf_rhs(compiled, rhs, value, path, issues);
        },
        ".x-enc.abnf" | ".x-enc.abnfb" | ".x-hash.abnf" | ".x-hash.abnfb" => {
            // The four transform annotations are documentation-only at
            // validation time. The validator does not have the encryption
            // keys, hash preimage, or algorithm context required to
            // reverse the transform, so the RHS ABNF cannot constrain
            // the carrier bytes here. The left-hand-side carrier and
            // ordinary constraints (bstr, .size, ...) have already
            // been validated by the preceding `validate_schema_node`
            // call on `lhs`; missing or type-mismatched carriers are
            // reported there. Record a non-fatal warning so users know
            // the RHS ABNF is not enforced.
            record_validation_warning_once(
                path,
                format!(
                    "transform operator `{op}` is documentation-only; the RHS ABNF is not applied to the carrier bytes"
                ),
            );
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
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::Cbor,
            );
        },
        ".cborseq" => {
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::CborSeq,
            );
        },
        ".prefp" => {
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::Prefp,
            );
        },
        ".prefpseq" => {
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::PrefpSeq,
            );
        },
        ".dtrm" => {
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::Dtrm,
            );
        },
        ".dtrmseq" => {
            let _ = validate_embedded_cbor_with_hint(
                compiled,
                definitions,
                rhs,
                value,
                path,
                issues,
                EmbeddedCborOperator::DtrmSeq,
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

/// Validate an embedded CBOR payload from a `bstr` and record a render hint.
///
/// This is the single entry point for the `.cbor`, `.cborseq`, `.prefp`,
/// `.prefpseq`, `.dtrm`, and `.dtrmseq` operators. On a successful byte-string
/// parse the `Document` is attached to the byte-string's path so the renderer
/// can show the decoded view inside the EDN-literals draft's `<<...>>`
/// wrapper. The hint is recorded even when the inner value fails the RHS
/// schema, so the user can still see what was decoded alongside the
/// validation error; raw `h'...'` fallback is reserved for parse failure,
/// wrong item cardinality, failed deterministic-encoding validation, and
/// resource-limit failures.
///
/// The serialization-draft checks use `cbork-utils`'s schema-independent
/// raw-byte checker (`check_serialization`):
/// * `.dtrm`/`.dtrmseq` — checks deterministic encoding plus preferred-plus (shortest
///   int/length, no indefinite, shortest float, map-key ordering).
/// * `.prefp`/`.prefpseq` — checks preferred-plus encoding without requiring
///   deterministic map-key ordering.
///
/// For `.dtrm` and `.dtrmseq`, the operator may also appear on a non-`bstr`
/// carrier (e.g. `any .dtrm T`). In that case the deterministic check still
/// runs against the surrounding source bytes, but no embedded-CBOR hint is
/// recorded because the value is not a byte string.
fn validate_embedded_cbor_with_hint(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
    operator: EmbeddedCborOperator,
) -> bool {
    let start_len = issues.len();

    if let Some(bytes) = value_to_bytes(value) {
        // Explicit bstr payload creates a new independent scope —
        // reset the serialization floor so nested controls are not
        // suppressed by an outer `.dtrm` or `.prefp` on the bstr
        // wrapper.
        let _floor_reset = SerializationFloorReset::new();
        // Parse as a sequence for the `.cborseq`/`.prefpseq`/`.dtrmseq`
        // operators so an empty payload is represented as a zero-item
        // document and can be rendered as `<<>>`. Single-item operators
        // continue to use the strict `Document::parse` to reject an empty
        // byte string as a missing required item.
        let document_result = if operator.allows_sequence() {
            Document::parse_sequence(bytes)
        } else {
            Document::parse(bytes)
        };

        let document = match document_result {
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

        // Serialization checks using the schema-independent raw-byte
        // checker from cbork-utils.  The monotonic serialization
        // floor tracks the effective constraint for direct `any`
        // composition — a weaker or equal operator skips the check.
        // Empty payloads for sequence operators skip the check.
        let is_empty_seq = operator.allows_sequence() && bytes.is_empty();
        let mode = if operator.requires_deterministic() {
            cbork_utils::serialization_checker::SerializationMode::Dtrm
        } else if operator.requires_preferred_plus() {
            cbork_utils::serialization_checker::SerializationMode::Prefp
        } else {
            cbork_utils::serialization_checker::SerializationMode::Cbor
        };
        let _floor_guard = if mode != cbork_utils::serialization_checker::SerializationMode::Cbor {
            let width = serialization_mode_width(mode);
            let skip =
                CURRENT_SERIALIZATION_FLOOR.with(|slot| slot.borrow().is_some_and(|f| f >= width));
            if skip {
                None
            } else {
                let guard = SerializationFloorGuard::push(mode);
                #[allow(clippy::collapsible_if)]
                if !is_empty_seq {
                    if let Err(err) = cbork_utils::serialization_checker::check_serialization(
                        bytes,
                        mode,
                        operator.allows_sequence(),
                    ) {
                        let label = if mode
                            == cbork_utils::serialization_checker::SerializationMode::Dtrm
                        {
                            "deterministic CBOR"
                        } else {
                            "preferred-plus CBOR"
                        };
                        let fatal =
                            mode == cbork_utils::serialization_checker::SerializationMode::Dtrm;
                        issues.push(ValidationIssue::new(
                            path.clone(),
                            label,
                            render_bytes(bytes),
                            Some(format!("{label} serialization check failed: {err}")),
                        ));
                        if fatal {
                            return false;
                        }
                    }
                }
                Some(guard)
            }
        } else if !is_empty_seq {
            if let Err(err) = cbork_utils::serialization_checker::check_serialization(
                bytes,
                mode,
                operator.allows_sequence(),
            ) {
                issues.push(ValidationIssue::new(
                    path.clone(),
                    "embedded CBOR",
                    render_bytes(bytes),
                    Some(format!("embedded CBOR serialization check failed: {err}")),
                ));
                return false;
            }
            None
        } else {
            None
        };

        if !operator.allows_sequence() && document.items().len() != 1 {
            issues.push(ValidationIssue::new(
                path.clone(),
                "a single embedded CBOR item",
                format!("{} top-level item(s)", document.items().len()),
                Some("embedded CBOR was not a single item".to_owned()),
            ));
            return false;
        }

        let previous_source = set_current_source_bytes(bytes);
        let previous_source_path = set_current_source_path(path);
        if operator.allows_sequence() {
            for (index, item) in document.items().iter().enumerate() {
                let mut child_path = path.clone();
                child_path.push(PathStep::EmbeddedItem(index));
                validate_schema_node(compiled, definitions, rhs, item, &mut child_path, issues);
            }
        } else {
            let Some(item) = document.items().first() else {
                restore_current_source_bytes(previous_source);
                restore_current_source_path(previous_source_path);
                return false;
            };
            let mut child_path = path.clone();
            child_path.push(PathStep::EmbeddedItem(0));
            validate_schema_node(compiled, definitions, rhs, item, &mut child_path, issues);
        }
        restore_current_source_bytes(previous_source);
        restore_current_source_path(previous_source_path);

        // Record the hint whenever embedded parsing succeeded, regardless
        // of RHS validation outcome. The renderer uses this to expose the
        // decoded view inside the `<<...>>` wrapper; validation errors are
        // already in the issue list.
        record_embedded_cbor_hint(path, operator, document);

        return issues.len() == start_len;
    }

    // Non-byte-string carriers. `.dtrm` / `.dtrmseq` and `.prefp` /
    // `.prefpseq` may also be applied directly (e.g. `any .dtrm T`,
    // `any .prefp T`); perform the serialization check against the
    // surrounding source bytes when required, then validate the value
    // against the RHS. No embedded-CBOR hint is recorded because the
    // value is not a byte string.
    if operator.requires_deterministic() || operator.requires_preferred_plus() {
        let source = current_source_bytes_view();
        let Some(relative_path) = source_relative_path(path) else {
            issues.push(ValidationIssue::new(
                path.clone(),
                "serialization check input",
                format!("{value}"),
                Some("could not locate the current item in the source bytes".to_owned()),
            ));
            return false;
        };
        let (start, end) =
            match cbork_utils::serialization_checker::item_span_at_path(&source, &relative_path) {
                Ok(span) => span,
                Err(error) => {
                    issues.push(ValidationIssue::new(
                        path.clone(),
                        "serialization check input",
                        format!("{value}"),
                        Some(format!("could not locate the current item: {error}")),
                    ));
                    return false;
                },
            };
        let Some(source) = source.get(start..end) else {
            issues.push(ValidationIssue::new(
                path.clone(),
                "serialization check input",
                format!("{value}"),
                Some("located item span was outside the source bytes".to_owned()),
            ));
            return false;
        };
        let mode = if operator.requires_deterministic() {
            cbork_utils::serialization_checker::SerializationMode::Dtrm
        } else {
            cbork_utils::serialization_checker::SerializationMode::Prefp
        };
        if let Err(err) = cbork_utils::serialization_checker::check_serialization(
            source,
            mode,
            operator.allows_sequence(),
        ) {
            let label = if operator.requires_deterministic() {
                "deterministic CBOR"
            } else {
                "preferred-plus CBOR"
            };
            issues.push(ValidationIssue::new(
                path.clone(),
                label,
                format!("{value}"),
                Some(format!("{label} serialization check failed: {err}")),
            ));
            return false;
        }
    }

    validate_schema_node(compiled, definitions, rhs, value, path, issues);
    issues.len() == start_len
}

/// Borrow the current deterministic-comparison source bytes without taking
/// ownership, used by `validate_embedded_cbor_with_hint` for non-byte-string
/// `.dtrm` / `.dtrmseq` carriers.
fn current_source_bytes_view() -> Vec<u8> {
    CURRENT_SOURCE_BYTES.with(|slot| slot.borrow().clone())
}

/// Set both the current source bytes and the path represented by their root.
fn set_current_source_context(
    bytes: &[u8],
    path: &[PathStep],
) {
    set_current_source_bytes(bytes);
    set_current_source_path(path);
}

/// Replace the current source-root path and return its previous value.
fn set_current_source_path(path: &[PathStep]) -> Vec<PathStep> {
    CURRENT_SOURCE_PATH.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), path.to_vec()))
}

/// Restore a previously saved source-root path.
fn restore_current_source_path(previous: Vec<PathStep>) {
    CURRENT_SOURCE_PATH.with(|slot| *slot.borrow_mut() = previous);
}

/// Convert a validation path into a path relative to the current source root.
fn source_relative_path(
    path: &[PathStep]
) -> Option<Vec<cbork_utils::serialization_checker::SerializationPathStep>> {
    let base = CURRENT_SOURCE_PATH.with(|slot| slot.borrow().clone());
    if path.get(..base.len())? != base.as_slice() {
        return None;
    }
    path.get(base.len()..)?
        .iter()
        .map(|step| {
            match step {
                PathStep::DocItem(index) | PathStep::EmbeddedItem(index) => {
                    Some(
                        cbork_utils::serialization_checker::SerializationPathStep::TopLevel(*index),
                    )
                },
                PathStep::ArrayItem(index) => {
                    Some(
                        cbork_utils::serialization_checker::SerializationPathStep::ArrayItem(
                            *index,
                        ),
                    )
                },
                PathStep::MapKey(index) => {
                    Some(cbork_utils::serialization_checker::SerializationPathStep::MapKey(*index))
                },
                PathStep::MapValue(index) => {
                    Some(
                        cbork_utils::serialization_checker::SerializationPathStep::MapValue(*index),
                    )
                },
                PathStep::TagInner => {
                    Some(cbork_utils::serialization_checker::SerializationPathStep::TagInner)
                },
            }
        })
        .collect()
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
///
/// `.abnf` and `.abnfb` share this dispatcher. The RHS is resolved to
/// the final ABNF source text (including composed `.det` expressions
/// and named text rules), parsed as ABNF, and the input data is matched
/// against the grammar using either `validate_text` (`.abnf`) or
/// `validate_bytes` (`.abnfb`).
///
/// On a successful `.abnfb` or `.abnf` validation that involves a byte
/// string or a non-empty text string, the trace API is also invoked so
/// the renderer can show the selected rule path and spans via a CDN
/// comment block. The trace is produced only on success and never
/// changes the boolean validation outcome. If the trace cannot be
/// produced (e.g. a memory limit), the validation result is still
/// considered successful and a non-fatal warning is recorded.
fn validate_abnf_rhs(
    compiled: &CompiledCDDL,
    rhs: &WrappedNode,
    value: &Value,
    path: &mut Vec<PathStep>,
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let pattern = resolve_text_rhs_for_abnf_with_det(compiled, rhs);
    let Some((pattern, used_det)) = pattern else {
        issues.push(ValidationIssue::new(
            path.clone(),
            "an ABNF pattern",
            format!("{value}"),
            Some("ABNF RHS did not resolve to text".to_owned()),
        ));
        return false;
    };

    // `.det` concatenates a human-readable label LHS with the RHS ABNF
    // source. The label is a bare identifier that the ABNF parser
    // would reject as a non-rule. Strip the LHS prefix only when the
    // source was produced by `.det`; direct text literals do not need
    // this preprocessing.
    let pattern = if used_det {
        strip_leading_label_line(&pattern)
    } else {
        pattern
    };

    // The ABNF parser requires every rule to be terminated by a
    // newline. Ensure the source ends with one.
    let pattern = if pattern.ends_with('\n') {
        pattern
    } else {
        format!("{pattern}\n")
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
            let result = document.match_text_with_trace(text);
            match result {
                Ok(trace) => {
                    record_abnf_match_trace(path, text.as_bytes(), trace);
                    true
                },
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
            match document.match_bytes_with_trace(bytes) {
                Ok(trace) => {
                    record_abnf_match_trace(path, bytes, trace);
                    true
                },
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

/// Store a captured `AbnfMatch` trace for the given path so the
/// detailed dump renderer can emit a CDN comment breakdown below the
/// value. The trace is stored as bytes (always, even for text inputs)
/// so the renderer can format binary spans with `h'...'` literals.
fn record_abnf_match_trace(
    path: &[PathStep],
    input: &[u8],
    trace: AbnfMatch,
) {
    if let Err(error) = render_abnf_breakdown::record_trace(path, input, trace) {
        record_validation_warning_once(
            path,
            format!("ABNF match trace could not be recorded: {error}"),
        );
    }
}

/// Strip a leading label produced by `.det` concatenation from an
/// ABNF source.
///
/// CDDL schemas commonly write `.abnfb` (or `.abnf`) RHSs as
/// `"label" .det <abnf-rule>` to document the start rule name. After
/// `.det` the LHS label is either prepended on its own line
/// (`label\nlabel = ...`) or directly concatenated
/// (`label<label> = ...`) depending on the literal content. The ABNF
/// parser expects every rule to be defined with `=` or `=/`, so a
/// bare identifier prefix would be rejected. This helper removes the
/// leading label in either form, ensures a trailing newline is present,
/// and leaves the remainder of the text otherwise unchanged.
fn strip_leading_label_line(text: &str) -> String {
    // First try the own-line form: the first line is a bare
    // identifier, followed by `\n`. We split on the first newline to
    // keep the slicing UTF-8 safe.
    let mut stripped = text.to_owned();
    if let Some((first_line, rest)) = text.split_once('\n')
        && is_bare_abnf_identifier(first_line)
    {
        rest.clone_into(&mut stripped);
    }

    // Then try the concatenated form: the text starts with a bare
    // identifier immediately followed by additional content. Strip
    // the longest bare-identifier prefix that is followed by
    // whitespace.
    if stripped == text {
        let prefix_len = stripped
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .count();
        if prefix_len > 0 {
            let after = stripped.chars().nth(prefix_len);
            if matches!(after, Some(' ' | '\t' | '\n' | '\r')) {
                // The leading identifier is pure ASCII so every
                // character is one byte; split_at returns the suffix
                // on a valid UTF-8 boundary.
                let (_, rest) = stripped.split_at(prefix_len);
                stripped = rest.trim_start().to_owned();
            }
        }
    }

    // The ABNF parser expects every rule to be terminated by a
    // newline. Ensure the stripped source ends with one.
    if !stripped.ends_with('\n') {
        stripped.push('\n');
    }
    stripped
}

/// Return true when `line` is a non-empty bare ABNF identifier with no
/// `=`, `=/`, `=`, `/`, `:`, `;`, `<`, `>`, `[`, `]`, `(`, `)`, or
/// whitespace tokens.
fn is_bare_abnf_identifier(line: &str) -> bool {
    !line.is_empty()
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Resolve the full text payload of an `.abnf` or `.abnfb` RHS.
///
/// Unlike [`resolve_text_rhs`], this evaluates the same text expressions
/// accepted by the CDDL compiler: direct text literals, named text
/// constants and rules, parenthesized text expressions, and the
/// `.det` text-processing operator. Unsupported forms return `None` so
/// the caller can emit a precise `ABNF RHS did not resolve to text`
/// diagnostic.
#[allow(dead_code)]
fn resolve_text_rhs_for_abnf(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<String> {
    resolve_text_rhs_for_abnf_with_det(compiled, node).map(|(text, _)| text)
}

/// Same as [`resolve_text_rhs_for_abnf`] but also returns whether the
/// `.det` operator was applied. The caller uses this flag to decide
/// whether the leading LHS label must be stripped before parsing.
fn resolve_text_rhs_for_abnf_with_det(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<(String, bool)> {
    // Direct text leaves: literal strings, named text constants, and
    // named text rules. The compiler's resolved-state map already
    // produces the final bytes for those cases.
    if let Some(text) = resolve_text_rhs(compiled, node) {
        return Some((text, false));
    }
    if let Some(text) = parse_text_from_node(node) {
        return Some((text, false));
    }

    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };

    // For `type2` nodes that contain a text-literal `value` child,
    // resolve the value directly. `resolve_type2_leaf` only handles
    // `type2` nodes with `typename` children, not literal values.
    if rule == "type2" {
        for child in children {
            if let WrappedNode::Syntax {
                rule: child_rule, ..
            } = child
                && child_rule == "value"
                && let Some((text, _)) = resolve_text_rhs_for_abnf_with_det(compiled, child)
            {
                return Some((text, false));
            }
        }
    }

    match rule.as_str() {
        // `text .det text` is the only composed text control the ABNF
        // validator currently relies on. Walk the children manually
        // because the standard `control_operator_parts` helper only
        // recognizes `type2` operands, while the LHS of `.det` is a
        // text-literal `value` node.
        "type1" => {
            // A `type1` with no `ctlop` child is just a single text
            // expression wrapped in a `type1`. Unwrap to the child.
            let has_ctlop = children.iter().any(|child| {
                matches!(
                    child,
                    WrappedNode::Syntax { rule, .. } if rule == "ctlop"
                )
            });
            if !has_ctlop {
                return children
                    .iter()
                    .find_map(|child| resolve_text_rhs_for_abnf_with_det(compiled, child));
            }
            let mut lhs: Option<&WrappedNode> = None;
            let mut op: Option<&str> = None;
            let mut rhs: Option<&WrappedNode> = None;
            for child in children {
                if let WrappedNode::Syntax {
                    rule: child_rule, ..
                } = child
                {
                    match child_rule.as_str() {
                        "value" if lhs.is_none() => lhs = Some(child),
                        "type2" if lhs.is_none() => lhs = Some(child),
                        "ctlop" => op = Some(child_text(child).trim()),
                        "value" | "type2" => rhs = Some(child),
                        _ => {},
                    }
                }
            }
            let (lhs, op, rhs) = (lhs?, op?, rhs?);
            if op != ".det" {
                return None;
            }
            let lhs_text = resolve_text_rhs_for_abnf_with_det(compiled, lhs)?.0;
            let rhs_text = resolve_text_rhs_for_abnf_with_det(compiled, rhs)?.0;
            let lhs_literal = TextLiteralBytes::from_bytes(lhs_text.into_bytes());
            let rhs_literal = TextLiteralBytes::from_bytes(rhs_text.into_bytes());
            let combined = lhs_literal.det(&rhs_literal);
            String::from_utf8(combined.into_bytes())
                .ok()
                .map(|s| (s, true))
        },
        // `( text .det text )` and `text` (a single-alternative text
        // expression) both flatten to their single child once the CDDL
        // parser has resolved the parentheses.
        "type" | "type2" => {
            children
                .iter()
                .find_map(|child| resolve_text_rhs_for_abnf_with_det(compiled, child))
        },
        _ => None,
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

/// Render-context carried through the schema-aware renderer.
///
/// Bundles the captured `SchemaNote`s with the `EmbeddedCborHint`s recorded
/// during validation. Schema notes already include labels recorded at every
/// inner path of an embedded payload, so the renderer does not need to look
/// up definitions or re-resolve the compiled schema at render time.
struct RenderContext<'a> {
    /// Captured schema annotations.
    notes: &'a [SchemaNote],
    /// Captured embedded-CBOR render hints.
    hints: &'a [EmbeddedCborHint],
}

impl<'a> RenderContext<'a> {
    /// Build a render context for the given validation run.
    fn new(
        notes: &'a [SchemaNote],
        hints: &'a [EmbeddedCborHint],
    ) -> Self {
        Self { notes, hints }
    }
}

/// Render a validation dump with optional highlighting.
fn render_validation_dump(
    schema_path: &Path,
    input_path: &str,
    document: &Document,
    ctx: &RenderContext<'_>,
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

    let sequence_note = cbor_sequence_note(ctx.notes);
    if let Some(note) = sequence_note {
        render_annotation_line(&mut output, note, color);
        push_dim(&mut output, "\n", color);
    }

    reset_render_counters();
    for (index, item) in document.items().iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let path = [PathStep::DocItem(index)];
        let indent = if sequence_note.is_some() { 2 } else { 0 };
        push_indent(&mut output, indent);
        render_value_with_highlight(item, &mut output, color, indent, &path, highlight, ctx);
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
    ctx: &RenderContext<'_>,
) {
    // Embedded-CBOR byte strings are rendered as `<<decoded view>>` instead
    // of raw `h'...'` whenever a hint is available for this path.
    if matches!(value, Value::Bytes(_))
        && let Some(hint) = embedded_cbor_hint_for_path(ctx.hints, path)
    {
        render_embedded_cbor_hint(hint, output, color, indent, path, highlight, ctx);
        return;
    }

    let node_highlight = is_highlighted(path, highlight);
    if let Some(note) = schema_note_for_path(ctx.notes, path) {
        if is_cbor_sequence_dump(ctx.notes) && matches!(path, [PathStep::DocItem(_)]) {
            render_sequence_field_annotation(output, &note, color, node_highlight);
        } else {
            render_annotation(output, &note, color, node_highlight);
        }
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
            crate::render_abnf_breakdown::append_breakdown_comments(path, output, indent, color);
        },
        Value::Text(value) => {
            render_token(
                output,
                format!("{value:?}"),
                ColorKind::Text,
                color,
                node_highlight,
            );
            crate::render_abnf_breakdown::append_breakdown_comments(path, output, indent, color);
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
                    ctx,
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
                    ctx,
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
                    ctx,
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
                    ctx,
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
    ctx: &RenderContext<'_>,
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
    render_value_with_highlight(&entry.key, output, color, indent, &key_path, highlight, ctx);
    render_punct(output, ": ", color, is_highlighted(path, highlight));
    render_value_with_highlight(
        &entry.value,
        output,
        color,
        indent,
        &value_path,
        highlight,
        ctx,
    );
}

/// Render an embedded-CBOR byte string's decoded view inside the
/// EDN-literals draft's `<<...>>` wrapper.
///
/// `indent` is the column where the wrapper delimiters should land. Each
/// embedded item is rendered into a fresh buffer using the same CDN
/// renderer with the appropriate child path, then inserted into the wrapper
/// at the calculated indentation. Nested embedded-CBOR byte strings inside
/// the decoded view are handled automatically by passing the same hints
/// through `render_value_with_highlight`.
fn render_embedded_cbor_hint(
    hint: &EmbeddedCborHint,
    output: &mut String,
    color: bool,
    indent: usize,
    path: &[PathStep],
    highlight: Option<&[PathStep]>,
    ctx: &RenderContext<'_>,
) {
    let depth = indent / 2;
    let node_highlight = is_highlighted(path, highlight);
    if let Some(note) = schema_note_for_path(ctx.notes, path) {
        if is_cbor_sequence_dump(ctx.notes) && matches!(path, [PathStep::DocItem(_)]) {
            render_sequence_field_annotation(output, &note, color, node_highlight);
        } else {
            render_annotation(output, &note, color, node_highlight);
        }
    }

    let items: &[Value] = hint.document.items();
    let inner_indent = indent.saturating_add(2);

    push_bracket(output, "<<", color, depth);
    if items.is_empty() {
        push_bracket(output, ">>", color, depth);
        return;
    }

    let is_single = !hint.operator.allows_sequence();
    let limits = EmbedLimits::default();
    let bytes_len = hint
        .document
        .to_preferred_plus_bytes()
        .map(|b| b.len())
        .unwrap_or(0);
    let expanded = try_charge_embed(bytes_len, limits);
    if expanded == EmbedBudget::LimitReached {
        // Resource limit reached; emit raw `h'...'` form per the
        // EDN-literals draft's byte-string diagnostic notation and stop.
        let raw = render_bytes_fallback_for_hint(hint);
        push_colored(output, raw, ColorKind::Bytes, color);
        return;
    }

    push_dim(output, "\n", color);

    if is_single {
        let Some(item) = items.first() else {
            release_embed_depth();
            push_indent(output, indent);
            push_bracket(output, ">>", color, depth);
            return;
        };
        let mut child_path = path.to_vec();
        child_path.push(PathStep::EmbeddedItem(0));
        push_indent(output, inner_indent);
        render_value_with_highlight(
            item,
            output,
            color,
            inner_indent,
            &child_path,
            highlight,
            ctx,
        );
        push_dim(output, "\n", color);
    } else {
        let last = items.len().saturating_sub(1);
        reset_sequence_counter();
        for (index, item) in items.iter().enumerate() {
            if !try_charge_sequence_item(limits) {
                // Sequence too long; emit a trailing diagnostic rather
                // than silently truncating.
                push_indent(output, inner_indent);
                push_colored(
                    output,
                    format!("... ({} more item(s) truncated)", items.len() - index),
                    ColorKind::Simple,
                    color,
                );
                push_dim(output, "\n", color);
                release_embed_depth();
                push_indent(output, indent);
                push_bracket(output, ">>", color, depth);
                return;
            }
            push_indent(output, inner_indent);
            let mut child_path = path.to_vec();
            child_path.push(PathStep::EmbeddedItem(index));
            render_value_with_highlight(
                item,
                output,
                color,
                inner_indent,
                &child_path,
                highlight,
                ctx,
            );
            if index != last {
                render_punct(output, ",", color, false);
            }
            push_dim(output, "\n", color);
        }
    }

    release_embed_depth();
    push_indent(output, indent);
    push_bracket(output, ">>", color, depth);
}

/// Render the original bytes for a hint as `h'...'` for use when a
/// resource limit is reached.
fn render_bytes_fallback_for_hint(hint: &EmbeddedCborHint) -> String {
    let bytes = hint.document.to_preferred_plus_bytes().unwrap_or_default();
    render_bytes(&bytes)
}

/// Prefix every non-empty continuation line in `text` with `indent` spaces.
///
/// The first line is left alone because the caller has already positioned
/// it; only subsequent lines need shifting. Empty lines are emitted without
/// trailing whitespace. The helper preserves the EDN-literals draft's
/// comma-separated formatting inside the `<<...>>` wrapper.
#[cfg(test)]
#[allow(dead_code)]
fn indent_multiline(
    text: &str,
    indent: usize,
) -> String {
    let mut out = String::with_capacity(text.len() + indent * 8);
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
            if !line.is_empty() {
                for _ in 0..indent {
                    out.push(' ');
                }
            }
        }
        out.push_str(line);
        first = false;
    }
    out
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

/// Render a standalone schema annotation line.
fn render_annotation_line(
    output: &mut String,
    text: &str,
    color: bool,
) {
    if color {
        let _ = write!(output, "{}", style(format!("/{text}/")).dim());
    } else {
        let _ = write!(output, "/{text}/");
    }
}

/// Render a CBOR sequence item label.
fn render_sequence_field_annotation(
    output: &mut String,
    text: &str,
    color: bool,
    highlight: bool,
) {
    let label = format!("{text}: ");
    if color && highlight {
        let _ = write!(output, "{}", style(label).red().bold());
    } else {
        push_dim(output, &label, color);
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

/// Truncate recorded schema notes and embedded-CBOR hints after a failed
/// speculative branch. Both streams grow in lockstep with validation, so a
/// single rollback point covers both.
fn truncate_current_schema_notes(len: usize) {
    CURRENT_SCHEMA_NOTES.with(|slot| slot.borrow_mut().truncate(len));
    CURRENT_EMBEDDED_CBOR_HINTS.with(|slot| slot.borrow_mut().truncate(len));
}

/// Record an embedded-CBOR render hint at `path` if no hint is already present
/// at that path.
///
/// The hint carries the parsed `Document` so the renderer can produce the
/// decoded view without re-parsing. The original byte string remains the
/// authoritative value at the path; the hint only augments rendering.
fn record_embedded_cbor_hint(
    path: &[PathStep],
    operator: EmbeddedCborOperator,
    document: Document,
) {
    CURRENT_EMBEDDED_CBOR_HINTS.with(|slot| {
        let mut hints = slot.borrow_mut();
        if hints.iter().any(|hint| hint.path == path) {
            return;
        }
        hints.push(EmbeddedCborHint {
            path: path.to_vec(),
            operator,
            document,
        });
    });
}

/// Take all recorded embedded-CBOR hints.
fn take_current_embedded_cbor_hints() -> Vec<EmbeddedCborHint> {
    CURRENT_EMBEDDED_CBOR_HINTS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

/// Clear recorded embedded-CBOR hints.
fn clear_current_embedded_cbor_hints() {
    CURRENT_EMBEDDED_CBOR_HINTS.with(|slot| slot.borrow_mut().clear());
}

/// Return the embedded-CBOR hint recorded for `path`, if any.
fn embedded_cbor_hint_for_path<'a>(
    hints: &'a [EmbeddedCborHint],
    path: &[PathStep],
) -> Option<&'a EmbeddedCborHint> {
    hints.iter().find(|hint| hint.path == path)
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

/// Return the root annotation for a CBOR sequence dump.
fn cbor_sequence_note(notes: &[SchemaNote]) -> Option<&str> {
    notes
        .iter()
        .find(|note| note.path.is_empty() && note.text.starts_with("CBOR sequence "))
        .map(|note| note.text.as_str())
}

/// Return true when rendering a top-level raw CBOR sequence.
fn is_cbor_sequence_dump(notes: &[SchemaNote]) -> bool {
    cbor_sequence_note(notes).is_some()
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
            PathStep::EmbeddedItem(index) => {
                let _ = write!(out, ".embedded[{index}]");
            },
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
    /// The selected rule denotes a CDDL group (or group entry), which is
    /// indefinite and cannot be validated against a concrete CBOR item.
    /// `explicit` is `true` when the user picked the rule via `--type`,
    /// and `false` when it was resolved as the natural root.
    IndefiniteRoot {
        /// The base name of the selected rule.
        name: String,
        /// Whether the rule was explicitly selected via `--type`.
        explicit: bool,
    },
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
fn resolve_validation_root<'a>(
    compiled: &'a CompiledCDDL,
    schema_path: &Path,
    type_name: Option<&str>,
) -> Result<(String, &'a WrappedNode), RootSelectionError> {
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

    let explicit = type_name.is_some();
    let check_indefinite =
        |name: String, node: &'a WrappedNode| check_indefinite_root(name, node, explicit);

    match type_name {
        None => {
            let (name, node) = primary_matches
                .iter()
                .find_map(|node| top_level_rule_signature(node).map(|(name, _)| (name, *node)))
                .ok_or(RootSelectionError::NoRootRule)?;
            check_indefinite(name, node)
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

            if primary_concrete.is_empty() {
                return Err(lookup_root_selection_error(
                    compiled,
                    canonical_schema.as_deref(),
                    requested,
                ));
            }

            // Pick the first concrete same-name rule. When more than
            // one exists in the primary file we defer to the compiler's
            // existing diagnostics rather than introducing bespoke
            // ambiguity handling; the compiler will already have raised
            // a diagnostic for the duplicate.
            if let Some((node, _rest)) = primary_concrete.split_first() {
                check_indefinite(requested.to_owned(), node)
            } else {
                Err(lookup_root_selection_error(
                    compiled,
                    canonical_schema.as_deref(),
                    requested,
                ))
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
        RootSelectionError::IndefiniteRoot { name, .. } => {
            format!(
                "validation error: \"{name}\" names a CDDL group; \
                 groups are indefinite and cannot be selected as the validation root"
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

/// Wrap a successfully-resolved root, rejecting it as an indefinite
/// CDDL group when its top-level RHS is `group` or `grpent`.
fn check_indefinite_root<'a>(
    name: String,
    node: &'a WrappedNode,
    explicit: bool,
) -> Result<(String, &'a WrappedNode), RootSelectionError> {
    if selected_root_is_indefinite_group(node) {
        Err(RootSelectionError::IndefiniteRoot { name, explicit })
    } else {
        Ok((name, node))
    }
}

/// Return `true` if the top-level RHS of `node` is a CDDL group or
/// group-entry rather than a concrete CBOR data item.
///
/// A rule whose RHS is a `group` (`foo = ( ... )`) or `grpent`
/// (`foo = ( key: value )`) does not denote a single CBOR item and so
/// cannot be selected as the validation root. The check inspects only
/// the selected rule's top-level RHS shape; nested groups inside
/// arrays, maps, choices, or member entries are valid CDDL and are
/// not rejected here.
fn selected_root_is_indefinite_group(node: &WrappedNode) -> bool {
    let children: &[WrappedNode] = match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } => children,
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {
            return false;
        },
    };
    let Some(rhs) = find_rhs_node(children) else {
        return false;
    };
    matches!(
        rhs,
        WrappedNode::Syntax { rule, .. } if matches!(rule.as_str(), "group" | "grpent")
    )
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

/// Array occurrence constraint for a normalized group element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayOccurrence {
    /// Exactly one occurrence.
    One,
    /// Zero or one occurrence.
    Optional,
    /// A bounded or unbounded numeric repetition range.
    Range {
        /// Minimum number of accepted items.
        min: usize,
        /// Maximum number of accepted items, or no upper bound.
        max: Option<usize>,
    },
}

/// A group element after pairing split numeric occurrences with their payload.
struct ArrayGroupElement<'a> {
    /// Element payload schema.
    body: &'a WrappedNode,
    /// Element occurrence constraint.
    occurrence: ArrayOccurrence,
}

/// Normalize array group entries, pairing split numeric occurrences with their payload.
fn extract_array_group_elements<'a>(
    compiled: &CompiledCDDL,
    grpchoice: &'a WrappedNode,
) -> Result<Vec<ArrayGroupElement<'a>>, ()> {
    let grpent_nodes = extract_grpent_nodes(grpchoice).ok_or(())?;
    let mut elements = Vec::new();
    let mut index = 0usize;

    while let Some(grpent) = grpent_nodes.get(index).copied() {
        if let Some((element, consumed)) =
            extract_array_group_element_with_split_lower_bound(compiled, &grpent_nodes, index)?
        {
            elements.push(element);
            index = index.saturating_add(consumed);
            continue;
        }

        let Some((occurrence, max_from_body)) = array_occurrence(compiled, grpent)? else {
            let body = find_grpent_body(grpent).ok_or(())?;
            elements.push(ArrayGroupElement {
                body,
                occurrence: ArrayOccurrence::One,
            });
            index = index.saturating_add(1);
            continue;
        };

        let (body, consumed_next) = if max_from_body || find_grpent_body(grpent).is_none() {
            let next = grpent_nodes
                .get(index.saturating_add(1))
                .and_then(|next| find_grpent_body(next))
                .ok_or(())?;
            (next, true)
        } else {
            (find_grpent_body(grpent).ok_or(())?, false)
        };

        elements.push(ArrayGroupElement { body, occurrence });
        index = index.saturating_add(if consumed_next { 2 } else { 1 });
    }

    Ok(elements)
}

/// Pair a split named/integer lower bound with the following `*...` occurrence.
fn extract_array_group_element_with_split_lower_bound<'a>(
    compiled: &CompiledCDDL,
    grpent_nodes: &[&'a WrappedNode],
    index: usize,
) -> Result<Option<(ArrayGroupElement<'a>, usize)>, ()> {
    let Some(lower_body) = grpent_nodes.get(index).and_then(|grpent| {
        if find_occurrence(grpent).is_some() {
            return None;
        }
        find_grpent_body(grpent)
    }) else {
        return Ok(None);
    };
    let Some(lower) = resolve_integer_bound(compiled, lower_body)
        .map(usize_from_nonnegative)
        .transpose()?
    else {
        return Ok(None);
    };
    let Some(next) = grpent_nodes.get(index.saturating_add(1)).copied() else {
        return Ok(None);
    };
    let Some(next_occur) = find_occurrence(next) else {
        return Ok(None);
    };
    if !child_text(next_occur).trim_start().starts_with('*') {
        return Ok(None);
    }
    let Some((ArrayOccurrence::Range { max, .. }, max_from_body)) =
        array_occurrence(compiled, next)?
    else {
        return Ok(None);
    };

    let (body, consumed) = if max_from_body {
        let body = grpent_nodes
            .get(index.saturating_add(2))
            .and_then(|grpent| find_grpent_body(grpent))
            .ok_or(())?;
        (body, 3)
    } else {
        (find_grpent_body(next).ok_or(())?, 2)
    };

    Ok(Some((
        ArrayGroupElement {
            body,
            occurrence: ArrayOccurrence::Range { min: lower, max },
        },
        consumed,
    )))
}

/// Extract the array occurrence attached to a group entry.
fn array_occurrence(
    compiled: &CompiledCDDL,
    grpent: &WrappedNode,
) -> Result<Option<(ArrayOccurrence, bool)>, ()> {
    let Some(occur) = find_occurrence(grpent) else {
        return Ok(None);
    };
    let occur_text = child_text(occur);
    let occur_trimmed = occur_text.trim();
    let uints = occurrence_uints(occur)?;

    if occur_trimmed == "?" {
        return Ok(Some((ArrayOccurrence::Optional, false)));
    }
    if occur_trimmed == "+" {
        return Ok(Some((ArrayOccurrence::Range { min: 1, max: None }, false)));
    }
    if occur_trimmed == "*" {
        return Ok(Some((ArrayOccurrence::Range { min: 0, max: None }, false)));
    }

    if !occur_trimmed.contains('*') {
        let Some(count) = uints.first().copied() else {
            return Ok(None);
        };
        return Ok(Some((
            ArrayOccurrence::Range {
                min: count,
                max: Some(count),
            },
            false,
        )));
    }

    if occur_trimmed.starts_with('*') {
        let max = if let Some(max) = uints.first().copied() {
            Some(max)
        } else {
            find_grpent_body(grpent)
                .and_then(|body| resolve_integer_bound(compiled, body))
                .map(usize_from_nonnegative)
                .transpose()?
        };
        return Ok(Some((
            ArrayOccurrence::Range { min: 0, max },
            max.is_some() && uints.is_empty(),
        )));
    }

    let min = uints.first().copied().unwrap_or(0);
    if let Some(max) = uints.get(1).copied() {
        return Ok(Some((
            ArrayOccurrence::Range {
                min,
                max: Some(max),
            },
            false,
        )));
    }

    let max_from_body = find_grpent_body(grpent)
        .and_then(|body| resolve_integer_bound(compiled, body))
        .map(usize_from_nonnegative)
        .transpose()?;

    Ok(Some((
        ArrayOccurrence::Range {
            min,
            max: max_from_body,
        },
        max_from_body.is_some(),
    )))
}

/// Resolve an integer bound from a syntax node that may wrap a `type2` leaf.
fn resolve_integer_bound(
    compiled: &CompiledCDDL,
    node: &WrappedNode,
) -> Option<i128> {
    resolve_integer_rhs(compiled, node)
        .or_else(|| parse_integer_from_node(node))
        .or_else(|| {
            node_children_find(node, "type2").and_then(|leaf| resolve_integer_rhs(compiled, leaf))
        })
        .or_else(|| node_children_find(node, "type2").and_then(parse_integer_from_node))
}

/// Find the occurrence node attached to a group entry.
fn find_occurrence(node: &WrappedNode) -> Option<&WrappedNode> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "grpent" {
        return None;
    }

    children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "occur"))
}

/// Extract numeric bounds from an occurrence node.
fn occurrence_uints(occur: &WrappedNode) -> Result<Vec<usize>, ()> {
    let WrappedNode::Syntax { children, .. } = occur else {
        return Ok(Vec::new());
    };

    children
        .iter()
        .filter_map(|child| {
            let WrappedNode::Syntax { rule, text, .. } = child else {
                return None;
            };
            (rule == "uint").then(|| {
                text.trim()
                    .parse::<i128>()
                    .ok()
                    .and_then(|value| usize_from_nonnegative(value).ok())
                    .ok_or(())
            })
        })
        .collect()
}

/// Convert a non-negative CDDL integer bound to `usize`.
fn usize_from_nonnegative(value: i128) -> Result<usize, ()> {
    if value < 0 {
        return Err(());
    }
    usize::try_from(value).map_err(|_| ())
}

/// Validate a repeated array body and update the consumed item index.
fn validate_array_repetition(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    body: &WrappedNode,
    min: usize,
    max: Option<usize>,
    items: &[Value],
    item_index: &mut usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
    element_index: usize,
) -> Option<()> {
    let mut count = 0usize;

    while count < min {
        if items.get(*item_index).is_none() {
            issues.push(ValidationIssue::new(
                path.to_owned(),
                format!("at least {min} array item(s)"),
                "end of array",
                Some(format!(
                    "group element {element_index} required at least {min} item(s)"
                )),
            ));
            return None;
        }
        let consumed = validate_array_element(
            compiled,
            definitions,
            body,
            items,
            *item_index,
            path,
            issues,
        )?;
        *item_index = item_index.saturating_add(consumed);
        count = count.saturating_add(1);
    }

    while max.is_none_or(|max| count < max) {
        if items.get(*item_index).is_none() {
            break;
        }
        let before = issues.len();
        let warning_len = validation_warning_count();
        if let Some(consumed) = validate_array_element(
            compiled,
            definitions,
            body,
            items,
            *item_index,
            path,
            issues,
        ) {
            *item_index = item_index.saturating_add(consumed);
            count = count.saturating_add(1);
        } else {
            issues.truncate(before);
            truncate_validation_warnings(warning_len);
            break;
        }
    }

    Some(())
}

/// Validate one array element, or a group splice that consumes multiple items.
fn validate_array_element(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    body: &WrappedNode,
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Option<usize> {
    match validate_array_group_splice(compiled, definitions, body, items, item_index, path, issues)
    {
        Ok(Some(consumed)) => return Some(consumed),
        Ok(None) => {},
        Err(()) => return None,
    }

    let item = items.get(item_index)?;
    let before = issues.len();
    let mut child_path = path.to_owned();
    child_path.push(PathStep::ArrayItem(item_index));
    record_schema_note_once(&child_path, schema_summary(body));
    validate_schema_node(compiled, definitions, body, item, &mut child_path, issues);
    (issues.len() == before).then_some(1)
}

/// Validate a group splice from the current array item index.
fn validate_array_group_splice(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    node: &WrappedNode,
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Result<Option<usize>, ()> {
    let WrappedNode::Syntax {
        rule,
        children,
        text,
        ..
    } = node
    else {
        return Ok(None);
    };

    match rule.as_str() {
        "type" => {
            validate_array_group_splice_type(
                compiled,
                definitions,
                children,
                items,
                item_index,
                path,
                issues,
            )
        },
        "type1" => {
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type2"))
                .map_or(Ok(None), |child| {
                    validate_array_group_splice(
                        compiled,
                        definitions,
                        child,
                        items,
                        item_index,
                        path,
                        issues,
                    )
                })
        },
        "group" => {
            validate_array_group_splice_group(
                compiled,
                definitions,
                node,
                items,
                item_index,
                path,
                issues,
            )
        },
        "grpent" => {
            children
                .iter()
                .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
                .or_else(|| find_grpent_body(node))
                .map_or(Ok(None), |child| {
                    validate_array_group_splice(
                        compiled,
                        definitions,
                        child,
                        items,
                        item_index,
                        path,
                        issues,
                    )
                })
        },
        "type2" if text.trim_start().starts_with('{') => {
            if matches!(items.get(item_index), Some(Value::Map(_))) {
                return Ok(None);
            }
            validate_array_group_splice_type2(
                compiled,
                definitions,
                children,
                items,
                item_index,
                path,
                issues,
            )
        },
        "type2" => {
            children
                .iter()
                .find_map(|child| {
                    match child {
                        WrappedNode::Syntax { rule, text, .. }
                            if rule == "typename" || rule == "groupname" =>
                        {
                            Some(text.trim())
                        },
                        _ => None,
                    }
                })
                .map_or(Ok(None), |name| {
                    validate_named_array_group_splice(
                        compiled,
                        definitions,
                        name,
                        items,
                        item_index,
                        path,
                        issues,
                    )
                })
        },
        "typename" | "groupname" => {
            validate_named_array_group_splice(
                compiled,
                definitions,
                text.trim(),
                items,
                item_index,
                path,
                issues,
            )
        },
        _ => Ok(None),
    }
}

/// Validate a named local rule as an array group splice when it expands to one.
fn validate_named_array_group_splice(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    name: &str,
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Result<Option<usize>, ()> {
    definitions
        .get(name)
        .and_then(|node| rule_rhs_or_self(node))
        .map_or(Ok(None), |node| {
            validate_array_group_splice(
                compiled,
                definitions,
                node,
                items,
                item_index,
                path,
                issues,
            )
        })
}

/// Get a rule RHS when available, otherwise return the node itself.
fn rule_rhs_or_self(node: &WrappedNode) -> Option<&WrappedNode> {
    match node {
        WrappedNode::RuleLine { children, .. } => find_rhs_node(children),
        WrappedNode::Syntax { .. } => Some(node),
        _ => None,
    }
}

/// Validate a spliced group type choice.
fn validate_array_group_splice_type(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Result<Option<usize>, ()> {
    let mut branch_issues = Vec::new();
    let mut saw_splice_branch = false;
    let mut best_success: Option<usize> = None;

    for child in children {
        if !matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type1") {
            continue;
        }

        let mut local_issues = Vec::new();
        let warning_len = validation_warning_count();
        let note_len = schema_note_count();
        match validate_array_group_splice(
            compiled,
            definitions,
            child,
            items,
            item_index,
            path,
            &mut local_issues,
        ) {
            Ok(Some(consumed)) => {
                saw_splice_branch = true;
                if local_issues.is_empty() {
                    if best_success.is_none_or(|best| consumed > best) {
                        best_success = Some(consumed);
                    }
                } else {
                    branch_issues.push(local_issues);
                }
            },
            Ok(None) => {},
            Err(()) => {
                saw_splice_branch = true;
                branch_issues.push(local_issues);
            },
        }
        truncate_validation_warnings(warning_len);
        truncate_current_schema_notes(note_len);
    }

    if let Some(consumed) = best_success {
        return Ok(Some(consumed));
    }

    if !saw_splice_branch {
        return Ok(None);
    }

    issues.push(ValidationIssue::new(
        path.to_owned(),
        "one of the listed group alternatives",
        "no matching array group",
        Some("none of the spliced array group alternatives matched".to_owned()),
    ));
    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
    Err(())
}

/// Validate a `type2` group as a splice into the containing array.
fn validate_array_group_splice_type2(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    children: &[WrappedNode],
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Result<Option<usize>, ()> {
    let Some(group) = children
        .iter()
        .find(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"))
    else {
        return Ok(None);
    };

    validate_array_group_splice_group(
        compiled,
        definitions,
        group,
        items,
        item_index,
        path,
        issues,
    )
}

/// Validate a concrete group as a splice into the containing array.
fn validate_array_group_splice_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    group: &WrappedNode,
    items: &[Value],
    item_index: usize,
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> Result<Option<usize>, ()> {
    let remaining = items.len().saturating_sub(item_index);
    let mut branch_issues = Vec::new();
    for prefix_len in (0..=remaining).rev() {
        for grpchoice in group_children(group, "grpchoice") {
            let mut local_issues = Vec::new();
            let warning_len = validation_warning_count();
            let note_len = schema_note_count();
            #[allow(
                clippy::indexing_slicing,
                reason = "Safe as indexes are bounded to slice"
            )]
            if validate_grpchoice_array(
                compiled,
                definitions,
                grpchoice,
                &items[item_index..item_index.saturating_add(prefix_len)],
                path,
                &mut local_issues,
            ) {
                return Ok(Some(prefix_len));
            }
            truncate_validation_warnings(warning_len);
            truncate_current_schema_notes(note_len);
            branch_issues.push(local_issues);
        }
    }

    issues.push(ValidationIssue::new(
        path.to_owned(),
        "a spliced array group",
        "no matching item sequence",
        Some("group did not match the array item sequence".to_owned()),
    ));
    if let Some(best) = best_issue_branch(branch_issues) {
        issues.extend(best);
    }
    Err(())
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
    let Ok(elements) = extract_array_group_elements(compiled, grpchoice) else {
        issues.push(ValidationIssue::new(
            path.to_owned(),
            "a group choice",
            "unrecognized group structure",
            Some("could not extract group elements".to_owned()),
        ));
        return false;
    };

    for (element_index, element) in elements.iter().enumerate() {
        match element.occurrence {
            ArrayOccurrence::One => {
                let Some(consumed) = validate_array_element(
                    compiled,
                    definitions,
                    element.body,
                    items,
                    item_index,
                    path,
                    issues,
                ) else {
                    if item_index < items.len() {
                        return false;
                    }
                    issues.push(ValidationIssue::new(
                        path.to_owned(),
                        "more array items",
                        "end of array",
                        Some(format!("missing array item for element {element_index}")),
                    ));
                    return false;
                };
                item_index = item_index.saturating_add(consumed);
            },
            ArrayOccurrence::Optional => {
                if validate_array_repetition(
                    compiled,
                    definitions,
                    element.body,
                    0,
                    Some(1),
                    items,
                    &mut item_index,
                    path,
                    issues,
                    element_index,
                )
                .is_none()
                {
                    return false;
                }
            },
            ArrayOccurrence::Range { min, max } => {
                if validate_array_repetition(
                    compiled,
                    definitions,
                    element.body,
                    min,
                    max,
                    items,
                    &mut item_index,
                    path,
                    issues,
                    element_index,
                )
                .is_none()
                {
                    return false;
                }
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
                    } else if let Some(name) = grpent_bare_reference_name(body) {
                        // A `grpent` that is a bare group/type reference
                        // (`foo = Bar` or a socket plug RHS such as
                        // `$$socket //= SomeGroup`) contributes the named
                        // group's map entries. Route through the named
                        // group resolver so socket plugs and group rules
                        // validate their entries.
                        validate_named_map_group(
                            compiled,
                            definitions,
                            &name,
                            entries,
                            used,
                            path,
                            issues,
                        )
                    } else if find_memberkey(body).is_some() {
                        // A single map-entry `grpent` body
                        // (`foo = key => value`): match one CBOR map
                        // entry against the member key and validate its
                        // key and value schemas.
                        validate_single_entry_map_group(
                            compiled,
                            definitions,
                            body,
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

/// If a `grpent` body is a bare group/type reference (no member key, no
/// parenthesized group) such as `foo = Bar` or a socket plug RHS
/// `$$socket //= SomeGroup`, return the referenced name.
fn grpent_bare_reference_name(node: &WrappedNode) -> Option<String> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "grpent" {
        return None;
    }
    // A bare group/type reference has no member key; a member-keyed
    // grpent is a map entry, not a group reference.
    if children
        .iter()
        .any(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "memberkey"))
    {
        return None;
    }
    find_single_typename(children)
}

/// Find the single `groupname`/`typename` reference in a bare grpent
/// body, descending through `type`/`type1`/`type2` wrappers.
fn find_single_typename(children: &[WrappedNode]) -> Option<String> {
    for child in children {
        match child {
            WrappedNode::Syntax { rule, text, .. }
                if matches!(rule.as_str(), "groupname" | "typename") =>
            {
                return Some(text.trim().to_owned());
            },
            WrappedNode::Syntax {
                children: inner, ..
            } => {
                if let Some(name) = find_single_typename(inner) {
                    return Some(name);
                }
            },
            _ => {},
        }
    }
    None
}

/// Validate a `grpent` that is a single map entry (`key => value` or
/// `key: value`) against the CBOR map entries, matching one entry and
/// validating its key and value schemas.
fn validate_single_entry_map_group(
    compiled: &CompiledCDDL,
    definitions: &HashMap<String, &WrappedNode>,
    grpent: &WrappedNode,
    entries: &[MapEntry],
    used: &mut [bool],
    path: &[PathStep],
    issues: &mut Vec<ValidationIssue>,
) -> bool {
    let Some(memberkey) = find_memberkey(grpent) else {
        return false;
    };
    let Some(body) = find_grpent_body(grpent) else {
        return false;
    };
    let Some((entry_index, entry)) =
        find_matching_map_entry(compiled, definitions, Some(memberkey), entries, used)
    else {
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
    record_schema_note_once(&key_path, schema_summary(memberkey));
    validate_schema_node(
        compiled,
        definitions,
        memberkey,
        &entry.key,
        &mut key_path,
        issues,
    );
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
    issues.is_empty()
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
                    // A bareword memberkey (e.g. `foo` in `{ foo: uint }`)
                    // names a literal text-string key to match against
                    // the CBOR map's key, not a schema to validate the
                    // key value against. Routing barewords through
                    // `validate_schema_node` would fall into its
                    // `unsupported syntax rule` arm.
                    if let Some(bareword) = memberkey_bareword_text(key_schema) {
                        match &entry.key {
                            Value::Text(actual) => actual == bareword,
                            _ => false,
                        }
                    } else {
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
                    }
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

/// If `memberkey` is a bareword memberkey (e.g. `foo` in `{ foo: uint }`),
/// return its text. Bareword member keys denote a literal text-string
/// key to match against the CBOR map's key, not a schema to validate
/// the key value against.
fn memberkey_bareword_text(memberkey: &WrappedNode) -> Option<&str> {
    let WrappedNode::Syntax { rule, children, .. } = memberkey else {
        return None;
    };
    if rule != "memberkey" {
        return None;
    }
    children.iter().find_map(|child| {
        match child {
            WrappedNode::Syntax { rule, text, .. } if rule == "bareword" => Some(text.trim()),
            _ => None,
        }
    })
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
///
/// The node may be the literal itself (`value`, `bytes`, `text`, `uint`,
/// `int`, `intfloat`, `number`) or a `type2`/`type` wrapper whose direct
/// child is a `value` (for example a byte-string member key written
/// `h'...' => T`). Recurse through the `value` wrapper so literals in
/// either shape resolve.
fn parse_value_literal(node: &WrappedNode) -> Option<EntryState> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            return match rule.as_str() {
                "value" => parse_value_literal(child),
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

/// Push a serialization floor, returning the previous value for restore.
fn push_serialization_floor(
    mode: cbork_utils::serialization_checker::SerializationMode
) -> Option<u8> {
    let new_width = serialization_mode_width(mode);
    CURRENT_SERIALIZATION_FLOOR.with(|slot| {
        let mut floor = slot.borrow_mut();
        let prev = *floor;
        if let Some(current) = *floor {
            if new_width > current {
                *floor = Some(new_width);
            }
        } else {
            *floor = Some(new_width);
        }
        prev
    })
}

/// Pop a previously pushed serialization floor.
fn pop_serialization_floor(previous: Option<u8>) {
    CURRENT_SERIALIZATION_FLOOR.with(|slot| *slot.borrow_mut() = previous);
}

/// RAII guard that pushes a serialization floor on creation and pops
/// it on drop.
struct SerializationFloorGuard {
    /// Previous floor value to restore on drop.
    prev: Option<u8>,
}

impl SerializationFloorGuard {
    /// Push a new floor if it is stricter than the current one.
    fn push(mode: cbork_utils::serialization_checker::SerializationMode) -> Self {
        Self {
            prev: push_serialization_floor(mode),
        }
    }
}

impl Drop for SerializationFloorGuard {
    fn drop(&mut self) {
        pop_serialization_floor(self.prev);
    }
}

/// RAII guard that resets the serialization floor to None on creation
/// and restores it on drop.  Used when entering an explicit bstr
/// payload scope.
struct SerializationFloorReset {
    /// Previous floor value to restore on drop.
    prev: Option<u8>,
}

impl SerializationFloorReset {
    /// Save the current floor and reset it to None.
    fn new() -> Self {
        let prev = CURRENT_SERIALIZATION_FLOOR.with(|slot| {
            let mut floor = slot.borrow_mut();
            let prev = *floor;
            *floor = None;
            prev
        });
        Self { prev }
    }
}

impl Drop for SerializationFloorReset {
    fn drop(&mut self) {
        pop_serialization_floor(self.prev);
    }
}

/// Map a serialization mode to its strictness level (0 = Cbor, 1 = Prefp, 2 = Dtrm).
fn serialization_mode_width(mode: cbork_utils::serialization_checker::SerializationMode) -> u8 {
    match mode {
        cbork_utils::serialization_checker::SerializationMode::Cbor => 0,
        cbork_utils::serialization_checker::SerializationMode::Prefp => 1,
        cbork_utils::serialization_checker::SerializationMode::Dtrm => 2,
    }
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
        PathStep, RenderContext, RootSelectionError, SchemaNote, ValidationIssue,
        clear_current_schema_notes, collect_definitions, exec, format_root_selection_error,
        indent_multiline, render_validation_dump, resolve_validation_root, root_rule_name,
        set_current_source_bytes, take_current_schema_notes, take_current_validation_warnings,
        unique_validation_issues, validate_document,
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
        let (issues, dump, _warnings) =
            validate_schema_bytes_with_dump_and_warnings(schema_name, schema, cbor);
        (issues, dump)
    }

    /// Same as `validate_schema_bytes_with_dump` but also returns the
    /// non-fatal warnings recorded by the validator. Used by the
    /// transform-annotation tests to assert the documentation-only
    /// warning is emitted without applying the RHS ABNF.
    fn validate_schema_bytes_with_dump_and_warnings(
        schema_name: &str,
        schema: &[u8],
        cbor: &[u8],
    ) -> (
        Vec<super::ValidationIssue>,
        String,
        Vec<super::ValidationWarning>,
    ) {
        let schema = write_temp_file(schema_name, schema);
        let compiled =
            CompiledCDDL::compile(&schema, None::<&Path>).expect("schema should compile");
        let root = root_rule_name(&compiled).expect("schema should have root rule");
        let definitions = collect_definitions(&compiled.complete_nodes);
        let document = Document::parse(cbor).expect("CBOR should parse");
        set_current_source_bytes(cbor);
        clear_current_schema_notes();
        super::clear_current_validation_warnings();
        super::clear_current_embedded_cbor_hints();
        let issues = validate_document(&compiled, &definitions, &root, &document);
        let notes = take_current_schema_notes();
        let hints = super::take_current_embedded_cbor_hints();
        let warnings = take_current_validation_warnings();
        let ctx = RenderContext::new(&notes, &hints);
        let dump = render_validation_dump(&schema, "input.cbor", &document, &ctx, None, false);
        (issues, dump, warnings)
    }

    #[test]
    fn validate_succeeds_for_matching_integer() {
        let schema = write_temp_file("schema_ok.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_ok.cbor", &[0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, false, None, true));
    }

    #[test]
    fn validate_fails_for_mismatched_integer() {
        let schema = write_temp_file("schema_fail.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_fail.cbor", &[0x02]);

        assert!(!exec(&schema, Some(&cbor), false, false, false, None, true));
    }

    #[test]
    fn validate_expected_failure_succeeds_for_mismatch() {
        let schema = write_temp_file("schema_expected_fail.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_expected_fail.cbor", &[0x02]);

        assert!(exec(&schema, Some(&cbor), false, false, true, None, true));
    }

    #[test]
    fn validate_expected_failure_fails_for_match() {
        let schema = write_temp_file("schema_unexpected_match.cddl", b"root = 1\n");
        let cbor = write_temp_file("value_unexpected_match.cbor", &[0x01]);

        assert!(!exec(&schema, Some(&cbor), false, false, true, None, true));
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
    fn validate_accepts_named_bstr_type_memberkey() {
        // RFC 8610 §3.5.2: an unquoted identifier on the LHS of `=>` is a
        // TYPE, not a bareword literal. `buuidv4 => any` must match a map
        // whose key is a 16-byte bstr by validating the key against the
        // `buuidv4` rule — never by comparing the key to the literal text
        // "buuidv4". This is the dntls-libs svcrec service-data shape.
        let schema = br"
root = { buuidv4 => any }
buuidv4 = bstr .size 16
";
        let mut cbor = vec![0xA1, 0x50];
        cbor.extend([
            0x55, 0x0E, 0x84, 0x00, 0xE2, 0x9B, 0x41, 0xD4, 0xA7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ]);
        cbor.push(0x00);

        let issues = validate_schema_bytes("named_bstr_type_memberkey.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_rejects_text_key_for_named_type_memberkey() {
        // Negative control: `buuidv4 => any` is a type key. A literal
        // text key "buuidv4" is NOT a 16-byte bstr, so it must not match.
        let schema = br"
root = { buuidv4 => any }
buuidv4 = bstr .size 16
";
        let cbor = [0xA1, 0x67, b'b', b'u', b'u', b'i', b'd', b'v', b'4', 0x00];

        let issues = validate_schema_bytes("text_key_for_type_memberkey.cddl", schema, &cbor);

        assert!(
            !issues.is_empty(),
            "text key must not match a bstr type key"
        );
    }

    #[test]
    fn validate_bareword_memberkey_still_matches_text_key() {
        // Regression: the `:` form with an unquoted identifier is a
        // literal text-string key (bareword), unchanged by the type-key
        // fix.
        let schema = br"
root = { foo: uint }
";
        let cbor = [0xA1, 0x63, b'f', b'o', b'o', 0x01];

        let issues = validate_schema_bytes("bareword_memberkey_text.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_bareword_memberkey_does_not_match_bstr_key() {
        // Negative control for the `:` form: `foo: uint` is the literal
        // text key "foo"; a bstr key must not match it.
        let schema = br"
root = { foo: uint }
";
        let mut cbor = vec![0xA1, 0x41, 0x00];
        cbor.push(0x01);

        let issues = validate_schema_bytes("bareword_memberkey_bstr.cddl", schema, &cbor);

        assert!(
            !issues.is_empty(),
            "bstr key must not match a bareword text key"
        );
    }

    #[test]
    fn validate_accepts_socket_plug_bstr_literal_key() {
        // Faithful dntls-libs svcrec shape for kinds with a concrete
        // binary UUID key, e.g.
        //   nostr-service = ( h'fd7c8fb4 4201 4bcd b2b6 a4a2e1e4c046' => data )
        // The `h'...'` member key is a literal byte-string value (not a
        // bareword), and must match a 16-byte bstr map key through the
        // `$$service-data` socket plug chain.
        let schema = br"
root = { * $$service-data }
$$service-data //= nostr-service
nostr-service = ( h'fd7c8fb442014bcdb2b6a4a2e1e4c046' => any )
";
        let mut cbor = vec![0xA1, 0x50];
        cbor.extend([
            0xFD, 0x7C, 0x8F, 0xB4, 0x42, 0x01, 0x4B, 0xCD, 0xB2, 0xB6, 0xA4, 0xA2, 0xE1, 0xE4,
            0xC0, 0x46,
        ]);
        cbor.push(0x00);

        let issues = validate_schema_bytes("svcrec_bstr_literal_plug.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_socket_plug_named_bstr_type_key() {
        // Faithful dntls-libs svcrec shape:
        //   service-record-data = { * $$service-data }
        //   $$service-data //= generic-service-data
        //   generic-service-data = ( buuidv4 => any )
        // The socket plug contributes a map entry whose key is a
        // 16-byte bstr validated against the `buuidv4` type.
        let schema = br"
root = { * $$service-data }
$$service-data //= generic-service-data
generic-service-data = ( buuidv4 => any )
buuidv4 = bstr .size 16
";
        let mut cbor = vec![0xA1, 0x50];
        cbor.extend([
            0x55, 0x0E, 0x84, 0x00, 0xE2, 0x9B, 0x41, 0xD4, 0xA7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ]);
        cbor.push(0x00);

        let issues = validate_schema_bytes("svcrec_socket_plug.cddl", schema, &cbor);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_socket_plug_within_generic_record() {
        // Faithful dntls-libs svcrec shape including the `.within`
        // guard: service-record-data = { * $$service-data } .within
        // generic-service-record, where generic-service-record admits
        // any generic-service-data entry.
        let schema = br"
root = { * $$service-data } .within generic-service-record
generic-service-record = { * generic-service-data }
$$service-data //= generic-service-data
generic-service-data = ( buuidv4 => any )
buuidv4 = bstr .size 16
";
        let mut cbor = vec![0xA1, 0x50];
        cbor.extend([
            0x55, 0x0E, 0x84, 0x00, 0xE2, 0x9B, 0x41, 0xD4, 0xA7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ]);
        cbor.push(0x00);

        let issues = validate_schema_bytes("svcrec_within_generic_record.cddl", schema, &cbor);

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
    fn validate_accepts_array_occurrence_with_named_upper_bound() {
        let schema = br"
root = [ threshold: uint, 2*MAX item ]
item = [ 1 ]
MAX = 5
";
        let issues = validate_schema_bytes("array_named_range.cddl", schema, &[
            0x83, 0x00, 0x81, 0x01, 0x81, 0x01,
        ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_array_occurrence_with_named_lower_bound() {
        let schema = br"
root = [ threshold: uint, MIN*5 item ]
item = [ 1 ]
MIN = 2
";
        let issues = validate_schema_bytes("array_named_lower_range.cddl", schema, &[
            0x83, 0x00, 0x81, 0x01, 0x81, 0x01,
        ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_accepts_exact_numeric_array_occurrence() {
        let issues = validate_schema_bytes("array_exact_range.cddl", b"root = [ 3*3 uint ]\n", &[
            0x83, 0x01, 0x02, 0x03,
        ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_splices_group_choice_into_array() {
        let schema = br"
root = [ threshold: uint, keyset-3 / keyset-5 ]
keyset-3 = ( 3*3 item )
keyset-5 = ( 5*5 item )
item = [ 1 ]
";
        let issues = validate_schema_bytes("array_group_splice.cddl", schema, &[
            0x84, 0x00, 0x81, 0x01, 0x81, 0x01, 0x81, 0x01,
        ]);

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_any_cborseq_accepts_raw_top_level_sequence() {
        let schema = br"
root = any .cborseq [ headers ]
headers = ( protected: bstr .size 0, unprotected: {} )
";
        let (issues, dump) =
            validate_schema_bytes_with_dump("raw_cborseq.cddl", schema, &[0x40, 0xA0]);

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(
            dump.contains("/CBOR sequence headers/\n  protected: h''\n  unprotected: {}"),
            "{dump}"
        );
        assert!(!dump.contains("/CBOR sequence ["), "{dump}");
        assert!(!dump.contains("/[ headers ]/"), "{dump}");
    }

    #[test]
    fn validate_any_cborseq_rejects_wrapped_array_item() {
        let schema = br"
root = any .cborseq [ headers ]
headers = ( protected: bstr .size 0, unprotected: {} )
";
        let issues = validate_schema_bytes("wrapped_array_cborseq.cddl", schema, &[
            0x43, 0x82, 0x40, 0xA0,
        ]);

        assert!(!issues.is_empty(), "wrapped array unexpectedly passed");
        assert!(
            issues
                .iter()
                .any(|issue| issue.expected == "a CBOR sequence matching the schema"),
            "{issues:#?}"
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.found == "1-item CBOR sequence: h'82 40 a0'"),
            "{issues:#?}"
        );
        assert!(
            issues.iter().any(|issue| {
                issue
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("CBOR sequence"))
            }),
            "{issues:#?}"
        );
    }

    #[test]
    fn validation_failure_output_deduplicates_exact_issues() {
        let issues = vec![
            ValidationIssue::new(
                vec![PathStep::DocItem(0)],
                "one of the listed CBOR sequence group alternatives",
                "no matching CBOR sequence group",
                Some("none of the CBOR sequence group alternatives matched".to_owned()),
            ),
            ValidationIssue::new(
                vec![PathStep::DocItem(0)],
                "one of the listed CBOR sequence group alternatives",
                "no matching CBOR sequence group",
                Some("none of the CBOR sequence group alternatives matched".to_owned()),
            ),
            ValidationIssue::new(
                vec![PathStep::DocItem(0)],
                "more CBOR sequence items",
                "end of CBOR sequence",
                Some("missing CBOR sequence item for element 0".to_owned()),
            ),
        ];

        let unique = unique_validation_issues(&issues);

        assert_eq!(unique.len(), 2);
        assert_eq!(unique[0], &issues[0]);
        assert_eq!(unique[1], &issues[2]);
    }

    #[test]
    fn validate_non_sequence_root_rejects_top_level_sequence() {
        let issues = validate_schema_bytes(
            "non_sequence_root.cddl",
            b"root = [ bstr .size 0, {} ]\n",
            &[0x40, 0xA0],
        );

        assert!(!issues.is_empty(), "top-level sequence unexpectedly passed");
        assert!(
            issues.iter().any(|issue| {
                issue.message.as_deref() == Some("CBOR sequence validation is not yet implemented")
            }),
            "{issues:#?}"
        );
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

        let compiled = CompiledCDDL::compile(
            write_temp_file("validation_dump_inline.cddl", b"root = 1\n"),
            None::<&Path>,
        )
        .expect("schema should compile");
        let _definitions = collect_definitions(&compiled.complete_nodes);
        let ctx = RenderContext::new(&notes, &[]);

        let rendered = render_validation_dump(
            Path::new("schema.cddl"),
            "input.cbor",
            &document,
            &ctx,
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

        assert!(exec(&schema, Some(&cbor), false, false, false, None, true));
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
            false,
            Some("payload"),
            true
        ));
        assert!(!exec(
            &schema,
            Some(&mismatching),
            false,
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
            false,
            Some("wrapper"),
            true
        ));
        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
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
        let (root, _node) = resolve_validation_root(&compiled, &schema, Some("payload"))
            .expect("payload is selectable");
        assert_eq!(root, "payload");

        // Detailed dump still passes through `exec`.
        assert!(exec(
            &schema,
            Some(&cbor),
            false,
            true,
            false,
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

        let indefinite_explicit = RootSelectionError::IndefiniteRoot {
            name: "member".to_owned(),
            explicit: true,
        };
        let rendered = format_root_selection_error(&indefinite_explicit, &schema);
        assert!(rendered.contains("CDDL group"));
        assert!(rendered.contains("cannot be selected as the validation root"));
        assert!(rendered.contains("member"));

        let indefinite_natural = RootSelectionError::IndefiniteRoot {
            name: "root".to_owned(),
            explicit: false,
        };
        let rendered = format_root_selection_error(&indefinite_natural, &schema);
        assert!(rendered.contains("CDDL group"));
        assert!(rendered.contains("cannot be selected as the validation root"));
        assert!(rendered.contains("root"));
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
            false,
            Some("uint"),
            true
        ));
    }

    // Plan 014 — indefinite CDDL groups must be rejected as the
    // validation root before data validation starts.

    #[test]
    fn natural_root_group_fails_before_data_validation() {
        let dir = write_temp_dir_tree(&["natural_group_root"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = ( key: uint )\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(7));

        assert!(!exec(&schema, Some(&cbor), false, false, false, None, true));

        let err = resolve_validation_root(
            &CompiledCDDL::compile(&schema, None::<&Path>).expect("schema compiles"),
            &schema,
            None,
        )
        .expect_err("group root must be rejected");
        assert!(matches!(
            err,
            RootSelectionError::IndefiniteRoot {
                ref name,
                explicit: false
            } if name == "root"
        ));

        let rendered = format_root_selection_error(&err, &schema);
        assert!(rendered.contains("CDDL group"));
        assert!(rendered.contains("cannot be selected as the validation root"));
    }

    #[test]
    fn type_override_local_group_rule_fails() {
        let dir = write_temp_dir_tree(&["type_override_group"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = [ member ]\nmember = ( key: uint )\n",
        );
        let cbor = write_cbor(&dir, "value.cbor", &[0x81, 0x01]);

        assert!(!exec(
            &schema,
            Some(&cbor),
            false,
            false,
            false,
            Some("member"),
            true
        ));

        let err = resolve_validation_root(
            &CompiledCDDL::compile(&schema, None::<&Path>).expect("schema compiles"),
            &schema,
            Some("member"),
        )
        .expect_err("group selection via --type must be rejected");
        assert!(matches!(
            err,
            RootSelectionError::IndefiniteRoot {
                ref name,
                explicit: true
            } if name == "member"
        ));
    }

    #[test]
    fn concrete_array_root_still_validates() {
        let dir = write_temp_dir_tree(&["concrete_array_root"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = [ uint ]\n");
        let cbor = write_cbor(&dir, "value.cbor", &[0x81, 0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, false, None, true));
    }

    #[test]
    fn concrete_map_root_still_validates() {
        let dir = write_temp_dir_tree(&["concrete_map_root"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { foo: uint }\n");
        let cbor = write_cbor(&dir, "value.cbor", &[0xA1, 0x63, 0x66, 0x6F, 0x6F, 0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, false, None, true));
    }

    #[test]
    fn nested_group_in_array_still_validates() {
        let dir = write_temp_dir_tree(&["nested_group_in_array"]);
        // Group splice inside an array: the validator must continue to
        // expand the group into the surrounding array context, even
        // though `member` itself is a CDDL group.
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = [ member ]\nmember = ( 1*1 item )\nitem = [ 1 ]\n",
        );
        // CBOR: [ [1] ] — array containing one nested [1] matching item.
        let cbor = write_cbor(&dir, "value.cbor", &[0x81, 0x81, 0x01]);

        assert!(exec(&schema, Some(&cbor), false, false, false, None, true));
    }

    #[test]
    fn fails_does_not_convert_indefinite_root_error() {
        let dir = write_temp_dir_tree(&["fails_indefinite_root"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = ( key: uint )\n");
        let cbor = write_cbor(&dir, "value.cbor", &cbor_integer(7));

        // `fails = true` must not turn the indefinite-root selection
        // error into a pass: this is a schema/root-selection error,
        // not a data validation mismatch.
        assert!(!exec(&schema, Some(&cbor), false, false, true, None, true));
    }

    // Plan 016 — embedded-CBOR byte strings must render as `<<...>>` with
    // their decoded contents, not as opaque `h'...'` values, whenever the
    // schema carries a `.cbor`, `.cborseq`, `.prefp`, `.prefpseq`, `.dtrm`,
    // or `.dtrmseq` control operator.
    //
    // Fixture provenance:
    // * The `<<...>>` output syntax is taken from `rfc/draft-ietf-cbor-edn-literals-25.txt`
    //   Section 2.5.6 (embedded CBOR) and Section 5.1 (grammar). The comma-separated sequence
    //   inside the wrapper is the draft's prescribed formatting.
    // * The deterministic re-encoding used by `.dtrm`/`.dtrmseq` is
    //   `rfc/draft-ietf-cbor-serialization-06.txt` Section 5 (deterministic serialization),
    //   which is a strict superset of Section 4 (preferred-plus serialization).
    // * The preferred-plus re-encoding used by `.prefp`/`.prefpseq` is
    //   `rfc/draft-ietf-cbor-serialization-06.txt` Section 4.
    // * The COSE-style `{protected: h'a10126', unprotected: {}}` example is taken from
    //   `rfc/draft-ietf-cbor-edn-literals-25.txt` Section 1.3.3 (embedded CBOR in a COSE
    //   example).

    fn encode_cbor_map(entries: &[(u8, u8)]) -> Vec<u8> {
        let mut out = vec![0xA0 | u8::try_from(entries.len()).expect("fits in u8")];
        for (key, value) in entries {
            out.push(*key);
            out.push(*value);
        }
        out
    }

    fn encode_cbor_bstr(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        match bytes.len() {
            0..=23 => out.push(0x40 | u8::try_from(bytes.len()).expect("fits in u8")),
            24..=255 => {
                out.push(0x58);
                out.push(u8::try_from(bytes.len()).expect("fits in u8"));
            },
            _ => {
                out.push(0x59);
                let len = u16::try_from(bytes.len()).expect("fits in u16");
                out.extend_from_slice(&len.to_be_bytes());
            },
        }
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn embedded_cbor_bstr_renders_as_double_angle_wrapper() {
        let dir = write_temp_dir_tree(&["embedded_cbor_basic"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = [ protected: bstr .cbor headers, unprotected: {} ]\n\
              headers = { ? 1 => int }\n",
        );
        // Build `[bstr({1: -7}), {}]`.
        let inner = encode_cbor_map(&[(0x01, 0x26)]);
        let mut cbor = vec![0x82];
        cbor.extend(encode_cbor_bstr(&inner));
        cbor.push(0xA0);
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "embedded_cbor_basic.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        // The `<<...>>` wrapper is present, the byte string is gone, and
        // the inner `{1: -7}` map is shown with the schema-aware labels.
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains(">>"), "{dump}");
        assert!(dump.contains("1: /int/ -7"), "{dump}");
        assert!(!dump.contains("h'a1 01 26'"), "{dump}");
    }

    #[test]
    fn plain_bstr_does_not_get_decoded() {
        let dir = write_temp_dir_tree(&["plain_bstr"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr }\n");
        // Even though the bytes are valid CBOR (a single integer `1`),
        // a plain `bstr` carrier must not be auto-decoded.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0x01]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "plain_bstr.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("h'01'"), "{dump}");
        assert!(!dump.contains("<<"), "{dump}");
    }

    // Plan 017 — `.abnfb` and `.abnf` must evaluate composed `.det` RHS
    // expressions, parse the resulting ABNF source, validate the input
    // bytes or text against the grammar, and render a CDN comment
    // breakdown of the selected match tree.
    //
    // Fixture provenance:
    // * The composed `.det` resolution rule comes from RFC 8610 §3.5 (the `.det`
    //   text-processing operator).
    // * The preferred-plus / deterministic / `.abnf` / `.abnfb` control operators are defined
    //   in `rfc/draft-ietf-cbor-serialization-06.txt` and the embedded-CBOR extension; plan
    //   017 integrates the `cbork-abnf-parser` trace API to expose match trees.
    // * The CDN comment syntax follows `rfc/draft-ietf-cbor-edn-literals-25.txt` section 2.2.

    #[test]
    fn abnfb_accepts_direct_text_rhs_with_valid_bytes() {
        let dir = write_temp_dir_tree(&["abnfb_direct"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .abnfb (\"FOO = 3OCTET\\nOCTET = %x00-FF\") }\n",
        );
        // Three arbitrary bytes — the grammar accepts any 3 octets.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0xAA, 0xBB, 0xCC]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "abnfb_direct.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("// ABNF:"), "{dump}");
    }

    #[test]
    fn abnfb_rejects_mismatch_with_abnf_data_error() {
        let dir = write_temp_dir_tree(&["abnfb_mismatch"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .abnfb (\"FOO = %x41.42.43\") }\n",
        );
        // The grammar requires `ABC`; we provide `ABD` to trigger a
        // data mismatch rather than a parse error.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(b"ABD"));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnfb_mismatch.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        let combined: String = issues
            .iter()
            .filter_map(|i| i.message.as_deref())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("ABNF") || combined.contains("match"),
            "expected an ABNF data-mismatch diagnostic, got: {combined}"
        );
    }

    #[test]
    fn abnfb_rejects_malformed_abnf_after_successful_rhs_resolution() {
        let dir = write_temp_dir_tree(&["abnfb_bad_abnf"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .abnfb (\"this is not valid abnf\") }\n",
        );
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(b"any bytes"));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnfb_bad_abnf.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        let combined: String = issues
            .iter()
            .filter_map(|i| i.message.as_deref())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("ABNF parsing failed"),
            "expected an ABNF-parse diagnostic, got: {combined}"
        );
    }

    #[test]
    fn abnfb_rejects_unsupported_rhs_form_with_clear_diagnostic() {
        let dir = write_temp_dir_tree(&["abnfb_unsupported"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            // The RHS is a numeric literal, not text; the resolver
            // should refuse it cleanly.
            b"root = { payload: bstr .abnfb (42) }\n",
        );
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(b"any bytes"));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnfb_unsupported.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        let combined: String = issues
            .iter()
            .filter_map(|i| i.message.as_deref())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("ABNF RHS did not resolve to text"),
            "expected an ABNF-RHS-resolution diagnostic, got: {combined}"
        );
    }

    #[test]
    fn abnfb_preserves_det_whitespace_semantics() {
        let dir = write_temp_dir_tree(&["abnfb_det_ws"]);
        // The LHS text ends with a newline so the result is on its own
        // line; the RHS named rule has 4 leading spaces; `.det` should
        // dedent both to produce a well-formed ABNF source.
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .abnfb (\"label\\n\" .det label-abnf) }\n\
              label-abnf = '  label = %x41.42.43'\n",
        );
        // After `.det`, the source is:
        //   label
        //   label = %x41.42.43
        // The leading "label\n" line is the LHS label that we strip.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(b"ABC"));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnfb_det_ws.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn abnfb_lhs_size_failure_remains_distinguishable_from_data_mismatch() {
        let dir = write_temp_dir_tree(&["abnfb_size_vs_mismatch"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: (bstr .size 3) .abnfb (\"FOO = 3OCTET\\nOCTET = %x00-FF\") }\n",
        );
        // The size constraint fails (only 2 bytes provided) before the
        // ABNF matcher runs, so the diagnostic must reference size.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0xAA, 0xBB]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnfb_size_vs_mismatch.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        let combined: String = issues
            .iter()
            .filter_map(|i| i.message.as_deref())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("size constraint failed") || combined.contains("size 3"),
            "size failure must be reported, got: {combined}"
        );
    }

    #[test]
    fn abnfb_successful_dump_includes_cdn_comment_breakdown() {
        let dir = write_temp_dir_tree(&["abnfb_dump"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .abnfb (\"FOO = 3OCTET\\nOCTET = %x00-FF\") }\n",
        );
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0xAA, 0xBB, 0xCC]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "abnfb_dump.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        // The original `h'...'` value must remain.
        assert!(dump.contains("h'aa bb cc'"), "{dump}");
        // The CDN comment breakdown must appear below the value with
        // indented `// ABNF: ...` lines.
        assert!(dump.contains("// ABNF:"), "{dump}");
        assert!(dump.contains("FOO"), "{dump}");
    }

    #[test]
    fn abnf_text_validation_uses_validate_text_path() {
        let dir = write_temp_dir_tree(&["abnf_text"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: text .abnf \"FOO = \\\"hello\\\"\" }\n",
        );
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        // The text "hello" as a 5-byte CBOR text string.
        cbor.extend_from_slice(&[0x65, b'h', b'e', b'l', b'l', b'o']);
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "abnf_text.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    // Plan 017 transform-annotation follow-up: the four `.x-enc.*` and
    // `.x-hash.*` forms are documentation-only at validation time. They
    // must still enforce the left-hand-side carrier constraints
    // (CBOR type, `.size`) but must not apply the right-hand-side ABNF
    // grammar to the encoded carrier bytes. They must also produce no
    // ABNF match trace or CDN comment breakdown.

    #[test]
    fn xenc_abnfb_accepts_valid_carrier_without_applying_abnf() {
        // The RHS would fail ABNF (the literal is not a complete rule
        // and is followed by an unparsable tail) but `.x-enc.abnfb`
        // must not parse or apply it — the carrier-only path must
        // accept the value.
        let dir = write_temp_dir_tree(&["xenc_abnfb_accept"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (bstr .size 4) .x-enc.abnfb \"this is not valid abnf\"\n",
        );
        // A 4-byte byte string that does not match any ABNF grammar.
        let cbor = encode_cbor_bstr(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump, warnings) = validate_schema_bytes_with_dump_and_warnings(
            "xenc_abnfb_accept.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        // The RHS ABNF must not produce a mismatch issue.
        assert!(
            issues.is_empty(),
            ".x-enc.abnfb must not apply the RHS ABNF: {issues:#?}"
        );
        // A documentation-only warning is expected.
        assert!(
            warnings
                .iter()
                .any(|w| w.text.contains(".x-enc.abnfb") && w.text.contains("documentation-only")),
            "expected a documentation-only warning, got: {warnings:#?}"
        );
        // The detailed dump must not contain any ABNF comment breakdown
        // for the transform annotation.
        assert!(
            !dump.contains("// ABNF:"),
            "transform annotation must not emit an ABNF breakdown:\n{dump}"
        );
    }

    #[test]
    fn xenc_abnfb_rejects_wrong_carrier_type() {
        // The LHS is `bstr .size 4`. Supplying an `int` should still
        // fail because the carrier constraints are authoritative.
        let dir = write_temp_dir_tree(&["xenc_abnfb_wrong_type"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (bstr .size 4) .x-enc.abnfb \"unused\"\n",
        );
        let _cbor_path = write_cbor(&dir, "value.cbor", &[0x18, 0x64]);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "xenc_abnfb_wrong_type.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &[0x18, 0x64],
        );

        assert!(
            !issues.is_empty(),
            "wrong carrier type must still be rejected"
        );
    }

    #[test]
    fn xenc_abnfb_rejects_wrong_size() {
        // The LHS is `bstr .size 4`. Supplying a 3-byte byte string
        // should still fail the size check.
        let dir = write_temp_dir_tree(&["xenc_abnfb_wrong_size"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (bstr .size 4) .x-enc.abnfb \"unused\"\n",
        );
        let _cbor_path = write_cbor(&dir, "value.cbor", &[0x43, 0xAA, 0xBB, 0xCC]);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "xenc_abnfb_wrong_size.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &[0x43, 0xAA, 0xBB, 0xCC],
        );

        assert!(!issues.is_empty(), "wrong .size must still be rejected");
        let combined: String = issues
            .iter()
            .filter_map(|i| i.message.as_deref())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("size 4") || combined.contains("size constraint"),
            "size failure must be reported, got: {combined}"
        );
    }

    #[test]
    fn xhash_abnf_accepts_valid_carrier_without_applying_abnf() {
        let dir = write_temp_dir_tree(&["xhash_abnf_accept"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (bstr .size 8) .x-hash.abnf \"this is not valid abnf\"\n",
        );
        let mut cbor = vec![0x48];
        cbor.extend_from_slice(&[0x00; 8]);
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump, warnings) = validate_schema_bytes_with_dump_and_warnings(
            "xhash_abnf_accept.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.text.contains(".x-hash.abnf") && w.text.contains("documentation-only")),
            "expected a documentation-only warning, got: {warnings:#?}"
        );
        assert!(
            !dump.contains("// ABNF:"),
            "transform annotation must not emit an ABNF breakdown:\n{dump}"
        );
    }

    #[test]
    fn xhash_abnfb_accepts_valid_carrier_without_applying_abnf() {
        let dir = write_temp_dir_tree(&["xhash_abnfb_accept"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (bstr .size 16) .x-hash.abnfb \"this is not valid abnf\"\n",
        );
        let mut cbor = vec![0x50];
        cbor.extend_from_slice(&[0x00; 16]);
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump, warnings) = validate_schema_bytes_with_dump_and_warnings(
            "xhash_abnfb_accept.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.text.contains(".x-hash.abnfb") && w.text.contains("documentation-only")),
            "expected a documentation-only warning, got: {warnings:#?}"
        );
        assert!(
            !dump.contains("// ABNF:"),
            "transform annotation must not emit an ABNF breakdown:\n{dump}"
        );
    }

    #[test]
    fn xenc_abnf_accepts_valid_text_carrier_without_applying_abnf() {
        // The `.x-enc.abnf` text variant must also be documentation-only
        // for ordinary text carriers.
        let dir = write_temp_dir_tree(&["xenc_abnf_accept"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = (text .size 4) .x-enc.abnf \"this is not valid abnf\"\n",
        );
        // 4-byte text string "test".
        let cbor = vec![0x64, b't', b'e', b's', b't'];
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "xenc_abnf_accept.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
    }

    // Plan 017 regression — the exact reproduction from the plan
    // is exercised as a CLI test in `lint::tests::cli_validate_dntls_*`
    // because the dntls schema imports `./time.cddl`, which is only
    // resolvable when the compiler runs from the schema's containing
    // directory. Unit tests cannot easily reproduce that context.

    #[test]
    fn embedded_cbor_indent_helper_indents_continuation_lines() {
        let text = "first\nsecond\nthird";
        let indented = indent_multiline(text, 4);
        assert_eq!(indented, "first\n    second\n    third");

        let blank = "first\n\nthird";
        let indented_blank = indent_multiline(blank, 2);
        assert_eq!(indented_blank, "first\n\n  third");
    }

    #[allow(dead_code)]
    fn encode_cbor_array(items: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80 | u8::try_from(items.len()).expect("fits in u8")];
        out.extend_from_slice(items);
        out
    }

    #[test]
    fn embedded_cborseq_renders_multiple_top_level_items() {
        let dir = write_temp_dir_tree(&["embedded_cborseq"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { ids: bstr .cborseq uint }\n");
        // CBOR sequence with two integers: 1 and 2 → `01 02`.
        let mut cbor = vec![0xA1, 0x63, b'i', b'd', b's'];
        cbor.extend(encode_cbor_bstr(&[0x01, 0x02]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "embedded_cborseq.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains(">>"), "{dump}");
        // Both items are present in the wrapper, separated by a comma.
        assert!(dump.contains("1,"), "{dump}");
        assert!(dump.contains('2'), "{dump}");
    }

    #[test]
    fn embedded_dtrm_keeps_raw_bytes_on_non_deterministic() {
        let dir = write_temp_dir_tree(&["embedded_dtrm"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr .dtrm int }\n");
        // A non-deterministically encoded CBOR map for `{1: 2}` —
        // `a2 02 01 01 02` is non-canonical. The validator must report
        // the deterministic failure and the renderer must keep the raw
        // bytes (no `<<...>>` wrapper) so the user can compare.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        let bad_bytes = vec![0xA2, 0x02, 0x01, 0x01, 0x02];
        cbor.extend(encode_cbor_bstr(&bad_bytes));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "embedded_dtrm.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("h'a2 02 01 01 02'"), "{dump}");
        assert!(!dump.contains("<<"), "{dump}");
    }

    #[test]
    fn embedded_dtrm_accepts_canonical_encoding() {
        let dir = write_temp_dir_tree(&["embedded_dtrm_ok"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr .dtrm int }\n");
        // `0x01` is the canonical encoding of integer 1.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0x01]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "embedded_dtrm_ok.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains('1'), "{dump}");
    }

    #[test]
    fn embedded_cbor_nested_two_levels() {
        let dir = write_temp_dir_tree(&["nested_two_levels"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"outer = bstr .cbor middle\n\
              middle = { inner: bstr .cbor leaf }\n\
              leaf = { value: uint }\n",
        );
        // Build leaf = {value: 1} → `a1 65 76 61 6c 75 65 01`
        let leaf_bytes = vec![0xA1, 0x65, b'v', b'a', b'l', b'u', b'e', 0x01];
        // Build middle = {inner: bstr(leaf)} → `a1 65 69 6e 6e 65 72 47 <leaf>`
        let mut middle_bytes = vec![0xA1, 0x65, b'i', b'n', b'n', b'e', b'r'];
        middle_bytes.extend(encode_cbor_bstr(&leaf_bytes));
        // Build outer = bstr(middle)
        let outer_bytes = encode_cbor_bstr(&middle_bytes);

        let _cbor_path = write_cbor(&dir, "value.cbor", &outer_bytes);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "nested_two_levels.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &outer_bytes,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        // Two levels of `<<...>>` should appear.
        let open = dump.matches("<<").count();
        let close = dump.matches(">>").count();
        assert_eq!(open, 2, "expected two `<<` wrappers, got:\n{dump}");
        assert_eq!(close, 2, "expected two `>>` wrappers, got:\n{dump}");
        // The leaf map should be visible at the deepest level.
        assert!(dump.contains("\"value\""), "{dump}");
        assert!(dump.contains('1'), "{dump}");
    }

    #[test]
    fn embedded_cbor_any_rhs_renders_generic_decoded_view() {
        let dir = write_temp_dir_tree(&["any_rhs"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr .cbor any }\n");
        // { payload: bstr(any) } where the inner CBOR is `{1: -7}`.
        let inner = encode_cbor_map(&[(0x01, 0x26)]);
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&inner));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "any_rhs.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains(">>"), "{dump}");
        // Generic renderer: just the key/value without a named schema label.
        assert!(dump.contains("1: -7"), "{dump}");
    }

    #[test]
    fn embedded_cbor_malformed_keeps_raw_bytes_and_emits_issue() {
        let dir = write_temp_dir_tree(&["malformed"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr .cbor int }\n");
        // CBOR that does not parse: `0xFF` is an invalid initial byte.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0xFF]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "malformed.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
        // Raw bytes remain visible because the embedded parse failed.
        assert!(dump.contains("h'ff'"), "{dump}");
        assert!(!dump.contains("<<"), "{dump}");
    }

    /// Three-level nested `bstr .cbor` regression fixture from plan 016.
    /// The schema deliberately has three `bstr .cbor` carriers, each in
    /// a distinct outer context, so the validator records three separate
    /// hints and the renderer must produce three nested `<<...>>` wrappers
    /// with the deepest `value` field still visible.
    #[test]
    fn embedded_cbor_three_level_nested_regression() {
        let dir = write_temp_dir_tree(&["nested_three_levels"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { outer: bstr .cbor middle }\n\
              middle = { inner: bstr .cbor leaf }\n\
              leaf = { tag: bstr .cbor value }\n\
              value = { v: uint }\n",
        );
        // value = {v: 1} → `a1 61 76 01`
        let value_bytes = vec![0xA1, 0x61, b'v', 0x01];
        // leaf = {tag: bstr(value)} → `a1 63 74 61 67 44 <value>`
        let mut leaf_bytes = vec![0xA1, 0x63, b't', b'a', b'g'];
        leaf_bytes.extend(encode_cbor_bstr(&value_bytes));
        // middle = {inner: bstr(leaf)} → `a1 65 69 6e 6e 65 72 <bstr(leaf)>`
        let mut middle_bytes = vec![0xA1, 0x65, b'i', b'n', b'n', b'e', b'r'];
        middle_bytes.extend(encode_cbor_bstr(&leaf_bytes));
        // root = {outer: bstr(middle)} → `a1 65 6f 75 74 65 72 <bstr(middle)>`
        let mut root_bytes = vec![0xA1, 0x65, b'o', b'u', b't', b'e', b'r'];
        root_bytes.extend(encode_cbor_bstr(&middle_bytes));
        let _cbor_path = write_cbor(&dir, "value.cbor", &root_bytes);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "nested_three_levels.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &root_bytes,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        // Three nested `<<...>>` wrappers, one per `.cbor` operator.
        let open = dump.matches("<<").count();
        let close = dump.matches(">>").count();
        assert_eq!(open, 3, "expected three `<<` wrappers, got:\n{dump}");
        assert_eq!(close, 3, "expected three `>>` wrappers, got:\n{dump}");
        // The leaf `v` field must appear at the deepest level.
        assert!(dump.contains("\"v\""), "{dump}");
        assert!(dump.contains('1'), "{dump}");
    }

    /// Empty `.cborseq` payload renders as `<<>>` per
    /// `rfc/draft-ietf-cbor-edn-literals-25.txt` Section 2.5.6.
    #[test]
    fn embedded_cborseq_empty_payload_renders_double_angle_empty() {
        let dir = write_temp_dir_tree(&["empty_cborseq"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { ids: bstr .cborseq uint }\n");
        let mut cbor = vec![0xA1, 0x63, b'i', b'd', b's'];
        cbor.extend(encode_cbor_bstr(&[]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "empty_cborseq.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<>>"), "{dump}");
    }

    /// Empty `.cbor` payload is rejected (a single-item context cannot be empty).
    #[test]
    fn embedded_cbor_empty_payload_rejected() {
        let dir = write_temp_dir_tree(&["empty_cbor_reject"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { payload: bstr .cbor int }\n");
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, _dump) = validate_schema_bytes_with_dump(
            "empty_cbor_reject.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(!issues.is_empty(), "{issues:#?}");
    }

    /// `.prefp` accepts preferred-plus encoding per
    /// `rfc/draft-ietf-cbor-serialization-06.txt` Section 4.
    #[test]
    fn embedded_prefp_accepts_canonical() {
        let dir = write_temp_dir_tree(&["prefp_ok"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .prefp int }\n",
        );
        // `0x01` is shortest-form for the integer 1.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0x01]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "prefp_ok.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains('1'), "{dump}");
    }

    /// `.prefp` rejects non-shortest-form integer encoding but keeps the
    /// decoded view visible alongside the validation error.
    #[test]
    fn embedded_prefp_rejects_non_shortest_form_but_keeps_decoded_view() {
        let dir = write_temp_dir_tree(&["prefp_fail"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { payload: bstr .prefp int }\n",
        );
        // `0x18 0x01` is a non-shortest encoding of integer 1; the
        // draft requires shortest-form encoding.
        let mut cbor = vec![0xA1, 0x67, b'p', b'a', b'y', b'l', b'o', b'a', b'd'];
        cbor.extend(encode_cbor_bstr(&[0x18, 0x01]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "prefp_fail.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        // The encoding fails the preferred-plus check; the issue must
        // be reported and the decoded view must remain visible.
        assert!(!issues.is_empty(), "{issues:#?}");
        assert!(
            issues.iter().any(|i| {
                i.message
                    .as_deref()
                    .is_some_and(|m| m.contains("preferred-plus"))
            }),
            "{issues:#?}"
        );
        assert!(dump.contains("<<"), "{dump}");
        assert!(dump.contains('1'), "{dump}");
    }

    /// `.prefpseq` accepts an empty sequence.
    #[test]
    fn embedded_prefpseq_empty_payload_renders_double_angle_empty() {
        let dir = write_temp_dir_tree(&["empty_prefpseq"]);
        let schema = write_cddl(
            &dir,
            "schema.cddl",
            b"root = { ids: bstr .prefpseq uint }\n",
        );
        let mut cbor = vec![0xA1, 0x63, b'i', b'd', b's'];
        cbor.extend(encode_cbor_bstr(&[]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "empty_prefpseq.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("<<>>"), "{dump}");
    }

    /// `.cborseq` with two top-level items renders both inside `<<...>>`,
    /// separated by commas per the EDN-literals draft.
    #[test]
    fn embedded_cborseq_two_items_shows_both_with_comma() {
        let dir = write_temp_dir_tree(&["cborseq_two_items"]);
        let schema = write_cddl(&dir, "schema.cddl", b"root = { ids: bstr .cborseq uint }\n");
        let mut cbor = vec![0xA1, 0x63, b'i', b'd', b's'];
        cbor.extend(encode_cbor_bstr(&[0x01, 0x02]));
        let _cbor_path = write_cbor(&dir, "value.cbor", &cbor);

        let (issues, dump) = validate_schema_bytes_with_dump(
            "cborseq_two_items.cddl",
            &{ std::fs::read(&schema).expect("schema read") },
            &cbor,
        );

        assert!(issues.is_empty(), "{issues:#?}");
        assert!(dump.contains("1,"), "{dump}");
        assert!(dump.contains('2'), "{dump}");
    }

    /// Resource-limit fallback: when the embedded byte count exceeds
    /// `max_embedded_bytes`, the renderer must emit raw `h'...'` rather
    /// than crash or silently truncate.
    #[test]
    fn embedded_cbor_resource_limit_falls_back_to_raw_bytes() {
        // Build a payload larger than `max_embedded_bytes` (16 MiB).
        // We use the raw document path rather than going through the
        // schema compiler, exercising the renderer's own limit checks.
        use cbork_edn::Document;

        use crate::decode::EmbedLimits;
        let limits = EmbedLimits {
            depth: 32,
            embedded_bytes: 64,
            sequence_items: 4096,
        };
        // Outer document: a map with one key whose value is a byte string.
        let inner: Vec<u8> = (0..200).collect();
        let mut outer = vec![0xA1, 0x01, 0x58, u8::try_from(inner.len()).unwrap()];
        outer.extend_from_slice(&inner);
        let document = Document::parse(&outer).expect("outer parses");
        let rendered = crate::decode::render_document(&document, false, false, false, limits);
        // The byte string exceeds the budget; renderer must emit raw bytes.
        assert!(rendered.contains("h'"), "{rendered}");
        // No `<<` expansion should occur.
        assert!(!rendered.contains("<<"), "{rendered}");
    }

    /// Resource-limit fallback: when the recursive depth exceeds
    /// `max_depth`, the renderer must stop expanding nested levels.
    #[test]
    fn embedded_cbor_resource_limit_depth_falls_back() {
        use cbork_edn::Document;

        use crate::decode::EmbedLimits;
        let limits = EmbedLimits {
            depth: 2,
            embedded_bytes: 1024 * 1024,
            sequence_items: 4096,
        };
        // Build two levels of nesting:
        // inner = `0x01` (an integer)
        // outer = `0x41 0x01` (byte string of length 1 containing inner)
        let outer = vec![0x41, 0x01];
        let document = Document::parse(&outer).expect("outer parses");
        // The single byte string parses successfully but the depth limit
        // of 2 is reached at the first level, so the renderer should
        // fall back to raw bytes.
        let rendered = crate::decode::render_document(&document, false, false, false, limits);
        assert!(rendered.contains("h'01"), "{rendered}");
    }
}
