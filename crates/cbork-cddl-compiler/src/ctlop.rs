// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Control-operator evaluation for semantic fixed-point resolution.

use cbork_abnf_parser::parse_abnf;

use crate::{
    error::Diagnostic,
    literals::{
        array::LiteralArray, byte::ByteLiteralBytes, regex::RegexLiteral, text::TextLiteralBytes,
    },
    node::{MetaData, SourceOrigin, WrappedNode},
    resolver_cache::{CompressionKind, EntryState, ResolverCache},
    semantic::{handle_rangeop_error, push_metadata, remove_metadata},
    symbols::{AssignmentKind, rule_head_from_children},
};

/// Walk the tree and evaluate control operators against currently-known
/// values in the cache.
///
/// For a `type1` node containing a `ctlop` and two `type2` children,
/// if both `type2` leaves resolve to concrete numeric values and the
/// operator is known, the result is computed and stored.
pub(crate) fn ctlop_pass(
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
            ctlop_process_ruleline(children, metadata, origin, span, cache, warnings);
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                ctlop_pass(child, cache, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Validate control operators after the tree has been finalized.
///
/// This replays ctlop evaluation against a scratch cache that excludes the
/// current target entry, so successful evaluations can be promoted into the
/// final cache without tripping redundant-write checks.
pub(crate) fn validate_ctlop_pass(
    nodes: &mut [WrappedNode],
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        validate_ctlop_visit(node, cache, warnings);
    }
}

/// Validate one node or recursively descend into its children.
fn validate_ctlop_visit(
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
            validate_ctlop_process_ruleline(children, metadata, origin, span, cache, warnings);
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                validate_ctlop_visit(child, cache, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Walk a `RuleLine` and validate any ctlop patterns it contains.
fn validate_ctlop_process_ruleline(
    children: &mut [WrappedNode],
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    cache: &mut ResolverCache,
    warnings: &mut Vec<Diagnostic>,
) {
    let promote_result = rule_head_from_children(children)
        .is_none_or(|head| matches!(head.assignment, AssignmentKind::Define));

    let mut type_name: Option<String> = None;
    let mut ctx = CtlopCtx {
        metadata,
        origin,
        span,
        cache,
        warnings,
    };
    validate_ctlop_walk_children(children, &mut type_name, &mut ctx, promote_result);
}

/// Validation context bundling common ctlop evaluation state.
struct CtlopCtx<'a> {
    /// Metadata accumulated during validation.
    metadata: &'a mut Vec<MetaData>,
    /// Source location information.
    origin: &'a SourceOrigin,
    /// Byte range in the source file.
    span: &'a std::ops::Range<usize>,
    /// Resolver cache for semantic analysis.
    cache: &'a mut ResolverCache,
    /// Diagnostics collected during the pass.
    warnings: &'a mut Vec<Diagnostic>,
}

/// Recurse through child nodes extracting typename and validating ctlops.
fn validate_ctlop_walk_children(
    children: &mut [WrappedNode],
    type_name: &mut Option<String>,
    ctx: &mut CtlopCtx<'_>,
    promote_result: bool,
) {
    for child in children {
        match child {
            WrappedNode::Syntax { rule, .. } => {
                match rule.as_str() {
                    "typename" | "groupname" => {
                        *type_name = Some(child_text(child).trim().to_owned());
                    },
                    "type1" => {
                        let name = type_name.clone();
                        if let Some(name) = name {
                            validate_ctlop_node(child, &name, ctx, promote_result);
                        }
                    },
                    "expr" | "line" | "type" => {
                        if let WrappedNode::Syntax {
                            children: inner, ..
                        } = child
                        {
                            validate_ctlop_walk_children(inner, type_name, ctx, promote_result);
                        }
                    },
                    _ => {
                        validate_ctlop_visit(child, ctx.cache, ctx.warnings);
                    },
                }
            },
            _ => {
                validate_ctlop_visit(child, ctx.cache, ctx.warnings);
            },
        }
    }
}

/// Validate one ctlop-bearing `type1` node against a scratch cache.
fn validate_ctlop_node(
    type1_node: &mut WrappedNode,
    name: &str,
    ctx: &mut CtlopCtx<'_>,
    promote_result: bool,
) {
    let WrappedNode::Syntax { children, .. } = type1_node else {
        return;
    };

    let mut lhs: Option<&WrappedNode> = None;
    let mut ctlop_text: Option<&str> = None;
    let mut rhs: Option<&WrappedNode> = None;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if lhs.is_none() => lhs = Some(child),
                "ctlop" => ctlop_text = Some(child_text(child).trim()),
                "type2" => rhs = Some(child),
                _ => {},
            }
        }
    }

    let (Some(lhs), Some(op), Some(rhs)) = (lhs, ctlop_text, rhs) else {
        return;
    };

    if !promote_result {
        if !validate_ctlop_semantics(op, lhs, rhs, ctx.cache) {
            push_metadata(ctx.metadata, MetaData::CtlopTypeMismatch);
        }
        return;
    }

    let mut scratch = clone_cache_without_target(ctx.cache, name);
    let before = scratch.is_resolved(name);
    let mut ctlop = CtlOp {
        name,
        lhs,
        op,
        rhs,
        metadata: ctx.metadata,
        origin: ctx.origin,
        span: ctx.span,
        cache: &mut scratch,
        warnings: ctx.warnings,
    };
    ctlop.try_eval_ctlop();

    let after = scratch.is_resolved(name);
    let structurally_valid = validate_ctlop_semantics(op, lhs, rhs, ctx.cache);

    if after && structurally_valid {
        if !before && let Some(state) = cache_lookup(name, &scratch) {
            let origin = scratch.origin(name).cloned();
            drop(ctx.cache.resolve_with_origin(name, state, origin));
        }
        let _ = remove_metadata(ctx.metadata, MetaData::CtlopTypeMismatch);
        return;
    }

    if structurally_valid {
        let _ = remove_metadata(ctx.metadata, MetaData::CtlopTypeMismatch);
        return;
    }

    if !ctx.metadata.contains(&MetaData::CtlopTypeMismatch) {
        push_metadata(ctx.metadata, MetaData::CtlopTypeMismatch);
    }
    ctx.warnings.push(Diagnostic {
        code: "E015",
        level: crate::error::DiagnosticLevel::Error,
        message: format!("control operator `{op}` on `{name}` could not be validated"),
        source_file: Some(ctx.origin.source_path.clone()),
        span: Some(ctx.span.clone()),
        previous_origin: None,
        related: Vec::new(),
    });
}

