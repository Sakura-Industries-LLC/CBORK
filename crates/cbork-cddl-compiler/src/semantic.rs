// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Semantic constant resolution — fixed-point pass.
//!
//! Walks the resolved AST repeatedly, seeding the [`ResolverCache`] with
//! directly-known literal constants and propagating values through type
//! references, until no further progress is possible.
//!
//! # Algorithm
//!
//! 1. For every `RuleLine`, ensure the LHS typename exists in the cache.
//! 2. If the RHS is a literal → parse it and resolve in the cache.
//! 3. If the RHS is a typename reference → if the target is already resolved, propagate
//!    the value.  Otherwise skip.
//! 4. Repeat until `cnt_unresolved()` stops decreasing.
//!
//! Later phases (ctlop evaluation) insert additional passes into the same
//! loop.

use crate::{
    MetaData,
    compiled::CompiledCDDL,
    ctlop::ctlop_pass,
    error::{Diagnostic, DiagnosticLevel},
    node::SourceOrigin,
    rangeop::rangeop_pass,
    resolver_cache::ResolverCache,
    seedops::seed_pass,
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the fixed-point constant resolution loop over `compiled`.
///
/// On return, the cache contains every typename seen in the document,
/// resolved to a concrete value where possible, or `Unresolved` where
/// it could not be determined.
pub fn resolve_constants(compiled: &mut CompiledCDDL) -> ResolverCache {
    let mut cache = ResolverCache::new();

    loop {
        let prev = cache.cnt_unresolved();

        resolve_constants_in_nodes(&mut compiled.user_nodes, &mut compiled.warnings, &mut cache);

        let current = cache.cnt_unresolved();
        if current == prev {
            break;
        }
    }

    cache
}

/// Run the constant-resolution passes over an arbitrary node slice.
pub(crate) fn resolve_constants_in_nodes(
    nodes: &mut [crate::WrappedNode],
    warnings: &mut Vec<Diagnostic>,
    cache: &mut ResolverCache,
) {
    for node in nodes {
        seed_pass(node, cache, warnings);
        rangeop_pass(node, cache, warnings);
        ctlop_pass(node, cache, warnings);
    }
}

/// Emit a warning for a redundant definition.
pub(crate) fn handle_rangeop_error(
    e: &crate::resolver_cache::CacheWriteError,
    name: &str,
    metadata: &mut Vec<MetaData>,
    origin: &SourceOrigin,
    span: &std::ops::Range<usize>,
    warnings: &mut Vec<Diagnostic>,
) {
    match e {
        crate::resolver_cache::CacheWriteError::RedundantType { .. } => {
            push_metadata(metadata, MetaData::RedundantDefinition);
            push_redundant_diagnostic(warnings, name, origin, span, None);
        },
        crate::resolver_cache::CacheWriteError::ConflictingType { .. } => {
            push_metadata(metadata, MetaData::ConflictingDefinition);
            push_conflict_diagnostic(warnings, name, origin, span, None);
        },
        _ => {},
    }
}

/// Emit a warning for a redundant definition.
pub(crate) fn push_redundant_diagnostic(
    warnings: &mut Vec<Diagnostic>,
    name: &str,
    current: &SourceOrigin,
    span: &std::ops::Range<usize>,
    existing: Option<&SourceOrigin>,
) {
    let mut message = format!(
        "redundant definition of `{name}` at {}:{}:{}",
        current.source_path.display(),
        current.line,
        current.column
    );
    if let Some(existing) = existing {
        use std::fmt::Write as _;
        let _ = write!(
            message,
            "; first defined at {}:{}:{}",
            existing.source_path.display(),
            existing.line,
            existing.column
        );
    }
    warnings.push(Diagnostic {
        code: "W001",
        level: DiagnosticLevel::Warning,
        message,
        source_file: Some(current.source_path.clone()),
        span: Some(span.clone()),
        previous_origin: existing.cloned(),
        related: Vec::new(),
    });
}

/// Emit an error for a conflicting definition.
pub(crate) fn push_conflict_diagnostic(
    warnings: &mut Vec<Diagnostic>,
    name: &str,
    current: &SourceOrigin,
    span: &std::ops::Range<usize>,
    existing: Option<&SourceOrigin>,
) {
    let mut message = format!(
        "conflicting definition of `{name}` at {}:{}:{}",
        current.source_path.display(),
        current.line,
        current.column
    );
    if let Some(existing) = existing {
        use std::fmt::Write as _;
        let _ = write!(
            message,
            "; previous definition at {}:{}:{}",
            existing.source_path.display(),
            existing.line,
            existing.column
        );
    }
    warnings.push(Diagnostic {
        code: "E014",
        level: DiagnosticLevel::Error,
        message,
        source_file: Some(current.source_path.clone()),
        span: Some(span.clone()),
        previous_origin: existing.cloned(),
        related: Vec::new(),
    });
}

/// Add metadata if it is not already present.
pub(crate) fn push_metadata(
    metadata: &mut Vec<MetaData>,
    flag: MetaData,
) -> bool {
    if !metadata.contains(&flag) {
        metadata.push(flag);
        return true;
    }
    false
}

/// Remove a metadata flag if present.
pub(crate) fn remove_metadata(
    metadata: &mut Vec<MetaData>,
    flag: MetaData,
) -> bool {
    if let Some(pos) = metadata.iter().position(|existing| *existing == flag) {
        metadata.remove(pos);
        return true;
    }
    false
}
