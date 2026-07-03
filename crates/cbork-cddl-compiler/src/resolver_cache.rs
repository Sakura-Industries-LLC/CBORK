// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Resolution cache for the CDDL compiler.
//!
//! Tracks every type/rule name encountered during include/import resolution
//! so that the compiler can detect conflicts, redundancies, and unresolved
//! references before emitting the final document.
//!
//! Each entry stores the concrete CDDL value it resolved to (or a marker
//! state such as [`EntryState::Unresolved`] or [`EntryState::Pruned`]).
//!
//! # Write semantics
//!
//! * An entry that does not exist is auto-created as **Unresolved** on the first read via
//!   [`ResolverCache::get`].
//! * [`ResolverCache::resolve`] transitions **Unresolved** → a concrete value variant. If
//!   the entry is already resolved with the *same* value the call returns
//!   [`CacheWriteError::RedundantType`].  If the value differs it returns
//!   [`CacheWriteError::ConflictingType`].
//! * [`ResolverCache::prune`] transitions any state → **Pruned**.
//! * Once **Pruned**, an entry can never be changed.
//! * Passing `Unresolved` or `Pruned` as the target state to [`ResolverCache::resolve`]
//!   is an error.

use std::{collections::HashMap, fmt};

use cbork_abnf_parser::AbnfDocument;

use crate::{
    literals::{byte::ByteLiteralBytes, regex::RegexLiteral, text::TextLiteralBytes},
    node::SourceOrigin,
};

// ---------------------------------------------------------------------------
// Entry state
// ---------------------------------------------------------------------------

/// The compression algorithm attached to a
/// [`EntryState::CompressionAbnf`] payload.
///
/// The names match the corresponding ctlop names (`.x-brotli`, etc.),
/// minus the leading `.` and the `.x-` prefix.  Generic compressed
/// payloads (no algorithm specified) use [`Self::Compressed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionKind {
    /// `.x-compressed` — generic compressed payload (algorithm not specified).
    Compressed,
    /// `.x-brotli` — Brotli compression.
    Brotli,
    /// `.x-zstd` — zstd compression.
    Zstd,
    /// `.x-gzip` — gzip compression.
    Gzip,
    /// `.x-deflate` — deflate compression.
    Deflate,
}

impl CompressionKind {
    /// The ctlop name (with the leading `.`), suitable for diagnostic
    /// messages and `Display` rendering.
    #[must_use]
    pub fn as_text(self) -> &'static str {
        match self {
            Self::Compressed => ".x-compressed",
            Self::Brotli => ".x-brotli",
            Self::Zstd => ".x-zstd",
            Self::Gzip => ".x-gzip",
            Self::Deflate => ".x-deflate",
        }
    }

    /// The ctlop name with the leading `.x-` replaced by the
    /// capitalized form `<Name>`, e.g. `"Brotli"`.  Used in `Display`
    /// implementation for [`EntryState::CompressionAbnf`].
    #[must_use]
    pub fn as_text_upper(self) -> &'static str {
        match self {
            Self::Compressed => "Compressed",
            Self::Brotli => "Brotli",
            Self::Zstd => "Zstd",
            Self::Gzip => "Gzip",
            Self::Deflate => "Deflate",
        }
    }
}

impl fmt::Display for CompressionKind {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(self.as_text())
    }
}

/// A concrete CDDL value or a marker state.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryState {
    /// The name has been seen but no definition has been supplied.
    Unresolved,

    // -- Concrete CDDL value types --
    /// An integer constant.
    Integer(i128),
    /// A floating-point constant.
    Float(f64),
    /// A text string constant (`tstr`).
    Text(TextLiteralBytes),
    /// A byte string constant (`bstr`).
    Bytes(ByteLiteralBytes),
    /// A regular expression (RFC 9741 `regexp`).
    Regex(Box<RegexLiteral>),
    /// An ABNF schema (RFC 9165).
    Abnf(Box<AbnfDocument>),
    /// An encrypted payload annotated with its plaintext ABNF shape.
    EncAbnf(Box<AbnfDocument>),
    /// A hashed payload annotated with its pre-hash ABNF shape.
    HashAbnf(Box<AbnfDocument>),
    /// A compressed payload annotated with its uncompressed ABNF
    /// shape and the compression algorithm kind.  See
    /// [`CompressionKind`].
    CompressionAbnf {
        /// Which compression algorithm annotates this payload.
        kind: CompressionKind,
        /// The pre-compression ABNF document.
        document: Box<AbnfDocument>,
    },
    /// An integer range (`1..10` or `1...10`).
    RangeInt {
        /// `true` for `...` (exclusive upper bound).
        exclusive: bool,
        /// Lower bound.
        min: i128,
        /// Upper bound.
        max: i128,
    },
    /// A floating-point range (`1.0..3.0` or `1.0...3.0`).
    RangeFloat {
        /// `true` for `...` (exclusive upper bound).
        exclusive: bool,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },

    // -- Marker --
    /// The entry was explicitly removed and must never be re-used.
    Pruned,
}

