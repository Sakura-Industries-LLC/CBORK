// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! The [`CompiledCDDL`] type - the public compiled document wrapper.
//!
//! Owns the source path and enriched ASTs, provides compilation from file
//! paths, and supports a tree-dump display for development.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use cbork_cddl_parser::{modules::Directive as ParserDirective, parse_cddl, parse_postlude};

use crate::{
    MetaData, WrappedNode,
    error::{CompileError, Diagnostic},
    finalize::{
        detect_elidable_self_references, detect_unreferenced_top_level_definitions,
        finalize_compiled,
    },
    generic::expand_generics,
    marker::{collect_marker_misuse, is_trailing_marker_comment},
    preprocessor::{inject_directives, process_ast},
    resolver::resolve_includes,
    resolver_cache::ResolverCache,
    symbols::rule_head_from_children,
};

/// A compiled CDDL document with enhanced AST.
///
/// The compiler keeps the user document and the standard postlude separate so
/// later passes can decide when and how to merge them.
#[derive(Debug)]
pub struct CompiledCDDL {
    /// The source file path this document was compiled from.
    pub source_path: PathBuf,
    /// Original source text before include/import resolution and
    /// generic expansion.
    pub raw_source: String,
    /// The logical root path used for absolute include resolution, if any.
    pub root_path: Option<PathBuf>,
    /// AST nodes from the user's source file.
    pub user_nodes: Vec<WrappedNode>,
    /// AST nodes from the standard postlude, marked [`MetaData::Silent`].
    pub postlude_nodes: Vec<WrappedNode>,
    /// Physically complete tree after surgical postlude injection.
    pub complete_nodes: Vec<WrappedNode>,
    /// Non-fatal warnings collected during compilation.
    pub warnings: Vec<Diagnostic>,
    /// Whether the source declares itself as a reusable library module.
    pub is_library: bool,
    /// Names intentionally declared as external by file directives.
    pub extern_names: HashSet<String>,
    /// Names of rules that the source has declared as part of its public
    /// library API via `;@ CBORK: Export`. The Export directive binds to
    /// the next rule definition while skipping blank lines, regular
    /// comments, and doc comments; the rule that follows is recorded
    /// here for downstream doc-lint checks and the import/include pass.
    pub exported_names: HashSet<String>,
    /// Per-import library registry populated by the resolver (Step
    /// 5.12).  For each `import` / `include` directive that resolved
    /// to a CBORK library, the canonical path of that library, its
    /// library flag, and its `exported_names` + `extern_names` sets
    /// are recorded so cross-file direct-use export linting can
    /// enforce the export contract.
    pub imported_libraries: Vec<ImportedLibrary>,
    /// Cache of resolved type constants (populated by semantic pass).
    pub resolved_types: ResolverCache,
}

/// Information about a single imported library, recorded during
/// directive resolution so Step 5.12 cross-file export linting can
/// enforce the library's export contract.
#[derive(Debug, Clone)]
pub struct ImportedLibrary {
    /// Canonical path of the imported file (filesystem path for
    /// regular imports, `catalog:<name>` for well-known imports).
    pub canonical_path: PathBuf,
    /// Whether the imported file declares itself as a library.
    pub is_library: bool,
    /// Names exported via `;@ CBORK: Export`.
    pub exported_names: HashSet<String>,
    /// Names declared via `;@ CBORK: Extern ...`.
    pub extern_names: HashSet<String>,
    /// Names cherry-picked via a `from ... import <name>,...`
    /// directive.  Empty for whole-file imports that do not name
    /// specific symbols.  Step 5.12 + BUG-004 use this to tell
    /// surface symbols (which must be treated as direct consumer
    /// references for collision detection) apart from private
    /// library roots (which the consumer does not see).
    pub directive_names: HashSet<String>,
    /// Source location of the import directive that brought this
    /// library in.
    pub import_origin: crate::SourceOrigin,
}