/// Walk a `RuleLine`'s children looking for ctlop patterns.
fn ctlop_process_ruleline(
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
    ctlop_walk_children(
        children,
        &mut type_name,
        metadata,
        origin,
        span,
        cache,
        warnings,
    );
}

/// Recurse through child nodes extracting typename and evaluating ctlops.
fn ctlop_walk_children(
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
                            evaluate_ctlop(child, &name, metadata, origin, span, cache, warnings);
                        }
                    },
                    "expr" | "line" | "type" => {
                        if let WrappedNode::Syntax {
                            children: inner, ..
                        } = child
                        {
                            ctlop_walk_children(
                                inner, type_name, metadata, origin, span, cache, warnings,
                            );
                        }
                    },
                    _ => {
                        ctlop_pass(child, cache, warnings);
                    },
                }
            },
            _ => {
                ctlop_pass(child, cache, warnings);
            },
        }
    }
}

/// Try to evaluate a `type1` node as a control operator expression.
fn evaluate_ctlop(
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
        let mut ctlop_text: Option<&str> = None;
        let mut rhs: Option<&WrappedNode> = None;

        for child in children {
            if let WrappedNode::Syntax { rule, .. } = child {
                match rule.as_str() {
                    "type2" if lhs.is_none() => lhs = Some(child),
                    "ctlop" => ctlop_text = Some(child_text(child).trim()),
                    "type2" => rhs = Some(child),
                    _ => {},
                }
            }
        }

        if let (Some(lhs), Some(op), Some(rhs)) = (lhs, ctlop_text, rhs) {
            let mut ctlop = CtlOp {
                name,
                lhs,
                op,
                rhs,
                metadata,
                origin,
                span,
                cache,
                warnings,
            };
            ctlop.try_eval_ctlop();
        }
    }
}

/// Control-operator evaluation context.
struct CtlOp<'a> {
    /// The type name being resolved.
    name: &'a str,
    /// Left-hand side AST node.
    lhs: &'a WrappedNode,
    /// The control operator text (e.g. `.plus`).
    op: &'a str,
    /// Right-hand side AST node.
    rhs: &'a WrappedNode,
    /// Metadata for the owning node.
    metadata: &'a mut Vec<MetaData>,
    /// Source location of the definition.
    origin: &'a SourceOrigin,
    /// Byte span of the definition.
    span: &'a std::ops::Range<usize>,
    /// Resolution cache.
    cache: &'a mut ResolverCache,
    /// Warning/error diagnostics.
    warnings: &'a mut Vec<Diagnostic>,
}

