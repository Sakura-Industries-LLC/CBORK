// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Compiler error and diagnostic types.
//!
//! Supports collecting multiple diagnostics before failing so that callers
//! get a complete picture of all recoverable problems in one pass.

use std::{fmt, ops::Range, path::PathBuf};

use crate::node::SourceOrigin;

/// The severity of a compiler diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticLevel {
    /// An error that prevents successful compilation.
    Error,
    /// A non-fatal warning.
    Warning,
}

/// A single compiler diagnostic with file, span, and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable diagnostic code such as `E001` or `W023`.
    pub code: &'static str,
    /// Severity level.
    pub level: DiagnosticLevel,
    /// Human-readable message.
    pub message: String,
    /// The source file that produced this diagnostic, if known.
    pub source_file: Option<PathBuf>,
    /// Byte offset span in the source, if known.
    pub span: Option<Range<usize>>,
    /// The previous source location involved in the diagnostic, if known.
    pub previous_origin: Option<SourceOrigin>,
    /// Secondary annotations attached to the diagnostic.
    ///
    /// Each sub-diagnostic points at a concrete CDDL snippet (typically a
    /// rendered line from the `concrete` renderer) and tags it with a
    /// relationship to the parent. The CLI diagnostic renderer uses these
    /// to emit compact diff gutters such as `==`, `--`, `??`, and `!!`
    /// next to the rendered CDDL, so the user can see exactly which
    /// lines participate in a check and how.
    #[doc(hidden)]
    pub related: Vec<Subdiag>,
}

/// A secondary annotation attached to a [`Diagnostic`].
///
/// A sub-diag pairs a rendered CDDL snippet (one or more lines, in source
/// order) with a label that explains its role relative to the parent
/// diagnostic. The snippet is plain CDDL text; the parent diagnostic's
/// `span` and `source_file` should point at the location the user should
/// navigate to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subdiag {
    /// Relationship of this snippet to the parent diagnostic.
    pub kind: SubdiagKind,
    /// Pre-rendered CDDL text. May be multi-line; the CLI renderer is
    /// responsible for laying it out under the parent diagnostic.
    pub snippet: String,
    /// Optional source location the snippet came from. When `None` the
    /// snippet is synthetic (e.g. an unfolded template).
    pub origin: Option<SourceOrigin>,
}

/// The role of a [`Subdiag`] relative to its parent diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubdiagKind {
    /// This snippet is the left-hand side of a check (e.g. the LHS of
    /// `.within` or `.and`).
    Lhs,
    /// This snippet is the right-hand side of a check (e.g. the RHS of
    /// `.within` or `.and`).
    Rhs,
    /// This snippet participates in a check and matched its counterpart.
    Matched,
    /// This snippet participates in a check but its counterpart is
    /// missing (e.g. an LHS entry with no matching RHS pattern).
    Unmatched,
    /// This snippet is structural context that does not change the check
    /// outcome (e.g. an optional RHS arm not chosen by the LHS).
    Optional,
    /// This snippet was the source of a fold (e.g. a named constant
    /// resolved to a value).
    FoldedFrom,
    /// Generic note attached to the parent diagnostic.
    Note,
}

/// Collection of diagnostics produced during compilation.
///
/// The compiler aggregates as many recoverable errors as possible before
/// returning.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// All diagnostics collected so far.
    pub diagnostics: Vec<Diagnostic>,
}

impl CompileError {
    /// Create an empty error collector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
        }
    }

    /// Add an error-level diagnostic.
    pub fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic {
            code,
            level: DiagnosticLevel::Error,
            message: message.into(),
            source_file: None,
            span: None,
            previous_origin: None,
            related: Vec::new(),
        });
    }

    /// Add an error-level diagnostic with a source file and span.
    pub fn error_spanned(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        source_file: Option<PathBuf>,
        span: Option<Range<usize>>,
    ) {
        self.diagnostics.push(Diagnostic {
            code,
            level: DiagnosticLevel::Error,
            message: message.into(),
            source_file,
            span,
            previous_origin: None,
            related: Vec::new(),
        });
    }

    /// Add a warning-level diagnostic.
    pub fn warn(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic {
            code,
            level: DiagnosticLevel::Warning,
            message: message.into(),
            source_file: None,
            span: None,
            previous_origin: None,
            related: Vec::new(),
        });
    }

    /// Returns `true` if any error-level diagnostics have been recorded.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.level == DiagnosticLevel::Error)
    }

    /// Returns `true` if no diagnostics have been recorded at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

impl Diagnostic {
    /// Returns `true` when the diagnostic is an error-level entry.
    /// Used by `--doc` and other layered lint passes to decide
    /// whether the surrounding pipeline must skip downstream work.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.level == DiagnosticLevel::Error
    }

    /// Attach a sub-diagnostic annotation to this diagnostic.
    #[must_use]
    pub fn with_related(
        mut self,
        subdiags: Vec<Subdiag>,
    ) -> Self {
        self.related = subdiags;
        self
    }

    /// Attach a single sub-diagnostic annotation.
    #[must_use]
    pub fn with_subdiag(
        mut self,
        kind: SubdiagKind,
        snippet: impl Into<String>,
        origin: Option<SourceOrigin>,
    ) -> Self {
        self.related.push(Subdiag {
            kind,
            snippet: snippet.into(),
            origin,
        });
        self
    }
}

impl Default for CompileError {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CompileError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        for diag in &self.diagnostics {
            let level = match diag.level {
                DiagnosticLevel::Error => "error",
                DiagnosticLevel::Warning => "warning",
            };
            match &diag.source_file {
                Some(path) => {
                    writeln!(
                        f,
                        "{level}[{}]: {} (in {})",
                        diag.code,
                        diag.message,
                        path.display()
                    )?;
                },
                None => {
                    writeln!(f, "{level}[{}]: {}", diag.code, diag.message)?;
                },
            }
        }
        Ok(())
    }
}

impl std::error::Error for CompileError {}
