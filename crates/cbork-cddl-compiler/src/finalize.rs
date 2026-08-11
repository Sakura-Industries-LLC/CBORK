// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Final AST validation and surgical postlude injection.
//!
//! After constant calculation has stabilized, this pass stitches in only the
//! standard-postlude definitions that are actually referenced, compares any
//! overlapping user definitions with the authoritative postlude, and then
//! runs the ctlop validation pass against the physically complete tree.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use cbork_cddl_parser::modules::Directive;

use crate::{
    MetaData, WrappedNode,
    compiled::CompiledCDDL,
    ctlop::validate_ctlop_pass,
    error::{Diagnostic, DiagnosticLevel},
    node::SourceOrigin,
    resolver::{
        collect_rule_names, directive_alias, directive_display_name, directive_has_names,
        directive_names, normalize_directive_name,
    },
    resolver_cache::{EntryState, ResolverCache},
    semantic::{
        push_conflict_diagnostic, push_metadata, push_redundant_diagnostic,
        resolve_constants_in_nodes,
    },
    symbols::{AssignmentKind, SymbolKind, rule_head, rule_head_from_children, rule_name},
};

/// Normalizes definition strengths across the compiled CDDL.
///
/// Walks all top-level definitions and resolves collisions between `=` and `:=`
/// bindings, pruning weaker definitions and emitting diagnostics for conflicts.
pub(crate) fn normalize_definition_strengths(compiled: &mut CompiledCDDL) {
    let mut seen = HashMap::<String, NormalizedDefinitionSite>::new();
    let mut pending = Vec::new();
    let consumer_source_path = compiled.source_path.clone();
    let consumer_directive_names = compiled
        .imported_libraries
        .iter()
        .flat_map(|lib| lib.directive_names.iter().cloned())
        .collect::<HashSet<_>>();
    let consumer_references =
        collect_consumer_references(&compiled.user_nodes, &consumer_source_path);
    collect_definition_strength_actions(
        &compiled.user_nodes,
        &mut seen,
        &mut pending,
        &consumer_source_path,
        &consumer_directive_names,
        &consumer_references,
    );

    let mut prune_keys = HashSet::new();
    for action in pending {
        match action.kind {
            DefinitionActionKind::PruneOnly => {
                prune_keys.insert(action.target.key.clone());
            },
            DefinitionActionKind::Redundant => {
                push_metadata_for_definition_key(
                    &mut compiled.user_nodes,
                    &action.target.key,
                    MetaData::RedundantDefinition,
                );
                push_redundant_diagnostic(
                    &mut compiled.warnings,
                    &action.target.name,
                    &action.target.origin,
                    &action.target.span,
                    Some(&action.kept.origin),
                );
                prune_keys.insert(action.target.key.clone());
            },
            DefinitionActionKind::Conflict => {
                // BUG-004 follow-on: a strength-normalization conflict
                // between two definitions that come from
                // independently imported files (different
                // `source_path`) and neither of which is on the
                // consumer's direct surface is not a consumer-side
                // collision.  Two CBORK libraries can each use the
                // same private root name (e.g. `all`) without any
                // actual collision in the consumer's surface; the
                // consumer's direct references are independent of
                // each library's private root.
                //
                // The consumer's "direct surface" includes both its
                // own definitions and the names it explicitly
                // cherry-picked via `from ... import <name>,...`
                // directives; whole-file `import` (with no cherry
                // list) does NOT put the library's private roots on
                // the consumer's surface.
                let kept_in_consumer = action.kept.origin.source_path == consumer_source_path;
                let target_in_consumer = action.target.origin.source_path == consumer_source_path;
                let name_on_consumer_surface = consumer_directive_names
                    .contains(&action.target.name)
                    || consumer_references.contains(&action.target.name);
                let independent_imported_pair = !kept_in_consumer
                    && !target_in_consumer
                    && !name_on_consumer_surface
                    && action.kept.origin.source_path != action.target.origin.source_path;
                if independent_imported_pair {
                    continue;
                }
                push_metadata_for_definition_key(
                    &mut compiled.user_nodes,
                    &action.target.key,
                    MetaData::ConflictingDefinition,
                );
                push_conflict_diagnostic(
                    &mut compiled.warnings,
                    &action.target.name,
                    &action.target.origin,
                    &action.target.span,
                    Some(&action.kept.origin),
                );
                prune_keys.insert(action.target.key.clone());
            },
        }
    }

    if !prune_keys.is_empty() {
        prune_definition_keys(&mut compiled.user_nodes, &prune_keys);
    }
}

/// Materialize the physically complete tree and run the final validation pass.
#[allow(
    clippy::too_many_lines,
    reason = "finalize_compiled is a single linear pipeline; refactoring into helpers \
              would obscure the strict ordering required between the pruner, strength \
              walker, semantic pass, and postlude merge."
)]
pub(crate) fn finalize_compiled(compiled: &mut CompiledCDDL) {
    let mut postlude_cache = ResolverCache::new();
    loop {
        let before = postlude_cache.cnt_unresolved();
        let mut postlude_warnings = Vec::new();
        resolve_constants_in_nodes(
            &mut compiled.postlude_nodes,
            &mut postlude_warnings,
            &mut postlude_cache,
        );
        if postlude_cache.cnt_unresolved() == before {
            break;
        }
    }

    validate_socket_rules(compiled);

    // Snapshot the original (unpruned) user tree *before* the
    // reachability pruner mutates it.  Two consumers downstream need
    // information only available in the unpruned tree:
    //
    // * The pre-populated cache used by the postlude merge below.
    // * The set of top-level rule names visible to the file, used by the reference-resolution
    //   pass to decide whether a referenced name is "user-defined" (and therefore not in need
    //   of postlude injection) or genuinely undefined.
    let original_user_nodes = compiled.user_nodes.clone();
    let original_definition_names = collect_definition_names(&original_user_nodes);
    let original_generic_definition_names =
        collect_generic_definition_base_names(&original_user_nodes);

    // Prune unreferenced prunable definitions BEFORE any definition-strength
    // normalization.  Two weak imported definitions that the importer never
    // references would otherwise flag each other as conflicting and the
    // later reachability pruner would silently remove them anyway.
    let pruned = prune_unreachable_prunable_definitions(&compiled.user_nodes);
    compiled.user_nodes = pruned.nodes;
    for name in &pruned.names {
        compiled.resolved_types.prune(name);
    }

    // Pre-populate the cache with every typename reference seen in the
    // *original* (unpruned) user tree, so the postlude merge can still
    // discover references that the reachability pruner dropped.  This
    // only creates Unresolved entries — it never calls resolve so it
    // does not emit RedundantType warnings for the (strong, weak)
    // case the strength walker below has already pruned.
    //
    // We cannot just call `resolve_constants` on the unpruned tree
    // because the seed pass would flag every imported weak definition
    // as redundant against its strong counterpart *before* the
    // strength walker had a chance to silently drop the weaker one.
    // That would re-introduce the false-positive W001 the new ordering
    // was designed to avoid.
    //
    // The pre-populated cache is captured in a side cache because
    // `resolve_constants` constructs a fresh `ResolverCache`; we
    // re-attach the side cache's Unresolved entries below via
    // `ResolverCache::get` (which auto-creates Unresolved and never
    // calls `resolve`, so it cannot reject the side cache's payload).
    let mut pre_populated = ResolverCache::new();
    seed_cache_with_all_references(&original_user_nodes, &mut pre_populated);

    // Definition-strength normalization runs against the pruned tree.
    // Imported (weak) definitions that were pruned by the reachability
    // pass no longer participate in the (weak, weak) collision check;
    // the (strong, weak) and (weak, strong) cases still resolve the same
    // way they did before.
    normalize_definition_strengths(compiled);

    // Step 5.8: diagnose plain-vs-generic collisions on the pruned,
    // strength-normalized tree so unreferenced weak imported generic
    // helpers do not spuriously collide with a strong local plain rule.
    //
    // We collect candidate collisions from the *pre-prune* tree (so
    // generic definitions are still visible even when their only call
    // site has been inlined and they would otherwise be pruned as
    // orphaned templates), then filter out any pair where at least one
    // side was pruned by the reachability pass.
    let pruned_keys = &pruned.keys;
    detect_plain_generic_collisions(
        &original_user_nodes,
        &mut compiled.warnings,
        pruned_keys,
        &compiled.source_path,
    );

    // The semantic fixed-point pass also runs against the pruned and
    // strength-normalized tree so it does not see the same definition
    // twice.  Running it before this point would let the cache-level
    // redundancy detector flag every imported weak definition as
    // redundant against its strong counterpart before the strength
    // check had a chance to silently drop the weaker one.
    let mut resolved_types = crate::semantic::resolve_constants(compiled);
    // Re-attach the pre-populated `Unresolved` entries so the
    // postlude merge can see references that the reachability pruner
    // dropped.  `ResolverCache::get` auto-creates an Unresolved
    // entry on first access; if the seed pass already resolved the
    // name, `get` simply returns the existing value.  We do not call
    // `resolve` here because the side cache holds `Unresolved` states
    // and `resolve` explicitly rejects them — the only way to merge
    // a side-cache entry into the live cache without changing its
    // state is via `get`.
    for (name, _pre_entry) in pre_populated.iter() {
        let _ = resolved_types.get(name);
    }
    compiled.resolved_types = resolved_types;

    let mut user_definition_names = collect_definition_names(&compiled.user_nodes);
    // Names of top-level rule definitions that were visible in the
    // *original* (unpruned) user tree but got pruned out of reach
    // are still considered "user-defined" for the purposes of
    // `handle_reference`'s "is this name defined somewhere?" check.
    // Without this, references that the reachability pruner dropped
    // (e.g. `empty_or_serialized_map` imported from rfc9052 and only
    // referenced from a kept rule) would be reported as E016
    // undefined-reference errors even though the name was present
    // in the original tree.
    //
    // We only count *top-level rule* names, not free typename
    // references — a reference to `foo` should not by itself
    // prevent an E016 from being emitted when `foo` is not defined
    // anywhere in the file.
    for name in &original_definition_names {
        user_definition_names.insert(name.clone());
    }
    validate_extern_declarations(compiled);
    let postlude_definitions = collect_definition_nodes(&compiled.postlude_nodes);
    let mut combined_cache = compiled.resolved_types.clone();
    merge_postlude_values_for_resolution(
        &mut combined_cache,
        &postlude_cache,
        &user_definition_names,
    );
    // Replay the user tree against the combined cache on a scratch clone so
    // we can learn any postlude-backed resolutions without re-emitting the
    // warnings that were already produced during the main semantic pass.
    let mut replay_nodes = compiled.user_nodes.clone();
    let mut replay_warnings = Vec::new();
    resolve_constants_in_nodes(&mut replay_nodes, &mut replay_warnings, &mut combined_cache);
    compiled.resolved_types = combined_cache;

    let mut complete_nodes = compiled.user_nodes.clone();
    compare_user_and_postlude_definitions(
        compiled,
        &mut complete_nodes,
        &user_definition_names,
        &postlude_definitions,
        &postlude_cache,
    );
    let mut injected_names = HashSet::new();
    let mut seen_missing = HashSet::new();
    let mut injection = InjectionContext {
        user_definition_names: &user_definition_names,
        postlude_definitions: &postlude_definitions,
        complete_nodes: &mut complete_nodes,
        injected_names: &mut injected_names,
        seen_missing: &mut seen_missing,
        generic_definition_names: &original_generic_definition_names,
        changed: false,
        is_library: compiled.is_library,
        extern_names: &compiled.extern_names,
        warnings: &mut compiled.warnings,
    };

    loop {
        let snapshot = injection.complete_nodes.clone();
        injection.changed = false;

        for node in &snapshot {
            let mut lhs_seen = false;
            scan_and_inject_references(node, &mut lhs_seen, &mut injection);
        }

        if !injection.changed {
            break;
        }
    }

    merge_injected_postlude_values(
        &mut compiled.resolved_types,
        &postlude_cache,
        &injected_names,
    );

    validate_ctlop_pass(
        complete_nodes.as_mut_slice(),
        &mut compiled.resolved_types,
        &mut compiled.warnings,
    );

    crate::ctlop::warn_serialization_weaker_inner(&complete_nodes, &mut compiled.warnings);

    crate::within::validate_within_pass(&complete_nodes, &mut compiled.warnings);

    detect_group_reference_cycles(&complete_nodes, &mut compiled.warnings);

    detect_direct_export_violations(
        &original_user_nodes,
        complete_nodes.as_slice(),
        &compiled.imported_libraries,
        &compiled.extern_names,
        &compiled.source_path,
        &compiled.raw_source,
        &mut compiled.warnings,
    );

    detect_non_library_imports(&compiled.imported_libraries, &mut compiled.warnings);

    detect_unused_directives(
        &original_user_nodes,
        complete_nodes.as_slice(),
        &compiled.imported_libraries,
        &compiled.extern_names,
        &compiled.source_path,
        &mut compiled.warnings,
    );

    compiled.complete_nodes = complete_nodes;
}

/// Validate socket rule assignment compatibility.
/// Validates that socket (plug/socket) references resolve correctly.
fn validate_socket_rules(compiled: &mut CompiledCDDL) {
    let mut seen = HashSet::new();
    validate_socket_rules_in_nodes(&compiled.user_nodes, &mut compiled.warnings, &mut seen);
}

