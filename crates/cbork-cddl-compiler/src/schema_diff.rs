// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Inline schema diff for `.within` / `.and` diagnostics.
//!
//! Step 5 of the `.within` / `.and` rewrite plan. Combines the
//! concrete renderer output (the "what the schema actually looks like"
//! side) with the structured subtype conflicts produced by Step 3 (the
//! "where the schema fails" side) into an ordered list of
//! [`SchemaDiffLine`]s that the CLI can render as an inline diff.
//!
//! # Architecture
//!
//! * [`build_schema_diff`] is the public entry point. It takes:
//!     * the LHS and RHS [`WrappedNode`] subtrees (as extracted by
//!       [`crate::within::validate_within_pass`] via `extract_within_operands`),
//!     * the slice of [`WithinConflict`]s collected by the subtype checker, and
//!     * the [`ResolutionMap`] used to fold constants and inline socket plugs.
//! * It renders both sides to multi-line text using [`concrete::render_subtree`] with the
//!   existing [`ConcretePolicy::for_lhs`] / [`ConcretePolicy::for_rhs`] policies, then
//!   splits the text into logical lines (preserving indentation as part of `text`).
//! * It runs a path-aware AST walk alongside the renderer. For each grpent (and similar
//!   atomic structural element) it records the [`PathSegment`] stack and the element's
//!   rendered text. It then locates the rendered line that contains each element's text.
//! * The diff is **path-authoritative**:
//!     * `WithinConflict.path` decides which line carries [`SchemaDiffKind::LhsRejected`]
//!       or [`SchemaDiffKind::RhsRequiredMissing`].
//!     * Text-based LCS is used only to mark [`SchemaDiffKind::Matched`] lines and to
//!       label unaligned lines as [`SchemaDiffKind::Context`].
//!     * If a conflict path cannot be matched to a rendered line, the conflict becomes a
//!       [`SchemaDiffKind::Note`] at the top of the diff rather than guessing.
//! * Optional RHS entries (`?` grpents) are detected from the AST, not from the rendered
//!   text, because the renderer drops the `?` marker. Unaligned RHS lines whose path
//!   overlaps an optional grpent become [`SchemaDiffKind::RhsOptional`].
//!
//! # v1 caveats
//!
//! v1 is intentionally conservative. It only attributes
//! `LhsRejected` / `RhsRequiredMissing` to lines whose path matches
//! the conflict's path. When the renderer inlines multiple grpents
//! onto a single line, the inlined line is treated as the line for
//! every grpent — that is, multiple conflicts on the same inlined
//! line all attach to it. A future revision could split inlined
//! renderings; for now this matches the plan's "v1 should not lie"
//! requirement.
//!
//! # Wiring
//!
//! [`build_schema_diff`] is called from
//! [`crate::within::check_within_constraint`]; the resulting diff
//! is converted to `Subdiag` entries and attached to the `E030`
//! `Diagnostic` via `Diagnostic.related` so the rendered error
//! shows the inline diff inline with the rule source.
//!
//! # Lints
//!
//! The diff builder is a path-aware AST walker plus a well-known
//! text LCS alignment. Both rely on 2D-array indexing and `+1` /
//! `+=` arithmetic on length-bounded inputs. The private
//! AST-walk helpers are intentionally tiny and named for their
//! obvious purpose; we suppress the missing-docs lint for them
//! rather than write a paragraph for each.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "LCS table indices and counters are bounded by input length"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "Private walker helpers are tiny and self-describing"
)]

use std::collections::HashMap;

use crate::{
    WrappedNode,
    concrete::{self, ConcretePolicy, ResolutionMap},
    within::{PathSegment, WithinConflict, WithinConflictKind},
};

/// Classification of a single line in the diff output.
///
/// The mapping to [`crate::error::SubdiagKind`] is fixed by Step 6 of
/// the plan:
///
/// * [`SchemaDiffKind::Matched`] → `SubdiagKind::Matched`
/// * [`SchemaDiffKind::LhsRejected`] → `SubdiagKind::Unmatched`
/// * [`SchemaDiffKind::RhsRequiredMissing`] → `SubdiagKind::Unmatched`
/// * [`SchemaDiffKind::RhsOptional`] → `SubdiagKind::Optional`
/// * [`SchemaDiffKind::Context`] → unrendered context
/// * [`SchemaDiffKind::Note`] → `SubdiagKind::Note`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SchemaDiffKind {
    /// Background line that explains context but is neither matched
    /// nor rejected. Used for whitespace, comments, and lines the v1
    /// aligner could not classify.
    Context,
    /// LHS and RHS lines that match exactly (modulo whitespace
    /// normalization).
    Matched,
    /// LHS line whose path overlaps a
    /// `LhsNotAccepted` / `TooManyMatches` / `PrimitiveMismatch` /
    /// `RangeMismatch` / `ControlMismatch` conflict. Carries the
    /// conflict's `reason`.
    LhsRejected,
    /// RHS line whose path overlaps a `MissingRequiredRhs` /
    /// `ControlMismatch` / `PrimitiveMismatch` / `RangeMismatch`
    /// conflict. Carries the conflict's `reason`.
    RhsRequiredMissing,
    /// RHS line that contains an optional (`?`) entry absent from
    /// the LHS. No conflict is required for this to be emitted.
    RhsOptional,
    /// Free-form note (e.g. a pathless conflict summary, or a
    /// conflict whose path could not be mapped to a rendered line).
    Note,
}