impl CompiledCDDL {
    /// Returns `true` if the compiled document has at least one
    /// hard error in its diagnostic list.  Downstream validators
    /// must not treat the [`complete_nodes`](Self::complete_nodes)
    /// tree as a valid schema tree when this is `true`; the
    /// physical structure may still be present (so callers can
    /// inspect it for diagnostics) but any conformance check
    /// should bail out before relying on it.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.warnings
            .iter()
            .any(|d| d.level == crate::DiagnosticLevel::Error)
    }

    /// Compile a CDDL source file into a [`CompiledCDDL`] document.
    ///
    /// Reads the file at `path`, parses it, processes module directives,
    /// and keeps the built-in postlude separate for later controlled merge.
    ///
    /// `root_path`, if provided, is the logical root for absolute include
    /// resolution (used in later stages).
    ///
    /// # Errors
    ///
    /// Returns [`CompileError`] if the file cannot be read, parsing fails, or
    /// directive parsing encounters errors.
    pub fn compile<P: AsRef<Path>>(
        path: P,
        root_path: Option<&Path>,
    ) -> Result<Self, CompileError> {
        let path = path.as_ref();

        let mut errors = CompileError::new();

        let raw_source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                errors.error("E001", format!("failed to read {}: {e}", path.display()));
                return Err(errors);
            },
        };

        let mut compiled =
            Self::compile_from_source(path.to_path_buf(), raw_source.as_str(), root_path)?;

        // Resolve include/import directives
        let mut visited = std::collections::HashSet::new();
        // Record the current file so self-references are caught
        if let Ok(canon) = std::fs::canonicalize(path) {
            visited.insert(canon);
        }
        resolve_includes(&mut compiled, &mut visited)?;
        detect_unreferenced_top_level_definitions(&mut compiled);
        detect_elidable_self_references(&mut compiled);

        expand_generics(&mut compiled.user_nodes, &mut compiled.warnings);

        // Definition-strength normalization, the semantic fixed-point
        // pass, and the postlude merge all run inside `finalize_compiled`
        // *after* the reachability-based pruner has removed unreferenced
        // prunable definitions.  Running the strength or cache-resolution
        // pass up here would let two unreferenced weak imports flag each
        // other as redundant or conflicting before either is pruned.
        finalize_compiled(&mut compiled);
        deduplicate_diagnostics(&mut compiled.warnings);

        Ok(compiled)
    }

    /// Build a compiled document from in-memory source without resolving
    /// include/import directives.
    ///
    /// This is used by the recursive resolver so that it can control cycle
    /// detection with a shared visited set.
    pub(crate) fn compile_from_source(
        source_path: PathBuf,
        raw_source: &str,
        root_path: Option<&Path>,
    ) -> Result<Self, CompileError> {
        let errors = CompileError::new();

        let user_pairs = parse_cddl(raw_source).map_err(|e| {
            let mut errs = CompileError::new();
            errs.error(
                "E002",
                format!("parse error in {}: {e}", source_path.display()),
            );
            errs
        })?;
        let user_pairs = process_ast(user_pairs).map_err(|e| {
            let mut errs = CompileError::new();
            errs.error(
                "E003",
                format!("preprocessor error in {}: {e}", source_path.display()),
            );
            errs
        })?;
        let mut user_nodes =
            inject_directives(&source_path, &user_pairs, raw_source).map_err(|e| {
                let mut errs = CompileError::new();
                errs.error(
                    "E004",
                    format!(
                        "directive injection error in {}: {e}",
                        source_path.display()
                    ),
                );
                errs
            })?;
        let (file_directives, mut file_directive_diagnostics) =
            scan_cbork_file_directives(&source_path, &user_nodes, raw_source);

        let mut marker_misuse_diagnostics = Vec::new();
        collect_marker_misuse(&user_nodes, raw_source, &mut marker_misuse_diagnostics);

        // Apply `;@ CBORK: Export` directives to tag the next rule
        // with `MetaData::Exported`.  This must run after the file
        // directives scan (which validates the Library / Extern /
        // Unknown shapes) and before finalization so the metadata
        // participates in pruning and the library-export surface is
        // discoverable by downstream passes.
        let exported_names = apply_export_directives(
            &mut user_nodes,
            file_directives.is_library,
            &mut file_directive_diagnostics,
            &source_path,
        );

        let postlude_pairs = parse_postlude().map_err(|e| {
            let mut errs = CompileError::new();
            errs.error("E005", format!("postlude parse error: {e}"));
            errs
        })?;
        let postlude_pairs = process_ast(postlude_pairs).map_err(|e| {
            let mut errs = CompileError::new();
            errs.error("E006", format!("postlude preprocess error: {e}"));
            errs
        })?;
        let postlude_path = PathBuf::from("<postlude>");
        let mut postlude_nodes =
            inject_directives(&postlude_path, &postlude_pairs, "").map_err(|e| {
                let mut errs = CompileError::new();
                errs.error("E007", format!("postlude directive error: {e}"));
                errs
            })?;
        for node in &mut postlude_nodes {
            tag_tree_silent(node);
        }

        let compiled = CompiledCDDL {
            source_path,
            raw_source: raw_source.to_owned(),
            root_path: root_path.map(Path::to_path_buf),
            user_nodes,
            postlude_nodes,
            complete_nodes: Vec::new(),
            warnings: {
                let mut diagnostics = errors.diagnostics;
                diagnostics.append(&mut file_directive_diagnostics);
                diagnostics.append(&mut marker_misuse_diagnostics);
                diagnostics
            },
            is_library: file_directives.is_library,
            extern_names: file_directives.extern_names,
            exported_names,
            imported_libraries: Vec::new(),
            resolved_types: ResolverCache::new(),
        };

        Ok(compiled)
    }
}

