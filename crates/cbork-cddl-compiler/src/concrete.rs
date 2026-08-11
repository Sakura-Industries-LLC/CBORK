// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Concrete CDDL rendering.
//!
//! Walks an enriched [`WrappedNode`] tree — the *complete* tree from
//! [`CompiledCDDL::complete_nodes`], which has already had includes
//! spliced in, generics expanded, and unreachable prunable rules
//! pruned — and produces a "concrete view" of the CDDL the compiler
//! actually sees. Named constants are folded to their resolved values,
//! socket and group-socket plug augmentations are inlined into the
//! group they appear in, type references are recursively inlined,
//! redundant rules are dropped, and only the structurally meaningful
//! top-level rules are emitted.
//!
//! In library mode (`--library` or `;@ CBORK: Library`), the named
//! constant definitions are preserved verbatim so a downstream file
//! can re-include them.
//!
//! The same renderer drives the [`cbork`](crate) `render` subcommand
//! and the diff-style output for `.within` / `.and` diagnostics. The
//! [`ConcretePolicy`] distinguishes the two consumers; the renderer
//! itself is shared.
//!
//! # Architecture
//!
//! * [`render_cddl`] — render a slice of `WrappedNode`s into a sequence of [`Line`]s.
//! * [`render_subtree`] — render a single sub-tree (used by the LSP hover layer).
//! * [`build_resolution`] — build the [`ResolutionMap`] that the renderer consults for
//!   inlining.
//! * [`Line`] / [`LineKind`] — each emitted line carries a tag so the diff renderer can
//!   align LHS against RHS.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
};

use crate::{
    error::{Diagnostic, DiagnosticLevel},
    node::{SourceOrigin, WrappedNode},
    resolver_cache::{EntryState, ResolverCache},
    symbols::{AssignmentKind, RuleHead, rule_head_from_children},
};

/// Which side of a check is being rendered.
///
/// The renderer is identical for both sides; the tag changes so the
/// diff renderer can label its output and so library-mode rules can
/// differ (LHS in a `.within` is the schema under test, RHS is the
/// template).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetSide {
    /// Render the full file (default; for the `cbork render` subcommand).
    #[default]
    Full,
    /// Render the left-hand side of a check.
    Lhs,
    /// Render the right-hand side of a check.
    Rhs,
}

/// Knobs controlling the concrete renderer.
#[allow(clippy::struct_excessive_bools, reason = "option flags not state.")]
#[derive(Debug, Clone)]
pub struct ConcretePolicy {
    /// Emit `; was <name>` comments when a named reference is folded or
    /// inlined. Default: `true`.
    pub provenance_comments: bool,
    /// Emit source and module comments in the concrete view. Default: `true`.
    pub emit_comments: bool,
    /// Which side of a check is being rendered. Default: `Full`.
    pub target: TargetSide,
    /// BUG-005 effective-mode rendering. When `true`, every inline
    /// named-reference is recursively expanded to its concrete body
    /// — even strong definitions, postlude aliases, and definitions
    /// that the normal concrete renderer would keep symbolic for
    /// readability.  Used by `.within`-diagnostic EFFECTIVE LHS/RHS
    /// rendering to let the user see the shape the subtype checker
    /// actually compared.
    pub effective_mode: bool,
}

impl Default for ConcretePolicy {
    fn default() -> Self {
        Self {
            provenance_comments: true,
            emit_comments: true,
            target: TargetSide::Full,
            effective_mode: false,
        }
    }
}

impl ConcretePolicy {
    /// Construct a policy for the `cbork render` subcommand.
    #[must_use]
    pub fn for_render() -> Self {
        Self::default()
    }

    /// Construct a policy for the left-hand side of a diff-style check.
    #[must_use]
    pub fn for_lhs() -> Self {
        Self {
            provenance_comments: false,
            target: TargetSide::Lhs,
            effective_mode: true,
            ..Self::default()
        }
    }

    /// Construct a policy for the right-hand side of a diff-style check.
    #[must_use]
    pub fn for_rhs() -> Self {
        Self {
            provenance_comments: false,
            target: TargetSide::Rhs,
            effective_mode: true,
            ..Self::default()
        }
    }

    /// Enable or suppress source and generated comments.
    #[must_use]
    pub fn with_comments(
        mut self,
        comments: bool,
    ) -> Self {
        self.emit_comments = comments;
        self.provenance_comments = comments;
        self
    }
}

/// One rendered line in a concrete view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Logical kind of this line, used by the diff renderer for
    /// alignment. Plain CDDL viewers can ignore the tag and just print
    /// the text.
    pub kind: LineKind,
    /// The line text, no trailing newline.
    pub text: String,
    /// Indent depth (number of two-space units). `0` for top level.
    pub indent: usize,
    /// Source origin of the line, if it maps to a real source span.
    pub origin: Option<SourceOrigin>,
}

/// Logical classification of a rendered line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    /// Top-level rule definition (`name = type`).
    RuleLine,
    /// Bareword constant or library-export definition preserved because
    /// of library mode.
    KeptDefinition,
    /// `; comment ...` line.
    Comment,
    /// `; Module: ...` and `; End Module: ...` markers.
    ModuleBoundary,
    /// Blank line preserved for readability.
    Blank,
    /// A map key, value, or group element emitted as part of a larger
    /// structure. Used by the diff renderer to align entries.
    GroupEntry,
    /// `; ...` provenance comment attached to the immediately preceding
    /// line. Inline-only; the diff renderer should not strip these.
    Provenance,
}

/// Sequence of rendered lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Concrete {
    /// The ordered line stream produced by the renderer.
    lines: Vec<Line>,
}

impl Concrete {
    /// Construct an empty concrete view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one line.
    pub fn push(
        &mut self,
        line: Line,
    ) {
        self.lines.push(line);
    }

    /// Borrow the underlying lines.
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Mutably borrow the underlying lines for in-place edits.
    pub fn lines_mut(&mut self) -> &mut Vec<Line> {
        &mut self.lines
    }

    /// Number of lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True if no lines have been emitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Flatten the lines into a single CDDL string, one line per
    /// rendered line. Blank lines are emitted as empty lines; comments
    /// are emitted with the leading `;`.
    #[must_use]
    pub fn to_cddl(&self) -> String {
        let mut raw = String::new();
        for line in &self.lines {
            let indent = "  ".repeat(line.indent);
            match line.kind {
                LineKind::Provenance => {
                    let _ = writeln!(raw, "{indent}; {}", line.text);
                },
                LineKind::Comment | LineKind::ModuleBoundary => {
                    // Comment and ModuleBoundary lines already include the
                    // leading `;` in their text, so emit verbatim.
                    let _ = writeln!(raw, "{indent}{}", line.text);
                },
                LineKind::Blank => {
                    let _ = writeln!(raw);
                },
                LineKind::RuleLine | LineKind::KeptDefinition | LineKind::GroupEntry => {
                    let _ = writeln!(raw, "{indent}{}", line.text);
                },
            }
        }
        normalize_rendered_cddl_text(&raw)
    }
}

/// Normalize physical rendered lines after structural rendering.
fn normalize_rendered_cddl_text(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0_usize;
    let physical_lines: Vec<&str> = raw.lines().collect();
    let mut idx = 0;
    while let Some(current) = physical_lines.get(idx) {
        let mut normalized_line = normalize_physical_choice_separator(current.trim_start());
        if let Some(next) = physical_lines.get(idx.saturating_add(1))
            && let Some(merged) = merge_trailing_choice_opener(&normalized_line, next.trim_start())
        {
            normalized_line = merged;
            idx = idx.saturating_add(1);
        }
        let trimmed = normalized_line.as_str();
        if trimmed.is_empty() {
            let _ = writeln!(out);
            idx = idx.saturating_add(1);
            continue;
        }
        if trimmed.starts_with(';') {
            let _ = writeln!(out, "{trimmed}");
            idx = idx.saturating_add(1);
            continue;
        }
        let leading_closers = leading_closer_count(trimmed);
        let indent = depth.saturating_sub(leading_closers);
        let _ = writeln!(out, "{}{}", "  ".repeat(indent), trimmed);
        depth = update_depth(depth, trimmed);
        idx = idx.saturating_add(1);
    }
    out
}

/// Move physical choice separators before provenance comments.
fn normalize_physical_choice_separator(line: &str) -> String {
    let Some(comment_idx) = line.find(" ; from") else {
        return line.to_owned();
    };
    let (code, comment_and_rest) = line.split_at(comment_idx);
    let Some(choice_rel_idx) = comment_and_rest.find(" /") else {
        return normalize_misplaced_closers(line.to_owned());
    };
    let (comment, choice_and_rest) = comment_and_rest.split_at(choice_rel_idx);
    if let Some(rest) = choice_and_rest.strip_prefix(" / ") {
        normalize_misplaced_closers(format!("{} / {}{}", code.trim_end(), rest, comment))
    } else if choice_and_rest == " /" {
        normalize_misplaced_closers(format!("{} /{}", code.trim_end(), comment))
    } else {
        normalize_misplaced_closers(line.to_owned())
    }
}

/// Merge a line ending in `/ ; from ...` with a following opener line.
fn merge_trailing_choice_opener(
    line: &str,
    next: &str,
) -> Option<String> {
    let next = next.trim();
    if !matches!(next, "[" | "(" | "{") {
        return None;
    }
    let comment_idx = line.find(" ; from")?;
    let (code, comment) = line.split_at(comment_idx);
    let code = code.trim_end();
    let code = code.strip_suffix('/')?.trim_end();
    Some(format!("{code} / {next}{comment}"))
}

/// Move any closers accidentally stranded in provenance text back into code.
fn normalize_misplaced_closers(line: String) -> String {
    let Some(comment_idx) = line.find(" ; from") else {
        return line;
    };
    let (code, comment) = line.split_at(comment_idx);
    if !comment.contains(')') {
        return line;
    }
    let close_count = comment.chars().filter(|c| *c == ')').count();
    let cleaned_comment: String = comment.chars().filter(|c| *c != ')').collect();
    let closers = ")".repeat(close_count);
    if let Some((left, right)) = code.split_once(" / ") {
        format!("{left}{closers} / {right}{cleaned_comment}")
    } else if let Some(left) = code.strip_suffix(" /") {
        format!("{left}{closers} /{cleaned_comment}")
    } else {
        format!("{code}{closers}{cleaned_comment}")
    }
}

/// Count closing delimiters at the start of a physical code line.
fn leading_closer_count(code: &str) -> usize {
    code.trim_start()
        .chars()
        .take_while(|c| matches!(c, ']' | ')' | '}'))
        .count()
}

/// Update rendered delimiter depth after emitting one physical line.
fn update_depth(
    mut depth: usize,
    code: &str,
) -> usize {
    for ch in code.chars() {
        match ch {
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {},
        }
    }
    depth
}

// ---------------------------------------------------------------------------
// ResolutionMap
// ---------------------------------------------------------------------------

/// All the data the renderer needs to inline references and fold
/// constants in one pass.
#[derive(Debug)]
pub struct ResolutionMap {
    /// Map from a rule name to the underlying `RuleLine` node. Used to
    /// inline `name = type` references into the rules that reference
    /// them.
    pub definitions: HashMap<String, WrappedNode>,
    /// Map from a socket name (with `$` or `$$` prefix, or just the
    /// bare groupname for implicitly-defined group sockets) to the
    /// list of `//=` augmentation bodies that define the socket's
    /// plug alternatives.
    pub socket_plugs: HashMap<String, Vec<WrappedNode>>,
    /// Map from a type socket name (with `$` prefix) to the list of
    /// `/=` augmentation bodies that define the socket's alternatives.
    pub type_plugs: HashMap<String, Vec<WrappedNode>>,
    /// Primitive-constant resolver cache.
    pub cache: ResolverCache,
    /// Set of rule names that are referenced by at least one other rule
    /// in the file. Used to decide which top-level `Define` rules are
    /// "inlined helpers" (suppressed from the effective view) versus
    /// "public surface" (emitted in library mode).
    pub referenced_names: HashSet<String>,
    /// Names of definitions that participate in genuine (guarded)
    /// recursion: members of a strongly connected component with more
    /// than one symbol, or definitions whose self-reference is wrapped
    /// in a constructor (map/array/tag/occurrence/ctlop/group). These
    /// are never expanded by the renderer; references to them are
    /// emitted symbolically and their definitions are retained.
    pub recursive_symbols: HashSet<String>,
    /// Names of definitions whose RHS top-level choice contains a
    /// provably bare self-arm together with at least one other arm
    /// (`x = x / int`). The bare self-arm contributes no structure and
    /// is elided when rendering; the linter warns about it (`E031`).
    pub elidable_self: HashSet<String>,
    /// Diagnostics collected by the renderer that callers may
    /// surface to the user.  Currently used for group-reference
    /// cycle detection (Step 5.10): the renderer must not stack-
    /// overflow when a group recursively references itself, but
    /// the user still needs to know the cycle was hit.
    pub render_diagnostics: RefCell<Vec<Diagnostic>>,
}

impl ResolutionMap {
    /// Look up a definition by name.
    #[must_use]
    pub fn get(
        &self,
        name: &str,
    ) -> Option<&WrappedNode> {
        self.definitions.get(name)
    }

    /// Return the plug bodies for a socket, if any.
    #[must_use]
    pub fn plugs_for(
        &self,
        name: &str,
    ) -> &[WrappedNode] {
        self.socket_plugs.get(name).map_or(&[], Vec::as_slice)
    }

    /// Return the type socket plug bodies, if any.
    #[must_use]
    pub fn type_plugs_for(
        &self,
        name: &str,
    ) -> &[WrappedNode] {
        self.type_plugs.get(name).map_or(&[], Vec::as_slice)
    }

    /// Resolve a primitive constant from the cache.
    #[must_use]
    pub fn resolve_constant(
        &self,
        name: &str,
    ) -> Option<&EntryState> {
        self.cache.peek(name)
    }

    /// Return whether `name` participates in genuine (guarded)
    /// recursion and must never be expanded by the renderer.
    #[must_use]
    pub fn is_recursive_symbol(
        &self,
        name: &str,
    ) -> bool {
        self.recursive_symbols.contains(name)
    }

    /// Return whether `name` has an elidable bare self-arm in its RHS
    /// choice (`x = x / int`).
    #[must_use]
    pub fn is_elidable_self(
        &self,
        name: &str,
    ) -> bool {
        self.elidable_self.contains(name)
    }

    /// Drain any render-time diagnostics collected so far.  Callers
    /// invoke this after rendering to surface cycle detection and
    /// similar render-only warnings.
    #[must_use]
    pub fn take_render_diagnostics(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut *self.render_diagnostics.borrow_mut())
    }
}

/// Build a [`ResolutionMap`] from a slice of `WrappedNode`s
/// (typically the post-prune `complete_nodes`).
#[must_use]
pub fn build_resolution(nodes: &[WrappedNode]) -> ResolutionMap {
    let mut defs: HashMap<String, WrappedNode> = HashMap::new();
    let mut plugs: HashMap<String, Vec<WrappedNode>> = HashMap::new();
    let mut type_plugs: HashMap<String, Vec<WrappedNode>> = HashMap::new();
    let mut cache = ResolverCache::new();

    collect_all(nodes, &mut defs, &mut plugs, &mut type_plugs, &mut cache);

    let def_names: HashSet<String> = defs.keys().cloned().collect();
    let referenced_names = collect_referenced_names(nodes, &def_names);

    let (recursive_symbols, elidable_self) = classify_recursion(&defs, &def_names);

    ResolutionMap {
        definitions: defs,
        socket_plugs: plugs,
        type_plugs,
        cache,
        referenced_names,
        recursive_symbols,
        elidable_self,
        render_diagnostics: RefCell::new(Vec::new()),
    }
}

/// Classify every definition as either genuinely (guarded) recursive,
/// elidable (provably bare self-arm), or neither.
///
/// The dependency graph over definitions is reduced to strongly
/// connected components:
///
/// * An SCC with more than one symbol is genuine recursion (mutual), even when every
///   internal edge is a bare choice arm: elision is opportunistic, and only the direct
///   self-arm case is proven safe.
/// * A size-1 SCC whose self-reference is wrapped in a constructor
///   (map/array/tag/occurrence/ctlop/group) is genuine recursion.
/// * A definition whose RHS top-level choice has a bare arm naming the definition itself
///   AND at least one other arm (`x = x / int`) is elidable: the bare self-arm
///   contributes no structure, so it is dropped when rendering. A bare self-arm with no
///   other arms (`x = x`) is NOT elided (nothing would remain).
fn classify_recursion(
    defs: &HashMap<String, WrappedNode>,
    def_names: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    // Postlude primitives are never expanded by the renderer and are
    // not part of the user's recursion structure. They must be excluded
    // from the graph: the postlude's own aliases (`bytes = bstr`) would
    // otherwise pair with a user shadow (`bstr = bytes .size 64`) into a
    // spurious strongly connected component.
    let is_postlude = |node: &WrappedNode| {
        node.metadata()
            .iter()
            .any(|m| matches!(m, crate::MetaData::StandardPostlude))
    };

    let mut graph: HashMap<String, HashSet<String>> = HashMap::new();
    let mut guarded_self: HashSet<String> = HashSet::new();
    let mut elidable_self: HashSet<String> = HashSet::new();

    for (name, node) in defs {
        if is_postlude(node) {
            continue;
        }
        let (unguarded, guarded) = classify_def_references(node);
        // Edges exclude the definition itself; self-reference is
        // handled via the self-edge classification below.
        let mut edges: HashSet<String> = unguarded.union(&guarded).cloned().collect();
        edges.remove(name);
        edges.retain(|n| {
            def_names.contains(n) && defs.get(n).is_some_and(|n_node| !is_postlude(n_node))
        });
        graph.insert(name.clone(), edges);

        let has_other_arm = top_level_choice_arm_count(node) > 1;
        if unguarded.contains(name) && has_other_arm {
            elidable_self.insert(name.clone());
        }
        if guarded.contains(name) {
            guarded_self.insert(name.clone());
        }
    }

    let sccs = strongly_connected_components(&graph);

    let mut recursive_symbols: HashSet<String> = HashSet::new();
    for component in sccs {
        if component.len() > 1 {
            recursive_symbols.extend(component);
        } else if let Some(name) = component.into_iter().next() {
            // A size-1 component is recursive when it has a guarded
            // self-reference, or a bare self-arm that cannot be elided
            // (no other arms to keep).
            if guarded_self.contains(&name)
                || (!elidable_self.contains(&name) && def_self_references(&defs[&name], &name))
            {
                recursive_symbols.insert(name);
            }
        }
    }

    (recursive_symbols, elidable_self)
}

/// Classify the references made by one definition's RHS.
///
/// Returns `(unguarded, guarded)`: unguarded names are referenced as
/// bare arms of the top-level choice (`x` in `x = x / int`); every
/// other reference (nested in maps, arrays, tags, occurrences, ctlops,
/// groups, or non-bare choice arms) is guarded.
fn classify_def_references(node: &WrappedNode) -> (HashSet<String>, HashSet<String>) {
    let mut unguarded: HashSet<String> = HashSet::new();
    let mut guarded: HashSet<String> = HashSet::new();
    let WrappedNode::RuleLine { children, .. } = node else {
        return (unguarded, guarded);
    };
    let Some(rhs) = find_rhs(children) else {
        return (unguarded, guarded);
    };
    // Only a top-level `type` choice can carry bare arms.
    if let WrappedNode::Syntax { children: tc, .. } = rhs
        && syntax_rule(rhs) == Some("type")
    {
        for arm in tc.iter().filter(|c| syntax_rule(c) == Some("type1")) {
            if let Some(name) = arm_is_bare_name(arm) {
                unguarded.insert(name);
            } else {
                collect_names_in_node(arm, &mut guarded);
            }
        }
        // Any non-type1 children of the choice (comments/commas carry
        // nothing, but be exhaustive).
        for other in tc.iter().filter(|c| syntax_rule(c) != Some("type1")) {
            collect_names_in_node(other, &mut guarded);
        }
    } else {
        collect_names_in_node(rhs, &mut guarded);
    }
    (unguarded, guarded)
}

/// Return the name if a choice arm is a provably bare reference: its
/// trimmed text is a single bare name and its subtree contains nothing
/// but name-wrapping syntax (`type`/`type1`/`type2`/`typename`/
/// `groupname`/`id`/`name`). Occurrences, ctlops, ranges, groups,
/// keys, tags, and generics all disqualify the arm.
pub(crate) fn arm_is_bare_name(arm: &WrappedNode) -> Option<String> {
    let WrappedNode::Syntax { text, .. } = arm else {
        return None;
    };
    let t = text.trim();
    if t.is_empty() || !t.chars().all(is_reference_name_char) {
        return None;
    }
    if arm_subtree_is_pure_name(arm) {
        Some(t.to_owned())
    } else {
        None
    }
}