/// One line of the inline diff output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaDiffLine {
    /// How the CLI should classify this line.
    pub kind: SchemaDiffKind,
    /// The line text, including any leading whitespace that
    /// preserves the renderer's indentation.
    pub text: String,
    /// Optional human-readable reason attached to
    /// [`SchemaDiffKind::LhsRejected`],
    /// [`SchemaDiffKind::RhsRequiredMissing`], and
    /// [`SchemaDiffKind::Note`] lines. `None` for
    /// [`SchemaDiffKind::Matched`] and unannotated
    /// [`SchemaDiffKind::Context`] / [`SchemaDiffKind::RhsOptional`]
    /// lines.
    pub reason: Option<String>,
}

/// Build the inline diff for a `.within` / `.and` check.
///
/// * `lhs_node` / `rhs_node` are the two subtrees extracted from the original `.within`
///   (or `.and`) ctlop by `extract_within_operands`.
/// * `conflicts` is the slice returned by [`crate::within::subtype_conflicts`]. An empty
///   slice produces a diff composed only of [`SchemaDiffKind::Matched`] and
///   [`SchemaDiffKind::Context`] lines.
/// * `resolution` is the concrete renderer's name-resolution cache.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "Single linear pipeline: render, attribute, classify, emit"
)]
pub(crate) fn build_schema_diff(
    lhs_node: &WrappedNode,
    rhs_node: &WrappedNode,
    conflicts: &[WithinConflict],
    resolution: &ResolutionMap,
) -> Vec<SchemaDiffLine> {
    let lhs_text =
        concrete::render_subtree(lhs_node, resolution, &ConcretePolicy::for_lhs()).to_cddl();
    let rhs_text =
        concrete::render_subtree(rhs_node, resolution, &ConcretePolicy::for_rhs()).to_cddl();
    let lhs_lines = split_logical_lines(&lhs_text);
    let rhs_lines = split_logical_lines(&rhs_text);

    let lhs_path_to_line = build_path_to_line_map(lhs_node, resolution, &lhs_lines, Side::Lhs);
    let rhs_path_to_line = build_path_to_line_map(rhs_node, resolution, &rhs_lines, Side::Rhs);

    let lcs = longest_common_subsequence(&lhs_lines, &rhs_lines);
    let lhs_matched = matched_mask(lhs_lines.len(), &lcs, true);
    let rhs_matched = matched_mask(rhs_lines.len(), &lcs, false);

    let rhs_has_optional = rhs_path_to_line.values().any(|info| info.optional);

    let mut lhs_reject: HashMap<usize, String> = HashMap::new();
    let mut rhs_required: HashMap<usize, String> = HashMap::new();
    let mut notes: Vec<SchemaDiffLine> = Vec::new();
    let mut lhs_used: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut rhs_used: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for conflict in conflicts {
        match conflict.kind {
            WithinConflictKind::LhsNotAccepted
            | WithinConflictKind::TooManyMatches
            | WithinConflictKind::PrimitiveMismatch
            | WithinConflictKind::RangeMismatch => {
                if let Some(info) =
                    lookup_path_with_suffix_fallback(&lhs_path_to_line, &conflict.path)
                    && lhs_used.insert(info.line)
                {
                    lhs_reject.insert(info.line, conflict.reason.clone());
                } else {
                    notes.push(SchemaDiffLine {
                        kind: SchemaDiffKind::Note,
                        text: String::new(),
                        reason: Some(conflict.reason.clone()),
                    });
                }
            },
            WithinConflictKind::MissingRequiredRhs => {
                if let Some(info) =
                    lookup_path_with_suffix_fallback(&rhs_path_to_line, &conflict.path)
                    && rhs_used.insert(info.line)
                {
                    rhs_required.insert(info.line, conflict.reason.clone());
                } else {
                    notes.push(SchemaDiffLine {
                        kind: SchemaDiffKind::Note,
                        text: String::new(),
                        reason: Some(conflict.reason.clone()),
                    });
                }
            },
            WithinConflictKind::ControlMismatch => {
                // Control mismatch spans both sides; attach to whichever
                // side's path can be resolved. If neither resolves, fall
                // back to a Note.
                let lhs_hit = lhs_path_to_line.get(&conflict.path).and_then(|info| {
                    if lhs_used.insert(info.line) {
                        lhs_reject.insert(info.line, conflict.reason.clone());
                        Some(info.line)
                    } else {
                        None
                    }
                });
                let rhs_hit = rhs_path_to_line.get(&conflict.path).and_then(|info| {
                    if rhs_used.insert(info.line) {
                        rhs_required.insert(info.line, conflict.reason.clone());
                        Some(info.line)
                    } else {
                        None
                    }
                });
                if lhs_hit.is_none() && rhs_hit.is_none() {
                    notes.push(SchemaDiffLine {
                        kind: SchemaDiffKind::Note,
                        text: String::new(),
                        reason: Some(conflict.reason.clone()),
                    });
                }
            },
            WithinConflictKind::DifferentStructure | WithinConflictKind::UnresolvedName => {
                notes.push(SchemaDiffLine {
                    kind: SchemaDiffKind::Note,
                    text: String::new(),
                    reason: Some(conflict.reason.clone()),
                });
            },
        }
    }

    let mut out = Vec::with_capacity(notes.len() + lhs_lines.len() + rhs_lines.len());
    out.extend(notes);

    for (i, line) in lhs_lines.iter().enumerate() {
        let kind = if lhs_matched[i] {
            SchemaDiffKind::Matched
        } else if lhs_reject.contains_key(&i) {
            SchemaDiffKind::LhsRejected
        } else {
            SchemaDiffKind::Context
        };
        out.push(SchemaDiffLine {
            kind,
            text: line.clone(),
            reason: lhs_reject.get(&i).cloned(),
        });
    }

    for (i, line) in rhs_lines.iter().enumerate() {
        let kind = if rhs_matched[i] {
            SchemaDiffKind::Matched
        } else if rhs_required.contains_key(&i) {
            SchemaDiffKind::RhsRequiredMissing
        } else if rhs_has_optional {
            // Only mark an unaligned line as optional if the line's
            // own path overlaps an optional grpent. Lines that are
            // unaligned for unrelated reasons (e.g. the LHS carries
            // extra required keys) stay `Context` so the user is not
            // misled into thinking the RHS "offered" them.
            let line_path_is_optional = rhs_path_to_line
                .values()
                .any(|info| info.line == i && info.optional);
            if line_path_is_optional {
                SchemaDiffKind::RhsOptional
            } else {
                SchemaDiffKind::Context
            }
        } else {
            SchemaDiffKind::Context
        };
        out.push(SchemaDiffLine {
            kind,
            text: line.clone(),
            reason: rhs_required.get(&i).cloned(),
        });
    }

    out
}

