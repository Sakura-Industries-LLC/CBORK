// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Embedded standards corpus used by `cbork`.
//!
//! All text is compiled into the binary with `include_str!` so the tool
//! remains self-contained at runtime.

use std::{fmt::Write as _, ops::RangeInclusive};

/// A standards document embedded into the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedDoc {
    /// Stable identifier used by the CLI and lookup tables.
    pub id: &'static str,
    /// Human-readable title.
    pub title: &'static str,
    /// Embedded source text.
    pub text: &'static str,
}

/// A line range inside an embedded document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocRange {
    /// One-based inclusive start line.
    pub start: usize,
    /// One-based inclusive start column.
    pub start_column: usize,
    /// One-based inclusive end line.
    pub end: usize,
    /// One-based inclusive end column, or `0` for end-of-line.
    pub end_column: usize,
}

impl DocRange {
    /// Create a new line range.
    #[must_use]
    pub const fn new(
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            start,
            start_column: 1,
            end,
            end_column: 0,
        }
    }

    /// Create a new line-and-column range.
    #[must_use]
    pub const fn span(
        start: usize,
        start_column: usize,
        end: usize,
        end_column: usize,
    ) -> Self {
        Self {
            start,
            start_column,
            end,
            end_column,
        }
    }

    /// Render the range in `start-end` form.
    #[must_use]
    pub fn display(self) -> String {
        if self.start == self.end {
            self.start.to_string()
        } else {
            format!("{}-{}", self.start, self.end)
        }
    }

    /// Convert to a standard inclusive range.
    #[must_use]
    pub fn as_range(self) -> RangeInclusive<usize> {
        self.start..=self.end
    }
}

/// A citation into an embedded standards document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Citation {
    /// Referenced document.
    pub doc: &'static str,
    /// Referenced line ranges.
    pub ranges: &'static [DocRange],
}

impl Citation {
    /// Create a citation from a document and line ranges.
    #[must_use]
    pub const fn new(
        doc: &'static str,
        ranges: &'static [DocRange],
    ) -> Self {
        Self { doc, ranges }
    }
}

/// RFC 8610.
pub const RFC8610: EmbeddedDoc = EmbeddedDoc {
    id: "rfc8610",
    title: "Concise Data Definition Language (CDDL)",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc8610.txt"
    )),
};

/// RFC 8742.
pub const RFC8742: EmbeddedDoc = EmbeddedDoc {
    id: "rfc8742",
    title: "Additional CBOR Control Operators",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc8742.txt"
    )),
};

/// RFC 9090.
pub const RFC9090: EmbeddedDoc = EmbeddedDoc {
    id: "rfc9090",
    title: "CBOR Tags for Serializations",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc9090.txt"
    )),
};

/// RFC 9165.
pub const RFC9165: EmbeddedDoc = EmbeddedDoc {
    id: "rfc9165",
    title: "CDDL Control Operators for ABNF, plus, cat, det, and feature",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc9165.txt"
    )),
};

/// RFC 9682.
pub const RFC9682: EmbeddedDoc = EmbeddedDoc {
    id: "rfc9682",
    title: "Concise Binary Object Representation (CBOR) Packed Serialization",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc9682.txt"
    )),
};

/// RFC 9741.
pub const RFC9741: EmbeddedDoc = EmbeddedDoc {
    id: "rfc9741",
    title: "CDDL Control Operators for Text and Byte String Transformations",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/rfc9741.txt"
    )),
};

/// Draft modules document.
pub const DRAFT_CDDL_MODULES_06: EmbeddedDoc = EmbeddedDoc {
    id: "draft-ietf-cbor-cddl-modules-06",
    title: "CDDL Modules",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/draft-ietf-cbor-cddl-modules-06.txt"
    )),
};

/// Draft EDN literals document.
pub const DRAFT_EDN_LITERALS_25: EmbeddedDoc = EmbeddedDoc {
    id: "draft-ietf-cbor-edn-literals-25",
    title: "CBOR Extended Diagnostic Notation Literals",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/draft-ietf-cbor-edn-literals-25.txt"
    )),
};

