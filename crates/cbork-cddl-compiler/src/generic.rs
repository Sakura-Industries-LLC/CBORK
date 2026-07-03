// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Generic expansion for CDDL type and group instantiations.
//!
//! CDDL generics behave like macros for the compiler pipeline: imported
//! definitions are first made available, then generic instantiations are
//! expanded before literal and control-operator folding.

use std::{collections::HashMap, path::PathBuf};

use cbork_cddl_parser::parse_cddl;

use crate::{
    WrappedNode,
    error::{Diagnostic, DiagnosticLevel},
    node::SourceOrigin,
    preprocessor::{inject_directives, process_ast},
};

/// Expand generic instantiations in-place.
///
/// Generic collection is intentionally silent here: the plain-vs-generic
/// collision check (E013) used to fire from this pass against the
/// unpruned resolved tree, which made an unreferenced weak imported
/// generic helper collide with a strong local plain rule.  The
/// collision is now diagnosed after pruning and definition-strength
/// normalization in `finalize::detect_plain_generic_collisions`, so
/// only retained, equally-strong collisions surface here.
pub(crate) fn expand_generics(
    nodes: &mut [WrappedNode],
    warnings: &mut Vec<Diagnostic>,
) {
    let definitions = collect_generic_definitions(nodes);

    for node in nodes {
        let mut stack = Vec::new();
        let _ = expand_node(node, &definitions, warnings, &mut stack);
    }
}

/// A generic type or group definition.
#[derive(Clone)]
#[allow(dead_code, reason = "origin/span kept for diagnostic reuse")]
struct GenericDefinition {
    /// Formal parameter names.
    params: Vec<String>,
    /// Definition body to clone for each instantiation.
    body: WrappedNode,
    /// Source origin of the generic rule definition.
    origin: SourceOrigin,
    /// Source span of the generic rule definition.
    span: std::ops::Range<usize>,
}

/// Collect all generic definitions visible in the resolved user tree.
///
/// Plain-rule/generic-rule collisions are intentionally NOT reported here.
/// They are diagnosed by `finalize::detect_plain_generic_collisions` on
/// the pruned + strength-normalized tree, so an unreferenced weak
/// imported generic helper does not spuriously collide with a strong
/// local plain rule.
fn collect_generic_definitions(nodes: &[WrappedNode]) -> HashMap<String, GenericDefinition> {
    let mut definitions = HashMap::new();
    let mut plain_rules = HashMap::new();
    collect_generic_definitions_in_nodes(nodes, &mut definitions, &mut plain_rules);
    definitions
}

/// Recursive implementation of generic definition collection.
///
/// First-seen wins: a plain rule and a generic rule with the same base
/// name are both recorded so the post-pruning collision detector sees
/// both definitions.  The order is deterministic by source-text
/// position because the tree is walked top-to-bottom.
fn collect_generic_definitions_in_nodes(
    nodes: &[WrappedNode],
    definitions: &mut HashMap<String, GenericDefinition>,
    plain_rules: &mut HashMap<String, RuleDefinitionSite>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine {
                children,
                origin,
                span,
                ..
            } => {
                if let Some(definition) = rule_definition_from_ruleline(children, origin, span) {
                    match definition {
                        RuleDefinition::Plain(site) => {
                            plain_rules.entry(site.name.clone()).or_insert(site);
                        },
                        RuleDefinition::Generic(name, definition) => {
                            definitions.entry(name).or_insert(*definition);
                        },
                    }
                }
                collect_generic_definitions_in_nodes(children, definitions, plain_rules);
            },
            WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
                collect_generic_definitions_in_nodes(children, definitions, plain_rules);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// One plain rule definition site.
#[allow(dead_code, reason = "origin/span kept for diagnostic reuse")]
struct RuleDefinitionSite {
    /// Bare rule name.
    name: String,
    /// Source origin of the definition.
    origin: SourceOrigin,
    /// Source span of the definition.
    span: std::ops::Range<usize>,
}

/// Classification of a rule definition for generic collision detection.
enum RuleDefinition {
    /// A plain non-generic rule.
    Plain(RuleDefinitionSite),
    /// A generic rule.
    Generic(String, Box<GenericDefinition>),
}

/// Extract a rule definition from a rule line.
fn rule_definition_from_ruleline(
    children: &[WrappedNode],
    rule_origin: &SourceOrigin,
    rule_span: &std::ops::Range<usize>,
) -> Option<RuleDefinition> {
    let expr = children.iter().find_map(|child| {
        if let WrappedNode::Syntax { rule, children, .. } = child
            && rule == "expr"
        {
            return Some(children.as_slice());
        }
        None
    })?;

    let mut name = None;
    let mut params = None;
    let mut body = None;

    for child in expr {
        if let WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } = child
        {
            match rule.as_str() {
                "typename" | "groupname" if name.is_none() => {
                    name = Some(text.trim().to_owned());
                },
                "genericparm" => {
                    params = Some(generic_names(text, children));
                },
                "type" | "grpent" => {
                    body = Some(child.clone());
                },
                _ => {},
            }
        }
    }

    let name = name?;
    let origin = rule_origin.clone();
    let span = rule_span.clone();

    let Some(params) = params else {
        return Some(RuleDefinition::Plain(RuleDefinitionSite {
            name,
            origin,
            span,
        }));
    };
    if params.is_empty() {
        return Some(RuleDefinition::Plain(RuleDefinitionSite {
            name,
            origin,
            span,
        }));
    }

    Some(RuleDefinition::Generic(
        name,
        Box::new(GenericDefinition {
            params,
            body: body?,
            origin,
            span,
        }),
    ))
}