// ---------------------------------------------------------------------------
// Line splitting
// ---------------------------------------------------------------------------

/// Split a renderer's multi-line output into logical lines.
///
/// Preserves leading whitespace (indentation) as part of each
/// returned string. Empty trailing newlines produced by the
/// renderer's final `writeln` are dropped so the diff does not end
/// with phantom empty lines.
fn split_logical_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text.split('\n').map(str::to_owned).collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Path-aware AST walking and line attribution
// ---------------------------------------------------------------------------

/// Which side of the diff we are building the path map for. Used
/// only to pick the right [`ConcretePolicy`].
#[derive(Debug, Clone, Copy)]
enum Side {
    Lhs,
    Rhs,
}

#[derive(Debug, Clone, Copy)]
struct LineInfo {
    /// Index into the rendered line vector that this path is
    /// attributed to.
    line: usize,
    /// True if the AST node at this path is an optional (`?`)
    /// grpent.
    optional: bool,
}

/// Walk the AST and build a map from [`PathSegment`] stack to the
/// rendered line that the AST node contributes to.
///
/// The algorithm: for every `grpent` (and similar atomic structural
/// element) in the AST, render just that element with
/// [`concrete::render_subtree`] to obtain its own multi-line text.
/// Then locate the first line in `rendered_lines` whose text contains
/// the element's text. The element's path is recorded against that
/// line.
///
/// Inlined renderings (e.g. a single-line `{ 1 => int, 2 => tstr }`)
/// cause every grpent to attribute to the same line — that is the
/// best v1 can do without rendering each grpent to its own line.
fn build_path_to_line_map(
    node: &WrappedNode,
    resolution: &ResolutionMap,
    rendered_lines: &[String],
    side: Side,
) -> HashMap<Vec<PathSegment>, LineInfo> {
    let policy = match side {
        Side::Lhs => ConcretePolicy::for_lhs(),
        Side::Rhs => ConcretePolicy::for_rhs(),
    };
    let atoms = collect_atoms(node, resolution, &policy);
    let mut out = HashMap::new();
    for atom in &atoms {
        // Skip atoms whose fragment didn't make it into the rendered
        // text (defensive — should not happen in practice).
        let Some(line) = find_line_containing(rendered_lines, &atom.fragment) else {
            continue;
        };
        out.entry(atom.path.clone()).or_insert(LineInfo {
            line,
            optional: atom.optional,
        });
    }
    out
}