/// True if a node's subtree contains only name-wrapping syntax rules.
fn arm_subtree_is_pure_name(node: &WrappedNode) -> bool {
    match node {
        WrappedNode::Syntax { rule, children, .. } => {
            let rule = rule.as_str();
            if matches!(
                rule,
                "type" | "type1" | "type2" | "typename" | "groupname" | "id" | "name" | "bareword"
            ) {
                children.iter().all(arm_subtree_is_pure_name)
            } else {
                false
            }
        },
        _ => false,
    }
}

/// Count the top-level choice arms of a definition's RHS (a `type`
/// node's `type1` children); a non-choice RHS counts as one arm.
fn top_level_choice_arm_count(node: &WrappedNode) -> usize {
    let WrappedNode::RuleLine { children, .. } = node else {
        return 1;
    };
    let Some(rhs) = find_rhs(children) else {
        return 1;
    };
    if let WrappedNode::Syntax { children: tc, .. } = rhs
        && syntax_rule(rhs) == Some("type")
    {
        tc.iter()
            .filter(|c| syntax_rule(c) == Some("type1"))
            .count()
    } else {
        1
    }
}

/// True if a definition references itself anywhere in its RHS.
fn def_self_references(
    node: &WrappedNode,
    name: &str,
) -> bool {
    let mut refs = HashSet::new();
    let WrappedNode::RuleLine { children, .. } = node else {
        return false;
    };
    let Some(rhs) = find_rhs(children) else {
        return false;
    };
    collect_names_in_node(rhs, &mut refs);
    refs.contains(name)
}

/// Return true if any syntax node in the subtree has the given rule.
fn subtree_has_rule(
    node: &WrappedNode,
    target: &str,
) -> bool {
    match node {
        WrappedNode::Syntax { rule, children, .. } => {
            if rule == target {
                return true;
            }
            children.iter().any(|c| subtree_has_rule(c, target))
        },
        _ => false,
    }
}

/// True if a `type1` node is a ctlop/rangeop expression (it has a
/// direct ctlop or rangeop child). Such arms must be parenthesized in
/// a choice: ctlops have no order of evaluation, so the operand scope
/// must be explicit.
fn type1_is_ctlop_expression(node: &WrappedNode) -> bool {
    let WrappedNode::Syntax { children, .. } = node else {
        return false;
    };
    children.iter().any(|c| {
        matches!(
            c,
            WrappedNode::Syntax { rule, .. } if rule == "ctlop" || rule == "rangeop"
        )
    })
}

/// True if a type-shaped node is a ctlop/rangeop expression at its own
/// top level (`bstr .cbor x`, `0 .. 255`, `(bstr .cbor x)`). Unlike a
/// deep subtree scan, this does not fire for a ctlop nested inside a
/// choice arm or map body (`(bytes .size 4) / (bytes .size 16)`,
/// `+ ({ ... .regexp ... } / text)`), which must render as ordinary
/// structure.
fn top_level_ctlop_expression(node: &WrappedNode) -> bool {
    let Some(children) = node_children(node) else {
        return false;
    };
    match syntax_rule(node) {
        Some("type") => {
            let type1s: Vec<&WrappedNode> = children
                .iter()
                .filter(|c| syntax_rule(c) == Some("type1"))
                .collect();
            type1s.len() == 1
                && type1s
                    .first()
                    .is_some_and(|t1| top_level_ctlop_expression(t1))
        },
        Some("type1") => {
            if type1_is_ctlop_expression(node) {
                return true;
            }
            // A single parenthesized operand (`(bstr .cbor x)`) hides
            // the ctlop one level deeper; see through exactly one
            // paren level, no further.
            let type2s: Vec<&WrappedNode> = children
                .iter()
                .filter(|c| syntax_rule(c) == Some("type2"))
                .collect();
            type2s.len() == 1
                && type2s
                    .first()
                    .and_then(|t2| paren_type_inner(t2))
                    .is_some_and(top_level_ctlop_expression)
        },
        _ => false,
    }
}

/// If `node` is a `(...)`-wrapped type2, return its inner type node.
fn paren_type_inner(node: &WrappedNode) -> Option<&WrappedNode> {
    let text = text_of(node).trim();
    if !(text.starts_with('(') && text.ends_with(')')) {
        return None;
    }
    let children = node_children(node)?;
    children
        .iter()
        .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2"))
}

/// Return the syntax children of a `Syntax` node, if any.
fn node_children(node: &WrappedNode) -> Option<&[WrappedNode]> {
    match node {
        WrappedNode::Syntax { children, .. } => Some(children),
        _ => None,
    }
}

impl RenderCx<'_> {
    /// True if a choice arm renders as a ctlop/rangeop expression:
    /// either the arm itself is one, or it is a bare reference to a
    /// definition whose RHS is one. Such arms must be parenthesized in
    /// a choice (ctlops have no order of evaluation).
    fn arm_renders_ctlop_expression(
        &self,
        arm: &WrappedNode,
    ) -> bool {
        if type1_is_ctlop_expression(arm) {
            return true;
        }
        if let Some(name) = arm_is_bare_name(arm)
            && let Some(def_node) = self.resolution.definitions.get(&name)
            && let WrappedNode::RuleLine { children, .. } = def_node
            && let Some(rhs) = find_rhs(children)
        {
            // Only a def whose RHS is a single ctlop expression gets
            // braced as a whole; a def whose RHS is a choice is braced
            // per-arm by the inlined choice itself.
            return rhs_is_single_ctlop_expression(rhs);
        }
        false
    }
}

/// True if a definition's RHS is a single ctlop/rangeop expression
/// (exactly one `type1` with a direct ctlop or rangeop child). Used to
/// brace such a definition when it is inlined as a choice arm.
fn rhs_is_single_ctlop_expression(rhs: &WrappedNode) -> bool {
    let WrappedNode::Syntax { children, .. } = rhs else {
        return false;
    };
    let type1s: Vec<&WrappedNode> = children
        .iter()
        .filter(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .collect();
    type1s.len() == 1
        && type1s
            .first()
            .is_some_and(|t1| type1_is_ctlop_expression(t1))
}

/// Return true if a definition's RHS is itself a ctlop/rangeop
/// expression (a `type1` with a direct ctlop or rangeop child). Such a
/// definition has `type1` shape and cannot be inlined into a ctlop
/// operand, which requires a `type2` (the grammar allows only one
/// ctlop per type1).
fn rhs_is_ctlop_expression(rhs: &WrappedNode) -> bool {
    let WrappedNode::Syntax { children, .. } = rhs else {
        return false;
    };
    children
        .iter()
        .filter(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .any(|t1| {
            matches!(
                t1,
                WrappedNode::Syntax { children: tc, .. }
                    if tc.iter().any(|c| matches!(
                        c,
                        WrappedNode::Syntax { rule, .. } if rule == "ctlop" || rule == "rangeop"
                    ))
            )
        })
}

/// Recursively collect every name referenced inside a syntax node.
fn collect_names_in_node(
    node: &WrappedNode,
    out: &mut HashSet<String>,
) {
    if let WrappedNode::Syntax {
        rule,
        text,
        children,
        ..
    } = node
    {
        if matches!(rule.as_str(), "typename" | "groupname") {
            let t = text.trim();
            if !t.is_empty() {
                out.insert(t.to_owned());
            }
        }
        for child in children {
            collect_names_in_node(child, out);
        }
    }
}

/// Tarjan's strongly connected components over a definition graph.
#[allow(
    clippy::items_after_statements,
    clippy::too_many_arguments,
    clippy::arithmetic_side_effects,
    reason = "Local Tarjan walker; recursion needs the shared mutable state"
)]
fn strongly_connected_components(graph: &HashMap<String, HashSet<String>>) -> Vec<Vec<String>> {
    let mut index = 0_usize;
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut lowlink: HashMap<String, usize> = HashMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut components: Vec<Vec<String>> = Vec::new();

    fn strongconnect(
        name: &str,
        graph: &HashMap<String, HashSet<String>>,
        index: &mut usize,
        indices: &mut HashMap<String, usize>,
        lowlink: &mut HashMap<String, usize>,
        on_stack: &mut HashSet<String>,
        stack: &mut Vec<String>,
        components: &mut Vec<Vec<String>>,
    ) {
        *index += 1;
        indices.insert(name.to_owned(), *index);
        lowlink.insert(name.to_owned(), *index);
        stack.push(name.to_owned());
        on_stack.insert(name.to_owned());

        if let Some(neighbors) = graph.get(name) {
            let neighbor_names: Vec<String> = neighbors.iter().cloned().collect();
            for neighbor in neighbor_names {
                if !indices.contains_key(&neighbor) {
                    strongconnect(
                        &neighbor, graph, index, indices, lowlink, on_stack, stack, components,
                    );
                    let neighbor_low = lowlink[&neighbor];
                    let my_low = lowlink[name];
                    lowlink.insert(name.to_owned(), my_low.min(neighbor_low));
                } else if on_stack.contains(&neighbor) {
                    let neighbor_idx = indices[&neighbor];
                    let my_low = lowlink[name];
                    lowlink.insert(name.to_owned(), my_low.min(neighbor_idx));
                }
            }
        }

        if lowlink[name] == indices[name] {
            let mut component = Vec::new();
            loop {
                let member = stack.pop().unwrap_or_default();
                on_stack.remove(&member);
                component.push(member.clone());
                if member == name {
                    break;
                }
            }
            components.push(component);
        }
    }

    let names: Vec<String> = graph.keys().cloned().collect();
    for name in names {
        if !indices.contains_key(&name) {
            strongconnect(
                &name,
                graph,
                &mut index,
                &mut indices,
                &mut lowlink,
                &mut on_stack,
                &mut stack,
                &mut components,
            );
        }
    }
    components
}

/// Walk a node tree and populate the resolution helpers.
fn collect_all(
    nodes: &[WrappedNode],
    defs: &mut HashMap<String, WrappedNode>,
    plugs: &mut HashMap<String, Vec<WrappedNode>>,
    type_plugs: &mut HashMap<String, Vec<WrappedNode>>,
    cache: &mut ResolverCache,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                if let Some(head) = rule_head_from_children(children) {
                    if head.assignment == AssignmentKind::Define {
                        defs.entry(head.name.clone())
                            .or_insert_with(|| node.clone());
                    }
                    if head.assignment == AssignmentKind::TypeAugment {
                        type_plugs
                            .entry(head.name.clone())
                            .or_default()
                            .push(node.clone());
                    }
                    if head.assignment == AssignmentKind::GroupAugment {
                        plugs
                            .entry(head.name.clone())
                            .or_default()
                            .push(node.clone());
                    }
                    if let Some(value) = extract_literal_value(node) {
                        drop(cache.resolve(&head.name, value));
                    }
                }
                collect_all(children, defs, plugs, type_plugs, cache);
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                collect_all(children, defs, plugs, type_plugs, cache);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Walk the post-resolution tree and collect every rule name that is
/// *referenced* by some other rule (i.e. appears as a `typename` or
/// `groupname` child of a non-LHS position). The set of names NOT in
/// this set are the "public surface" of the file in library mode.
fn collect_referenced_names(
    nodes: &[WrappedNode],
    def_names: &HashSet<String>,
) -> HashSet<String> {
    let mut lhss: HashSet<String> = HashSet::new();
    for node in nodes {
        if let WrappedNode::RuleLine { children, .. } = node
            && let Some(head) = rule_head_from_children(children)
        {
            lhss.insert(head.name);
        }
    }
    let mut referenced: HashSet<String> = HashSet::new();
    walk_for_references(nodes, &lhss, def_names, &mut referenced);
    referenced
}

/// Inner walker for [`collect_referenced_names`]. Kept at module
/// scope so `#[allow]` annotations don't bleed into item-level lint
/// suppressions elsewhere.
#[allow(
    clippy::items_after_statements,
    reason = "Local helper only used from collect_referenced_names"
)]
fn walk_for_references(
    nodes: &[WrappedNode],
    lhs_set: &HashSet<String>,
    def_names: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                let mut past_lhs = false;
                for c in children {
                    if let WrappedNode::Syntax { rule, .. } = c {
                        match rule.as_str() {
                            "assignt" | "assigng" => past_lhs = true,
                            "typename" | "groupname" => {
                                let Some(name) = syntax_text(c) else {
                                    continue;
                                };
                                if past_lhs && def_names.contains(&name) && !lhs_set.contains(&name)
                                {
                                    out.insert(name);
                                }
                            },
                            _ => {},
                        }
                    }
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                walk_for_references(children, lhs_set, def_names, out);
            },
            _ => {},
        }
    }
}

/// Best-effort text extraction for a syntax node (used by reference
/// collection).
fn syntax_text(node: &WrappedNode) -> Option<String> {
    if let WrappedNode::Syntax { text, .. } = node {
        Some(text.trim().to_owned())
    } else {
        None
    }
}

/// Try to extract a primitive literal value from a `RuleLine`'s `RHS`.
fn extract_literal_value(node: &WrappedNode) -> Option<EntryState> {
    let WrappedNode::RuleLine { children, .. } = node else {
        return None;
    };
    let WrappedNode::Syntax {
        rule, children: tc, ..
    } = children
        .iter()
        .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "expr"))
        .or_else(|| {
            children
                .iter()
                .find(|c| matches!(c, WrappedNode::Syntax { .. }))
        })?
    else {
        return None;
    };
    if rule != "expr" {
        return None;
    }
    let type_node = tc.iter().find(
        |c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "grpent"),
    )?;
    let WrappedNode::Syntax {
        children: type_children,
        ..
    } = type_node
    else {
        return None;
    };
    // A multi-arm choice is not a constant: caching the first arm would
    // collapse `"" / text .regexp "x"` to just the first literal when
    // the rule is folded. Only pure single-literal rules are constants.
    let type1_count = type_children
        .iter()
        .filter(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .count();
    if type1_count > 1 {
        return None;
    }
    // A range (`0..255`) or control (`"a" .size 2`) expression is not
    // a constant either: caching the first operand would collapse the
    // range to its lower bound or drop the ctlop when the rule is
    // folded. Scan the RHS for rangeop/ctlop anywhere.
    if subtree_has_rule(type_node, "rangeop") || subtree_has_rule(type_node, "ctlop") {
        return None;
    }
    let t2 = type_children.iter().find_map(|c| {
        match c {
            WrappedNode::Syntax { rule, children, .. } if rule == "type" => {
                children.iter().find_map(|cc| {
                    match cc {
                WrappedNode::Syntax {
                    rule, children: ic, ..
                } if rule == "type1" => ic
                    .iter()
                    .find(|icc| matches!(icc, WrappedNode::Syntax { rule, .. } if rule == "type2")),
                _ => None,
            }
                })
            },
            WrappedNode::Syntax { rule, children, .. } if rule == "type1" => {
                children
                    .iter()
                    .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type2"))
            },
            WrappedNode::Syntax { rule, children, .. } if rule == "grpent" => Some(c),
            _ => None,
        }
    })?;
    let value_node = match t2 {
        WrappedNode::Syntax { children, .. } => {
            children
                .iter()
                .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "value"))
        },
        _ => return None,
    };
    let value_node = value_node?;
    let WrappedNode::Syntax { text, children, .. } = value_node else {
        return None;
    };
    let trimmed = text.trim();
    if let Ok(n) = trimmed.parse::<i128>() {
        return Some(EntryState::Integer(n));
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        return Some(EntryState::Float(f));
    }
    if trimmed.starts_with('"')
        && let Some(body) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
    {
        return Some(EntryState::Text(
            crate::literals::text::TextLiteralBytes::from_bytes(body.as_bytes().to_vec()),
        ));
    }
    if trimmed.starts_with('h') || trimmed.starts_with('b') {
        return Some(EntryState::Bytes(
            crate::literals::byte::ByteLiteralBytes::from_bytes(trimmed.as_bytes().to_vec()),
        ));
    }
    let _ = children;
    None
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Render a slice of `WrappedNode`s into a structured concrete view.
///
/// `resolution` is the [`ResolutionMap`] built from the same set of
/// nodes (typically via [`build_resolution`] on `complete_nodes`).
///
/// Effective-view semantics:
///
/// * The first top-level `Define` rule is always emitted.
/// * Every other top-level `Define` rule is suppressed (its contents are inlined wherever
///   the rule is referenced).
/// * Top-level group augmentations (`//=`) are never emitted as separate lines; they
///   expand in place at the use site.
#[must_use]
pub fn render_cddl(
    nodes: &[WrappedNode],
    resolution: &ResolutionMap,
    policy: &ConcretePolicy,
) -> Concrete {
    let mut out = Concrete::new();
    let mut cx = RenderCx::new(resolution, policy);
    let first_name = first_define_name(nodes);

    for node in nodes {
        render_top(node, &mut cx, 0, first_name.as_deref(), &mut out);
    }

    // Output-based reachability: the first pass inlines every reference
    // it can; anything emitted as a bare (symbolic) name is recorded in
    // `symbolic_refs`.  Those definitions must remain in the document or
    // the intermediate lint reports `E016` undefined references.  Retain
    // them, iterating to a fixed point because a retained definition can
    // itself reference further definitions symbolically. Each retained
    // definition is rendered into its own buffer and appended in
    // name-sorted order at the end, so the retention layout does not
    // depend on the iteration in which a reference was first recorded
    // (which varies with how the source document is shaped).
    let retained = retain_referenced_definitions(&mut cx);
    let retained_out = retained.into_values().collect::<String>();
    let mut sink = Concrete::new();
    for line in retained_out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let spaces = line.chars().take_while(|c| *c == ' ').count();
        sink.push(Line {
            kind: LineKind::RuleLine,
            text: line.trim_start().to_owned(),
            indent: spaces / 2,
            origin: None,
        });
    }
    out.lines_mut().extend(sink.lines().iter().cloned());
    out
}

/// Output-based reachability: render every definition recorded as a
/// bare (symbolic) reference so the document stays self-contained.
///
/// The first render pass inlines everything it can; anything emitted
/// as a bare name is recorded in `symbolic_refs`. Those definitions
/// must remain in the document or the intermediate lint reports `E016`
/// undefined references. This retains them, iterating to a fixed point
/// because a retained definition can itself reference further
/// definitions symbolically. Each retained definition is rendered into
/// its own buffer and returned in name-sorted order, so the layout
/// does not depend on the iteration in which a reference was first
/// recorded (which varies with how the source document is shaped).
fn retain_referenced_definitions(cx: &mut RenderCx<'_>) -> BTreeMap<String, String> {
    let mut retained: BTreeMap<String, String> = BTreeMap::new();
    let mut emitted_augments: HashSet<String> = HashSet::new();
    loop {
        let mut progressed = false;
        let pending: Vec<String> = cx
            .symbolic_refs
            .iter()
            .filter(|name| !retained.contains_key(*name))
            .cloned()
            .collect();
        for name in pending {
            // A reference recorded bare (an unqualified name inside an
            // imported generic body, which the alias wrap leaves
            // untouched) may resolve to a qualified definition
            // (`global-attributes` → `cst.global-attributes`).
            let def_node = cx.resolution.definitions.get(&name).or_else(|| {
                let suffix = format!(".{name}");
                cx.resolution
                    .definitions
                    .iter()
                    .find(|(k, _)| k.ends_with(&suffix))
                    .map(|(_, v)| v)
            });
            let Some(def_node) = def_node else {
                // A symbolic reference to a type plug (`$eid` kept
                // symbolic as a `.within` LHS) resolves through its
                // `/= ` augment lines, which live in `type_plugs`;
                // retain them or the intermediate lint cannot resolve
                // the socket (`E030`).
                let augments = cx.resolution.type_plugs.get(&name).or_else(|| {
                    let suffix = format!(".{name}");
                    cx.resolution
                        .type_plugs
                        .iter()
                        .find(|(k, _)| k.ends_with(&suffix))
                        .map(|(_, v)| v)
                });
                if let Some(augments) = augments {
                    let mut body = Concrete::new();
                    for augment in augments {
                        let WrappedNode::RuleLine { children, .. } = augment else {
                            continue;
                        };
                        let Some(head) = rule_head_from_children(children) else {
                            continue;
                        };
                        // Every augment arm is emitted (the socket's
                        // choice set is the union of all `/= ` lines),
                        // unless the file-level emission already placed
                        // it.
                        if !emitted_augments.insert(augment_identity(augment)) {
                            continue;
                        }
                        render_type_augment(augment, &head, cx, 0, &mut body);
                    }
                    let body = body.to_cddl();
                    if !body.is_empty() && retained.insert(name.clone(), body).is_none() {
                        progressed = true;
                    }
                }
                continue;
            };
            let WrappedNode::RuleLine { children, .. } = def_node else {
                continue;
            };
            let Some(head) = rule_head_from_children(children) else {
                continue;
            };
            if def_node
                .metadata()
                .iter()
                .any(|m| matches!(m, crate::MetaData::StandardPostlude))
            {
                continue;
            }
            match head.assignment {
                AssignmentKind::Define => {
                    let mut body = Concrete::new();
                    render_define(def_node, &head, cx, 0, &mut body, false);
                    let body = body.to_cddl();
                    if !body.is_empty() && retained.insert(name.clone(), body).is_none() {
                        progressed = true;
                    }
                },
                // A symbolic reference to a type plug (`$eid` kept
                // symbolic as a `.within` LHS) needs its `/= ` augment
                // lines retained, or the intermediate lint cannot
                // resolve the socket (`E030`).
                AssignmentKind::TypeAugment => {
                    let mut body = Concrete::new();
                    render_type_augment(def_node, &head, cx, 0, &mut body);
                    let body = body.to_cddl();
                    if !body.is_empty() && retained.insert(name.clone(), body).is_none() {
                        progressed = true;
                    }
                },
                AssignmentKind::GroupAugment => {},
            }
        }
        if !progressed {
            break;
        }
    }
    retained
}