impl Eq for EntryState {}

impl EntryState {
    /// Returns `true` if this state represents a resolved concrete value.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        matches!(
            self,
            EntryState::Integer(_)
                | EntryState::Float(_)
                | EntryState::Text(_)
                | EntryState::Bytes(_)
                | EntryState::Regex(_)
                | EntryState::Abnf(_)
                | EntryState::EncAbnf(_)
                | EntryState::HashAbnf(_)
                | EntryState::CompressionAbnf { .. }
                | EntryState::RangeInt { .. }
                | EntryState::RangeFloat { .. }
        )
    }
}

impl fmt::Display for EntryState {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            EntryState::Unresolved => f.write_str("Unresolved"),
            EntryState::Integer(v) => write!(f, "Integer({v})"),
            EntryState::Float(v) => write!(f, "Float({v})"),
            EntryState::Text(v) => write!(f, "Text({v})"),
            EntryState::Bytes(v) => write!(f, "Bytes({v})"),
            EntryState::Regex(v) => write!(f, "Regex({v})"),
            EntryState::Abnf(v) => write!(f, "ABNF({v})"),
            EntryState::EncAbnf(v) => write!(f, "EncABNF({v})"),
            EntryState::HashAbnf(v) => write!(f, "HashABNF({v})"),
            EntryState::CompressionAbnf { kind, document } => {
                write!(f, "{}ABNF({document})", kind.as_text_upper())
            },
            EntryState::RangeInt {
                exclusive,
                min,
                max,
            } => {
                let op = if *exclusive { "..." } else { ".." };
                write!(f, "RangeInt({min}{op}{max})")
            },
            EntryState::RangeFloat {
                exclusive,
                min,
                max,
            } => {
                let op = if *exclusive { "..." } else { ".." };
                write!(f, "RangeFloat({min}{op}{max})")
            },
            EntryState::Pruned => f.write_str("Pruned"),
        }
    }
}

// ---------------------------------------------------------------------------
// Write errors
// ---------------------------------------------------------------------------

/// Error returned when a write operation on the [`ResolverCache`] fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheWriteError {
    /// The entry is already resolved with a *different* value.
    ConflictingType {
        /// The value already stored in the cache.
        existing: EntryState,
        /// The value that was rejected.
        attempted: EntryState,
    },
    /// The entry is already resolved with the *same* value.
    RedundantType {
        /// The value that was already present.
        value: EntryState,
    },
    /// An attempt was made to explicitly set an entry to Unresolved.
    CannotSetUnresolved,
    /// An attempt was made to modify a pruned entry.
    CannotModifyPruned,
}

impl fmt::Display for CacheWriteError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            CacheWriteError::ConflictingType {
                existing,
                attempted,
            } => {
                write!(
                    f,
                    "conflicting type definition: already {existing}, \
                     cannot re-resolve as {attempted}"
                )
            },
            CacheWriteError::RedundantType { value } => {
                write!(f, "redundant type resolution: {value} already known")
            },
            CacheWriteError::CannotSetUnresolved => {
                f.write_str("cannot explicitly set an entry to Unresolved")
            },
            CacheWriteError::CannotModifyPruned => {
                f.write_str("cannot modify a pruned cache entry")
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Cache entry
// ---------------------------------------------------------------------------

/// A single entry in the resolution cache.
///
/// Wraps an [`EntryState`] so additional metadata (provenance, source file,
/// span) can be added without changing the public API.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The resolved state or marker.
    state: EntryState,
    /// Provenance of the first successful resolution, if any.
    origin: Option<SourceOrigin>,
}