/// BUG-007 helper: look up a conflict path in a `path-to-line` map,
/// falling back to the longest path suffix that has an entry.
///
/// The subtype checker constructs conflict paths from `ResolvedType`
/// recursion (including `ArrayIndex`, `ControlOp`, etc.), while the
/// diff renderer only emits atoms for grpents (`MapEntry` / `ChoiceArm`).
/// The two paths rarely match exactly.  The longest-suffix fallback
/// lets a path like `[ArrayIndex(0), ControlOp(Within), MapEntry(0)]`
/// resolve to the atom at `[MapEntry(0)]` when the inner `MapEntry` is
/// the closest attribution point.
fn lookup_path_with_suffix_fallback(
    map: &HashMap<Vec<PathSegment>, LineInfo>,
    path: &[PathSegment],
) -> Option<LineInfo> {
    if let Some(info) = map.get(path).copied() {
        return Some(info);
    }
    let mut best: Option<(usize, LineInfo)> = None;
    for (candidate, info) in map {
        if candidate.len() > path.len() {
            continue;
        }
        if path[path.len() - candidate.len()..] == **candidate {
            match best {
                Some((len, _)) if len >= candidate.len() => {},
                _ => best = Some((candidate.len(), *info)),
            }
        }
    }
    best.map(|(_, info)| info)
}