/// Expand generic instantiations recursively.
fn expand_node(
    node: &mut WrappedNode,
    definitions: &HashMap<String, GenericDefinition>,
    warnings: &mut Vec<Diagnostic>,
    stack: &mut Vec<String>,
) -> bool {
    if is_generic_ruleline(node) {
        return expand_nested_rule_children(node, definitions, warnings, stack);
    }

    if let Some(instantiation) = bare_type1_instantiation(node)
        && let Some(definition) = definitions.get(&instantiation.name)
    {
        if let Some(expanded) = expand_instantiation(
            instantiation,
            definition,
            definitions,
            warnings,
            node,
            stack,
        ) {
            *node = expanded;
        }
        return true;
    }

    if let Some(instantiation) = instantiation_from_node(node)
        && let Some(definition) = definitions.get(&instantiation.name)
    {
        if let Some(expanded) = expand_instantiation(
            instantiation,
            definition,
            definitions,
            warnings,
            node,
            stack,
        ) {
            *node = expanded;
        }
        return true;
    }

    match node {
        WrappedNode::RuleLine {
            text,
            span,
            children,
            ..
        }
        | WrappedNode::Syntax {
            text,
            span,
            children,
            ..
        } => {
            let original_text = text.clone();
            let parent_span = span.clone();
            let mut child_replacements = Vec::new();

            for child in children {
                let child_span = node_span(child);
                let child_text = node_text(child).to_owned();
                if expand_node(child, definitions, warnings, stack) {
                    child_replacements.push((child_span, child_text, node_text(child).to_owned()));
                }
            }

            if child_replacements.is_empty() {
                return false;
            }

            if let Some(updated) =
                apply_child_text_replacements(&original_text, &parent_span, child_replacements)
            {
                *text = updated;
            }
            true
        },
        WrappedNode::Directive { children, .. } => {
            let mut changed = false;
            for child in children {
                changed |= expand_node(child, definitions, warnings, stack);
            }
            changed
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => false,
    }
}

/// Generic rule lines are templates and must not be expanded in place.
/// Some parsed rule nodes can still contain following rule/directive
/// nodes as children, so skip the template expression but keep walking
/// nested definitions.
fn expand_nested_rule_children(
    node: &mut WrappedNode,
    definitions: &HashMap<String, GenericDefinition>,
    warnings: &mut Vec<Diagnostic>,
    stack: &mut Vec<String>,
) -> bool {
    let WrappedNode::RuleLine {
        text,
        span,
        children,
        ..
    } = node
    else {
        return false;
    };

    let original_text = text.clone();
    let parent_span = span.clone();
    let mut child_replacements = Vec::new();

    for child in children {
        let child_span = node_span(child);
        let child_text = node_text(child).to_owned();
        if expand_nested_rule_descendants(child, definitions, warnings, stack) {
            child_replacements.push((child_span, child_text, node_text(child).to_owned()));
        }
    }

    if child_replacements.is_empty() {
        return false;
    }

    if let Some(updated) =
        apply_child_text_replacements(&original_text, &parent_span, child_replacements)
    {
        *text = updated;
    }
    true
}

/// Walk only nested rule/directive descendants while skipping ordinary
/// syntax expressions. This keeps open generic templates symbolic but
/// still processes concrete definitions that the parser attached under
/// the same rule node.
fn expand_nested_rule_descendants(
    node: &mut WrappedNode,
    definitions: &HashMap<String, GenericDefinition>,
    warnings: &mut Vec<Diagnostic>,
    stack: &mut Vec<String>,
) -> bool {
    match node {
        WrappedNode::RuleLine { .. } => expand_node(node, definitions, warnings, stack),
        WrappedNode::Directive { children, .. } => {
            let mut changed = false;
            for child in children {
                changed |= expand_nested_rule_descendants(child, definitions, warnings, stack);
            }
            changed
        },
        WrappedNode::Syntax {
            text,
            span,
            children,
            ..
        } => {
            let original_text = text.clone();
            let parent_span = span.clone();
            let mut child_replacements = Vec::new();

            for child in children {
                let child_span = node_span(child);
                let child_text = node_text(child).to_owned();
                if expand_nested_rule_descendants(child, definitions, warnings, stack) {
                    child_replacements.push((child_span, child_text, node_text(child).to_owned()));
                }
            }

            if child_replacements.is_empty() {
                return false;
            }

            if let Some(updated) =
                apply_child_text_replacements(&original_text, &parent_span, child_replacements)
            {
                *text = updated;
            }
            true
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => false,
    }
}

/// Return whether this rule line declares generic parameters.
///
/// Generic definitions are templates. They must not be expanded in
/// place while their formal parameters are still symbolic; concrete
/// expansion happens when a non-definition call site instantiates them.
fn is_generic_ruleline(node: &WrappedNode) -> bool {
    let WrappedNode::RuleLine { children, .. } = node else {
        return false;
    };
    children.iter().any(rule_expr_has_genericparm)
}

/// Return whether a rule line's own `expr` declares generic parameters.
fn rule_expr_has_genericparm(node: &WrappedNode) -> bool {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return false;
    };
    if rule != "expr" {
        return false;
    }
    children
        .iter()
        .any(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "genericparm"))
}