/// Recursively walks nodes and validates socket-like `$xxx` references.
fn validate_socket_rules_in_nodes(
    nodes: &[WrappedNode],
    warnings: &mut Vec<Diagnostic>,
    seen: &mut HashSet<MissingReferenceKey>,
) {
    for node in nodes {
        if let Some(head) = rule_head(node) {
            validate_socket_rule(node, &head, warnings, seen);
        }

        match node {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Directive { children, .. }
            | WrappedNode::Syntax { children, .. } => {
                validate_socket_rules_in_nodes(children, warnings, seen);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Validates a single socket rule, checking that its `$xxx` reference targets
/// a valid assignment.
fn validate_socket_rule(
    node: &WrappedNode,
    head: &crate::RuleHead,
    warnings: &mut Vec<Diagnostic>,
    seen: &mut HashSet<MissingReferenceKey>,
) {
    let valid = match (head.kind, head.assignment) {
        (SymbolKind::TypeSocket, AssignmentKind::TypeAugment)
        | (SymbolKind::GroupSocket, AssignmentKind::GroupAugment)
        | (SymbolKind::Type | SymbolKind::Group, _) => true,
        (SymbolKind::TypeSocket | SymbolKind::GroupSocket, _) => false,
    };

    if valid {
        return;
    }

    let (expected, used) = match (head.kind, head.assignment) {
        (SymbolKind::TypeSocket, AssignmentKind::GroupAugment) => ("/=", "//="),
        (SymbolKind::TypeSocket, AssignmentKind::Define) => ("/=", "="),
        (SymbolKind::GroupSocket, AssignmentKind::TypeAugment) => ("//=", "/="),
        (SymbolKind::GroupSocket, AssignmentKind::Define) => ("//=", "="),
        _ => return,
    };

    let origin = node.origin();
    let span = definition_span(node);
    let key = MissingReferenceKey {
        source_path: origin.source_path.clone(),
        span: span.clone(),
        name: format!("{}:{used}", head.name),
    };
    if seen.insert(key) {
        warnings.push(Diagnostic {
            code: "E017",
            level: DiagnosticLevel::Error,
            message: format!(
                "socket `{}` must be extended with `{expected}`, found `{used}` at {}:{}:{}",
                head.name,
                origin.source_path.display(),
                origin.line,
                origin.column
            ),
            source_file: Some(origin.source_path.clone()),
            span: Some(span),
            previous_origin: None,
            related: Vec::new(),
        });
    }
}

/// A top-level user rule definition site in the current source file.
///
/// Used by [`detect_unreferenced_top_level_definitions`] to compute
/// per-file reachability and emit E020 diagnostics for definitions
/// that the current source file itself never references.
#[derive(Clone)]
struct DefinitionSite {
    /// Rule name.
    name: String,
    /// Source origin of this definition.
    origin: SourceOrigin,
    /// Source span of this definition.
    span: Range<usize>,
}

/// A definition site whose strength has been normalized for comparison.
#[derive(Clone)]
struct NormalizedDefinitionSite {
    /// Key used to identify and de-duplicate this definition.
    key: DefinitionKey,
    /// The definition name (rule name).
    name: String,
    /// CDDL signature of the right-hand side.
    signature: String,
    /// Source location information.
    origin: SourceOrigin,
    /// Byte range in the source file.
    span: Range<usize>,
    /// Whether this definition is prunable (not `:=`).
    is_prunable: bool,
}

/// Unique key for identifying a definition across files.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DefinitionKey {
    /// The definition (rule) name.
    name: String,
    /// Absolute path to the source file.
    source_path: PathBuf,
    /// 1-based line number.
    line: usize,
    /// 1-based column number.
    column: usize,
}

/// The kind of action to take when two definitions collide.
enum DefinitionActionKind {
    /// Remove the target definition without further diagnostic.
    PruneOnly,
    /// The kept definition has a stronger binding; the target is redundant.
    Redundant,
    /// Both definitions have the same strength; this is a conflict.
    Conflict,
}

/// A resolved action for a pair of colliding definition sites.
struct DefinitionAction {
    /// The kind of resolution.
    kind: DefinitionActionKind,
    /// The definition site to keep.
    kept: NormalizedDefinitionSite,
    /// The definition site that is pruned or diagnosed.
    target: NormalizedDefinitionSite,
}

/// Recursively collects definition-strength resolution actions for all nodes.
///
/// The strength matrix is:
///
/// * `(strong, weak)` and `(weak, strong)` — the weaker side is silently pruned
///   (`PruneOnly`) and the stronger one is kept.  This is the "importer-wins" path for
///   `;@ CBORK: Library` files.
/// * `(weak, weak)` with matching signatures — `Redundant` warning.
/// * `(weak, weak)` with different signatures — `Conflict` error.
/// * `(strong, strong)` with matching signatures — `Redundant` (multiple identical `=`
///   definitions are flagged but neither is auto-pruned).
/// * `(strong, strong)` with different signatures — `Conflict` error. Two strong
///   definitions with different signatures cannot be reconciled and must be reported.
///
/// All actions operate on the **post-pruning** tree (see
/// [`prune_unreachable_prunable_definitions`]), so unreferenced weak
/// definitions never participate in the `(weak, weak)` case.
fn collect_definition_strength_actions(
    nodes: &[WrappedNode],
    seen: &mut HashMap<String, NormalizedDefinitionSite>,
    pending: &mut Vec<DefinitionAction>,
    consumer_source_path: &std::path::Path,
    consumer_directive_names: &HashSet<String>,
    consumer_references: &HashSet<String>,
) {
    for node in nodes {
        let Some(site) = normalized_definition_site(node) else {
            recurse_into(
                node,
                seen,
                pending,
                consumer_source_path,
                consumer_directive_names,
                consumer_references,
            );
            continue;
        };

        if let Some(previous) = seen.get(&site.name).cloned() {
            // Same source origin (file + line + column) reached through
            // a different import path is idempotent, not a redundant
            // definition.  Two distinct import paths can converge on
            // the same canonical rule, and the second arrival must not
            // surface a W001.
            if previous.key == site.key {
                recurse_into(
                    node,
                    seen,
                    pending,
                    consumer_source_path,
                    consumer_directive_names,
                    consumer_references,
                );
                continue;
            }
            // BUG-004 follow-on: a conflict between two definitions
            // that come from independently imported files (different
            // `source_path`) and neither of which lives on the
            // consumer's direct surface is not a consumer-side
            // collision.  Two CBORK libraries can each use the same
            // private root name (e.g. `all`) without any actual
            // collision in the consumer's surface; the consumer's
            // direct references are independent of each library's
            // private root.  Skip the action entirely so neither a
            // Conflict nor a Redundant diagnostic is emitted for the
            // pair; the (strong, weak) → PruneOnly path is also
            // suppressed so the imported prunable definition is left
            // intact (the consumer does not directly reference it).
            //
            // The consumer's "direct surface" includes:
            //   * its own definitions (origin matches consumer);
            //   * names it explicitly cherry-picked via `from ... import <name>,...` directives;
            //   * names referenced by the consumer's own definitions.
            let previous_in_consumer = previous.origin.source_path == consumer_source_path;
            let site_in_consumer = site.origin.source_path == consumer_source_path;
            let name_on_consumer_surface = consumer_directive_names.contains(&site.name)
                || consumer_references.contains(&site.name);
            let independent_imported_pair = !previous_in_consumer
                && !site_in_consumer
                && !name_on_consumer_surface
                && previous.origin.source_path != site.origin.source_path;
            if independent_imported_pair {
                continue;
            }
            // The strength pair decides the action:
            //   (strong, weak) | (weak, strong)  ->  PruneOnly
            //   else, matching signatures         ->  Redundant
            //   else, differing signatures         ->  Conflict
            let strength_pair_mixed = previous.is_prunable != site.is_prunable;
            let signatures_match = previous.signature == site.signature;

            let (kind, kept, target) = if strength_pair_mixed {
                let (kept, target) = if previous.is_prunable {
                    (site.clone(), previous.clone())
                } else {
                    (previous.clone(), site.clone())
                };
                (DefinitionActionKind::PruneOnly, kept, target)
            } else if signatures_match {
                (
                    DefinitionActionKind::Redundant,
                    previous.clone(),
                    site.clone(),
                )
            } else {
                (
                    DefinitionActionKind::Conflict,
                    previous.clone(),
                    site.clone(),
                )
            };

            pending.push(DefinitionAction { kind, kept, target });

            // For (weak, strong), the strong site becomes the new
            // first-seen definition; subsequent collisions are measured
            // against it.  Other cases leave `seen` unchanged.
            if strength_pair_mixed && previous.is_prunable {
                seen.insert(site.name.clone(), site);
            }
        } else {
            seen.insert(site.name.clone(), site);
        }

        recurse_into(
            node,
            seen,
            pending,
            consumer_source_path,
            consumer_directive_names,
            consumer_references,
        );
    }
}

/// Walk into the children of a non-rule node, continuing the strength
/// collection.
fn recurse_into(
    node: &WrappedNode,
    seen: &mut HashMap<String, NormalizedDefinitionSite>,
    pending: &mut Vec<DefinitionAction>,
    consumer_source_path: &std::path::Path,
    consumer_directive_names: &HashSet<String>,
    consumer_references: &HashSet<String>,
) {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            collect_definition_strength_actions(
                children,
                seen,
                pending,
                consumer_source_path,
                consumer_directive_names,
                consumer_references,
            );
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Creates a normalized definition site from a node, if it represents a top-level
/// definition.
fn normalized_definition_site(node: &WrappedNode) -> Option<NormalizedDefinitionSite> {
    let WrappedNode::RuleLine {
        children: _,
        origin,
        span,
        ..
    } = node
    else {
        return None;
    };

    let head = rule_head(node)?;
    if head.assignment != AssignmentKind::Define {
        return None;
    }
    let name = head.name;
    let signature = rule_signature(node);
    let is_prunable = node.metadata().contains(&MetaData::Prunable);

    Some(NormalizedDefinitionSite {
        key: DefinitionKey {
            name: name.clone(),
            source_path: origin.source_path.clone(),
            line: origin.line,
            column: origin.column,
        },
        name,
        signature,
        origin: origin.clone(),
        span: span.clone(),
        is_prunable,
    })
}

/// Pushes metadata onto a definition by its key, recording action information.
fn push_metadata_for_definition_key(
    nodes: &mut [WrappedNode],
    key: &DefinitionKey,
    metadata: MetaData,
) {
    for node in nodes {
        let node_name = top_level_rule_name(node);
        match node {
            WrappedNode::RuleLine {
                children,
                origin,
                metadata: node_metadata,
                ..
            } => {
                if node_name.as_deref() == Some(key.name.as_str())
                    && origin.source_path == key.source_path
                    && origin.line == key.line
                    && origin.column == key.column
                {
                    push_metadata(node_metadata, metadata);
                }
                push_metadata_for_definition_key(children, key, metadata);
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                push_metadata_for_definition_key(children, key, metadata);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Prunes definition keys from the tree based on resolved actions.
fn prune_definition_keys(
    nodes: &mut Vec<WrappedNode>,
    prune_keys: &HashSet<DefinitionKey>,
) {
    nodes.retain_mut(|node| retain_definition_node(node, prune_keys));
}

/// Retains a definition node that should survive pruning.
fn retain_definition_node(
    node: &mut WrappedNode,
    prune_keys: &HashSet<DefinitionKey>,
) -> bool {
    let node_name = top_level_rule_name(node);
    match node {
        WrappedNode::RuleLine {
            children, origin, ..
        } => {
            if let Some(name) = node_name {
                let key = DefinitionKey {
                    name,
                    source_path: origin.source_path.clone(),
                    line: origin.line,
                    column: origin.column,
                };
                if prune_keys.contains(&key) {
                    return false;
                }
            }
            children.retain_mut(|child| retain_definition_node(child, prune_keys));
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            children.retain_mut(|child| retain_definition_node(child, prune_keys));
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
    true
}

/// Build a structural definition site for a rule node, scoped to the
/// current source file's own top-level definitions.
///
/// Used only by [`detect_unreferenced_top_level_definitions`] to walk
/// the user-defined rule list and figure out which entries the current
/// source file references.
fn definition_site(node: &WrappedNode) -> Option<DefinitionSite> {
    let WrappedNode::RuleLine {
        children: _,
        origin,
        span,
        ..
    } = node
    else {
        return None;
    };

    let head = rule_head(node)?;
    if head.assignment != AssignmentKind::Define {
        return None;
    }
    let name = head.name;

    Some(DefinitionSite {
        name,
        origin: origin.clone(),
        span: span.clone(),
    })
}

/// Detects unreferenced top-level definitions and emits diagnostics.
pub(crate) fn detect_unreferenced_top_level_definitions(compiled: &mut CompiledCDDL) {
    let sites =
        current_source_top_level_definition_sites(&compiled.user_nodes, &compiled.source_path);
    if sites.len() <= 1 {
        return;
    }

    let root_name = match sites.first() {
        Some(site) => site.name.clone(),
        None => return,
    };
    let edges = current_source_top_level_edges(&compiled.user_nodes, &compiled.source_path);
    let reachable = current_source_reachable_top_level_names(&edges, &root_name);
    let order = sites
        .iter()
        .enumerate()
        .map(|(index, site)| (site.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let site_by_name = sites
        .iter()
        .map(|site| (site.name.clone(), site.clone()))
        .collect::<HashMap<_, _>>();
    let unreachable = sites
        .iter()
        .skip(1)
        .filter(|site| !site.name.starts_with('$') && !reachable.contains(&site.name))
        .map(|site| site.name.clone())
        .collect::<HashSet<_>>();
    let component_roots = disconnected_component_roots(&edges, &unreachable, &order);

    let level = if compiled.is_library {
        DiagnosticLevel::Warning
    } else {
        DiagnosticLevel::Error
    };

    for name in component_roots {
        let Some(site) = site_by_name.get(&name) else {
            continue;
        };
        compiled.warnings.push(Diagnostic {
            code: "E020",
            level,
            message: format!(
                "unreferenced top-level definition `{}` at {}:{}:{}",
                site.name,
                site.origin.source_path.display(),
                site.origin.line,
                site.origin.column
            ),
            source_file: Some(site.origin.source_path.clone()),
            span: Some(site.span.clone()),
            previous_origin: None,
            related: Vec::new(),
        });
    }
}

/// Collects all top-level definition sites in the current source file.
fn current_source_top_level_definition_sites(
    nodes: &[WrappedNode],
    source_path: &std::path::Path,
) -> Vec<DefinitionSite> {
    let mut sites = Vec::new();
    let mut seen_names = HashSet::new();
    for node in nodes {
        let WrappedNode::RuleLine { origin, .. } = node else {
            continue;
        };
        if origin.source_path != source_path {
            continue;
        }
        if let Some(site) = definition_site(node) {
            if !seen_names.insert(site.name.clone()) {
                continue;
            }
            sites.push(site);
        }
    }
    sites
}

/// Computes the set of top-level names reachable via edges starting from root.
fn current_source_reachable_top_level_names(
    edges: &HashMap<String, HashSet<String>>,
    root_name: &str,
) -> HashSet<String> {
    let mut reachable = HashSet::new();
    let mut stack = vec![root_name.to_owned()];
    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        let Some(references) = edges.get(&name) else {
            continue;
        };
        for reference in references {
            if !reachable.contains(reference) {
                stack.push(reference.clone());
            }
        }
    }
    reachable
}

/// Builds a graph of top-level rule references within the current source file.
fn current_source_top_level_edges(
    nodes: &[WrappedNode],
    source_path: &std::path::Path,
) -> HashMap<String, HashSet<String>> {
    let mut edges = HashMap::<String, HashSet<String>>::new();
    for node in nodes {
        let WrappedNode::RuleLine { origin, .. } = node else {
            continue;
        };
        if origin.source_path != source_path {
            continue;
        }
        let Some(head) = rule_head(node) else {
            continue;
        };
        let mut references = HashSet::new();
        collect_top_level_rule_references(node, &mut references);
        edges.entry(head.name).or_default().extend(references);
    }
    edges
}

/// Finds disconnected components among unreachable definitions.
fn disconnected_component_roots(
    edges: &HashMap<String, HashSet<String>>,
    unreachable: &HashSet<String>,
    order: &HashMap<String, usize>,
) -> Vec<String> {
    if unreachable.is_empty() {
        return Vec::new();
    }

    let sccs = strongly_connected_components(edges, unreachable);
    let mut component_index = HashMap::<String, usize>::new();
    for (index, component) in sccs.iter().enumerate() {
        for name in component {
            component_index.insert(name.clone(), index);
        }
    }

    let mut has_incoming = vec![false; sccs.len()];
    for (name, references) in edges {
        let Some(&from) = component_index.get(name) else {
            continue;
        };
        for reference in references {
            let Some(&to) = component_index.get(reference) else {
                continue;
            };
            if from != to
                && let Some(entry) = has_incoming.get_mut(to)
            {
                *entry = true;
            }
        }
    }

    let mut roots = Vec::new();
    for (index, component) in sccs.into_iter().enumerate() {
        if has_incoming.get(index).copied().unwrap_or(false) {
            continue;
        }
        if let Some(first) = component
            .into_iter()
            .min_by_key(|name| order.get(name).copied().unwrap_or(usize::MAX))
        {
            roots.push(first);
        }
    }

    roots.sort_by_key(|name| order.get(name).copied().unwrap_or(usize::MAX));
    roots
}

/// Computes strongly connected components using Tarjan's algorithm.
fn strongly_connected_components(
    edges: &HashMap<String, HashSet<String>>,
    nodes: &HashSet<String>,
) -> Vec<Vec<String>> {
    struct Tarjan<'a> {
        edges: &'a HashMap<String, HashSet<String>>,
        nodes: &'a HashSet<String>,
        index: usize,
        indices: HashMap<String, usize>,
        lowlink: HashMap<String, usize>,
        stack: Vec<String>,
        on_stack: HashSet<String>,
        result: Vec<Vec<String>>,
    }

    impl Tarjan<'_> {
        fn visit(
            &mut self,
            node: &str,
        ) {
            let node_name = node.to_owned();
            self.indices.insert(node_name.clone(), self.index);
            self.lowlink.insert(node_name.clone(), self.index);
            self.index = self.index.wrapping_add(1);
            self.stack.push(node_name.clone());
            self.on_stack.insert(node_name.clone());

            for neighbor in self
                .edges
                .get(node)
                .into_iter()
                .flat_map(|neighbors| neighbors.iter())
            {
                if !self.nodes.contains(neighbor) {
                    continue;
                }
                if !self.indices.contains_key(neighbor) {
                    self.visit(neighbor);
                    let low = self.lowlink.get(neighbor).copied().unwrap_or(usize::MAX);
                    if let Some(entry) = self.lowlink.get_mut(node) {
                        *entry = (*entry).min(low);
                    }
                } else if self.on_stack.contains(neighbor) {
                    let neighbor_index = self.indices.get(neighbor).copied().unwrap_or(usize::MAX);
                    if let Some(entry) = self.lowlink.get_mut(node) {
                        *entry = (*entry).min(neighbor_index);
                    }
                }
            }

            let lowlink_val = self.lowlink.get(node).copied().unwrap_or(usize::MAX);
            let indices_val = self.indices.get(node).copied().unwrap_or(usize::MAX);
            if lowlink_val == indices_val {
                let mut component = Vec::new();
                while let Some(top) = self.stack.pop() {
                    self.on_stack.remove(&top);
                    component.push(top.clone());
                    if top == node {
                        break;
                    }
                }
                self.result.push(component);
            }
        }
    }

    let mut tarjan = Tarjan {
        edges,
        nodes,
        index: 0,
        indices: HashMap::new(),
        lowlink: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        result: Vec::new(),
    };

    for node in nodes {
        if !tarjan.indices.contains_key(node) {
            tarjan.visit(node);
        }
    }

    tarjan.result
}

/// Detect cycles through bare group references inside rule bodies
/// (Step 5.10).  Builds a graph from each top-level rule to the
/// names of any bare group references in its body and reports
/// strongly connected components with more than one member as E030
/// diagnostics.
///
/// A cycle through bare group references would otherwise cause the
/// concrete renderer to stack-overflow when expanding one of the
/// participating rules.  Detecting the cycle statically means we
/// surface a clean diagnostic instead of letting the render path
/// silently break with a placeholder.
fn detect_group_reference_cycles(
    nodes: &[WrappedNode],
    warnings: &mut Vec<crate::Diagnostic>,
) {
    let mut rule_names: HashSet<String> = HashSet::new();
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
    for node in nodes {
        let WrappedNode::RuleLine { children, .. } = node else {
            continue;
        };
        let Some(head) = rule_head(node) else {
            continue;
        };
        if head.assignment != crate::symbols::AssignmentKind::Define {
            continue;
        }
        rule_names.insert(head.name.clone());
        let mut refs = HashSet::new();
        collect_bare_group_references(children, &mut refs);
        for reference in refs {
            if head.name != reference {
                edges
                    .entry(head.name.clone())
                    .or_default()
                    .insert(reference);
            }
        }
    }
    let sccs = strongly_connected_components(&edges, &rule_names);
    let mut reported: HashSet<(String, String)> = HashSet::new();
    for component in sccs {
        if component.len() < 2 {
            continue;
        }
        let mut sorted = component.clone();
        sorted.sort();
        let pair_key = match sorted.as_slice() {
            [first, second, ..] => (first.clone(), second.clone()),
            _ => continue,
        };
        if !reported.insert(pair_key) {
            continue;
        }
        let cycle_text = sorted.join(" -> ");
        let first = sorted.first().cloned().unwrap_or_default();
        warnings.push(crate::Diagnostic {
            code: "E030",
            level: crate::DiagnosticLevel::Error,
            message: format!("recursive group reference cycle: {cycle_text} -> {first}"),
            source_file: None,
            span: None,
            previous_origin: None,
            related: Vec::new(),
        });
    }
}

/// Walk a rule body's children and collect the names of any bare
/// group references.  CDDL grammar produces a bare group reference
/// as a `grpent` whose only meaningful descendant is a `typename`
/// chain (no `memberkey`, no `ctlop`, no parenthesized inner
/// group).  Both the bare-typename form (`{ GroupName, ... }`) and
/// Walk a rule body's children and collect the names of any bare
/// group references.  A bare group reference is a `grpent` whose
/// parent is a `grpchoice` (i.e. a top-level group element of a
/// group definition), and whose own body is a bare typename chain
/// with no memberkey, operator, or parenthesized inner group.
///
/// This deliberately ignores type references that appear as map
/// values (e.g. `{ key => { properties } }`) because those are
/// normal type references, not CDDL group inclusions.
fn collect_bare_group_references(
    children: &[WrappedNode],
    out: &mut HashSet<String>,
) {
    for child in children {
        collect_bare_group_references_inner(child, false, out);
    }
}

/// Recursive walker for [`collect_bare_group_references`].  The
/// `inside_memberkey` flag tracks whether the current node is
/// descended from a `memberkey` (i.e. it sits on the *value* side of
/// a `key => value` or `key : value`).  Type references inside a
/// memberkey are map values, not bare group references, and must be
/// ignored.
fn collect_bare_group_references_inner(
    node: &WrappedNode,
    inside_memberkey: bool,
    out: &mut HashSet<String>,
) {
    if let WrappedNode::Syntax { rule, children, .. } = node {
        match rule.as_str() {
            "grpchoice" => {
                // Direct children of a grpchoice at the top level of
                // a group body are the bare group references.  Walk
                // with `inside_memberkey` unchanged so inner grpents
                // inside value-side expressions are still
                // recognised as type references, not group inclusions.
                for c in children {
                    collect_bare_group_references_inner(c, inside_memberkey, out);
                }
            },
            "grpent" => {
                // Inside a grpent, look for a single bare typename
                // chain.  A grpent with a memberkey is a key/value
                // entry, not a group reference.
                let has_memberkey = children.iter().any(|c| {
                    matches!(
                        c,
                        WrappedNode::Syntax { rule, .. }
                            if rule == "memberkey"
                    )
                });
                if !inside_memberkey
                    && !has_memberkey
                    && let Some(name) = bare_typename_in_grpent(children)
                {
                    out.insert(name);
                }
                let next_inside_memberkey = inside_memberkey || has_memberkey;
                for c in children {
                    collect_bare_group_references_inner(c, next_inside_memberkey, out);
                }
            },
            _ => {
                for c in children {
                    collect_bare_group_references_inner(c, inside_memberkey, out);
                }
            },
        }
    }
}

/// Return `Some(name)` if a `grpent` is a bare single-identifier
/// reference to a type or group (no key, no operator, no parenthesized
/// body).  Returns `None` for key/value entries or structured grpents.
fn bare_typename_in_grpent(children: &[WrappedNode]) -> Option<String> {
    // A bare reference grpent has the shape:
    //   grpent
    //     type
    //       type1
    //         type2
    //           typename <name>
    // with no other meaningful children.
    let mut typename: Option<String> = None;
    for child in children {
        match child {
            WrappedNode::Syntax { rule, .. }
                if matches!(
                    rule.as_str(),
                    "occur" | "memberkey" | "ctlop" | "optcom" | "assignt"
                ) =>
            {
                return None;
            },
            WrappedNode::Syntax { rule, .. } if rule == "type" => {
                let name = single_typename_in_type(child)?;
                if typename.is_some() && typename.as_deref() != Some(name.as_str()) {
                    return None;
                }
                typename = Some(name);
            },
            _ => {},
        }
    }
    typename
}

/// If a `type` node contains exactly one `typename` leaf, return
/// that name.  Recurses through `type1` / `type2` wrappers.
fn single_typename_in_type(node: &WrappedNode) -> Option<String> {
    let WrappedNode::Syntax {
        rule,
        children,
        text,
        ..
    } = node
    else {
        return None;
    };
    match rule.as_str() {
        "typename" | "groupname" => Some(text.trim().to_owned()),
        "type" | "type1" | "type2" => {
            let mut found: Option<String> = None;
            for child in children {
                if let Some(name) = single_typename_in_type(child) {
                    if found.is_some() && found.as_deref() != Some(name.as_str()) {
                        return None;
                    }
                    found = Some(name);
                } else if !is_trivial_type_wrapper(child) {
                    return None;
                }
            }
            found
        },
        _ => None,
    }
}

/// Return `true` for node kinds that are structurally transparent
/// when descending into a type chain (comments and module framing).
fn is_trivial_type_wrapper(node: &WrappedNode) -> bool {
    matches!(
        node,
        WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. }
    )
}

/// Collects top-level rule references from a set of nodes.
fn collect_top_level_rule_references(
    node: &WrappedNode,
    references: &mut HashSet<String>,
) {
    let WrappedNode::RuleLine { children, .. } = node else {
        return;
    };

    let local_generic_params = top_level_generic_params(children);
    let mut lhs_seen = false;
    for child in children {
        if matches!(child, WrappedNode::Syntax { .. }) {
            collect_top_level_rule_references_node(
                child,
                &mut lhs_seen,
                &local_generic_params,
                references,
            );
        }
    }
}

/// Collects top-level rule references within a single node.
fn collect_top_level_rule_references_node(
    node: &WrappedNode,
    lhs_seen: &mut bool,
    local_generic_params: &HashSet<String>,
    references: &mut HashSet<String>,
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
                if *lhs_seen {
                    if !local_generic_params.contains(name) {
                        references.insert(name.to_owned());
                    }
                } else {
                    *lhs_seen = true;
                }
            }

            for child in children {
                collect_top_level_rule_references_node(
                    child,
                    lhs_seen,
                    local_generic_params,
                    references,
                );
            }
        },
        WrappedNode::RuleLine { .. }
        | WrappedNode::Directive { .. }
        | WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Extracts generic parameter names from the children of a top-level rule.
fn top_level_generic_params(children: &[WrappedNode]) -> HashSet<String> {
    for child in children {
        let WrappedNode::Syntax {
            rule,
            children: expr_children,
            ..
        } = child
        else {
            continue;
        };
        if rule != "expr" {
            continue;
        }

        for expr_child in expr_children {
            let WrappedNode::Syntax {
                rule,
                text,
                children,
                ..
            } = expr_child
            else {
                continue;
            };
            if rule == "genericparm" {
                return generic_param_names(text, children);
            }
        }
    }
    HashSet::new()
}

/// Returns the set of generic parameter names used in a type expression.
fn generic_param_names(
    text: &str,
    children: &[WrappedNode],
) -> HashSet<String> {
    let mut names = HashSet::new();
    for child in children {
        let WrappedNode::Syntax { rule, text, .. } = child else {
            continue;
        };
        if rule == "id" {
            names.insert(text.trim().to_owned());
        }
    }

    if !names.is_empty() {
        return names;
    }

    let trimmed = text.trim().trim_start_matches('<').trim_end_matches('>');
    for part in trimmed.split(',') {
        let name = part.trim();
        if !name.is_empty() {
            names.insert(name.to_owned());
        }
    }
    names
}

/// Generate a whitespace-insensitive structural signature for a rule body.
fn rule_signature(node: &WrappedNode) -> String {
    let mut out = String::new();
    if let WrappedNode::RuleLine { children, .. } = node {
        signature_ruleline_rhs(children, &mut out);
    } else {
        signature_node(node, &mut out);
    }
    out
}

/// Returns the CDDL signature of a ruleline's right-hand side as a string.
fn signature_ruleline_rhs(
    children: &[WrappedNode],
    out: &mut String,
) {
    for child in children {
        let WrappedNode::Syntax {
            rule,
            children: expr_children,
            ..
        } = child
        else {
            continue;
        };
        if rule != "expr" {
            continue;
        }

        let mut after_assignment = false;
        for expr_child in expr_children {
            let WrappedNode::Syntax { rule, .. } = expr_child else {
                continue;
            };
            if rule == "assignt" || rule == "assigng" {
                after_assignment = true;
                continue;
            }
            if after_assignment {
                signature_node(expr_child, out);
            }
        }
        return;
    }

    for child in children {
        signature_node(child, out);
    }
}

/// Append a stable structural signature for one node.
fn signature_node(
    node: &WrappedNode,
    out: &mut String,
) {
    use std::fmt::Write as _;

    match node {
        WrappedNode::RuleLine { children, .. } => {
            out.push_str("RuleLine(");
            signature_children(children, out);
            out.push(')');
        },
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            let _ = write!(out, "Syntax[{rule}](");
            if children.is_empty() {
                let _ = write!(out, "{:?}", text.trim());
            } else {
                signature_children(children, out);
            }
            out.push(')');
        },
        WrappedNode::Directive {
            directive,
            children,
            ..
        } => {
            let _ = write!(out, "Directive[{directive:?}](");
            signature_children(children, out);
            out.push(')');
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Append only semantic child nodes to a signature.
fn signature_children(
    children: &[WrappedNode],
    out: &mut String,
) {
    for child in children {
        match child {
            WrappedNode::Syntax { .. } | WrappedNode::RuleLine { .. } => {
                signature_node(child, out);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::Directive { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Pruned user tree plus names that were removed.
struct PrunedTree {
    /// Retained user nodes.
    nodes: Vec<WrappedNode>,
    /// Definition keys (name + source path + line + column) removed
    /// from the retained tree by the reachability pass.
    keys: HashSet<DefinitionKey>,
    /// Definition names removed from the retained tree.
    names: HashSet<String>,
}

/// Remove prunable definitions that are not reachable from retained roots.
fn prune_unreachable_prunable_definitions(nodes: &[WrappedNode]) -> PrunedTree {
    let definitions = collect_definition_nodes(nodes);
    let all_nodes_by_name = collect_all_definition_nodes(nodes);
    let roots = collect_non_prunable_definition_names(nodes);
    let reachable = reachable_definition_names(&definitions, &all_nodes_by_name, roots);
    let mut pruned_names = HashSet::new();
    let mut pruned_keys: HashSet<DefinitionKey> = HashSet::new();
    let retained = prune_nodes_with_keys(
        nodes,
        &reachable,
        &mut pruned_names,
        &mut pruned_keys,
        false,
    );

    PrunedTree {
        nodes: retained,
        keys: pruned_keys,
        names: pruned_names,
    }
}

/// Gather rule names that are not marked prunable.
fn collect_non_prunable_definition_names(nodes: &[WrappedNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for node in nodes {
        collect_non_prunable_definition_names_node(node, &mut names, false);
    }
    names
}

/// Recursive implementation for non-prunable root collection.
fn collect_non_prunable_definition_names_node(
    node: &WrappedNode,
    names: &mut HashSet<String>,
    inherited_prunable: bool,
) {
    let is_prunable = inherited_prunable || node.metadata().contains(&MetaData::Prunable);
    if let Some(name) = top_level_rule_name(node)
        && !is_prunable
    {
        names.insert(name);
    }

    match node {
        WrappedNode::RuleLine { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_non_prunable_definition_names_node(child, names, is_prunable);
            }
        },
        WrappedNode::Directive {
            directive,
            children,
            ..
        } => {
            collect_directive_non_prunable_definition_names(
                directive,
                children,
                names,
                is_prunable,
            );
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Gather non-prunable roots from a directive subtree using import semantics.
fn collect_directive_non_prunable_definition_names(
    directive: &Directive,
    children: &[WrappedNode],
    names: &mut HashSet<String>,
    inherited_prunable: bool,
) {
    if let Some(wanted) = directive_imported_names(directive) {
        for child in children {
            collect_named_import_non_prunable_definition_names(
                child,
                &wanted,
                names,
                inherited_prunable,
            );
        }
    } else {
        let directive_prunable = inherited_prunable || directive_is_import(directive);
        for child in children {
            collect_non_prunable_definition_names_node(child, names, directive_prunable);
        }
    }
}

/// Gather roots from a named-import subtree, keeping only selected rule names.
#[allow(
    clippy::only_used_in_recursion,
    reason = "names is threaded through recursion for sibling accumulation"
)]
fn collect_named_import_non_prunable_definition_names(
    node: &WrappedNode,
    wanted: &HashSet<&str>,
    names: &mut HashSet<String>,
    inherited_prunable: bool,
) {
    // Use the full name (including `<...>`) so a selected generic
    // template `all<keytype>` is recorded as `all<keytype>` and is
    // distinguished from a plain `all` in the same imported subtree.
    let node_name = top_level_full_name(node);
    let selected_by_import = node_name.as_deref().is_some_and(|name| {
        let head = name.split('<').next().unwrap_or(name);
        wanted.contains(name)
            || wanted.contains(head)
            || wanted
                .iter()
                .any(|w| w.split('<').next().unwrap_or(w) == head)
    });
    let is_prunable = inherited_prunable
        || node.metadata().contains(&MetaData::Prunable)
        || (node_name.is_some() && !selected_by_import);

    // A named import of a socket (for example `$$service-data`) selects
    // the augmentation rule, whose RHS names the concrete plug body.  That
    // RHS is the dependency that must remain reachable even though the
    // augmentation is not itself a normal definition root.
    if selected_by_import && let WrappedNode::RuleLine { .. } = node {
        let mut references = HashSet::new();
        collect_rhs_references(node, &mut references);
        names.extend(references);
    }

    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_named_import_non_prunable_definition_names(
                    child,
                    wanted,
                    names,
                    is_prunable,
                );
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Compute the closure of definitions referenced by the retained roots.
///
/// `all_nodes_by_name` maps every rule name to ALL its definition
/// nodes (a socket augmented by several `/= ` lines has one entry per
/// arm); every arm's references participate in the walk, not just the
/// first arm that survived [`collect_definition_nodes`].
fn reachable_definition_names(
    definitions: &HashMap<String, WrappedNode>,
    all_nodes_by_name: &HashMap<String, Vec<WrappedNode>>,
    roots: HashSet<String>,
) -> HashSet<String> {
    let mut reachable = HashSet::new();
    let mut stack = roots.into_iter().collect::<Vec<_>>();

    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }

        let mut references = HashSet::new();
        if let Some(nodes) = all_nodes_by_name.get(&name) {
            for node in nodes {
                collect_rhs_references(node, &mut references);
            }
        } else if let Some(definition) = definitions.get(&name) {
            collect_rhs_references(definition, &mut references);
        }
        for reference in references {
            if (definitions.contains_key(&reference) || all_nodes_by_name.contains_key(&reference))
                && !reachable.contains(&reference)
            {
                stack.push(reference);
            }
        }
    }

    reachable
}

/// Return a copy of nodes with unreachable prunable rule definitions removed.
#[allow(
    dead_code,
    reason = "kept for completeness alongside _with_keys variant"
)]
fn prune_nodes(
    nodes: &[WrappedNode],
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    inherited_prunable: bool,
) -> Vec<WrappedNode> {
    prune_nodes_with_keys(
        nodes,
        reachable,
        pruned_names,
        &mut HashSet::new(),
        inherited_prunable,
    )
}

/// Like [`prune_nodes`] but also records the [`DefinitionKey`] of every
/// removed definition so downstream passes (notably the Step-5.8
/// plain-vs-generic collision detector) can distinguish between a
/// generic definition that was retained and one that was pruned.
fn prune_nodes_with_keys(
    nodes: &[WrappedNode],
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    pruned_keys: &mut HashSet<DefinitionKey>,
    inherited_prunable: bool,
) -> Vec<WrappedNode> {
    nodes
        .iter()
        .filter_map(|node| {
            prune_node_with_keys(
                node,
                reachable,
                pruned_names,
                pruned_keys,
                inherited_prunable,
            )
        })
        .collect()
}

/// Prune one node if it is an unreachable prunable rule definition.
#[allow(
    dead_code,
    reason = "kept for completeness alongside _with_keys variant"
)]
fn prune_node(
    node: &WrappedNode,
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    inherited_prunable: bool,
) -> Option<WrappedNode> {
    prune_node_with_keys(
        node,
        reachable,
        pruned_names,
        &mut HashSet::new(),
        inherited_prunable,
    )
}

/// Like [`prune_node`] but also records the [`DefinitionKey`] of every
/// removed definition in `pruned_keys`.
fn prune_node_with_keys(
    node: &WrappedNode,
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    pruned_keys: &mut HashSet<DefinitionKey>,
    inherited_prunable: bool,
) -> Option<WrappedNode> {
    let is_prunable = inherited_prunable || node.metadata().contains(&MetaData::Prunable);
    // The reachability map is keyed by the bare rule name (see
    // [`collect_definition_nodes`]).  Use the bare head for the
    // reachability lookup so generic rules match their bare name.
    let full_name = top_level_full_name(node);
    let head = full_name
        .as_deref()
        .map(|n| n.split('<').next().unwrap_or(n));
    if let Some(name) = &full_name
        && is_prunable
        && !reachable.contains(name)
        && !head.is_some_and(|h| reachable.contains(h))
    {
        pruned_names.insert(name.clone());
        if let Some(key) = definition_key(node) {
            pruned_keys.insert(key);
        }
        return None;
    }

    let mut retained = node.clone();
    match &mut retained {
        WrappedNode::RuleLine { children, .. } | WrappedNode::Syntax { children, .. } => {
            *children =
                prune_nodes_with_keys(children, reachable, pruned_names, pruned_keys, is_prunable);
        },
        WrappedNode::Directive {
            directive,
            children,
            ..
        } => {
            *children = prune_directive_children_with_keys(
                directive,
                children,
                reachable,
                pruned_names,
                pruned_keys,
                is_prunable,
            );
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }

    Some(retained)
}

/// Extract a [`DefinitionKey`] from a top-level `RuleLine` node.
fn definition_key(node: &WrappedNode) -> Option<DefinitionKey> {
    let WrappedNode::RuleLine { origin, .. } = node else {
        return None;
    };
    let head = rule_head(node)?;
    Some(DefinitionKey {
        name: head.name,
        source_path: origin.source_path.clone(),
        line: origin.line,
        column: origin.column,
    })
}

/// Top-level rule name preserving the generic parameter list.
///
/// Unlike [`top_level_rule_name`] (which strips the `<...>` so callers can
/// compare base names against roots and postlude entries), this helper
/// keeps the full textual head so the reachability pruner can distinguish
/// a generic rule from a plain rule with the same base name.
fn top_level_full_name(node: &WrappedNode) -> Option<String> {
    let WrappedNode::RuleLine { text, .. } = node else {
        return None;
    };
    let lhs = text
        .split_once('=')
        .map_or(text.as_str(), |(lhs, _)| lhs)
        .trim();
    let end = lhs.find([' ', '\t']).unwrap_or(lhs.len());
    Some(lhs.get(..end)?.trim().to_owned())
}

/// Like [`prune_directive_children`] but also records pruned [`DefinitionKey`]s.
fn prune_directive_children_with_keys(
    directive: &Directive,
    children: &[WrappedNode],
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    pruned_keys: &mut HashSet<DefinitionKey>,
    inherited_prunable: bool,
) -> Vec<WrappedNode> {
    let wanted = directive_imported_names(directive);
    children
        .iter()
        .filter_map(|child| {
            if let Some(names) = &wanted {
                prune_named_import_node_with_keys(
                    child,
                    names,
                    reachable,
                    pruned_names,
                    pruned_keys,
                    inherited_prunable,
                )
            } else {
                prune_node_with_keys(
                    child,
                    reachable,
                    pruned_names,
                    pruned_keys,
                    inherited_prunable || directive_is_import(directive),
                )
            }
        })
        .collect()
}

/// Like [`prune_named_import_node`] but also records pruned [`DefinitionKey`]s.
fn prune_named_import_node_with_keys(
    node: &WrappedNode,
    wanted: &HashSet<&str>,
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    pruned_keys: &mut HashSet<DefinitionKey>,
    inherited_prunable: bool,
) -> Option<WrappedNode> {
    // Use the *full* name (including the generic parameter list) so a
    // generic rule and a plain rule with the same base name are
    // distinguished.  Without this, an imported generic `all<keytype>`
    // would be matched by the bare name `all` and could be reported as
    // reachable just because the consumer's plain `all` is a retained
    // root.
    let full_name = top_level_full_name(node);
    let selected_by_import = full_name.as_deref().is_some_and(|name| {
        let head = name.split('<').next().unwrap_or(name);
        wanted.contains(name)
            || wanted.contains(head)
            || wanted
                .iter()
                .any(|w| w.split('<').next().unwrap_or(w) == head)
    });
    let self_prunable = node.metadata().contains(&MetaData::Prunable);
    let is_prunable =
        inherited_prunable || self_prunable || (full_name.is_some() && !selected_by_import);

    if let Some(name) = &full_name
        && is_prunable
        && !reachable.contains(name)
    {
        let head = name.split('<').next().unwrap_or(name);
        if !reachable.contains(head) {
            pruned_names.insert(name.clone());
            if let Some(key) = definition_key(node) {
                pruned_keys.insert(key);
            }
            return None;
        }
    }

    let mut retained = node.clone();
    match &mut retained {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } => {
            *children =
                prune_nodes_with_keys(children, reachable, pruned_names, pruned_keys, is_prunable);
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }

    Some(retained)
}

/// Prune directive children using import semantics when metadata is absent.
#[allow(
    dead_code,
    reason = "kept for completeness alongside _with_keys variant"
)]
fn prune_directive_children(
    directive: &Directive,
    children: &[WrappedNode],
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    inherited_prunable: bool,
) -> Vec<WrappedNode> {
    let wanted = directive_imported_names(directive);
    children
        .iter()
        .filter_map(|child| {
            if let Some(names) = &wanted {
                prune_named_import_node(child, names, reachable, pruned_names, inherited_prunable)
            } else {
                prune_node(
                    child,
                    reachable,
                    pruned_names,
                    inherited_prunable || directive_is_import(directive),
                )
            }
        })
        .collect()
}

/// Prune a named-import subtree by applying import selection at rule nodes.
fn prune_named_import_node(
    node: &WrappedNode,
    wanted: &HashSet<&str>,
    reachable: &HashSet<String>,
    pruned_names: &mut HashSet<String>,
    inherited_prunable: bool,
) -> Option<WrappedNode> {
    // `top_level_rule_name` returns the rule's typename without the
    // generic parameter list, but the wanted names from a named
    // import/include include the `<...>` form (`foo<t>`).  Match on
    // both forms so generics are recognized as selected.
    let selected_by_import = top_level_rule_name(node).is_some_and(|name| {
        wanted.contains(name.as_str())
            || wanted
                .iter()
                .any(|w| w.split('<').next().is_some_and(|head| head == name))
    });
    let is_prunable = inherited_prunable
        || node.metadata().contains(&MetaData::Prunable)
        || (top_level_rule_name(node).is_some() && !selected_by_import);

    if let Some(name) = top_level_rule_name(node)
        && is_prunable
        && !reachable.contains(&name)
    {
        pruned_names.insert(name);
        return None;
    }

    let mut retained = node.clone();
    match &mut retained {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } => {
            *children = children
                .iter()
                .filter_map(|child| {
                    prune_named_import_node(child, wanted, reachable, pruned_names, is_prunable)
                })
                .collect();
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }

    Some(retained)
}

/// Return explicitly imported names, if this is a named import directive.
fn directive_imported_names(directive: &Directive) -> Option<HashSet<&str>> {
    match directive {
        Directive::ImportFrom { names, .. } | Directive::ImportFromAs { names, .. } => {
            Some(names.iter().map(String::as_str).collect())
        },
        Directive::Import { .. }
        | Directive::ImportAs { .. }
        | Directive::Include { .. }
        | Directive::IncludeAs { .. }
        | Directive::IncludeFrom { .. }
        | Directive::IncludeFromAs { .. } => None,
    }
}

/// Return whether the directive imports an external subtree.
fn directive_is_import(directive: &Directive) -> bool {
    matches!(
        directive,
        Directive::Import { .. }
            | Directive::ImportAs { .. }
            | Directive::ImportFrom { .. }
            | Directive::ImportFromAs { .. }
    )
}

/// Collect RHS typename/groupname references from a rule definition.
fn collect_rhs_references(
    node: &WrappedNode,
    references: &mut HashSet<String>,
) {
    match node {
        WrappedNode::RuleLine { children, .. } => {
            // For generic definitions, still walk the body so references
            // inside the body (such as a `.within` RHS that resolves
            // to a definition-site alias) survive reachability pruning.
            // Formal parameter names themselves are not real references
            // and must be filtered out so the pruner does not chase
            // them as if they were missing definitions.
            let formal_params: HashSet<String> = if is_generic_definition(children) {
                collect_generic_param_names(children)
            } else {
                HashSet::new()
            };
            let mut lhs_seen = false;
            for child in children {
                if matches!(child, WrappedNode::Syntax { .. }) {
                    collect_rhs_references_node_filtered(
                        child,
                        &mut lhs_seen,
                        references,
                        &formal_params,
                    );
                }
            }
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_rhs_references(child, references);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Return the formal parameter names declared by a generic definition
/// rule line.  Used to filter formal parameters out of RHS-reference
/// collection so the reachability pruner does not treat them as
/// missing definitions.
fn collect_generic_param_names(children: &[WrappedNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            children: expr_children,
            ..
        } = child
            && rule == "expr"
        {
            for expr_child in expr_children {
                if let WrappedNode::Syntax {
                    rule,
                    text,
                    children,
                    ..
                } = expr_child
                    && rule == "genericparm"
                {
                    for grand in children {
                        if let WrappedNode::Syntax { rule, text, .. } = grand
                            && rule == "id"
                        {
                            names.insert(text.trim().to_owned());
                        }
                    }
                    // Also fall back to parsing the genericparm's
                    // text in case the id children are absent.
                    let trimmed = text.trim();
                    if trimmed.starts_with('<') && trimmed.ends_with('>') {
                        for part in trimmed
                            .get(1..trimmed.len().saturating_sub(1))
                            .unwrap_or("")
                            .split(',')
                        {
                            let name = part.trim();
                            if !name.is_empty() {
                                names.insert(name.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    names
}

/// Like [`collect_rhs_references_node_filtered`] but with no formal
/// parameter exclusion.  Kept as a thin wrapper for any future
/// callers that want the unfiltered form; the live pipeline uses the
/// filtered variant directly so the generic-param filter is always
/// applied.
#[allow(dead_code)]
fn collect_rhs_references_node(
    node: &WrappedNode,
    lhs_seen: &mut bool,
    references: &mut HashSet<String>,
) {
    collect_rhs_references_node_filtered(node, lhs_seen, references, &HashSet::new());
}

/// Recursive RHS reference collector that skips the first LHS name in
/// an expression and additionally ignores any typename/groupname
/// whose trimmed text appears in `exclude`.  Generic-body reference
/// collection passes the rule's formal parameter names through
/// `exclude` so they are not treated as missing definitions.
fn collect_rhs_references_node_filtered(
    node: &WrappedNode,
    lhs_seen: &mut bool,
    references: &mut HashSet<String>,
    exclude: &HashSet<String>,
) {
    match node {
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            if rule == "typename" || rule == "groupname" || rule == "type_socket" {
                if *lhs_seen {
                    let name = text.trim();
                    if !exclude.contains(name) {
                        references.insert(name.to_owned());
                    }
                } else {
                    *lhs_seen = true;
                }
            }

            for child in children {
                collect_rhs_references_node_filtered(child, lhs_seen, references, exclude);
            }
        },
        WrappedNode::RuleLine { .. }
        | WrappedNode::Directive { .. }
        | WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Walk the user tree and pre-populate the cache with every typename
/// and groupname reference (including LHS names) as `Unresolved`.
///
/// The seed pass only creates cache entries for LHS names of rules
/// it visits; it does not recurse into the rule body to register
/// references.  Calling `cache.get(name)` here is enough to create
/// the auto-`Unresolved` entry without going through `resolve`, so
/// no `RedundantType` warnings are emitted even if the same name is
/// encountered in the postlude.
fn seed_cache_with_all_references(
    nodes: &[WrappedNode],
    cache: &mut ResolverCache,
) {
    fn visit(
        node: &WrappedNode,
        cache: &mut ResolverCache,
    ) {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                if let Some(name) = top_level_rule_name(node) {
                    let _ = cache.get(&name);
                }
                for child in children {
                    visit(child, cache);
                }
            },
            WrappedNode::Syntax {
                rule,
                text,
                children,
                ..
            } if rule == "typename" || rule == "groupname" => {
                let _ = cache.get(text.trim());
                for child in children {
                    visit(child, cache);
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                for child in children {
                    visit(child, cache);
                }
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }

    for node in nodes {
        visit(node, cache);
    }
}

/// Compare user definitions against authoritative postlude definitions.
///
/// The postlude contains RFC 8610 standard types such as `int`, `bytes`, `bstr`,
/// and `tstr` whose right-hand sides are tag references (`#0`, `#2`, etc.) and
/// not concrete literals.  The seed-pass cache therefore holds those postlude
/// names in `Unresolved` state, which means a cache-state-only comparison
/// silently skips every tag-based postlude type and never reports a
/// redundant-definition warning for `bytes = bstr`.
///
/// This implementation uses a structural signature comparison
/// (`rule_signature`) so the redundant-definition diagnostic fires when the
/// user redefines a postlude name with the exact same content, regardless of
/// whether either side resolves to a concrete value.  Two definitions are
/// considered equivalent iff their signatures match byte-for-byte, which
/// includes the literal text of any leaf nodes (so `h'ff'` and `h'01'` are
/// not collapsed into a single "redundant" match).
///
/// A signature mismatch raises a hard `ConflictingDefinition` error so the
/// user is told their override is incompatible with the postlude.  One
/// exception is the ctlop-form / tag-form equivalence documented in
/// RFC 8610 §3.8.4: the postlude's `encoded-cbor = #6.24(bstr)` and a
/// user's `encoded-cbor = bytes .cbor any` are two syntactic forms of the
/// same semantic intent.  When the ctlop's argument is the postlude's
/// `any`, the user's redefinition is treated as a redundant restatement of
/// the postlude.  Any other ctlop argument (`bytes .cbor type1`,
/// `bytes .cbor int`, …) is a different definition and is reported as a
/// conflict.
///
/// The postlude definition is never injected over a user definition with the
/// same name; `handle_reference` already consults `user_definition_names`
/// before pulling anything from the postlude.
fn compare_user_and_postlude_definitions(
    compiled: &mut CompiledCDDL,
    retained_nodes: &mut [WrappedNode],
    user_names: &HashSet<String>,
    postlude_definitions: &HashMap<String, WrappedNode>,
    _postlude_cache: &ResolverCache,
) {
    for name in user_names {
        let Some(postlude_node) = postlude_definitions.get(name) else {
            continue;
        };

        let Some(user_node) = find_definition_node(&compiled.user_nodes, name) else {
            continue;
        };

        let user_signature = rule_signature(user_node);
        let postlude_signature = rule_signature(postlude_node);
        let user_origin = user_node.origin().clone();
        let user_span = definition_span(user_node);
        let postlude_origin = postlude_node.origin().clone();

        let signatures_match = user_signature == postlude_signature;
        let ctlop_form_matches =
            !signatures_match && is_ctlop_form_of_tag_postlude(user_node, postlude_node);
        let relationship = if signatures_match || ctlop_form_matches {
            DefinitionRelationship::Redundant
        } else {
            DefinitionRelationship::Conflicting
        };

        match relationship {
            DefinitionRelationship::Redundant => {
                push_metadata_for_definition(
                    &mut compiled.user_nodes,
                    name,
                    MetaData::RedundantDefinition,
                );
                push_metadata_for_definition(retained_nodes, name, MetaData::RedundantDefinition);
                push_redundant_diagnostic(
                    &mut compiled.warnings,
                    name,
                    &user_origin,
                    &user_span,
                    Some(&postlude_origin),
                );
            },
            DefinitionRelationship::Conflicting => {
                push_metadata_for_definition(
                    &mut compiled.user_nodes,
                    name,
                    MetaData::ConflictingDefinition,
                );
                push_metadata_for_definition(retained_nodes, name, MetaData::ConflictingDefinition);
                push_conflict_diagnostic(
                    &mut compiled.warnings,
                    name,
                    &user_origin,
                    &user_span,
                    Some(&postlude_origin),
                );
            },
        }
    }
}

/// The relationship between a user definition and its postlude counterpart.
enum DefinitionRelationship {
    /// The user re-states the postlude definition; surface a W001.
    Redundant,
    /// The user re-defines the postlude name with a different content;
    /// surface an E014 so the user is told their override is incompatible.
    Conflicting,
}

/// Return `true` when `user_node` is the RFC 8610 §3.8.4 ctlop-form
/// equivalent of `postlude_node`'s tag form.
///
/// Specifically: the postlude side is a tag literal (`#6.X(...)` or
/// `#X(...)`) and the user side is `bytes .cbor any` or
/// `bstr .cbor any`.  Any other ctlop argument — `bytes .cbor int`,
/// `bytes .cbor type1` where `type1` is locally bound to `bstr`, etc. —
/// is a *different* definition and must fall through to the conflict
/// branch.
fn is_ctlop_form_of_tag_postlude(
    user_node: &WrappedNode,
    postlude_node: &WrappedNode,
) -> bool {
    let postlude_rhs = render_rhs_text(postlude_node);
    if !postlude_rhs.trim_start().starts_with('#') {
        return false;
    }

    let user_rhs = render_rhs_text(user_node);
    let trimmed = user_rhs.trim_start();
    let after_cbor = match trimmed.split_once(".cbor") {
        Some((before, after)) => {
            if !(before.trim() == "bytes" || before.trim() == "bstr") {
                return false;
            }
            after.trim_start()
        },
        None => return false,
    };

    // The argument must be exactly the postlude's `any` token.  We accept
    // either `any` followed by a delimiter (whitespace, end of string, or a
    // continuation that is not part of the same identifier) or `any` as the
    // entire remainder of the RHS.  This rejects `bytes .cbor anyfoo` while
    // accepting `bytes .cbor any` and `bytes .cbor any,` (defensively).
    let arg = after_cbor
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '/' | '{' | '}' | '(' | ')'))
        .next()
        .unwrap_or("");
    arg == "any"
}

/// Render the right-hand side of a rule as plain text.
///
/// This is a deliberately loose renderer: it reads the `text` field of the
/// top-level `RuleLine` and returns everything after the first `=`.  It is
/// only used by the ctlop-form / tag-form equivalence check, which is a
/// best-effort heuristic.  The signature-based comparison in
/// `compare_user_and_postlude_definitions` is the authoritative comparison.
fn render_rhs_text(node: &WrappedNode) -> String {
    if let WrappedNode::RuleLine { text, .. } = node
        && let Some((_, rhs)) = text.split_once('=')
    {
        return rhs.trim().to_string();
    }
    String::new()
}

/// Shared state for the surgical postlude injection walk.
struct InjectionContext<'a> {
    /// User-defined names already present in the tree.
    user_definition_names: &'a HashSet<String>,
    /// Standard postlude definitions keyed by name.
    postlude_definitions: &'a HashMap<String, WrappedNode>,
    /// Tree that will become the physically complete document.
    complete_nodes: &'a mut Vec<WrappedNode>,
    /// Names already injected from the postlude.
    injected_names: &'a mut HashSet<String>,
    /// Missing references already reported.
    seen_missing: &'a mut HashSet<MissingReferenceKey>,
    /// Bare names that only exist as generic templates.
    generic_definition_names: &'a HashSet<String>,
    /// Whether this walk injected anything new.
    changed: bool,
    /// Whether unresolved references should be downgraded to warnings.
    is_library: bool,
    /// Names explicitly declared as external by file directives.
    extern_names: &'a HashSet<String>,
    /// Diagnostics accumulated during the pass.
    warnings: &'a mut Vec<Diagnostic>,
}

/// Validates that extern declarations reference existing definitions.
fn validate_extern_declarations(compiled: &mut CompiledCDDL) {
    if compiled.extern_names.is_empty() {
        return;
    }

    let user_definitions = collect_definition_names(&compiled.user_nodes);
    let postlude_definitions = collect_definition_names(&compiled.postlude_nodes);
    for extern_name in &compiled.extern_names {
        if extern_name.starts_with('$') {
            continue;
        }
        if !(user_definitions.contains(extern_name) || postlude_definitions.contains(extern_name)) {
            continue;
        }
        compiled.warnings.push(Diagnostic {
            code: "E019",
            level: DiagnosticLevel::Error,
            message: format!(
                "extern declaration `{extern_name}` contradicts a definite local or standard definition"
            ),
            source_file: Some(compiled.source_path.clone()),
            span: None,
            previous_origin: None,
            related: Vec::new(),
        });
    }
}

/// Walk the current tree and inject any referenced postlude definitions.
fn scan_and_inject_references(
    node: &WrappedNode,
    lhs_seen: &mut bool,
    ctx: &mut InjectionContext<'_>,
) {
    match node {
        WrappedNode::RuleLine { children, .. } => {
            if is_generic_definition(children) {
                return;
            }
            let mut seen_lhs = false;
            for child in children {
                match child {
                    WrappedNode::Syntax { .. } => {
                        scan_and_inject_references(child, &mut seen_lhs, ctx);
                    },
                    WrappedNode::RuleLine { .. } | WrappedNode::Directive { .. } => {
                        let mut nested_lhs_seen = false;
                        scan_and_inject_references(child, &mut nested_lhs_seen, ctx);
                    },
                    WrappedNode::Comment { .. }
                    | WrappedNode::ModuleStart { .. }
                    | WrappedNode::ModuleEnd { .. } => {},
                }
            }
        },
        WrappedNode::Directive { children, .. } => {
            for child in children {
                let mut nested_lhs_seen = false;
                scan_and_inject_references(child, &mut nested_lhs_seen, ctx);
            }
        },
        WrappedNode::Syntax { children, .. } => {
            if let WrappedNode::Syntax {
                rule,
                text,
                span,
                origin,
                ..
            } = node
                && (rule == "typename" || rule == "groupname")
            {
                if *lhs_seen {
                    handle_reference(text.trim(), origin, span, ctx);
                } else {
                    *lhs_seen = true;
                }
            }

            for child in children {
                scan_and_inject_references(child, lhs_seen, ctx);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Return whether a rule line defines a generic type or group.
fn is_generic_definition(children: &[WrappedNode]) -> bool {
    children.iter().any(|child| {
        if let WrappedNode::Syntax { rule, children, .. } = child
            && rule == "expr"
        {
            return children.iter().any(|expr_child| {
                matches!(expr_child, WrappedNode::Syntax { rule, .. } if rule == "genericparm")
            });
        }
        false
    })
}

/// Handle a referenced typename by injecting or reporting it.
fn handle_reference(
    ref_name: &str,
    origin: &crate::SourceOrigin,
    span: &Range<usize>,
    ctx: &mut InjectionContext<'_>,
) {
    if is_undefined_socket_reference(ref_name) {
        return;
    }

    if ctx.user_definition_names.contains(ref_name) {
        return;
    }

    if let Some(postlude_node) = ctx.postlude_definitions.get(ref_name) {
        if ctx.injected_names.insert(ref_name.to_owned()) {
            let mut injected = postlude_node.clone();
            tag_standard_postlude(&mut injected);
            ctx.complete_nodes.push(injected);
            ctx.changed = true;
        }
        return;
    }

    let key = MissingReferenceKey {
        source_path: origin.source_path.clone(),
        span: span.clone(),
        name: ref_name.to_owned(),
    };
    if ctx.seen_missing.insert(key) {
        if ctx.is_library && ctx.extern_names.contains(ref_name) {
            return;
        }
        if ctx.generic_definition_names.contains(ref_name) {
            ctx.warnings.push(Diagnostic {
                code: "E016",
                level: DiagnosticLevel::Error,
                message: format!(
                    "undefined reference `{ref_name}` at {}:{}:{}; `{ref_name}` is only defined as a generic template and must be instantiated with arguments",
                    origin.source_path.display(),
                    origin.line,
                    origin.column
                ),
                source_file: Some(origin.source_path.clone()),
                span: Some(span.clone()),
                previous_origin: None,
                related: Vec::new(),
            });
            return;
        }
        ctx.warnings.push(Diagnostic {
            code: "E016",
            level: if ctx.is_library {
                DiagnosticLevel::Warning
            } else {
                DiagnosticLevel::Error
            },
            message: format!(
                "undefined reference `{ref_name}` at {}:{}:{}",
                origin.source_path.display(),
                origin.line,
                origin.column
            ),
            source_file: Some(origin.source_path.clone()),
            span: Some(span.clone()),
            previous_origin: None,
            related: Vec::new(),
        });
    }
}

/// Returns `true` if a socket reference name is undefined.
fn is_undefined_socket_reference(ref_name: &str) -> bool {
    ref_name.starts_with('$')
}

/// Merge injected postlude values into the main resolver cache.
fn merge_injected_postlude_values(
    user_cache: &mut ResolverCache,
    postlude_cache: &ResolverCache,
    injected_names: &HashSet<String>,
) {
    for name in injected_names {
        if user_cache.is_resolved(name) {
            continue;
        }

        let Some(state) = lookup_state(postlude_cache, name) else {
            continue;
        };
        let origin = postlude_cache.origin(name).cloned();
        drop(user_cache.resolve_with_origin(name, state, origin));
    }
}

/// Merge postlude values into a working cache, excluding user-defined names.
fn merge_postlude_values_for_resolution(
    working_cache: &mut ResolverCache,
    postlude_cache: &ResolverCache,
    user_definition_names: &HashSet<String>,
) {
    for (name, state) in postlude_cache.iter() {
        if user_definition_names.contains(name) || !state.is_resolved() {
            continue;
        }

        let origin = postlude_cache.origin(name).cloned();
        drop(working_cache.resolve_with_origin(name, state.clone(), origin));
    }
}

/// Look up a resolved state by name.
fn lookup_state(
    cache: &ResolverCache,
    name: &str,
) -> Option<EntryState> {
    cache
        .iter()
        .find_map(|(key, state)| (key == name && state.is_resolved()).then(|| state.clone()))
}

/// Gather every top-level definition name in a tree.
fn collect_definition_names(nodes: &[WrappedNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for node in nodes {
        collect_definition_names_node(node, &mut names);
    }
    names
}

/// Gather the bare names of generic templates.
fn collect_generic_definition_base_names(nodes: &[WrappedNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_generic_definition_base_names_in_nodes(nodes, &mut names);
    names
}

/// Recursively collect generic template base names.
fn collect_generic_definition_base_names_in_nodes(
    nodes: &[WrappedNode],
    names: &mut HashSet<String>,
) {
    for node in nodes {
        if let Some(name) = top_level_full_name(node)
            && let Some((head, _)) = name.split_once('<')
        {
            names.insert(head.to_owned());
        }

        match node {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Directive { children, .. }
            | WrappedNode::Syntax { children, .. } => {
                collect_generic_definition_base_names_in_nodes(children, names);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Collect top-level definition nodes keyed by name.
fn collect_definition_nodes(nodes: &[WrappedNode]) -> HashMap<String, WrappedNode> {
    let mut defs = HashMap::new();
    for node in nodes {
        // Use the bare rule name (no `<...>`) so reachability lookups
        // match typename text from [`collect_rhs_references`].  A plain
        // `all` and a generic `all<keytype>` would collide here, but
        // [`collect_definition_names`] and the plain-vs-generic
        // collision detector distinguish them via the source AST.
        if let Some(name) = top_level_rule_name(node) {
            defs.entry(name).or_insert_with(|| node.clone());
        }
        collect_definition_nodes_nested(node, &mut defs);
    }
    defs
}

/// Recursively collect definition names.
fn collect_definition_names_node(
    node: &WrappedNode,
    names: &mut HashSet<String>,
) {
    if let Some(name) = top_level_full_name(node) {
        names.insert(name.clone());
    }

    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_definition_names_node(child, names);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Collect EVERY definition node per rule name, keeping all duplicate
/// arms (multiple `/= ` augment lines for one socket, shadowed names).
/// The reachability walk needs every arm's references, while
/// [`collect_definition_nodes`] keeps only the first.
fn collect_all_definition_nodes(nodes: &[WrappedNode]) -> HashMap<String, Vec<WrappedNode>> {
    let mut defs: HashMap<String, Vec<WrappedNode>> = HashMap::new();
    for node in nodes {
        collect_all_definition_nodes_nested(node, &mut defs);
    }
    defs
}

/// Recursive helper for [`collect_all_definition_nodes`].
fn collect_all_definition_nodes_nested(
    node: &WrappedNode,
    defs: &mut HashMap<String, Vec<WrappedNode>>,
) {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            if let Some(name) = top_level_rule_name(node) {
                defs.entry(name).or_default().push(node.clone());
            }
            for child in children {
                collect_all_definition_nodes_nested(child, defs);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Recursively collect nested definition nodes.
fn collect_definition_nodes_nested(
    node: &WrappedNode,
    defs: &mut HashMap<String, WrappedNode>,
) {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            for child in children {
                if let Some(name) = top_level_rule_name(child) {
                    defs.entry(name).or_insert_with(|| child.clone());
                }
                collect_definition_nodes_nested(child, defs);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Extract the top-level rule name from a node.
fn top_level_rule_name(node: &WrappedNode) -> Option<String> {
    rule_name(node)
}

/// Return the byte span of a definition node.
fn definition_span(node: &WrappedNode) -> Range<usize> {
    match node {
        WrappedNode::RuleLine { span, .. }
        | WrappedNode::Comment { span, .. }
        | WrappedNode::Syntax { span, .. }
        | WrappedNode::Directive { span, .. } => span.clone(),
        WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => 0..0,
    }
}

/// Find the first top-level definition node for a name.
fn find_definition_node<'a>(
    nodes: &'a [WrappedNode],
    name: &str,
) -> Option<&'a WrappedNode> {
    for node in nodes {
        if top_level_rule_name(node).as_deref() == Some(name) {
            return Some(node);
        }
        if let Some(found) = find_definition_node_nested(node, name) {
            return Some(found);
        }
    }
    None
}

/// Recursively search for a matching definition node.
fn find_definition_node_nested<'a>(
    node: &'a WrappedNode,
    name: &str,
) -> Option<&'a WrappedNode> {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            for child in children {
                if top_level_rule_name(child).as_deref() == Some(name) {
                    return Some(child);
                }
                if let Some(found) = find_definition_node_nested(child, name) {
                    return Some(found);
                }
            }
            None
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => None,
    }
}

/// Recursively tag a tree as standard postlude.
fn tag_standard_postlude(node: &mut WrappedNode) {
    node.map_nodes_mut(&mut |child| {
        if !child.metadata().contains(&MetaData::StandardPostlude) {
            match child {
                WrappedNode::RuleLine { metadata, .. }
                | WrappedNode::Comment { metadata, .. }
                | WrappedNode::Syntax { metadata, .. }
                | WrappedNode::Directive { metadata, .. }
                | WrappedNode::ModuleStart { metadata, .. }
                | WrappedNode::ModuleEnd { metadata, .. } => {
                    metadata.push(MetaData::StandardPostlude);
                },
            }
        }
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

/// Push a metadata flag on every matching top-level definition node.
fn push_metadata_for_definition(
    nodes: &mut [WrappedNode],
    name: &str,
    flag: MetaData,
) {
    for node in nodes {
        if let Some(node_name) = top_level_rule_name(node)
            && node_name == name
            && let WrappedNode::RuleLine { metadata, .. } = node
        {
            let _ = push_metadata(metadata, flag);
        }
        if let WrappedNode::Directive { children, .. }
        | WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. } = node
        {
            push_metadata_for_definition(children, name, flag);
        }
    }
}

/// Track missing references at a specific source site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MissingReferenceKey {
    /// Source file that referenced the missing name.
    source_path: PathBuf,
    /// Byte span of the reference.
    span: Range<usize>,
    /// Missing reference name.
    name: String,
}

// ---------------------------------------------------------------------------
// Step 5.8: plain-vs-generic collision detection on the pruned tree.
// ---------------------------------------------------------------------------

/// One retained definition site for plain-vs-generic collision detection.
struct RetainedDefinition {
    /// Rule name.
    name: String,
    /// Whether this rule line carries a generic parameter list.
    is_generic: bool,
    /// Whether this rule line carries the prunable (`=`) assignment; a `:=`
    /// consumer re-importing a library rule has been promoted to strong by
    /// [`normalize_definition_strengths`].
    is_prunable: bool,
    /// Source origin of the definition.
    origin: crate::SourceOrigin,
    /// Source span of the definition.
    span: Range<usize>,
}

/// Detect plain-rule vs generic-rule collisions on the user tree,
/// filtering out any pair where at least one side was pruned.
///
/// The pre-Step-5.8 collector in `generic.rs` reported this collision
/// against the unpruned resolved tree, which caused unreferenced weak
/// imported generic helpers to spuriously collide with a strong local
/// plain rule.  After Step 5.8 the collector is silent and this
/// pass runs against the original (pre-prune) tree so that generic
/// definition templates — which the reachability pruner would
/// otherwise drop once their last call site has been inlined — are
/// still visible for the collision check.
///
/// `pruned_names` is the set of definition keys removed by the
/// reachability pass (one `DefinitionKey` per removed definition,
/// identified by name + source path + line + column).  Any candidate
/// collision pair where at least one site matches a pruned name is
/// dropped — unreferenced weak helpers do not collide with retained
/// strong local rules, exactly the behavior Step 5.8 asks for.
///
/// `consumer_source_path` is the path of the file being compiled
/// (the consumer).  When BUG-004 reproducer shapes are detected —
/// a pair where both the plain and generic sides come from
/// independently imported files (different `source_path`) and
/// neither side lives in the consumer's own file — the pair is
/// dropped: the two private library roots share a name in the
/// consumer's effective namespace only because both libraries were
/// imported without an alias, but the consumer's direct surface
/// does not reference either root, so the plain-vs-generic collision
/// is not actionable.
/// Detect definitions whose RHS top-level choice contains a provably
/// bare self-arm together with at least one other arm
/// (`x = x / int`). The self-arm contributes no structure and the
/// concrete renderer elides it, so the source should be simplified;
/// emit a warning (`E031`) so authors can fix the definition.
pub(crate) fn detect_elidable_self_references(compiled: &mut CompiledCDDL) {
    let mut warnings = Vec::new();
    for node in &compiled.user_nodes {
        let WrappedNode::RuleLine {
            children,
            origin,
            span,
            ..
        } = node
        else {
            continue;
        };
        let Some(head) = rule_head_from_children(children) else {
            continue;
        };
        if head.assignment != AssignmentKind::Define {
            continue;
        }
        let Some(rhs) = crate::concrete::find_rhs(children) else {
            continue;
        };
        let WrappedNode::Syntax { children: tc, .. } = rhs else {
            continue;
        };
        if crate::concrete::syntax_rule(rhs) != Some("type") {
            continue;
        }
        let arms: Vec<&WrappedNode> = tc
            .iter()
            .filter(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type1"))
            .collect();
        if arms.len() < 2 {
            continue;
        }
        if arms.iter().any(|arm| {
            crate::concrete::arm_is_bare_name(arm).as_deref() == Some(head.name.as_str())
        }) {
            warnings.push(Diagnostic {
                code: "E031",
                level: DiagnosticLevel::Warning,
                message: format!(
                    "self-referential choice arm `{}` in `{} = ...` is redundant: the reference is unguarded (it adds no structure) and the renderer elides it",
                    head.name, head.name
                ),
                source_file: Some(origin.source_path.clone()),
                span: Some(span.clone()),
                previous_origin: None,
                related: Vec::new(),
            });
        }
    }
    compiled.warnings.extend(warnings);
}

/// Drop plain-vs-generic definition collisions where the pair is not
/// actionable (the consumer's direct surface does not reference either
/// side; see the doc comment on the caller).
pub(crate) fn detect_plain_generic_collisions(
    nodes: &[WrappedNode],
    warnings: &mut Vec<crate::Diagnostic>,
    pruned_keys: &HashSet<DefinitionKey>,
    consumer_source_path: &std::path::Path,
) {
    let mut retained: Vec<RetainedDefinition> = Vec::new();
    collect_retained_definitions(nodes, &mut retained);

    // For each name, partition retained sites into (plain, generic) groups
    // and emit E013 once per pair, deduplicating on (plain-source-location,
    // generic-source-location) so the same collision is not reported twice.
    let mut by_name: HashMap<&str, Vec<&RetainedDefinition>> = HashMap::new();
    for site in &retained {
        by_name.entry(site.name.as_str()).or_default().push(site);
    }

    let mut reported: HashSet<(PathBuf, usize, usize, PathBuf, usize, usize)> = HashSet::new();
    for (name, sites) in by_name {
        let mut plains: Vec<&RetainedDefinition> = Vec::new();
        let mut generics: Vec<&RetainedDefinition> = Vec::new();
        for site in &sites {
            if site.is_generic {
                generics.push(*site);
            } else {
                plains.push(*site);
            }
        }
        if plains.is_empty() || generics.is_empty() {
            continue;
        }
        for plain in &plains {
            // Drop the pair entirely if the plain side was pruned by
            // the reachability pass: an unreferenced weak helper
            // definition must not spuriously collide with a retained
            // strong local rule.
            let plain_key = definition_key_from_site(plain);
            if pruned_keys.contains(&plain_key) {
                continue;
            }
            for generic in &generics {
                // Same on the generic side.
                let generic_key = definition_key_from_site(generic);
                if pruned_keys.contains(&generic_key) {
                    continue;
                }
                // CDDL shadowing: a non-prunable local plain rule
                // with the same base name silently shadows a
                // prunable imported generic helper.  The local rule
                // wins; no E013.
                let plain_shadows_imported_generic = !plain.is_prunable && generic.is_prunable;
                if plain_shadows_imported_generic {
                    continue;
                }
                // BUG-004 fix: when both sides of the pair come
                // from independently imported files (different
                // source paths) and neither side lives in the
                // consumer's own source file, the pair does not
                // represent a true collision in the consumer's
                // namespace.  Two CBORK libraries can each use the
                // same private root name (e.g. `all`) without any
                // actual collision in the consumer's surface; the
                // consumer's direct references are independent of
                // each library's private root.
                let plain_in_consumer = plain.origin.source_path == consumer_source_path;
                let generic_in_consumer = generic.origin.source_path == consumer_source_path;
                let independent_imported_pair = !plain_in_consumer
                    && !generic_in_consumer
                    && plain.origin.source_path != generic.origin.source_path;
                if independent_imported_pair {
                    continue;
                }
                let key = (
                    plain.origin.source_path.clone(),
                    plain.origin.line,
                    plain.origin.column,
                    generic.origin.source_path.clone(),
                    generic.origin.line,
                    generic.origin.column,
                );
                if !reported.insert(key) {
                    continue;
                }
                push_plain_generic_collision(
                    warnings,
                    name,
                    &generic.origin,
                    &generic.span,
                    &plain.origin,
                );
            }
        }
    }
}

/// Build a [`DefinitionKey`] for a [`RetainedDefinition`] site.
fn definition_key_from_site(site: &RetainedDefinition) -> DefinitionKey {
    DefinitionKey {
        name: site.name.clone(),
        source_path: site.origin.source_path.clone(),
        line: site.origin.line,
        column: site.origin.column,
    }
}

/// Recursively gather retained plain and generic rule definitions from a
/// (post-pruning, post-strength-normalization) tree.
fn collect_retained_definitions(
    nodes: &[WrappedNode],
    out: &mut Vec<RetainedDefinition>,
) {
    for node in nodes {
        if let WrappedNode::RuleLine {
            children,
            origin,
            span,
            ..
        } = node
        {
            let Some(head) = crate::symbols::rule_head(node) else {
                recurse_into_retained(node, out);
                continue;
            };
            if head.assignment != crate::symbols::AssignmentKind::Define {
                recurse_into_retained(node, out);
                continue;
            }
            let is_generic = is_generic_ruleline(children);
            let is_prunable = node.metadata().contains(&crate::MetaData::Prunable);
            out.push(RetainedDefinition {
                name: head.name,
                is_generic,
                is_prunable,
                origin: origin.clone(),
                span: span.clone(),
            });
        }
        recurse_into_retained(node, out);
    }
}

/// Recurse into the children of a non-rule node, continuing the
/// retained-definition collection.
fn recurse_into_retained(
    node: &WrappedNode,
    out: &mut Vec<RetainedDefinition>,
) {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            collect_retained_definitions(children, out);
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Whether a rule line declares a generic parameter list.
fn is_generic_ruleline(children: &[WrappedNode]) -> bool {
    fn has_genericparm(node: &WrappedNode) -> bool {
        match node {
            WrappedNode::Syntax { rule, children, .. } => {
                rule == "genericparm" || children.iter().any(has_genericparm)
            },
            WrappedNode::RuleLine { children, .. } => children.iter().any(has_genericparm),
            WrappedNode::Directive { .. }
            | WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => false,
        }
    }
    children.iter().any(has_genericparm)
}

/// Emit an E013 plain-vs-generic collision diagnostic.
fn push_plain_generic_collision(
    warnings: &mut Vec<crate::Diagnostic>,
    name: &str,
    generic_origin: &crate::SourceOrigin,
    generic_span: &Range<usize>,
    plain_origin: &crate::SourceOrigin,
) {
    warnings.push(crate::Diagnostic {
        code: "E013",
        level: crate::DiagnosticLevel::Error,
        message: format!(
            "rule name collision: `{name}` is defined both as a plain rule and as a generic rule"
        ),
        source_file: Some(generic_origin.source_path.clone()),
        span: Some(generic_span.clone()),
        previous_origin: Some(plain_origin.clone()),
        related: Vec::new(),
    });
}

/// Step 5.12 cross-file export contract surface scan.
///
/// Walks the consumer's user tree, identifies references whose
/// definition sits in an imported library, and emits a `W003`
/// warning for every reference whose target is NOT in the imported
/// library's `exported_names` or `extern_names` set.
///
/// Rules:
/// * Only `import`-shaped directives contribute an [`crate::compiled::ImportedLibrary`]
///   entry — `include` splices are treated as private helpers.
/// * If the imported file is not a CBORK library, no warning is emitted — the export
///   contract only applies to library-shaped modules.
/// * References within the consumer's own file never warn — a file never violates its own
///   export contract.
/// * Transitive use does NOT warn.  A library that exports `T` may internally reference
///   its own private helpers; consumers referencing `T` do not transitively inherit the
///   warning.
/// * `;@ CBORK: Extern ...` names declared by the consumer are exempt from the contract
///   when they appear on the consumer's side — they are explicitly declared external.
/// * References to postlude-injected primitives (`uint`, `bstr`, `any`, ...) never warn —
///   the postlude is not a library and the contract only applies to library files.
fn detect_direct_export_violations(
    pre_prune_nodes: &[WrappedNode],
    post_prune_nodes: &[WrappedNode],
    imported_libraries: &[crate::compiled::ImportedLibrary],
    consumer_extern_names: &HashSet<String>,
    consumer_source_path: &Path,
    consumer_source_text: &str,
    warnings: &mut Vec<crate::Diagnostic>,
) {
    let libs_by_path: HashMap<PathBuf, &crate::compiled::ImportedLibrary> = imported_libraries
        .iter()
        .map(|lib| (lib.canonical_path.clone(), lib))
        .collect();

    // Pass 1: build a map from definition name (bare, alias stripped)
    // to its source path from the PRE-PRUNE tree so we still see
    // imports the reachability pruner dropped.
    let mut def_sources: HashMap<String, PathBuf> = HashMap::new();
    collect_definition_sources(pre_prune_nodes, &mut def_sources);

    // Pass 2: walk the POST-PRUNE consumer's own rules (which are
    // never prunable) and record every typename reference whose
    // definition comes from a non-consumer file.
    let mut reported: HashSet<(PathBuf, String)> = HashSet::new();
    walk_for_cross_file_refs(
        post_prune_nodes,
        &def_sources,
        &libs_by_path,
        consumer_extern_names,
        consumer_source_text,
        consumer_source_path,
        &mut reported,
    );

    let mut diagnostics: Vec<crate::Diagnostic> = reported
        .into_iter()
        .map(|(lib_path, name)| crate::Diagnostic {
            code: "W003",
            level: crate::DiagnosticLevel::Warning,
            message: format!(
                "direct use of non-exported symbol `{name}` from library `{}`; import only the library's `;@ CBORK: Export` or `;@ CBORK: Extern` surface",
                lib_path.display()
            ),
            source_file: Some(consumer_source_path.to_path_buf()),
            span: None,
            previous_origin: None,
            related: Vec::new(),
        })
        .collect();
    diagnostics.sort_by(|a, b| a.message.cmp(&b.message));
    warnings.extend(diagnostics);
}

/// Emit a W007 warning for every imported/included target that
/// is not a CBORK library file.
fn detect_non_library_imports(
    imported_libraries: &[crate::compiled::ImportedLibrary],
    warnings: &mut Vec<crate::Diagnostic>,
) {
    let mut diagnostics: Vec<crate::Diagnostic> = imported_libraries
        .iter()
        .filter(|lib| !lib.is_library)
        .map(|lib| crate::Diagnostic {
            code: "W007",
            level: crate::DiagnosticLevel::Warning,
            message: format!(
                "directly imported/included file `{}` is not a CBORK Library; reusable modules should declare `;@ CBORK: Library`",
                lib.canonical_path.display()
            ),
            source_file: Some(lib.import_origin.source_path.clone()),
            span: None,
            previous_origin: None,
            related: Vec::new(),
        })
        .collect();
    diagnostics.sort_by(|a, b| a.message.cmp(&b.message));
    warnings.extend(diagnostics);
}

/// Walk every top-level `RuleLine` and record its LHS name (with
/// the alias prefix stripped) and its origin source path.  Both the
/// fully-aliased name and the bare unaliased name are recorded so
/// that references recorded by the walker (which see the alias
/// prefix on imported rules) can resolve correctly.  The postlude
/// injects primitives via `MetaData::StandardPostlude`; we keep
/// them in the map so the reference walker can still resolve them,
/// but the postlude's pseudo-path is filtered out when deciding
/// whether a reference crosses a library boundary.
fn collect_definition_sources(
    nodes: &[WrappedNode],
    out: &mut HashMap<String, PathBuf>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                let Some(head) = rule_head_from_children(children) else {
                    continue;
                };
                let WrappedNode::RuleLine { origin, .. } = node else {
                    continue;
                };
                let path = origin.source_path.clone();
                out.entry(head.name.clone()).or_insert(path.clone());
                if let Some((_, bare)) = head.name.rsplit_once('.') {
                    out.entry(bare.to_owned()).or_insert(path);
                }
                collect_definition_sources(children, out);
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                collect_definition_sources(children, out);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Recursive walker that detects references to non-exported
/// library symbols.  Collects every `(library_canonical_path,
/// referenced_name)` pair into `reported`.
fn walk_for_cross_file_refs(
    nodes: &[WrappedNode],
    def_sources: &HashMap<String, PathBuf>,
    libs_by_path: &HashMap<PathBuf, &crate::compiled::ImportedLibrary>,
    consumer_extern_names: &HashSet<String>,
    consumer_source_text: &str,
    consumer_source_path: &Path,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine {
                children,
                origin,
                span,
                ..
            } => {
                // Only enforce the contract for references in the
                // consumer's own rules — a library file never
                // violates its own contract.
                if origin.source_path == consumer_source_path {
                    let rule_source_text =
                        source_text_for_span(consumer_source_text, span).unwrap_or("");
                    walk_rule_line_for_cross_file_refs(
                        children,
                        def_sources,
                        libs_by_path,
                        consumer_extern_names,
                        rule_source_text,
                        reported,
                    );
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                walk_for_cross_file_refs(
                    children,
                    def_sources,
                    libs_by_path,
                    consumer_extern_names,
                    consumer_source_text,
                    consumer_source_path,
                    reported,
                );
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Walk the children of a single top-level `RuleLine` looking for
/// typename references that resolve to a definition in an imported
/// library.  Each reference is checked against the consumer's
/// `extern_names` allow-list and the library's `exported_names` /
/// `extern_names` allow-list.  Violations are recorded into
/// `reported` for the caller to surface as `W003` warnings.
fn walk_rule_line_for_cross_file_refs(
    children: &[WrappedNode],
    def_sources: &HashMap<String, PathBuf>,
    libs_by_path: &HashMap<PathBuf, &crate::compiled::ImportedLibrary>,
    consumer_extern_names: &HashSet<String>,
    consumer_source_text: &str,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    let bound_generic_params = collect_generic_param_names(children);
    let scan = ExportRefScan {
        def_sources,
        libs_by_path,
        consumer_extern_names,
        consumer_source_text,
        bound_generic_params: &bound_generic_params,
    };
    // A top-level rule's body is wrapped in a `Syntax[expr]` node
    // that contains the LHS name, the assignment operator, and the
    // RHS type expression.  Walk down into the expr, skip everything
    // up to and including the assignment, then collect every typename
    // reference in the RHS body that resolves to a definition in an
    // imported library.
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            children: sub,
            ..
        } = child
            && rule == "expr"
        {
            walk_expr_for_cross_file_refs(sub, &scan, reported);
        }
    }
}

/// Shared state for W003 direct-reference scanning within one consumer rule.
struct ExportRefScan<'a> {
    /// Definition-name to source-path map built from the unpruned tree.
    def_sources: &'a HashMap<String, PathBuf>,
    /// Imported library metadata indexed by canonical source path.
    libs_by_path: &'a HashMap<PathBuf, &'a crate::compiled::ImportedLibrary>,
    /// Consumer-declared extern names that are exempt from W003.
    consumer_extern_names: &'a HashSet<String>,
    /// The original source text for the current consumer rule.
    consumer_source_text: &'a str,
    /// Formal generic parameters bound by the current rule definition.
    bound_generic_params: &'a HashSet<String>,
}

/// Walk an `expr` Syntax subtree, skipping the LHS name (everything
/// before `assignt` / `assigng`) and recording typename references
/// found in the RHS body.
fn walk_expr_for_cross_file_refs(
    children: &[WrappedNode],
    scan: &ExportRefScan<'_>,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    let mut past_lhs = false;
    for child in children {
        match child {
            WrappedNode::Syntax { rule, .. } if rule == "assignt" || rule == "assigng" => {
                past_lhs = true;
            },
            _ if past_lhs => {
                collect_type_refs_recursive(child, scan, reported);
            },
            _ => {},
        }
    }
}

/// Walk a `type` / `type1` / `type2` chain looking for the leaf
/// `typename` and check whether it names a symbol defined in an
/// imported library.
fn collect_type_refs_from_typename(
    text: &str,
    children: &[WrappedNode],
    scan: &ExportRefScan<'_>,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    if let Some(name) = leaf_typename(text, children) {
        record_if_violation(&name, scan, reported);
    }
    // Do NOT recurse into children — the typename is a leaf.
    // Descending further would walk into generic-body expansions.
}

/// Walk every nested `Syntax` subtree, descending into the children
/// of any node that is not itself a `typename` / `groupname`, so
/// that typename references buried inside `type` / `type1` /
/// `type2` / `id` chains are still detected.
fn collect_type_refs_recursive(
    node: &WrappedNode,
    scan: &ExportRefScan<'_>,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    let WrappedNode::Syntax {
        rule,
        text,
        children,
        ..
    } = node
    else {
        return;
    };
    match rule.as_str() {
        "typename" | "groupname" => {
            collect_type_refs_from_typename(text, children, scan, reported);
        },
        _ => {
            // Only recurse into children when this node does NOT
            // itself name a typename reference.  If leaf_typename
            // found a name, this node IS the reference; descending
            // further would walk into generic-body expansions.
            if let Some(name) = leaf_typename(text, children) {
                record_if_violation(&name, scan, reported);
            } else {
                for child in children {
                    collect_type_refs_recursive(child, scan, reported);
                }
            }
        },
    }
}

/// Return the leaf typename name if `text` (or its children) contains
/// exactly one.  Strip the alias prefix (everything up to and
/// including the last `.`) because the contract is about the
/// underlying rule, not its alias.
fn leaf_typename(
    text: &str,
    children: &[WrappedNode],
) -> Option<String> {
    let raw = text.trim();
    if !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        let unaliased = raw.rsplit('.').next().unwrap_or(raw);
        return Some(unaliased.to_owned());
    }
    // Generic instantiations like `a2d.argon2id<innerhash>` carry
    // the referenced type before `<`.  Extract it so the walker
    // records the direct reference instead of descending into the
    // generic body.
    if raw.contains('<') {
        let base = raw.split('<').next().unwrap_or(raw);
        let unaliased = base.rsplit('.').next().unwrap_or(base);
        return Some(unaliased.to_owned());
    }
    let mut found: Option<String> = None;
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            text,
            children: sub,
            ..
        } = child
            && matches!(rule.as_str(), "typename" | "groupname")
        {
            if found.is_some() {
                return None;
            }
            found = leaf_typename(text, sub);
        }
    }
    found
}

/// Record a `(library_path, referenced_name)` violation if the
/// referenced name resolves to a non-exported, non-extern symbol
/// in an imported library.  Names that are part of the consumer's
/// own `extern_names` allow-list are always permitted (the consumer
/// has explicitly opted in to those definitions).
fn record_if_violation(
    name: &str,
    scan: &ExportRefScan<'_>,
    reported: &mut HashSet<(PathBuf, String)>,
) {
    if scan.consumer_extern_names.contains(name) {
        return;
    }
    if scan.bound_generic_params.contains(name) {
        return;
    }
    if !source_has_direct_reference(scan.consumer_source_text, name) {
        return;
    }
    let Some(def_source) = scan.def_sources.get(name) else {
        // The name is not a top-level definition anywhere — likely a
        // builtin primitive that the postlude injects.  Skip.
        return;
    };
    let Some(lib) = scan.libs_by_path.get(def_source) else {
        return;
    };
    if lib.exported_names.contains(name) || lib.extern_names.contains(name) {
        return;
    }
    reported.insert((lib.canonical_path.clone(), name.to_owned()));
}

/// Return whether the consumer wrote `name` in non-comment CDDL source.
///
/// W003 is a direct-use contract. Generic expansion intentionally rewrites
/// imported template bodies to the call-site origin so diagnostics point at
/// the instantiation, but private helper references introduced by that
/// expansion were not written by the consumer and must not be reported as
/// direct uses.
fn source_has_direct_reference(
    source: &str,
    name: &str,
) -> bool {
    source
        .lines()
        .filter_map(cddl_source_before_comment)
        .any(|line| contains_cddl_name(line, name))
}

/// Return the original source slice covered by a parser byte span.
fn source_text_for_span<'a>(
    source: &'a str,
    span: &Range<usize>,
) -> Option<&'a str> {
    source.get(span.clone())
}

/// Strip trailing CDDL comments and ignore standalone comment lines.
fn cddl_source_before_comment(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with(';') {
        return None;
    }
    Some(
        line.split_once(';')
            .map_or(line, |(source, _comment)| source),
    )
}

/// Return whether `line` contains `name` as a CDDL identifier token.
fn contains_cddl_name(
    line: &str,
    name: &str,
) -> bool {
    line.match_indices(name).any(|(start, _)| {
        let end = start.saturating_add(name.len());
        let before = line.get(..start).and_then(|s| s.chars().next_back());
        let after = line.get(end..).and_then(|s| s.chars().next());
        !is_cddl_name_char(before) && !is_cddl_name_char(after)
    })
}

/// Return whether a character is part of the CDDL identifier body.
fn is_cddl_name_char(ch: Option<char>) -> bool {
    ch.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

// ---------------------------------------------------------------------------
// Unused import / include / library-export linting (Step 5.12)
// ---------------------------------------------------------------------------

/// Walk the consumer's own rules (the post-prune tree, restricted to
/// rules whose origin is the consumer's source path) and gather
/// every typename / groupname reference, including the LHS name and
/// any references inside map keys, group bodies, and choice arms.
/// The set includes both fully-aliased names (e.g.
/// `lib.public-rule`) and the bare unaliased form so that
/// resolution against directive-selected names and library
/// `exported_names` works for both kinds of consumer syntax.
fn collect_consumer_references(
    nodes: &[WrappedNode],
    consumer_source_path: &Path,
) -> HashSet<String> {
    let mut refs = HashSet::new();
    for node in nodes {
        if let WrappedNode::RuleLine { origin, .. } = node
            && origin.source_path == consumer_source_path
        {
            collect_references_in_node(node, &mut refs);
        }
        // Recurse into nested structure so we still see references
        // inside `Directive` / `Syntax` subtrees for completeness.
        match node {
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                refs.extend(collect_consumer_references(children, consumer_source_path));
            },
            WrappedNode::RuleLine { .. }
            | WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
    refs
}

/// Inner walker that visits every `Syntax` subtree and records each
/// `typename` / `groupname` text it encounters.  The text is recorded
/// as-is, and also stripped of the alias prefix (the substring after
/// the last `.`) so a reference like `lib.public-rule` matches both
/// the full form and the bare `public-rule` form.
fn collect_references_in_node(
    node: &WrappedNode,
    out: &mut HashSet<String>,
) {
    if let WrappedNode::Syntax { rule, text, .. } = node
        && (rule == "typename" || rule == "groupname")
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            out.insert(trimmed.to_owned());
            if let Some((_, bare)) = trimmed.rsplit_once('.') {
                out.insert(bare.to_owned());
            }
        }
    }
    match node {
        WrappedNode::RuleLine { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_references_in_node(child, out);
            }
        },
        // Do not recurse into `Directive` children: those are
        // imported rules, not consumer-authored references.
        WrappedNode::Directive { .. }
        | WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Detect unused `import` / `include` directives and unused selected
/// import names.  Emits `W004` for an entire directive that
/// contributes no referenced name and `W005` for each individual
/// selected name that is never referenced.
///
/// BUG-002: an earlier version of this pass also emitted `W006` for
/// each exported library name that the consumer did not use.  That
/// was the wrong model: a library export is a public API surface,
/// not an obligation for every consumer of the library to reference
/// every exported name.  The `from`-clause path is fully covered
/// by `W005`; whole-library imports intentionally let the consumer
/// pick a subset, so no extra diagnostic should fire.
#[allow(
    clippy::too_many_arguments,
    reason = "Step 5.12 pass needs pre/post trees plus library registry"
)]
fn detect_unused_directives(
    pre_prune_nodes: &[WrappedNode],
    post_prune_nodes: &[WrappedNode],
    imported_libraries: &[crate::compiled::ImportedLibrary],
    consumer_extern_names: &HashSet<String>,
    consumer_source_path: &Path,
    warnings: &mut Vec<crate::Diagnostic>,
) {
    let consumer_refs = collect_consumer_references(post_prune_nodes, consumer_source_path);
    // `consumer_extern_names` is currently unused now that the W006
    // path is gone.  Keep the parameter so the call site is stable
    // for the planned `W006` redefinition (a future "library's
    // exported surface is consumed by way of `;@ CBORK: Extern`"
    // diagnostic could use it).
    let _ = consumer_extern_names;
    let _ = imported_libraries;

    // W004 + W005: walk the pre-prune tree (which still contains
    // the resolved Directive nodes from `resolve_includes`).
    for node in pre_prune_nodes {
        let WrappedNode::Directive {
            directive,
            children,
            span,
            origin,
            ..
        } = node
        else {
            continue;
        };
        // Skip directives that came from imported modules: only the
        // consumer's own directives are reported against the
        // consumer.
        if origin.source_path != consumer_source_path {
            continue;
        }

        // Collect the names actually brought in by this directive:
        // either the explicit `from` list (after aliasing) or every
        // top-level rule that has been spliced into `children`.
        let brought_in: Vec<String> = if directive_has_names(directive) {
            let alias = directive_alias(directive);
            directive_names(directive)
                .iter()
                .map(|n| normalize_directive_name(n, alias))
                .collect()
        } else {
            collect_rule_names(children).into_iter().collect()
        };

        // W004: no brought-in name is referenced.
        let any_used = brought_in.iter().any(|n| consumer_refs.contains(n));
        if !any_used && !brought_in.is_empty() {
            warnings.push(crate::Diagnostic {
                code: "W004",
                level: crate::DiagnosticLevel::Warning,
                message: format!(
                    "unused `{}` directive: no symbol from `{}` is referenced by the consumer",
                    directive_display_name(directive),
                    if directive_has_names(directive) {
                        directive_names(directive).join(", ")
                    } else {
                        "(entire module)".to_owned()
                    },
                ),
                source_file: Some(consumer_source_path.to_path_buf()),
                span: Some(span.clone()),
                previous_origin: None,
                related: Vec::new(),
            });
        }

        // W005: per-name unused for `from` clauses.  Fires even when
        // the directive as a whole is partially used, so a
        // selected name that is genuinely never referenced is
        // reported even if its siblings are.
        if directive_has_names(directive) {
            let alias = directive_alias(directive);
            for raw in directive_names(directive) {
                let normalized = normalize_directive_name(raw, alias);
                if !consumer_refs.contains(&normalized) {
                    warnings.push(crate::Diagnostic {
                        code: "W005",
                        level: crate::DiagnosticLevel::Warning,
                        message: format!(
                            "unused selected import name `{raw}` from `{}`",
                            directive_display_name(directive),
                        ),
                        source_file: Some(consumer_source_path.to_path_buf()),
                        span: Some(span.clone()),
                        previous_origin: None,
                        related: Vec::new(),
                    });
                }
            }
        }
    }
}
