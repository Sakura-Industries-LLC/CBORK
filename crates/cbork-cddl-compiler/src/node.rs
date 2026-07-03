// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Enhanced AST node types for CDDL compiler stages.
//!
//! [`WrappedNode`] is the compiler-owned AST representation that can carry
//! directives, provenance, and metadata alongside parsed CDDL constructs.

use std::{ops::Range, path::PathBuf};

use cbork_cddl_parser::modules::Directive;

/// Source provenance for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOrigin {
    /// Source file the node came from.
    pub source_path: PathBuf,
    /// One-based line number of the node start.
    pub line: usize,
    /// One-based column number of the node start.
    pub column: usize,
}

impl SourceOrigin {
    /// Construct a new source origin.
    #[must_use]
    pub fn new(
        source_path: PathBuf,
        line: usize,
        column: usize,
    ) -> Self {
        Self {
            source_path,
            line,
            column,
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Metadata attached to an AST node.
///
/// Each node carries a list of metadata entries. The list may contain
/// duplicate entries. Metadata controls downstream behaviour such as
/// pruning eligibility and emission control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaData {
    /// The node may be removed if it becomes dangling after include
    /// expansion and resolution.
    Prunable,
    /// The node remains part of the processed model but should not appear
    /// in concise emitted output.
    Silent,
    /// The node was injected from the standard postlude.
    StandardPostlude,
    /// The definition was seen more than once with the same value.
    RedundantDefinition,
    /// The definition conflicts with an earlier value.
    ConflictingDefinition,
    /// Range operator applied to incompatible or non-numeric types.
    RangeTypeMismatch,
    /// Control operator applied to incompatible types.
    CtlopTypeMismatch,
    /// The rule is part of a CBORK library's public export surface,
    /// either explicitly via `;@ CBORK: Export` or because the file
    /// is a library and the rule is the only top-level entry point.
    Exported,
}

// ---------------------------------------------------------------------------
// WrappedNode
// ---------------------------------------------------------------------------

/// A node in the enhanced CDDL AST.
///
/// Wraps parsed pest pairs alongside directive, metadata, and provenance
/// information that the raw parse tree cannot carry.
#[derive(Debug, Clone)]
pub enum WrappedNode {
    /// A CDDL rule/expression line (the `line` grammar rule wrapping an
    /// `expr`).
    RuleLine {
        /// The source text of the rule.
        text: String,
        /// Byte span in the original source.
        span: Range<usize>,
        /// Source location of the line.
        origin: SourceOrigin,
        /// Child syntax nodes nested beneath this line.
        children: Vec<WrappedNode>,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
    /// A non-directive comment preserved from the source.
    Comment {
        /// The comment text, including the leading `;`.
        text: String,
        /// Byte span in the original source.
        span: Range<usize>,
        /// Source location of the comment.
        origin: SourceOrigin,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
    /// A generic recursive syntax node for nested grammar structure.
    Syntax {
        /// The pest rule name.
        rule: String,
        /// The source text for this subtree.
        text: String,
        /// Byte span in the original source.
        span: Range<usize>,
        /// Source location of the syntax node.
        origin: SourceOrigin,
        /// Child syntax nodes.
        children: Vec<WrappedNode>,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
    /// A module directive comment and its parsed directive data.
    Directive {
        /// The parsed module directive.
        directive: Directive,
        /// The original comment text from the source.
        source_comment: String,
        /// Byte span of the comment in the original source.
        span: Range<usize>,
        /// Source location of the directive comment.
        origin: SourceOrigin,
        /// Child nodes bounded by this directive.
        children: Vec<WrappedNode>,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
    /// Synthetic start marker emitted by the injection pass.
    ModuleStart {
        /// The module-preamble text.
        text: String,
        /// Source location inherited from the directive comment.
        origin: SourceOrigin,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
    /// Synthetic end marker emitted by the injection pass.
    ModuleEnd {
        /// The module-epilogue text.
        text: String,
        /// Source location inherited from the directive comment.
        origin: SourceOrigin,
        /// Metadata flags controlling pruning and emission.
        metadata: Vec<MetaData>,
    },
}

impl WrappedNode {
    /// Return a short human-readable label for the node kind.
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            WrappedNode::RuleLine { .. } => "RuleLine",
            WrappedNode::Comment { .. } => "Comment",
            WrappedNode::Syntax { .. } => "Syntax",
            WrappedNode::Directive { .. } => "Directive",
            WrappedNode::ModuleStart { .. } => "ModuleStart",
            WrappedNode::ModuleEnd { .. } => "ModuleEnd",
        }
    }

    /// Return a reference to this node's metadata list.
    #[must_use]
    pub fn metadata(&self) -> &[MetaData] {
        match self {
            WrappedNode::RuleLine { metadata, .. }
            | WrappedNode::Comment { metadata, .. }
            | WrappedNode::Syntax { metadata, .. }
            | WrappedNode::Directive { metadata, .. }
            | WrappedNode::ModuleStart { metadata, .. }
            | WrappedNode::ModuleEnd { metadata, .. } => metadata,
        }
    }

    /// Return this node's source origin.
    #[must_use]
    pub fn origin(&self) -> &SourceOrigin {
        match self {
            WrappedNode::RuleLine { origin, .. }
            | WrappedNode::Comment { origin, .. }
            | WrappedNode::Syntax { origin, .. }
            | WrappedNode::Directive { origin, .. }
            | WrappedNode::ModuleStart { origin, .. }
            | WrappedNode::ModuleEnd { origin, .. } => origin,
        }
    }

    /// Apply a function that maps over every `WrappedNode` in the tree.
    pub fn map_nodes_mut(
        &mut self,
        f: &mut impl FnMut(&mut WrappedNode),
    ) {
        f(self);
        match self {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                for child in children {
                    child.map_nodes_mut(f);
                }
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }

    /// Build the module-start marker text for a directive.
    #[must_use]
    pub fn module_start_marker(directive: &Directive) -> String {
        format!("; Module: {}", directive_display(directive))
    }

    /// Build the module-end marker text for a directive.
    #[must_use]
    pub fn module_end_marker(directive: &Directive) -> String {
        format!("; End Module: {}", directive_display(directive))
    }

    /// Recursively count the number of nodes in this tree.
    #[must_use]
    pub fn node_count(&self) -> usize {
        let child_count: usize = match self {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                children.iter().map(WrappedNode::node_count).sum()
            },
            _ => 0,
        };
        1_usize.wrapping_add(child_count)
    }
}

/// Produce a compact display string for a directive suitable for marker
/// comments.
fn directive_display(d: &Directive) -> String {
    match d {
        Directive::Import { filename } => {
            format!("import {filename:?}")
        },
        Directive::ImportAs { filename, alias } => {
            format!("import {filename:?} as {alias}")
        },
        Directive::ImportFrom { names, filename } => {
            let names_str = names.join(", ");
            format!("import {names_str} from {filename:?}")
        },
        Directive::ImportFromAs {
            names,
            filename,
            alias,
        } => {
            let names_str = names.join(", ");
            format!("import {names_str} from {filename:?} as {alias}")
        },
        Directive::Include { filename } => {
            format!("include {filename:?}")
        },
        Directive::IncludeAs { filename, alias } => {
            format!("include {filename:?} as {alias}")
        },
        Directive::IncludeFrom { names, filename } => {
            let names_str = names.join(", ");
            format!("include {names_str} from {filename:?}")
        },
        Directive::IncludeFromAs {
            names,
            filename,
            alias,
        } => {
            let names_str = names.join(", ");
            format!("include {names_str} from {filename:?} as {alias}")
        },
    }
}