/// Expand one instantiation against its generic definition.
fn expand_instantiation(
    instantiation: Instantiation,
    definition: &GenericDefinition,
    definitions: &HashMap<String, GenericDefinition>,
    warnings: &mut Vec<Diagnostic>,
    node: &WrappedNode,
    stack: &mut Vec<String>,
) -> Option<WrappedNode> {
    if stack.contains(&instantiation.name) {
        push_generic_error(
            warnings,
            node,
            format!("recursive generic expansion of `{}`", instantiation.name),
        );
        return None;
    }

    if definition.params.len() != instantiation.args.len() {
        push_generic_error(
            warnings,
            node,
            format!(
                "generic `{}` expected {} argument(s), got {}",
                instantiation.name,
                definition.params.len(),
                instantiation.args.len()
            ),
        );
        return None;
    }

    let mut replacements = HashMap::new();
    for (param, arg) in definition.params.iter().zip(instantiation.args) {
        replacements.insert(param.clone(), argument_as_type2(arg));
    }

    let mut expanded = body_for_call_site(node, &definition.body);
    substitute_params(&mut expanded, &replacements);
    reorigin_node(&mut expanded, node.origin().clone(), node_span(node));

    stack.push(instantiation.name);
    expand_node(&mut expanded, definitions, warnings, stack);
    stack.pop();

    Some(expanded)
}

/// One generic instantiation expression.
struct Instantiation {
    /// Generic definition name.
    name: String,
    /// Actual generic arguments.
    args: Vec<WrappedNode>,
}

/// Extract a bare `type1` whose only meaningful child is a generic instantiation.
fn bare_type1_instantiation(node: &WrappedNode) -> Option<Instantiation> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "type1" {
        return None;
    }

    let mut instantiation = None;
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if instantiation.is_none() => {
                    instantiation = instantiation_from_node(child);
                },
                "ctlop" | "rangeop" => return None,
                _ => {},
            }
        }
    }
    instantiation
}

/// Extract a direct `name<...>` instantiation from a `type2`/`grpent` node.
fn instantiation_from_node(node: &WrappedNode) -> Option<Instantiation> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "type2" && rule != "grpent" {
        return None;
    }

    let mut name = None;
    let mut args = None;

    for child in children {
        if let WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } = child
        {
            match rule.as_str() {
                "typename" | "groupname" => name = Some(text.trim().to_owned()),
                "genericarg" => args = Some(generic_arg_nodes(text, children)),
                _ => {},
            }
        }
    }

    Some(Instantiation {
        name: name?,
        args: args?,
    })
}