impl CtlOp<'_> {
    /// Attempt to evaluate the control operator represented by this context.
    #[allow(clippy::cast_precision_loss)]
    fn try_eval_ctlop(&mut self) {
        let lhs_val = resolve_type2_leaf(self.lhs, self.cache);
        let rhs_val = resolve_type2_leaf(self.rhs, self.cache);

        let result = match self.op {
            ".cat" | ".det" => self.try_eval_cat_det(lhs_val, rhs_val),
            ".b64u" | ".b64u-sloppy" | ".b64c" | ".b64c-sloppy" | ".hex" | ".hexlc" | ".hexuc"
            | ".b32" | ".h32" | ".b45" => self.try_eval_encoding(rhs_val.as_ref()),
            ".base10" => self.try_eval_base10(rhs_val.as_ref()),
            ".abnf" | ".abnfb" => self.try_eval_abnf(),
            ".x-enc.abnf" | ".x-enc.abnfb" => self.try_eval_abnf_annotation(EntryState::EncAbnf),
            ".x-hash.abnf" | ".x-hash.abnfb" => self.try_eval_abnf_annotation(EntryState::HashAbnf),
            ".x-compressed.abnf" | ".x-compressed.abnfb" => {
                self.try_eval_abnf_compression(CompressionKind::Compressed)
            },
            ".x-brotli.abnf" | ".x-brotli.abnfb" => {
                self.try_eval_abnf_compression(CompressionKind::Brotli)
            },
            ".x-zstd.abnf" | ".x-zstd.abnfb" => {
                self.try_eval_abnf_compression(CompressionKind::Zstd)
            },
            ".x-gzip.abnf" | ".x-gzip.abnfb" => {
                self.try_eval_abnf_compression(CompressionKind::Gzip)
            },
            ".x-deflate.abnf" | ".x-deflate.abnfb" => {
                self.try_eval_abnf_compression(CompressionKind::Deflate)
            },
            ".regexp" => self.try_eval_regexp(),
            ".json" => self.try_eval_json(rhs_val.as_ref()),
            ".join" => self.try_eval_join(),
            ".printf" => self.try_eval_printf(),
            ".plus" => Self::try_eval_plus(lhs_val, rhs_val),
            _ => None,
        };

        if let Some(entry) = result
            && let Err(e) =
                self.cache
                    .resolve_with_origin(self.name, entry, Some(self.origin.clone()))
        {
            handle_rangeop_error(
                &e,
                self.name,
                self.metadata,
                self.origin,
                self.span,
                self.warnings,
            );
        }
    }

    /// Evaluate `.cat` / `.det` control operators.
    fn try_eval_cat_det(
        &mut self,
        lhs_val: Option<EntryState>,
        rhs_val: Option<EntryState>,
    ) -> Option<EntryState> {
        let lhs_is_bytes = matches!(&lhs_val, Some(EntryState::Bytes(_)));
        let rhs_is_bytes = matches!(&rhs_val, Some(EntryState::Bytes(_)));

        let lhs_text = entry_to_text(lhs_val, self.metadata);
        let rhs_text = entry_to_text(rhs_val, self.metadata);
        match (lhs_text, rhs_text) {
            (Some(l), Some(r)) => {
                let result_text = if self.op == ".cat" {
                    l.cat(&r)
                } else {
                    l.det(&r)
                };
                if lhs_is_bytes || rhs_is_bytes {
                    Some(EntryState::Bytes(ByteLiteralBytes::from(result_text)))
                } else {
                    Some(EntryState::Text(result_text))
                }
            },
            _ => None,
        }
    }

    /// Evaluate encoding operators (.b64u, .hex, etc.) applied to text.
    fn try_eval_encoding(
        &mut self,
        rhs_val: Option<&EntryState>,
    ) -> Option<EntryState> {
        match rhs_val.as_ref() {
            Some(EntryState::Bytes(b)) => {
                let result_text = match self.op {
                    ".b64u" | ".b64u-sloppy" => b.to_b64u(),
                    ".b64c" | ".b64c-sloppy" => b.to_b64c(),
                    ".hex" | ".hexlc" => b.to_hexlc(),
                    ".hexuc" => b.to_hexuc(),
                    ".b32" => b.to_b32(),
                    ".h32" => b.to_h32(),
                    ".b45" => b.to_b45(),
                    _ => return None,
                };
                Some(EntryState::Text(result_text))
            },
            Some(_) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
            None => None,
        }
    }

    /// Evaluate `.base10` conversion.
    fn try_eval_base10(
        &mut self,
        rhs_val: Option<&EntryState>,
    ) -> Option<EntryState> {
        match rhs_val.as_ref() {
            Some(EntryState::Integer(i)) => {
                Some(EntryState::Text(TextLiteralBytes::from_base10(*i)))
            },
            Some(_) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
            None => None,
        }
    }

    /// Evaluate `.json` validation.
    fn try_eval_json(
        &mut self,
        rhs_val: Option<&EntryState>,
    ) -> Option<EntryState> {
        match rhs_val.as_ref() {
            Some(EntryState::Text(t)) => {
                if t.validate_json().is_ok() {
                    Some(EntryState::Text(t.clone()))
                } else {
                    push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                    None
                }
            },
            Some(_) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
            None => None,
        }
    }

    /// Evaluate the `.abnf` / `.abnfb` operator.
    fn try_eval_abnf(&mut self) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "text" && target != "bytes" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let rhs_text = abnf_controller_text(self.rhs, self.cache, self.metadata)?;
        let source = match std::str::from_utf8(rhs_text.as_ref()) {
            Ok(source) => source,
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                return None;
            },
        };

        match parse_abnf(source) {
            Ok(document) => Some(EntryState::Abnf(Box::new(document))),
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
        }
    }

    /// Evaluate unofficial annotated ABNF wrappers such as `.x-enc.abnf` and
    /// `.x-hash.abnf`.
    fn try_eval_abnf_annotation(
        &mut self,
        ctor: fn(Box<cbork_abnf_parser::AbnfDocument>) -> EntryState,
    ) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "bytes" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let rhs_text = abnf_controller_text(self.rhs, self.cache, self.metadata)?;
        let source = match std::str::from_utf8(rhs_text.as_ref()) {
            Ok(source) => source,
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                return None;
            },
        };

        match parse_abnf(source) {
            Ok(document) => Some(ctor(Box::new(document))),
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
        }
    }

    /// Evaluate unofficial annotated ABNF compression wrappers such as
    /// `.x-brotli.abnf` and `.x-compressed.abnf`.
    ///
    /// The carrier must be `bytes`; the RHS resolves to text.  The
    /// resulting [`EntryState::CompressionAbnf`] records the
    /// compression algorithm kind and the parsed ABNF document so
    /// downstream passes can reverse the compression and continue
    /// validating the inner payload.
    fn try_eval_abnf_compression(
        &mut self,
        kind: CompressionKind,
    ) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "bytes" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let rhs_text = abnf_controller_text(self.rhs, self.cache, self.metadata)?;
        let source = match std::str::from_utf8(rhs_text.as_ref()) {
            Ok(source) => source,
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                return None;
            },
        };

        match parse_abnf(source) {
            Ok(document) => {
                Some(EntryState::CompressionAbnf {
                    kind,
                    document: Box::new(document),
                })
            },
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
        }
    }

    /// Evaluate the `.regexp` operator.
    fn try_eval_regexp(&mut self) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "text" && target != "bytes" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let rhs_text = abnf_controller_text(self.rhs, self.cache, self.metadata)?;
        match RegexLiteral::parse(rhs_text.as_ref()) {
            Ok(regex) => Some(EntryState::Regex(Box::new(regex))),
            Err(_e) => {
                push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
                None
            },
        }
    }

    /// Evaluate the `.join` operator.
    fn try_eval_join(&mut self) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "text" && target != "bytes" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let Ok(Some(array)) = LiteralArray::new(self.rhs, self.cache) else {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        };

        let result = if target == "text" {
            array.join_text().map(EntryState::Text)
        } else {
            array.try_join_bytes().map(EntryState::Bytes)
        };

        if let Ok(entry) = result {
            Some(entry)
        } else {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            None
        }
    }

    /// Evaluate the `.printf` operator.
    fn try_eval_printf(&mut self) -> Option<EntryState> {
        let target = child_text(self.lhs).trim();
        if target != "text" {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        }

        let Ok(Some(array)) = LiteralArray::new(self.rhs, self.cache) else {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            return None;
        };

        if let Ok(text) = array.printf() {
            Some(EntryState::Text(text))
        } else {
            push_metadata(self.metadata, MetaData::CtlopTypeMismatch);
            None
        }
    }

    /// Evaluate the `.plus` operator.
    fn try_eval_plus(
        lhs_val: Option<EntryState>,
        rhs_val: Option<EntryState>,
    ) -> Option<EntryState> {
        match (lhs_val, rhs_val) {
            (Some(EntryState::Integer(l)), Some(EntryState::Integer(r))) => {
                l.checked_add(r).map(EntryState::Integer)
            },
            (Some(EntryState::Integer(l)), Some(EntryState::Float(r))) =>
            {
                #[allow(clippy::cast_precision_loss, reason = "acceptable")]
                Some(EntryState::Float(l as f64 + r))
            },
            (Some(EntryState::Float(l)), Some(EntryState::Integer(r))) =>
            {
                #[allow(clippy::cast_precision_loss, reason = "acceptable")]
                Some(EntryState::Float(l + r as f64))
            },
            (Some(EntryState::Float(l)), Some(EntryState::Float(r))) => {
                Some(EntryState::Float(l + r))
            },
            _ => None,
        }
    }
}