/// File-level directives parsed from comment annotations.
#[derive(Debug, Default)]
struct FileDirectives {
    /// Whether `;@ CBORK: Library` was found as the first non-whitespace comment.
    is_library: bool,
    /// Names declared via `;@ CBORK: Extern <name>,...`.
    extern_names: HashSet<String>,
}

/// Scans leading comments and comment directives in a file's nodes.
///
/// Returns the parsed directives together with any diagnostics (e.g., misplaced
/// or duplicate directives).
///
/// `source_text` is the full pre-transform CDDL source. It is used to
/// classify the position of `;@`-style CBORK directive comments so that
/// trailing markers (after non-whitespace CDDL source on the same line)
/// are treated as ordinary comments and never apply as CBORK directives.
#[allow(
    clippy::too_many_lines,
    reason = "Directive scan accumulates many small per-directive arms in one match."
)]
fn scan_cbork_file_directives(
    source_path: &Path,
    nodes: &[WrappedNode],
    source_text: &str,
) -> (FileDirectives, Vec<Diagnostic>) {
    let mut directives = FileDirectives::default();
    let mut diagnostics = Vec::new();
    let mut first_library_origin = None;
    let mut first_library_span = None;

    for node in nodes {
        if let WrappedNode::Comment {
            text, span, origin, ..
        } = node
        {
            if is_trailing_marker_comment(text, origin, source_text) {
                continue;
            }
            if is_cbork_library_comment(text) {
                directives.is_library = true;
                first_library_origin = Some(origin.clone());
                first_library_span = Some(span.clone());
            }
            continue;
        }

        break;
    }

    let mut directive_sites = Vec::new();
    collect_cbork_directive_sites(nodes, source_text, &mut directive_sites, &mut diagnostics);

    let mut extern_origins = std::collections::HashMap::<String, crate::SourceOrigin>::new();
    for site in directive_sites {
        match site.directive {
            CborkDirective::Library => {
                let is_first = first_library_span
                    .as_ref()
                    .is_some_and(|first| *first == site.span);
                if is_first {
                    continue;
                }

                diagnostics.push(Diagnostic {
                    code: "E018",
                    level: crate::DiagnosticLevel::Error,
                    message: if directives.is_library {
                        "duplicate `;@ CBORK: Library` directive".to_owned()
                    } else {
                        "misplaced `;@ CBORK: Library` directive; it must appear before any non-comment content".to_owned()
                    },
                    source_file: Some(source_path.to_path_buf()),
                    span: Some(site.span),
                    previous_origin: first_library_origin.clone(),
                    related: Vec::new(),
                });
            },
            // BUG-001: A valid `;@ CBORK: Export` immediately before a
            // rule used to emit a false `E020` from this pass.  The
            // E020 was redundant with the E022 emitted by
            // `apply_export_directives_inner` (which catches the
            // real invalid cases: export at EOF, export before an
            // `import` / `include` directive, and export in a
            // non-library file).  Valid exports are now applied
            // silently here; only `apply_export_directives_inner`
            // reports problems, with the more specific E022 code.
            CborkDirective::Export => {},
            CborkDirective::Extern(names) => {
                for name in names {
                    if !is_valid_extern_name(&name) {
                        diagnostics.push(Diagnostic {
                            code: "E019",
                            level: crate::DiagnosticLevel::Error,
                            message: format!("invalid extern name `{name}`"),
                            source_file: Some(source_path.to_path_buf()),
                            span: Some(site.span.clone()),
                            previous_origin: None,
                            related: Vec::new(),
                        });
                        continue;
                    }

                    if let Some(previous_origin) = extern_origins.get(&name) {
                        diagnostics.push(Diagnostic {
                            code: "E019",
                            level: crate::DiagnosticLevel::Error,
                            message: format!("duplicate extern declaration `{name}`"),
                            source_file: Some(source_path.to_path_buf()),
                            span: Some(site.span.clone()),
                            previous_origin: Some(previous_origin.clone()),
                            related: Vec::new(),
                        });
                        continue;
                    }

                    extern_origins.insert(name.clone(), site.origin.clone());
                    directives.extern_names.insert(name);
                }
            },
            CborkDirective::Unknown(name) => {
                diagnostics.push(Diagnostic {
                    code: "E021",
                    level: crate::DiagnosticLevel::Error,
                    message: format!(
                        "unknown `;@ CBORK: {name}` directive (recognized: Library, Export, Extern ...)"
                    ),
                    source_file: Some(source_path.to_path_buf()),
                    span: Some(site.span.clone()),
                    previous_origin: None,
                    related: Vec::new(),
                });
            },
        }
    }

    if !directives.is_library && !directives.extern_names.is_empty() {
        for site in collect_extern_sites(nodes, source_text) {
            diagnostics.push(Diagnostic {
                code: "E019",
                level: crate::DiagnosticLevel::Error,
                message: "`;@ CBORK: Extern ...` requires `;@ CBORK: Library` in the same file"
                    .to_owned(),
                source_file: Some(source_path.to_path_buf()),
                span: Some(site.span),
                previous_origin: first_library_origin.clone(),
                related: Vec::new(),
            });
        }
    }

    (directives, diagnostics)
}