/// Extract the generic body at the same syntactic level as the call site.
fn body_for_call_site(
    call_site: &WrappedNode,
    body: &WrappedNode,
) -> WrappedNode {
    let WrappedNode::Syntax { rule, .. } = call_site else {
        return body.clone();
    };

    match rule.as_str() {
        "type1" => body_as_type1(body).unwrap_or_else(|| body.clone()),
        "type2" => body_as_type2(body).unwrap_or_else(|| body.clone()),
        "grpent" => body_as_grpent(body).unwrap_or_else(|| body.clone()),
        _ => body.clone(),
    }
}

/// Convert a generic body to a `type1` node for a type-expression call site.
fn body_as_type1(body: &WrappedNode) -> Option<WrappedNode> {
    if let Some(type_node) = direct_nested_rule(body, "type") {
        return direct_nested_rule(&type_node, "type1");
    }
    if let Some(group_entry) = body_as_grpent(body) {
        return parse_type1_argument(&format!("{{ {} }}", group_text_for_type(&group_entry)));
    }
    direct_nested_rule(body, "type1")
}

/// Convert a generic body to a `type2` node for a type operand call site.
fn body_as_type2(body: &WrappedNode) -> Option<WrappedNode> {
    if let Some(type1_node) = body_as_type1(body) {
        return Some(argument_as_type2(type1_node));
    }
    None
}

/// Convert a generic body to a `grpent` node for a group-entry call site.
fn body_as_grpent(body: &WrappedNode) -> Option<WrappedNode> {
    if let WrappedNode::Syntax { rule, .. } = body
        && rule == "grpent"
    {
        return Some(body.clone());
    }
    direct_nested_rule(body, "grpent")
}

/// Return group text suitable for embedding inside `{ ... }`.
fn group_text_for_type(group_entry: &WrappedNode) -> String {
    let text = node_text(group_entry).trim();
    if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        inner.trim().to_owned()
    } else {
        text.to_owned()
    }
}

