// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Semantic seed-pass helpers.
//!
//! This module owns the literal-seeding pass and RHS classification used
//! to populate the initial semantic cache.

use crate::{
    error::Diagnostic,
    literals::{byte::ByteLiteralBytes, text::TextLiteralBytes},
    node::{MetaData, SourceOrigin, WrappedNode},
    resolver_cache::{EntryState, ResolverCache},
    semantic::{push_conflict_diagnostic, push_metadata, push_redundant_diagnostic},
    symbols::{AssignmentKind, rule_head_from_children},
};

/// Walk one node tree, seeding and propagating constants.
pub(crate) fn seed_pass(
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
            process_ruleline(children, metadata, origin, span, cache, warnings);
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                seed_pass(child, cache, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Process the children of a `RuleLine`, extracting typename and RHS.
fn process_ruleline(
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
    process_rule_children(
        children,
        &mut type_name,
        metadata,
        origin,
        span,
        cache,
        warnings,
    );
}

/// Process the nested rule children, extracting typename and dispatching to RHS
/// resolution.
fn process_rule_children(
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
            WrappedNode::Syntax { rule, text, .. } => {
                match rule.as_str() {
                    "typename" | "groupname" => {
                        *type_name = Some(text.trim().to_owned());
                        seed_pass(child, cache, warnings);
                    },
                    "type" | "grpent" => {
                        let name = type_name.clone();
                        if let Some(name) = name {
                            match try_resolve_rhs(child, &name, cache, origin) {
                                Ok(()) => {},
                                Err(ResolveStatus::Redundant) => {
                                    if push_metadata(metadata, MetaData::RedundantDefinition) {
                                        push_redundant_diagnostic(
                                            warnings,
                                            &name,
                                            origin,
                                            span,
                                            cache.origin(&name),
                                        );
                                    }
                                },
                                Err(ResolveStatus::Conflicting) => {
                                    // BUG-004 fix: a cache-level
                                    // conflict between two definitions
                                    // that come from independently
                                    // imported files (different
                                    // `source_path`) is not a
                                    // consumer-side collision.  Two
                                    // CBORK libraries can each use
                                    // the same private root name
                                    // (e.g. `all`) without any actual
                                    // collision in the consumer's
                                    // surface; the consumer's direct
                                    // references are independent of
                                    // each library's private root.
                                    // The diagnostic is still
                                    // emitted when the existing
                                    // entry and the incoming
                                    // definition share a source
                                    // path (genuine same-file
                                    // collision).
                                    let existing_origin = cache.origin(&name);
                                    let cross_library_conflict =
                                        existing_origin.as_ref().is_some_and(|existing| {
                                            existing.source_path != origin.source_path
                                        });
                                    if !cross_library_conflict
                                        && push_metadata(metadata, MetaData::ConflictingDefinition)
                                    {
                                        push_conflict_diagnostic(
                                            warnings,
                                            &name,
                                            origin,
                                            span,
                                            cache.origin(&name),
                                        );
                                    }
                                },
                            }
                        }
                    },
                    "expr" | "line" => {
                        if let WrappedNode::Syntax {
                            children: inner, ..
                        } = child
                        {
                            process_rule_children(
                                inner, type_name, metadata, origin, span, cache, warnings,
                            );
                        }
                    },
                    _ => {
                        seed_pass(child, cache, warnings);
                    },
                }
            },
            _ => {
                seed_pass(child, cache, warnings);
            },
        }
    }
}

/// Attempt to resolve the RHS of a rule against the cache.
fn try_resolve_rhs(
    type_node: &WrappedNode,
    name: &str,
    cache: &mut ResolverCache,
    origin: &SourceOrigin,
) -> Result<(), ResolveStatus> {
    let _ = cache.get(name);

    match classify_rhs(type_node) {
        RhsKind::Literal(entry) => {
            match cache.resolve_with_origin(name, entry, Some(origin.clone())) {
                Ok(()) => Ok(()),
                Err(crate::resolver_cache::CacheWriteError::RedundantType { .. }) => {
                    Err(ResolveStatus::Redundant)
                },
                Err(crate::resolver_cache::CacheWriteError::ConflictingType { .. }) => {
                    Err(ResolveStatus::Conflicting)
                },
                Err(_e) => Ok(()),
            }
        },
        RhsKind::Reference(ref target) => {
            let _ = cache.get(target);
            if let Some(state) = cache_is_resolved(target, cache) {
                match cache.resolve_with_origin(name, state, Some(origin.clone())) {
                    Ok(()) => Ok(()),
                    Err(crate::resolver_cache::CacheWriteError::RedundantType { .. }) => {
                        Err(ResolveStatus::Redundant)
                    },
                    Err(crate::resolver_cache::CacheWriteError::ConflictingType { .. }) => {
                        Err(ResolveStatus::Conflicting)
                    },
                    Err(_e) => Ok(()),
                }
            } else {
                Ok(())
            }
        },
        RhsKind::Complex => Ok(()),
        RhsKind::Error(_msg) => Ok(()),
    }
}