impl CacheEntry {
    /// Construct an unresolved entry.
    fn new_unresolved() -> Self {
        Self {
            state: EntryState::Unresolved,
            origin: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ResolverCache
// ---------------------------------------------------------------------------

/// The resolution cache.
///
/// All fields are private.
#[derive(Debug, Clone)]
pub struct ResolverCache {
    /// The backing map.
    entries: HashMap<String, CacheEntry>,
}

impl ResolverCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    /// Return the current state of an entry, auto-creating it as
    /// [`EntryState::Unresolved`] if it does not exist.
    #[must_use]
    pub fn get(
        &mut self,
        name: &str,
    ) -> &EntryState {
        let entry = self
            .entries
            .entry(name.to_owned())
            .or_insert_with(CacheEntry::new_unresolved);
        &entry.state
    }

    /// Read-only lookup that does not auto-create the entry.
    ///
    /// Use this from renderers and other observers that must not mutate
    /// the cache as a side effect of reading. Returns `None` if the name
    /// has never been touched.
    #[must_use]
    pub fn peek(
        &self,
        name: &str,
    ) -> Option<&EntryState> {
        self.entries.get(name).map(|e| &e.state)
    }

    /// Returns `true` if the entry is [`EntryState::Unresolved`].
    #[must_use]
    pub fn is_unresolved(
        &self,
        name: &str,
    ) -> bool {
        self.entries
            .get(name)
            .is_some_and(|e| matches!(e.state, EntryState::Unresolved))
    }

    /// Returns `true` if the entry holds any resolved concrete value.
    #[must_use]
    pub fn is_resolved(
        &self,
        name: &str,
    ) -> bool {
        self.entries
            .get(name)
            .is_some_and(|e| e.state.is_resolved())
    }

    /// Returns `true` if the entry is [`EntryState::Pruned`].
    #[must_use]
    pub fn is_pruned(
        &self,
        name: &str,
    ) -> bool {
        self.entries
            .get(name)
            .is_some_and(|e| matches!(e.state, EntryState::Pruned))
    }

    /// Number of entries in the [`EntryState::Unresolved`] state.
    #[must_use]
    pub fn cnt_unresolved(&self) -> u64 {
        self.count_by_state(|s| matches!(s, EntryState::Unresolved))
    }

    /// Number of entries holding a resolved concrete value.
    #[must_use]
    pub fn cnt_resolved(&self) -> u64 {
        self.count_by_state(EntryState::is_resolved)
    }

    /// Number of entries in the [`EntryState::Pruned`] state.
    #[must_use]
    pub fn cnt_pruned(&self) -> u64 {
        self.count_by_state(|s| matches!(s, EntryState::Pruned))
    }

    /// Total number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the cache has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Walk every entry.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &EntryState)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), &v.state))
    }

    /// Return the origin associated with an entry, if one has been recorded.
    #[must_use]
    pub fn origin(
        &self,
        name: &str,
    ) -> Option<&SourceOrigin> {
        self.entries
            .get(name)
            .and_then(|entry| entry.origin.as_ref())
    }

    // ------------------------------------------------------------------
    // Write
    // ------------------------------------------------------------------

    /// Resolve a previously-unresolved entry to the given concrete state.
    ///
    /// `new_state` must be a concrete value variant.  Passing
    /// [`EntryState::Unresolved`] or [`EntryState::Pruned`] is an error.
    ///
    /// # Errors
    ///
    /// * [`CacheWriteError::ConflictingType`] — already resolved with a different value.
    /// * [`CacheWriteError::RedundantType`] — already resolved with the same value.
    /// * [`CacheWriteError::CannotSetUnresolved`] — `new_state` is
    ///   [`EntryState::Unresolved`].
    /// * [`CacheWriteError::CannotModifyPruned`] — the entry is pruned or `new_state` is
    ///   [`EntryState::Pruned`].
    pub fn resolve(
        &mut self,
        name: &str,
        new_state: EntryState,
    ) -> Result<(), CacheWriteError> {
        self.resolve_with_origin(name, new_state, None)
    }

    /// Resolve a previously-unresolved entry to the given concrete state
    /// and record its source origin.
    ///
    /// The origin is only stored on the first successful resolution.
    ///
    /// # Errors
    ///
    /// Returns [`CacheWriteError`] on conflict, redundancy, pruned entry,
    /// or attempt to set Unresolved/Pruned.
    pub fn resolve_with_origin(
        &mut self,
        name: &str,
        new_state: EntryState,
        origin: Option<SourceOrigin>,
    ) -> Result<(), CacheWriteError> {
        match &new_state {
            EntryState::Unresolved => return Err(CacheWriteError::CannotSetUnresolved),
            EntryState::Pruned => return Err(CacheWriteError::CannotModifyPruned),
            _ => {},
        }

        let entry = self
            .entries
            .entry(name.to_owned())
            .or_insert_with(CacheEntry::new_unresolved);

        match &entry.state {
            EntryState::Pruned => Err(CacheWriteError::CannotModifyPruned),
            EntryState::Unresolved => {
                entry.state = new_state;
                if entry.origin.is_none() {
                    entry.origin = origin;
                }
                Ok(())
            },
            _ => {
                // The semantic passes revisit the same rule across fixed-point
                // iterations. Re-applying the exact same resolution from the
                // same source origin is idempotent, not a redundant definition.
                if origin.is_some() && entry.origin == origin && entry.state == new_state {
                    return Ok(());
                }

                if entry.state == new_state {
                    Err(CacheWriteError::RedundantType { value: new_state })
                } else {
                    Err(CacheWriteError::ConflictingType {
                        existing: entry.state.clone(),
                        attempted: new_state,
                    })
                }
            },
        }
    }

    /// Mark an entry as pruned.  Idempotent.
    pub fn prune(
        &mut self,
        name: &str,
    ) {
        let entry = self
            .entries
            .entry(name.to_owned())
            .or_insert_with(CacheEntry::new_unresolved);
        entry.state = EntryState::Pruned;
    }

    // ------------------------------------------------------------------
    // Diagnostic dump
    // ------------------------------------------------------------------

    /// Produce a human-readable summary for diagnostic output.
    #[must_use]
    pub fn dump(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();

        let _ = writeln!(out, "ResolverCache ({} entries):", self.entries.len());
        let _ = writeln!(out, "  unresolved: {}", self.cnt_unresolved());
        let _ = writeln!(out, "  resolved:   {}", self.cnt_resolved());
        let _ = writeln!(out, "  pruned:     {}", self.cnt_pruned());

        if self.entries.is_empty() {
            return out;
        }

        let mut names: Vec<&str> = self.entries.keys().map(String::as_str).collect();
        names.sort_unstable();

        let _ = writeln!(out, "  entries:");
        for name in names {
            if let Some(state) = self.entries.get(name).map(|e| &e.state) {
                let _ = writeln!(out, "    {name}: {state}");
            }
        }

        out
    }

    // ------------------------------------------------------------------
    // Private
    // ------------------------------------------------------------------

    /// Count entries matching a predicate.
    fn count_by_state(
        &self,
        pred: impl Fn(&EntryState) -> bool,
    ) -> u64 {
        let mut count: u64 = 0;
        for entry in self.entries.values() {
            if pred(&entry.state) {
                count = count.wrapping_add(1);
            }
        }
        count
    }
}

impl Default for ResolverCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ResolverCache {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(&self.dump())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cache_is_empty() {
        let cache = ResolverCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn get_auto_creates_unresolved() {
        let mut cache = ResolverCache::new();
        let state = cache.get("foo");
        assert_eq!(state, &EntryState::Unresolved);
        assert_eq!(cache.cnt_unresolved(), 1);
    }

