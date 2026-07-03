// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Diagnostic rationale registry.
//!
//! Each entry maps a diagnostic code to one or more embedded standards
//! citations that explain why the rule exists.

use crate::rfc::{Citation, DocRange};

/// A diagnostic rationale entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhyEntry {
    /// Stable diagnostic code.
    pub code: &'static str,
    /// Short human-readable summary.
    pub summary: &'static str,
    /// Supporting standards citations.
    pub citations: &'static [Citation],
}

/// RFC citations supporting `W001`.
static W001_CITATIONS: &[Citation] = &[
    Citation::new("rfc8610", &[DocRange::span(2669, 4, 2671, 0)]),
    Citation::new("rfc8610", &[DocRange::new(2623, 2627)]),
];

/// RFC citations supporting `E009`.
static E009_CITATIONS: &[Citation] = &[
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        359, 367,
    )]),
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        414, 435,
    )]),
];

/// RFC citations supporting `E010`.
static E010_CITATIONS: &[Citation] = &[
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        414, 435,
    )]),
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        677, 677,
    )]),
];

/// RFC citations supporting `E011`.
static E011_CITATIONS: &[Citation] = &[
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        359, 367,
    )]),
    Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
        414, 435,
    )]),
];

/// RFC citations supporting `E012`.
static E012_CITATIONS: &[Citation] = &[
    Citation::new("rfc8610", &[DocRange::new(2682, 2684)]),
    Citation::new("rfc9165", &[DocRange::new(264, 303)]),
];

/// RFC citations supporting `E013`.
static E013_CITATIONS: &[Citation] = &[
    Citation::new("rfc8610", &[DocRange::new(2682, 2684)]),
    Citation::new("rfc8610", &[DocRange::new(2669, 2670)]),
];

/// RFC citations supporting `E014`.
static E014_CITATIONS: &[Citation] = &[Citation::new("rfc8610", &[DocRange::span(
    2669, 0, 2671, 40,
)])];

/// RFC citations supporting `E015`.
static E015_CITATIONS: &[Citation] = &[
    Citation::new("rfc8610", &[DocRange::new(1475, 1480)]),
    Citation::new("rfc8610", &[DocRange::new(2104, 2111)]),
];

/// RFC citations supporting `E016`.
static E016_CITATIONS: &[Citation] = &[
    Citation::new("rfc8610", &[DocRange::new(2623, 2627)]),
    Citation::new("rfc8610", &[DocRange::new(2676, 2680)]),
];

/// All diagnostic rationale entries.
static ENTRIES: &[WhyEntry] = &[
    WhyEntry {
        code: "E009",
        summary: "cannot resolve import or include source",
        citations: E009_CITATIONS,
    },
    WhyEntry {
        code: "E010",
        summary: "duplicate or cyclical module import/include",
        citations: E010_CITATIONS,
    },
    WhyEntry {
        code: "E011",
        summary: "included file could not be loaded",
        citations: E011_CITATIONS,
    },
    WhyEntry {
        code: "E012",
        summary: "generic expansion failed",
        citations: E012_CITATIONS,
    },
    WhyEntry {
        code: "E013",
        summary: "plain and generic rule names collide",
        citations: E013_CITATIONS,
    },
    WhyEntry {
        code: "E014",
        summary: "conflicting definition",
        citations: E014_CITATIONS,
    },
    WhyEntry {
        code: "E015",
        summary: "control operator validation failed",
        citations: E015_CITATIONS,
    },
    WhyEntry {
        code: "E016",
        summary: "undefined reference",
        citations: E016_CITATIONS,
    },
    WhyEntry {
        code: "W001",
        summary: "redundant definition",
        citations: W001_CITATIONS,
    },
];

/// Look up a diagnostic rationale by code.
#[must_use]
pub fn find(code: &str) -> Option<&'static WhyEntry> {
    ENTRIES
        .iter()
        .find(|entry| entry.code.eq_ignore_ascii_case(code))
}

/// Return all diagnostic rationale entries.
#[must_use]
pub fn all() -> &'static [WhyEntry] {
    ENTRIES
}