/// Resolve an ABNF controller value to text.
fn abnf_controller_text(
    node: &WrappedNode,
    cache: &mut ResolverCache,
    metadata: &mut Vec<MetaData>,
) -> Option<TextLiteralBytes> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };

    if rule != "type2" {
        return None;
    }

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "value"
            && let Some(text) = abnf_value_text(child, metadata)
        {
            return Some(text);
        }
    }

    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child
            && rule == "typename"
        {
            let ref_name = text.trim();
            if cache.is_resolved(ref_name) {
                return cache_lookup_text(ref_name, cache, metadata);
            }
        }
    }

    None
}

/// Resolve a `value` subtree to text for ABNF use.
fn abnf_value_text(
    node: &WrappedNode,
    metadata: &mut Vec<MetaData>,
) -> Option<TextLiteralBytes> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };

    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            return match rule.as_str() {
                "text" => {
                    match TextLiteralBytes::parse(text.as_bytes()) {
                        Ok(text) => Some(text),
                        Err(_e) => {
                            push_metadata(metadata, MetaData::CtlopTypeMismatch);
                            None
                        },
                    }
                },
                "bytes" => {
                    match ByteLiteralBytes::parse(text.as_bytes()) {
                        Ok(bytes) => {
                            match TextLiteralBytes::try_from(bytes) {
                                Ok(text) => Some(text),
                                Err(_e) => {
                                    push_metadata(metadata, MetaData::CtlopTypeMismatch);
                                    None
                                },
                            }
                        },
                        Err(_e) => {
                            push_metadata(metadata, MetaData::CtlopTypeMismatch);
                            None
                        },
                    }
                },
                _ => {
                    push_metadata(metadata, MetaData::CtlopTypeMismatch);
                    None
                },
            };
        }
    }

    None
}

/// Resolve a cached typename to text if possible.
fn cache_lookup_text(
    name: &str,
    cache: &mut ResolverCache,
    metadata: &mut Vec<MetaData>,
) -> Option<TextLiteralBytes> {
    match cache.get(name) {
        EntryState::Text(t) => Some(t.clone()),
        EntryState::Bytes(b) => {
            match TextLiteralBytes::try_from(b.clone()) {
                Ok(text) => Some(text),
                Err(_e) => {
                    push_metadata(metadata, MetaData::CtlopTypeMismatch);
                    None
                },
            }
        },
        _ => {
            push_metadata(metadata, MetaData::CtlopTypeMismatch);
            None
        },
    }
}

/// Convert an `Option<EntryState>` to an `Option<TextLiteralBytes>`,
/// converting Bytes→Text as needed.  Returns `None` (and tags metadata)
/// if the entry is not a Text or Bytes variant.
fn entry_to_text(
    val: Option<EntryState>,
    metadata: &mut Vec<MetaData>,
) -> Option<TextLiteralBytes> {
    match val {
        Some(EntryState::Text(t)) => Some(t),
        Some(EntryState::Bytes(b)) => {
            if let Ok(t) = TextLiteralBytes::try_from(b) {
                Some(t)
            } else {
                push_metadata(metadata, MetaData::CtlopTypeMismatch);
                None
            }
        },
        _ => {
            if val.is_some() {
                push_metadata(metadata, MetaData::CtlopTypeMismatch);
            }
            None
        },
    }
}