/// Find a direct or expression-wrapped syntax node with the requested rule.
fn direct_nested_rule(
    node: &WrappedNode,
    target_rule: &str,
) -> Option<WrappedNode> {
    match node {
        WrappedNode::Syntax { rule, children, .. } => {
            if rule == target_rule {
                return Some(node.clone());
            }
            if rule == "expr" || rule == "type" || rule == "type1" || rule == "type2" {
                return children
                    .iter()
                    .find_map(|child| direct_nested_rule(child, target_rule));
            }
            None
        },
        WrappedNode::Directive { children, .. } => {
            children
                .iter()
                .find_map(|child| direct_nested_rule(child, target_rule))
        },
        WrappedNode::RuleLine { children, .. } => {
            children
                .iter()
                .find_map(|child| direct_nested_rule(child, target_rule))
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => None,
    }
}

/// Return the source text attached to a node.
fn node_text(node: &WrappedNode) -> &str {
    match node {
        WrappedNode::RuleLine { text, .. }
        | WrappedNode::Comment { text, .. }
        | WrappedNode::Syntax { text, .. } => text,
        WrappedNode::Directive { source_comment, .. } => source_comment,
        WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => "",
    }
}

/// Rewrite expanded generic provenance to the concrete call site.
fn reorigin_node(
    node: &mut WrappedNode,
    origin: SourceOrigin,
    span: std::ops::Range<usize>,
) {
    match node {
        WrappedNode::RuleLine {
            origin: node_origin,
            span: node_span,
            children,
            ..
        }
        | WrappedNode::Syntax {
            origin: node_origin,
            span: node_span,
            children,
            ..
        }
        | WrappedNode::Directive {
            origin: node_origin,
            span: node_span,
            children,
            ..
        } => {
            *node_origin = origin.clone();
            *node_span = span.clone();
            for child in children {
                reorigin_node(child, origin.clone(), span.clone());
            }
        },
        WrappedNode::Comment {
            origin: node_origin,
            span: node_span,
            ..
        } => {
            *node_origin = origin;
            *node_span = span;
        },
        WrappedNode::ModuleStart {
            origin: node_origin,
            ..
        }
        | WrappedNode::ModuleEnd {
            origin: node_origin,
            ..
        } => {
            *node_origin = origin;
        },
    }
}

/// Substitute formal parameter references in an expanded body.
///
/// When a bare parameter reference is replaced with its concrete
/// argument, the *parent* node still holds the original source text
/// containing the formal parameter name. Diagnostics render the parent
/// text verbatim, so a stale parent would surface the formal name (e.g.
/// `T`) even after substitution. To keep parents honest, this pass
/// also rewrites each parent's `text` by replacing the substituted
/// child's old text with the replacement's text, working depth-first
/// from the substituted leaf up to the body root.
fn substitute_params(
    node: &mut WrappedNode,
    replacements: &HashMap<String, WrappedNode>,
) -> bool {
    if let Some(param) = bare_param_reference(node)
        && let Some(replacement) = replacements.get(&param)
    {
        *node = replacement.clone();
        return true;
    }

    match node {
        WrappedNode::RuleLine {
            text,
            span,
            children,
            ..
        }
        | WrappedNode::Syntax {
            text,
            span,
            children,
            ..
        } => {
            let original_text = text.clone();
            let parent_span = span.clone();
            let mut child_replacements = Vec::new();

            for child in children {
                let child_span = node_span(child);
                let child_text = node_text(child).to_owned();
                if substitute_params(child, replacements) {
                    child_replacements.push((child_span, child_text, node_text(child).to_owned()));
                }
            }

            if child_replacements.is_empty() {
                return false;
            }

            if let Some(updated) =
                apply_child_text_replacements(&original_text, &parent_span, child_replacements)
            {
                *text = updated;
            }
            true
        },
        WrappedNode::Directive { children, .. } => {
            let mut changed = false;
            for child in children {
                changed |= substitute_params(child, replacements);
            }
            changed
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => false,
    }
}

/// Update a parent syntax text after one or more children were substituted.
///
/// Generic substitution must not blindly replace identifier strings in
/// the parent text: a formal parameter can share text with a member key
/// label, and only the child syntax span identifies the parameter
/// occurrence. Apply replacements from right to left so earlier byte
/// offsets remain valid.
fn apply_child_text_replacements(
    original_text: &str,
    parent_span: &std::ops::Range<usize>,
    mut replacements: Vec<(std::ops::Range<usize>, String, String)>,
) -> Option<String> {
    let mut updated = original_text.to_owned();
    replacements.sort_by_key(|(span, ..)| span.start);

    for (child_span, old_text, replacement_text) in replacements.into_iter().rev() {
        if child_span.start < parent_span.start || child_span.end > parent_span.end {
            replace_unambiguous_text(&mut updated, &old_text, &replacement_text)?;
            continue;
        }
        let start = child_span.start.saturating_sub(parent_span.start);
        let end = child_span.end.saturating_sub(parent_span.start);
        if start > end
            || end > updated.len()
            || !updated.is_char_boundary(start)
            || !updated.is_char_boundary(end)
        {
            replace_unambiguous_text(&mut updated, &old_text, &replacement_text)?;
            continue;
        }
        if updated.get(start..end) != Some(old_text.as_str()) {
            replace_unambiguous_text(&mut updated, &old_text, &replacement_text)?;
            continue;
        }
        updated.replace_range(start..end, &replacement_text);
    }

    Some(updated)
}

/// Replace `old_text` only when it occurs exactly once in `updated`.
///
/// Span-based edits are preferred because they distinguish a generic formal
/// from an identical member-key label. Alias wrapping can legitimately shift a
/// parent node's text before generic expansion runs; when the original child
/// span no longer indexes the same text, an exact single-occurrence replacement
/// is still safe. Ambiguous replacements are rejected instead of guessing.
fn replace_unambiguous_text(
    updated: &mut String,
    old_text: &str,
    replacement_text: &str,
) -> Option<()> {
    let mut matches = updated.match_indices(old_text);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let end = start.checked_add(old_text.len())?;
    updated.replace_range(start..end, replacement_text);
    Some(())
}

/// Return the parameter name for a bare parameter reference.
fn bare_param_reference(node: &WrappedNode) -> Option<String> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };
    if rule != "type2" && rule != "grpent" {
        return None;
    }

    let mut param = None;
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            match rule.as_str() {
                "typename" | "groupname" => param = Some(text.trim().to_owned()),
                "genericarg" => return None,
                _ => {},
            }
        }
    }
    param
}