fn find_line_containing(
    lines: &[String],
    fragment: &str,
) -> Option<usize> {
    let needle = fragment.trim();
    if needle.is_empty() {
        return None;
    }
    // BUG-007 fix: multi-line fragments (e.g. an inlined `.within`
    // chain split across lines by the pretty renderer) must match
    // against any rendered line that contains any of the fragment's
    // non-empty lines.  Each candidate is also tried with a trailing
    // punctuation-tolerant prefix so an empty body `{` rendered as
    // `{` on its own line still matches `key: {}`.
    let mut candidates: Vec<&str> = needle
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Add the full needle as a final candidate (covers single-line
    // fragments where splitting didn't help).
    if candidates.is_empty() {
        candidates.push(needle);
    }
    for needle in &candidates {
        // Try the literal substring first.
        for (i, line) in lines.iter().enumerate() {
            if line.contains(needle) {
                return Some(i);
            }
        }
        // Fall back: also accept a line that starts with the
        // needle's `key: prefix` even when the body `{...}` was
        // split across lines by the pretty renderer.
        if let Some(prefix) = needle.split('{').next() {
            let trimmed_prefix = prefix.trim_end();
            if trimmed_prefix != *needle && !trimmed_prefix.is_empty() {
                for (i, line) in lines.iter().enumerate() {
                    if line.trim_start().starts_with(trimmed_prefix) {
                        return Some(i);
                    }
                }
            }
        }
    }
    None
}

/// A structural element of the schema along with its rendered text
/// fragment and the path that locates it.
#[derive(Debug, Clone)]
struct Atom {
    /// Path from the schema root to this element.
    path: Vec<PathSegment>,
    /// The element's rendered text (possibly multi-line).
    fragment: String,
    /// True if the element is an optional (`?`) grpent.
    optional: bool,
}

fn collect_atoms(
    node: &WrappedNode,
    resolution: &ResolutionMap,
    policy: &ConcretePolicy,
) -> Vec<Atom> {
    let mut out = Vec::new();
    let mut path: Vec<PathSegment> = Vec::new();
    walk_atoms(node, resolution, policy, &mut path, false, &mut out);
    out
}

#[allow(
    clippy::too_many_lines,
    reason = "Single linear AST walker; each rule branch is small but the dispatch dominates"
)]
fn walk_atoms(
    node: &WrappedNode,
    resolution: &ResolutionMap,
    policy: &ConcretePolicy,
    path: &mut Vec<PathSegment>,
    optional: bool,
    out: &mut Vec<Atom>,
) {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return;
    };
    match rule.as_str() {
        "type" => {
            // `type` wraps a choice of `type1` arms. The BUG-007
            // fix only emits `ChoiceArm(i)` when the type is a real
            // choice (multiple type1 children).  Single-type1 types
            // (e.g. an array `[a, b, c]`) do not push ChoiceArm so
            // their grpent atoms carry paths like `[MapEntry(0)]`,
            // matching the subtype checker's path segments.
            let type1_indices: Vec<usize> = children
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    if syntax_rule_of(c) == Some("type1") {
                        Some(i)
                    } else {
                        None
                    }
                })
                .collect();
            if type1_indices.len() > 1 {
                for i in type1_indices {
                    path.push(PathSegment::ChoiceArm(i));
                    walk_atoms(&children[i], resolution, policy, path, optional, out);
                    path.pop();
                }
            } else if let Some(&i) = type1_indices.first() {
                walk_atoms(&children[i], resolution, policy, path, optional, out);
            }
        },
        "type1" => {
            // BUG-007: descend into a single type2 child (when
            // present) so a brace/bracket body inside the type1
            // exposes per-grpent atoms with `MapEntry(i)` paths.
            // The subtype checker uses `MapEntry(i)` for map-level
            // subtype failures, so the diff renderer must produce
            // atoms with the same path shape.
            let mut descended = false;
            for child in children {
                if matches!(syntax_rule_of(child), Some("type2")) {
                    walk_atoms(child, resolution, policy, path, optional, out);
                    descended = true;
                    break;
                }
            }
            if !descended {
                let fragment = concrete::render_subtree(node, resolution, policy).to_cddl();
                if !fragment.trim().is_empty() {
                    out.push(Atom {
                        path: path.clone(),
                        fragment: fragment.trim().to_owned(),
                        optional,
                    });
                }
            }
        },
        "type2" => {
            // For a type2 carrying a group/grpchoice, descend into
            // the group so each grpent gets its own atom. Otherwise
            // treat the type2 as an atom.
            let mut descended = false;
            for child in children {
                if matches!(syntax_rule_of(child), Some("group" | "grpchoice")) {
                    walk_atoms(child, resolution, policy, path, optional, out);
                    descended = true;
                }
            }
            if !descended {
                let fragment = concrete::render_subtree(node, resolution, policy).to_cddl();
                if !fragment.trim().is_empty() {
                    out.push(Atom {
                        path: path.clone(),
                        fragment: fragment.trim().to_owned(),
                        optional,
                    });
                }
            }
        },
        "group" | "grpchoice" => {
            // Each grpent child gets a MapEntry path segment. A
            // group can also wrap a single `grpchoice`, in which
            // case the grpchoice carries the grpents (this is the
            // common shape for `{ k => v, ... }`).
            //
            // BUG-007 fix: the `MapEntry(i)` index counts ONLY
            // grpents, not the position in `children`.  The subtype
            // checker's `MapEntry(i)` likewise counts grpents after
            // expand_map_sockets, so the index must agree.  Non-
            // grpent siblings (e.g. comments, empty-text nodes) are
            // skipped without consuming a position.
            let mut grpent_idx: usize = 0;
            for child in children {
                match syntax_rule_of(child) {
                    Some("grpent") => {
                        let (grpent_optional, _grpent_text) = grpent_optionality(child);
                        path.push(PathSegment::MapEntry(grpent_idx));
                        walk_atoms(
                            child,
                            resolution,
                            policy,
                            path,
                            optional || grpent_optional,
                            out,
                        );
                        path.pop();
                        grpent_idx = grpent_idx.saturating_add(1);
                    },
                    Some("grpchoice") => {
                        // Recurse into the nested grpchoice without
                        // pushing a path segment; the grpents inside
                        // will be attributed relative to this node.
                        walk_atoms(child, resolution, policy, path, optional, out);
                    },
                    _ => {},
                }
            }
        },
        "grpent" => {
            // Render the grpent on its own to get its fragment.
            let fragment = concrete::render_subtree(node, resolution, policy).to_cddl();
            if !fragment.trim().is_empty() {
                out.push(Atom {
                    path: path.clone(),
                    fragment: fragment.trim().to_owned(),
                    optional,
                });
            }
        },
        _ => {},
    }
}

/// Return `(optional, trimmed_text)` for a `grpent` node. A
/// grpent is optional when its source text starts with `?`.
fn grpent_optionality(grpent: &WrappedNode) -> (bool, String) {
    if let WrappedNode::Syntax { text, .. } = grpent {
        let trimmed = text.trim_start();
        (trimmed.starts_with('?'), trimmed.to_owned())
    } else {
        (false, String::new())
    }
}