/// Find the name of the first top-level `Define` rule. The first rule
/// is always emitted in the effective view, regardless of whether it
/// is referenced anywhere.
fn first_define_name(nodes: &[WrappedNode]) -> Option<String> {
    for node in nodes {
        if let WrappedNode::RuleLine { children, .. } = node
            && let Some(head) = rule_head_from_children(children)
            && head.assignment == AssignmentKind::Define
        {
            return Some(head.name);
        }
    }
    None
}

/// Render a single sub-tree (used by LSP hover and the future diff
/// renderer).
#[must_use]
pub fn render_subtree(
    node: &WrappedNode,
    resolution: &ResolutionMap,
    policy: &ConcretePolicy,
) -> Concrete {
    let mut out = Concrete::new();
    let mut cx = RenderCx::new(resolution, policy);
    if policy.target != TargetSide::Full {
        render_pretty_rhs(&mut cx, node, 0, text_of(node), &mut out);
        if !out.lines().is_empty() {
            return out;
        }
    }
    let mut prov: Option<(String, String)> = None;
    let (rendered, _) = cx.render_with_inlining(node, &mut prov);
    out.push(Line {
        kind: LineKind::RuleLine,
        text: rendered,
        indent: 0,
        origin: None,
    });
    out
}

/// Convenience: render a slice to a CDDL string.
#[must_use]
pub fn render_to_string(
    nodes: &[WrappedNode],
    resolution: &ResolutionMap,
    policy: &ConcretePolicy,
) -> String {
    render_cddl(nodes, resolution, policy).to_cddl()
}

// ---------------------------------------------------------------------------
// Top-level rendering
// ---------------------------------------------------------------------------

/// A stable identity for a socket augment rule, used to avoid emitting
/// the same augment twice during the retention fixed-point.
fn augment_identity(node: &WrappedNode) -> String {
    match node {
        WrappedNode::RuleLine { text, origin, .. } => {
            format!("{text}#{}:{}", origin.line, origin.column)
        },
        _ => String::new(),
    }
}

/// Emit a `name /= type` augment rule with its RHS rendered concretely.
/// Verbatim source text would reference definitions that concrete mode
/// drops (E016); the concrete RHS keeps the line self-contained.
fn render_type_augment(
    node: &WrappedNode,
    head: &RuleHead,
    cx: &mut RenderCx<'_>,
    indent: usize,
    out: &mut Concrete,
) {
    let WrappedNode::RuleLine {
        children, origin, ..
    } = node
    else {
        return;
    };
    let rhs = find_rhs(children);
    {
        let mut head_text = String::new();
        let _ = write!(&mut head_text, "{} /= ", head.name);
        out.push(Line {
            kind: LineKind::RuleLine,
            text: head_text,
            indent,
            origin: Some(origin.clone()),
        });
    }
    if let Some(rhs_node) = rhs {
        let rhs_text = text_of(rhs_node).trim().to_owned();
        let body_start = out.len();
        render_pretty_rhs(cx, rhs_node, indent.saturating_add(1), &rhs_text, out);
        if out.len().saturating_sub(body_start) >= 1
            && let Some(first_body) = out.lines().get(body_start)
        {
            let first_body_text = first_body.text.clone();
            let body_tail: Vec<Line> = out
                .lines()
                .get(body_start.saturating_add(1)..)
                .unwrap_or_default()
                .to_vec();
            out.lines_mut().truncate(body_start);
            if let Some(head) = out.lines_mut().last_mut() {
                head.text.push_str(&first_body_text);
            }
            for mut tail_line in body_tail {
                tail_line.indent = tail_line.indent.saturating_sub(1);
                out.push(tail_line);
            }
        }
    }
}

/// Walk one top-level node, deciding whether to skip it, keep it
/// verbatim, or render with substitutions.
#[allow(
    clippy::ref_option,
    reason = "Mirrors the structure of the cached lookup in ResolutionMap"
)]
fn render_top(
    node: &WrappedNode,
    cx: &mut RenderCx<'_>,
    indent: usize,
    first_name: Option<&str>,
    out: &mut Concrete,
) {
    match node {
        WrappedNode::RuleLine { children, .. } => {
            let Some(head) = rule_head_from_children(children) else {
                return;
            };
            match head.assignment {
                AssignmentKind::GroupAugment | AssignmentKind::TypeAugment => {
                    // `//=`/`/=` augmentations are socket-plug extensions;
                    // they are expanded in place at the plug use site or
                    // emitted by the output-based retention pass
                    // (name-sorted, like retained definitions) so their
                    // position does not depend on whether the rule came
                    // from the file's own tree or an imported module.
                },
                AssignmentKind::Define => {
                    let is_first = first_name == Some(head.name.as_str());

                    if is_first {
                        // Always emit the first rule.
                        render_define(node, &head, cx, indent, out, is_first);
                    } else {
                        // Concrete mode: the rule is inlined wherever it
                        // is used. Drop the separate top-level line.
                    }
                },
            }
        },
        WrappedNode::Comment { text, origin, .. } => {
            let body = text.trim();
            // `;@ CBORK:` directives are semantically meaningful (the
            // Library marker and Extern declarations change how the
            // document is compiled), so they survive even in
            // no-comments mode — the re-lint of the rendered document
            // needs them to recognize extern names. They are emitted
            // once (the same marker can appear on several nodes when a
            // library imports another library carrying it).
            if !cx.policy.emit_comments && !body.starts_with(";@ CBORK:") {
                return;
            }
            if body.starts_with(";@ CBORK:") && !cx.emitted_directives.insert(body.to_owned()) {
                return;
            }
            if !body.is_empty() {
                out.push(Line {
                    kind: LineKind::Comment,
                    text: body.to_owned(),
                    indent,
                    origin: Some(origin.clone()),
                });
            }
        },
        WrappedNode::ModuleStart { text, origin, .. }
        | WrappedNode::ModuleEnd { text, origin, .. } => {
            if !cx.policy.emit_comments {
                return;
            }
            out.push(Line {
                kind: LineKind::ModuleBoundary,
                text: text.trim().to_owned(),
                indent,
                origin: Some(origin.clone()),
            });
        },
        WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
            for child in children {
                render_top(child, cx, indent, first_name, out);
            }
        },
    }
}

/// Render a `name = type` rule with substitutions applied.
///
/// `is_root` is true when this is the document's first define (the
/// rendered subject); a root that shadows a postlude or imported name
/// (and is therefore flagged redundant) must still render — the
/// round-trip's second pass would otherwise emit nothing for it.
fn render_define(
    node: &WrappedNode,
    head: &RuleHead,
    cx: &mut RenderCx<'_>,
    indent: usize,
    out: &mut Concrete,
    is_root: bool,
) {
    if !is_root
        && node.metadata().iter().any(|m| {
            matches!(
                m,
                crate::MetaData::RedundantDefinition | crate::MetaData::ConflictingDefinition
            )
        })
    {
        return;
    }

    let WrappedNode::RuleLine {
        children, origin, ..
    } = node
    else {
        return;
    };

    let rhs = find_rhs(children);
    {
        let mut head_text = String::new();
        let _ = write!(&mut head_text, "{}", head.name);
        if let Some(genericparm) = find_genericparm(children) {
            let _ = write!(&mut head_text, "{}", genericparm.trim());
        }
        let _ = write!(&mut head_text, " = ");
        out.push(Line {
            kind: LineKind::RuleLine,
            text: head_text,
            indent,
            origin: Some(origin.clone()),
        });
    }

    if let Some(rhs_node) = rhs {
        let rhs_text = text_of(rhs_node).trim().to_owned();
        let body_start = out.len();
        let previous_def = cx.current_def.replace(head.name.clone());
        render_pretty_rhs(cx, rhs_node, indent.saturating_add(1), &rhs_text, out);
        cx.current_def = previous_def;
        if out.len().saturating_sub(body_start) >= 1
            && let Some(first_body) = out.lines().get(body_start)
        {
            let first_body_text = first_body.text.clone();
            let body_tail: Vec<Line> = out
                .lines()
                .get(body_start.saturating_add(1)..)
                .unwrap_or_default()
                .to_vec();
            out.lines_mut().truncate(body_start);
            if let Some(head) = out.lines_mut().last_mut() {
                head.text.push_str(&first_body_text);
            }
            for mut tail_line in body_tail {
                tail_line.indent = tail_line.indent.saturating_sub(1);
                out.push(tail_line);
            }
        }
    }
}

/// Pretty-print the RHS of a top-level rule. The result is one or more
/// `Line`s pushed onto `out`, with `Line.indent` honoured by
/// `Concrete::to_cddl`.
///
/// Layout strategy:
///
/// * Short RHS (single typename, single literal, no `/`/`{`/`[`/`.within`) → one line,
///   the existing compact inlining text.
/// * `type` whose `type1` children are joined by `/` or `//` (a choice) → one indented
///   line per arm, with `,` or `or` separators; the first arm sits on the same line as
///   the LHS, the rest are stacked below at `indent` (one line each, `; provenance` for
///   any inlined references). Single-arm choice stays inline.
/// * `type2` whose source text starts with `{` or `[` → a block: opening bracket on its
///   own line, then one entry per line, then closing bracket on its own line, with
///   `,`-separated entries at `indent+1`. Each entry's grpent may further be a multi-line
///   group if it's a plug expansion.
/// * `.within` LHS expressions are pretty-printed recursively; `.within` sits on its own
///   line at the parent indent; the RHS is a multi-line block.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "Bounded indent arithmetic and indexed access on checked-bounds slices"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Single dispatch site, splitting harms readability"
)]
fn render_pretty_rhs(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
    indent: usize,
    source_text: &str,
    out: &mut Concrete,
) {
    for line in render_pretty_lines(cx, rhs_node, source_text) {
        out.push(Line {
            kind: LineKind::GroupEntry,
            text: line.text,
            indent: indent.saturating_add(line.indent),
            origin: None,
        });
    }
}

/// One physical pretty-rendered line before final text normalization.
#[derive(Debug, Clone)]
struct PrettyLine {
    /// Indent level relative to the expression being rendered.
    indent: usize,
    /// CDDL text for the physical line, without trailing newline.
    text: String,
}

/// Construct a [`PrettyLine`].
fn line(
    indent: usize,
    text: impl Into<String>,
) -> PrettyLine {
    PrettyLine {
        indent,
        text: text.into(),
    }
}

/// Add extra indentation to all but the first line.
fn nest_following_lines(
    lines: &mut [PrettyLine],
    extra_indent: usize,
) {
    for rendered_line in lines.iter_mut().skip(1) {
        rendered_line.indent = rendered_line.indent.saturating_add(extra_indent);
    }
}

/// Convert already-rendered multi-line text back into structured lines.
fn split_rendered_lines(text: &str) -> Vec<PrettyLine> {
    text.lines()
        .map(|raw| {
            let spaces = raw.chars().take_while(|c| *c == ' ').count();
            let indent = spaces / 2;
            line(indent, raw.trim_start().to_owned())
        })
        .collect()
}

/// Convert rendered text to one or more [`PrettyLine`]s.
fn rendered_text_to_lines(text: String) -> Vec<PrettyLine> {
    if text.contains('\n') {
        split_rendered_lines(&text)
    } else if let Some(lines) = structured_within_flat_lines(&text) {
        lines
    } else if let Some(lines) = structured_flat_lines(&text) {
        lines
    } else {
        vec![line(0, text)]
    }
}

/// Reflow a flat `.within` expression into physical lines.
///
/// Diagnostics must not collapse effective `.within` chains into a
/// single unreadable line. This catches text produced by inlining named
/// definitions, where we no longer have the original operand AST but
/// still have balanced CDDL text.
fn structured_within_flat_lines(text: &str) -> Option<Vec<PrettyLine>> {
    let trimmed = text.trim();
    let within_pos = find_within_split(trimmed)?;
    let (lhs_src, rhs_src_with_op) = trimmed.split_at(within_pos);
    let rhs_src = rhs_src_with_op.strip_prefix(".within")?.trim();
    let mut lhs_lines = rendered_within_operand_lines(lhs_src.trim());
    let rhs_lines = rendered_within_operand_lines(rhs_src);
    join_control_lines(lhs_lines.as_mut_slice(), rhs_lines, ".within")
}

/// Render one operand of a flat `.within` expression.
///
/// Unlike general expression rendering, `.within` operands should not
/// keep even a single-entry `{ ... }` or `[ ... ]` wrapper on one line:
/// diagnostics need stable vertical structure.
fn rendered_within_operand_lines(text: &str) -> Vec<PrettyLine> {
    if let Some(lines) = structured_within_flat_lines(text) {
        lines
    } else if let Some(lines) = structured_wrapped_flat_lines(text, true) {
        lines
    } else {
        rendered_text_to_lines(text.to_owned())
    }
}

/// Join structured control-operator operands without ever producing a
/// single physical line for a `.within` chain.
fn join_control_lines(
    left_lines: &mut [PrettyLine],
    right_lines: Vec<PrettyLine>,
    op: &str,
) -> Option<Vec<PrettyLine>> {
    if left_lines.is_empty() {
        return Some(right_lines);
    }
    if right_lines.is_empty() {
        return Some(left_lines.to_vec());
    }
    if left_lines.len() == 1 && right_lines.len() == 1 && op == ".within" {
        let mut out = left_lines.to_vec();
        out.push(line(0, op.to_owned()));
        out.extend(right_lines);
        return Some(out);
    }
    let (first_right, remaining_right) = right_lines.split_first()?;
    let mut out = left_lines.to_vec();
    if let Some(last_left) = out.last_mut() {
        last_left.text.push(' ');
        last_left.text.push_str(op);
        last_left.text.push(' ');
        last_left.text.push_str(&first_right.text);
    }
    out.extend(remaining_right.iter().cloned());
    Some(out)
}

/// Reflow a flat but structurally delimited effective rendering into
/// readable physical lines. This is deliberately syntax-light: it
/// only splits at top-level separators inside a balanced wrapper and
/// leaves quoted strings untouched.
fn structured_flat_lines(text: &str) -> Option<Vec<PrettyLine>> {
    structured_wrapped_flat_lines(text, false)
}

/// Reflow a flat expression with matching outer delimiters.
///
/// When `force_single_part` is true, even a single-entry `{ ... }`,
/// `[ ... ]`, or `( ... )` wrapper is expanded to three physical
/// lines. This is used for `.within` operands where compact output is
/// harder to read than a stable block shape.
fn structured_wrapped_flat_lines(
    text: &str,
    force_single_part: bool,
) -> Option<Vec<PrettyLine>> {
    let trimmed = text.trim();
    let (open, close) = matching_outer_delimiters(trimmed)?;
    let inner = strip_outer_delimiters(trimmed, open, close)?;
    let separator = if has_top_level_separator(inner, ',') {
        Some((',', ","))
    } else if open == '('
        && !has_top_level_member_operator(inner)
        && has_top_level_separator(inner, '/')
    {
        Some(('/', " /"))
    } else if force_single_part {
        None
    } else {
        return None;
    };
    let parts = if let Some((separator_char, _)) = separator {
        split_top_level(inner, separator_char)
    } else {
        vec![inner]
    };
    if parts.len() <= 1 && !force_single_part {
        return None;
    }
    let mut lines = vec![line(0, open.to_string())];
    let last_index = parts.len().saturating_sub(1);
    for (idx, part) in parts.into_iter().enumerate() {
        let mut part_lines =
            structured_flat_lines(part.trim()).unwrap_or_else(|| vec![line(0, part.trim())]);
        if idx != last_index
            && let Some((_, separator_text)) = separator
            && let Some(last) = part_lines.last_mut()
        {
            last.text.push_str(separator_text);
        }
        for mut part_line in part_lines {
            part_line.indent = part_line.indent.saturating_add(1);
            lines.push(part_line);
        }
    }
    lines.push(line(0, close.to_string()));
    Some(lines)
}

/// Return the matching outer delimiters if `text` is a wrapped
/// expression.
fn matching_outer_delimiters(text: &str) -> Option<(char, char)> {
    let open = text.chars().next()?;
    let close = match open {
        '{' => '}',
        '[' => ']',
        '(' => ')',
        _ => return None,
    };
    text.ends_with(close).then_some((open, close))
}

/// Strip a proven outer delimiter pair only when the closing delimiter
/// balances the first character, not an earlier nested expression.
fn strip_outer_delimiters(
    text: &str,
    open: char,
    close: char,
) -> Option<&str> {
    let mut depth = 0_usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            in_string = Some(ch);
            continue;
        }
        if ch == open {
            depth = depth.saturating_add(1);
        } else if ch == close {
            depth = depth.saturating_sub(1);
            if depth == 0 && chars.peek().is_some() {
                return None;
            }
            if depth == 0 {
                return text.get(open.len_utf8()..idx);
            }
        }
    }
    None
}

/// Whether `separator` appears at top level in `text`.
fn has_top_level_separator(
    text: &str,
    separator: char,
) -> bool {
    split_top_level(text, separator).len() > 1
}

/// Whether `text` contains a top-level map/group member operator.
fn has_top_level_member_operator(text: &str) -> bool {
    let mut paren = 0_usize;
    let mut square = 0_usize;
    let mut curly = 0_usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            in_string = Some(ch);
            continue;
        }
        match ch {
            '(' => paren = paren.saturating_add(1),
            ')' => paren = paren.saturating_sub(1),
            '[' => square = square.saturating_add(1),
            ']' => square = square.saturating_sub(1),
            '{' => curly = curly.saturating_add(1),
            '}' => curly = curly.saturating_sub(1),
            ':' if paren == 0 && square == 0 && curly == 0 => return true,
            '=' if paren == 0 && square == 0 && curly == 0 => {
                if let Some((_, '>')) = chars.peek() {
                    return true;
                }
            },
            _ => {},
        }
    }
    false
}

/// Split `text` on a separator that is not nested inside delimiters or
/// quoted strings.
fn split_top_level(
    text: &str,
    separator: char,
) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0_usize;
    let mut paren = 0_usize;
    let mut square = 0_usize;
    let mut curly = 0_usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in text.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            in_string = Some(ch);
            continue;
        }
        match ch {
            '(' => paren = paren.saturating_add(1),
            ')' => paren = paren.saturating_sub(1),
            '[' => square = square.saturating_add(1),
            ']' => square = square.saturating_sub(1),
            '{' => curly = curly.saturating_add(1),
            '}' => curly = curly.saturating_sub(1),
            _ if ch == separator && paren == 0 && square == 0 && curly == 0 => {
                if let Some(part) = text.get(start..idx) {
                    parts.push(part);
                }
                start = idx.saturating_add(ch.len_utf8());
            },
            _ => {},
        }
    }
    if let Some(part) = text.get(start..) {
        parts.push(part);
    }
    parts
}

/// Append text to code before any trailing provenance comment.
fn append_before_provenance_comment(
    text: &mut String,
    suffix: &str,
) {
    if let Some(idx) = text.find(" ; from") {
        let (head, tail) = text.split_at(idx);
        *text = format!("{head}{suffix}{tail}");
    } else {
        text.push_str(suffix);
    }
}

/// Append a `/` choice separator to the final line of an arm.
fn append_choice_separator(lines: &mut [PrettyLine]) {
    if let Some(last) = lines.last_mut() {
        append_before_provenance_comment(&mut last.text, " /");
    }
}

/// True if `text` contains a ctlop (`.name`) at bracket depth zero,
/// outside strings. Used to decide whether a rendered operand needs
/// parentheses when it is joined by another control operator.
fn has_top_level_ctlop(text: &str) -> bool {
    let mut depth = 0usize;
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    let mut prev_was_space = false;
    for byte in text.bytes() {
        if escaped {
            escaped = false;
            prev_was_space = false;
            continue;
        }
        if in_double {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_double = false;
            }
            prev_was_space = false;
            continue;
        }
        if in_single {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'\'' {
                in_single = false;
            }
            prev_was_space = false;
            continue;
        }
        match byte {
            b'"' => in_double = true,
            b'\'' => in_single = true,
            b'(' | b'[' | b'{' => {
                depth = depth.saturating_add(1);
                prev_was_space = false;
            },
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                prev_was_space = false;
            },
            b' ' | b'\t' | b'\n' => prev_was_space = true,
            b'.' if depth == 0 && prev_was_space => return true,
            _ => prev_was_space = false,
        }
    }
    false
}