/// A single parsed directive together with its location in source.
#[derive(Debug, Clone)]
struct DirectiveSite {
    /// The kind of directive.
    directive: CborkDirective,
    /// Byte range of the directive in the source file.
    span: std::ops::Range<usize>,
    /// Origin information (source path and character offset).
    origin: crate::SourceOrigin,
}

/// A CBORK annotation parsed from a comment.
#[derive(Debug, Clone)]
enum CborkDirective {
    /// `;@ CBORK: Library` — marks this file as a library.
    Library,
    /// `;@ CBORK: Export` — marks the next rule as publicly exported.
    Export,
    /// `;@ CBORK: Extern <name>,...` — declares extern identifiers.
    Extern(Vec<String>),
    /// `;@ CBORK: <unknown>` — recognized namespace but unknown
    /// directive.  Stored as the original directive text so the
    /// diagnostic can name the bad directive.
    Unknown(String),
}

/// Recursively walks the node tree, collecting every standalone
/// `;@ CBORK:...` comment into `out`.
///
/// Trailing `;@` markers (those that appear after non-whitespace CDDL
/// source on the same source line) are skipped — they are treated as
/// ordinary comments and never apply as CBORK directives.
///
/// Also emits a W002 warning for every `;@ <other>: ...` directive so
/// users notice that a tool annotation was ignored.
fn collect_cbork_directive_sites(
    nodes: &[WrappedNode],
    source_text: &str,
    out: &mut Vec<DirectiveSite>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            WrappedNode::Comment {
                text, span, origin, ..
            } => {
                if is_trailing_marker_comment(text, origin, source_text) {
                    continue;
                }
                if let Some(directive) = parse_cbork_comment(text) {
                    out.push(DirectiveSite {
                        directive,
                        span: span.clone(),
                        origin: origin.clone(),
                    });
                } else if let Some((namespace, rest)) = parse_external_directive(text) {
                    diagnostics.push(Diagnostic {
                        code: "W002",
                        level: crate::DiagnosticLevel::Warning,
                        message: format!(
                            "unknown external directive `;@ {namespace}: {rest}` (ignored: only `;@ CBORK: ...` is recognized)"
                        ),
                        source_file: Some(origin.source_path.clone()),
                        span: Some(span.clone()),
                        previous_origin: None,
                        related: Vec::new(),
                    });
                }
            },
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                collect_cbork_directive_sites(children, source_text, out, diagnostics);
            },
            WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Collects only `Extern` directive sites from the given nodes.
fn collect_extern_sites(
    nodes: &[WrappedNode],
    source_text: &str,
) -> Vec<DirectiveSite> {
    let mut sites = Vec::new();
    let mut diagnostics = Vec::new();
    collect_cbork_directive_sites(nodes, source_text, &mut sites, &mut diagnostics);
    sites
        .into_iter()
        .filter(|site| matches!(site.directive, CborkDirective::Extern(_)))
        .collect()
}