/// A semantic status reported by constant seeding.
#[derive(Debug)]
enum ResolveStatus {
    /// The node was already known with the same value.
    Redundant,
    /// The node conflicts with an earlier value.
    Conflicting,
}

/// Return a clone of the resolved state for `name`, or `None`.
fn cache_is_resolved(
    name: &str,
    cache: &ResolverCache,
) -> Option<EntryState> {
    if cache.is_resolved(name) {
        for (key, state) in cache.iter() {
            if key == name && state.is_resolved() {
                return Some(state.clone());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// RHS classification
// ---------------------------------------------------------------------------

/// What the RHS of a rule reduces to for constant-propagation purposes.
enum RhsKind {
    /// A directly-known literal value.
    Literal(EntryState),
    /// A reference to another typename.
    Reference(String),
    /// Contains choices, control operators, or other non-simple structure.
    Complex,
    /// The RHS is syntactically malformed (e.g. unquoted text literal).
    Error(String),
}

/// Classify a `type` or `grpent` subtree.
fn classify_rhs(node: &WrappedNode) -> RhsKind {
    match node {
        WrappedNode::Syntax { rule, children, .. } => {
            match rule.as_str() {
                "type" | "type1" | "type2" | "grpent" => {
                    let leaf = find_rhs_leaf(children);
                    match leaf {
                        Some(RhsLeaf::Value(node)) => parse_value(&node),
                        Some(RhsLeaf::TypeName(name)) => RhsKind::Reference(name),
                        Some(RhsLeaf::Complex) | None => RhsKind::Complex,
                    }
                },
                _ => RhsKind::Complex,
            }
        },
        _ => RhsKind::Complex,
    }
}

/// A leaf node found at the bottom of a type subtree.
enum RhsLeaf {
    /// A literal value node.
    Value(WrappedNode),
    /// A bare typename reference.
    TypeName(String),
    /// Something else (group, paren expression, etc.).
    Complex,
}

/// Descend the RHS tree looking for the single leaf.
fn find_rhs_leaf(children: &[WrappedNode]) -> Option<RhsLeaf> {
    let type1s: Vec<&WrappedNode> = children
        .iter()
        .filter(|n| matches!(n, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .collect();

    if type1s.len() > 1 {
        return Some(RhsLeaf::Complex);
    }

    if let Some(type1) = type1s.first() {
        return find_type1_leaf(type1);
    }

    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            match rule.as_str() {
                "value" => return Some(RhsLeaf::Value(child.clone())),
                "typename" => {
                    return Some(RhsLeaf::TypeName(text.trim().to_owned()));
                },
                "type1" | "type2" => {
                    return find_type1_leaf(child);
                },
                _ => {},
            }
        }
    }

    None
}

/// Descend into a `type1` node.
fn find_type1_leaf(node: &WrappedNode) -> Option<RhsLeaf> {
    if let WrappedNode::Syntax { children, .. } = node {
        let mut found_type2 = false;
        let mut result = None;

        for child in children {
            if let WrappedNode::Syntax { rule, .. } = child {
                match rule.as_str() {
                    "type2" => {
                        found_type2 = true;
                        result = find_type2_leaf(child);
                    },
                    "ctlop" | "rangeop" => {
                        return Some(RhsLeaf::Complex);
                    },
                    _ => {},
                }
            }
        }

        if found_type2 {
            return result;
        }
    }
    None
}

/// Descend into a `type2` node.
fn find_type2_leaf(node: &WrappedNode) -> Option<RhsLeaf> {
    if let WrappedNode::Syntax { children, .. } = node {
        for child in children {
            if let WrappedNode::Syntax { rule, text, .. } = child {
                match rule.as_str() {
                    "value" => return Some(RhsLeaf::Value(child.clone())),
                    "typename" => {
                        return Some(RhsLeaf::TypeName(text.trim().to_owned()));
                    },
                    _ => {},
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Literal parsing
// ---------------------------------------------------------------------------

/// Parse a `value` subtree into an [`EntryState`].
fn parse_value(node: &WrappedNode) -> RhsKind {
    if let WrappedNode::Syntax { children, .. } = node {
        for child in children {
            if let WrappedNode::Syntax { rule, text, .. } = child {
                return match rule.as_str() {
                    "uint" => parse_uint(text),
                    "int" => parse_int(text),
                    "intfloat" => parse_intfloat(text),
                    "hexfloat" => parse_hexfloat(text),
                    "number" => parse_number(text),
                    "text" => parse_text(text),
                    "bytes" => parse_bytes(text),
                    _ => RhsKind::Complex,
                };
            }
        }
    }
    RhsKind::Complex
}

/// Parse a `uint` literal into an Integer entry.  Supports the
/// CDDL `uint` grammar: decimal (`0` or non-zero leading digit
/// followed by decimal digits), `0x` hexadecimal, and `0b` binary.
/// The caller has already stripped the optional `-` sign for
/// `int` literals, so the magnitude is parsed here.
fn parse_uint(text: &str) -> RhsKind {
    let trimmed = text.trim();
    let body = if let Some(rest) = stripped_prefix(trimmed, "0x") {
        match u128::from_str_radix(rest, 16) {
            Ok(v) => v.cast_signed(),
            Err(_) => return RhsKind::Complex,
        }
    } else if let Some(rest) = stripped_prefix(trimmed, "0b") {
        match u128::from_str_radix(rest, 2) {
            Ok(v) => v.cast_signed(),
            Err(_) => return RhsKind::Complex,
        }
    } else {
        match trimmed.parse::<i128>() {
            Ok(v) => v,
            Err(_) => return RhsKind::Complex,
        }
    };
    RhsKind::Literal(EntryState::Integer(body))
}

/// Parse an `int` literal: a sign followed by the CDDL `uint`
/// grammar.  A leading `-` is folded into a single negation so
/// `0x10` becomes 16, `-0x10` becomes -16, `0b1010` becomes 10,
/// and `-0b1010` becomes -10.
fn parse_int(text: &str) -> RhsKind {
    let trimmed = text.trim();
    let (sign, rest) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (-1_i128, stripped)
    } else {
        (1_i128, trimmed)
    };
    match parse_uint(rest) {
        RhsKind::Literal(EntryState::Integer(magnitude)) => {
            // Match `i128::checked_neg` semantics: any non-zero
            // magnitude may be negated; `0` stays `0` so the result
            // cannot overflow even for `i128::MIN` (which is
            // representable as a negative literal in CDDL only
            // through the broader semantic layer, not the seed).
            let signed = magnitude.checked_mul(sign).unwrap_or(magnitude);
            RhsKind::Literal(EntryState::Integer(signed))
        },
        other => other,
    }
}

/// Parse an intfloat literal (decimal int or float).
fn parse_intfloat(text: &str) -> RhsKind {
    let trimmed = text.trim();
    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
        match trimmed.parse::<f64>() {
            Ok(v) => RhsKind::Literal(EntryState::Float(v)),
            Err(_) => RhsKind::Complex,
        }
    } else {
        parse_uint(trimmed)
    }
}

/// Parse a CDDL hexfloat literal.  The CDDL `hexfloat` grammar is
/// `-? 0x <hexdigits> ( . <hexdigits> )? p <exponent>`.  The `p`
/// exponent is a *binary* exponent, not a decimal one, so
/// `0x1.fp+2` is `1.9375 * 2^2 = 7.75`.  We require the `0x`
/// prefix, the `p` exponent marker, and an `+/-?` decimal
/// exponent; otherwise we return [`RhsKind::Complex`] so the
/// downstream cache treats the value as opaque.
fn parse_hexfloat(text: &str) -> RhsKind {
    let trimmed = text.trim();
    let (sign, rest) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (-1.0_f64, stripped)
    } else {
        (1.0_f64, trimmed)
    };
    let Some(hex_body) = stripped_prefix(rest, "0x") else {
        return RhsKind::Complex;
    };
    // The body must contain a `p` exponent marker.  Split into the
    // mantissa and exponent parts.
    let Some((mantissa, exp_str)) = hex_body.split_once('p') else {
        return RhsKind::Complex;
    };
    let mantissa = mantissa.trim();
    if mantissa.is_empty() {
        return RhsKind::Complex;
    }
    let exp: i32 = match exp_str.trim().parse() {
        Ok(v) => v,
        Err(_) => return RhsKind::Complex,
    };
    // Mantissa may be `H` or `H.H` where `H` is one or more hex
    // digits.  Anything else (including a leading `.` or a
    // trailing `.` without digits, or non-hex characters) is
    // rejected as Complex rather than guessed.
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    if int_part.is_empty() || !int_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return RhsKind::Complex;
    }
    if let Some(f) = frac_part
        && (f.is_empty() || !f.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return RhsKind::Complex;
    }
    // Compute the value: int_part as a hex integer plus frac_part
    // as a hex fraction in [0, 1), then multiply by 2^exp.
    let int_value: f64 = match u128::from_str_radix(int_part, 16) {
        // The cast is lossy above 2^53; for values that large the
        // CDDL literal would not be a representable IEEE 754
        // double anyway, so we let the precision loss stand and
        // let the caller observe it via the resulting `f64`.
        Ok(v) => f64_from_u128(v),
        Err(_) => return RhsKind::Complex,
    };
    let frac_value: f64 = match frac_part {
        Some(f) => {
            // The fraction length is at most a few hex digits in
            // any practical CDDL literal; saturate rather than
            // panic if a 32-bit platform ever sees an absurdly
            // long fraction.
            let digits: i32 = match f.len().try_into() {
                Ok(v) => v,
                Err(_) => return RhsKind::Complex,
            };
            let raw: f64 = match u128::from_str_radix(f, 16) {
                Ok(v) => f64_from_u128(v),
                Err(_) => return RhsKind::Complex,
            };
            // Each hex digit to the right of the point is worth
            // 1 / 16^(position + 1).  Position 0 is the leftmost
            // digit.  Use a 4-bit shift per digit for numerical
            // stability when the fraction is short.
            raw / 16_f64.powi(digits)
        },
        None => 0.0,
    };
    let mantissa_value = sign * (int_value + frac_value);
    let result = mantissa_value * 2_f64.powi(exp);
    if !result.is_finite() {
        return RhsKind::Complex;
    }
    RhsKind::Literal(EntryState::Float(result))
}

/// Parse a `number` literal.  Routes to the hexfloat parser only
/// when the value contains a `p` (or `P`) binary-exponent marker;
/// a bare `0x...` literal is a hex *integer* and a bare `0b...`
/// literal is a binary *integer*; both are delegated to the
/// integer parsers with any sign intact.
fn parse_number(text: &str) -> RhsKind {
    let trimmed = text.trim();
    let body = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let body_lower = body.to_ascii_lowercase();
    if body_lower.starts_with("0x") {
        if body_lower.contains('p') {
            return parse_hexfloat(trimmed);
        }
        return parse_int(trimmed);
    }
    if body_lower.starts_with("0b") {
        return parse_int(trimmed);
    }
    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
        parse_intfloat(trimmed)
    } else {
        parse_uint(trimmed)
    }
}

/// Strip a literal prefix and the character that must follow it
/// (so `"0x10"` and `"0x10 "` both start with `10` / `10 `).  The
/// parser is intentionally strict: anything that is not the prefix
/// or that has additional junk immediately after the prefix is
/// rejected as Complex.
fn stripped_prefix<'a>(
    text: &'a str,
    prefix: &str,
) -> Option<&'a str> {
    let after = text.strip_prefix(prefix)?;
    // A digit or a sign must follow immediately for the prefix to
    // count as a true radix marker; otherwise the text is just a
    // literal that happens to start with the bytes "0x" / "0b".
    let next = after.chars().next()?;
    if next.is_ascii_alphanumeric() || next == '+' || next == '-' || next == '.' {
        Some(after)
    } else {
        None
    }
}

/// Lossy `u128` → `f64` cast used by the hexfloat parser.  The
/// mantissa is at most 52 bits wide; values that exceed that
/// precision are saturated to `f64::INFINITY` so the result is
/// guaranteed finite-or-infinite, never a wrapped-around finite
/// value that would mask a real precision loss.
#[allow(
    clippy::cast_precision_loss,
    reason = "precision loss is intentional; we saturate the \
              unrepresentable range to infinity first"
)]
fn f64_from_u128(v: u128) -> f64 {
    if v > (1_u128 << 127) {
        f64::INFINITY
    } else {
        v as f64
    }
}

/// Parse a `text` literal into a Text entry.
fn parse_text(text: &str) -> RhsKind {
    let trimmed = text.trim();
    match TextLiteralBytes::parse(trimmed.as_bytes()) {
        Ok(tlb) => RhsKind::Literal(EntryState::Text(tlb)),
        Err(_e) => RhsKind::Error(format!("invalid text literal: {trimmed}")),
    }
}

/// Parse a `bytes` literal into a Bytes entry.
fn parse_bytes(text: &str) -> RhsKind {
    let trimmed = text.trim();
    match ByteLiteralBytes::parse(trimmed.as_bytes()) {
        Ok(blb) => RhsKind::Literal(EntryState::Bytes(blb)),
        Err(_e) => RhsKind::Error(format!("invalid bytes literal: {trimmed}")),
    }
}