/// Return true when source text contains a top-level `/` separator.
fn has_top_level_choice_separator(text: &str) -> bool {
    let mut depth = 0_usize;
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => return true,
            _ => {},
        }
    }
    false
}

/// Render an expression as structured physical lines.
fn render_pretty_lines(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
    source_text: &str,
) -> Vec<PrettyLine> {
    // Special case: `.within` chain — detect by scanning the text.
    if let Some(within_pos) = find_within_split(source_text) {
        // When the `.within` is one arm of a multi-arm choice, render
        // every arm separately so the sibling arms survive (`A .within
        // B / C / D` keeps C and D).
        if let WrappedNode::Syntax { rule, children, .. } = rhs_node
            && rule == "type"
        {
            let type1s: Vec<&WrappedNode> = children
                .iter()
                .filter(|c| syntax_rule(c) == Some("type1"))
                .collect();
            if type1s.len() > 1 {
                let mut lines = Vec::new();
                let mut arms = type1s.iter().peekable();
                while let Some(t1) = arms.next() {
                    let mut arm_lines = render_pretty_lines(cx, t1, text_of(t1));
                    if arms.peek().is_some() {
                        append_choice_separator(&mut arm_lines);
                    }
                    lines.extend(arm_lines);
                }
                return lines;
            }
        }
        // AST-driven split: rhs_node (a `type`) contains a single `type1`
        // whose children are `type2` (LHS), `ctlop` (operator), `type2` (RHS).
        // Rendering each side via the AST routes `{...}` and `[...]` bodies
        // through `render_brace_block`, which expands socket plugs at use
        // sites via `render_grpent`. This is the only way the LHS of a
        // `.within` expression gets the same inlining treatment as a
        // stand-alone brace/bracket body.
        let (lhs_node, rhs_node_ast) = extract_within_operands(rhs_node);
        // Render the LHS via AST so brace/bracket bodies get grpent expansion.
        if let Some(lhs) = lhs_node {
            let previous_within_lhs = cx.within_lhs;
            cx.within_lhs = true;
            let mut lhs_lines = render_pretty_lines(cx, lhs, text_of(lhs));
            cx.within_lhs = previous_within_lhs;
            // A `.within` LHS that renders as a top-level choice or a
            // ctlop-bearing operand must be parenthesized: without the
            // parens the operator binds the last arm/operand only
            // (`A / B .within R`; `bstr .dtrm {...} .within ...`
            // chains two ctlops on one type1 and fails the grammar).
            wrap_within_lhs(&mut lhs_lines);
            let rhs_lines = if let Some(rn) = rhs_node_ast {
                // The `.within` RHS is a ctlop operand; a named operand
                // whose definition carries a ctlop stays symbolic
                // (inlining it would chain two ctlops per type1).
                if let Some(name) = arm_is_bare_name(rn)
                    && cx.name_resolves_to_ctlop_expression(&name)
                {
                    cx.record_symbolic_ref(&name);
                    vec![line(0, name.clone())]
                } else {
                    render_pretty_lines(cx, rn, text_of(rn))
                }
            } else {
                // Same byte-safety argument as the LHS fallback above.
                let (_, rhs_src) = source_text.split_at(within_pos);
                vec![line(
                    0,
                    rhs_src.trim_start_matches(".within").trim().to_owned(),
                )]
            };
            if let Some(first_rhs) = rhs_lines.first() {
                if lhs_lines.len() == 1 && rhs_lines.len() == 1 {
                    lhs_lines.push(line(0, ".within"));
                    lhs_lines.push(first_rhs.clone());
                    return lhs_lines;
                }
                return join_control_lines(lhs_lines.as_mut_slice(), rhs_lines, ".within")
                    .unwrap_or(lhs_lines);
            }
            return lhs_lines;
        }
        // Fallback: emit LHS as raw text. `within_pos` is a
        // verified ASCII byte index from `find_within_split`,
        // so `split_at` is safe here.
        let (lhs_src, _) = source_text.split_at(within_pos);
        let (_, rhs_src) = source_text.split_at(within_pos);
        return vec![line(
            0,
            format!(
                "{} .within {}",
                lhs_src.trim(),
                rhs_src.trim_start_matches(".within").trim()
            ),
        )];
    }
    let rule = syntax_rule(rhs_node);
    match rule.unwrap_or("") {
        "type" => render_type_lines(cx, rhs_node, source_text),
        "type1" | "type2" | "grpent" | "group" | "grpchoice" => {
            // Single type1 or a brace/bracket body. If the source text
            // starts with `{` or `[`, lay it out as a block.
            let trimmed = source_text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                render_brace_block_lines(cx, rhs_node)
            } else if let Some(lines) = render_structured_non_block(cx, rhs_node, source_text) {
                lines
            } else {
                let (rendered, _) = cx.render_with_inlining(rhs_node, &mut None);
                let mut text = rendered;
                if cx.policy.provenance_comments {
                    annotate_inline_refs(cx, &mut text, rhs_node);
                }
                rendered_text_to_lines(text)
            }
        },
        _ => render_default_pretty_lines(cx, rhs_node),
    }
}

/// Render a `type` node, preserving real choice separators.
fn render_type_lines(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
    source_text: &str,
) -> Vec<PrettyLine> {
    let type1s: Vec<&WrappedNode> = if let WrappedNode::Syntax { children, .. } = rhs_node {
        children
            .iter()
            .filter(|c| syntax_rule(c) == Some("type1"))
            .collect()
    } else {
        Vec::new()
    };
    if let [only_t1] = type1s.as_slice() {
        let t1_text = text_of(only_t1).trim();
        if t1_text.starts_with('{') || t1_text.starts_with('[') {
            return render_brace_block_lines(cx, only_t1);
        }
    }
    if type1s.is_empty() {
        let (rendered, _) = cx.render_with_inlining(rhs_node, &mut None);
        return rendered_text_to_lines(rendered);
    }
    if let [only_t1] = type1s.as_slice() {
        let rendered_lines = render_pretty_lines(cx, only_t1, text_of(only_t1));
        if rendered_lines.is_empty() {
            let (rendered, _) = cx.render_with_inlining(only_t1, &mut None);
            return rendered_text_to_lines(rendered);
        }
        return rendered_lines;
    }
    render_type_choice_arms(cx, &type1s, has_top_level_choice_separator(source_text))
}

/// Render each `type1` arm in a `type` choice.
fn render_type_choice_arms(
    cx: &mut RenderCx<'_>,
    type1s: &[&WrappedNode],
    is_choice: bool,
) -> Vec<PrettyLine> {
    let mut lines = Vec::new();
    // Elide a provably bare self-arm (`x = x / int`): the arm names the
    // definition being rendered, contributes no structure, and the
    // classification confirmed there are other arms to keep.
    let arms: Vec<&WrappedNode> = type1s
        .iter()
        .filter(|t1| !should_elide_self_arm(cx, t1))
        .copied()
        .collect();
    let multi_arm = arms.len() > 1;
    let mut arms = arms.iter().peekable();
    while let Some(t1) = arms.next() {
        let previous_choice_arm = cx.choice_arm;
        cx.choice_arm = true;
        let mut arm_lines = render_pretty_lines(cx, t1, text_of(t1));
        cx.choice_arm = previous_choice_arm;
        // A ctlop/rangeop expression used as a choice arm must be
        // parenthesized (`(text .regexp "x") / "y"`): ctlops have no
        // order of evaluation, so the operand scope must be explicit.
        // The check runs on the rendered arm so an inlined named
        // reference that renders as a ctlop expression braces
        // identically to a literal.
        let arm_flat = arm_lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let arm_bare =
            !arm_flat.trim().is_empty() && arm_flat.trim().chars().all(is_reference_name_char);
        if multi_arm
            && !arm_lines.is_empty()
            && !arm_bare
            && (cx.arm_renders_ctlop_expression(t1) || has_top_level_ctlop(&arm_flat))
        {
            if let Some(first) = arm_lines.first_mut() {
                first.text = format!("({}", first.text);
            }
            if let Some(last) = arm_lines.last_mut() {
                last.text.push(')');
            }
        }
        if is_choice && arms.peek().is_some() {
            append_choice_separator(&mut arm_lines);
        }
        lines.extend(arm_lines);
    }
    lines
}

/// Return true when a choice arm is the definition's own provably bare
/// self-reference and the definition was classified as elidable.
fn should_elide_self_arm(
    cx: &RenderCx<'_>,
    arm: &WrappedNode,
) -> bool {
    let Some(current) = cx.current_def.as_deref() else {
        return false;
    };
    if !cx.resolution.is_elidable_self(current) {
        return false;
    }
    arm_is_bare_name(arm).as_deref() == Some(current)
}

/// Render fallback syntax, with special handling for parenthesized choices.
fn render_default_pretty_lines(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
) -> Vec<PrettyLine> {
    if let Some(lines) = render_parenthesized_choice_lines(cx, rhs_node) {
        return lines;
    }
    let (rendered, _) = cx.render_with_inlining(rhs_node, &mut None);
    rendered_text_to_lines(rendered)
}

/// Render `(a / b / c)` as a multi-line parenthesized choice.
fn render_parenthesized_choice_lines(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
) -> Option<Vec<PrettyLine>> {
    let WrappedNode::Syntax { children, text, .. } = rhs_node else {
        return None;
    };
    let trimmed = text.trim();
    if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
        return None;
    }
    let Some(WrappedNode::Syntax {
        children: type_kids,
        ..
    }) = children.iter().find(|c| syntax_rule(c) == Some("type"))
    else {
        return None;
    };
    let type1s: Vec<&WrappedNode> = type_kids
        .iter()
        .filter(|c| syntax_rule(c) == Some("type1"))
        .collect();
    if type1s.len() <= 1 {
        return None;
    }
    Some(render_wrapped_choice_lines(cx, &type1s))
}

/// Wrap rendered choice arms in surrounding parentheses.
fn render_wrapped_choice_lines(
    cx: &mut RenderCx<'_>,
    type1s: &[&WrappedNode],
) -> Vec<PrettyLine> {
    let previous = cx.force_inline;
    cx.force_inline = previous || choice_contains_socket_arm(cx, type1s);
    let mut lines = vec![line(0, "(")];
    let mut arms = type1s.iter().peekable();
    while let Some(t1) = arms.next() {
        let mut arm_lines = render_pretty_lines(cx, t1, text_of(t1));
        if arms.peek().is_some() {
            append_choice_separator(&mut arm_lines);
        }
        for mut rendered_line in arm_lines {
            rendered_line.indent = rendered_line.indent.saturating_add(1);
            lines.push(rendered_line);
        }
    }
    lines.push(line(0, ")"));
    cx.force_inline = previous;
    lines
}

/// Return whether a parenthesized choice contains a named body supplied by a
/// group-socket augmentation. Ordinary choices retain their existing render
/// policy and are not forced through socket expansion.
fn choice_contains_socket_arm(
    cx: &RenderCx<'_>,
    type1s: &[&WrappedNode],
) -> bool {
    let socket_names: HashSet<String> = cx
        .resolution
        .socket_plugs
        .values()
        .flatten()
        .filter_map(|plug| {
            let WrappedNode::RuleLine { children, .. } = plug else {
                return None;
            };
            find_rhs(children).and_then(bare_type_or_group_name)
        })
        .collect();
    if socket_names.is_empty() {
        return false;
    }
    type1s.iter().any(|arm| {
        collect_bare_typenames(arm)
            .iter()
            .any(|name| socket_names.contains(name))
    })
}

/// Render non-bracket syntax forms that still need structured layout.
fn render_structured_non_block(
    cx: &mut RenderCx<'_>,
    rhs_node: &WrappedNode,
    source_text: &str,
) -> Option<Vec<PrettyLine>> {
    match syntax_rule(rhs_node) {
        Some("type1") => render_type1_lines(cx, rhs_node),
        Some("type2") => render_type2_lines(cx, rhs_node, source_text),
        Some("grpent") => render_grpent_lines(cx, rhs_node),
        _ => None,
    }
}

/// Render `type1` control/range operators with structured operands.
fn render_type1_lines(
    cx: &mut RenderCx<'_>,
    node: &WrappedNode,
) -> Option<Vec<PrettyLine>> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    let mut type2s: Vec<&WrappedNode> = Vec::new();
    let mut operator: Option<String> = None;
    for child in children {
        match syntax_rule(child) {
            Some("type2") => type2s.push(child),
            Some("rangeop") => operator = Some("..".to_owned()),
            Some("ctlop") => {
                let ctlop_text = text_of(child).trim();
                if let Some(name) = ctlop_text.split_whitespace().next()
                    && operator.is_none()
                {
                    operator = Some(name.to_owned());
                }
            },
            _ => {},
        }
    }
    let [left, right] = type2s.as_slice() else {
        if operator.is_none()
            && let Some(t2) = type2s.first()
            && let WrappedNode::Syntax { children, .. } = t2
            && leading_tag(children, text_of(t2)).is_some()
        {
            // A bare tagged `#6.37(...)` type1 must render through the
            // same structured path as an inlined definition, or the
            // output depends on whether the source was already
            // flattened. Delegate to `render_type2_lines` so tags wrap
            // their inner choice multi-line consistently.
            return render_type2_lines(cx, t2, text_of(t2));
        }
        return None;
    };
    let op = operator?;
    let previous_within_lhs = cx.within_lhs;
    if op == ".within" {
        cx.within_lhs = true;
    }
    let mut left_lines = render_pretty_lines(cx, left, text_of(left));
    if op == ".within" {
        cx.within_lhs = previous_within_lhs;
    }
    let right_lines = if op == ".det" {
        // `.det` operands are text literals that may span lines in the
        // source; a multi-line literal is not valid inside a tag's
        // parens, so flatten it to a single line with `\n` escapes
        // (the value is preserved). A named operand resolves like any
        // other reference (inlined, or symbolic and retained).
        if let Some(name) = arm_is_bare_name(right) {
            let (rendered, _p) = cx.render_named_reference(&name, &mut None, &mut HashSet::new());
            vec![line(0, flatten_multiline_string(&rendered))]
        } else {
            vec![line(0, flatten_multiline_string(text_of(right).trim()))]
        }
    } else if arm_is_bare_name(right).is_some() {
        // A ctlop operand is a type2 position; a definition whose RHS
        // is itself a ctlop expression must not be inlined here (the
        // grammar allows only one ctlop per type1).
        cx.ctlop_rhs = true;
        let rendered = render_pretty_lines(cx, right, text_of(right));
        cx.ctlop_rhs = false;
        rendered
    } else {
        render_pretty_lines(cx, right, text_of(right))
    };
    // A ctlop RHS that renders as a top-level choice must be
    // parenthesized (`bstr .bits (A / B)`): without the parens the `/`
    // escapes the operator on re-parse (`bstr .bits A / B` binds the
    // ctlop to `A` only).
    let right_lines = wrap_choice_operand_lines(right_lines);
    {
        // A ctlop LHS that renders as a top-level choice must be
        // parenthesized: the operator binds the whole operand, and
        // without the parens the operator text would attach to the last
        // choice arm (`restriction .default "private"`).
        let left_flat = left_lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if RenderCx::contains_top_level_choice(&left_flat) {
            if let Some(first) = left_lines.first_mut() {
                first.text = format!("({}", first.text);
            }
            if let Some(last) = left_lines.last_mut() {
                last.text.push(')');
            }
        }
    }
    join_control_lines(left_lines.as_mut_slice(), right_lines, &op)
}

/// Wrap rendered lines in parentheses when their flattened text is a
/// top-level choice (a ctlop operand must not let `/` escape it).
fn wrap_choice_operand_lines(mut lines: Vec<PrettyLine>) -> Vec<PrettyLine> {
    let flat = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if RenderCx::contains_top_level_choice(&flat)
        && !(flat.trim_start().starts_with('(') && flat.trim_end().ends_with(')'))
        && let Some(first) = lines.first_mut()
    {
        first.text = format!("({}", first.text);
        if let Some(last) = lines.last_mut() {
            last.text.push(')');
        }
    }
    lines
}

/// Parenthesize a `.within` LHS that renders as a top-level choice or
/// a ctlop-bearing operand (see the call site for the grammar reason).
fn wrap_within_lhs(lines: &mut [PrettyLine]) {
    let flat = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if RenderCx::contains_top_level_choice(&flat)
        || (has_top_level_ctlop(&flat)
            && !(flat.trim_start().starts_with('(') && flat.trim_end().ends_with(')')))
    {
        if let Some(first) = lines.first_mut() {
            first.text = format!("({}", first.text);
        }
        if let Some(last) = lines.last_mut() {
            last.text.push(')');
        }
    }
}

/// Render `type2` tags and parenthesized choices as structured lines.
fn render_type2_lines(
    cx: &mut RenderCx<'_>,
    node: &WrappedNode,
    source_text: &str,
) -> Option<Vec<PrettyLine>> {
    let WrappedNode::Syntax { children, text, .. } = node else {
        return None;
    };
    if let Some(tag) = leading_tag(children, text) {
        let inner = children.iter().find(|c| {
            matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2")
        })?;
        let tag = cx.resolve_tag_head(&tag, &mut None, &mut HashSet::new());
        let mut lines = render_pretty_lines(cx, inner, text_of(inner));
        if let Some(first) = lines.first_mut() {
            first.text = format!("{tag}({}", first.text);
        }
        nest_following_lines(&mut lines, 1);
        if let Some(last) = lines.last_mut() {
            last.text.push(')');
        }
        return Some(lines);
    }
    if source_text.trim().starts_with('(')
        && source_text.trim().ends_with(')')
        && let Some(WrappedNode::Syntax {
            children: type_kids,
            ..
        }) = children.iter().find(|c| syntax_rule(c) == Some("type"))
    {
        let type1s: Vec<&WrappedNode> = type_kids
            .iter()
            .filter(|c| syntax_rule(c) == Some("type1"))
            .collect();
        if type1s.len() > 1 {
            return Some(render_wrapped_choice_lines(cx, &type1s));
        }
    }
    None
}

/// Render group entries that expand to socket plug choices.
fn render_grpent_lines(
    cx: &mut RenderCx<'_>,
    node: &WrappedNode,
) -> Option<Vec<PrettyLine>> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    if let Some(plug_name) = RenderCx::find_socket_plug_name(children)
        && !plug_name.is_empty()
        && let Some(plugs) = cx.socket_plugs_for(&plug_name)
        && !plugs.is_empty()
    {
        return Some(render_plug_choice_lines(cx, &plugs));
    }
    None
}

/// Render `{ ... }` and `[ ... ]` bodies as nested blocks.
fn render_brace_block_lines(
    cx: &mut RenderCx<'_>,
    node: &WrappedNode,
) -> Vec<PrettyLine> {
    let mut out = Vec::new();
    let trimmed = text_of(node).trim();
    let (open, close) = if trimmed.starts_with('[') {
        ('[', ']')
    } else {
        ('{', '}')
    };
    out.push(line(0, open.to_string()));
    let grp = find_group_child(node);
    if let Some(g) = grp {
        let entries = collect_group_entries(g);
        let mut entries = entries.into_iter().peekable();
        while let Some(entry) = entries.next() {
            let mut entry_lines = render_group_entry_lines(cx, &entry);
            if entries.peek().is_some()
                && let Some(last) = entry_lines.last_mut()
            {
                if last.text.trim_end().ends_with(',') {
                    // Already comma-terminated.
                } else if let Some(idx) = last.text.find(" ; from") {
                    let (head, tail) = last.text.split_at(idx);
                    last.text = format!("{head},{tail}");
                } else {
                    last.text.push(',');
                }
            }
            for mut rendered_line in entry_lines {
                rendered_line.indent = rendered_line.indent.saturating_add(1);
                out.push(rendered_line);
            }
        }
    }
    out.push(line(0, close.to_string()));
    out
}

/// Render one collected group entry, expanding nested structure when needed.
fn render_group_entry_lines(
    cx: &mut RenderCx<'_>,
    entry: &WrappedNode,
) -> Vec<PrettyLine> {
    if let WrappedNode::Syntax { rule, .. } = entry
        && rule == "grpent"
        && find_group_child(entry).is_some()
    {
        let first_char = text_of(entry).trim().chars().next();
        // An entry that is a block followed by a top-level ctlop
        // (`[ ... ] .within [ ... ]`) is not a pure block; the
        // brace-block renderer would drop the ctlop continuation.
        let block_continues = find_within_split(text_of(entry).trim()).is_some();
        if matches!(first_char, Some('{' | '[')) && !block_continues {
            return render_brace_block_lines(cx, entry);
        }
    }
    if let Some(lines) = render_structured_non_block(cx, entry, text_of(entry))
        && !lines.is_empty()
    {
        return lines;
    }
    // BUG-005 follow-on: a `key: { ... }` entry whose value is a
    // brace or bracket body must render the value across multiple
    // indented lines instead of being collapsed to a single
    // one-liner.  The inline `render_grpent` path would otherwise
    // emit `key: {field1, field2, field3}` for arbitrarily long
    // nested maps/arrays, which defeats the multi-line renderer
    // contract from the `.within` diagnostic.
    if let Some(lines) = render_grpent_keyed_block(cx, entry) {
        return lines;
    }
    let rendered = if let WrappedNode::Syntax {
        rule, children: gc, ..
    } = entry
        && rule == "grpent"
    {
        let (r, _) = cx.render_grpent(entry, gc, &mut None, &mut HashSet::new());
        r
    } else {
        let (r, _) = cx.render_with_inlining(entry, &mut None);
        r
    };
    let mut text = rendered;
    if cx.policy.provenance_comments {
        annotate_inline_refs(cx, &mut text, entry);
    }
    if text.contains('\n') {
        split_rendered_lines(&text)
    } else {
        vec![line(0, text)]
    }
}