fn syntax_rule_of(node: &WrappedNode) -> Option<&str> {
    if let WrappedNode::Syntax { rule, .. } = node {
        Some(rule.as_str())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// LCS alignment (context only — conflicts drive LhsRejected / RhsRequiredMissing)
// ---------------------------------------------------------------------------

fn matched_mask(
    n: usize,
    lcs: &[(usize, usize)],
    is_lhs: bool,
) -> Vec<bool> {
    let mut mask = vec![false; n];
    for pair in lcs {
        let idx = if is_lhs { pair.0 } else { pair.1 };
        if idx < mask.len() {
            mask[idx] = true;
        }
    }
    mask
}

/// Standard LCS table returning the set of matched index pairs.
///
/// Uses normalized whitespace for the equality test so that
/// indentation differences between LHS and RHS do not break the
/// alignment. Used here only for the [`SchemaDiffKind::Matched`]
/// classification; the conflict-driven path map is the authority
/// for [`SchemaDiffKind::LhsRejected`] and
/// [`SchemaDiffKind::RhsRequiredMissing`].
fn longest_common_subsequence(
    lhs: &[String],
    rhs: &[String],
) -> Vec<(usize, usize)> {
    let lhs_norm: Vec<String> = lhs.iter().map(|s| normalize_for_lcs(s)).collect();
    let rhs_norm: Vec<String> = rhs.iter().map(|s| normalize_for_lcs(s)).collect();

    let n = lhs_norm.len();
    let m = rhs_norm.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }

    // dp[i][j] = LCS length of lhs_norm[i..] and rhs_norm[j..]
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if lhs_norm[i] == rhs_norm[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut pairs = Vec::with_capacity(dp[0][0]);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if lhs_norm[i] == rhs_norm[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

fn normalize_for_lcs(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use cbork_cddl_parser::parse_cddl;

    use super::*;
    use crate::{
        concrete::build_resolution,
        preprocessor::{inject_directives, process_ast},
    };

    fn parse_snippet(source: &str) -> Vec<WrappedNode> {
        let pairs = parse_cddl(source).expect("parse should succeed");
        let pairs = process_ast(pairs).expect("preprocess should succeed");
        inject_directives(&std::path::PathBuf::from("<test>"), &pairs, source)
            .expect("directive injection should succeed")
    }

    fn first_operands(source: &str) -> (WrappedNode, WrappedNode) {
        let nodes = parse_snippet(source);
        for node in &nodes {
            if let WrappedNode::RuleLine { children, .. } = node
                && let Some((lhs, rhs)) = find_first_within_pair(children)
            {
                return (lhs, rhs);
            }
        }
        panic!("no `.within` pair found in source: {source}");
    }

    fn find_first_within_pair(children: &[WrappedNode]) -> Option<(WrappedNode, WrappedNode)> {
        fn walk(children: &[WrappedNode]) -> Option<(WrappedNode, WrappedNode)> {
            for child in children {
                if let WrappedNode::Syntax {
                    rule, children: tc, ..
                } = child
                    && rule == "type"
                {
                    for c in tc {
                        if let WrappedNode::Syntax {
                            rule: cr,
                            children: t1c,
                            ..
                        } = c
                            && cr == "type1"
                        {
                            let mut ctlop_idx: Option<usize> = None;
                            for (i, t) in t1c.iter().enumerate() {
                                if let WrappedNode::Syntax { rule: r, text, .. } = t
                                    && r == "ctlop"
                                    && text.trim() == ".within"
                                {
                                    ctlop_idx = Some(i);
                                    break;
                                }
                            }
                            if let Some(idx) = ctlop_idx {
                                let lhs = t1c[..idx]
                                    .iter()
                                    .rev()
                                    .find(|x| {
                                        matches!(x, WrappedNode::Syntax { rule, .. }
                                            if rule == "type2")
                                    })
                                    .cloned();
                                let rhs = t1c[idx + 1..]
                                    .iter()
                                    .find(|x| {
                                        matches!(x, WrappedNode::Syntax { rule, .. }
                                            if rule == "type2")
                                    })
                                    .cloned();
                                if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                                    return Some((lhs, rhs));
                                }
                            }
                        }
                    }
                }
                if let WrappedNode::Syntax {
                    children: inner, ..
                } = child
                    && let Some(pair) = walk(inner)
                {
                    return Some(pair);
                }
            }
            None
        }
        walk(children)
    }

    fn diff_for(source: &str) -> Vec<SchemaDiffLine> {
        let nodes = parse_snippet(source);
        let resolution = build_resolution(&nodes);
        let (lhs, rhs) = first_operands(source);
        build_schema_diff(&lhs, &rhs, &[], &resolution)
    }

    fn diff_with_conflicts(
        source: &str,
        conflicts: &[WithinConflict],
    ) -> Vec<SchemaDiffLine> {
        let nodes = parse_snippet(source);
        let resolution = build_resolution(&nodes);
        let (lhs, rhs) = first_operands(source);
        build_schema_diff(&lhs, &rhs, conflicts, &resolution)
    }

    // -- 1. Identical maps produce only Matched / Context lines ----

    #[test]
    fn identical_maps_are_fully_matched() {
        let source = "x = { 1 => int, 2 => tstr } .within { 1 => int, 2 => tstr }\n";
        let diff = diff_for(source);
        assert!(
            diff.iter()
                .all(|l| matches!(l.kind, SchemaDiffKind::Matched | SchemaDiffKind::Context)),
            "expected only Matched/Context, got {diff:#?}"
        );
        assert!(
            diff.iter().any(|l| l.kind == SchemaDiffKind::Matched),
            "expected at least one Matched line, got {diff:#?}"
        );
    }

    // -- 2. Extra LHS key produces LhsRejected ---------------------

    #[test]
    fn extra_lhs_key_produces_lhs_rejected() {
        // The LHS has a third key `3 => bool` that the RHS does not
        // accept. The conflict is path-authoritative: the line
        // attributed to `MapEntry(2)` on the LHS becomes
        // `LhsRejected`.
        let source = "x = { 1 => int, 2 => tstr, 3 => bool } .within { 1 => int, ? 2 => tstr }\n";
        let conflict = WithinConflict {
            path: vec![PathSegment::MapEntry(2)],
            kind: WithinConflictKind::LhsNotAccepted,
            lhs: None,
            rhs: None,
            reason: "LHS has a required key not accepted by the RHS".to_owned(),
        };
        let diff = diff_with_conflicts(source, &[conflict]);
        assert!(
            diff.iter().any(|l| l.kind == SchemaDiffKind::LhsRejected),
            "expected at least one LhsRejected line, got {diff:#?}"
        );
        // The LhsRejected line must carry the conflict reason.
        let rejected = diff
            .iter()
            .find(|l| l.kind == SchemaDiffKind::LhsRejected)
            .unwrap();
        assert_eq!(
            rejected.reason.as_deref(),
            Some("LHS has a required key not accepted by the RHS"),
            "LhsRejected line should carry the conflict reason, got {rejected:?}"
        );
    }

    // -- 3. Missing required RHS key produces RhsRequiredMissing --

    #[test]
    fn missing_required_rhs_key_produces_required_missing() {
        // The RHS has a third required key `3 => bool` that the LHS
        // does not provide.
        let source = "x = { 1 => int, 2 => tstr } .within { 1 => int, 2 => tstr, 3 => bool }\n";
        let conflict = WithinConflict {
            path: vec![PathSegment::MapEntry(2)],
            kind: WithinConflictKind::MissingRequiredRhs,
            lhs: None,
            rhs: None,
            reason: "RHS requires a key the LHS does not provide".to_owned(),
        };
        let diff = diff_with_conflicts(source, &[conflict]);
        assert!(
            diff.iter()
                .any(|l| l.kind == SchemaDiffKind::RhsRequiredMissing),
            "expected at least one RhsRequiredMissing line, got {diff:#?}"
        );
        let required = diff
            .iter()
            .find(|l| l.kind == SchemaDiffKind::RhsRequiredMissing)
            .unwrap();
        assert_eq!(
            required.reason.as_deref(),
            Some("RHS requires a key the LHS does not provide"),
            "RhsRequiredMissing line should carry the conflict reason, got {required:?}"
        );
    }

    // -- 4. Optional RHS key produces RhsOptional -----------------

    #[test]
    fn optional_rhs_key_produces_rhs_optional() {
        // RHS has an optional key `? 3 => bool` that the LHS omits.
        // No conflict is required.
        let source = "x = { 1 => int, 2 => tstr } .within { 1 => int, 2 => tstr, ? 3 => bool }\n";
        let diff = diff_for(source);
        assert!(
            diff.iter().any(|l| l.kind == SchemaDiffKind::RhsOptional),
            "expected at least one RhsOptional line, got {diff:#?}"
        );
    }

    // -- 5. Pqsig-style nested fixture preserves indentation -----

    #[test]
    fn pqsig_style_nested_keeps_indentation() {
        // A nested map where the inner key/value pair must keep its
        // two-space indent in the diff output.
        let source = "\
            payload = { 1 => int }\n\
            sig = bstr\n\
            x = { sig => bstr, payload => { 1 => int, 2 => tstr } } .within \
                { sig => bstr, payload => { 1 => int, ? 2 => tstr } }\n\
        ";
        let diff = diff_for(source);
        let has_indented = diff
            .iter()
            .any(|l| l.text.starts_with("  ") || l.text.starts_with("    "));
        assert!(
            has_indented,
            "expected at least one indented line in the diff, got {diff:#?}"
        );
        // The whole diff should still be classifiable into the five
        // kinds (no Notes — no conflicts supplied).
        assert!(
            diff.iter()
                .all(|l| !matches!(l.kind, SchemaDiffKind::Note | SchemaDiffKind::LhsRejected)),
            "pqsig-style diff should be plain Matched/Context/Optional, got {diff:#?}"
        );
    }

    // -- Sanity: no conflicts, no Notes ---------------------------

    #[test]
    fn no_conflicts_means_no_notes() {
        let source = "x = { 1 => int } .within { 1 => int, ? 2 => tstr }\n";
        let diff = diff_for(source);
        assert!(
            diff.iter().all(|l| l.kind != SchemaDiffKind::Note),
            "expected no Note lines with empty conflicts, got {diff:#?}"
        );
    }

    // -- Sanity: matched lines never carry a reason ---------------

    #[test]
    fn matched_lines_carry_no_reason() {
        let source = "x = { 1 => int, 2 => tstr } .within { 1 => int, 2 => tstr }\n";
        let diff = diff_for(source);
        for l in &diff {
            if l.kind == SchemaDiffKind::Matched {
                assert!(
                    l.reason.is_none(),
                    "Matched line should not carry a reason, got {l:?}"
                );
            }
        }
    }

    // -- Sanity: empty resolution does not panic -----------------

    #[test]
    fn empty_resolution_does_not_panic() {
        let source = "x = { 1 => int } .within { 1 => int }\n";
        let (lhs, rhs) = first_operands(source);
        let resolution = ResolutionMap {
            definitions: std::collections::HashMap::new(),
            socket_plugs: std::collections::HashMap::new(),
            type_plugs: std::collections::HashMap::new(),
            cache: crate::resolver_cache::ResolverCache::default(),
            referenced_names: std::collections::HashSet::new(),
            recursive_symbols: std::collections::HashSet::new(),
            elidable_self: std::collections::HashSet::new(),
            render_diagnostics: std::cell::RefCell::new(Vec::new()),
        };
        let diff = build_schema_diff(&lhs, &rhs, &[], &resolution);
        assert!(
            diff.iter().any(|l| l.kind == SchemaDiffKind::Matched),
            "expected at least one Matched line, got {diff:#?}"
        );
    }

    // -- Sanity: pathless conflict becomes a Note -----------------

    #[test]
    fn pathless_conflict_becomes_note() {
        // A pathless conflict (e.g. DifferentStructure) cannot be
        // attributed to a line. It must surface as a Note rather
        // than being mis-attached to an arbitrary LHS line.
        let source = "x = { 1 => int } .within { 1 => int }\n";
        let conflict = WithinConflict {
            path: Vec::new(),
            kind: WithinConflictKind::DifferentStructure,
            lhs: None,
            rhs: None,
            reason: "structural mismatch".to_owned(),
        };
        let diff = diff_with_conflicts(source, &[conflict]);
        let note = diff
            .iter()
            .find(|l| l.kind == SchemaDiffKind::Note)
            .expect("expected a Note for the pathless conflict");
        assert_eq!(note.reason.as_deref(), Some("structural mismatch"));
        // No LHS line should be mis-classified as LhsRejected for a
        // pathless conflict.
        assert!(
            !diff.iter().any(|l| l.kind == SchemaDiffKind::LhsRejected),
            "pathless conflict should not produce LhsRejected, got {diff:#?}"
        );
    }

    // -- Sanity: conflict with unmapped path becomes a Note -------

    #[test]
    fn unmapped_path_conflict_becomes_note() {
        // A conflict whose path does not match any rendered line
        // (e.g. a `ControlOp` path on a top-level primitive) cannot
        // be attributed. It must surface as a Note.
        let source = "x = { 1 => int } .within { 1 => int }\n";
        let conflict = WithinConflict {
            path: vec![PathSegment::ControlOp(crate::within::ControlOp::Cbor)],
            kind: WithinConflictKind::ControlMismatch,
            lhs: None,
            rhs: None,
            reason: "control operator mismatch".to_owned(),
        };
        let diff = diff_with_conflicts(source, &[conflict]);
        assert!(
            diff.iter().any(|l| l.kind == SchemaDiffKind::Note),
            "expected a Note for the unmapped-path conflict, got {diff:#?}"
        );
    }
}
