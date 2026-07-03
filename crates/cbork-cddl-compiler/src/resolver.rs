// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Include/import resolution pass.
//!
//! Walks the enriched AST, resolves every `Directive` node against the
//! filesystem or built-in catalog, recursively compiles referenced
//! modules, and splices the result into `Directive.children`.
//!
//! Also handles cycle detection and prunability propagation.

use std::{
    collections::HashSet,
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use cbork_cddl_parser::modules::{Directive, FileName};

use crate::{MetaData, WrappedNode, compiled::CompiledCDDL, error::CompileError};

/// Canonicalize a path for duplicate-module detection.
fn canonicalize_for_dedup(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

/// Resolve all include/import directives in the compiled document.
///
/// Walks `compiled.user_nodes` recursively.  For each `Directive` node,
/// resolves the target module, recursively compiles it, and splices the
/// resulting AST into `Directive.children`.
///
/// `visited` tracks canonicalised paths already seen in the current
/// resolution chain so that duplicate inclusions (even non-recursive) are
/// detected and rejected.
///
/// # Errors
///
/// Returns [`CompileError`] with collected diagnostics if any module
/// cannot be resolved, parsed, or a cycle is detected.
pub fn resolve_includes<S: BuildHasher>(
    compiled: &mut CompiledCDDL,
    visited: &mut HashSet<PathBuf, S>,
) -> Result<(), CompileError> {
    let mut errors = CompileError::new();
    let mut imported_libraries: Vec<crate::compiled::ImportedLibrary> = Vec::new();

    let node_count = compiled.user_nodes.len();
    for i in 0..node_count {
        // Re-borrow through indexing to avoid borrow conflict
        let source_path = compiled.source_path.clone();
        let root_path = compiled.root_path.clone();
        let node = compiled.user_nodes.get_mut(i).ok_or_else(|| {
            let mut errs = CompileError::new();
            errs.error(
                "E008",
                "internal error: node index out of bounds".to_owned(),
            );
            errs
        })?;
        if let Err(e) = resolve_node_single(
            node,
            &source_path,
            root_path.as_deref(),
            visited,
            &mut imported_libraries,
        ) {
            errors.diagnostics.extend(e.diagnostics);
        }
    }

    compiled.imported_libraries.extend(imported_libraries);

    if errors.has_errors() {
        Err(errors)
    } else {
        Ok(())
    }
}

/// Resolve directives within a single node, recursing into children.
#[allow(
    clippy::too_many_lines,
    reason = "import + include + alias + library tracking all share the directive-resolution path"
)]
fn resolve_node_single<S: BuildHasher>(
    node: &mut WrappedNode,
    source_path: &Path,
    root_path: Option<&Path>,
    visited: &mut HashSet<PathBuf, S>,
    imported_libraries: &mut Vec<crate::compiled::ImportedLibrary>,
) -> Result<(), CompileError> {
    match node {
        WrappedNode::Directive {
            directive,
            children,
            span,
            origin,
            ..
        } => {
            validate_directive_alias_names(directive, origin, span)?;

            let is_import = is_import_directive(directive);
            let has_from = directive_has_names(directive);

            // Resolve the filename to CDDL source text
            let source_dir = source_path.parent().unwrap_or(Path::new("."));
            let filename = directive_filename(directive);
            let source_text = filename.resolve(source_dir, root_path).map_err(|e| {
                let mut errs = CompileError::new();
                errs.error_spanned(
                    "E009",
                    format!(
                        "cannot resolve {} in {}: {e}",
                        directive_display_name(directive),
                        source_path.display()
                    ),
                    Some(origin.source_path.clone()),
                    Some(span.clone()),
                );
                errs
            })?;

            // Determine the canonical path for cycle detection
            let canonical = canonical_path_for_filename(filename, source_dir, root_path);

            // Cycle / duplicate check (see `check_visited` for the
            // full scoping rules).
            check_visited(visited, canonical.as_ref(), directive, origin, span)?;

            // Compile the referenced module.
            // Well-known (catalog) names are parsed directly from the
            // catalog text; filesystem paths go through file I/O.
            // Well-known imports use a stable `catalog:<name>` pseudo
            // path so that two imports of the same well-known module
            // produce rules with the same source origin (see Step 5.9).
            let mut sub_compiled = if let FileName::WellKnown(name) = filename {
                let pseudo_path = PathBuf::from(format!("catalog:{name}"));
                CompiledCDDL::compile_from_source(pseudo_path, source_text.as_str(), root_path)?
            } else {
                // Filesystem path — compile from file
                let sub_path = canonical
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(filename_path_str(filename)));
                let raw_source = std::fs::read_to_string(&sub_path).map_err(|e| {
                    let mut errs = CompileError::new();
                    errs.error_spanned(
                        "E011",
                        format!("failed to read {}: {e}", sub_path.display()),
                        Some(origin.source_path.clone()),
                        Some(span.clone()),
                    );
                    errs
                })?;
                CompiledCDDL::compile_from_source(sub_path, raw_source.as_str(), root_path)?
            };

            // Recursively resolve includes in the sub-module.  For
            // imports, the entry we just inserted in `visited` is
            // popped once the subtree is resolved so that sibling
            // scopes can re-import the same well-known module under a
            // different alias.  True cycles are still caught because
            // the current file's path remains in `visited` for the
            // duration of its own subtree walk.
            let resolve_result = resolve_includes(&mut sub_compiled, visited);
            if is_import && let Some(ref can) = canonical {
                visited.remove(can);
            }
            resolve_result?;

            // Mark prunability only.  Step 4 does not delete anything yet;
            // later passes decide what can actually be pruned.
            let mut resolved = sub_compiled.user_nodes;

            if has_from {
                let wanted: HashSet<String> = directive_names(directive)
                    .iter()
                    .map(|name| normalize_directive_name(name, directive_alias(directive)))
                    .collect();

                for node in &mut resolved {
                    if !rule_name_matches(node, &wanted) {
                        tag_tree_prunable(node);
                    }
                }
            } else if is_import {
                // Bare imports make the whole imported subtree prunable.
                for node in &mut resolved {
                    tag_tree_prunable(node);
                }
            }

            // Handle alias wrapping
            if let Some(alias) = directive_alias(directive) {
                let local_rule_names = collect_rule_names(&resolved);
                wrap_with_alias(&mut resolved, alias, &local_rule_names);
            }

            // Step 5.12: record the imported / included library's
            // metadata so the consumer's finalization pass can
            // enforce the export contract for direct uses.  Both
            // `import` and `include` directives count: the original
            // Step 5.12 requirement says "if a file directly imports
            // or includes another file and directly references a
            // non-exported symbol from that imported/included file,
            // emit a warning."  Only module-shaped directives with a
            // canonical path contribute; relative-only paths and
            // catalog-only entries (no canonical path resolved) are
            // skipped.
            let is_include = is_include_directive(directive);
            if (is_import || is_include)
                && let Some(can) = canonical.as_ref()
            {
                imported_libraries.push(crate::compiled::ImportedLibrary {
                    canonical_path: can.clone(),
                    is_library: sub_compiled.is_library,
                    exported_names: sub_compiled.exported_names.clone(),
                    extern_names: sub_compiled.extern_names.clone(),
                    directive_names: crate::resolver::directive_names(directive)
                        .iter()
                        .map(|name| {
                            crate::resolver::normalize_directive_name(
                                name,
                                crate::resolver::directive_alias(directive),
                            )
                        })
                        .collect(),
                    import_origin: origin.clone(),
                });
            }

            *children = resolved;
        },
        // Recurse into child-bearing nodes
        WrappedNode::RuleLine { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                resolve_node_single(child, source_path, root_path, visited, imported_libraries)?;
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Directive helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the directive is an import variant.
pub(crate) fn is_import_directive(d: &Directive) -> bool {
    matches!(
        d,
        Directive::Import { .. }
            | Directive::ImportAs { .. }
            | Directive::ImportFrom { .. }
            | Directive::ImportFromAs { .. }
    )
}

/// Returns `true` if the directive has a `from` clause with explicit names.
pub(crate) fn directive_has_names(d: &Directive) -> bool {
    matches!(
        d,
        Directive::ImportFrom { .. }
            | Directive::ImportFromAs { .. }
            | Directive::IncludeFrom { .. }
            | Directive::IncludeFromAs { .. }
    )
}

/// Returns `true` if the directive is an include variant.
pub(crate) fn is_include_directive(d: &Directive) -> bool {
    matches!(
        d,
        Directive::Include { .. }
            | Directive::IncludeAs { .. }
            | Directive::IncludeFrom { .. }
            | Directive::IncludeFromAs { .. }
    )
}

/// Check whether `canonical` is already in the active resolution
/// chain and, if not, record it.
///
/// Both `include` and `import` reject re-references that hit an
/// entry already in the active chain.  The difference is what
/// happens *after* the subtree has been resolved: includes leave
/// the entry in place (a file may appear at most once in the
/// resolved include tree, per the spec), while imports pop the
/// entry so that the same module can be imported again from a
/// sibling scope under a different alias.  True cycles are still
/// caught because the current file's path is in `visited` for the
/// duration of its own subtree walk.
fn check_visited<S: BuildHasher>(
    visited: &mut HashSet<PathBuf, S>,
    canonical: Option<&PathBuf>,
    directive: &Directive,
    origin: &crate::node::SourceOrigin,
    span: &std::ops::Range<usize>,
) -> Result<(), CompileError> {
    let Some(can) = canonical else {
        return Ok(());
    };
    if visited.contains(can) {
        let mut errs = CompileError::new();
        errs.error_spanned(
            "E010",
            format!(
                "module {} already included (cycle or duplicate)",
                directive_display_name(directive)
            ),
            Some(origin.source_path.clone()),
            Some(span.clone()),
        );
        return Err(errs);
    }
    visited.insert(can.clone());
    Ok(())
}

/// Extract the [`FileName`] from any directive variant.
fn directive_filename(d: &Directive) -> &FileName {
    match d {
        Directive::Import { filename }
        | Directive::ImportAs { filename, .. }
        | Directive::ImportFrom { filename, .. }
        | Directive::ImportFromAs { filename, .. }
        | Directive::Include { filename }
        | Directive::IncludeAs { filename, .. }
        | Directive::IncludeFrom { filename, .. }
        | Directive::IncludeFromAs { filename, .. } => filename,
    }
}

/// Extract explicit rule names from a `from` clause, or an empty slice.
pub(crate) fn directive_names(d: &Directive) -> &[String] {
    match d {
        Directive::ImportFrom { names, .. }
        | Directive::ImportFromAs { names, .. }
        | Directive::IncludeFrom { names, .. }
        | Directive::IncludeFromAs { names, .. } => names,
        _ => &[],
    }
}

/// Extract the alias from an `as` clause, if present.
pub(crate) fn directive_alias(d: &Directive) -> Option<&str> {
    match d {
        Directive::ImportAs { alias, .. }
        | Directive::ImportFromAs { alias, .. }
        | Directive::IncludeAs { alias, .. }
        | Directive::IncludeFromAs { alias, .. } => Some(alias),
        _ => None,
    }
}

/// Human-readable display for the directive kind (used in errors).
pub(crate) fn directive_display_name(d: &Directive) -> String {
    match d {
        Directive::Import { filename } => format!("import {filename}"),
        Directive::ImportAs { filename, alias } => {
            format!("import {filename} as {alias}")
        },
        Directive::ImportFrom { names, filename } => {
            format!("import {} from {filename}", names.join(", "))
        },
        Directive::ImportFromAs {
            names,
            filename,
            alias,
        } => {
            format!("import {} from {filename} as {alias}", names.join(", "))
        },
        Directive::Include { filename } => format!("include {filename}"),
        Directive::IncludeAs { filename, alias } => {
            format!("include {filename} as {alias}")
        },
        Directive::IncludeFrom { names, filename } => {
            format!("include {} from {filename}", names.join(", "))
        },
        Directive::IncludeFromAs {
            names,
            filename,
            alias,
        } => {
            format!("include {} from {filename} as {alias}", names.join(", "))
        },
    }
}

/// Validate that named selectors in `... from ... as alias` are already
/// prefixed with the alias mandated by the modules draft.
fn validate_directive_alias_names(
    directive: &Directive,
    origin: &crate::node::SourceOrigin,
    span: &std::ops::Range<usize>,
) -> Result<(), CompileError> {
    let Some(alias) = directive_alias(directive) else {
        return Ok(());
    };

    if !directive_has_names(directive) {
        return Ok(());
    }

    let mut errors = CompileError::new();
    for name in directive_names(directive) {
        if name == "*" || name.starts_with(&format!("{alias}.")) {
            continue;
        }

        errors.error_spanned(
            "E009",
            format!(
                "named selectors in `{}` must be prefixed with `{alias}.`; expected `{alias}.{name}`",
                directive_display_name(directive)
            ),
            Some(origin.source_path.clone()),
            Some(span.clone()),
        );
    }

    if errors.has_errors() {
        Err(errors)
    } else {
        Ok(())
    }
}

/// Extract just the path string from a filesystem [`FileName`].
fn filename_path_str(filename: &FileName) -> &str {
    match filename {
        FileName::Relative(p) | FileName::Absolute(p) => p,
        FileName::WellKnown(_) => "",
    }
}

// ---------------------------------------------------------------------------
// Canonical path resolution for cycle detection
// ---------------------------------------------------------------------------

/// Resolve the canonical filesystem path for a filename.
///
/// Returns `None` for well-known (catalog) names that have no filesystem
/// path.
fn canonical_path_for_filename(
    filename: &FileName,
    parent_dir: &Path,
    root_path: Option<&Path>,
) -> Option<PathBuf> {
    match filename {
        FileName::WellKnown(name) => Some(PathBuf::from(format!("catalog:{name}"))),
        FileName::Relative(path) => {
            let full = parent_dir.join(path);
            canonicalize_for_dedup(&full)
        },
        FileName::Absolute(path) => {
            let full = if let Some(root) = root_path {
                root.join(path.trim_start_matches('/'))
            } else {
                PathBuf::from(path)
            };
            canonicalize_for_dedup(&full)
        },
    }
}

// ---------------------------------------------------------------------------
// Prunability propagation
// ---------------------------------------------------------------------------

/// Recursively push [`MetaData::Prunable`] onto every node in a tree.
fn tag_tree_prunable(node: &mut WrappedNode) {
    node.map_nodes_mut(&mut |child| {
        if !child.metadata().contains(&MetaData::Prunable) {
            match child {
                WrappedNode::RuleLine { metadata, .. }
                | WrappedNode::Comment { metadata, .. }
                | WrappedNode::Syntax { metadata, .. }
                | WrappedNode::Directive { metadata, .. }
                | WrappedNode::ModuleStart { metadata, .. }
                | WrappedNode::ModuleEnd { metadata, .. } => metadata.push(MetaData::Prunable),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Cherry-picking helpers
// ---------------------------------------------------------------------------

/// Check whether a node's rule name matches one of the cherry-picked names.
///
/// Wanted names have already been normalized (alias prefix stripped
/// and generic parameter list stripped) by [`normalize_directive_name`].
/// The LHS extracted from the rule line may still carry its generic
/// parameter list (`wrapper<T>`); strip that too so generic templates
/// are recognized as cherry-picked.  Without this the resolver tags
/// every cherry-picked generic as prunable, which makes the
/// reachability pass drop the definition before it can be expanded.
fn rule_name_matches(
    node: &WrappedNode,
    wanted: &HashSet<String>,
) -> bool {
    match node {
        WrappedNode::RuleLine { text, .. } => {
            let lhs =
                normalize_rule_name(text.split_once('=').map_or(text.as_str(), |(lhs, _)| lhs));
            let lhs_base = lhs.split('<').next().unwrap_or(lhs.as_str()).trim();
            wanted.iter().any(|w| w == lhs_base)
        },
        _ => true, // keep comments, directives, markers — they're not rules
    }
}

/// Normalize a directive-selected rule name for matching.
pub(crate) fn normalize_directive_name(
    name: &str,
    alias: Option<&str>,
) -> String {
    let trimmed = name.trim();
    let unaliased = alias
        .and_then(|prefix| trimmed.strip_prefix(&format!("{prefix}.")))
        .unwrap_or(trimmed);
    normalize_rule_name(unaliased)
}

/// Normalize a rule name by trimming surrounding whitespace.
fn normalize_rule_name(name: &str) -> String {
    name.trim().to_owned()
}

// ---------------------------------------------------------------------------
// Alias wrapping
// ---------------------------------------------------------------------------

/// Prepend an alias prefix to all rule names in a resolved subtree.
///
/// This transforms `COSE_Key` into `alias.COSE_Key` for every top-level
/// rule in the subtree, preserving nested aliases when present.
fn wrap_with_alias(
    nodes: &mut [WrappedNode],
    alias: &str,
    local_rule_names: &HashSet<String>,
) {
    for node in nodes {
        wrap_with_alias_node(node, alias, local_rule_names);
    }
}

/// Recursively apply alias prefixes to rule and typename/groupname
/// nodes.
///
/// When the node is the body of a generic definition (a `RuleLine`
/// whose LHS carries a `<...>` parameter list), the alias-prefix
/// rewriting inside the body is **skipped**: generic bodies are
/// templates whose internal references (including private aliases
/// like `std.Wrapper` used by `.within`) are resolved at the
/// definition site, not at the consumer's call site.  Re-prefixing
/// those references with the consumer's alias would silently break
/// `.within` checks for imported generics.
fn wrap_with_alias_node(
    node: &mut WrappedNode,
    alias: &str,
    local_rule_names: &HashSet<String>,
) {
    wrap_with_alias_node_with_mode(node, alias, local_rule_names, ChildAliasMode::Normal);
}

/// Internal walker that respects a [`ChildAliasMode`].  Generic-body
/// mode preserves already-qualified typename/groupname references
/// inside generic definition bodies.
fn wrap_with_alias_node_with_mode(
    node: &mut WrappedNode,
    alias: &str,
    local_rule_names: &HashSet<String>,
    mode: ChildAliasMode,
) {
    match mode {
        ChildAliasMode::Normal => wrap_with_alias_normal(node, alias, local_rule_names),
        ChildAliasMode::GenericBody => wrap_with_alias_generic_body(node, alias, local_rule_names),
    }
}

/// Standard alias-prefix walker.  See [`wrap_with_alias_node`].
fn wrap_with_alias_normal(
    node: &mut WrappedNode,
    alias: &str,
    local_rule_names: &HashSet<String>,
) {
    match node {
        WrappedNode::RuleLine { text, children, .. } => {
            let trimmed = text.trim();
            // BUG-003 follow-on: the rule line's LHS may already
            // carry an importer's alias (e.g. `a.Wrapper<t>` after
            // the lib_a resolver wrapped the import).  The consumer's
            // wrap must not prepend the consumer's alias on top of
            // an existing one.  Detect the existing prefix by
            // checking the bare name before the first space or `=`.
            let lhs_name = trimmed
                .split_once([' ', '\t', '='])
                .map_or(trimmed, |(head, _)| head)
                .trim();
            let already_qualified = lhs_name.contains('.') || lhs_name == alias;
            if !already_qualified {
                *text = prepend_alias_to_rule_text(trimmed, alias);
            }
            let is_generic = is_generic_rule_text(text.trim());
            let child_mode = if is_generic {
                ChildAliasMode::GenericBody
            } else {
                ChildAliasMode::Normal
            };
            for child in children {
                wrap_with_alias_node_with_mode(child, alias, local_rule_names, child_mode);
            }
            // For generic definitions, the LHS typename (the
            // generic's own name) must also be rewritten so the
            // expansion pass can find the definition under the
            // alias-prefixed name.  Walk the children in generic-body
            // mode above deliberately skips this rewriting, so we
            // patch the LHS typename directly here.
            if is_generic {
                rewrite_generic_lhs_typename(node, alias, local_rule_names);
            }
        },
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            if rule == "typename" || rule == "groupname" {
                let name = text.trim();
                // BUG-003 follow-on: a reference like `a.Wrapper` is
                // already aliased by a transitive importer (lib_a
                // was imported as `a`); the consumer's wrap must not
                // prepend the consumer's alias on top of an
                // existing one.  `name.contains('.')` catches every
                // already-qualified reference regardless of which
                // alias the importer used, and `name == alias`
                // covers the (rare) self-aliasing case.
                let already_qualified = name.contains('.') || name == alias;
                if !already_qualified && local_rule_names.contains(name) {
                    *text = format!("{alias}.{name}");
                }
            }
            for child in children {
                wrap_with_alias_node_with_mode(
                    child,
                    alias,
                    local_rule_names,
                    ChildAliasMode::Normal,
                );
            }
        },
        WrappedNode::Directive { children, .. } => {
            // BUG-003 follow-on: a `Directive` node's children are the
            // already-resolved subtree of an imported/included file.
            // The imported subtree has already been wrapped with its
            // own alias prefix when the importer's resolver ran, so
            // walking back into it here would prepend the consumer's
            // alias on top of the importer's alias and turn
            // `a.Wrapper<t>` into `middle.a.Wrapper<t>`.  Once
            // double-prefixed, an instantiation like
            // `middle.via-alias = a.Wrapper<inner-type>` cannot find
            // the (single-prefixed) definition and the body clones
            // leave bare references to `tagged<t>` / `untagged<t>`,
            // which then surface as E016 in the finalizer.
            //
            // The Directive children are already in their final
            // consumer-shape form, so we leave them alone.
            let _ = (children, alias, local_rule_names);
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Alias walker used inside generic definition bodies.
///
/// Generic bodies are templates whose internal references resolve
/// against the *generic definition's own import scope*, not the
/// consumer's.  References that already carry a definition-site
/// alias prefix (e.g. `std.Wrapper`) are preserved verbatim so the
/// generic's `.within` RHS continues to resolve through the
/// library's own scope.  Bare references to private same-module
/// helpers (e.g. `protected-signed-coswid-header`) are re-prefixed
/// with the consumer's alias so the expansion matches the helper's
/// own definition-site key under that alias.
fn wrap_with_alias_generic_body(
    node: &mut WrappedNode,
    alias: &str,
    local_rule_names: &HashSet<String>,
) {
    match node {
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            if rule == "typename" || rule == "groupname" {
                let name = text.trim();
                // BUG-003: the typename text can carry generic
                // parameters (`tagged<t>`, `Wrapper<inner-type>`).
                // `local_rule_names` only carries the base name
                // (`tagged`), so a naive `contains(name)` lookup
                // misses every parameterized reference inside the
                // generic body and the alias prefix is never
                // applied.  Strip the generic-argument list before
                // comparing.  Also: if the base name already carries
                // a `.` (an importer used a different alias) or is
                // the alias itself, the reference is already
                // qualified and must not be re-prefixed with the
                // consumer's alias.
                let base_name = name
                    .split_once('<')
                    .map_or(name, |(head, _rest)| head.trim());
                let already_qualified = base_name.contains('.') || base_name == alias;
                if !already_qualified && local_rule_names.contains(base_name) {
                    *text = format!("{alias}.{name}");
                }
            }
            for child in children {
                wrap_with_alias_generic_body(child, alias, local_rule_names);
            }
        },
        WrappedNode::RuleLine { children, .. } | WrappedNode::Directive { children, .. } => {
            // BUG-003 follow-on: a nested `Directive` node carries the
            // already-wrapped subtree of an inner importer.  Walking
            // into it here would re-prefix the inner subtree with
            // the consumer's alias.  Leave it alone.
            let _ = children;
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// For a generic definition `RuleLine`, rewrite only the LHS typename
/// (the generic's own name) with the consumer's alias so expansion
/// can find the definition under the alias-prefixed name.  All other
/// typename/groupname text inside the body is left untouched so that
/// definition-site aliases survive expansion.
///
/// Non-generic `RuleLine`s nested inside the generic body (e.g. an
/// imported definition reached via an inner `Directive`) are left
/// untouched: their definitions need to keep their own alias
/// context unchanged so the wrapping happens at the next walk-up
/// level.
fn rewrite_generic_lhs_typename(
    node: &mut WrappedNode,
    alias: &str,
    local_rule_names: &HashSet<String>,
) {
    let WrappedNode::RuleLine { text, children, .. } = node else {
        return;
    };
    let trimmed = text.trim();
    if !is_generic_rule_text(trimmed) {
        return;
    }
    let Some(expr) = children.iter_mut().find_map(|child| {
        if let WrappedNode::Syntax { rule, children, .. } = child
            && rule == "expr"
        {
            return Some(children);
        }
        None
    }) else {
        return;
    };
    for expr_child in expr.iter_mut() {
        if let WrappedNode::Syntax { rule, text, .. } = expr_child
            && (rule == "typename" || rule == "groupname")
        {
            let name = text.trim();
            if !name.starts_with(alias) && local_rule_names.contains(name) {
                *text = format!("{alias}.{name}");
            }
            break;
        }
    }
}

/// Whether a child of a rule line should be processed in generic-body
/// mode (preserving definition-site aliases) or in normal mode
/// (re-prefixing typename references with the consumer's alias).
#[derive(Clone, Copy)]
enum ChildAliasMode {
    /// Apply the standard alias-prefix rewriting.
    Normal,
    /// Inside a generic definition body: preserve already-qualified
    /// typename/groupname references so definition-site aliases
    /// survive expansion unchanged.
    GenericBody,
}

/// Return whether a rule line's LHS carries a `<...>` generic
/// parameter list.  Used to detect generic definitions whose bodies
/// must keep their definition-site alias references.
fn is_generic_rule_text(rule_text: &str) -> bool {
    let name_end = rule_text.find([' ', '=', '\t']).unwrap_or(rule_text.len());
    let name = rule_text.get(..name_end).unwrap_or(rule_text);
    name.contains('<')
}

/// Collect the set of top-level rule names defined in a resolved subtree,
/// including rules nested inside directive children for transitive alias wrapping.
pub(crate) fn collect_rule_names(nodes: &[WrappedNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for node in nodes {
        collect_rule_names_recurse(node, &mut names);
    }
    names
}

/// Recursively collect rule names, walking into directive children.
fn collect_rule_names_recurse(
    node: &WrappedNode,
    names: &mut HashSet<String>,
) {
    if let Some(name) = top_level_rule_name(node) {
        names.insert(name);
    }
    match node {
        WrappedNode::RuleLine { children, .. } | WrappedNode::Directive { children, .. } => {
            for child in children {
                collect_rule_names_recurse(child, names);
            }
        },
        WrappedNode::Syntax { .. }
        | WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Extract the top-level rule name from a rule line.
fn top_level_rule_name(node: &WrappedNode) -> Option<String> {
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

/// Insert an alias prefix into a rule text before the typename.
fn prepend_alias_to_rule_text(
    rule_text: &str,
    alias: &str,
) -> String {
    // Rule text looks like "typename = type"
    // Find the first space or '=' or '<' to locate the typename end
    let name_end = rule_text
        .find([' ', '=', '<', '\t'])
        .unwrap_or(rule_text.len());
    let name = rule_text.get(..name_end).unwrap_or(rule_text);
    let rest = rule_text.get(name_end..).unwrap_or("");
    format!("{alias}.{name}{rest}")
}