/// Render a `grpent` whose value is a brace or bracket body using
/// the multiline nested-block layout (the same layout the top-level
/// brace renderer uses), so a `key: { ... }` entry with many fields
/// expands each field onto its own indented line instead of being
/// collapsed to a single one-liner.
///
/// Returns `None` when the entry is not a keyed brace/bracket body
/// so the caller falls through to the inline renderer.
fn render_grpent_keyed_block(
    cx: &mut RenderCx<'_>,
    entry: &WrappedNode,
) -> Option<Vec<PrettyLine>> {
    let WrappedNode::Syntax { children, .. } = entry else {
        return None;
    };
    if syntax_rule(entry) != Some("grpent") {
        return None;
    }
    // Walk children for: memberkey, occur, and the brace/bracket
    // value type.  We need at least one memberkey and a value
    // type whose text starts with `{` or `[`.
    let mut memberkey: Option<&WrappedNode> = None;
    let mut value: Option<&WrappedNode> = None;
    let mut occur_text: Option<String> = None;
    for child in children {
        match syntax_rule(child) {
            Some("memberkey") => memberkey = Some(child),
            Some("type" | "type1") => {
                if value.is_none() {
                    value = Some(child);
                }
            },
            Some("occur") => occur_text = Some(text_of(child).trim().to_owned()),
            _ => {},
        }
    }
    let (mk, val) = (memberkey?, value?);
    // Render the value via the inline path first so we see the
    // post-inlining text (`Headers` -> `{1: 57, 4: bstr .size 32,
    // ...}`).  The raw node text still says `Headers` and would
    // not match a brace/bracket prefix check.
    let (rendered_val, _) = cx.render_with_inlining(val, &mut None);
    let mut rendered_val = rendered_val.trim().to_owned();
    if rendered_val.lines().count() > 1 {
        // Nested content reaches here either pretty-printed (from an
        // inlined definition) or via the single-line string path (a
        // literal value). Flatten so both produce identical output and
        // the round-trip is byte-stable.
        rendered_val = rendered_val
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        rendered_val = normalize_block_spacing(&rendered_val);
        rendered_val = dedent_paren_spacing(&rendered_val);
    }
    if !(rendered_val.starts_with('[') || rendered_val.starts_with('{')) {
        return None;
    }
    // The value must be a single block: its matching close delimiter
    // must be the final character. A choice whose first arm is a block
    // (`{ ... } / text`) must NOT go through the keyed-block splitter,
    // which would slice the trailing arm (`text` -> `tex`) and emit a
    // stray close delimiter. Such values fall through to the general
    // member rendering, which wraps the choice in parentheses.
    let close = if rendered_val.starts_with('[') {
        ']'
    } else {
        '}'
    };
    if rendered_val.chars().rev().find(|c| !c.is_whitespace()) != Some(close) {
        return None;
    }
    let mk_text = render_memberkey_text(cx, mk);
    let prefix = occur_text
        .as_deref()
        .filter(|s| !s.is_empty())
        .map_or(String::new(), |s| format!("{s} "));
    // Render the inlined value as a multi-line brace/bracket block.
    // The AST recursion (`render_pretty_lines(cx, val, ...)`) would
    // re-walk the un-inlined `Body` typename and emit a single line,
    // so we render the inlined text directly via a small helper
    // that splits the body on top-level commas and emits each entry
    // on its own indented line.  Group entries that themselves
    // contain a brace block are indented one extra level.
    let mut value_lines = render_inlined_brace_lines(&rendered_val);
    if value_lines.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(value_lines.len().saturating_add(1));
    if let Some(first) = value_lines.first_mut() {
        first.text = format!("{prefix}{mk_text} {}", first.text);
    }
    out.extend(value_lines);
    Some(out)
}

/// Normalize spacing around `{`, `}`, `[`, `]` in a flattened
/// single-line block body so pretty-printed and string-path renderings
/// agree (`[+ {` -> `[ + {`, `}]` -> `} ]`, `[]` -> `[ ]`). Quoted
/// literals (`"..."` and `'...'`) are skipped.
fn normalize_block_spacing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    let mut prev: Option<char> = None;
    while let Some(c) = chars.next() {
        if escaped {
            escaped = false;
            out.push(c);
            prev = Some(c);
            continue;
        }
        if in_double {
            out.push(c);
            if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_double = false;
            }
            prev = Some(c);
            continue;
        }
        if in_single {
            out.push(c);
            if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                in_single = false;
            }
            prev = Some(c);
            continue;
        }
        match c {
            '"' => {
                in_double = true;
                out.push(c);
            },
            '\'' => {
                in_single = true;
                out.push(c);
            },
            '[' | '{' => {
                out.push(c);
                match chars.peek() {
                    // Empty block: `[]` / `{}` render as `[ ]` / `{ }`.
                    Some(']' | '}') => out.push(' '),
                    // Occurrence arrays (`[+ x]`) keep the compact form.
                    Some('+') => {},
                    Some(&n) if n != ' ' && n != '\t' => out.push(' '),
                    _ => {},
                }
            },
            ']' | '}' => {
                if let Some(p) = prev
                    && p != ' '
                    && p != '\t'
                    && p != '['
                    && p != '{'
                {
                    out.push(' ');
                }
                out.push(c);
            },
            _ => out.push(c),
        }
        prev = Some(c);
    }
    out
}

/// Remove spaces immediately inside parentheses. The flatten join
/// produces `( a, b )` from a multi-line parenthesized group, while the
/// literal form is `(a, b)`; dropping the paren-adjacent spaces keeps
/// both renderings byte-identical.
fn dedent_paren_spacing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    let mut prev: Option<char> = None;
    while let Some(c) = chars.next() {
        if escaped {
            escaped = false;
            out.push(c);
            prev = Some(c);
            continue;
        }
        if in_double {
            out.push(c);
            if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_double = false;
            }
            prev = Some(c);
            continue;
        }
        if in_single {
            out.push(c);
            if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                in_single = false;
            }
            prev = Some(c);
            continue;
        }
        match c {
            '"' => {
                in_double = true;
                out.push(c);
            },
            '\'' => {
                in_single = true;
                out.push(c);
            },
            ' ' if prev == Some('(') || chars.peek() == Some(&')') => {},
            _ => out.push(c),
        }
        prev = Some(c);
    }
    out
}

/// Render an inlined `{...}` or `[...]` body whose AST node
/// representation no longer matches its post-inlining text.  The
/// top-level body is split on commas (respecting nested braces,
/// brackets, and parens), and each entry is emitted on its own
/// line indented one level past the `key {` header.  A trailing
/// close delimiter is emitted last.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "byte offsets walked manually; UTF-8 boundaries checked via char_indices"
)]
fn render_inlined_brace_lines(text: &str) -> Vec<PrettyLine> {
    let trimmed = text.trim();
    let (open, close) = if trimmed.starts_with('[') {
        ('[', ']')
    } else if trimmed.starts_with('{') {
        ('{', '}')
    } else {
        return Vec::new();
    };
    let inner = trimmed
        .get(1..trimmed.len().saturating_sub(1))
        .unwrap_or("");
    let entries = split_top_level_commas(inner);
    let mut out = vec![line(0, open.to_string())];
    let count = entries.len();
    for (i, entry) in entries.into_iter().enumerate() {
        let is_last = i + 1 == count;
        let mut entry_line = entry.trim().to_owned();
        if !is_last && !entry_line.ends_with(',') {
            entry_line.push(',');
        }
        out.push(line(1, entry_line));
    }
    out.push(line(0, close.to_string()));
    out
}

/// Split a body string on top-level commas, respecting nested
/// braces, brackets, parens, and CDDL generic-argument angle
/// brackets (so `Wrapper<A, B>` is kept on one line).
#[allow(
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    reason = "depth counters advance by 1 per char, cannot overflow in practice; `i` is a char boundary from char_indices"
)]
fn split_top_level_commas(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth_brace = 0_i32;
    let mut depth_bracket = 0_i32;
    let mut depth_paren = 0_i32;
    let mut depth_angle = 0_i32;
    let mut in_str = false;
    let mut escape = false;
    let mut start = 0_usize;
    for (i, ch) in body.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '<' => depth_angle += 1,
            // The `>` of the `=>` member operator must not be treated
            // as a generic-angle close; it would drive the angle depth
            // negative and suppress every later top-level comma split.
            '>' if !body[..i].ends_with('=') => depth_angle -= 1,
            ',' if depth_brace == 0
                && depth_bracket == 0
                && depth_paren == 0
                && depth_angle == 0 =>
            {
                let chunk = body.get(start..i).unwrap_or("");
                out.push(chunk.to_owned());
                start = i + 1_usize;
            },
            _ => {},
        }
    }
    let tail = body.get(start..).unwrap_or("").trim();
    if !tail.is_empty() {
        out.push(tail.to_owned());
    }
    out
}

/// Render a `memberkey` node's text, including any group
/// occurrence markers from its sub-syntax.  Falls back to the
/// trimmed source text for plain identifiers.
fn render_memberkey_text(
    cx: &mut RenderCx<'_>,
    mk: &WrappedNode,
) -> String {
    // Fold the key side (e.g. `iodef-Incident` -> `-19`) the same way
    // the inline member rendering does. `render_memberkey` includes the
    // trailing operator text (`=>` / `:`).
    if let Some((rendered, _)) = cx.render_memberkey(mk, &mut None, &mut HashSet::new()) {
        return rendered.trim().to_owned();
    }
    let (rendered, _) = cx.render_with_inlining(mk, &mut None);
    let trimmed = rendered.trim().to_owned();
    if trimmed.is_empty() {
        text_of(mk).trim().to_owned()
    } else {
        trimmed
    }
}

/// Render all socket plug arms as a parenthesized choice.
fn render_plug_choice_lines(
    cx: &mut RenderCx<'_>,
    plugs: &[WrappedNode],
) -> Vec<PrettyLine> {
    let mut lines = vec![line(0, "(")];
    let mut members: Vec<PrettyLine> = Vec::new();
    let mut all_members = true;
    let mut plugs = plugs.iter().peekable();
    while let Some(plug) = plugs.next() {
        let rendered = render_plug_arm(cx, plug);
        let is_member = rendered.trim_start().starts_with('(');
        if !is_member {
            all_members = false;
        }
        members.push(line(1, rendered));
        if plugs.peek().is_none() {
            break;
        }
    }
    let sep = if all_members { ", " } else { " /" };
    let last = members.len().saturating_sub(1);
    for (i, mut rendered_line) in members.into_iter().enumerate() {
        if i != last {
            rendered_line.text.push_str(sep);
        }
        lines.push(rendered_line);
    }
    lines.push(line(0, ")"));
    lines
}

/// Render one socket plug augmentation body.
fn render_plug_arm(
    cx: &mut RenderCx<'_>,
    plug: &WrappedNode,
) -> String {
    let WrappedNode::RuleLine { children, .. } = plug else {
        return String::new();
    };
    let Some(rhs) = find_rhs(children) else {
        return String::new();
    };
    let previous = cx.force_inline;
    cx.force_inline = true;
    if let Some(inner) = find_parenthesized_grpent(rhs)
        && let WrappedNode::Syntax { children: gc, .. } = inner
    {
        let (rendered, _) = cx.render_grpent(inner, gc, &mut None, &mut HashSet::new());
        cx.force_inline = previous;
        return format!("({rendered})");
    }
    if let Some(name) = bare_type_or_group_name(rhs) {
        let rendered = cx
            .render_named_reference(&name, &mut None, &mut HashSet::new())
            .0;
        cx.force_inline = previous;
        return rendered;
    }
    let rendered = cx.render_with_inlining(rhs, &mut None).0;
    cx.force_inline = previous;
    rendered
}

/// Find the innermost parenthesized group entry inside a plug arm.
fn find_parenthesized_grpent(node: &WrappedNode) -> Option<&WrappedNode> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    for child in children {
        if syntax_rule(child) == Some("grpent") {
            return Some(child);
        }
        if let Some(found) = find_parenthesized_grpent(child) {
            return Some(found);
        }
    }
    None
}

/// Locate the first top-level `.within` in a source string, returning
/// the byte position of the `.within` keyword. `None` if not found.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "Manual byte walk is bounded by the loop predicate and UTF-8 safety"
)]
fn find_within_split(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let needle = b".within";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Must not be inside `[...]` or `{...}` or `(...)`.
            let mut depth_sq = 0_i32;
            let mut depth_cu = 0_i32;
            let mut depth_pa = 0_i32;
            for c in text[..i].chars() {
                match c {
                    '[' => depth_sq += 1,
                    ']' => depth_sq -= 1,
                    '{' => depth_cu += 1,
                    '}' => depth_cu -= 1,
                    '(' => depth_pa += 1,
                    ')' => depth_pa -= 1,
                    _ => {},
                }
            }
            if depth_sq == 0 && depth_cu == 0 && depth_pa == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Extract the LHS and RHS operand `type2` nodes from the AST
/// shape produced for `LHS .within RHS` expressions. The
/// grammar produces a `type` node containing a single
/// `type1`, whose children are `type2`, `ctlop`, `type2`. We
/// return the first two `type2`-shaped children so the
/// pretty-printer can recurse into each side through the AST
/// (which routes `{...}` and `[...]` bodies through
/// `render_brace_block` and expands socket plugs at use sites).
/// Returns `(None, None)` if the AST shape doesn't match.
fn extract_within_operands(rhs_node: &WrappedNode) -> (Option<&WrappedNode>, Option<&WrappedNode>) {
    let WrappedNode::Syntax { children, .. } = rhs_node else {
        return (None, None);
    };
    for c in children {
        if let WrappedNode::Syntax {
            rule, children: tc, ..
        } = c
            && rule == "type1"
        {
            let mut ts = tc.iter().filter(|x| {
                matches!(
                    syntax_rule(x),
                    Some("type2" | "type" | "type1" | "group" | "grpchoice")
                )
            });
            return (ts.next(), ts.next());
        }
    }
    (None, None)
}

/// Find the first `group` child of a node (used to walk into a
/// `{ ... }` or `[ ... ]` body).
fn find_group_child(node: &WrappedNode) -> Option<&WrappedNode> {
    if let WrappedNode::Syntax { children, rule, .. } = node {
        for c in children {
            if syntax_rule(c) == Some("group") {
                return Some(c);
            }
            if matches!(rule.as_str(), "type1" | "type2" | "type" | "grpent")
                && matches!(syntax_rule(c), Some("type1" | "type2" | "type" | "group"))
                && let Some(found) = find_group_child(c)
            {
                return Some(found);
            }
        }
    }
    None
}

/// Collect grpent entries out of a `group`/`grpchoice` subtree, in
/// order. `optcom` (comment) nodes are dropped — we don't try to
/// preserve source comments here.
#[allow(
    clippy::items_after_statements,
    reason = "Local walker only used from collect_group_entries"
)]
fn collect_group_entries(group: &WrappedNode) -> Vec<WrappedNode> {
    let mut out = Vec::new();
    walk_for_grpents(group, &mut out);
    out
}

/// Inner walker for [`collect_group_entries`]. Recursively collects
/// grpent nodes from a `group` or `grpchoice` subtree.
fn walk_for_grpents(
    node: &WrappedNode,
    out: &mut Vec<WrappedNode>,
) {
    if let WrappedNode::Syntax { children, .. } = node {
        for c in children {
            if let Some(r) = syntax_rule(c) {
                if r == "grpent" {
                    out.push(c.clone());
                } else if r == "group" || r == "grpchoice" {
                    walk_for_grpents(c, out);
                }
            }
        }
    }
}

/// Scan a rendered string for an inlined typename reference; if found
/// and the name has a definition we are emitting elsewhere, append a
/// `; from <name>` tail-comment. Mutates `text` in place.
fn annotate_inline_refs(
    cx: &mut RenderCx<'_>,
    text: &mut String,
    node: &WrappedNode,
) {
    let defs = collect_bare_typenames(node);
    for name in defs {
        if cx.resolution.definitions.contains_key(&name)
            && !name.starts_with('"')
            && !is_primitive_keyword(&name)
        {
            let _ = write!(text, " ; from {name}");
            break; // one comment is enough to keep it human-readable
        }
    }
}

/// Walk a node tree and collect bare `typename` / `groupname` child
/// names. Used to add provenance comments to inlined references.
#[allow(
    clippy::items_after_statements,
    reason = "Local walker only used from collect_bare_typenames"
)]
fn collect_bare_typenames(node: &WrappedNode) -> Vec<String> {
    let mut out = Vec::new();
    walk_for_typenames(node, &mut out);
    out
}

/// Inner walker for [`collect_bare_typenames`]. Recursively collects
/// `typename` and `groupname` child node texts.
fn walk_for_typenames(
    node: &WrappedNode,
    out: &mut Vec<String>,
) {
    if let WrappedNode::Syntax { children, .. } = node {
        for c in children {
            if let WrappedNode::Syntax { rule, text, .. } = c {
                if rule == "typename" || rule == "groupname" {
                    let n = text.trim().to_owned();
                    if !n.is_empty() {
                        out.push(n);
                    }
                }
                walk_for_typenames(c, out);
            }
        }
    }
}

/// CDDL primitive keywords that we don't want to annotate as
/// "from <keyword>".
fn is_primitive_keyword(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "uint"
            | "nint"
            | "integer"
            | "float"
            | "text"
            | "tstr"
            | "bytes"
            | "bstr"
            | "bool"
            | "null"
            | "any"
            | "true"
            | "false"
    )
}

// ---------------------------------------------------------------------------
// Sub-tree rendering with inlining
// ---------------------------------------------------------------------------

/// Threaded recursion context. Bundles the resolution map, the policy,
/// and the cycle-tracking set into a single parameter so the recursion
/// lints see one `cx` parameter that's used at every level rather
/// than several parameters each only used in recursive calls.
#[allow(
    clippy::struct_excessive_bools,
    reason = "Render-context mode flags (ctlop operand, choice arm, within LHS, elision, socket expansion)."
)]
struct RenderCx<'a> {
    /// Resolution state (definitions, plug sources, cache, exports).
    resolution: &'a ResolutionMap,
    /// Active render policy.
    policy: &'a ConcretePolicy,
    /// Session-level set of names currently being expanded through
    /// inlining.  Acts as a safety net for recursive group
    /// references that enter the inliner through a fresh local
    /// `visited` set (e.g. via the pretty-printer's brace-block
    /// walker).  Mirrors the local `visited` parameter of
    /// [`RenderCx::render_named_reference`].
    in_progress: HashSet<String>,
    /// Names emitted as bare (symbolic) references because they could
    /// not be inlined (cycles, strong definitions, unresolved names).
    /// Top-level emission retains the definitions for these names so
    /// the rendered document is self-contained.
    symbolic_refs: HashSet<String>,
    /// `;@ CBORK:` directive comments already emitted (deduplicated:
    /// the same directive can appear on several nodes when a library
    /// imports another library carrying the same marker).
    emitted_directives: HashSet<String>,
    /// The definition whose RHS is currently being rendered, when any.
    /// Used to elide provably bare self-arms (`x = x / int`).
    current_def: Option<String>,
    /// True while rendering the RHS operand of a ctlop. A definition
    /// whose RHS already carries a ctlop/rangeop cannot be inlined into
    /// that operand: the grammar allows only one ctlop per type1, so
    /// the chained form would not parse.
    ctlop_rhs: bool,
    /// True while rendering a choice arm (a `type1` position). A
    /// definition whose RHS is a parenthesized group cannot be inlined
    /// there: the grammar's choice arms are types, not groups.
    choice_arm: bool,
    /// True while rendering the LHS operand of a `.within` expression.
    /// Type-plug LHS operands stay symbolic: inlining them changes how
    /// the within-checker evaluates the constraint (named operands are
    /// resolved differently from literal ones), so the rendered form
    /// must preserve the source's operand shape.
    within_lhs: bool,
    /// Expand strong definitions while recursively materializing a socket
    /// arm. Ordinary named references keep the normal concrete-render policy.
    force_inline: bool,
}

impl<'a> RenderCx<'a> {
    /// Bundle a resolution map and policy into a render context.
    fn new(
        resolution: &'a ResolutionMap,
        policy: &'a ConcretePolicy,
    ) -> Self {
        Self {
            resolution,
            policy,
            in_progress: HashSet::new(),
            symbolic_refs: HashSet::new(),
            emitted_directives: HashSet::new(),
            current_def: None,
            ctlop_rhs: false,
            choice_arm: false,
            within_lhs: false,
            force_inline: false,
        }
    }