    #[test]
    fn get_idempotent() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("bar");
        let state = cache.get("bar");
        assert_eq!(state, &EntryState::Unresolved);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn resolve_integer() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("x");
        cache.resolve("x", EntryState::Integer(42)).unwrap();
        assert!(cache.is_resolved("x"));
        assert_eq!(cache.cnt_resolved(), 1);
        assert!(matches!(cache.get("x"), EntryState::Integer(42)));
    }

    #[test]
    fn resolve_text() {
        let mut cache = ResolverCache::new();
        cache
            .resolve(
                "msg",
                EntryState::Text(TextLiteralBytes::parse(b"\"hello\"").unwrap()),
            )
            .unwrap();
        assert!(matches!(cache.get("msg"), EntryState::Text(_)));
    }

    #[test]
    fn resolve_bytes() {
        let mut cache = ResolverCache::new();
        cache
            .resolve(
                "data",
                EntryState::Bytes(ByteLiteralBytes::parse(b"'hello'").unwrap()),
            )
            .unwrap();
        assert!(matches!(cache.get("data"), EntryState::Bytes(_)));
    }

    #[test]
    fn resolve_float() {
        let mut cache = ResolverCache::new();
        cache
            .resolve("pi", EntryState::Float(std::f64::consts::PI))
            .unwrap();
        assert!(
            matches!(cache.get("pi"), EntryState::Float(f) if (f - std::f64::consts::PI).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn resolve_regex() {
        let mut cache = ResolverCache::new();
        cache
            .resolve(
                "re",
                EntryState::Regex(Box::new(RegexLiteral::parse(b"[a-z]+").unwrap())),
            )
            .unwrap();
        assert!(matches!(cache.get("re"), EntryState::Regex(r) if r.as_ref().source() == "[a-z]+"));
    }

    #[test]
    fn resolve_abnf() {
        let mut cache = ResolverCache::new();
        let document = cbork_abnf_parser::parse_abnf("rule = 1*ALPHA\n").unwrap();
        cache
            .resolve("schema", EntryState::Abnf(Box::new(document.clone())))
            .unwrap();
        assert!(matches!(cache.get("schema"), EntryState::Abnf(a) if a.as_ref() == &document));
    }

    #[test]
    fn resolve_conflicting() {
        let mut cache = ResolverCache::new();
        cache.resolve("x", EntryState::Integer(1)).unwrap();
        let result = cache.resolve("x", EntryState::Integer(2));
        assert!(matches!(
            result,
            Err(CacheWriteError::ConflictingType { .. })
        ));
    }

    #[test]
    fn resolve_redundant() {
        let mut cache = ResolverCache::new();
        cache.resolve("x", EntryState::Integer(1)).unwrap();
        let result = cache.resolve("x", EntryState::Integer(1));
        assert!(matches!(result, Err(CacheWriteError::RedundantType { .. })));
    }

    #[test]
    fn resolve_same_origin_is_idempotent() {
        let mut cache = ResolverCache::new();
        let origin = SourceOrigin::new("test.cddl".into(), 1, 1);
        cache
            .resolve_with_origin("x", EntryState::Integer(1), Some(origin.clone()))
            .unwrap();
        let result = cache.resolve_with_origin("x", EntryState::Integer(1), Some(origin));
        assert!(
            result.is_ok(),
            "same-origin fixed-point replay should be idempotent"
        );
    }

    #[test]
    fn resolve_rejects_unresolved() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("x");
        let result = cache.resolve("x", EntryState::Unresolved);
        assert!(matches!(result, Err(CacheWriteError::CannotSetUnresolved)));
    }

    #[test]
    fn resolve_rejects_pruned() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("x");
        let result = cache.resolve("x", EntryState::Pruned);
        assert!(matches!(result, Err(CacheWriteError::CannotModifyPruned)));
    }

    #[test]
    fn prune_then_cannot_modify() {
        let mut cache = ResolverCache::new();
        cache.resolve("x", EntryState::Integer(1)).unwrap();
        cache.prune("x");
        assert!(cache.is_pruned("x"));
        assert_eq!(cache.cnt_pruned(), 1);

        let result = cache.resolve("x", EntryState::Integer(2));
        assert!(matches!(result, Err(CacheWriteError::CannotModifyPruned)));
    }

    #[test]
    fn prune_idempotent() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("x");
        cache.prune("x");
        cache.prune("x");
        assert!(cache.is_pruned("x"));
    }