/// Resolve a `type2` leaf to its concrete cache value.
#[must_use]
pub fn resolve_type2_leaf(
    node: &WrappedNode,
    cache: &ResolverCache,
) -> Option<EntryState> {
    if let WrappedNode::Syntax {
        rule,
        children,
        text,
        ..
    } = node
        && rule == "type2"
    {
        for child in children {
            if let WrappedNode::Syntax { rule, text, .. } = child
                && rule == "typename"
            {
                let ref_name = text.trim();
                if cache.is_resolved(ref_name) {
                    return cache_lookup(ref_name, cache);
                }
            }
        }

        let value = text.trim();
        if !value.is_empty() {
            if let Ok(i) = value.parse::<i128>() {
                return Some(EntryState::Integer(i));
            }
            if let Ok(f) = value.parse::<f64>() {
                return Some(EntryState::Float(f));
            }
        }
    }
    None
}

/// Press the text from a node, returning an empty string for non-text
/// variants.
#[must_use]
pub fn child_text(node: &WrappedNode) -> &str {
    match node {
        WrappedNode::RuleLine { text, .. }
        | WrappedNode::Comment { text, .. }
        | WrappedNode::ModuleStart { text, .. }
        | WrappedNode::ModuleEnd { text, .. }
        | WrappedNode::Syntax { text, .. } => text,
        WrappedNode::Directive { source_comment, .. } => source_comment,
    }
}

/// Lookup a name in the cache (read-only iteration).
fn cache_lookup(
    name: &str,
    cache: &ResolverCache,
) -> Option<EntryState> {
    for (key, state) in cache.iter() {
        if key == name && state.is_resolved() {
            return Some(state.clone());
        }
    }
    None
}

/// Clone the cache while omitting a target name.
fn clone_cache_without_target(
    cache: &ResolverCache,
    target: &str,
) -> ResolverCache {
    let mut cloned = ResolverCache::new();
    for (name, state) in cache.iter() {
        if name == target {
            continue;
        }
        let origin = cache.origin(name).cloned();
        drop(cloned.resolve_with_origin(name, state.clone(), origin));
    }
    cloned
}

/// Operand families used by structural validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandFamily {
    /// No strong family information was available.
    Unknown,
    /// Any data item.
    Any,
    /// UTF-8 text string.
    Text,
    /// Byte string.
    Bytes,
    /// Numeric value of any kind.
    Numeric,
    /// Integer value.
    Integer,
    /// Floating-point value.
    Float,
    /// Array-shaped value.
    Array,
    /// Group-shaped value.
    Group,
}

/// Validate a control operator structurally, even when it does not fold to a
/// concrete literal.
#[must_use]
pub fn validate_ctlop_semantics(
    op: &str,
    lhs: &WrappedNode,
    rhs: &WrappedNode,
    cache: &ResolverCache,
) -> bool {
    let lhs_family = operand_family(lhs, cache);
    let rhs_family = operand_family(rhs, cache);

    match op {
        ".cat" | ".det" => validate_cat_det_family(lhs_family, rhs_family),
        ".b64u" | ".b64u-sloppy" | ".b64c" | ".b64c-sloppy" | ".hex" | ".hexlc" | ".hexuc"
        | ".b32" | ".h32" | ".b45" | ".base10" => {
            validate_string_to_bytes_or_int_family(lhs_family, rhs_family)
        },
        ".abnf" | ".abnfb" | ".regexp" => validate_string_source_family(lhs_family),
        ".x-enc.abnf" | ".x-enc.abnfb" | ".x-hash.abnf" | ".x-hash.abnfb" => {
            validate_abnf_annotation_family(lhs_family, rhs_family)
        },
        ".x-compressed.abnf"
        | ".x-compressed.abnfb"
        | ".x-brotli.abnf"
        | ".x-brotli.abnfb"
        | ".x-zstd.abnf"
        | ".x-zstd.abnfb"
        | ".x-gzip.abnf"
        | ".x-gzip.abnfb"
        | ".x-deflate.abnf"
        | ".x-deflate.abnfb" => validate_abnf_compression_family(lhs_family, rhs_family),
        ".x-enc" | ".x-hash" => validate_annotation_family(lhs_family, rhs_family),
        ".x-compressed" | ".x-brotli" | ".x-zstd" | ".x-gzip" | ".x-deflate" => {
            validate_compression_family(lhs_family, rhs_family)
        },
        ".join" => validate_join_family(lhs_family, rhs_family),
        ".printf" => validate_printf_family(lhs_family, rhs_family),
        ".plus" => validate_numeric_family(lhs_family, rhs_family),
        ".json" => validate_json_family(lhs_family, rhs_family),
        ".size" => validate_size_family(lhs_family, rhs_family),
        ".bits" => validate_bits_family(lhs_family, rhs_family),
        ".lt" | ".le" | ".gt" | ".ge" => validate_ordering_family(lhs_family, rhs_family),
        ".eq" | ".ne" | ".and" | ".default" | ".within" => validate_relation_family(),
        ".sdnv" => validate_sdnv_family(lhs_family, rhs_family),
        ".sdnvseq" | ".oid" => validate_sdnv_sequence_family(lhs_family, rhs_family),
        ".feature" => validate_feature_family(lhs_family, rhs_family),
        ".cbor" | ".cborseq" | ".prefp" | ".prefpseq" | ".dtrm" | ".dtrmseq" => {
            validate_serialization_family(lhs_family, rhs_family)
        },
        _ => false,
    }
}