    /// Record that `name` was emitted as a bare reference instead of
    /// being inlined, so the retention pass can keep its definition.
    fn record_symbolic_ref(
        &mut self,
        name: &str,
    ) {
        self.symbolic_refs.insert(name.to_owned());
    }

    /// Look up a socket's plug arms, falling back to the qualified
    /// suffix (`payload-or-evidence` → `cst.payload-or-evidence` for
    /// references inside imported generic bodies that the alias wrap
    /// leaves bare).
    fn socket_plugs_for(
        &self,
        name: &str,
    ) -> Option<Vec<WrappedNode>> {
        self.resolution.socket_plugs.get(name).cloned().or_else(|| {
            let suffix = format!(".{name}");
            self.resolution
                .socket_plugs
                .iter()
                .find(|(k, _)| k.ends_with(&suffix))
                .map(|(_, v)| v.clone())
        })
    }

    /// Look up a type plug's augment arms with the same qualified
    /// suffix fallback as [`RenderCx::socket_plugs_for`].
    fn type_plugs_for(
        &self,
        name: &str,
    ) -> Option<Vec<WrappedNode>> {
        self.resolution.type_plugs.get(name).cloned().or_else(|| {
            let suffix = format!(".{name}");
            self.resolution
                .type_plugs
                .iter()
                .find(|(k, _)| k.ends_with(&suffix))
                .map(|(_, v)| v.clone())
        })
    }

    /// Whether `name` resolves to a plug under its qualified suffix.
    fn qualified_plug(
        &self,
        name: &str,
    ) -> Option<String> {
        let suffix = format!(".{name}");
        self.resolution
            .socket_plugs
            .keys()
            .chain(self.resolution.type_plugs.keys())
            .find(|k| k.ends_with(&suffix))
            .cloned()
    }

    /// Resolve a `#6.<name>` tag argument. The argument is a type
    /// expression; a bare name goes through the reference machinery so
    /// it is folded to a literal or recorded for retention instead of
    /// being emitted verbatim (`#6.<TAG_FOR_THIS_SEMANTICS>` → E016).
    fn resolve_tag_head(
        &mut self,
        tag: &str,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> String {
        let Some(inner) = tag.strip_prefix("#6.<").and_then(|s| s.strip_suffix('>')) else {
            return tag.to_owned();
        };
        let trimmed = inner.trim();
        if !trimmed.is_empty() && trimmed.chars().all(is_reference_name_char) {
            let (rendered, _) = self.render_named_reference(trimmed, prov, visited);
            return format!("#6.<{rendered}>");
        }
        tag.to_owned()
    }

    /// Record that a recursive group reference was hit while inlining
    /// `name`.  The renderer cannot fully expand the cycle, so it
    /// leaves a stable placeholder behind; this method records the
    /// diagnostic so the caller can surface it to the user.
    fn record_cycle(
        &self,
        name: &str,
    ) {
        let origin = self.resolution.definitions.get(name).and_then(|n| {
            match n {
                WrappedNode::RuleLine { origin, .. } => Some(origin.clone()),
                _ => None,
            }
        });
        let mut diagnostics = self.resolution.render_diagnostics.borrow_mut();
        let already_reported = diagnostics
            .iter()
            .any(|d| d.code == "E030" && d.message.contains(&format!("`{name}`")));
        if already_reported {
            return;
        }
        let origin = origin.unwrap_or_else(|| {
            SourceOrigin {
                source_path: std::path::PathBuf::from("<render>"),
                line: 0,
                column: 0,
            }
        });
        diagnostics.push(Diagnostic {
            code: "E030",
            level: DiagnosticLevel::Warning,
            message: format!(
                "recursive group reference cycle through `{name}`: the renderer broke the cycle at this use site and emitted the bare name as a placeholder"
            ),
            source_file: Some(origin.source_path),
            span: None,
            previous_origin: None,
            related: Vec::new(),
        });
    }

    /// Render a single node, inlining typename references and folding
    /// primitive constants. Returns the rendered CDDL text and an
    /// optional provenance pair (folded name, folded value).
    fn render_with_inlining(
        &mut self,
        node: &WrappedNode,
        prov: &mut Option<(String, String)>,
    ) -> (String, Option<(String, String)>) {
        self.render_with_inlining_inner(node, prov, &mut HashSet::new())
    }

    /// Inner: render with a cycle-tracking set threaded through.
    fn render_with_inlining_inner(
        &mut self,
        node: &WrappedNode,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let WrappedNode::Syntax {
            rule,
            children,
            text,
            ..
        } = node
        else {
            return (text_of(node).trim().to_owned(), None);
        };

        match rule.as_str() {
            "expr" => self.render_expr(children, prov, visited),
            "group" | "grpchoice" => self.render_group(children, prov, visited),
            "type" => self.render_type_choice(children, prov, visited),
            "type1" => {
                self.render_type1(children, prov, visited)
                    .unwrap_or_default()
            },
            "type2" => self.render_type2(node, children, text, prov, visited),
            "grpent" => self.render_grpent(node, children, prov, visited),
            _ => (text.trim().to_owned(), None),
        }
    }

    /// Render the body of an `expr` rule. The `expr` children are:
    /// LHS, optional `genericparm`, `assignt`/`assigng`, RHS. We only
    /// render the RHS here (LHS is rendered by `render_define`).
    fn render_expr(
        &mut self,
        children: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let mut out = String::new();
        for child in children {
            let Some(rule) = syntax_rule(child) else {
                continue;
            };
            match rule {
                "typename" | "groupname" | "assignt" | "assigng" | "genericparm" => {},
                "type" | "type1" | "type2" | "grpent" => {
                    let (rendered, p) = self.render_with_inlining_inner(child, prov, visited);
                    if prov.is_none() {
                        *prov = p;
                    }
                    let _ = write!(&mut out, "{rendered}");
                },
                _ => {
                    let _ = write!(&mut out, "{}", text_of(child).trim());
                },
            }
        }
        (out, None)
    }

    /// Render a `type` (the choice-of-`type1` arm).
    fn render_type_choice(
        &mut self,
        children: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let type1s: Vec<&WrappedNode> = children
            .iter()
            .filter(|c| syntax_rule(c) == Some("type1"))
            .collect();
        let multi_arm = type1s.len() > 1;
        let parts: Vec<String> = type1s
            .iter()
            .map(|c| {
                let t1_children = match c {
                    WrappedNode::Syntax { children: cs, .. } => cs.as_slice(),
                    _ => &[],
                };
                let previous_choice_arm = self.choice_arm;
                self.choice_arm = true;
                let (mut r, p) = self
                    .render_type1(t1_children, prov, visited)
                    .unwrap_or_default();
                self.choice_arm = previous_choice_arm;
                // Parenthesize ctlop/rangeop arms (see the pretty path).
                // The check runs on the RENDERED arm so an inlined
                // named reference that renders as a ctlop expression
                // (`bstr .bits &(...)`) braces identically to a literal.
                if multi_arm && (self.arm_renders_ctlop_expression(c) || has_top_level_ctlop(&r)) {
                    r = format!("({r})");
                }
                if prov.is_none() {
                    *prov = p;
                }
                r
            })
            .collect();
        (parts.join(" / "), None)
    }

    /// Whether `name`'s definition (or a bare-name alias chain) has a
    /// ctlop/rangeop RHS, meaning it cannot be inlined into a ctlop
    /// operand position (the grammar allows one ctlop per type1).
    fn name_resolves_to_ctlop_expression(
        &self,
        name: &str,
    ) -> bool {
        let mut current = name.to_owned();
        let mut steps = 0_usize;
        while steps < 32 {
            steps = steps.saturating_add(1);
            let Some(def_node) = self.resolution.definitions.get(&current) else {
                return false;
            };
            let WrappedNode::RuleLine { children, .. } = def_node else {
                return false;
            };
            let Some(rhs) = find_rhs(children) else {
                return false;
            };
            if rhs_is_ctlop_expression(rhs) {
                return true;
            }
            let Some(next) = arm_is_bare_name(rhs) else {
                return false;
            };
            current = next;
        }
        false
    }

    /// Render a parenthesized ctlop expression (`(uint .le 16)`),
    /// keeping the parens unless the operator was dropped during
    /// rendering.
    fn render_parenthesized_ctlop(
        &mut self,
        inner: &WrappedNode,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let (inner_text, p) = self.render_with_inlining_inner(inner, prov, visited);
        if prov.is_none() {
            *prov = p;
        }
        let flat = inner_text.trim();
        // `.feature` flags render as their LHS only; when the rendered
        // inner carries no operator, the parens are redundant and are
        // dropped so the render of a literal `(uint .feature "x")`
        // matches the re-render of `(uint)`.
        if flat.contains('/') || flat.contains('\n') || flat.contains("..") || flat.contains(" .") {
            (format!("({inner_text})"), None)
        } else {
            (inner_text, None)
        }
    }

    /// Render a `type1` (may contain a ctlop, a rangeop, or be a
    /// simple `type2`).
    fn render_type1(
        &mut self,
        children: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> Option<(String, Option<(String, String)>)> {
        let mut type2s: Vec<&WrappedNode> = Vec::new();
        let mut operator: Option<String> = None;
        for child in children {
            match syntax_rule(child) {
                Some("type2") => type2s.push(child),
                Some("rangeop") => operator = Some("..".to_owned()),
                Some("ctlop") => {
                    let ctlop_text = text_of(child).trim();
                    if let Some(name) = ctlop_text.split_whitespace().next()
                        && operator.is_none()
                    {
                        operator = Some(name.to_owned());
                    }
                },
                _ => {},
            }
        }
        // A generic-argument substitution can nest a `type1` inside a
        // `type1` (`message<"sleep", 1..100>` substitutes the arg into
        // the reference's type1 position); tolerate it by recursing.
        if type2s.is_empty()
            && let Some(WrappedNode::Syntax { children: ic, .. }) =
                children.iter().find(|c| syntax_rule(c) == Some("type1"))
        {
            return self.render_type1(ic, prov, visited);
        }
        let first = *type2s.first()?;
        if let Some(op) = operator {
            let previous_within_lhs = self.within_lhs;
            if op == ".within" {
                self.within_lhs = true;
            }
            let (mut lo, _) = self.render_type2_inline(first, prov, visited);
            if op == ".within" {
                self.within_lhs = previous_within_lhs;
            }
            if op.starts_with(".feature") {
                return Some((lo, None));
            }
            if Self::contains_top_level_choice(&lo) {
                // Parenthesize a ctlop LHS that renders as a top-level
                // choice: the operator binds the whole operand, and
                // without the parens the operator text would attach to
                // the last choice arm (`restriction .default "private"`).
                lo = format!("({lo})");
            }
            let hi = type2s.get(1).copied().unwrap_or(first);
            let previous_ctlop_rhs = self.ctlop_rhs;
            if op != ".det" && arm_is_bare_name(hi).is_some() {
                self.ctlop_rhs = true;
            }
            let hi_text = if op == ".det" {
                if let Some(name) = arm_is_bare_name(hi) {
                    // A named `.det` operand resolves like any other
                    // reference (inlined, or kept symbolic and
                    // retained); the raw text would drop the definition.
                    // A definition whose RHS is itself a ctlop
                    // expression stays symbolic — inlining it would
                    // chain two ctlops on one type1 (`A .det B .det C`).
                    if self.name_resolves_to_ctlop_expression(&name) {
                        self.record_symbolic_ref(&name);
                        name
                    } else {
                        flatten_multiline_string(
                            &self.render_named_reference(&name, prov, visited).0,
                        )
                    }
                } else {
                    flatten_multiline_string(text_of(hi).trim())
                }
            } else {
                let mut rendered = self.render_type2_inline(hi, prov, visited).0;
                self.ctlop_rhs = previous_ctlop_rhs;
                // A ctlop RHS that renders as a top-level choice must be
                // parenthesized: the operator binds the whole operand
                // (`bstr .cbor (A / B)`), and without the parens the
                // `/` escapes the operator on re-parse (`bstr .cbor A /
                // B` binds the ctlop to `A` only) — and the render
                // would depend on whether the operand came from an
                // inlined definition or literal source.
                if Self::contains_top_level_choice(&rendered)
                    && !(rendered.starts_with('(') && rendered.ends_with(')'))
                {
                    rendered = format!("({rendered})");
                }
                rendered
            };
            return Some((format!("{lo} {op} {hi_text}"), None));
        }
        Some(self.render_type2_inline(first, prov, visited))
    }

    /// Render a `type2` (the leaf type level in CDDL's grammar).
    fn render_type2(
        &mut self,
        _node: &WrappedNode,
        children: &[WrappedNode],
        text: &str,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        if text.trim_start().starts_with("&(") {
            // Keep the enum verbatim (it may carry per-entry comments);
            // the pretty printer reformats it. Record any definition
            // names the verbatim text still references so the
            // retention pass keeps them.
            record_embedded_references(
                &mut self.symbolic_refs,
                text,
                &self.resolution.definitions,
                &self.resolution.type_plugs,
            );
            return (simplify_singleton_enum_key(text), None);
        }

        if let Some(tag) = leading_tag(children, text) {
            let inner = children.iter().find(|c| {
                matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type" || rule == "type1" || rule == "type2")
            });
            let inner_text = inner
                .map(|inner_node| self.render_with_inlining_inner(inner_node, prov, visited).0)
                .unwrap_or_default();
            // A `#6.<name>(...)` tag argument is a type expression;
            // resolve a bare name so the reference is folded or
            // recorded for retention (`#6.<TAG_FOR_THIS_SEMANTICS>`).
            let tag = self.resolve_tag_head(&tag, prov, visited);
            // `leading_tag` already includes the leading `#`, so do not add one here.
            return (format!("{tag}({inner_text})"), None);
        }

        if text.trim() == "#" {
            return ("#".to_owned(), None);
        }

        if text.trim_start().starts_with('~')
            && let Some(name) = first_type_or_group_name(children)
        {
            return self.render_named_reference(&name, prov, visited);
        }

        // Handle a type2 whose source text is a delimited group
        // (`(...)`, `{ ... }`, or `[ ... ]`) by rendering the inner
        // `group` child and re-wrapping it.
        if let Some(WrappedNode::Syntax { children: gc, .. }) =
            first_child_with_rule(children, "group")
        {
            let trimmed = text.trim();
            if trimmed.starts_with('(') || trimmed.starts_with('[') || trimmed.starts_with('{') {
                let (open, close) = if trimmed.starts_with('(') {
                    ('(', ')')
                } else if trimmed.starts_with('[') {
                    ('[', ']')
                } else {
                    ('{', '}')
                };
                let (inner_text, p) = self.render_group(gc, prov, visited);
                if prov.is_none() {
                    *prov = p;
                }
                // A single-line literal block must use the same bracket
                // spacing as the flattened form of an inlined block, or
                // the two renderings disagree (`{...}` vs `{ ... }`).
                let wrapped = format!("{open}{inner_text}{close}");
                return (
                    if inner_text.contains('\n') {
                        wrapped
                    } else {
                        dedent_paren_spacing(&normalize_block_spacing(&wrapped))
                    },
                    None,
                );
            }
        }

        // A parenthesized ctlop expression (`(uint .le 16)`) must keep
        // its parens: without them the ctlop chains with the outer
        // operand (`bytes .size uint .le 16`) and fails the grammar.
        if let Some(inner) = first_child_with_rule(children, "type")
            && let trimmed = text.trim()
            && trimmed.starts_with('(')
            && trimmed.ends_with(')')
            && rhs_is_ctlop_expression(inner)
        {
            return self.render_parenthesized_ctlop(inner, prov, visited);
        }

        if let Some(brackets) = detect_group_brackets(children) {
            return (brackets, None);
        }

        if let Some(inner) = first_child_with_rule(children, "type") {
            return self.render_with_inlining_inner(inner, prov, visited);
        }

        for child in children {
            let Some(child_rule) = syntax_rule(child) else {
                continue;
            };
            match child_rule {
                "typename" => {
                    let name = text_of(child).trim().to_owned();
                    return self.render_named_reference(&name, prov, visited);
                },
                "groupname" => {
                    let name = text_of(child).trim().to_owned();
                    if let Some(plugs) = self.resolution.socket_plugs.get(&name)
                        && !plugs.is_empty()
                    {
                        return self.render_plug_choice(plugs, prov, visited);
                    }
                    return self.render_named_reference(&name, prov, visited);
                },
                "value" => return (text_of(child).trim().to_owned(), None),
                _ => {},
            }
        }
        (text.trim().to_owned(), None)
    }

    /// Convenience: render a `type2` from a borrowed `&WrappedNode`.
    fn render_type2_inline(
        &mut self,
        node: &WrappedNode,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let WrappedNode::Syntax { children, text, .. } = node else {
            return (text_of(node).trim().to_owned(), None);
        };
        self.render_type2(node, children, text, prov, visited)
    }

    /// Look up `name` in the resolution map and inline its body if
    /// it's a structural type. Fold to a literal if it's a primitive
    /// constant. Recursion is depth-limited by the local `visited`
    /// parameter AND by the session-level `in_progress` set on the
    /// render context.  Both checks are necessary because some
    /// rendering paths (e.g. the brace-block pretty printer) reach
    /// the inliner through a fresh `visited` set and would otherwise
    /// allow recursive group references to stack-overflow.
    #[allow(
        clippy::too_many_lines,
        reason = "Resolves one reference through many guarded paths (plugs, cycles, recursion, ctlop/choice/within operand rules)"
    )]
    fn render_named_reference(
        &mut self,
        name: &str,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        // A ctlop RHS operand whose definition (or its alias chain)
        // already carries a ctlop/rangeop must stay symbolic: the
        // grammar allows only one ctlop per type1, so the inlined
        // chained form would not parse.
        if self.policy.target == TargetSide::Full && self.ctlop_rhs {
            let mut current = name.to_owned();
            // Follow bare-name aliases (`x = y`) up to a bounded chain.
            let mut steps = 0_usize;
            while steps < 32 {
                steps = steps.saturating_add(1);
                let Some(def_node) = self.resolution.definitions.get(&current) else {
                    break;
                };
                let WrappedNode::RuleLine { children, .. } = def_node else {
                    break;
                };
                let Some(rhs) = find_rhs(children) else {
                    break;
                };
                if rhs_is_ctlop_expression(rhs) {
                    self.record_symbolic_ref(name);
                    return (name.to_owned(), None);
                }
                let Some(next) = arm_is_bare_name(rhs) else {
                    break;
                };
                current = next;
            }
        }
        // A choice arm is a type position: a definition whose RHS is a
        // parenthesized group cannot be inlined there (the grammar's
        // choice arms are types, not groups).
        if self.policy.target == TargetSide::Full
            && self.choice_arm
            && let Some(def_node) = self.resolution.definitions.get(name)
            && let WrappedNode::RuleLine { children, .. } = def_node
            && let Some(rhs) = find_rhs(children)
            && matches!(syntax_rule(rhs), Some("grpent" | "group" | "grpchoice"))
        {
            self.record_symbolic_ref(name);
            return (name.to_owned(), None);
        }
        // A `.within` LHS operand that is a type plug stays symbolic:
        // inlining the plug's arms changes how the within-checker
        // evaluates the constraint (named operands are resolved
        // differently from literal ones), so the rendered form must
        // preserve the source's operand shape. The augment lines that
        // define the plug are emitted separately.
        if self.policy.target == TargetSide::Full
            && self.within_lhs
            && (self.resolution.type_plugs.contains_key(name)
                || self.resolution.socket_plugs.contains_key(name)
                || self.qualified_plug(name).is_some())
        {
            self.record_symbolic_ref(name);
            return (name.to_owned(), None);
        }
        if let Some(plugs) = self.socket_plugs_for(name)
            && !plugs.is_empty()
        {
            return self.render_plug_choice(&plugs, prov, visited);
        }

        if let Some(plugs) = self.type_plugs_for(name)
            && !plugs.is_empty()
        {
            return self.render_type_plug_choice(&plugs, prov, visited);
        }

        if name.starts_with('$') {
            return (name.to_owned(), None);
        }

        // Cycle-aware rendering: a symbol that participates in genuine
        // (guarded) recursion is never expanded. The reference stays
        // symbolic (the definition is retained by the top-level
        // emission pass), which is what keeps concrete output bounded
        // for self-referential types. Applies to the `cbork render`
        // path only; the effective `.within` diagnostic renderers keep
        // their own force-inline cycle handling.
        if self.policy.target == TargetSide::Full && self.resolution.is_recursive_symbol(name) {
            self.record_symbolic_ref(name);
            return (name.to_owned(), None);
        }

        if !visited.insert(name.to_owned()) {
            self.record_cycle(name);
            self.record_symbolic_ref(name);
            return (name.to_owned(), None);
        }
        if self.in_progress.contains(name) {
            self.record_cycle(name);
            self.record_symbolic_ref(name);
            return (name.to_owned(), None);
        }
        self.in_progress.insert(name.to_owned());

        if let Some(state) = self.resolution.resolve_constant(name)
            && let Some(literal) = constant_to_cddl(state)
        {
            // BUG-005: in effective mode, keep well-known base type
            // names readable (`bstr`, not its tag `#2`).  Constant
            // folding still applies to user-defined constants.
            if !(self.policy.effective_mode && is_effective_base_name(name)) {
                let provenance = prov.is_none().then(|| (name.to_owned(), literal.clone()));
                visited.remove(name);
                self.in_progress.remove(name);
                return (literal, provenance);
            }
        }

        let mut def_node = self.resolution.get(name);
        // BUG-005: in effective mode, when the bare name isn't
        // found, scan the resolution map for qualified matches
        // (e.g. `cose.Headers` for bare name `Headers`).  Imported
        // library rules are stored under their aliased names in the
        // consumer's tree, but the `.within` RHS references them
        // by their library-local bare names because it IS the
        // library's own source.
        let mut qualified_name = name.to_owned();
        if def_node.is_none() {
            let suffix = format!(".{name}");
            if let Some((qname, qnode)) = self
                .resolution
                .definitions
                .iter()
                .find(|(k, _)| k.ends_with(&suffix))
            {
                qualified_name = qname.clone();
                def_node = Some(qnode);
            }
        }

        if let Some(def_node) = def_node
            && let Some(rendered) = self.inline_definition(def_node, prov, visited)
        {
            visited.remove(name);
            self.in_progress.remove(name);
            return (rendered, None);
        }

        visited.remove(name);
        self.in_progress.remove(name);
        // When the reference resolved through its qualified suffix,
        // record and emit the qualified name so the retained definition
        // matches the reference in the output.
        self.record_symbolic_ref(&qualified_name);
        (qualified_name, None)
    }