    #[test]
    fn iteration_sees_all() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("u");
        cache.resolve("i", EntryState::Integer(7)).unwrap();
        let _ = cache.get("p");
        cache.prune("p");

        let mut seen: Vec<(&str, &EntryState)> = cache.iter().collect();
        seen.sort_by_key(|(k, _)| *k);

        assert_eq!(seen.len(), 3);
        assert!(matches!(seen[0].1, EntryState::Integer(7)));
        assert!(matches!(seen[1].1, EntryState::Pruned));
        assert!(matches!(seen[2].1, EntryState::Unresolved));
    }

    #[test]
    fn counts_accurate() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("u1");
        let _ = cache.get("u2");
        cache.resolve("r1", EntryState::Integer(1)).unwrap();
        cache
            .resolve(
                "r2",
                EntryState::Text(TextLiteralBytes::parse(b"\"hi\"").unwrap()),
            )
            .unwrap();
        let _ = cache.get("p1");
        cache.prune("p1");

        assert_eq!(cache.cnt_unresolved(), 2);
        assert_eq!(cache.cnt_resolved(), 2);
        assert_eq!(cache.cnt_pruned(), 1);
    }

    #[test]
    fn dump_shows_all() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("u");
        cache.resolve("r", EntryState::Integer(99)).unwrap();

        let dump = cache.dump();
        assert!(dump.contains("ResolverCache"));
        assert!(dump.contains("Unresolved"));
        assert!(dump.contains("Integer(99)"));
    }

    #[test]
    fn display_trait() {
        let mut cache = ResolverCache::new();
        let _ = cache.get("x");
        assert!(cache.to_string().contains('x'));
    }

    #[test]
    fn cross_type_conflict() {
        let mut cache = ResolverCache::new();
        cache.resolve("x", EntryState::Integer(1)).unwrap();
        let result = cache.resolve(
            "x",
            EntryState::Text(TextLiteralBytes::parse(b"\"nope\"").unwrap()),
        );
        assert!(matches!(
            result,
            Err(CacheWriteError::ConflictingType { .. })
        ));
    }
}