/// Return `true` when the operand family is acceptable, treating unknown
/// shapes as potentially valid so that unresolved aliases can still pass the
/// final structural pass.
fn family_matches(
    family: OperandFamily,
    allowed: &[OperandFamily],
) -> bool {
    family == OperandFamily::Unknown || allowed.contains(&family)
}

/// Validate `.cat` and `.det` as string concatenation operators.
fn validate_cat_det_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
}

/// Validate text-to-bytes or text-to-integer conversion operators.
fn validate_string_to_bytes_or_int_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text])
        && family_matches(rhs_family, &[OperandFamily::Bytes, OperandFamily::Integer])
}

/// Validate operators that consume source text or bytes for parsing.
fn validate_string_source_family(lhs_family: OperandFamily) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
}

/// Validate unofficial annotation-style wrappers such as `.x-enc` and `.x-hash`.
///
/// These operators intentionally preserve the inner type relationship without
/// folding to a literal in the current compiler.
fn validate_annotation_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Any])
}

/// Validate unofficial annotated ABNF wrappers such as `.x-enc.abnf` and
/// `.x-hash.abnf`.
fn validate_abnf_annotation_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
}

/// Validate unofficial compression annotations such as `.x-brotli` and
/// `.x-compressed`.  The carrier must be a `bstr`; the controller is the
/// (logical, uncompressed) payload schema, which can be anything.
fn validate_compression_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Any])
}

/// Validate unofficial compression ABNF annotations such as
/// `.x-brotli.abnf` and `.x-compressed.abnf`.  The carrier must be a
/// `bstr`; the RHS is an inline ABNF source.
fn validate_abnf_compression_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
}

/// Validate `.join`, which joins array or group-shaped content into a string.
fn validate_join_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text, OperandFamily::Bytes])
        && family_matches(rhs_family, &[OperandFamily::Array, OperandFamily::Group])
}

/// Validate `.printf`, which formats an array or group of arguments.
fn validate_printf_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text])
        && family_matches(rhs_family, &[OperandFamily::Array, OperandFamily::Group])
}

/// Validate numeric comparisons and arithmetic-like operands.
fn validate_numeric_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[
        OperandFamily::Numeric,
        OperandFamily::Integer,
        OperandFamily::Float,
    ]) && family_matches(rhs_family, &[
        OperandFamily::Numeric,
        OperandFamily::Integer,
        OperandFamily::Float,
    ])
}

/// Validate `.json`, which emits text for JSON values. The RHS is the
/// JSON structure template — a map (or any type) — so `Group` is
/// accepted; a named template resolves to `Unknown` and also passes.
fn validate_json_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Text])
        && family_matches(rhs_family, &[OperandFamily::Any, OperandFamily::Group])
}

/// Validate `.size`, which constrains byte length or integer width.
fn validate_size_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[
        OperandFamily::Text,
        OperandFamily::Bytes,
        OperandFamily::Numeric,
        OperandFamily::Integer,
    ]) && family_matches(rhs_family, &[
        OperandFamily::Numeric,
        OperandFamily::Integer,
    ])
}

/// Validate `.bits`, which constrains bit positions in a byte string.
fn validate_bits_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[
        OperandFamily::Bytes,
        OperandFamily::Numeric,
        OperandFamily::Integer,
    ]) && family_matches(rhs_family, &[
        OperandFamily::Numeric,
        OperandFamily::Integer,
        OperandFamily::Array,
        OperandFamily::Group,
    ])
}

/// Validate ordering comparisons such as `.lt` and `.ge`.
fn validate_ordering_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    validate_numeric_family(lhs_family, rhs_family)
}

/// Validate relation-only operators whose structural check is tautological.
fn validate_relation_family() -> bool {
    true
}

/// Validate `.sdnv`, which operates on bytes and numeric controllers.
fn validate_sdnv_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[
            OperandFamily::Numeric,
            OperandFamily::Integer,
        ])
}

/// Validate `.sdnvseq` and `.oid`, which accept byte-oriented sequence content.
fn validate_sdnv_sequence_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[OperandFamily::Bytes])
        && family_matches(rhs_family, &[
            OperandFamily::Array,
            OperandFamily::Group,
            OperandFamily::Numeric,
            OperandFamily::Integer,
        ])
}

/// Validate `.feature`, which is a generic feature selection operator.
fn validate_feature_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[
        OperandFamily::Any,
        OperandFamily::Text,
        OperandFamily::Bytes,
    ]) && family_matches(rhs_family, &[
        OperandFamily::Text,
        OperandFamily::Array,
        OperandFamily::Group,
        OperandFamily::Any,
    ])
}

/// Validate serialization-oriented operators such as `.cbor` and `.dtrm`.
fn validate_serialization_family(
    lhs_family: OperandFamily,
    rhs_family: OperandFamily,
) -> bool {
    family_matches(lhs_family, &[
        OperandFamily::Any,
        OperandFamily::Text,
        OperandFamily::Bytes,
    ]) && family_matches(rhs_family, &[
        OperandFamily::Any,
        OperandFamily::Text,
        OperandFamily::Bytes,
        OperandFamily::Array,
        OperandFamily::Group,
    ])
}