/// Apply `;@ CBORK: Export` directives by tagging the next
/// `RuleLine` with [`MetaData::Exported`].
///
/// Rules of the application:
///
/// * The file must be a CBORK library; otherwise an E022 diagnostic fires for the
///   offending directive.
/// * The directive must be followed (after any whitespace, normal comments, or doc
///   comments) by a `RuleLine`; otherwise an E022 diagnostic fires.  The directive must
///   NOT skip over an `import` / `include` directive comment — if it does, an E022
///   diagnostic fires.
/// * Consecutive `Export` directives with no rule between them also produce an E022
///   diagnostic per offending directive.
/// * Any `Directive` node sitting between two `Export` directives aborts the chain: the
///   second `Export` is rejected as having no following rule.
///
/// Returns the set of exported rule names so downstream passes
/// (direct-use linting, library export warnings) can consult it.
fn apply_export_directives(
    nodes: &mut [WrappedNode],
    is_library: bool,
    diagnostics: &mut Vec<Diagnostic>,
    source_path: &Path,
) -> HashSet<String> {
    let mut exported: HashSet<String> = HashSet::new();
    let mut pending_export: Option<std::ops::Range<usize>> = None;
    apply_export_directives_inner(
        nodes,
        is_library,
        diagnostics,
        source_path,
        &mut exported,
        &mut pending_export,
    );
    if let Some(prev) = pending_export.take() {
        diagnostics.push(Diagnostic {
            code: "E022",
            level: crate::DiagnosticLevel::Error,
            message: "`;@ CBORK: Export` at end of file with no following rule".to_owned(),
            source_file: Some(source_path.to_path_buf()),
            span: Some(prev),
            previous_origin: None,
            related: Vec::new(),
        });
    }
    exported
}

/// Recursive helper for [`apply_export_directives`] that carries the
/// `exported` set through nested directive scopes so a `;@ CBORK:
/// Export` inside a directive's children still tags the next rule in
/// that scope.
fn apply_export_directives_inner(
    nodes: &mut [WrappedNode],
    is_library: bool,
    diagnostics: &mut Vec<Diagnostic>,
    source_path: &Path,
    exported: &mut HashSet<String>,
    pending_export: &mut Option<std::ops::Range<usize>>,
) {
    let mut i = 0;
    while i < nodes.len() {
        let Some(node) = nodes.get_mut(i) else {
            break;
        };
        let advance = match node {
            WrappedNode::Comment { text, span, .. } => {
                handle_export_comment(
                    text,
                    span,
                    is_library,
                    diagnostics,
                    source_path,
                    pending_export,
                );
                true
            },
            WrappedNode::Directive {
                directive,
                span,
                children,
                ..
            } => {
                if let Some(prev) = pending_export.take() {
                    diagnostics.push(Diagnostic {
                        code: "E022",
                        level: crate::DiagnosticLevel::Error,
                        message: format!(
                            "`;@ CBORK: Export` must be followed by a rule, not an `{}` directive",
                            directive_short_name(directive)
                        ),
                        source_file: Some(source_path.to_path_buf()),
                        span: Some(prev),
                        previous_origin: None,
                        related: Vec::new(),
                    });
                }
                let _ = span;
                apply_export_directives_inner(
                    children.as_mut_slice(),
                    is_library,
                    diagnostics,
                    source_path,
                    exported,
                    pending_export,
                );
                true
            },
            WrappedNode::RuleLine {
                metadata, children, ..
            } => {
                let head = rule_head_from_children(children);
                if pending_export.take().is_some() {
                    metadata.push(MetaData::Exported);
                    if let Some(h) = &head {
                        exported.insert(h.name.clone());
                    }
                }
                scan_children_for_exports(
                    children,
                    is_library,
                    diagnostics,
                    source_path,
                    pending_export,
                );
                true
            },
            _ => true,
        };
        let _ = advance;
        i = i.saturating_add(1);
    }
}

#[allow(
    clippy::doc_markdown,
    reason = "CDDL directive names are identifiers, not Rust code"
)]
/// Walk RuleLine children and process any `;@ CBORK: Export` comments
/// they contain.  The parser nests trailing comments under the
/// preceding RuleLine, so the export-for-the-next-rule might be a
/// child of the current RuleLine rather than a top-level sibling.
fn scan_children_for_exports(
    children: &mut [WrappedNode],
    is_library: bool,
    diagnostics: &mut Vec<Diagnostic>,
    source_path: &Path,
    pending_export: &mut Option<std::ops::Range<usize>>,
) {
    for child in children.iter() {
        if let WrappedNode::Comment { text, span, .. } = child {
            handle_export_comment(
                text,
                span,
                is_library,
                diagnostics,
                source_path,
                pending_export,
            );
        }
    }
}

