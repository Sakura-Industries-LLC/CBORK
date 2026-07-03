// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Range-operator evaluation for semantic fixed-point resolution.

use crate::{
    ctlop::{child_text, resolve_type2_leaf},
    error::Diagnostic,
    node::{MetaData, SourceOrigin, WrappedNode},
    resolver_cache::{EntryState, ResolverCache},
    semantic::{handle_rangeop_error, push_metadata},
    symbols::{AssignmentKind, rule_head_from_children},
};

/// Walk the tree and resolve range operators (`..` and `...`) against
/// the cache.
pub(crate) fn rangeop_pass(
    node: &mut WrappedNode,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    match node {
        WrappedNode::RuleLine {
            children,
            metadata,
            origin,
            span,
            ..
        } => {
            rangeop_process_ruleline(children, metadata, origin, span, cache, warnings);
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                rangeop_pass(child, cache, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Walk a `RuleLine`'s children looking for rangeop patterns.
fn rangeop_process_ruleline(
    children: &mut [WrappedNode],
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    let should_resolve = rule_head_from_children(children)
        .is_none_or(|head| matches!(head.assignment, AssignmentKind::Define));
    if !should_resolve {
        return;
    }

    let mut type_name: Option<String> = None;
    rangeop_walk_children(
        children,
        &mut type_name,
        metadata,
        origin,
        span,
        cache,
        warnings,
    );
}

/// Recurse through child nodes extracting typename and evaluating rangeops.
fn rangeop_walk_children(
    children: &mut [WrappedNode],
    type_name: &mut Option<String>,
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    for child in children {
        match child {
            WrappedNode::Syntax { rule, .. } => {
                match rule.as_str() {
                    "typename" | "groupname" => {
                        *type_name = Some(child_text(child).to_owned());
                    },
                    "type1" => {
                        let name = type_name.clone();
                        if let Some(name) = name {
                            evaluate_rangeop(child, &name, metadata, origin, span, cache, warnings);
                        }
                    },
                    "expr" | "line" | "type" => {
                        if let WrappedNode::Syntax {
                            children: inner, ..
                        } = child
                        {
                            rangeop_walk_children(
                                inner, type_name, metadata, origin, span, cache, warnings,
                            );
                        }
                    },
                    _ => {
                        rangeop_pass(child, cache, warnings);
                    },
                }
            },
            _ => {
                rangeop_pass(child, cache, warnings);
            },
        }
    }
}

/// Try to evaluate a `type1` node as a range operation.
fn evaluate_rangeop(
    type1_node: &mut WrappedNode,
    name: &str,
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    if let WrappedNode::Syntax { children, .. } = type1_node {
        let mut lhs: Option<&WrappedNode> = None;
        let mut rangeop_found = false;
        let mut exclusive = false;
        let mut rhs: Option<&WrappedNode> = None;

        for child in children.iter() {
            if let WrappedNode::Syntax { rule, .. } = child {
                match rule.as_str() {
                    "type2" if !rangeop_found => lhs = Some(child),
                    "rangeop" => {
                        rangeop_found = true;
                        exclusive = child_text(child).contains("...");
                    },
                    "type2" => rhs = Some(child),
                    _ => {},
                }
            }
        }

        if let (Some(lhs), Some(rhs)) = (lhs, rhs)
            && rangeop_found
        {
            try_build_range(
                name, lhs, rhs, exclusive, metadata, origin, span, cache, warnings,
            );
        }
    }
}

/// Attempt to build a `RangeInt` or `RangeFloat` from two type2 nodes.
#[allow(clippy::too_many_arguments)]
fn try_build_range(
    name: &str,
    lhs: &WrappedNode,
    rhs: &WrappedNode,
    exclusive: bool,
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    let lhs_val = resolve_type2_leaf(lhs, cache);
    let rhs_val = resolve_type2_leaf(rhs, cache);

    match (lhs_val, rhs_val) {
        (Some(EntryState::Integer(l)), Some(EntryState::Integer(r))) => {
            let entry = EntryState::RangeInt {
                exclusive,
                min: l,
                max: r,
            };
            if let Err(e) = cache.resolve_with_origin(name, entry, Some(origin.clone())) {
                handle_rangeop_error(&e, name, metadata, origin, span, warnings);
            }
        },
        (Some(EntryState::Float(l)), Some(EntryState::Float(r))) => {
            let entry = EntryState::RangeFloat {
                exclusive,
                min: l,
                max: r,
            };
            if let Err(e) = cache.resolve_with_origin(name, entry, Some(origin.clone())) {
                handle_rangeop_error(&e, name, metadata, origin, span, warnings);
            }
        },
        _ => {
            push_metadata(metadata, MetaData::RangeTypeMismatch);
        },
    }
}