    /// Inline a definition by rendering its RHS with the LHS's name
    /// already in the visited set. Returns `None` if the definition has
    /// no inlinable RHS (e.g. it's a strong `:=` definition).
    fn inline_definition(
        &mut self,
        def_node: &WrappedNode,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> Option<String> {
        let WrappedNode::RuleLine { children, .. } = def_node else {
            return None;
        };
        let head = rule_head_from_children(children)?;
        // BUG-005: effective mode inlines everything EXCEPT
        // well-known postlude primitives which should stay readable
        // (`bstr`, not `#2`).  Normal mode keeps strong definitions
        // symbolic in Full renders and keeps postlude primitives
        // readable.
        if !self.policy.effective_mode
            && !self.force_inline
            && head.assignment == AssignmentKind::Define
            && is_strong_definition(def_node)
            && self.policy.target == TargetSide::Full
        {
            return None;
        }
        // Postlude primitives stay symbolic even in effective mode
        // so `bstr` stays `bstr` rather than folding to `#2`.
        let in_postlude = def_node
            .metadata()
            .iter()
            .any(|m| matches!(m, crate::MetaData::StandardPostlude));
        if in_postlude
            && (self.policy.target == TargetSide::Full || is_effective_base_name(&head.name))
        {
            return None;
        }
        let rhs = find_rhs(children)?;
        // The inlined body is not itself a ctlop operand or `.within`
        // LHS operand: those flags apply only to the direct operand
        // reference. Clear them while rendering the body so names
        // nested inside it inline normally.
        let saved_ctlop_rhs = self.ctlop_rhs;
        let saved_within_lhs = self.within_lhs;
        self.ctlop_rhs = false;
        self.within_lhs = false;
        // BUG-005: effective mode routes inlined bodies through the
        // pretty-printer so nested maps, arrays, choice groups, and
        // ctlop chains render across multiple indented lines.  The
        // normal diff-side path returns `render_with_inlining_inner`
        // which is always single-line; effective mode must use the
        // same multiline pipeline as the `cbork render` subcommand.
        if self.policy.effective_mode {
            let text = text_of(rhs).trim().to_owned();
            let mut out = Concrete::new();
            let previous_def = self.current_def.replace(head.name.clone());
            render_pretty_rhs(self, rhs, 0, &text, &mut out);
            self.current_def = previous_def;
            let rendered = out.to_cddl();
            let rendered = rendered.trim_end_matches('\n').to_owned();
            if rendered.is_empty() {
                self.ctlop_rhs = saved_ctlop_rhs;
                self.within_lhs = saved_within_lhs;
                return Some(self.render_with_inlining_inner(rhs, prov, visited).0);
            }
            self.ctlop_rhs = saved_ctlop_rhs;
            self.within_lhs = saved_within_lhs;
            return Some(rendered);
        }
        if self.policy.target != TargetSide::Full {
            if text_of(rhs).trim().starts_with('(')
                && let WrappedNode::Syntax { children: rc, .. } = rhs
                && let Some(WrappedNode::Syntax { children: gc, .. }) =
                    first_child_with_rule(rc, "group")
            {
                let rendered = self.render_group(gc, prov, visited).0;
                self.ctlop_rhs = saved_ctlop_rhs;
                self.within_lhs = saved_within_lhs;
                return Some(format!("({rendered})"));
            }
            self.ctlop_rhs = saved_ctlop_rhs;
            self.within_lhs = saved_within_lhs;
            return Some(self.render_with_inlining_inner(rhs, prov, visited).0);
        }
        if text_of(rhs).contains(".size") {
            self.ctlop_rhs = saved_ctlop_rhs;
            self.within_lhs = saved_within_lhs;
            return Some(self.render_with_inlining_inner(rhs, prov, visited).0);
        }
        // Route the inlined body through the pretty-printer so ctlop
        // chains, parenthesized choices, and brace bodies inside the
        // inlined definition appear on their own lines.
        let text = text_of(rhs).trim().to_owned();
        let mut out = Concrete::new();
        let previous_def = self.current_def.replace(head.name.clone());
        render_pretty_rhs(self, rhs, 0, &text, &mut out);
        self.current_def = previous_def;
        let rendered = out.to_cddl();
        // Trim trailing newline `to_cddl` always appends.
        let rendered = rendered.trim_end_matches('\n').to_owned();
        self.ctlop_rhs = saved_ctlop_rhs;
        self.within_lhs = saved_within_lhs;
        if rendered.is_empty() {
            Some(self.render_with_inlining_inner(rhs, prov, visited).0)
        } else {
            Some(rendered)
        }
    }

    /// Render a `grpent` (parenthesized group element) node, inlining
    /// socket plugs and folding constants. Handles three shapes:
    ///
    /// * a single bare typename reference (e.g. `signature` inside `[signature, ...]`) ->
    ///   resolve it via the cache or by inlining the definition;
    /// * a `key => value` (or `key : value`) grpent -> recurse into each side so the key
    ///   and value are independently resolved (this is what folds `ed25519` to `-19` in
    ///   `{ ed25519 => bstr }`);
    /// * a parenthesized expression (e.g. `(foo => bar)`) -> just recurse into the inner
    ///   type.
    #[allow(
        clippy::too_many_lines,
        reason = "The renderer keeps distinct grpent shapes together for precedence clarity"
    )]
    fn render_grpent(
        &mut self,
        node: &WrappedNode,
        children: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        // Socket-plug check first: a grpent whose only meaningful
        // child names a socket (`one-pq-signature //= ...`) is
        // expanded to a `/`-joined choice of plug bodies.
        if let Some(plug_name) = Self::find_socket_plug_name(children)
            && !plug_name.is_empty()
            && let Some(plugs) = self.socket_plugs_for(&plug_name)
            && !plugs.is_empty()
        {
            return self.render_plug_choice(&plugs, prov, visited);
        }

        // A grpent that wraps a `group` child (`( ... )` shape)
        // must recurse into the group so that typename references
        // inside the parenthesized body are inlined.
        if let Some(WrappedNode::Syntax { children: gc, .. }) =
            first_child_with_rule(children, "group")
        {
            let previous = self.force_inline;
            self.force_inline = true;
            let (inner_text, p) = self.render_group(gc, prov, visited);
            self.force_inline = previous;
            if prov.is_none() {
                *prov = p;
            }
            let occurrence = children
                .iter()
                .find(|child| syntax_rule(child) == Some("occur"))
                .map(|child| text_of(child).trim())
                .filter(|text| !text.is_empty())
                .map_or(String::new(), |text| format!("{text} "));
            return (format!("{occurrence}({inner_text})"), None);
        }

        // Detect `key <ctlop> value` structure. The ctlop separates
        // two type-shaped operands; we recurse into each so constant
        // folding and structural inlining fire on both sides.
        //
        // The grpent can carry the operator in two different shapes:
        //
        // * a `ctlop` child (e.g. inside a `{ ... }` body the ctlop is its own sibling) -- the
        //   operator text is on the ctlop node;
        // * a `memberkey` child whose text already contains the `=>` or `:` -- the operator is
        //   part of the memberkey text and the next sibling is the value.
        //
        // We handle both.
        let mut type_children: Vec<&WrappedNode> = Vec::new();
        let mut ctlop_text: Option<String> = None;
        let mut memberkey_node: Option<&WrappedNode> = None;
        let mut occur_text: Option<String> = None;
        for c in children {
            if let Some(r) = syntax_rule(c) {
                match r {
                    "type" | "type1" | "type2" => type_children.push(c),
                    "ctlop" => {
                        ctlop_text = Some(text_of(c).trim().to_owned());
                    },
                    "memberkey" => {
                        memberkey_node = Some(c);
                    },
                    "occur" => {
                        occur_text = Some(text_of(c).trim().to_owned());
                    },
                    _ => {},
                }
            }
        }
        if ctlop_text.is_some() && type_children.len() == 2 {
            let (left, p1) = match type_children.first() {
                Some(n) => self.render_with_inlining_inner(n, prov, visited),
                None => (String::new(), None),
            };
            // The right operand is a type2 position; apply the same
            // ctlop-operand rule as the type-level ctlop join.
            let previous_ctlop_rhs = self.ctlop_rhs;
            if type_children
                .get(1)
                .is_some_and(|n| arm_is_bare_name(n).is_some())
            {
                self.ctlop_rhs = true;
            }
            let (right, _p2) = match type_children.get(1) {
                Some(n) => self.render_with_inlining_inner(n, prov, visited),
                None => (String::new(), None),
            };
            self.ctlop_rhs = previous_ctlop_rhs;
            if prov.is_none() {
                *prov = p1;
            }
            let sep = ctlop_text.unwrap_or_default();
            return (format!("{left} {sep} {right}"), None);
        }
        if let Some(mk) = memberkey_node
            && let Some((rendered_key, _)) = self.render_memberkey(mk, prov, visited)
            && type_children.len() == 1
        {
            let (mut right, _p2) = match type_children.first() {
                Some(n) => self.render_with_inlining_inner(n, prov, visited),
                None => (String::new(), None),
            };
            let right_trimmed = right.trim();
            let block_close = if right_trimmed.starts_with('[') {
                Some(']')
            } else if right_trimmed.starts_with('{') {
                Some('}')
            } else {
                None
            };
            if let Some(close) = block_close
                && right_trimmed.chars().rev().find(|c| !c.is_whitespace()) == Some(close)
                && block_ends_value(right_trimmed, close)
            {
                // A block value must use the same flattened single-line
                // body the keyed-block renderer sees, laid out with one
                // entry per line. Otherwise the layout depends on whether
                // the value came from an inlined definition (pretty path)
                // or literal source (string path), and the round-trip is
                // unstable. A choice whose first arm is a block
                // (`{ ... } / text`) or a ctlop-joined block
                // (`[ ... ] .within [ ... ]`) is not a pure block and
                // falls through to the choice flattening below.
                let open = if close == ']' { '[' } else { '{' };
                let inner = right_trimmed
                    .get(1..right_trimmed.len().saturating_sub(1))
                    .unwrap_or("");
                let flat = normalize_block_spacing(
                    &inner.split_whitespace().collect::<Vec<_>>().join(" "),
                );
                let entries = split_top_level_commas(&flat);
                let mut body = String::new();
                let mut iter = entries.iter().peekable();
                while let Some(entry) = iter.next() {
                    let entry = entry.trim();
                    if entry.is_empty() {
                        continue;
                    }
                    let suffix = if iter.peek().is_some() { "," } else { "" };
                    let _ = write!(body, "\n  {entry}{suffix}");
                }
                let prefix = occur_text
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map_or(String::new(), |s| format!("{s} "));
                return (
                    format!("{prefix}{rendered_key} {open}{body}\n{close}"),
                    None,
                );
            }
            if Self::contains_top_level_choice(&right) {
                // A member value that is a top-level choice must be
                // parenthesized (`key: (A / B)`); without the parens
                // the `/` is parsed as a group choice and the entry is
                // ambiguous. Flatten multi-line inlined choices so the
                // output matches the literal form. The structural
                // formatter owns whitespace canonicalization, so a
                // multi-line ctlop value (`key: { ... } .and { ... }`)
                // needs no wrap here.
                let flat = right.split_whitespace().collect::<Vec<_>>().join(" ");
                let flat = normalize_block_spacing(&flat);
                let flat = dedent_paren_spacing(&flat);
                // Wrap only when the value is not already fully
                // parenthesized, or re-rendering accumulates parens.
                let already_paren = flat.starts_with('(') && flat.ends_with(')');
                if already_paren {
                    right = flat;
                } else {
                    right = format!("({flat})");
                }
            }
            let prefix = occur_text
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(String::new(), |s| format!("{s} "));
            return (format!("{prefix}{rendered_key} {right}"), None);
        }

        // An occurrence-marked type may carry a parenthesized choice as its
        // sole type child (`+(choice)`). Preserve the occurrence while
        // rendering the child structurally, and group an inlined choice so
        // its `/` separators remain inside the occurrence.
        if self.force_inline
            && memberkey_node.is_none()
            && occur_text.is_some()
            && type_children.len() == 1
        {
            let (mut rendered, _p) = match type_children.first() {
                Some(n) => self.render_with_inlining_inner(n, prov, visited),
                None => (String::new(), None),
            };
            if Self::contains_top_level_choice(&rendered) {
                rendered = format!("({rendered})");
            }
            let prefix = occur_text
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(String::new(), |s| format!("{s} "));
            return (format!("{prefix}{rendered}"), None);
        }

        // Occurrence-marked references (for example `+item` in an
        // array) are not textually bare, but their referenced body
        // still needs to be expanded so the concrete view is fully
        // inlined (`[+ Incident]` must not leave `Incident` detached).
        if memberkey_node.is_none() {
            let prefix = occur_text
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(String::new(), |s| format!("{s} "));

            // The occurrence must wrap a pure bare name. A loose name
            // scan would misread `+ { -17 => text, ... }` as a bare
            // reference to the first map key (`-17`). The structure
            // check tolerates stale parent text left by generic
            // substitution, so the name comes from the child recursion.
            let type_child = children.iter().find(|c| {
                matches!(
                    c,
                    WrappedNode::Syntax { rule, .. }
                        if rule == "type" || rule == "type1" || rule == "type2"
                )
            });
            if let Some(name) = type_child.and_then(|tc| {
                // The occurrence must wrap a single bare name. A
                // parenthesized choice (`? (uint / text)`) is not a
                // bare reference; resolving it would collapse to the
                // first arm. The structure check tolerates stale parent
                // text left by generic substitution, so the name comes
                // from the child recursion.
                let tc_text = text_of(tc).trim();
                if arm_is_bare_name(tc).is_some()
                    || (arm_subtree_is_pure_name(tc)
                        && !tc_text.starts_with('(')
                        && !tc_text.starts_with('[')
                        && !tc_text.starts_with('{'))
                {
                    bare_type_or_group_name(tc)
                } else {
                    None
                }
            }) {
                let (rendered, p) = self.render_named_reference(&name, prov, visited);
                if prov.is_none() {
                    *prov = p;
                }
                let rendered = if !prefix.is_empty() && Self::contains_top_level_choice(&rendered) {
                    format!("({rendered})")
                } else {
                    rendered
                };
                return (format!("{prefix}{rendered}"), None);
            }
            // An occurrence-marked brace/bracket block (`+ { ... }`)
            // must render through the same multi-line pipeline as an
            // inlined block, or a second render pass collapses it to a
            // single line and the round-trip is unstable.
            if let Some(block) = type_child
                && let trimmed = text_of(block).trim()
                && (trimmed.starts_with('{') || trimmed.starts_with('['))
            {
                let mut out = Concrete::new();
                let text = trimmed.to_owned();
                render_pretty_rhs(self, block, 0, &text, &mut out);
                let rendered = out.to_cddl();
                let rendered = rendered.trim_end_matches('\n').to_owned();
                return (format!("{prefix}{rendered}"), None);
            }
        }

        // A single type child that is a ctlop/rangeop expression
        // (`bstr .cbor x`) must render through the type path (with the
        // ctlop-operand rule applied), not as raw source text that
        // would leave the operand name unresolved.
        if type_children.len() == 1
            && let Some(t1) = type_children.first()
            && top_level_ctlop_expression(t1)
        {
            let rendered = self.render_with_inlining_inner(t1, prov, visited).0;
            // The rendered expression may itself be a top-level choice
            // (the ctlop can sit nested inside a map body); re-wrap so
            // the `/` separators stay inside, and keep the occurrence
            // that applies to the whole entry. A result that is
            // already fully parenthesized is not re-wrapped, or
            // re-rendering accumulates parens.
            let rendered = if Self::contains_top_level_choice(&rendered)
                && !(rendered.starts_with('(') && rendered.ends_with(')'))
            {
                format!("({rendered})")
            } else {
                rendered
            };
            let prefix = occur_text
                .as_deref()
                .filter(|s| !s.is_empty())
                .map_or(String::new(), |s| format!("{s} "));
            return (format!("{prefix}{rendered}"), None);
        }

        // Single bare typename reference: a grpent whose trimmed text
        // is a bare identifier and whose only substantive child is
        // a single type-shaped node. This is the
        // `[signature, alg-sig-map-ed25519-ml-dsa-44]` case.
        if let WrappedNode::Syntax {
            text, children: gc, ..
        } = node
        {
            let t = text.trim();
            let is_bare = !t.is_empty() && t.chars().all(is_reference_name_char);
            let non_comma = gc.iter().filter(|c| !is_comma_node(c)).count();
            if is_bare
                && non_comma == 1
                // A range or control expression (`0..255`, `bstr .size 4`)
                // is not a name; resolving it would collapse the range to
                // its first operand.
                && !subtree_has_rule(node, "rangeop")
                && !subtree_has_rule(node, "ctlop")
                && let Some(name) = bare_type_or_group_name(node)
            {
                return self.render_named_reference(&name, prov, visited);
            }
        }

        // A verbatim fallback must still record any definition name it
        // emits raw, or the retained set misses it (`bstr / #6.24(bstr)
        // / (bstr .cbor admin-record)` keeps `admin-record` symbolic).
        let raw_text = text_of(node).trim();
        record_embedded_references(
            &mut self.symbolic_refs,
            raw_text,
            &self.resolution.definitions,
            &self.resolution.type_plugs,
        );
        (raw_text.to_owned(), None)
    }

    /// Render a `memberkey` (the `key =>` or `key :` half of a
    /// grpent) so that the key side is also resolved through
    /// constant folding. The memberkey text contains the trailing
    /// operator (e.g. `ed25519 =>`); we strip the operator off
    /// and recurse into the key shape.
    fn render_memberkey(
        &mut self,
        node: &WrappedNode,
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> Option<(String, Option<(String, String)>)> {
        let WrappedNode::Syntax { children, text, .. } = node else {
            return None;
        };
        // The key is a type-shaped child. Render it and append the
        // operator text from the trailing source text.
        let key_node = children.iter().find(|c| {
            matches!(
                syntax_rule(c),
                Some("type" | "type1" | "type2" | "value" | "bareword")
            )
        })?;
        let (rendered_key, p) = self.render_with_inlining_inner(key_node, prov, visited);
        if prov.is_none() {
            *prov = p;
        }
        let rendered_key = simplify_singleton_enum_key(&rendered_key);
        // A member key that renders as a top-level choice must be
        // parenthesized (`* (int / tstr) => any`); without the parens
        // the `/` is parsed as a group choice and the entry is
        // ambiguous. Flatten multi-line inlined choices so the output
        // matches the literal form.
        let rendered_key = if Self::contains_top_level_choice(&rendered_key) {
            let flat = rendered_key
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let flat = normalize_block_spacing(&flat);
            let flat = dedent_paren_spacing(&flat);
            format!("({flat})")
        } else if rendered_key.contains('\n') {
            // A key with an inlined block operand (`bytes .oid [\n 2,\n
            // 5,\n ...\n]`) must flatten to the single-line literal
            // form, or a re-render of the emitted document collapses it
            // and the round-trip differs.
            let flat = rendered_key
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            dedent_paren_spacing(&normalize_block_spacing(&flat))
        } else {
            rendered_key
        };
        // Recover the operator (`=>` or `:` or `~`) from the trailing
        // source text. The memberkey text is e.g. `ed25519 =>`.
        let trailing = memberkey_operator(text);
        Some((format!("{rendered_key}{trailing}"), None))
    }

    /// Return whether `text` contains a choice separator outside nested CDDL
    /// delimiters.  Inlined named choices need parentheses when they are used as
    /// the RHS of a group member; otherwise `/` is parsed as a group choice and
    /// escapes the `=>` binding.
    fn contains_top_level_choice(text: &str) -> bool {
        let mut depth = 0usize;
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        for byte in text.bytes() {
            if in_double {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_double = false;
                }
                continue;
            }
            if in_single {
                if byte == b'\'' {
                    in_single = false;
                }
                continue;
            }
            match byte {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'(' | b'[' | b'{' => depth = depth.saturating_add(1),
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'/' if depth == 0 => return true,
                _ => {},
            }
        }
        false
    }

    /// Look for a child that names a socket plug (bare typename,
    /// groupname, or single-identifier type/type2) and return that
    /// name. Returns `None` if no plausible name is found, an empty
    /// `Some` if a candidate name was found but did not match a plug.
    fn find_socket_plug_name(children: &[WrappedNode]) -> Option<String> {
        let mut candidate: Option<String> = None;
        for child in children {
            if let WrappedNode::Syntax { rule, text, .. } = child {
                let name_opt = match rule.as_str() {
                    "typename" | "groupname" => Some(text.trim().to_owned()),
                    "type" | "type2" => {
                        let t = text.trim();
                        if !t.is_empty() && t.chars().all(is_reference_name_char) {
                            Some(t.to_owned())
                        } else {
                            None
                        }
                    },
                    _ => None,
                };
                if let Some(name) = name_opt {
                    candidate = Some(name);
                    break;
                }
            }
        }
        candidate
    }

    /// Render a sequence of plug `RuleLines` as a `/`-joined choice
    /// wrapped in parens (matching the CDDL grammar's `grpent` shape).
    fn render_plug_choice(
        &mut self,
        plugs: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let mut parts: Vec<String> = Vec::new();
        let mut all_members = true;
        for plug in plugs {
            let WrappedNode::RuleLine { children, .. } = plug else {
                continue;
            };
            let Some(rhs) = find_rhs(children) else {
                continue;
            };
            if let Some(inner) = find_parenthesized_grpent(rhs)
                && let WrappedNode::Syntax { children: gc, .. } = inner
            {
                let rendered = self.render_grpent(inner, gc, prov, visited).0;
                parts.push(format!("({rendered})"));
            } else {
                all_members = false;
                parts.push(self.render_with_inlining_inner(rhs, prov, visited).0);
            }
        }
        match parts.len() {
            0 => (String::new(), None),
            1 => (parts.into_iter().next().unwrap_or_default(), None),
            _ => {
                // Member-shaped arms (`(key => value)`) are group
                // entries and must be comma-separated; `/` between
                // members is not accepted by the grammar.
                let sep = if all_members { ", " } else { " / " };
                (format!("({})", parts.join(sep)), None)
            },
        }
    }

    /// Render a sequence of type-socket `/=` `RuleLine`s as a
    /// `/`-joined choice. Unlike group plugs, these are type
    /// alternatives, so the rendered result is not forced into a
    /// parenthesized group-entry shape unless the choice needs it.
    fn render_type_plug_choice(
        &mut self,
        plugs: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let parts: Vec<String> = plugs
            .iter()
            .filter_map(|plug| {
                let WrappedNode::RuleLine { children, .. } = plug else {
                    return None;
                };
                let rhs = find_rhs(children)?;
                let rendered = self.render_with_inlining_inner(rhs, prov, visited).0;
                Some(simplify_singleton_enum_key(&rendered))
            })
            .collect();
        match parts.len() {
            0 => (String::new(), None),
            1 => (parts.into_iter().next().unwrap_or_default(), None),
            _ => (parts.join(" / "), None),
        }
    }

    /// Render a `group` or `grpchoice` body, walking its `grpent` children
    /// and inlining any socket-plug references. Returns a `, `-joined
    /// entry list with no surrounding brackets (the caller adds them).
    fn render_group(
        &mut self,
        children: &[WrappedNode],
        prov: &mut Option<(String, String)>,
        visited: &mut HashSet<String>,
    ) -> (String, Option<(String, String)>) {
        let mut entries: Vec<String> = Vec::new();
        for child in children {
            match child {
                WrappedNode::Syntax {
                    rule, children: gc, ..
                } if rule == "grpent" => {
                    let (rendered, p) = self.render_grpent(child, gc, prov, visited);
                    if prov.is_none() {
                        *prov = p;
                    }
                    if !rendered.is_empty() {
                        entries.push(rendered);
                    }
                },
                WrappedNode::Syntax {
                    rule, children: gc, ..
                } if rule == "group" || rule == "grpchoice" => {
                    let (inner_text, p) = self.render_group(gc, prov, visited);
                    if prov.is_none() {
                        *prov = p;
                    }
                    if !inner_text.is_empty() {
                        entries.push(inner_text);
                    }
                },
                _ => {},
            }
        }
        (entries.join(", "), None)
    }
}