/// Process a single comment node for pending Export state.
fn handle_export_comment(
    text: &str,
    span: &std::ops::Range<usize>,
    is_library: bool,
    diagnostics: &mut Vec<Diagnostic>,
    source_path: &Path,
    pending_export: &mut Option<std::ops::Range<usize>>,
) {
    if let Some(CborkDirective::Export) = parse_cbork_comment(text) {
        if !is_library {
            diagnostics.push(Diagnostic {
                code: "E022",
                level: crate::DiagnosticLevel::Error,
                message:
                    "`;@ CBORK: Export` is only valid in a library file (set `;@ CBORK: Library`)"
                        .to_owned(),
                source_file: Some(source_path.to_path_buf()),
                span: Some(span.clone()),
                previous_origin: None,
                related: Vec::new(),
            });
            *pending_export = None;
        } else if let Some(prev) = pending_export.take() {
            diagnostics.push(Diagnostic {
                code: "E022",
                level: crate::DiagnosticLevel::Error,
                message: "consecutive `;@ CBORK: Export` directives with no rule between them"
                    .to_owned(),
                source_file: Some(source_path.to_path_buf()),
                span: Some(prev),
                previous_origin: None,
                related: Vec::new(),
            });
            *pending_export = Some(span.clone());
        } else {
            *pending_export = Some(span.clone());
        }
        return;
    }
    let _parsed = parse_cbork_comment(text);
    // Both recognised-but-non-Export CBORK directives AND plain
    // comments leave any pending Export queued: the directive scan
    // already validated the directive shape, and plain comments are
    // just whitespace from this pass's point of view.  We bind the
    // value into a binding rather than `let _ =` because clippy
    // flags non-binding `let _` on non-Copy values.
}

/// Short display name for a directive (used in error messages).
fn directive_short_name(d: &ParserDirective) -> &'static str {
    match d {
        ParserDirective::Import { .. }
        | ParserDirective::ImportAs { .. }
        | ParserDirective::ImportFrom { .. }
        | ParserDirective::ImportFromAs { .. } => "import",
        ParserDirective::Include { .. }
        | ParserDirective::IncludeAs { .. }
        | ParserDirective::IncludeFrom { .. }
        | ParserDirective::IncludeFromAs { .. } => "include",
    }
}

/// Returns `true` if a comment contains the `;@ CBORK: Library` directive.
fn is_cbork_library_comment(text: &str) -> bool {
    matches!(parse_cbork_comment(text), Some(CborkDirective::Library))
}

/// Tries to parse a `;@ CBORK:...` comment into a [`CborkDirective`].
///
/// Returns `None` when the text is not a CBORK directive comment.
/// Unknown CBORK directives are surfaced as
/// [`CborkDirective::Unknown`] so the caller can emit an E021
/// diagnostic; the parser itself never returns `None` for a
/// well-formed `;@ CBORK:...` comment.
fn parse_cbork_comment(text: &str) -> Option<CborkDirective> {
    let rest = text.trim_start().strip_prefix(";@")?;
    let rest = rest.trim_start().strip_prefix("CBORK:")?;
    let rest = rest.trim_start();
    let head = rest.split_whitespace().next().unwrap_or("");
    match head {
        "Library" => Some(CborkDirective::Library),
        "Export" => Some(CborkDirective::Export),
        "Extern" => {
            let rest = rest.trim_start().strip_prefix("Extern")?.trim_start();
            let names = rest
                .split(';')
                .next()
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            Some(CborkDirective::Extern(names))
        },
        other => Some(CborkDirective::Unknown(other.to_owned())),
    }
}

/// Returns `Some(namespace, rest_text)` when the comment is a
/// `;@ <namespace>: ...` directive in any namespace other than
/// `CBORK` (which is handled by [`parse_cbork_comment`]).  The
/// caller emits a W002 warning so the user knows the annotation
/// was ignored.  Returns `None` for plain comments, non-`;@`
/// comments, and `;@ CBORK:` directives.
fn parse_external_directive(text: &str) -> Option<(String, String)> {
    let rest = text.trim_start().strip_prefix(";@")?.trim_start();
    let (namespace, after) = rest.split_once(':')?;
    let namespace = namespace.trim();
    if namespace.is_empty() || namespace == "CBORK" {
        return None;
    }
    Some((namespace.to_owned(), after.trim().to_owned()))
}

/// Returns `true` if `name` is a valid extern identifier.
fn is_valid_extern_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || matches!(first, '@' | '_' | '$')) {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '_' | '$' | '-' | '.'))
}