/// Extract generic parameter names.
fn generic_names(
    text: &str,
    children: &[WrappedNode],
) -> Vec<String> {
    let from_children = children
        .iter()
        .filter_map(|child| {
            if let WrappedNode::Syntax { rule, text, .. } = child
                && rule == "id"
            {
                return Some(text.trim().to_owned());
            }
            None
        })
        .collect::<Vec<_>>();

    if !from_children.is_empty() {
        return from_children;
    }

    text.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Extract generic argument subtrees.
fn generic_arg_nodes(
    text: &str,
    children: &[WrappedNode],
) -> Vec<WrappedNode> {
    let from_children = children
        .iter()
        .filter(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .cloned()
        .collect::<Vec<_>>();

    if !from_children.is_empty() {
        return from_children;
    }

    generic_arg_texts(text)
        .into_iter()
        .filter_map(|arg| parse_type1_argument(&arg))
        .collect()
}

/// Split a generic argument list into top-level argument text.
fn generic_arg_texts(text: &str) -> Vec<String> {
    let inner = text
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if inner.is_empty() {
        return Vec::new();
    }

    let mut args = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut quote = None;
    let mut chars = inner.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if let Some(active_quote) = quote {
            if ch == '\\' {
                let _ = chars.next();
                continue;
            }
            if ch == active_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '<' | '(' | '[' | '{' => depth = depth.saturating_add(1),
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if let Some(arg) = inner.get(start..i) {
                    args.push(arg.trim().to_owned());
                }
                start = i.saturating_add(ch.len_utf8());
            },
            _ => {},
        }
    }

    if let Some(arg) = inner.get(start..) {
        args.push(arg.trim().to_owned());
    }
    args.into_iter().filter(|arg| !arg.is_empty()).collect()
}

/// Parse a generic argument as a `type1` subtree.
fn parse_type1_argument(text: &str) -> Option<WrappedNode> {
    let source = format!("__generic_arg = {text}\n");
    let pairs = parse_cddl(&source).ok()?;
    let pairs = process_ast(pairs).ok()?;
    let nodes = inject_directives(&PathBuf::from("<generic-arg>"), &pairs, &source).ok()?;

    nodes.iter().find_map(extract_first_rhs_type1)
}

/// Extract the first RHS `type1` node from a parsed helper document.
fn extract_first_rhs_type1(node: &WrappedNode) -> Option<WrappedNode> {
    match node {
        WrappedNode::RuleLine { children, .. } => children.iter().find_map(extract_first_rhs_type1),
        WrappedNode::Syntax { rule, children, .. } if rule == "expr" => {
            let mut lhs_seen = false;
            for child in children {
                if let WrappedNode::Syntax { rule, .. } = child {
                    match rule.as_str() {
                        "typename" | "groupname" if !lhs_seen => lhs_seen = true,
                        "type" | "grpent" if lhs_seen => {
                            return child_first_type1(child);
                        },
                        _ => {},
                    }
                }
            }
            None
        },
        WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
            children.iter().find_map(extract_first_rhs_type1)
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => None,
    }
}

/// Return the first direct or nested `type1` child.
fn child_first_type1(node: &WrappedNode) -> Option<WrappedNode> {
    match node {
        WrappedNode::Syntax { rule, children, .. } if rule == "type1" => Some(node.clone()),
        WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
            children.iter().find_map(child_first_type1)
        },
        _ => None,
    }
}

/// Convert a generic argument `type1` into a replacement `type2` when possible.
fn argument_as_type2(arg: WrappedNode) -> WrappedNode {
    let WrappedNode::Syntax { children, .. } = &arg else {
        return arg;
    };

    let mut type2 = None;
    let mut has_operator = false;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if type2.is_none() => type2 = Some(child.clone()),
                "ctlop" | "rangeop" => has_operator = true,
                _ => {},
            }
        }
    }

    if has_operator {
        arg
    } else {
        type2.unwrap_or(arg)
    }
}

/// Record a generic-expansion error.
fn push_generic_error(
    warnings: &mut Vec<Diagnostic>,
    node: &WrappedNode,
    message: String,
) {
    warnings.push(Diagnostic {
        code: "E012",
        level: DiagnosticLevel::Error,
        message,
        source_file: Some(node.origin().source_path.clone()),
        span: Some(node_span(node)),
        previous_origin: None,
        related: Vec::new(),
    });
}

/// Return a node span.
fn node_span(node: &WrappedNode) -> std::ops::Range<usize> {
    match node {
        WrappedNode::RuleLine { span, .. }
        | WrappedNode::Comment { span, .. }
        | WrappedNode::Syntax { span, .. }
        | WrappedNode::Directive { span, .. } => span.clone(),
        WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => 0..0,
    }
}