/// Classify a `type2` operand into a rough semantic family.
fn operand_family(
    node: &WrappedNode,
    cache: &ResolverCache,
) -> OperandFamily {
    if let Some(value) = resolve_type2_leaf(node, cache) {
        return match value {
            EntryState::Text(_) => OperandFamily::Text,
            EntryState::Bytes(_) => OperandFamily::Bytes,
            EntryState::Integer(_) | EntryState::RangeInt { .. } => OperandFamily::Integer,
            EntryState::Float(_) | EntryState::RangeFloat { .. } => OperandFamily::Float,
            _ => OperandFamily::Unknown,
        };
    }

    let text = child_text(node).trim();
    if text.is_empty() {
        return OperandFamily::Unknown;
    }

    if matches!(text, "any" | "#") {
        return OperandFamily::Any;
    }
    if matches!(text, "text" | "tstr") {
        return OperandFamily::Text;
    }
    if matches!(text, "bytes" | "bstr") {
        return OperandFamily::Bytes;
    }
    if matches!(text, "uint" | "nint" | "int") {
        return OperandFamily::Integer;
    }
    if matches!(
        text,
        "number" | "float" | "float16" | "float32" | "float64" | "float16-32" | "float32-64"
    ) {
        return OperandFamily::Numeric;
    }
    if matches!(
        text,
        "true" | "false" | "bool" | "nil" | "null" | "undefined"
    ) {
        return OperandFamily::Any;
    }

    if text.starts_with('"') {
        return OperandFamily::Text;
    }
    if text.starts_with("h'") || text.starts_with("b64'") || text.starts_with('\'') {
        return OperandFamily::Bytes;
    }
    if text.starts_with('[') {
        return OperandFamily::Array;
    }
    if text.starts_with('{') || text.starts_with('&') {
        return OperandFamily::Group;
    }
    if text.parse::<i128>().is_ok() {
        return OperandFamily::Integer;
    }
    if text.parse::<f64>().is_ok() {
        return OperandFamily::Numeric;
    }

    OperandFamily::Unknown
}

// ---------------------------------------------------------------------------
// Serialization composition warnings (W008 — plan 019)
// ---------------------------------------------------------------------------

/// How strictly a serialization operator constrains encoding.
///
/// The ordered width is `.cbor(0) < .prefp(1) < .dtrm(2)`.
/// A higher index means *narrower* (stricter) encoding constraints.
fn serialization_width(op: &str) -> Option<u8> {
    match op {
        ".cbor" | ".cborseq" => Some(0),
        ".prefp" | ".prefpseq" => Some(1),
        ".dtrm" | ".dtrmseq" => Some(2),
        _ => None,
    }
}

/// Walk the resolved user tree and warn when a direct `any` composition
/// names an inner encoding that is weaker than the current effective
/// serialization constraint.
///
/// For each `RuleLine` whose RHS is `any .<op> <controller>`, the
/// function resolves `<controller>` to its definition node and checks
/// whether that definition itself carries a serialization operator on
/// `any`.  If the inner operator is *wider* (weaker) than the outer
/// effective constraint, a W008 warning is emitted using the wording
/// required by the draft:
///
/// ```text
/// warning[W008]: inner explicit encoding `.prefp` is wider than the
/// current `.dtrm` constraint; this type will be constrained to `.dtrm`
/// ```
///
/// * Narrowing paths (`.cbor → .prefp → .dtrm`) produce no warning.
/// * Repeated identical operators produce no warning.
/// * `.cborseq`/`.prefpseq`/`.dtrmseq` are treated the same as their single-item
///   counterparts for the purposes of the width comparison.
pub(crate) fn warn_serialization_weaker_inner(
    nodes: &[WrappedNode],
    warnings: &mut Vec<crate::error::Diagnostic>,
) {
    for node in nodes {
        warn_serialization_weaker_inner_node(node, nodes, warnings);
    }
}