/// Removes duplicate diagnostics, keeping the highest-ranked one for each key.
fn deduplicate_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    let mut best_by_key = HashMap::<
        (
            &'static str,
            crate::DiagnosticLevel,
            Option<PathBuf>,
            Option<std::ops::Range<usize>>,
        ),
        Diagnostic,
    >::new();

    for diagnostic in diagnostics.drain(..) {
        let key = (
            diagnostic.code,
            diagnostic.level,
            diagnostic.source_file.clone(),
            diagnostic.span.clone(),
        );
        match best_by_key.get_mut(&key) {
            Some(existing) => {
                if diagnostic_rank(&diagnostic) > diagnostic_rank(existing) {
                    *existing = diagnostic;
                }
            },
            None => {
                best_by_key.insert(key, diagnostic);
            },
        }
    }

    *diagnostics = best_by_key.into_values().collect();
    diagnostics.sort_by(|left, right| {
        left.source_file
            .cmp(&right.source_file)
            .then_with(|| {
                left.span
                    .as_ref()
                    .map(|span| span.start)
                    .cmp(&right.span.as_ref().map(|span| span.start))
            })
            .then_with(|| left.code.cmp(right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
}

/// Assigns a heuristic rank to a diagnostic for stable deduplication.
///
/// Diagnostics that carry a `previous_origin` are ranked higher so they survive dedup.
fn diagnostic_rank(diagnostic: &Diagnostic) -> usize {
    usize::from(diagnostic.previous_origin.is_some())
        .saturating_mul(2)
        .saturating_add(diagnostic.message.len())
}

/// Recursively push [`MetaData::Silent`] onto every node in a tree.
fn tag_tree_silent(node: &mut WrappedNode) {
    node.map_nodes_mut(&mut |child| {
        if !child.metadata().contains(&MetaData::Silent) {
            match child {
                WrappedNode::RuleLine { metadata, .. }
                | WrappedNode::Comment { metadata, .. }
                | WrappedNode::Syntax { metadata, .. }
                | WrappedNode::Directive { metadata, .. }
                | WrappedNode::ModuleStart { metadata, .. }
                | WrappedNode::ModuleEnd { metadata, .. } => metadata.push(MetaData::Silent),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tree dump display
// ---------------------------------------------------------------------------

/// Renders a tree-dump view of a [`CompiledCDDL`] for development and
/// debugging.
///
/// # Examples
///
/// ```
/// # use cbork_cddl_compiler::dump_tree;
/// // let dump = dump_tree(&compiled);
/// // println!("{dump}");
/// ```
#[must_use]
pub fn dump_tree(compiled: &CompiledCDDL) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();

    let _ = writeln!(
        out,
        "CompiledCDDL (source: {})",
        compiled.source_path.display()
    );
    if let Some(ref root) = compiled.root_path {
        let _ = writeln!(out, "  root_path: {}", root.display());
    }
    let _ = writeln!(out, "  user nodes:");

    for (i, node) in compiled.user_nodes.iter().enumerate() {
        let is_last = i.wrapping_add(1) == compiled.user_nodes.len();
        dump_node(node, &mut out, 2, is_last);
    }

    if !compiled.postlude_nodes.is_empty() {
        let _ = writeln!(
            out,
            "  postlude nodes ({} total, all Silent):",
            compiled.postlude_nodes.len()
        );
        for (i, node) in compiled.postlude_nodes.iter().enumerate() {
            let is_last = i.wrapping_add(1) == compiled.postlude_nodes.len();
            dump_node(node, &mut out, 2, is_last);
        }
    }

    if compiled
        .complete_nodes
        .iter()
        .any(|node| tree_contains_metadata(node, MetaData::StandardPostlude))
    {
        let _ = writeln!(
            out,
            "  complete nodes ({} total):",
            compiled.complete_nodes.len()
        );
        for (i, node) in compiled.complete_nodes.iter().enumerate() {
            let is_last = i.wrapping_add(1) == compiled.complete_nodes.len();
            dump_node(node, &mut out, 2, is_last);
        }
    }

    if !compiled.warnings.is_empty() {
        let _ = writeln!(out, "  warnings ({}):", compiled.warnings.len());
        for w in &compiled.warnings {
            let _ = writeln!(out, "    - {}", w.message);
        }
    }

    let _ = writeln!(out, "{}", compiled.resolved_types);

    out
}

/// Recursively dump a [`WrappedNode`] with tree-drawing characters.
fn dump_node(
    node: &WrappedNode,
    out: &mut String,
    indent: usize,
    is_last: bool,
) {
    use std::fmt::Write as _;

    let prefix = if is_last { "└── " } else { "├── " };
    let _ = write!(out, "{:indent$}{prefix}", "", indent = indent);

    let meta_str = format_metadata(node.metadata());

    match node {
        WrappedNode::RuleLine { text, children, .. } => {
            let trimmed = text.trim();
            let _ = writeln!(out, "RuleLine: {trimmed}{meta_str}");
            dump_children(children, out, indent);
        },
        WrappedNode::Comment { text, .. } => {
            let trimmed = text.trim();
            let _ = writeln!(out, "Comment: {trimmed}{meta_str}");
        },
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            let trimmed = text.trim();
            let _ = writeln!(out, "Syntax[{rule}]: {trimmed}{meta_str}");
            dump_children(children, out, indent);
        },
        WrappedNode::ModuleStart { text, .. } | WrappedNode::ModuleEnd { text, .. } => {
            let _ = writeln!(out, "{text}{meta_str}");
        },
        WrappedNode::Directive {
            directive,
            children,
            ..
        } => {
            let _ = writeln!(out, "Directive: {directive:?}{meta_str}");
            let child_count = children.len();
            for (j, child) in children.iter().enumerate() {
                let child_last = j.wrapping_add(1) == child_count;
                dump_node(child, out, indent.wrapping_add(4), child_last);
            }
            if children.is_empty() {
                let _ = writeln!(
                    out,
                    "{:indent$}    (no children resolved yet)",
                    "",
                    indent = indent
                );
            }
        },
    }
}

/// Recursively dump child nodes with continuation lines.
fn dump_children(
    children: &[WrappedNode],
    out: &mut String,
    indent: usize,
) {
    use std::fmt::Write as _;

    let child_count = children.len();
    for (j, child) in children.iter().enumerate() {
        let child_is_last = j.wrapping_add(1) == child_count;
        let prefix = if child_is_last {
            "└── "
        } else {
            "├── "
        };
        let child_indent = indent.wrapping_add(4);
        let _ = write!(out, "{:child_indent$}{prefix}", "");

        let meta_str = format_metadata(child.metadata());
        match child {
            WrappedNode::RuleLine {
                text,
                children: grandkids,
                ..
            } => {
                let trimmed = text.trim();
                let _ = writeln!(out, "RuleLine: {trimmed}{meta_str}");
                for (k, gk) in grandkids.iter().enumerate() {
                    let last = k.wrapping_add(1) == grandkids.len();
                    dump_node(gk, out, child_indent.wrapping_add(4), last);
                }
            },
            WrappedNode::Comment { text, .. } => {
                let trimmed = text.trim();
                let _ = writeln!(out, "Comment: {trimmed}{meta_str}");
            },
            WrappedNode::Syntax {
                rule,
                text,
                children: grandkids,
                ..
            } => {
                let trimmed = text.trim();
                let _ = writeln!(out, "Syntax[{rule}]: {trimmed}{meta_str}");
                for (k, gk) in grandkids.iter().enumerate() {
                    let last = k.wrapping_add(1) == grandkids.len();
                    dump_node(gk, out, child_indent.wrapping_add(4), last);
                }
            },
            _ => {
                dump_node(child, out, child_indent, child_is_last);
            },
        }
    }
}

/// Format a metadata slice as a compact suffix string.
fn format_metadata(meta: &[MetaData]) -> String {
    if meta.is_empty() {
        return String::new();
    }
    let tags: Vec<&str> = meta
        .iter()
        .map(|m| {
            match m {
                MetaData::Prunable => "Prunable",
                MetaData::Silent => "Silent",
                MetaData::StandardPostlude => "StandardPostlude",
                MetaData::RedundantDefinition => "RedundantDefinition",
                MetaData::ConflictingDefinition => "ConflictingDefinition",
                MetaData::RangeTypeMismatch => "RangeTypeMismatch",
                MetaData::Exported => "Exported",
                MetaData::CtlopTypeMismatch => "CtlopTypeMismatch",
            }
        })
        .collect();
    format!("  [{}]", tags.join(", "))
}

impl fmt::Display for CompiledCDDL {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(&dump_tree(self))
    }
}

/// Return `true` if any node in a subtree carries the requested metadata.
fn tree_contains_metadata(
    node: &WrappedNode,
    target: MetaData,
) -> bool {
    if node.metadata().contains(&target) {
        return true;
    }

    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } => {
            children
                .iter()
                .any(|child| tree_contains_metadata(child, target))
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => false,
    }
}
