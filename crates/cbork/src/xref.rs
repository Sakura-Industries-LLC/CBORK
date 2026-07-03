// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Standards cross-reference registry.
//!
//! This maps grammar concepts, control operators, and other user-facing
//! schema terms to authoritative embedded RFC excerpts.

use crate::rfc::{Citation, DocRange};

/// A standards cross-reference entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrefEntry {
    /// Canonical lookup key.
    pub key: &'static str,
    /// Alternate lookup keys.
    pub aliases: &'static [&'static str],
    /// Short description of the entry.
    pub summary: &'static str,
    /// Supporting standards citations.
    pub citations: &'static [Citation],
}

/// All standards cross-reference entries.
static XREF_ENTRIES: &[XrefEntry] = &[
    XrefEntry {
        key: "root-rule",
        aliases: &["first-rule", "top-rule"],
        summary: "the first rule defines the semantics of the specification",
        citations: &[Citation::new("rfc8610", &[DocRange::new(2623, 2627)])],
    },
    XrefEntry {
        key: "rule-definitions",
        aliases: &["rule", "rule-name", "generic-parameters"],
        summary: "rule names can be defined as types or groups, including generic parameters",
        citations: &[Citation::new("rfc8610", &[DocRange::new(2669, 2684)])],
    },
    XrefEntry {
        key: "include-import",
        aliases: &["modules", "include", "import"],
        summary: "module directives may include or import named rules, optionally with aliases",
        citations: &[
            Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
                359, 367,
            )]),
            Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
                414, 435,
            )]),
            Citation::new("draft-ietf-cbor-cddl-modules-06", &[DocRange::new(
                677, 677,
            )]),
        ],
    },
    XrefEntry {
        key: ".size",
        aliases: &["size"],
        summary: "size control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1484, 1505)])],
    },
    XrefEntry {
        key: ".bits",
        aliases: &["bits"],
        summary: "bit-string control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1526, 1549)])],
    },
    XrefEntry {
        key: ".regexp",
        aliases: &["regexp", "regex"],
        summary: "regular-expression control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1575, 1597)])],
    },
    XrefEntry {
        key: ".cbor",
        aliases: &["cbor"],
        summary: "embedded CBOR payload control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1646, 1655)])],
    },
    XrefEntry {
        key: ".cborseq",
        aliases: &["cborseq"],
        summary: "embedded CBOR sequence control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1646, 1660)])],
    },
    XrefEntry {
        key: ".within",
        aliases: &["within"],
        summary: "constraint control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1667, 1706)])],
    },
    XrefEntry {
        key: ".and",
        aliases: &["and"],
        summary: "intersection control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1667, 1706)])],
    },
    XrefEntry {
        key: ".lt",
        aliases: &["lt"],
        summary: "less-than control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".le",
        aliases: &["le"],
        summary: "less-than-or-equal control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".gt",
        aliases: &["gt"],
        summary: "greater-than control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".ge",
        aliases: &["ge"],
        summary: "greater-than-or-equal control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".eq",
        aliases: &["eq"],
        summary: "equality control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".ne",
        aliases: &["ne"],
        summary: "inequality control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1710, 1758)])],
    },
    XrefEntry {
        key: ".default",
        aliases: &["default"],
        summary: "default-value control operator",
        citations: &[Citation::new("rfc8610", &[DocRange::new(1748, 1758)])],
    },
    XrefEntry {
        key: ".sdnv",
        aliases: &["sdnv"],
        summary: "SDNV control operator",
        citations: &[Citation::new("rfc9090", &[DocRange::new(453, 470)])],
    },
    XrefEntry {
        key: ".sdnvseq",
        aliases: &["sdnvseq"],
        summary: "SDNV sequence control operator",
        citations: &[Citation::new("rfc9090", &[DocRange::new(453, 476)])],
    },
    XrefEntry {
        key: ".oid",
        aliases: &["oid"],
        summary: "OID control operator",
        citations: &[Citation::new("rfc9090", &[DocRange::new(462, 482)])],
    },
    XrefEntry {
        key: ".plus",
        aliases: &["plus"],
        summary: "numeric addition control operator",
        citations: &[Citation::new("rfc9165", &[DocRange::new(125, 176)])],
    },
    XrefEntry {
        key: ".cat",
        aliases: &["cat"],
        summary: "string concatenation control operator",
        citations: &[Citation::new("rfc9165", &[DocRange::new(177, 218)])],
    },
    XrefEntry {
        key: ".det",
        aliases: &["det"],
        summary: "dedenting concatenation control operator",
        citations: &[Citation::new("rfc9165", &[DocRange::new(219, 252)])],
    },
    XrefEntry {
        key: ".abnf",
        aliases: &["abnf"],
        summary: "ABNF controller on text strings",
        citations: &[Citation::new("rfc9165", &[DocRange::new(264, 313)])],
    },
    XrefEntry {
        key: ".abnfb",
        aliases: &["abnfb"],
        summary: "ABNF controller on byte strings",
        citations: &[Citation::new("rfc9165", &[DocRange::new(264, 313)])],
    },
    XrefEntry {
        key: ".feature",
        aliases: &["feature"],
        summary: "feature annotation control operator",
        citations: &[Citation::new("rfc9165", &[DocRange::new(363, 422)])],
    },
    XrefEntry {
        key: ".b64u",
        aliases: &["b64u"],
        summary: "base64url text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(84, 115)])],
    },
    XrefEntry {
        key: ".b64c",
        aliases: &["b64c"],
        summary: "base64 classic text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(84, 115)])],
    },
    XrefEntry {
        key: ".b64u-sloppy",
        aliases: &["b64u-sloppy"],
        summary: "sloppy base64url text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(91, 115)])],
    },
    XrefEntry {
        key: ".b64c-sloppy",
        aliases: &["b64c-sloppy"],
        summary: "sloppy base64 classic text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(91, 115)])],
    },
    XrefEntry {
        key: ".b45",
        aliases: &["b45"],
        summary: "Base45 text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(100, 115)])],
    },
    XrefEntry {
        key: ".b32",
        aliases: &["b32"],
        summary: "Base32 text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(100, 115)])],
    },
    XrefEntry {
        key: ".h32",
        aliases: &["h32"],
        summary: "base32hex text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(100, 115)])],
    },
    XrefEntry {
        key: ".hex",
        aliases: &["hex"],
        summary: "hex text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(97, 115)])],
    },
    XrefEntry {
        key: ".hexlc",
        aliases: &["hexlc"],
        summary: "lowercase hex text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(97, 115)])],
    },
    XrefEntry {
        key: ".hexuc",
        aliases: &["hexuc"],
        summary: "uppercase hex text-to-bytes control operator",
        citations: &[Citation::new("rfc9741", &[DocRange::new(97, 115)])],
    },
    XrefEntry {
        key: ".base10",
        aliases: &["base10"],
        summary: "decimal text-to-integer control operator",
        citations: &[Citation::new("rfc9741", &[
            DocRange::new(106, 115),
            DocRange::new(243, 261),
        ])],
    },
    XrefEntry {
        key: ".printf",
        aliases: &["printf"],
        summary: "printf-style text formatting control operator",
        citations: &[Citation::new("rfc9741", &[
            DocRange::new(108, 115),
            DocRange::new(274, 307),
        ])],
    },
    XrefEntry {
        key: ".json",
        aliases: &["json"],
        summary: "JSON text-to-any control operator",
        citations: &[Citation::new("rfc9741", &[
            DocRange::new(111, 115),
            DocRange::new(317, 343),
        ])],
    },
    XrefEntry {
        key: ".join",
        aliases: &["join"],
        summary: "array join control operator",
        citations: &[Citation::new("rfc9741", &[
            DocRange::new(114, 115),
            DocRange::new(362, 407),
        ])],
    },
    XrefEntry {
        key: ".prefp",
        aliases: &["prefp"],
        summary: "preferred-plus serialization control operator",
        citations: &[Citation::new("draft-ietf-cbor-serialization-06", &[
            DocRange::new(8, 9),
            DocRange::new(16, 21),
        ])],
    },
    XrefEntry {
        key: ".prefpseq",
        aliases: &["prefpseq"],
        summary: "preferred-plus CBOR sequence serialization control operator",
        citations: &[Citation::new("draft-ietf-cbor-serialization-06", &[
            DocRange::new(8, 9),
            DocRange::new(16, 21),
        ])],
    },
    XrefEntry {
        key: ".dtrm",
        aliases: &["dtrm"],
        summary: "deterministic serialization control operator",
        citations: &[Citation::new("draft-ietf-cbor-serialization-06", &[
            DocRange::new(8, 9),
            DocRange::new(16, 21),
            DocRange::new(243, 265),
        ])],
    },
    XrefEntry {
        key: ".dtrmseq",
        aliases: &["dtrmseq"],
        summary: "deterministic CBOR sequence serialization control operator",
        citations: &[Citation::new("draft-ietf-cbor-serialization-06", &[
            DocRange::new(8, 9),
            DocRange::new(16, 21),
            DocRange::new(243, 265),
        ])],
    },
];

/// Find all xref entries matching a query.
#[must_use]
pub fn find(query: &str) -> Vec<&'static XrefEntry> {
    let key = normalize(query);
    let exact: Vec<_> = XREF_ENTRIES
        .iter()
        .filter(|entry| entry_matches(entry, &key))
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    XREF_ENTRIES
        .iter()
        .filter(|entry| {
            normalize(entry.key).contains(&key)
                || entry
                    .aliases
                    .iter()
                    .any(|alias| normalize(alias).contains(&key))
                || normalize(entry.summary).contains(&key)
        })
        .collect()
}

/// Return all known xref entries.
#[must_use]
pub fn all() -> &'static [XrefEntry] {
    XREF_ENTRIES
}

/// Return `true` when a query matches a canonical key or alias.
fn entry_matches(
    entry: &XrefEntry,
    key: &str,
) -> bool {
    normalize(entry.key) == key || entry.aliases.iter().any(|alias| normalize(alias) == key)
}

/// Normalize a lookup key for comparison.
fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '.')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}