/// Recursive helper for [`warn_serialization_weaker_inner`].
fn warn_serialization_weaker_inner_node(
    node: &WrappedNode,
    all_nodes: &[WrappedNode],
    warnings: &mut Vec<crate::error::Diagnostic>,
) {
    match node {
        WrappedNode::RuleLine {
            children,
            origin,
            span,
            ..
        } => {
            let Some(expr) = children.iter().find_map(|c| {
                if let WrappedNode::Syntax { rule, children, .. } = c
                    && rule == "expr"
                {
                    return Some(children.as_slice());
                }
                None
            }) else {
                return;
            };
            let Some((ctlop_text, ctlop_span, controller_name)) =
                find_serialization_ctlop_on_any(expr)
            else {
                return;
            };
            let Some(outer_width) = serialization_width(&ctlop_text) else {
                return;
            };

            // Resolve the controller to its definition node in the tree.
            let Some(def) = find_definition_node(controller_name.as_str(), all_nodes) else {
                return;
            };
            let WrappedNode::RuleLine {
                children: def_children,
                ..
            } = def
            else {
                return;
            };
            let Some(def_expr) = def_children.iter().find_map(|c| {
                if let WrappedNode::Syntax { rule, children, .. } = c
                    && rule == "expr"
                {
                    return Some(children.as_slice());
                }
                None
            }) else {
                return;
            };
            let Some((inner_ctlop, inner_ctlop_span, _inner_ctrl)) =
                find_serialization_ctlop_on_any(def_expr)
            else {
                return;
            };
            let Some(inner_width) = serialization_width(&inner_ctlop) else {
                return;
            };

            if inner_width >= outer_width {
                return;
            }

            let outer_op = ctlop_text.trim();
            let inner_op = inner_ctlop.trim();

            warnings.push(crate::error::Diagnostic {
                code: "W008",
                level: crate::error::DiagnosticLevel::Warning,
                message: format!(
                    "inner explicit encoding `{inner_op}` is wider than the current \
                     `{outer_op}` constraint; this type will be constrained to `{outer_op}`"
                ),
                source_file: Some(origin.source_path.clone()),
                span: Some(inner_ctlop_span.clone()),
                previous_origin: Some(crate::node::SourceOrigin {
                    source_path: origin.source_path.clone(),
                    line: origin.line,
                    column: origin
                        .column
                        .saturating_add(ctlop_span.start.saturating_sub(span.start)),
                }),
                related: Vec::new(),
            });
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                warn_serialization_weaker_inner_node(child, all_nodes, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Scan a `RuleLine`'s `expr` children for a pattern where the RHS is
/// `any .<serialization-op> <typename>` (e.g. `any .dtrm mytype`).
///
/// Returns the ctlop text, its span, and the controller typename if found.
fn find_serialization_ctlop_on_any(
    expr_children: &[WrappedNode]
) -> Option<(String, std::ops::Range<usize>, String)> {
    let mut lhs_seen = false;
    for child in expr_children {
        if let WrappedNode::Syntax { rule, .. } = child {
            if (rule == "typename" || rule == "groupname") && !lhs_seen {
                lhs_seen = true;
                continue;
            }
            if rule == "assignt" {
                continue;
            }
            if rule == "type1" {
                return find_serialization_in_type1(child);
            }
            if let WrappedNode::Syntax { children: c, .. } = child
                && let Some(found) = find_serialization_ctlop_on_any(c)
            {
                return Some(found);
            }
        }
    }
    None
}

/// Walk a `type1` node looking for `any .<ctlop> <typename>`.
fn find_serialization_in_type1(
    type1: &WrappedNode
) -> Option<(String, std::ops::Range<usize>, String)> {
    let WrappedNode::Syntax { children, .. } = type1 else {
        return None;
    };
    let mut type2_found = None;
    let mut ctlop_found = None;
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" if type2_found.is_none() => type2_found = Some(child),
                "ctlop" if ctlop_found.is_none() => ctlop_found = Some(child),
                _ => {},
            }
        }
    }
    let ctlop_node = ctlop_found?;
    let WrappedNode::Syntax {
        text: ctlop_text,
        span: ctlop_span,
        ..
    } = ctlop_node
    else {
        return None;
    };
    let _ = serialization_width(ctlop_text.trim())?;
    let type2 = type2_found?;
    let _ = type2;
    let controller_name = find_rhs_type2_text(type1, ctlop_node)?;
    Some((ctlop_text.clone(), ctlop_span.clone(), controller_name))
}

/// After a ctlop node in type1, find the rhs type2's typename text.
#[allow(clippy::collapsible_if)]
fn find_rhs_type2_text(
    type1: &WrappedNode,
    ctlop_node: &WrappedNode,
) -> Option<String> {
    let WrappedNode::Syntax { children, .. } = type1 else {
        return None;
    };
    let ctlop_span = node_span(ctlop_node);
    let mut after_ctlop = false;
    for child in children {
        let child_span = node_span(child);
        if !after_ctlop {
            if child_span == ctlop_span {
                after_ctlop = true;
            }
            continue;
        }
        #[allow(clippy::collapsible_if)]
        if let WrappedNode::Syntax {
            rule,
            text,
            children: c,
            ..
        } = child
        {
            if rule == "type2" {
                for gc in c {
                    if let WrappedNode::Syntax {
                        rule: r, text: t, ..
                    } = gc
                        && (r == "typename" || r == "groupname")
                    {
                        return Some(t.trim().to_owned());
                    }
                }
                return Some(text.trim().to_owned());
            }
        }
    }
    None
}

/// Extract the source span from a `WrappedNode`.
fn node_span(node: &WrappedNode) -> std::ops::Range<usize> {
    match node {
        WrappedNode::RuleLine { span, .. }
        | WrappedNode::Comment { span, .. }
        | WrappedNode::Syntax { span, .. }
        | WrappedNode::Directive { span, .. } => span.clone(),
        WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => 0..0,
    }
}

/// Walk the entire node tree looking for the `RuleLine` that defines
/// the given name, starting from the provided sibling scope.  This is
/// used to resolve controller references for W008.
fn find_definition_node<'a>(
    name: &str,
    search_siblings: &'a [WrappedNode],
) -> Option<&'a WrappedNode> {
    for node in search_siblings {
        if let WrappedNode::RuleLine { children, .. } = node {
            if let Some(n) = rule_name(node)
                && (n == name || n.starts_with(&format!("{name}<")))
            {
                return Some(node);
            }
            for child in children {
                if let Some(found) = find_definition_node(name, std::slice::from_ref(child)) {
                    return Some(found);
                }
            }
            continue;
        }
        if let Some(found) = find_definition_node_in_children(node, name) {
            return Some(found);
        }
    }
    None
}

/// Recursive helper for [`find_definition_node`] that walks directive
/// and syntax children.
fn find_definition_node_in_children<'a>(
    node: &'a WrappedNode,
    name: &str,
) -> Option<&'a WrappedNode> {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => find_definition_node(name, children),
        _ => None,
    }
}

/// Extract the LHS typename from a `RuleLine` node's source text.
fn rule_name(node: &WrappedNode) -> Option<String> {
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