/// Render an `EntryState` as its CDDL literal form.
fn constant_to_cddl(state: &EntryState) -> Option<String> {
    match state {
        EntryState::Integer(n) => Some(n.to_string()),
        EntryState::Float(f) => Some(f.to_string()),
        EntryState::Text(t) => Some(t.to_string()),
        EntryState::Bytes(b) => Some(b.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tree-shape helpers
// ---------------------------------------------------------------------------

/// True if the text is a single block (`[ ... ]` / `{ ... }`) that
/// closes at the very end — i.e. there is no trailing `.within`, choice
/// arm, or other continuation after the matching bracket.
fn block_ends_value(
    text: &str,
    _close: char,
) -> bool {
    let mut depth = 0usize;
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for (i, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_double {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'\'' {
                in_single = false;
            }
            continue;
        }
        match byte {
            b'"' => in_double = true,
            b'\'' => in_single = true,
            b'[' | b'{' => depth = depth.saturating_add(1),
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return text
                        .get(i.saturating_add(1)..)
                        .is_some_and(|rest| rest.trim().is_empty());
                }
            },
            _ => {},
        }
    }
    false
}

/// Record every definition name that appears as a bare token in `text`
/// (a verbatim-emitted entry), so the retention pass keeps it.
fn record_embedded_references(
    symbolic_refs: &mut HashSet<String>,
    text: &str,
    definitions: &HashMap<String, WrappedNode>,
    type_plugs: &HashMap<String, Vec<WrappedNode>>,
) {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '$');
    let mut start: Option<usize> = None;
    for (i, &byte) in text.as_bytes().iter().enumerate() {
        let c = byte as char;
        if is_name_char(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take()
            && let Some(name) = text.get(s..i)
            && (definitions.contains_key(name) || type_plugs.contains_key(name))
        {
            symbolic_refs.insert(name.to_owned());
        }
    }
    if let Some(s) = start
        && let Some(name) = text.get(s..)
        && (definitions.contains_key(name) || type_plugs.contains_key(name))
    {
        symbolic_refs.insert(name.to_owned());
    }
}

/// Find the RHS type-like node in a `RuleLine`'s children.
pub(crate) fn find_rhs(children: &[WrappedNode]) -> Option<&WrappedNode> {
    let mut past_lhs = false;
    for child in children {
        let Some(rule) = syntax_rule(child) else {
            continue;
        };
        match rule {
            "expr" => return find_rhs_in_expr(child),
            "assignt" | "assigng" => past_lhs = true,
            "type" | "type1" | "type2" | "group" | "grpent" if past_lhs => {
                return Some(child);
            },
            _ => {},
        }
    }
    None
}

/// Find the RHS type-like node inside an `expr` wrapper.
fn find_rhs_in_expr(node: &WrappedNode) -> Option<&WrappedNode> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };
    let mut past_lhs = false;
    for child in children {
        let Some(rule) = syntax_rule(child) else {
            continue;
        };
        match rule {
            "assignt" | "assigng" => past_lhs = true,
            "type" | "type1" | "type2" | "group" | "grpent" if past_lhs => {
                return Some(child);
            },
            _ => {},
        }
    }
    None
}

/// Find the `genericparm` text (e.g. `<T, U>`) on the LHS, recursing
/// into the `expr` wrapper that the parser emits inside a rule line.
fn find_genericparm(children: &[WrappedNode]) -> Option<String> {
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child
            && rule == "genericparm"
        {
            return Some(text.trim().to_owned());
        }
        if let WrappedNode::Syntax {
            rule,
            children: sub,
            ..
        } = child
            && rule == "expr"
        {
            for c in sub {
                if let WrappedNode::Syntax { rule: r, text, .. } = c
                    && r == "genericparm"
                {
                    return Some(text.trim().to_owned());
                }
            }
        }
    }
    None
}

/// Return the `rule` field of a `WrappedNode::Syntax`, or `None`.
pub(crate) fn syntax_rule(node: &WrappedNode) -> Option<&str> {
    if let WrappedNode::Syntax { rule, .. } = node {
        Some(rule.as_str())
    } else {
        None
    }
}

/// Return the source text of a `WrappedNode`, regardless of variant.
pub(crate) fn text_of(node: &WrappedNode) -> &str {
    match node {
        WrappedNode::Syntax { text, .. }
        | WrappedNode::RuleLine { text, .. }
        | WrappedNode::Comment { text, .. } => text,
        _ => "",
    }
}

/// True if a `WrappedNode` is a CDDL optional-comma/comment node
/// (`optcom`) that is irrelevant for arity checks.
fn is_comma_node(node: &WrappedNode) -> bool {
    if let WrappedNode::Syntax { rule, text, .. } = node {
        if rule == "optcom" {
            return true;
        }
        let t = text.trim();
        return t == "," || t.is_empty();
    }
    false
}

/// If a node is (or contains as a child) a bare identifier that names
/// a type or group, return that identifier. Used by grpent/group
/// fallback paths to drive constant folding and structural inlining
/// when the body of a group element is just a typename reference.
fn bare_type_or_group_name(node: &WrappedNode) -> Option<String> {
    match node {
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            match rule.as_str() {
                "typename" | "groupname" => Some(text.trim().to_owned()),
                "type" | "type1" | "grpent" => {
                    // Bug 001: generic substitution replaces children
                    // but leaves the parent text stale.  These
                    // non-leaf rules must always recurse into
                    // children to find the current typename.
                    recurse_bare_name(children)
                },
                "type2" | "id" | "name" => {
                    let t = text.trim();
                    if !t.is_empty()
                        && t.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                    {
                        Some(t.to_owned())
                    } else {
                        recurse_bare_name(children)
                    }
                },
                _ => recurse_bare_name(children),
            }
        },
        _ => None,
    }
}

/// Recursive helper for [`bare_type_or_group_name`].
fn recurse_bare_name(children: &[WrappedNode]) -> Option<String> {
    for c in children {
        if let Some(name) = bare_type_or_group_name(c) {
            return Some(name);
        }
    }
    None
}

/// True if a node is a strong `name := ...` definition.
fn is_strong_definition(node: &WrappedNode) -> bool {
    let WrappedNode::RuleLine { children, .. } = node else {
        return false;
    };
    let Some(head) = rule_head_from_children(children) else {
        return false;
    };
    matches!(head.assignment, AssignmentKind::Define)
        && !head.name.is_empty()
        && has_colon_eq(text_of(node))
}

/// True if the rule text contains the `:=` operator.
fn has_colon_eq(text: &str) -> bool {
    text.contains(":=")
}

/// Find the first child whose `WrappedNode::Syntax::rule` matches.
fn first_child_with_rule<'a>(
    children: &'a [WrappedNode],
    rule: &str,
) -> Option<&'a WrappedNode> {
    children.iter().find(|c| syntax_rule(c) == Some(rule))
}

/// Return the first type/group name directly contained in a node's children.
fn first_type_or_group_name(children: &[WrappedNode]) -> Option<String> {
    children.iter().find_map(|child| {
        let WrappedNode::Syntax { rule, text, .. } = child else {
            return None;
        };
        matches!(rule.as_str(), "typename" | "groupname").then(|| text.trim().to_owned())
    })
}

/// Characters allowed in references and socket names.
fn is_reference_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '$')
}

/// Names that should remain symbolic in an effective diagnostic view.
fn is_effective_base_name(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "uint"
            | "nint"
            | "bstr"
            | "tstr"
            | "text"
            | "int"
            | "float"
            | "bool"
            | "nil"
            | "null"
            | "integer"
    )
}

/// Recover the member-key operator from the source text without
/// carrying symbolic enum/key syntax forward after the rendered key
/// has been simplified.
fn memberkey_operator(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.contains("=>") {
        " =>"
    } else if trimmed.contains(':') {
        ":"
    } else if trimmed.contains('~') {
        " ~"
    } else {
        ""
    }
}

/// Flatten a text literal that spans lines in the source into a single
/// line by replacing each embedded newline (and its leading whitespace)
/// with the `\n` escape. Multi-line literals are not valid inside a
/// tag's parens, so emitted output must carry them escaped.
fn flatten_multiline_string(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.contains('\n') {
        return trimmed.to_owned();
    }
    let Some(quote) = trimmed.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
        return trimmed.to_owned();
    };
    let Some(inner) = trimmed
        .strip_prefix(quote)
        .and_then(|s| s.strip_suffix(quote))
    else {
        return trimmed.to_owned();
    };
    let mut out = String::new();
    out.push(quote);
    for line in inner.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str("\\n");
        out.push_str(line);
    }
    out.push(quote);
    out
}

/// Collapse a singleton enum literal key like `&(ClockClass: -2)`
/// to its only concrete value, `-2`.
fn simplify_singleton_enum_key(rendered_key: &str) -> String {
    let trimmed = rendered_key.trim();
    let Some(inner) = trimmed.strip_prefix("&(").and_then(|s| s.strip_suffix(')')) else {
        return rendered_key.to_owned();
    };
    // Only a single-entry enum simplifies to its value. Multi-entry
    // enums (newline- or comma-separated) must be preserved verbatim;
    // unwrapping them would drop the first key and emit the remaining
    // entries as raw text.
    if inner.trim().contains('\n') || inner.matches(':').count() > 1 {
        return rendered_key.to_owned();
    }
    let Some((_, value)) = inner.split_once(':') else {
        return rendered_key.to_owned();
    };
    if value.contains('/') || value.contains(',') {
        rendered_key.to_owned()
    } else {
        value.trim().to_owned()
    }
}

/// Detect a `[ ... ]` or `{ ... }` group bracket on a type2's children.
fn detect_group_brackets(children: &[WrappedNode]) -> Option<String> {
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child
            && (rule == "group" || rule == "grpchoice" || rule == "grpent")
        {
            let trimmed = text.trim();
            if trimmed.starts_with('[') || trimmed.starts_with('{') {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Detect a leading `#N.tag` tag, recovering the family digit from the
/// source text because the grammar consumes it without surfacing it as
/// a child.
fn leading_tag(
    children: &[WrappedNode],
    source_text: &str,
) -> Option<String> {
    let mut number: Option<String> = None;
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child
            && rule == "head_number"
        {
            number = Some(text.trim().to_owned());
        }
    }
    let number = number?;
    let trimmed = source_text.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let after_hash = trimmed.get(1..)?;
    let family = after_hash.chars().next()?;
    if !family.is_ascii_digit() {
        return None;
    }
    Some(format!("#{family}.{number}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use cbork_cddl_parser::parse_postlude;

    use super::*;
    use crate::compiled::CompiledCDDL;

    #[allow(clippy::expect_used, reason = "Allowed in tests")]
    fn compile(src: &str) -> CompiledCDDL {
        let dir = std::env::temp_dir().join("cbork_concrete_test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = dir.join(format!("test_{pid}_{nanos}.cddl"));
        std::fs::write(&path, src).expect("write cddl");
        CompiledCDDL::compile(&path, None).expect("compile")
    }

    fn render_user(src: &str) -> String {
        let compiled = compile(src);
        let res = build_resolution(&compiled.complete_nodes);
        render_to_string(
            &compiled.complete_nodes,
            &res,
            &ConcretePolicy::for_render(),
        )
    }

    #[test]
    fn folds_integer_constant_in_map() {
        let cddl = render_user("x = {\n  a => 1\n}\nA = 1\n");
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains("a => 1"), "missing `a => 1`:\n{cddl}");
    }

    #[test]
    fn inlines_socket_plug_into_map() {
        let cddl = render_user(
            "plug //= (a => int)\n\
             plug //= (b => int)\n\
             root = { plug }\n",
        );
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("(a => int), (b => int)"),
            "expected comma-separated inline plug members:\n{cddl}"
        );
    }

    #[test]
    fn does_not_render_top_level_socket_augmentations() {
        let cddl = render_user(
            "plug //= (a => int)\n\
             root = { plug }\n",
        );
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("a => int"),
            "missing plug entry:\n{cddl}"
        );
    }

    #[test]
    fn library_mode_removed() {
        // Library-preserving rendering was removed: an unreferenced
        // named constant is folded into its use site instead of being
        // emitted as a separate top-level definition.
        let cddl = render_user(
            "root = { a => A }\n\
             A = 1\n",
        );
        assert!(!cddl.contains("A = 1"), "A should be folded:\n{cddl}");
    }

    #[test]
    fn inlines_structural_references() {
        let cddl = render_user(
            "inner = { a => int }\n\
             outer = inner / bstr\n",
        );
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            // The structural type is rendered as a multi-line block,
            // so after whitespace normalization the contents collapse to
            // `{ a => int, }` (the entry comma is preserved).
            normalized.contains("{ a => int") && normalized.contains("=> int"),
            "expected inline:\n{cddl}"
        );
    }

    #[test]
    fn pqsig_style_socket_plug_expands_to_choice() {
        let cddl = render_user(
            "one-pq-signature //= (ml-dsa-44 => bstr)\n\
             one-pq-signature //= (ml-dsa-65 => bstr)\n\
             one-pq-signature //= (ml-dsa-87 => bstr)\n\
             alg-sig-map = {\n\
               ed25519 => bstr,\n\
               one-pq-signature\n\
             } .within { 2*2 int => bstr }\n\
             bstr = bytes .size 64\n",
        );
        assert!(
            cddl.contains("ml-dsa-44") && cddl.contains("ml-dsa-65") && cddl.contains("ml-dsa-87"),
            "expected all three plug entries inlined:\n{cddl}"
        );
    }

    #[test]
    fn postlude_parsing_smoke() {
        drop(parse_postlude().expect("postlude parses"));
    }

    #[test]
    fn resolves_unresolved_name_unchanged() {
        let compiled = compile("x = undefined_name\n");
        let res = build_resolution(&compiled.complete_nodes);
        let cddl = render_to_string(
            &compiled.complete_nodes,
            &res,
            &ConcretePolicy::for_render(),
        );
        assert!(cddl.contains("undefined_name"), "missing ref:\n{cddl}");
    }

    #[test]
    fn inlines_type_reference_inside_parenthesized_plug_arm_body() {
        // Plug RHS is a type reference whose body is a parenthesized
        // grpent referencing another type.  Both levels must inline.
        let cddl = render_user(
            "plug //= loc\n\
             root = { plug }\n\
             loc = ( key => inner_type )\n\
             inner_type = { field => int }\n",
        );
        assert!(
            cddl.contains("field => int"),
            "expected inner_type inlined inside loc body inside plug arm:\n{cddl}"
        );
    }

    #[test]
    fn inlines_type_reference_inside_parenthesized_plug_arm_with_nested_map() {
        let cddl = render_user(
            "plug //= loc\n\
             root = { plug }\n\
             loc = ( a => level1 )\n\
             level1 = { b => level2 }\n\
             level2 = [ int ]\n",
        );
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("[ int ]") || normalized.contains("int"),
            "expected level2 inlined inside level1 inside loc:\n{cddl}"
        );
    }

    #[test]
    fn inlines_type_reference_inside_parenthesized_plug_arm_in_array_context() {
        let cddl = render_user(
            "plug //= entry\n\
             root = [ plug ]\n\
             entry = ( key => inner_type )\n\
             inner_type = tstr\n",
        );
        assert!(
            cddl.contains("tstr"),
            "expected inner_type inlined to tstr:\n{cddl}"
        );
    }

    #[test]
    fn expands_every_named_socket_arm_and_nested_reference() {
        let cddl = render_user(
            "service-data //= ip-location\n\
             service-data //= tor-location\n\
             root = { service-data }\n\
             ip-location = ( h'01' => ip-address-locations )\n\
             ip-address-locations = [ +ip-address-or-prefix ]\n\
             ip-address-or-prefix = bstr .size 16\n\
             tor-location = ( h'02' => tor-addresses )\n\
             tor-addresses = [ +tor-address ]\n\
             tor-address = bstr .size 32\n",
        );
        let normalized = cddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("h'01'")
                && normalized.contains("bstr .size 16")
                && normalized.contains("h'02'")
                && normalized.contains("bstr .size 32"),
            "expected every named socket arm and nested reference to expand:\n{cddl}"
        );
    }
}