/// Draft serialization document.
pub const DRAFT_SERIALIZATION_06: EmbeddedDoc = EmbeddedDoc {
    id: "draft-ietf-cbor-serialization-06",
    title: "CBOR Serialization",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/draft-ietf-cbor-serialization-06.txt"
    )),
};

/// STD94 / RFC 8949.
pub const STD94_RFC8949: EmbeddedDoc = EmbeddedDoc {
    id: "std94-rfc8949",
    title: "Concise Binary Object Representation (CBOR)",
    text: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rfc/std94-rfc8949.txt"
    )),
};

/// All embedded standards documents.
pub const DOCS: &[EmbeddedDoc] = &[
    RFC8610,
    RFC8742,
    RFC9090,
    RFC9165,
    RFC9682,
    RFC9741,
    DRAFT_CDDL_MODULES_06,
    DRAFT_EDN_LITERALS_25,
    DRAFT_SERIALIZATION_06,
    STD94_RFC8949,
];

/// Find an embedded document by identifier.
#[must_use]
pub fn find_doc(id: &str) -> Option<&'static EmbeddedDoc> {
    let key = normalize(id);
    DOCS.iter()
        .find(|doc| normalize(doc.id) == key)
        .or_else(|| DOCS.iter().find(|doc| normalize(doc.title) == key))
}

/// Return all embedded standards documents.
#[must_use]
pub fn all_docs() -> &'static [EmbeddedDoc] {
    DOCS
}

/// Render a list of embedded document identifiers.
#[must_use]
pub fn render_doc_list() -> String {
    let mut out = String::new();
    for doc in all_docs() {
        let _ = writeln!(out, "{:<32} {}", doc.id, doc.title);
    }
    out
}

/// Render an embedded RFC excerpt for a citation.
#[must_use]
pub fn render_citation(
    doc: &EmbeddedDoc,
    range: DocRange,
    heading: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "  {heading} {} [{}]:",
        doc.id.to_uppercase(),
        range.display()
    );
    let lines: Vec<_> = doc.text.lines().collect();
    for line_number in range.as_range() {
        if let Some(line) = lines.get(line_number.saturating_sub(1)) {
            let rendered = if line_number == range.start && line_number == range.end {
                slice_line(line, range.start_column, range.end_column)
            } else if line_number == range.start {
                slice_line(line, range.start_column, 0)
            } else if line_number == range.end {
                slice_line(line, 1, range.end_column)
            } else {
                (*line).to_owned()
            };
            let _ = writeln!(out, "    {rendered}");
        }
    }
    out
}

/// Render one or more citations grouped under a common heading.
#[must_use]
pub fn render_citations(
    heading: &str,
    citations: &[Citation],
) -> String {
    let mut out = String::new();
    for citation in citations {
        if let Some(doc) = find_doc(citation.doc) {
            for range in citation.ranges {
                out.push_str(&render_citation(doc, *range, heading));
            }
        }
    }
    out
}

/// Render the full embedded document as raw text.
#[must_use]
pub fn render_doc(id: &str) -> Option<&'static str> {
    find_doc(id).map(|doc| doc.text)
}

/// Normalize a document lookup key.
fn normalize(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// Slice a single line by 1-based inclusive column positions.
fn slice_line(
    line: &str,
    start_column: usize,
    end_column: usize,
) -> String {
    let chars: Vec<char> = line.chars().collect();
    let start = start_column.saturating_sub(1).min(chars.len());
    let end = if end_column == 0 {
        chars.len()
    } else {
        end_column.min(chars.len())
    };

    chars
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DocRange, RFC8610, render_citation};

    #[test]
    fn citation_rendering_trims_to_hidden_columns() {
        let rendered = render_citation(&RFC8610, DocRange::span(2669, 4, 2671, 0), "WHY");

        assert!(rendered.contains("WHY RFC8610 [2669-2671]:"));
        assert!(
            rendered
                .contains("A plain equals sign defines the rule name as the equivalent of the",)
        );
        assert!(rendered.contains("defined with a different expression."));
    }
}
