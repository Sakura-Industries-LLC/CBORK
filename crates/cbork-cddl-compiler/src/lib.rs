// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! CDDL module compiler.
//!
//! Consumes parsed CDDL documents and module directives to produce
//! a resolved, pruned, and fully-expanded CDDL document ready for
//! downstream processing.
//!
//! # Architecture
//!
//! * [`CompiledCDDL`] — the public compiled document type that owns source text and an
//!   enriched AST.
//! * [`WrappedNode`] — the compiler-owned enhanced AST node enum.
//! * [`CompileError`] — multi-diagnostic error collection.
//! * [`dump_tree`] — development-oriented tree dump for inspecting the compiled AST.

mod compiled;
mod concrete;
mod ctlop;
mod doc_block;
mod doc_lint;
mod doc_semantics;
mod error;
mod finalize;
mod generic;
pub mod literals;
mod marker;
mod node;
mod preprocessor;
mod pretty;
mod rangeop;
mod resolver;
mod resolver_cache;
mod schema_diff;
mod seedops;
mod semantic;
mod symbols;
mod transform;
#[allow(
    dead_code,
    reason = "validation pipeline is wired in (validate_within_pass); \
              the remaining public helpers are only consumed from unit tests"
)]
mod within;
pub use compiled::{CompiledCDDL, dump_tree};
pub use concrete::{
    Concrete, ConcretePolicy, Line, LineKind, ResolutionMap, TargetSide, build_resolution,
    render_cddl, render_subtree, render_to_string,
};
pub use ctlop::{child_text, resolve_type2_leaf, validate_ctlop_semantics};
pub use doc_block::{DocBinding, DocBlock, DocLine, DocScan, doc_block_range, scan_doc_blocks};
pub use doc_lint::{
    MappedDiagnostics, RumdlError, RumdlRun, SafetyReport, SuppressedWarning, SuppressionReason,
    apply_rumdl_fixes, lint_synthetic_markdown, map_rumdl_diagnostics, validate_doc_source,
};
pub use doc_semantics::{
    DocInternalPolicy, DocSemanticsConfig, DocSemanticsReport, check_doc_semantics,
};
pub use error::{CompileError, Diagnostic, DiagnosticLevel, Subdiag, SubdiagKind};
pub use marker::{
    CommentMarker, MarkerPosition, classify_comment_marker, classify_comment_position,
    collect_marker_spacing_issues, detect_marker_misuse, is_trailing_marker_comment,
    source_line_for,
};
pub use node::{MetaData, SourceOrigin, WrappedNode};
pub use preprocessor::{inject_directives, process_ast};
pub use pretty::pretty_print;
pub use resolver::resolve_includes;
pub use resolver_cache::{CacheWriteError, EntryState, ResolverCache};
pub use semantic::resolve_constants;
pub use symbols::{AssignmentKind, RuleHead, SymbolKind};
pub use transform::{
    ReverseTransformError, SPLICE_MARKER_PREFIX, SyntheticLine, SyntheticLineKind,
    SyntheticMarkdown, collapse_blank_lines, find_splice_markers, reverse_transform, source_line,
    splice_span, transform_to_markdown, trim_trailing_whitespace,
};

#[cfg(test)]
mod tests;
