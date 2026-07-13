// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! `.within` and `.and` control-operator validation.
//!
//! Implements structural subtype checking for the `.within` control operator
//! (RFC 8610 §3.8.5) and lays the foundation for `.and` intersection
//! validation.
//!
//! # Architecture
//!
//! * [`ResolvedType`] — a normalized, recursion-free representation of CDDL type
//!   structures, decoupled from the raw parse tree.
//! * [`resolve_type()`] — converts a [`WrappedNode`] type subtree into a
//!   [`ResolvedType`].
//! * [`is_subtype()`] — the core structural subtype checker (Stage 3).

use std::collections::{HashMap, HashSet};

use crate::{
    WrappedNode,
    concrete::{self, ConcretePolicy, ResolutionMap},
    error::{Diagnostic, DiagnosticLevel, Subdiag, SubdiagKind},
    node::SourceOrigin,
    schema_diff::{self, SchemaDiffKind},
    symbols::{AssignmentKind, rule_head_from_children},
};

/// Borrow a node's source text for debug/error formatting.
fn text_of_for_debug(node: &WrappedNode) -> &str {
    match node {
        WrappedNode::RuleLine { text, .. }
        | WrappedNode::Syntax { text, .. }
        | WrappedNode::Comment { text, .. }
        | WrappedNode::ModuleStart { text, .. }
        | WrappedNode::ModuleEnd { text, .. } => text,
        WrappedNode::Directive { source_comment, .. } => source_comment,
    }
}

/// Bundled context passed through every within-pass call. Keeps the
/// per-call argument list short and the lifetime relationships
/// explicit.
struct WithinContext<'a> {
    /// Map of top-level definitions used by the subtype checker.
    defs: &'a DefinitionMap,
    /// Concrete-resolution map for rendering LHS/RHS snippets.
    resolution: &'a ResolutionMap,
    /// Concrete-renderer policy for rendering LHS/RHS snippets.
    policy: &'a ConcretePolicy,
    /// The full node tree the within pass is walking. Used to
    /// collect socket augmentations that target the LHS.
    all_nodes: &'a [WrappedNode],
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Schema-relevant control operators recognized by `ResolvedType`.
///
/// Operators not listed here are tracked as `Other(name)` so that the
/// subtype checker can still carry them through resolution without losing
/// information.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ControlOp {
    /// `.cbor` — CBOR encoding refinement of a byte string.
    Cbor,
    /// `.prefp` — preferred-plus CBOR encoding refinement of a byte string.
    Prefp,
    /// `.dtrm` — deterministic CBOR encoding refinement of a byte string.
    Dtrm,
    /// `.cborseq` — sequence of CBOR-encoded items.
    CborSeq,
    /// `.prefpseq` — sequence of preferred-plus CBOR-encoded items.
    PrefpSeq,
    /// `.dtrmseq` — sequence of deterministically CBOR-encoded items.
    DtrmSeq,
    /// `.and` — schema intersection.
    And,
    /// `.within` — subtype constraint.
    Within,
    /// `.size` — byte/text length bound.
    Size,
    /// `.bits` — bit-level layout bound.
    Bits,
    /// `.gt` — strict greater-than numeric bound. Narrows the carrier
    /// range.
    Gt,
    /// `.ge` — greater-than-or-equal numeric bound. Narrows the carrier
    /// range.
    Ge,
    /// `.lt` — strict less-than numeric bound. Narrows the carrier
    /// range.
    Lt,
    /// `.le` — less-than-or-equal numeric bound. Narrows the carrier
    /// range.
    Le,
    /// `.x-enc` — unofficial annotation: the byte string holds the
    /// encryption of the controller.  Narrows the carrier `bstr`.
    XEnc,
    /// `.x-hash` — unofficial annotation: the byte string holds the
    /// hash of the controller.  Narrows the carrier `bstr`.
    XHash,
    /// `.x-compressed` — unofficial annotation: the byte string holds a
    /// compressed form of the controller, using an unspecified
    /// algorithm.  Narrows the carrier `bstr`.  This is the generic
    /// parent of [`Self::XBrotli`], [`Self::XZstd`], [`Self::XGzip`],
    /// and [`Self::XDeflate`].
    XCompressed,
    /// `.x-brotli` — unofficial annotation: the byte string holds a
    /// Brotli-compressed form of the controller.  Narrows the carrier
    /// `bstr`.  Is within [`Self::XCompressed`].
    XBrotli,
    /// `.x-zstd` — unofficial annotation: the byte string holds a
    /// zstd-compressed form of the controller.  Narrows the carrier
    /// `bstr`.  Is within [`Self::XCompressed`].
    XZstd,
    /// `.x-gzip` — unofficial annotation: the byte string holds a
    /// gzip-compressed form of the controller.  Narrows the carrier
    /// `bstr`.  Is within [`Self::XCompressed`].
    XGzip,
    /// `.x-deflate` — unofficial annotation: the byte string holds a
    /// deflate-compressed form of the controller.  Narrows the carrier
    /// `bstr`.  Is within [`Self::XCompressed`].
    XDeflate,
    /// Any other control operator text not normalized above.
    Other(String),
}

impl ControlOp {
    /// Render the canonical operator text (with the leading `.`).
    #[must_use]
    pub(crate) fn as_text(&self) -> &str {
        match self {
            Self::Cbor => ".cbor",
            Self::Prefp => ".prefp",
            Self::Dtrm => ".dtrm",
            Self::CborSeq => ".cborseq",
            Self::PrefpSeq => ".prefpseq",
            Self::DtrmSeq => ".dtrmseq",
            Self::And => ".and",
            Self::Within => ".within",
            Self::Size => ".size",
            Self::Bits => ".bits",
            Self::Gt => ".gt",
            Self::Ge => ".ge",
            Self::Lt => ".lt",
            Self::Le => ".le",
            Self::XEnc => ".x-enc",
            Self::XHash => ".x-hash",
            Self::XCompressed => ".x-compressed",
            Self::XBrotli => ".x-brotli",
            Self::XZstd => ".x-zstd",
            Self::XGzip => ".x-gzip",
            Self::XDeflate => ".x-deflate",
            Self::Other(s) => s.as_str(),
        }
    }

    /// Normalize a control-operator text fragment into a [`ControlOp`].
    ///
    /// The input is expected to be the trimmed text of a `ctlop` node
    /// (including the leading `.`).
    #[must_use]
    pub(crate) fn from_text(text: &str) -> Self {
        match text.trim() {
            ".cbor" => Self::Cbor,
            ".prefp" => Self::Prefp,
            ".dtrm" => Self::Dtrm,
            ".cborseq" => Self::CborSeq,
            ".prefpseq" => Self::PrefpSeq,
            ".dtrmseq" => Self::DtrmSeq,
            ".and" => Self::And,
            ".within" => Self::Within,
            ".size" => Self::Size,
            ".bits" => Self::Bits,
            ".gt" => Self::Gt,
            ".ge" => Self::Ge,
            ".lt" => Self::Lt,
            ".le" => Self::Le,
            // BUG-010: the `.abnf` / `.abnfb` annotated forms collapse
            // to the same `ControlOp` as the base operator for
            // `.within` subtype checks.  The carrier narrowing
            // behavior is identical and the controller-comparison
            // rules treat them the same way.  This is already true
            // for the compression family; the same normalization
            // must apply to `.x-enc` and `.x-hash`, otherwise the
            // `.x-enc.abnfb` form falls through as `Other(...)` and
            // the subtype checker rejects it structurally against
            // plain `Bstr` / `Nil` instead of using the carrier.
            ".x-enc" | ".x-enc.abnf" | ".x-enc.abnfb" => Self::XEnc,
            ".x-hash" | ".x-hash.abnf" | ".x-hash.abnfb" => Self::XHash,
            ".x-compressed" | ".x-compressed.abnf" | ".x-compressed.abnfb" => Self::XCompressed,
            ".x-brotli" | ".x-brotli.abnf" | ".x-brotli.abnfb" => Self::XBrotli,
            ".x-zstd" | ".x-zstd.abnf" | ".x-zstd.abnfb" => Self::XZstd,
            ".x-gzip" | ".x-gzip.abnf" | ".x-gzip.abnfb" => Self::XGzip,
            ".x-deflate" | ".x-deflate.abnf" | ".x-deflate.abnfb" => Self::XDeflate,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Whether this operator can be collapsed to its carrier type without
    /// losing information needed for subtype checking.
    ///
    /// Operators such as `.cbor`, `.dtrm`, `.cborseq`, `.dtrmseq`, `.and`,
    /// `.within`, `.size`, `.bits`, `.gt`, `.ge`, `.lt`, `.le`, `.x-enc`,
    /// `.x-hash`, the compression annotation family (`.x-compressed`,
    /// `.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate`) are narrower
    /// than the carrier and must be preserved; only operators that are
    /// syntactically important but semantically transparent may
    /// collapse.
    #[must_use]
    pub(crate) fn is_schema_relevant(&self) -> bool {
        matches!(
            self,
            Self::Cbor
                | Self::Prefp
                | Self::Dtrm
                | Self::CborSeq
                | Self::PrefpSeq
                | Self::DtrmSeq
                | Self::And
                | Self::Within
                | Self::Size
                | Self::Bits
                | Self::Gt
                | Self::Ge
                | Self::Lt
                | Self::Le
                | Self::XEnc
                | Self::XHash
                | Self::XCompressed
                | Self::XBrotli
                | Self::XZstd
                | Self::XGzip
                | Self::XDeflate
        ) || matches!(self, Self::Other(_))
    }

    /// Whether this operator is known to *narrow* its carrier.
    ///
    /// A narrowing operator `op` means `Control(op, T, _) ⊆ T` (and
    /// therefore `Control(op, T, _) ⊆ R` whenever `T ⊆ R`). The set
    /// covers numeric range refinements (`.gt`/`.ge`/`.lt`/`.le`),
    /// length refinements (`.size`), bit-layout refinements (`.bits`),
    /// CBOR encoding refinements (`.cbor`/`.cborseq` and the stricter
    /// `.dtrm`/`.dtrmseq`), the unofficial encryption / hash
    /// annotation wrappers (`.x-enc`/`.x-hash`), and the unofficial
    /// compression annotation wrappers (`.x-compressed`, `.x-brotli`,
    /// `.x-zstd`, `.x-gzip`, `.x-deflate`) — all of which always
    /// produce a `bstr` regardless of the controller.
    ///
    /// Excludes `.and` and `.within` — those are *relations*, not
    /// narrowings. Excludes `ControlOp::Other(_)` because the narrowing
    /// property is only proven for the named set.
    #[must_use]
    pub(crate) fn is_narrowing(&self) -> bool {
        matches!(
            self,
            Self::Gt
                | Self::Ge
                | Self::Lt
                | Self::Le
                | Self::Size
                | Self::Bits
                | Self::Cbor
                | Self::CborSeq
                | Self::Prefp
                | Self::PrefpSeq
                | Self::Dtrm
                | Self::DtrmSeq
                | Self::XEnc
                | Self::XHash
                | Self::XCompressed
                | Self::XBrotli
                | Self::XZstd
                | Self::XGzip
                | Self::XDeflate
        )
    }

    /// Classify a compression-annotation operator.
    ///
    /// Returns `true` for the generic, algorithm-agnostic
    /// `.x-compressed`, and `false` for each of the algorithm-named
    /// variants (`.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate`).
    /// Returns `false` for non-compression operators.
    #[must_use]
    pub(crate) fn is_compression_generic(&self) -> bool {
        matches!(self, Self::XCompressed)
    }

    /// Classify a compression-annotation operator as a named algorithm.
    ///
    /// Returns `true` for `.x-brotli`, `.x-zstd`, `.x-gzip`, and
    /// `.x-deflate`.  Returns `false` for `.x-compressed` and for
    /// non-compression operators.
    #[must_use]
    pub(crate) fn is_compression_named(&self) -> bool {
        matches!(
            self,
            Self::XBrotli | Self::XZstd | Self::XGzip | Self::XDeflate
        )
    }

    /// Whether this operator is the unofficial encryption wrapper
    /// `.x-enc`.  Step 5.11 gives the wrapper its own transform
    /// family so it cannot be within `.x-hash`, `.x-compressed`, or
    /// any named compression algorithm.
    #[must_use]
    pub(crate) fn is_encryption(&self) -> bool {
        matches!(self, Self::XEnc)
    }

    /// Whether this operator is the unofficial hash wrapper `.x-hash`.
    /// Step 5.11 gives the wrapper its own transform family so it
    /// cannot be within `.x-enc`, `.x-compressed`, or any named
    /// compression algorithm.
    #[must_use]
    pub(crate) fn is_hash_annotation(&self) -> bool {
        matches!(self, Self::XHash)
    }
}

/// A normalized, syntax-free representation of a CDDL type structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedType {
    /// Any type (`any = #` from the postlude).
    Any,
    /// A primitive leaf type: `int`, `tstr`, `bool`, etc.
    Primitive(PrimitiveKind),
    /// A numeric range: `0..255`, `1.5..3.0`
    Range {
        /// Lower bound, if specified.
        lo: Option<i128>,
        /// Upper bound, if specified.
        hi: Option<i128>,
        /// Whether this is a float range.
        is_float: bool,
    },
    /// A tagged CBOR value: `#6.123(inner)`
    Tag {
        /// The CBOR tag number.
        tag: u64,
        /// The inner type wrapped by the tag.
        inner: Box<ResolvedType>,
    },
    /// An array type: `[t1, t2]`
    Array {
        /// Ordered element types with occurrence specifiers.
        elements: Vec<ArrayElement>,
    },
    /// A map/group type: `{ k1 => v1, k2 => v2 }`
    Map {
        /// Map entries with key, value, and occurrence.
        entries: Vec<MapEntry>,
    },
    /// A type choice: `A / B / C`
    Choice(Vec<ResolvedType>),
    /// A socket reference: `$message` (choices accumulated separately).
    Socket {
        /// The socket name including the leading `$`.
        name: String,
    },
    /// A control operator applied to a type: `bstr .cbor payload`.
    ///
    /// The `carrier` is the type the operator is applied to (e.g. `bstr`),
    /// and the `controller` is the operand that constrains the carrier
    /// (e.g. `payload`).
    Control {
        /// The control operator in use.
        op: ControlOp,
        /// The type the operator is applied to.
        carrier: Box<ResolvedType>,
        /// The operand that constrains the carrier.
        controller: Box<ResolvedType>,
    },
    /// A schema intersection: `A .and B`.
    ///
    /// In CDDL, `.and` is schema intersection, not boolean logic. The
    /// resulting type only accepts values that satisfy every operand.
    /// This is a dedicated variant (rather than `ControlOp::And`)
    /// because intersection has its own subtype rules that are simpler
    /// to express with a flat list of operands.
    Intersection(Vec<ResolvedType>),
    /// A named type reference that has not (yet) been resolved.
    Named(String),
    /// BUG-009: a concrete text-label value used as a bareword map
    /// member key (`foo:` in `{ foo: T }`).  A bareword key names a
    /// single concrete text string, not a type reference, so the
    /// subtype checker must accept any `TextKey` as a subtype of
    /// `tstr` and as a valid alternative in a key choice.
    TextKey(String),
}

/// CDDL primitive type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PrimitiveKind {
    /// `int` (integer, signed or unsigned)
    Int,
    /// `uint` (unsigned integer)
    Uint,
    /// `nint` (negative integer)
    Nint,
    /// `tstr` (text string)
    Tstr,
    /// `bstr` (byte string)
    Bstr,
    /// `bool` (boolean)
    Bool,
    /// `nil` / `null`
    Nil,
    /// `float` (generic float)
    Float,
    /// `float16` (half-precision float)
    Float16,
    /// `float32` (single-precision float)
    Float32,
    /// `float64` (double-precision float)
    Float64,
    /// `undefined`
    Undefined,
}

/// Occurrence specifier for array elements and map entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Occurrence {
    /// Exactly one (no prefix, the default).
    One,
    /// Zero or one (`?` prefix).
    Optional,
    /// Zero or more (`*` prefix).
    ZeroOrMore,
    /// One or more (`+` prefix).
    OneOrMore,
    /// Exact count range (`n*m` where n ≤ m).
    Range {
        /// Minimum count.
        lo: u32,
        /// Maximum count.
        hi: u32,
    },
}

impl Occurrence {
    /// Minimum number of elements this occurrence allows.
    #[must_use]
    fn min(&self) -> u32 {
        match self {
            Self::One | Self::OneOrMore => 1,
            Self::Optional | Self::ZeroOrMore => 0,
            Self::Range { lo, .. } => *lo,
        }
    }

    /// Maximum number of elements this occurrence allows.
    /// Returns `None` for unbounded occurrences.
    #[must_use]
    fn max(&self) -> Option<u32> {
        match self {
            Self::One | Self::Optional => Some(1),
            Self::ZeroOrMore | Self::OneOrMore => None,
            Self::Range { hi, .. } => Some(*hi),
        }
    }
}

/// A single element in an array type, with its occurrence specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArrayElement {
    /// The element type.
    pub ty: ResolvedType,
    /// The occurrence specifier for this element.
    pub occurrence: Occurrence,
}

/// A single entry in a map type, with key, value, and occurrence specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MapEntry {
    /// The key type (or specific key value).
    pub key: ResolvedType,
    /// The value type.
    pub value: ResolvedType,
    /// The occurrence specifier for this entry.
    pub occurrence: Occurrence,
}

// ---------------------------------------------------------------------------
// Type resolution
// ---------------------------------------------------------------------------

/// Well-known primitive type names (postlude-defined or RFC 8610 built-ins).
fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "uint"
            | "nint"
            | "int"
            | "tstr"
            | "text"
            | "bstr"
            | "bytes"
            | "bool"
            | "nil"
            | "null"
            | "float"
            | "float16"
            | "float32"
            | "float64"
            | "tdate"
            | "time"
            | "number"
            | "eb64url"
            | "eb64legacy"
            | "eb16"
            | "undefined"
    )
}

/// Map a well-known primitive name to its [`PrimitiveKind`].
fn primitive_kind_for_name(name: &str) -> Option<PrimitiveKind> {
    match name {
        "uint" => Some(PrimitiveKind::Uint),
        "nint" => Some(PrimitiveKind::Nint),
        "int" | "integer" => Some(PrimitiveKind::Int),
        "tstr" | "text" => Some(PrimitiveKind::Tstr),
        "bstr" | "bytes" => Some(PrimitiveKind::Bstr),
        "bool" | "boolean" => Some(PrimitiveKind::Bool),
        "nil" | "null" => Some(PrimitiveKind::Nil),
        "float" | "float64" | "number" => Some(PrimitiveKind::Float),
        "float16" => Some(PrimitiveKind::Float16),
        "float32" => Some(PrimitiveKind::Float32),
        "undefined" => Some(PrimitiveKind::Undefined),
        "tdate" | "time" | "eb64url" | "eb64legacy" | "eb16" => {
            // These are postlude types that wrap other primitives.
            // Treat them as opaque named types for now.
            None
        },
        _ => None,
    }
}

/// Resolve a `type2` (or `type`, `type1`, `group`) subtree into a [`ResolvedType`].
///
/// This is a structural conversion — named references become [`ResolvedType::Named`]
/// and are resolved later during subtype checking.
pub(crate) fn resolve_type(node: &WrappedNode) -> ResolvedType {
    match node {
        WrappedNode::Syntax {
            rule,
            children,
            text,
            ..
        } => {
            match rule.as_str() {
                "type2" => resolve_type2(node),
                "type1" => resolve_type1(node),
                "type" => resolve_type_choice(node),
                "group" | "grpchoice" => resolve_group(children),
                "grpent" => resolve_single_grpent(children),
                "typename" | "groupname" => {
                    let name = text.trim().to_owned();
                    if name == "any" {
                        ResolvedType::Any
                    } else if let Some(kind) = primitive_kind_for_name(&name) {
                        ResolvedType::Primitive(kind)
                    } else if is_socket_name(&name) {
                        ResolvedType::Socket { name }
                    } else {
                        ResolvedType::Named(name)
                    }
                },
                "value" => {
                    // A bare value (e.g. the `1` in `1: T`) inside a
                    // memberkey. Parse it as a range, a float range, or
                    // fall back to a Named reference.
                    let val = text.trim();
                    if let Ok(i) = val.parse::<i128>() {
                        ResolvedType::Range {
                            lo: Some(i),
                            hi: Some(i),
                            is_float: false,
                        }
                    } else if let Ok(f) = val.parse::<f64>() {
                        let bits = i128::from(f.to_bits());
                        ResolvedType::Range {
                            lo: Some(bits),
                            hi: Some(bits),
                            is_float: true,
                        }
                    } else {
                        ResolvedType::Named(val.to_owned())
                    }
                },
                _ => {
                    // For unknown rule names, try to resolve from children
                    if let Some(first) = children.first() {
                        resolve_type(first)
                    } else {
                        ResolvedType::Named(text.trim().to_owned())
                    }
                },
            }
        },
        WrappedNode::RuleLine { children, .. } => {
            // A RuleLine: the RHS type is the first non-typename, non-assignt
            // syntax child after the LHS.
            find_rhs_type(children).map_or(ResolvedType::Named(String::new()), resolve_type)
        },
        _ => ResolvedType::Named(String::new()),
    }
}

/// Resolve a `type2` node — the leaf type level in CDDL's grammar.
fn resolve_type2(node: &WrappedNode) -> ResolvedType {
    let WrappedNode::Syntax { children, text, .. } = node else {
        return ResolvedType::Named(String::new());
    };

    // Check for tagged type: #6.123(type)
    if let Some(tagged) = try_resolve_tagged(children) {
        return tagged;
    }

    // Check for parenthesized type: ( type )
    if let Some(inner) = try_resolve_parenthesized_type(children) {
        return inner;
    }

    // Check for map/array group. The first non-whitespace character
    // of the type2 text identifies the group delimiter (`{` for map,
    // `[` for array, `&` for group socket).
    let delimiter = text
        .trim_start()
        .chars()
        .next()
        .filter(|c| matches!(*c, '{' | '[' | '&'));
    if let Some(group) = try_resolve_group_from_type2(children, delimiter) {
        return group;
    }

    // Check for a typename/groupname reference
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            match rule.as_str() {
                "typename" | "groupname" => {
                    let name = text.trim().to_owned();
                    if name == "any" {
                        return ResolvedType::Any;
                    }
                    if let Some(kind) = primitive_kind_for_name(&name) {
                        return ResolvedType::Primitive(kind);
                    }
                    if is_socket_name(&name) {
                        return ResolvedType::Socket { name };
                    }
                    return ResolvedType::Named(name);
                },
                "value" => {
                    let val = text.trim();
                    if let Ok(i) = val.parse::<i128>() {
                        return ResolvedType::Range {
                            lo: Some(i),
                            hi: Some(i),
                            is_float: false,
                        };
                    }
                },
                _ => {},
            }
        }
    }

    // Fallback: use text content
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        if trimmed == "any" {
            return ResolvedType::Any;
        }
        if let Some(kind) = primitive_kind_for_name(trimmed) {
            return ResolvedType::Primitive(kind);
        }
        if is_socket_name(trimmed) {
            return ResolvedType::Socket {
                name: trimmed.to_owned(),
            };
        }
        return ResolvedType::Named(trimmed.to_owned());
    }

    ResolvedType::Named(String::new())
}

/// Resolve a `type1` node — may contain ctlop or rangeop.
#[allow(
    clippy::indexing_slicing,
    reason = "slicing guarded by len checks above"
)]
fn resolve_type1(node: &WrappedNode) -> ResolvedType {
    let WrappedNode::Syntax { children, .. } = node else {
        return ResolvedType::Named(String::new());
    };

    // type1 = type2 ( (rangeop | ctlop) type2 )?
    // Collect type2 children and check for rangeop
    let mut type2_nodes: Vec<&WrappedNode> = Vec::new();
    let mut has_rangeop = false;
    let mut ctlop_text: Option<String> = None;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "type2" => type2_nodes.push(child),
                "rangeop" => has_rangeop = true,
                "ctlop" => ctlop_text = Some(crate::ctlop::child_text(child).trim().to_owned()),
                _ => {},
            }
        }
    }

    if type2_nodes.is_empty() {
        return ResolvedType::Named(String::new());
    }

    // Range: type2 .. type2
    if has_rangeop && type2_nodes.len() == 2 {
        let lo_val = type2_value(type2_nodes[0]);
        let hi_val = type2_value(type2_nodes[1]);
        let is_float = lo_val.is_some_and(|(_, f)| f) || hi_val.is_some_and(|(_, f)| f);
        let lo = lo_val.map(|(v, _)| v);
        let hi = hi_val.map(|(v, _)| v);
        return ResolvedType::Range { lo, hi, is_float };
    }

    // Control operator: preserve schema-relevant operators as
    // `ResolvedType::Control`. Only operators proven irrelevant to
    // subtype checking collapse to the carrier.
    // `.and` is a special case: it produces `ResolvedType::Intersection`
    // rather than `Control` because intersection has distinct subtype
    // rules that are easier to express with a flat list of operands.
    if let Some(op_text) = ctlop_text {
        if type2_nodes.len() == 2 && op_text == ".and" {
            let a = resolve_type2(type2_nodes[0]);
            let b = resolve_type2(type2_nodes[1]);
            return ResolvedType::Intersection(vec![a, b]);
        }
        let op = ControlOp::from_text(&op_text);
        if op.is_schema_relevant() && type2_nodes.len() == 2 {
            let carrier = resolve_type2(type2_nodes[0]);
            let controller = resolve_type2(type2_nodes[1]);
            return ResolvedType::Control {
                op,
                carrier: Box::new(carrier),
                controller: Box::new(controller),
            };
        }
        return resolve_type2(type2_nodes[0]);
    }

    // Simple type1 wrapping a type2
    resolve_type2(type2_nodes[0])
}

/// Resolve a `type` node — a choice of type1 alternatives.
fn resolve_type_choice(node: &WrappedNode) -> ResolvedType {
    let WrappedNode::Syntax { children, .. } = node else {
        return ResolvedType::Named(String::new());
    };

    let alternatives: Vec<ResolvedType> = children
        .iter()
        .filter(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type1"))
        .map(resolve_type1)
        .collect();

    if alternatives.is_empty() {
        ResolvedType::Named(String::new())
    } else if alternatives.len() == 1 {
        alternatives
            .into_iter()
            .next()
            .unwrap_or(ResolvedType::Named(String::new()))
    } else {
        ResolvedType::Choice(alternatives)
    }
}

/// Resolve a `group` node — an array `[ ... ]` or map `{ ... }`.
fn resolve_group(children: &[WrappedNode]) -> ResolvedType {
    if children.is_empty() {
        return ResolvedType::Named(String::new());
    }

    // Walk children looking for grpchoice nodes
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "grpchoice"
        {
            return resolve_grpchoice(child);
        }
    }

    ResolvedType::Named(String::new())
}

/// Resolve a `grpchoice` node — the comma-separated entries in a group.
///
/// The `delimiter` is the type2's opening character when known:
/// * `Some('{')` — the group is a map (entries default to map form).
/// * `Some('[')` — the group is an array.
/// * `Some('&')` — the group is a group socket (entries default to map).
/// * `None` — derive map-ness by inspecting the grpents.
fn resolve_grpchoice(node: &WrappedNode) -> ResolvedType {
    resolve_grpchoice_with_delimiter(node, None)
}

/// Resolve a `grpchoice` with an explicit delimiter hint.
///
/// See [`resolve_grpchoice`] for the no-delimiter variant. When the
/// delimiter is `Some('{')` or `Some('&')` (map / group-socket
/// context), entries default to map form; when `Some('[')`, entries
/// default to array form; when `None`, the resolver inspects the
/// grpents to decide.
fn resolve_grpchoice_with_delimiter(
    node: &WrappedNode,
    delimiter: Option<char>,
) -> ResolvedType {
    let WrappedNode::Syntax { children, .. } = node else {
        return ResolvedType::Named(String::new());
    };

    // The `:` form of map entry (`key: value`) is recognized in
    // map / group-socket / parenthesized context (`{`, `&`, `(`). A
    // parenthesized grpchoice with a single grpent like `(1: T)` is
    // semantically a single-entry map. Inside an array `[ ... ]` a
    // `:` is a stray character, not a map separator, and the entry
    // must be treated as a bare array element. Likewise when the
    // delimiter is unknown we fall back to `=>` only.
    let in_map_context = matches!(delimiter, Some('{' | '&' | '('));
    let recognizes_colon = in_map_context;

    let mut map_entries: Vec<MapEntry> = Vec::new();
    let mut array_elements: Vec<ArrayElement> = Vec::new();
    let mut is_map = in_map_context;

    // First pass: check for memberkey entries to determine if this is
    // a map. Bare names (socket plugs) only count in a map context.
    if !is_map {
        for child in children {
            if let WrappedNode::Syntax {
                children: entry_children,
                ..
            } = child
                && entry_has_map_separator(entry_children, recognizes_colon)
            {
                is_map = true;
                break;
            }
        }
    }

    // Second pass: resolve entries
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            children: entry_children,
            ..
        } = child
            && rule == "grpent"
        {
            if entry_has_map_separator(entry_children, recognizes_colon) {
                if let Some(entry) = resolve_map_entry(child) {
                    map_entries.push(entry);
                }
            } else if is_map && has_bare_name(entry_children) {
                // Socket plug inside a map
                if let Some(entry) = resolve_map_entry(child) {
                    map_entries.push(entry);
                }
            } else {
                let element = resolve_array_element(child);
                array_elements.push(element);
            }
        }
    }

    if is_map {
        ResolvedType::Map {
            entries: map_entries,
        }
    } else {
        ResolvedType::Array {
            elements: array_elements,
        }
    }
}

/// True if the grpent's children contain a memberkey whose text
/// carries a map-entry separator. With `recognizes_colon = true` both
/// `=>` and `:` are accepted (RFC9581 `key: value` form); with
/// `recognizes_colon = false` only `=>` is accepted, matching the
/// behaviour required inside an array context.
fn entry_has_map_separator(
    children: &[WrappedNode],
    recognizes_colon: bool,
) -> bool {
    children.iter().any(|c| {
        if let WrappedNode::Syntax { rule, text, .. } = c
            && rule == "memberkey"
        {
            text.contains("=>") || (recognizes_colon && text.contains(':'))
        } else {
            false
        }
    })
}

/// Check if a grpent contains a `=>` indicating a map entry.
/// Check if a `grpent` contains a `=>` indicating a map entry.
///
/// The `=>` separator is inside a `memberkey` node (an atomic `$` rule in
/// pest), so we check for a `memberkey` child whose text contains `=>`.
/// True if the grpent's children contain a memberkey with an `=>`
/// arrow, indicating a map entry of the form `key => value`.
fn has_map_arrow(children: &[WrappedNode]) -> bool {
    children.iter().any(|c| {
        matches!(
            c,
            WrappedNode::Syntax { rule, text, .. }
                if rule == "memberkey" && text.contains("=>")
        )
    })
}

/// True if the grpent's children contain a memberkey with a `:`
/// separator, indicating a map entry of the form `key: value`.
/// This is the single-key map form used by RFC9581
/// (`(1: #6.1(int / float))`).
fn has_map_colon(children: &[WrappedNode]) -> bool {
    children.iter().any(|c| {
        matches!(
            c,
            WrappedNode::Syntax { rule, text, .. }
                if rule == "memberkey" && text.contains(':')
        )
    })
}

/// Check if a `grpent` contains a `groupname` child (socket plug reference).
fn has_groupname(children: &[WrappedNode]) -> bool {
    children
        .iter()
        .any(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "groupname"))
}

/// Check if a `grpent` contains a bare name reference (socket plug) without
/// a `memberkey`.  A socket plug appears as `type`, `typename`, or
/// `groupname` when it's used bare in a map/group.
fn has_bare_name(children: &[WrappedNode]) -> bool {
    let has_memberkey = children
        .iter()
        .any(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "memberkey"));
    if has_memberkey {
        return false;
    }
    children.iter().any(|c| {
        matches!(
            c,
            WrappedNode::Syntax { rule, .. }
                if rule == "typename" || rule == "groupname" || rule == "type"
        )
    })
}

/// Resolve a single map entry from a `grpent` node.
///
/// For map entries (`key => value`), the key type lives inside the `memberkey`
/// child (which is an atomic `$` pest rule), and the value type is a sibling
/// `type` child of the grpent.
fn resolve_map_entry(node: &WrappedNode) -> Option<MapEntry> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };

    let mut occurrence = Occurrence::One;
    let mut key: Option<ResolvedType> = None;
    let mut value: Option<ResolvedType> = None;
    let mut saw_memberkey = false;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "occur" => {
                    occurrence = resolve_occurrence(child);
                },
                "memberkey" => {
                    saw_memberkey = true;
                    key = extract_memberkey_type(child);
                },
                "typename" | "groupname" => {
                    // Bare name without memberkey → socket plug
                    if !saw_memberkey {
                        let name = crate::ctlop::child_text(child).trim().to_owned();
                        key = Some(ResolvedType::Socket { name });
                    }
                },
                "type" | "type1" | "type2" | "group" => {
                    if saw_memberkey {
                        // Value after memberkey → key
                        value = Some(resolve_type(child));
                    } else if key.is_none() {
                        // Bare type without memberkey → socket plug
                        let name = crate::ctlop::child_text(child).trim().to_owned();
                        key = Some(ResolvedType::Socket { name });
                    }
                },
                _ => {},
            }
        }
    }

    Some(MapEntry {
        key: key.unwrap_or(ResolvedType::Any),
        value: value.unwrap_or(ResolvedType::Any),
        occurrence,
    })
}

/// Resolve a single `grpent` node into a [`ResolvedType`].
///
/// A grpent like `(ml-dsa-44 => ml-dsa-seed)` is a single map or array
/// entry within a parenthesized group.  We wrap it as a one-entry
/// `Map` or `Array`.
fn resolve_single_grpent(children: &[WrappedNode]) -> ResolvedType {
    // Alternative 3 in the grammar: `[occur] "(" group ")"`. The grpent
    // wraps a parenthesized group. Recurse into the group with `(` as
    // the delimiter so that the inner grpchoice recognizes the `:` map
    // form (e.g. `(1: T)` is a single-entry map).
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "group"
        {
            return resolve_group_with_delimiter(child, Some('('));
        }
    }
    // A grpent is a map entry if it has a memberkey with `=>` or `:`
    // separator. The `:` form is used by RFC9581-style single-key
    // maps (e.g. `(1: #6.1(int))`). Both forms are valid in any
    // context where a grpent appears; the grpchoice wrapper is
    // responsible for deciding array-vs-map based on the group
    // delimiter.
    if entry_has_map_separator(children, true) {
        let entry = resolve_map_entry_from_children(children);
        ResolvedType::Map {
            entries: vec![entry],
        }
    } else {
        let element = resolve_array_element_from_children(children);
        ResolvedType::Array {
            elements: vec![element],
        }
    }
}

/// Resolve a map entry from grpent children directly (no `WrappedNode` wrapper).
fn resolve_map_entry_from_children(children: &[WrappedNode]) -> MapEntry {
    let mut occurrence = Occurrence::One;
    let mut key: Option<ResolvedType> = None;
    let mut value: Option<ResolvedType> = None;

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "occur" => {
                    occurrence = resolve_occurrence(child);
                },
                "memberkey" => {
                    key = extract_memberkey_type(child);
                },
                "typename" | "groupname" => {
                    let name = crate::ctlop::child_text(child).trim().to_owned();
                    key = Some(ResolvedType::Socket { name });
                },
                "type" | "type1" | "type2" | "group" => {
                    value = Some(resolve_type(child));
                },
                _ => {},
            }
        }
    }

    MapEntry {
        key: key.unwrap_or(ResolvedType::Any),
        value: value.unwrap_or(ResolvedType::Any),
        occurrence,
    }
}

/// Resolve an array element from grpent children directly.
fn resolve_array_element_from_children(children: &[WrappedNode]) -> ArrayElement {
    let mut occurrence = Occurrence::One;
    let mut ty = ResolvedType::Named(String::new());

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "occur" => occurrence = resolve_occurrence(child),
                "type" | "type1" | "type2" | "group" => {
                    ty = resolve_type(child);
                },
                _ => {},
            }
        }
    }

    ArrayElement { ty, occurrence }
}
///
/// The memberkey text is `type1 S ( "^" S )? "=>"`. The type1 is the first
/// child.
fn extract_memberkey_type(node: &WrappedNode) -> Option<ResolvedType> {
    let WrappedNode::Syntax { children, text, .. } = node else {
        return None;
    };
    // The CDDL grammar's first memberkey alternative uses
    // `type1 ~ "=>"`, the second uses `(value | bareword) ~ ":"`.
    // The parser produces distinct child rules:
    //   * `type1` for the `=>` form and for numeric/text values
    //   * `bareword` for the bareword `foo:` form
    // BUG-009: a bareword member key is a concrete text label,
    // not a type reference.  Treat it as `TextKey("foo")` so the
    // subtype checker can compare it against `tstr` (and against a
    // choice arm `int / tstr`) without falling through to
    // `unresolved name: foo`.
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "bareword"
        {
            let key = text.trim_end_matches(':').trim().to_owned();
            return Some(ResolvedType::TextKey(key));
        }
    }
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && (rule == "type1" || rule == "type" || rule == "value")
        {
            let ty = resolve_type(child);
            // If the resolver fell back to Named for a primitive
            // integer (e.g. `1`) but the memberkey's overall text
            // indicates the value form, re-parse the text directly
            // as a numeric Range.
            if matches!(ty, ResolvedType::Named(ref n) if n.parse::<i128>().is_ok())
                && text
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '-')
            {
                let val = text.trim();
                if let Ok(i) = val.trim_end_matches(':').trim().parse::<i128>() {
                    return Some(ResolvedType::Range {
                        lo: Some(i),
                        hi: Some(i),
                        is_float: false,
                    });
                }
            }
            return Some(ty);
        }
    }
    None
}

/// Resolve a single array element from a `grpent` node.
fn resolve_array_element(node: &WrappedNode) -> ArrayElement {
    let WrappedNode::Syntax { children, .. } = node else {
        return ArrayElement {
            ty: ResolvedType::Named(String::new()),
            occurrence: Occurrence::One,
        };
    };

    let mut occurrence = Occurrence::One;
    let mut ty = ResolvedType::Named(String::new());

    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child {
            match rule.as_str() {
                "occur" => occurrence = resolve_occurrence(child),
                "type" | "type1" | "type2" | "group" => {
                    ty = resolve_type(child);
                },
                _ => {},
            }
        }
    }

    ArrayElement { ty, occurrence }
}

/// Resolve an occurrence specifier from an `occur` node.
fn resolve_occurrence(node: &WrappedNode) -> Occurrence {
    // Check the full text for occurrence patterns first (must be before
    // individual child checks — a `2*5` occur node has a `uint` child with
    // text "2", and the full text "2*5" is only recoverable from the node's
    // own text field).
    let full_text = crate::ctlop::child_text(node).trim();
    if full_text == "?" {
        return Occurrence::Optional;
    }
    if full_text == "*" {
        return Occurrence::ZeroOrMore;
    }
    if full_text == "+" {
        return Occurrence::OneOrMore;
    }
    if let Some((lo_str, hi_str)) = full_text.split_once('*') {
        let lo = lo_str.trim().parse::<u32>().unwrap_or(0);
        let hi = hi_str.trim().parse::<u32>().unwrap_or(u32::MAX);
        return Occurrence::Range { lo, hi };
    }

    // Fallback: check for a bare uint child (e.g. an unwrapped integer)
    if let WrappedNode::Syntax { children, .. } = node {
        for child in children {
            if let WrappedNode::Syntax { rule, text, .. } = child
                && rule == "uint"
                && let Ok(n) = text.trim().parse::<u32>()
            {
                return Occurrence::Range { lo: n, hi: n };
            }
        }
    }

    Occurrence::One
}

/// Try to resolve a tagged type like `#6.123(inner)` from type2 children.
fn try_resolve_tagged(children: &[WrappedNode]) -> Option<ResolvedType> {
    let mut tag_number: Option<u64> = None;
    let mut inner_type: Option<&WrappedNode> = None;

    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            if rule == "head_number" {
                tag_number = text.trim().parse::<u64>().ok();
            } else if rule == "type" || rule == "type1" || rule == "type2" {
                inner_type = Some(child);
            }
        }
    }

    if let (Some(tag), Some(inner)) = (tag_number, inner_type) {
        Some(ResolvedType::Tag {
            tag,
            inner: Box::new(resolve_type(inner)),
        })
    } else {
        None
    }
}

/// Try to resolve a parenthesized type `(type)` from type2 children.
fn try_resolve_parenthesized_type(children: &[WrappedNode]) -> Option<ResolvedType> {
    // Look for a "type" child (bypassing any whitespace/comments)
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "type"
        {
            let resolved = resolve_type_choice(child);
            // A parenthesized grpchoice with a single grpent that uses
            // the `:` form (e.g. `(1: T)`) is semantically a single
            // map entry. The inner grpchoice resolves to an Array
            // because the delimiter is unknown; unwrap nested layers
            // until the inner element is a Map.
            let mut current = resolved;
            while let ResolvedType::Array { elements } = &current
                && elements.len() == 1
                && let Some(inner) = elements.first()
            {
                let inner_ty = inner.ty.clone();
                if matches!(&inner_ty, ResolvedType::Map { .. }) {
                    return Some(inner_ty);
                }
                // Stop if the inner is not an Array (no further
                // unwrapping is safe).
                if !matches!(&inner_ty, ResolvedType::Array { .. }) {
                    break;
                }
                current = inner_ty;
            }
            return Some(current);
        }
    }
    None
}

/// Try to resolve a map/array group from type2 children.
///
/// A type2 can contain a group like `[ a, b ]` or `{ k => v }`. The
/// `delimiter` (when known) hints at the group context:
/// * `Some('{')` — the group is a map (entries default to map form).
/// * `Some('[')` — the group is an array.
/// * `Some('&')` — the group is a group socket (entries default to map).
/// * `None` — the delimiter is unknown (caller did not pass it); the resolver falls back
///   to inspecting the grpents.
fn try_resolve_group_from_type2(
    children: &[WrappedNode],
    delimiter: Option<char>,
) -> Option<ResolvedType> {
    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child
            && (rule == "group" || rule == "grpchoice")
        {
            // If the caller did not pass a delimiter, try to read
            // it from the group's own text (e.g. "{1: T}" or "[1, 2]").
            let effective_delimiter = delimiter.or_else(|| {
                let trimmed = text.trim_start();
                trimmed
                    .chars()
                    .next()
                    .filter(|c| matches!(*c, '{' | '[' | '&'))
            });
            return Some(resolve_group_with_delimiter(child, effective_delimiter));
        }
    }
    None
}

/// Resolve a group node, honoring the type2's delimiter when known.
///
/// When the delimiter is `Some('{')` or `Some('&')` (map / group-socket
/// context), the resolver treats the group as a map even if no
/// individual grpent has an explicit map arrow. This matches RFC9581
/// where a group of bare socket plugs like `{ $$BASE, * $$ELECTIVE }`
/// is a map whose entries' keys are produced by the socket plugs.
fn resolve_group_with_delimiter(
    node: &WrappedNode,
    delimiter: Option<char>,
) -> ResolvedType {
    let WrappedNode::Syntax { rule, .. } = node else {
        return ResolvedType::Named(String::new());
    };
    if rule == "grpchoice" {
        return resolve_grpchoice_with_delimiter(node, delimiter);
    }
    // Find the first grpchoice child and recurse.
    let WrappedNode::Syntax { children, .. } = node else {
        return ResolvedType::Named(String::new());
    };
    for child in children {
        if let WrappedNode::Syntax { rule, .. } = child
            && rule == "grpchoice"
        {
            return resolve_grpchoice_with_delimiter(child, delimiter);
        }
    }
    ResolvedType::Named(String::new())
}

/// Extract a numeric value from a type2 node for range bounds.
fn type2_value(node: &WrappedNode) -> Option<(i128, bool)> {
    let WrappedNode::Syntax { children, text, .. } = node else {
        return None;
    };

    for child in children {
        if let WrappedNode::Syntax {
            rule,
            text: child_text,
            ..
        } = child
            && rule == "value"
        {
            let val = child_text.trim();
            if let Ok(i) = val.parse::<i128>() {
                return Some((i, false));
            }
            if let Ok(f) = val.parse::<f64>() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "Best-effort float→int for range bounds"
                )]
                return Some((f as i128, true));
            }
        }
    }

    let val = text.trim();
    if let Ok(i) = val.parse::<i128>() {
        Some((i, false))
    } else {
        None
    }
}

/// Find the RHS type node in a `RuleLine`'s children.
///
/// `RuleLine` children contain a single `expr` node with the LHS typename,
/// assignt, and RHS type. Walk into `expr` to find the type after `assignt`.
fn find_rhs_type(children: &[WrappedNode]) -> Option<&WrappedNode> {
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            children: expr_children,
            ..
        } = child
            && rule == "expr"
        {
            let mut past_lhs = false;
            for expr_child in expr_children {
                if let WrappedNode::Syntax { rule, .. } = expr_child {
                    match rule.as_str() {
                        "assignt" | "assigng" => {
                            past_lhs = true;
                        },
                        "type" | "type1" | "type2" | "group" | "grpent" if past_lhs => {
                            return Some(expr_child);
                        },
                        _ => {},
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Definition resolution
// ---------------------------------------------------------------------------

/// A map from definition name to its `RuleLine` node.
#[derive(Debug, Clone)]
pub(crate) struct DefinitionMap {
    /// Name → owned `RuleLine` node (the LHS text and full subtree).
    definitions: HashMap<String, WrappedNode>,
    /// Socket name → resolved choices from `//=` augmentations.
    socket_choices: HashMap<String, Vec<ResolvedType>>,
}

impl DefinitionMap {
    /// Build a definition map from a set of complete `WrappedNode`s,
    /// recursively descending into `Directive` children.
    #[must_use]
    pub(crate) fn from_nodes(nodes: &[WrappedNode]) -> Self {
        let mut defs = HashMap::new();
        for node in nodes {
            collect_into_map(node, &mut defs);
        }
        let socket_choices = collect_all_socket_choices(nodes);
        Self {
            definitions: defs,
            socket_choices,
        }
    }

    /// Look up a definition by name.
    #[must_use]
    pub(crate) fn get(
        &self,
        name: &str,
    ) -> Option<&WrappedNode> {
        self.definitions.get(name)
    }

    /// Return `true` if the map contains a definition for `name`.
    #[must_use]
    pub(crate) fn contains(
        &self,
        name: &str,
    ) -> bool {
        self.definitions.contains_key(name)
    }

    /// Return the resolved choices for a socket, if any.
    #[must_use]
    pub(crate) fn socket_choices_for(
        &self,
        name: &str,
    ) -> Option<&[ResolvedType]> {
        self.socket_choices.get(name).map(Vec::as_slice)
    }
}

/// Recursively collect `RuleLine` nodes keyed by their top-level rule name.
fn collect_into_map(
    node: &WrappedNode,
    map: &mut HashMap<String, WrappedNode>,
) {
    if let Some(name) = extract_rule_name(node) {
        map.entry(name).or_insert_with(|| node.clone());
    }

    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Directive { children, .. }
        | WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_into_map(child, map);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Collect all socket `//=` augmentations from a node tree.
///
/// Returns a map from socket name to its resolved choice types.
fn collect_all_socket_choices(nodes: &[WrappedNode]) -> HashMap<String, Vec<ResolvedType>> {
    let mut choices: HashMap<String, Vec<ResolvedType>> = HashMap::new();
    collect_socket_from_nodes(nodes, &mut choices);
    choices
}

/// Recursively scan nodes for `//=` and `/=` socket augmentations.
fn collect_socket_from_nodes(
    nodes: &[WrappedNode],
    choices: &mut HashMap<String, Vec<ResolvedType>>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                // Collect both `//=` (group augment) and `$/=` (type
                // augment) socket plugs. Group augmentations (//=)
                // always define a socket; type augmentations (/=) only
                // when the LHS name is a socket (starts with `$`).
                if let Some(head) = rule_head_from_children(children)
                    && let Some(rhs) = find_rhs_type(children)
                {
                    let is_socket_aug = matches!(head.assignment, AssignmentKind::GroupAugment)
                        || (matches!(head.assignment, AssignmentKind::TypeAugment)
                            && head.kind.is_socket());
                    if is_socket_aug {
                        let ty = resolve_type(rhs);
                        choices.entry(head.name).or_default().push(ty);
                    }
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                collect_socket_from_nodes(children, choices);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Extract the top-level rule name from a `RuleLine`'s text.
///
/// The text is of the form `name<genericparm> = type`. This extracts
/// everything before the first space, `<`, `=`, or tab.
fn extract_rule_name(node: &WrappedNode) -> Option<String> {
    let WrappedNode::RuleLine { text, .. } = node else {
        return None;
    };
    let lhs = text
        .split_once('=')
        .map_or(text.as_str(), |(lhs, _)| lhs)
        .trim();
    let name = lhs
        .chars()
        .take_while(|ch| !matches!(ch, ' ' | '<' | '\t'))
        .collect::<String>();
    if name.is_empty() { None } else { Some(name) }
}

/// Collect all socket augmentation choices for a given socket name.
///
/// Walks the complete node tree looking for `$sock /= type` rule lines and
/// resolves the RHS of each to a [`ResolvedType`].
///
/// Socket augmentations across included files are collected from `Directive`
/// children, so they appear in definition order.
#[must_use]
pub(crate) fn collect_socket_choices(
    socket_name: &str,
    nodes: &[WrappedNode],
) -> Vec<ResolvedType> {
    let mut choices = Vec::new();
    collect_socket_choices_recurse(socket_name, nodes, &mut choices);
    choices
}

/// Recursively search nodes for socket augmentations.
fn collect_socket_choices_recurse(
    socket_name: &str,
    nodes: &[WrappedNode],
    choices: &mut Vec<ResolvedType>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { text, children, .. } => {
                // Check if this is "$sock /= type"
                let trimmed = text.trim();
                let prefix = format!("{socket_name} /=");
                if trimmed.starts_with(&prefix)
                    && let Some(rhs) = find_rhs_type(children)
                {
                    choices.push(resolve_type(rhs));
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                collect_socket_choices_recurse(socket_name, children, choices);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Resolve a named type reference to its [`ResolvedType`] by looking it up
/// in the definition map and resolving its RHS.
///
/// Returns `None` if the name is not found or is a socket (which needs
/// special handling via [`collect_socket_choices`]).
#[must_use]
pub(crate) fn resolve_definition(
    name: &str,
    defs: &DefinitionMap,
) -> Option<ResolvedType> {
    let node = defs.get(name)?;
    Some(resolve_type(node))
}

/// Check whether a rule name is a socket reference (starts with `$`).
#[must_use]
pub(crate) fn is_socket_name(name: &str) -> bool {
    name.starts_with('$')
}

// ---------------------------------------------------------------------------
// Structural subtype checking
// ---------------------------------------------------------------------------

/// Check whether `lhs` is a structural subtype of `rhs`.
///
/// Returns `Ok(())` if `lhs ⊆ rhs`, or `Err(message)` describing why not.
///
/// Named references are resolved via `defs`. Recursive types are detected via
/// a visited set and assumed compatible (coinductive).
pub(crate) fn is_subtype(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
) -> Result<(), String> {
    let mut visited = HashSet::new();
    is_subtype_impl(lhs, rhs, defs, &mut visited)
}

/// Check whether a primitive is a subtype of another primitive.
fn is_primitive_subtype(
    sub: PrimitiveKind,
    sup: PrimitiveKind,
) -> bool {
    if sub == sup {
        return true;
    }
    matches!(
        (sub, sup),
        (
            PrimitiveKind::Uint | PrimitiveKind::Nint,
            PrimitiveKind::Int
        ) | (
            PrimitiveKind::Float16,
            PrimitiveKind::Float32 | PrimitiveKind::Float64 | PrimitiveKind::Float,
        ) | (
            PrimitiveKind::Float32,
            PrimitiveKind::Float64 | PrimitiveKind::Float,
        ) | (PrimitiveKind::Float64, PrimitiveKind::Float)
    )
}

/// Check occurrence compatibility: `lhs ⊆ rhs`.
fn occurrence_compatible(
    lhs: Occurrence,
    rhs: Occurrence,
) -> bool {
    if lhs.min() < rhs.min() {
        return false;
    }
    match (lhs.max(), rhs.max()) {
        (Some(lhs_hi), Some(rhs_hi)) => lhs_hi <= rhs_hi,
        (None, Some(_)) => false,
        (_, None) => true,
    }
}

/// Internal recursive subtype check with cycle detection.
#[allow(
    clippy::too_many_lines,
    reason = "single large match is clearer than split"
)]
fn is_subtype_impl(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
) -> Result<(), String> {
    let conflicts = collect_subtype_conflicts(lhs, rhs, defs, visited, &mut Vec::new());
    match conflicts.first() {
        Some(c) => Err(c.reason.clone()),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Structured subtype conflicts
// ---------------------------------------------------------------------------

/// A position within a CDDL schema tree.
///
/// Used by [`WithinConflict`] so the diff renderer can quote the exact
/// line of source the user should look at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PathSegment {
    /// Index into an array.
    ArrayIndex(usize),
    /// Index into a map entry list.
    MapEntry(usize),
    /// Index into a choice arm list.
    ChoiceArm(usize),
    /// A control operator applied to a carrier.
    ControlOp(ControlOp),
    /// A group entry inside a `group` or `grpent` construct.
    GroupEntry(usize),
}

/// The kind of failure found by the subtype checker.
///
/// Variants are intentionally narrow so the diff renderer and CLI can
/// map them to specific styles or rule codes without re-parsing the
/// `reason` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithinConflictKind {
    /// A required entry on the LHS is not accepted by the RHS map.
    MissingRequiredRhs,
    /// The LHS entry has no matching RHS entry (broader RHS that does
    /// not satisfy the required LHS shape).
    LhsNotAccepted,
    /// A map entry matched more times than the RHS allows.
    TooManyMatches,
    /// Two primitive types are not in a subtype relationship.
    PrimitiveMismatch,
    /// Two numeric ranges are not in a containment relationship.
    RangeMismatch,
    /// A control operator constraint is violated (e.g. `.cbor ⊄ .dtrm`).
    ControlMismatch,
    /// The two sides have structurally different shapes.
    DifferentStructure,
    /// A named reference cannot be resolved.
    UnresolvedName,
}

/// A single, structured subtype-check failure.
///
/// The collector produces zero or more of these for a single
/// `is_subtype` call. The first conflict's `reason` is what
/// [`is_subtype_impl`] surfaces to the existing string API; downstream
/// consumers should switch to [`subtype_conflicts`] for full context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WithinConflict {
    /// The path from the root of the schema to the failure site.
    pub path: Vec<PathSegment>,
    /// The kind of failure.
    pub kind: WithinConflictKind,
    /// The LHS sub-type involved in the failure, if known.
    pub lhs: Option<ResolvedType>,
    /// The RHS sub-type involved in the failure, if known.
    pub rhs: Option<ResolvedType>,
    /// Human-readable explanation of the failure.
    pub reason: String,
}

/// Collect every subtype failure between `lhs` and `rhs`.
///
/// The returned vector is empty when `lhs ⊆ rhs` holds. Otherwise it
/// contains one [`WithinConflict`] for every failure site the checker
/// was able to localize. This is the structured companion to
/// [`is_subtype`].
#[must_use]
pub(crate) fn subtype_conflicts(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
) -> Vec<WithinConflict> {
    let mut visited = HashSet::new();
    let mut path = Vec::new();
    let mut conflicts = Vec::new();
    collect_subtype_conflicts_inner(lhs, rhs, defs, &mut visited, &mut path, &mut conflicts);
    conflicts
}

/// Recursive subtype-conflict collector that threads state through
/// recursion. Walks the schema and pushes a [`WithinConflict`] for every
/// structural mismatch it can localize. Conflicts accumulate (rather
/// than short-circuit) so downstream consumers can render a full diff.
#[allow(
    clippy::too_many_lines,
    reason = "single large match is clearer than split"
)]
fn collect_subtype_conflicts(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
    path: &mut Vec<PathSegment>,
) -> Vec<WithinConflict> {
    let mut conflicts = Vec::new();
    collect_subtype_conflicts_inner(lhs, rhs, defs, visited, path, &mut conflicts);
    conflicts
}

/// Inner subtype-conflict collector that mutates the provided
/// `conflicts` vector and threads `visited`/`path` state through
/// recursion. Use [`subtype_conflicts`] for the public entry point.
#[allow(
    clippy::too_many_lines,
    reason = "single large match is clearer than split"
)]
fn collect_subtype_conflicts_inner(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
    path: &mut Vec<PathSegment>,
    conflicts: &mut Vec<WithinConflict>,
) {
    match (lhs, rhs) {
        // BUG-009: a bareword map member key (`foo:`) is a concrete
        // text-label value.  It is by definition a subtype of
        // `tstr`, of another concrete text label, and of `any`.
        (
            ResolvedType::TextKey(_),
            ResolvedType::Primitive(_) | ResolvedType::TextKey(_) | ResolvedType::Any,
        )
        | (_, ResolvedType::Any) => {},

        (ResolvedType::Any, _) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::DifferentStructure,
                Some(lhs.clone()),
                Some(rhs.clone()),
                "Any is not a subtype of anything except Any".to_owned(),
            );
        },

        (ResolvedType::Primitive(l), ResolvedType::Primitive(r)) => {
            if !is_primitive_subtype(*l, *r) {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::PrimitiveMismatch,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("{l:?} is not a subtype of {r:?}"),
                );
            }
        },

        (
            ResolvedType::Range {
                lo: llo,
                hi: lhi,
                is_float: lf,
            },
            ResolvedType::Range {
                lo: rlo,
                hi: rhi,
                is_float: rf,
            },
        ) => {
            if lf != rf {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::RangeMismatch,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    "float range not subtype of integer range".to_owned(),
                );
                return;
            }
            let lo_ok = match (llo, rlo) {
                (Some(l), Some(r)) => l >= r,
                (None, Some(_)) => false,
                _ => true,
            };
            let hi_ok = match (lhi, rhi) {
                (Some(l), Some(r)) => l <= r,
                (None, Some(_)) => false,
                _ => true,
            };
            if !(lo_ok && hi_ok) {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::RangeMismatch,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("range {llo:?}..{lhi:?} not within {rlo:?}..{rhi:?}"),
                );
            }
        },

        // Range ⊆ Primitive: a specific value fits in its type domain
        (
            ResolvedType::Range {
                lo,
                hi,
                is_float: lf,
            },
            ResolvedType::Primitive(kind),
        ) => {
            let result: Result<(), String> = if *lf {
                match kind {
                    PrimitiveKind::Float
                    | PrimitiveKind::Float16
                    | PrimitiveKind::Float32
                    | PrimitiveKind::Float64 => Ok(()),
                    _ => Err("float range is not a subtype of non-float primitive".to_owned()),
                }
            } else {
                match kind {
                    PrimitiveKind::Int => Ok(()),
                    PrimitiveKind::Uint => {
                        if lo.is_some_and(|l| l >= 0) {
                            Ok(())
                        } else {
                            Err(
                                "integer range contains negative values, not a subtype of uint"
                                    .to_owned(),
                            )
                        }
                    },
                    PrimitiveKind::Nint => {
                        if hi.is_some_and(|h| h < 0) {
                            Ok(())
                        } else {
                            Err(
                                "integer range contains non-negative values, not a subtype of nint"
                                    .to_owned(),
                            )
                        }
                    },
                    _ => Err("range is not a subtype of this primitive".to_owned()),
                }
            };
            if let Err(reason) = result {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::PrimitiveMismatch,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    reason,
                );
            }
        },

        // Primitive ⊄ Range: a broad type is never a subtype of a specific range
        (ResolvedType::Primitive(_), ResolvedType::Range { .. }) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::PrimitiveMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                "primitive type is not a subtype of a specific range".to_owned(),
            );
        },

        (ResolvedType::Tag { tag: lt, inner: li }, ResolvedType::Tag { tag: rt, inner: ri }) => {
            if lt != rt {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::DifferentStructure,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("tag {lt} != tag {rt}"),
                );
                return;
            }
            collect_subtype_conflicts_inner(li, ri, defs, visited, path, conflicts);
        },

        (ResolvedType::Array { elements: le }, ResolvedType::Array { elements: re }) => {
            collect_array_conflicts(le, re, lhs, rhs, defs, visited, path, conflicts);
        },

        (ResolvedType::Map { entries: le }, ResolvedType::Map { entries: re }) => {
            collect_map_conflicts(le, re, defs, path, conflicts);
        },

        (ResolvedType::Choice(alts), _) => {
            for (i, alt) in alts.iter().enumerate() {
                path.push(PathSegment::ChoiceArm(i));
                collect_subtype_conflicts_inner(alt, rhs, defs, visited, path, conflicts);
                path.pop();
            }
        },

        (_, ResolvedType::Choice(alts)) => {
            let mut arm_reasons = Vec::new();
            for (i, alt) in alts.iter().enumerate() {
                path.push(PathSegment::ChoiceArm(i));
                let mut sub = Vec::new();
                collect_subtype_conflicts_inner(lhs, alt, defs, visited, path, &mut sub);
                path.pop();
                if sub.is_empty() {
                    return;
                }
                if let Some(conflict) = sub.first() {
                    arm_reasons.push(format!("choice[{i}]: {}", conflict.reason));
                }
            }
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::DifferentStructure,
                Some(lhs.clone()),
                Some(rhs.clone()),
                if arm_reasons.is_empty() {
                    format!("{} not subtype of any choice arm", type_name(lhs))
                } else {
                    format!(
                        "{} not subtype of any choice arm: [{}]",
                        type_name(lhs),
                        arm_reasons.join(", ")
                    )
                },
            );
        },

        (ResolvedType::Socket { name }, _) => {
            if let Some(choices) = defs.socket_choices_for(name) {
                let lhs_choice = ResolvedType::Choice(choices.to_vec());
                collect_subtype_conflicts_inner(&lhs_choice, rhs, defs, visited, path, conflicts);
            } else {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::UnresolvedName,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("socket {name} has no choices to resolve"),
                );
            }
        },

        (ResolvedType::Named(ln), ResolvedType::Named(rn)) => {
            if ln == rn {
                return;
            }
            let key = (ln.clone(), rn.clone());
            if visited.contains(&key) {
                return;
            }
            visited.insert(key);
            match (defs.get(ln), defs.get(rn)) {
                (Some(lnode), Some(rnode)) => {
                    let lt = resolve_type(lnode);
                    let rt = resolve_type(rnode);
                    collect_subtype_conflicts_inner(&lt, &rt, defs, visited, path, conflicts);
                },
                (Some(lnode), None) => {
                    let lt = resolve_type(lnode);
                    collect_subtype_conflicts_inner(&lt, rhs, defs, visited, path, conflicts);
                },
                (None, Some(rnode)) => {
                    let rt = resolve_type(rnode);
                    collect_subtype_conflicts_inner(lhs, &rt, defs, visited, path, conflicts);
                },
                (None, None) => {
                    push_conflict(
                        conflicts,
                        path,
                        WithinConflictKind::UnresolvedName,
                        Some(lhs.clone()),
                        Some(rhs.clone()),
                        format!("{ln} not subtype of {rn} (both unresolved)"),
                    );
                },
            }
        },

        (ResolvedType::Named(name), _) => {
            let key = (name.clone(), type_name(rhs));
            if visited.contains(&key) {
                return;
            }
            visited.insert(key);
            if let Some(node) = defs.get(name) {
                let resolved = resolve_type(node);
                collect_subtype_conflicts_inner(&resolved, rhs, defs, visited, path, conflicts);
            } else if let Some(node) = defs_find_suffix(defs, name) {
                let resolved = resolve_type(node);
                collect_subtype_conflicts_inner(&resolved, rhs, defs, visited, path, conflicts);
            } else {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::UnresolvedName,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("unresolved name: {name}"),
                );
            }
        },

        (_, ResolvedType::Named(name)) => {
            let key = (type_name(lhs), name.clone());
            if visited.contains(&key) {
                return;
            }
            visited.insert(key);
            if let Some(node) = defs.get(name) {
                let resolved = resolve_type(node);
                collect_subtype_conflicts_inner(lhs, &resolved, defs, visited, path, conflicts);
            } else if let Some(node) = defs_find_suffix(defs, name) {
                let resolved = resolve_type(node);
                collect_subtype_conflicts_inner(lhs, &resolved, defs, visited, path, conflicts);
            } else {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::UnresolvedName,
                    Some(lhs.clone()),
                    Some(rhs.clone()),
                    format!("unresolved name: {name}"),
                );
            }
        },

        // `L ⊆ (A .and B)` requires `L ⊆ A` and `L ⊆ B`.
        // Each operand that fails produces its own conflict so the
        // diff renderer can show which arm is responsible.
        (_, ResolvedType::Intersection(operands)) => {
            for operand in operands {
                collect_subtype_conflicts_inner(lhs, operand, defs, visited, path, conflicts);
            }
        },

        // `(A .and B) ⊆ R` — conservative implementation requires
        // both `A ⊆ R` and `B ⊆ R`. This is safe (no false positives)
        // at the cost of false negatives when one operand is broader
        // than R but the intersection is still within R.
        (ResolvedType::Intersection(operands), _) => {
            for operand in operands {
                collect_subtype_conflicts_inner(operand, rhs, defs, visited, path, conflicts);
            }
        },

        // `.within` is an assertion on its carrier, not a different
        // value shape. Once the assertion exists in the tree it is
        // validated independently by `validate_within_pass`; when a
        // `.within` expression is used as an operand to another subtype
        // check, the effective schema is its carrier.
        (
            ResolvedType::Control {
                op: ControlOp::Within,
                carrier,
                controller: _,
            },
            _,
        ) => {
            path.push(PathSegment::ControlOp(ControlOp::Within));
            collect_subtype_conflicts_inner(carrier, rhs, defs, visited, path, conflicts);
            path.pop();
        },

        (
            _,
            ResolvedType::Control {
                op: ControlOp::Within,
                carrier,
                controller: _,
            },
        ) => {
            path.push(PathSegment::ControlOp(ControlOp::Within));
            collect_subtype_conflicts_inner(lhs, carrier, defs, visited, path, conflicts);
            path.pop();
        },

        // `Control(op, carrier, _) ⊆ R` when the operator is known to
        // narrow its carrier (e.g. `.gt`, `.ge`, `.lt`, `.le`, `.size`,
        // `.bits`, `.cbor`, `.cborseq`, `.dtrm`, `.dtrmseq`). The
        // narrowing rule says: a value that satisfies a narrowing
        // operator on `carrier` is also a valid `carrier`, so the
        // subtype check reduces to `carrier ⊆ R`. This is what makes
        // `uint .gt 1 ⊆ uint`, `bstr .cbor T ⊆ bstr`, etc. pass.
        //
        // `.and` is represented separately as `Intersection`.
        // `.within` is handled above as an assertion wrapper.
        (
            ResolvedType::Control {
                op,
                carrier,
                controller: _,
            },
            _,
        ) if op.is_narrowing() && !matches!(rhs, ResolvedType::Control { .. }) => {
            path.push(PathSegment::ControlOp(op.clone()));
            collect_subtype_conflicts_inner(carrier, rhs, defs, visited, path, conflicts);
            path.pop();
        },

        _ => {
            if let (ResolvedType::Control { .. }, ResolvedType::Control { .. }) = (lhs, rhs) {
                collect_control_conflicts(lhs, rhs, defs, visited, path, conflicts);
                return;
            }
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::DifferentStructure,
                Some(lhs.clone()),
                Some(rhs.clone()),
                format!(
                    "{} not subtype of {} (different structure)",
                    type_name(lhs),
                    type_name(rhs)
                ),
            );
        },
    }
}

/// Push a [`WithinConflict`] into the collector, cloning the current
/// path so the caller can keep mutating it without affecting the
/// recorded conflict.
fn push_conflict(
    conflicts: &mut Vec<WithinConflict>,
    path: &[PathSegment],
    kind: WithinConflictKind,
    lhs: Option<ResolvedType>,
    rhs: Option<ResolvedType>,
    reason: String,
) {
    conflicts.push(WithinConflict {
        path: path.to_vec(),
        kind,
        lhs,
        rhs,
        reason,
    });
}

/// Map subtyping for a `(Control, Control)` pair, structured as
/// conflicts. Mirrors [`is_control_subtype`] but emits conflicts
/// instead of a single `Result`.
#[allow(
    clippy::too_many_lines,
    reason = "control-operator compatibility matrix is best read in one place"
)]
fn collect_control_conflicts(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
    path: &mut Vec<PathSegment>,
    conflicts: &mut Vec<WithinConflict>,
) {
    let (
        ResolvedType::Control {
            op: lop,
            carrier: lcarrier,
            controller: lctrl,
        },
        ResolvedType::Control {
            op: rop,
            carrier: rcarrier,
            controller: rctrl,
        },
    ) = (lhs, rhs)
    else {
        push_conflict(
            conflicts,
            path,
            WithinConflictKind::DifferentStructure,
            Some(lhs.clone()),
            Some(rhs.clone()),
            format!(
                "control subtype check on non-Control types: {} vs {}",
                type_name(lhs),
                type_name(rhs)
            ),
        );
        return;
    };

    path.push(PathSegment::ControlOp(lop.clone()));
    collect_subtype_conflicts_inner(lcarrier, rcarrier, defs, visited, path, conflicts);
    #[allow(clippy::unnested_or_patterns)]
    match (lop, rop) {
        (a, b) if a == b => {
            collect_subtype_conflicts_inner(lctrl, rctrl, defs, visited, path, conflicts);
        },
        (ControlOp::Dtrm, ControlOp::Cbor)
        | (ControlOp::Dtrm, ControlOp::Prefp)
        | (ControlOp::Prefp, ControlOp::Cbor)
        | (ControlOp::DtrmSeq, ControlOp::CborSeq)
        | (ControlOp::DtrmSeq, ControlOp::PrefpSeq)
        | (ControlOp::PrefpSeq, ControlOp::CborSeq) => {
            collect_subtype_conflicts_inner(lctrl, rctrl, defs, visited, path, conflicts);
        },
        (ControlOp::Cbor, ControlOp::Prefp) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".cbor is broader than .prefp".to_owned(),
            );
        },
        (ControlOp::Cbor, ControlOp::Dtrm) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".cbor is broader than .dtrm".to_owned(),
            );
        },
        (ControlOp::Prefp, ControlOp::Dtrm) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".prefp is broader than .dtrm".to_owned(),
            );
        },
        (ControlOp::CborSeq, ControlOp::PrefpSeq) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".cborseq is broader than .prefpseq".to_owned(),
            );
        },
        (ControlOp::CborSeq, ControlOp::DtrmSeq) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".cborseq is broader than .dtrmseq".to_owned(),
            );
        },
        (ControlOp::PrefpSeq, ControlOp::DtrmSeq) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".prefpseq is broader than .dtrmseq".to_owned(),
            );
        },
        // Compression annotation compatibility matrix (mirrors
        // `is_control_subtype`):
        // * Named algorithm ⊆ `.x-compressed`: check controller.
        // * `.x-compressed` is not ⊆ a named algorithm.
        // * Two different named algorithms are not mutually within each other.
        (named, ControlOp::XCompressed) if named.is_compression_named() => {
            collect_subtype_conflicts_inner(lctrl, rctrl, defs, visited, path, conflicts);
        },
        (ControlOp::XCompressed, named) if named.is_compression_named() => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                ".x-compressed is broader than a named compression algorithm".to_owned(),
            );
        },
        (a, b) if a.is_compression_named() && b.is_compression_named() => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                format!(
                    "compression algorithm {} is not within {}",
                    a.as_text(),
                    b.as_text()
                ),
            );
        },
        // Encryption wrapper compatibility matrix (Step 5.11):
        // * `.x-enc` is within `.x-enc` (already covered by `a == b`).
        // * `.x-enc` is not within `.x-hash`, `.x-compressed`, or any named compression algorithm.
        // * `.x-hash` is not within `.x-enc`.
        // * The compression family does not subtype the encryption family and vice versa.
        (ControlOp::XEnc, ControlOp::XHash | _) | (ControlOp::XHash, ControlOp::XEnc)
            if lop.is_encryption()
                || rop.is_encryption()
                || (lop.is_hash_annotation() && rop.is_hash_annotation()) =>
        {
            push_encryption_hash_conflict(conflicts, path, lop, rop, lhs, rhs);
        },
        (a, b) => {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::ControlMismatch,
                Some(lhs.clone()),
                Some(rhs.clone()),
                format!(
                    "control operator {} is not within {}",
                    a.as_text(),
                    b.as_text()
                ),
            );
        },
    }
    path.pop();
}

/// Emit a control-mismatch diagnostic for an `.x-enc` / `.x-hash`
/// incompatibility.  The two wrappers belong to distinct transform
/// families (encryption vs hash) and are never mutually within each
/// other; an `.x-enc` value is also not within any compression
/// annotation and vice versa.
fn push_encryption_hash_conflict(
    conflicts: &mut Vec<WithinConflict>,
    path: &[PathSegment],
    lop: &ControlOp,
    rop: &ControlOp,
    lhs: &ResolvedType,
    rhs: &ResolvedType,
) {
    let reason = match (lop, rop) {
        (ControlOp::XEnc, ControlOp::XHash) => ".x-enc is not within .x-hash".to_owned(),
        (ControlOp::XHash, ControlOp::XEnc) => ".x-hash is not within .x-enc".to_owned(),
        (ControlOp::XEnc, _) | (_, ControlOp::XEnc) => {
            ".x-enc is not within a non-encryption transform".to_owned()
        },
        (ControlOp::XHash, _) | (_, ControlOp::XHash) => {
            ".x-hash is not within a non-hash transform".to_owned()
        },
        _ => {
            format!(
                "control operator {} is not within {}",
                lop.as_text(),
                rop.as_text()
            )
        },
    };
    push_conflict(
        conflicts,
        path,
        WithinConflictKind::ControlMismatch,
        Some(lhs.clone()),
        Some(rhs.clone()),
        reason,
    );
}

/// Map subtyping for a `(Map, Map)` pair, structured as conflicts.
fn collect_map_conflicts(
    le: &[MapEntry],
    re: &[MapEntry],
    defs: &DefinitionMap,
    path: &mut Vec<PathSegment>,
    conflicts: &mut Vec<WithinConflict>,
) {
    let expanded_lhs = expand_map_sockets(le, defs);
    let expanded_rhs = expand_map_sockets(re, defs);

    // Backward check: every RHS required entry must have at least
    // its minimum LHS matches. The TooManyMatches check is skipped
    // here because it produces false positives when the LHS provides
    // multiple concrete entries (e.g. `(1: T), (4: T), (5: T)`) that
    // happen to all be subtypes of a single RHS schema entry — RFC9581
    // is the canonical example.
    for (i, re_entry) in expanded_rhs.iter().enumerate() {
        let match_count: u32 = expanded_lhs
            .iter()
            .filter(|le_entry| map_entry_matches(le_entry, re_entry, defs))
            .count()
            .try_into()
            .unwrap_or(u32::MAX);

        let re_min = re_entry.occurrence.min();

        if match_count < re_min {
            let detail = best_rhs_missing_detail(&expanded_lhs, re_entry, defs);
            let reason = if detail.is_empty() {
                format!(
                    "map[{i}]: expected at least {re_min} matching entries, found {match_count}"
                )
            } else {
                format!(
                    "map[{i}]: expected at least {re_min} matching entries, found {match_count}; {detail}"
                )
            };
            path.push(PathSegment::MapEntry(i));
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::MissingRequiredRhs,
                None,
                Some(re_entry.value.clone()),
                reason,
            );
            path.pop();
        }
    }

    // Subtype check is symmetric: LHS may not have required entries
    // that the RHS does not require.
    for (i, le_entry) in expanded_lhs.iter().enumerate() {
        let le_min = le_entry.occurrence.min();
        if le_min == 0 {
            continue;
        }
        let match_count: u32 = expanded_rhs
            .iter()
            .filter(|re_entry| map_entry_matches(le_entry, re_entry, defs))
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        if match_count == 0 {
            let detail = best_lhs_rejected_detail(le_entry, &expanded_rhs, defs);
            let reason = if detail.is_empty() {
                format!("map[{i}]: LHS required entry has no matching RHS entry")
            } else {
                format!("map[{i}]: LHS required entry has no matching RHS entry; {detail}")
            };
            path.push(PathSegment::MapEntry(i));
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::LhsNotAccepted,
                Some(le_entry.value.clone()),
                None,
                reason,
            );
            path.pop();
        }
    }
}

/// Explain why a required RHS map entry did not find enough LHS
/// matches. This is diagnostic-only: it does not change subtype
/// semantics. Prefer a candidate whose key matches but whose value
/// fails, because that points at the field the user recognizes.
fn best_rhs_missing_detail(
    lhs_entries: &[MapEntry],
    rhs_entry: &MapEntry,
    defs: &DefinitionMap,
) -> String {
    for (idx, lhs_entry) in lhs_entries.iter().enumerate() {
        if subtype_conflicts(&lhs_entry.key, &rhs_entry.key, defs).is_empty() {
            let value_conflicts = subtype_conflicts(&lhs_entry.value, &rhs_entry.value, defs);
            if let Some(conflict) = value_conflicts.first() {
                return format!(
                    "nearest LHS map[{idx}] has a compatible key but its value is rejected: {}",
                    conflict.reason
                );
            }
        }
    }

    for (idx, lhs_entry) in lhs_entries.iter().enumerate() {
        let key_conflicts = subtype_conflicts(&lhs_entry.key, &rhs_entry.key, defs);
        if let Some(conflict) = key_conflicts.first() {
            return format!(
                "nearest LHS map[{idx}] key {} is not accepted by RHS key {}: {}",
                render_type(&lhs_entry.key),
                render_type(&rhs_entry.key),
                conflict.reason
            );
        }
    }

    String::new()
}

/// Explain why a required LHS map entry was not accepted by any RHS
/// map entry. This is diagnostic-only and mirrors
/// [`best_rhs_missing_detail`].
fn best_lhs_rejected_detail(
    lhs_entry: &MapEntry,
    rhs_entries: &[MapEntry],
    defs: &DefinitionMap,
) -> String {
    for (idx, rhs_entry) in rhs_entries.iter().enumerate() {
        if subtype_conflicts(&lhs_entry.key, &rhs_entry.key, defs).is_empty() {
            let value_conflicts = subtype_conflicts(&lhs_entry.value, &rhs_entry.value, defs);
            if let Some(conflict) = value_conflicts.first() {
                return format!(
                    "nearest RHS map[{idx}] accepts the key but rejects the value: {}",
                    conflict.reason
                );
            }
        }
    }

    for (idx, rhs_entry) in rhs_entries.iter().enumerate() {
        let key_conflicts = subtype_conflicts(&lhs_entry.key, &rhs_entry.key, defs);
        if let Some(conflict) = key_conflicts.first() {
            return format!(
                "nearest RHS map[{idx}] key {} does not accept LHS key {}: {}",
                render_type(&rhs_entry.key),
                render_type(&lhs_entry.key),
                conflict.reason
            );
        }
    }

    String::new()
}

/// Array subtyping with occurrence-aware trailing-repeat support.
///
/// When the RHS has a trailing element whose occurrence is `*` (zero
/// or more) or `+` (one or more), it absorbs any extra LHS elements
/// beyond the fixed-length prefix. This fixes the case where LHS has
/// more elements than RHS but the RHS's last element is a repeat
/// pattern.
#[allow(
    clippy::too_many_arguments,
    reason = "collecting context for detailed diagnostics"
)]
#[allow(
    clippy::too_many_lines,
    reason = "combined prefix match and trailing repeat logic in one pass"
)]
fn collect_array_conflicts(
    le: &[ArrayElement],
    re: &[ArrayElement],
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
    path: &mut Vec<PathSegment>,
    conflicts: &mut Vec<WithinConflict>,
) {
    // A trailing RHS element whose occurrence is a repeat (ZeroOrMore,
    // OneOrMore, or Range) can absorb consecutive LHS elements.
    let trailing_repeat = re.last().filter(|re_last| {
        matches!(
            re_last.occurrence,
            Occurrence::ZeroOrMore | Occurrence::OneOrMore | Occurrence::Range { .. }
        )
    });

    let fixed_re = if trailing_repeat.is_some() {
        re.len().saturating_sub(1)
    } else {
        re.len()
    };

    // If there is no trailing repeat and LHS is longer than RHS,
    // the LHS cannot be a subtype.
    if trailing_repeat.is_none() && le.len() > re.len() {
        push_conflict(
            conflicts,
            path,
            WithinConflictKind::DifferentStructure,
            Some(lhs.clone()),
            Some(rhs.clone()),
            format!("array len {} > rhs len {}", le.len(), re.len()),
        );
        return;
    }

    // Match the fixed-length prefix one-to-one.
    let match_len = le.len().min(fixed_re);
    for (idx, la) in le.iter().take(match_len).enumerate() {
        let Some(ra) = re.get(idx) else {
            break;
        };
        if !occurrence_compatible(la.occurrence, ra.occurrence) {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::DifferentStructure,
                None,
                None,
                format!(
                    "array[{idx}]: occurrence {:?} not compatible with {:?}",
                    la.occurrence, ra.occurrence
                ),
            );
            return;
        }
        path.push(PathSegment::ArrayIndex(idx));
        collect_subtype_conflicts_inner(&la.ty, &ra.ty, defs, visited, path, conflicts);
        path.pop();
    }

    // Any remaining LHS elements beyond the fixed prefix are absorbed
    // by the trailing RHS repeat. They must all be subtypes of the
    // repeat element's type.
    if le.len() > fixed_re {
        if let Some(repeat_elem) = trailing_repeat {
            let repeat_min = repeat_elem.occurrence.min();
            let repeat_count: u32 = le
                .len()
                .saturating_sub(fixed_re)
                .try_into()
                .unwrap_or(u32::MAX);
            if repeat_count < repeat_min {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::DifferentStructure,
                    None,
                    None,
                    format!(
                        "trailing repeat expected at least {repeat_min} matches, found {repeat_count}"
                    ),
                );
                return;
            }
            if let (Some(repeat_max), true) = (
                repeat_elem.occurrence.max(),
                repeat_count > repeat_elem.occurrence.max().unwrap_or(u32::MAX),
            ) {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::TooManyMatches,
                    None,
                    None,
                    format!(
                        "trailing repeat expected at most {repeat_max} matches, found {repeat_count}"
                    ),
                );
                return;
            }
            for (j, la) in le.iter().skip(fixed_re).enumerate() {
                let inner_idx = fixed_re.saturating_add(j);
                path.push(PathSegment::ArrayIndex(inner_idx));
                collect_subtype_conflicts_inner(
                    &la.ty,
                    &repeat_elem.ty,
                    defs,
                    visited,
                    path,
                    conflicts,
                );
                path.pop();
            }
        } else {
            push_conflict(
                conflicts,
                path,
                WithinConflictKind::DifferentStructure,
                None,
                None,
                format!(
                    "unconsumed LHS elements after RHS pattern (len {} > {})",
                    le.len(),
                    re.len()
                ),
            );
        }
    }

    // RHS elements beyond the LHS length (no trailing repeat case)
    // must be optional or zero-or-more.
    if trailing_repeat.is_none() {
        for (i, ra) in re.iter().enumerate().skip(le.len()) {
            if ra.occurrence.min() > 0 {
                push_conflict(
                    conflicts,
                    path,
                    WithinConflictKind::DifferentStructure,
                    None,
                    None,
                    format!("array[{i}]: missing LHS element for required RHS entry"),
                );
            }
        }
    }
}

/// Check whether a candidate LHS map entry is a subtype of a candidate
/// RHS map entry, with the key and value subtype check delegated to
/// the structured collector. Used by [`collect_map_conflicts`].
fn map_entry_matches(
    le_entry: &MapEntry,
    re_entry: &MapEntry,
    defs: &DefinitionMap,
) -> bool {
    if let ResolvedType::Socket { name } = &le_entry.key {
        let lhs_entries = socket_choice_entries(name, defs);
        if !lhs_entries.is_empty() {
            return lhs_entries
                .iter()
                .all(|lhs_entry| map_entry_matches(lhs_entry, re_entry, defs));
        }
    }

    if let ResolvedType::Socket { name } = &re_entry.key {
        let rhs_entries = socket_choice_entries(name, defs);
        if !rhs_entries.is_empty() {
            return rhs_entries
                .iter()
                .any(|rhs_entry| map_entry_matches(le_entry, rhs_entry, defs));
        }
    }

    subtype_conflicts(&le_entry.key, &re_entry.key, defs).is_empty()
        && subtype_conflicts(&le_entry.value, &re_entry.value, defs).is_empty()
}

/// Check that a `ResolvedType::Control` on the LHS is a subtype of a
/// `ResolvedType::Control` on the RHS.
///
/// Both sides must be `Control`; the caller is responsible for the
/// routing. The function first checks carrier compatibility, then the
/// operator compatibility matrix:
///
/// * Equal operators: the controller must subtype.
/// * `.dtrm ⊆ .cbor`: the controller must subtype (deterministic is a subset of general
///   CBOR).
/// * `.dtrmseq ⊆ .cborseq`: same directionality as `.dtrm ⊆ .cbor`.
/// * Reverse directions fail with a specific reason string so the diff renderer can quote
///   the operator names.
/// * `ControlOp::Other` is not assumed compatible across operators; the only valid
///   direction for two unknown operators is identical text.
///
/// Reference: `crates/cbork/plan.md` § Step 2.
fn is_control_subtype(
    lhs: &ResolvedType,
    rhs: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<(String, String)>,
) -> Result<(), String> {
    let (
        ResolvedType::Control {
            op: lop,
            carrier: lcarrier,
            controller: lctrl,
        },
        ResolvedType::Control {
            op: rop,
            carrier: rcarrier,
            controller: rctrl,
        },
    ) = (lhs, rhs)
    else {
        return Err(format!(
            "is_control_subtype called with non-Control types: {} vs {}",
            type_name(lhs),
            type_name(rhs)
        ));
    };

    if matches!(lop, ControlOp::Within) {
        return is_subtype_impl(lcarrier, rhs, defs, visited);
    }
    if matches!(rop, ControlOp::Within) {
        return is_subtype_impl(lhs, rcarrier, defs, visited);
    }

    // 1. Carrier compatibility: the carrier of the LHS must fit inside the carrier of the
    //    RHS. For serialization operators the carriers are usually the same primitive (`bstr`
    //    for `.cbor`/`.dtrm`); the recursive call also handles user-defined carrier types.
    is_subtype_impl(lcarrier, rcarrier, defs, visited).map_err(|e| format!("carrier: {e}"))?;

    // 2. Operator compatibility matrix.
    #[allow(clippy::unnested_or_patterns)]
    match (lop, rop) {
        (a, b) if a == b => {
            is_subtype_impl(lctrl, rctrl, defs, visited).map_err(|e| format!("controller: {e}"))
        },
        (ControlOp::Dtrm, ControlOp::Cbor)
        | (ControlOp::Dtrm, ControlOp::Prefp)
        | (ControlOp::Prefp, ControlOp::Cbor)
        | (ControlOp::DtrmSeq, ControlOp::CborSeq)
        | (ControlOp::DtrmSeq, ControlOp::PrefpSeq)
        | (ControlOp::PrefpSeq, ControlOp::CborSeq) => {
            is_subtype_impl(lctrl, rctrl, defs, visited).map_err(|e| format!("controller: {e}"))
        },
        (ControlOp::Cbor, ControlOp::Prefp) => Err(".cbor is broader than .prefp".to_owned()),
        (ControlOp::Cbor, ControlOp::Dtrm) => Err(".cbor is broader than .dtrm".to_owned()),
        (ControlOp::Prefp, ControlOp::Dtrm) => Err(".prefp is broader than .dtrm".to_owned()),
        (ControlOp::CborSeq, ControlOp::PrefpSeq) => {
            Err(".cborseq is broader than .prefpseq".to_owned())
        },
        (ControlOp::CborSeq, ControlOp::DtrmSeq) => {
            Err(".cborseq is broader than .dtrmseq".to_owned())
        },
        (ControlOp::PrefpSeq, ControlOp::DtrmSeq) => {
            Err(".prefpseq is broader than .dtrmseq".to_owned())
        },
        // Compression annotation compatibility matrix:
        // * A named algorithm (`.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate`) is within the generic
        //   `.x-compressed` (the carrier is compatible and the controller must subtype).
        // * The generic `.x-compressed` is *not* within a named algorithm: the generic is broader
        //   than any specific algorithm.
        // * Two different named algorithms are not mutually within each other: a brotli-compressed
        //   value is not a zstd-compressed value.
        (named, ControlOp::XCompressed) if named.is_compression_named() => {
            is_subtype_impl(lctrl, rctrl, defs, visited).map_err(|e| format!("controller: {e}"))
        },
        (ControlOp::XCompressed, named) if named.is_compression_named() => {
            Err(".x-compressed is broader than a named compression algorithm".to_owned())
        },
        (a, b) if a.is_compression_named() && b.is_compression_named() => {
            Err(format!(
                "compression algorithm {} is not within {}",
                a.as_text(),
                b.as_text()
            ))
        },
        // Encryption / hash wrapper compatibility matrix (Step 5.11):
        // * `.x-enc` is not within `.x-hash`, `.x-compressed`, or any named compression algorithm.
        // * `.x-hash` is not within `.x-enc`, `.x-compressed`, or any named compression algorithm.
        (lop, rop)
            if lop.is_encryption()
                || rop.is_encryption()
                || (lop.is_hash_annotation() && rop.is_hash_annotation()) =>
        {
            Err(encryption_hash_reason(lop, rop))
        },
        (a, b) => {
            Err(format!(
                "control operator {} is not within {}",
                a.as_text(),
                b.as_text()
            ))
        },
    }
}

/// Human-readable explanation for an `.x-enc` / `.x-hash`
/// incompatibility.  Mirrors [`push_encryption_hash_conflict`].
fn encryption_hash_reason(
    lop: &ControlOp,
    rop: &ControlOp,
) -> String {
    match (lop, rop) {
        (ControlOp::XEnc, ControlOp::XHash) => ".x-enc is not within .x-hash".to_owned(),
        (ControlOp::XHash, ControlOp::XEnc) => ".x-hash is not within .x-enc".to_owned(),
        (ControlOp::XEnc, _) | (_, ControlOp::XEnc) => {
            ".x-enc is not within a non-encryption transform".to_owned()
        },
        (ControlOp::XHash, _) | (_, ControlOp::XHash) => {
            ".x-hash is not within a non-hash transform".to_owned()
        },
        _ => {
            format!(
                "control operator {} is not within {}",
                lop.as_text(),
                rop.as_text()
            )
        },
    }
}

/// Unwrap a single-element `Array` wrapper to get the inner type.
///
/// Socket choices like `(key => value)` parse as `Array([Map(...)])`.
/// This extracts the inner `Map`.
#[allow(clippy::indexing_slicing, reason = "guarded by len check")]
fn unwrap_single_array(ty: &ResolvedType) -> &ResolvedType {
    if let ResolvedType::Array { elements } = ty
        && elements.len() == 1
    {
        &elements[0].ty
    } else {
        ty
    }
}

/// Expand resolvable named map entries while preserving socket-keyed
/// entries as choices. Socket plugs in maps are alternatives; selecting
/// only the first arm would make later concrete arms fail `.within`.
fn expand_map_sockets(
    entries: &[MapEntry],
    defs: &DefinitionMap,
) -> Vec<MapEntry> {
    let mut expanded: Vec<MapEntry> = Vec::new();
    for entry in entries {
        if let ResolvedType::Socket { name } = &entry.key {
            if defs.socket_choices_for(name).is_some() {
                expanded.push(entry.clone());
                continue;
            }
            // Fallback: resolve via deep named resolution
            let resolved = resolve_named_deep(&entry.key, defs, &mut HashSet::new());
            if !matches!(
                &resolved,
                ResolvedType::Socket { .. } | ResolvedType::Named(_)
            ) {
                flatten_def_entries(&resolved, &entry.occurrence, &mut expanded);
                continue;
            }
        }
        expanded.push(entry.clone());
    }
    expanded
}

/// Return all concrete map entries exposed by a group socket's plug choices.
fn socket_choice_entries(
    name: &str,
    defs: &DefinitionMap,
) -> Vec<MapEntry> {
    let mut entries = Vec::new();
    let Some(choices) = defs.socket_choices_for(name) else {
        return entries;
    };

    for choice in choices {
        collect_choice_map_entries(choice, &Occurrence::One, &mut entries);
    }

    entries
}

/// Collect concrete entries from a resolved group/socket choice arm.
fn collect_choice_map_entries(
    ty: &ResolvedType,
    parent_occ: &Occurrence,
    entries: &mut Vec<MapEntry>,
) {
    match unwrap_single_array(ty) {
        ResolvedType::Map {
            entries: choice_entries,
        } => {
            for entry in choice_entries {
                let mut flat_entry = entry.clone();
                if flat_entry.occurrence == Occurrence::One {
                    flat_entry.occurrence = *parent_occ;
                }
                entries.push(flat_entry);
            }
        },
        ResolvedType::Choice(alts) => {
            for alt in alts {
                collect_choice_map_entries(alt, parent_occ, entries);
            }
        },
        _ => {},
    }
}

/// Render a [`ResolvedType`] as a human-readable CDDL-like string.
///
/// Used in diagnostic messages to show the resolved LHS and RHS of a
/// failed `.within` check.
#[must_use]
pub(crate) fn render_type(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::Any => "any".to_owned(),
        ResolvedType::Primitive(k) => primitive_name(*k).to_owned(),
        ResolvedType::Range {
            lo,
            hi,
            is_float: _,
        } => {
            let lo_str = lo.map_or(String::new(), |v| v.to_string());
            let hi_str = hi.map_or(String::new(), |v| v.to_string());
            format!("{lo_str}..{hi_str}")
        },
        ResolvedType::Tag { tag, inner } => {
            format!("#6.{tag}({})", render_type(inner))
        },
        ResolvedType::Array { elements } => {
            let parts: Vec<String> = elements
                .iter()
                .map(|e| {
                    let occ = occurrence_prefix(e.occurrence);
                    format!("{occ}{}", render_type(&e.ty))
                })
                .collect();
            format!("[{}]", parts.join(", "))
        },
        ResolvedType::Map { entries } => {
            let parts: Vec<String> = entries
                .iter()
                .map(|e| {
                    let occ = occurrence_prefix(e.occurrence);
                    format!("{occ}{} => {}", render_type(&e.key), render_type(&e.value))
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        },
        ResolvedType::Choice(alts) => {
            let parts: Vec<String> = alts.iter().map(render_type).collect();
            parts.join(" / ")
        },
        ResolvedType::Control {
            op,
            carrier,
            controller,
        } => {
            format!(
                "{} {} {}",
                render_type(carrier),
                op.as_text(),
                render_type(controller),
            )
        },
        ResolvedType::Intersection(operands) => {
            let parts: Vec<String> = operands.iter().map(render_type).collect();
            format!("({})", parts.join(" .and "))
        },
        ResolvedType::Socket { name } | ResolvedType::Named(name) => name.clone(),
        ResolvedType::TextKey(s) => format!("\"{}\"", s.escape_debug()),
    }
}

/// Return a short prefix string for an occurrence specifier.
fn occurrence_prefix(occ: Occurrence) -> String {
    match occ {
        Occurrence::One => String::new(),
        Occurrence::Optional => "? ".to_owned(),
        Occurrence::ZeroOrMore => "* ".to_owned(),
        Occurrence::OneOrMore => "+ ".to_owned(),
        Occurrence::Range { lo, hi } => format!("{lo}*{hi} "),
    }
}

/// Return the CDDL name for a primitive kind.
const fn primitive_name(k: PrimitiveKind) -> &'static str {
    match k {
        PrimitiveKind::Int => "int",
        PrimitiveKind::Uint => "uint",
        PrimitiveKind::Nint => "nint",
        PrimitiveKind::Tstr => "tstr",
        PrimitiveKind::Bstr => "bstr",
        PrimitiveKind::Bool => "bool",
        PrimitiveKind::Nil => "nil",
        PrimitiveKind::Float => "float",
        PrimitiveKind::Float16 => "float16",
        PrimitiveKind::Float32 => "float32",
        PrimitiveKind::Float64 => "float64",
        PrimitiveKind::Undefined => "undefined",
    }
}

/// Flatten a resolved definition type into map entries.
///
/// Handles both direct `Map` types and `Choice` of `Map` entries (produced
/// when a parenthesized group is resolved through `resolve_type_choice`).
fn flatten_def_entries(
    def_type: &ResolvedType,
    parent_occ: &Occurrence,
    expanded: &mut Vec<MapEntry>,
) {
    match def_type {
        ResolvedType::Map { entries } => {
            for de in entries {
                let mut flat_entry = de.clone();
                if flat_entry.occurrence == Occurrence::One {
                    flat_entry.occurrence = *parent_occ;
                }
                expanded.push(flat_entry);
            }
        },
        ResolvedType::Choice(alts) => {
            for alt in alts {
                if let ResolvedType::Map { entries } = alt {
                    for de in entries {
                        let mut flat_entry = de.clone();
                        if flat_entry.occurrence == Occurrence::One {
                            flat_entry.occurrence = *parent_occ;
                        }
                        expanded.push(flat_entry);
                    }
                }
            }
        },
        _ => {
            // Single unwrapped type — keep as-is
        },
    }
}

/// Look up a definition by suffix match (for prefixed import names).
fn defs_find_suffix<'a>(
    defs: &'a DefinitionMap,
    name: &str,
) -> Option<&'a WrappedNode> {
    // Try exact match first
    if let Some(node) = defs.get(name) {
        return Some(node);
    }
    // Try suffix match: any key ending with ".name"
    let suffix = format!(".{name}");
    let keys: Vec<String> = defs.definitions.keys().cloned().collect();
    for key in keys {
        if key.ends_with(&suffix) {
            return defs.get(&key);
        }
    }
    None
}

/// Return a human-readable name for a [`ResolvedType`] variant.
fn type_name(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::Any => "any".to_owned(),
        ResolvedType::Primitive(k) => format!("{k:?}"),
        ResolvedType::Range {
            lo,
            hi,
            is_float: _,
        } => format!("{lo:?}..{hi:?}"),
        ResolvedType::Tag { tag, inner: _ } => format!("#6.{tag}(...)"),
        ResolvedType::Array { elements } => format!("[{} elements]", elements.len()),
        ResolvedType::Map { entries } => format!("{{ {} entries }}", entries.len()),
        ResolvedType::Choice(alts) => format!("choice({} arms)", alts.len()),
        ResolvedType::Control { op, .. } => format!("control({})", op.as_text()),
        ResolvedType::Intersection(operands) => format!("intersection({} arms)", operands.len()),
        ResolvedType::Socket { name } | ResolvedType::Named(name) => name.clone(),
        ResolvedType::TextKey(s) => format!("\"{s}\""),
    }
}

// ---------------------------------------------------------------------------
// Pass entry point
// ---------------------------------------------------------------------------

/// Validate all `.within` control operators in the complete node tree.
///
/// Walks the tree looking for `type1` nodes containing a `ctlop` with text
/// `.within`. For each, resolves the LHS and RHS types and checks that
/// `lhs ⊆ rhs`. Sockets on the LHS are resolved by collecting all their
/// `/=` augmentations from the full tree.
pub(crate) fn validate_within_pass(
    nodes: &[WrappedNode],
    warnings: &mut Vec<Diagnostic>,
) {
    let defs = DefinitionMap::from_nodes(nodes);
    let resolution = concrete::build_resolution(nodes);
    let policy = ConcretePolicy::for_render();
    let ctx = WithinContext {
        defs: &defs,
        resolution: &resolution,
        policy: &policy,
        all_nodes: nodes,
    };
    validate_within_visit(nodes, &ctx, warnings);
    // Surface render-only diagnostics (e.g. group-reference cycle
    // detection from Step 5.10).  These are produced by the renderer
    // during effective-view construction but are not visible to the
    // caller otherwise.
    warnings.extend(resolution.take_render_diagnostics());
}

/// Recurse through nodes looking for `.within` operators.
fn validate_within_visit(
    nodes: &[WrappedNode],
    ctx: &WithinContext<'_>,
    warnings: &mut Vec<Diagnostic>,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                if ruleline_has_generic_params(children) {
                    validate_within_nested_rule_children(children, ctx, warnings);
                } else {
                    validate_within_in_children(children, ctx, warnings);
                }
            },
            WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
                validate_within_visit(children, ctx, warnings);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

/// Open generic rule lines are templates, but parser wrapping can leave
/// following concrete rules nested beneath them. Skip the template
/// expression while still validating nested concrete definitions.
fn validate_within_nested_rule_children(
    children: &[WrappedNode],
    ctx: &WithinContext<'_>,
    warnings: &mut Vec<Diagnostic>,
) {
    for child in children {
        validate_within_nested_rule_descendants(child, ctx, warnings);
    }
}

/// Recursively search for concrete rule/directive descendants under an
/// open generic template without validating the template expression
/// itself.
fn validate_within_nested_rule_descendants(
    node: &WrappedNode,
    ctx: &WithinContext<'_>,
    warnings: &mut Vec<Diagnostic>,
) {
    match node {
        WrappedNode::RuleLine { .. } => {
            validate_within_visit(std::slice::from_ref(node), ctx, warnings);
        },
        WrappedNode::Directive { children, .. } | WrappedNode::Syntax { children, .. } => {
            for child in children {
                validate_within_nested_rule_descendants(child, ctx, warnings);
            }
        },
        WrappedNode::Comment { .. }
        | WrappedNode::ModuleStart { .. }
        | WrappedNode::ModuleEnd { .. } => {},
    }
}

/// Return whether a rule line declares generic parameters.
///
/// Open generic definitions are templates. A `.within` inside such a
/// template cannot always be proven until a concrete instantiation
/// substitutes the formal parameters, so validation happens at the
/// expanded call sites instead.
fn ruleline_has_generic_params(children: &[WrappedNode]) -> bool {
    children.iter().any(rule_expr_has_genericparm)
}

/// Return whether a rule line's own `expr` declares generic parameters.
fn rule_expr_has_genericparm(node: &WrappedNode) -> bool {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return false;
    };
    if rule != "expr" {
        return false;
    }
    children
        .iter()
        .any(|child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "genericparm"))
}

/// Walk children looking for `.within` ctlops in `type1` nodes.
#[allow(
    clippy::indexing_slicing,
    reason = "slicing guarded by ctlop_idx check"
)]
fn validate_within_in_children(
    children: &[WrappedNode],
    ctx: &WithinContext<'_>,
    warnings: &mut Vec<Diagnostic>,
) {
    for child in children {
        if let WrappedNode::Syntax {
            rule,
            children: type_children,
            ..
        } = child
            && rule == "type1"
        {
            let mut ctlop_idx: Option<usize> = None;
            for (i, tc) in type_children.iter().enumerate() {
                if let WrappedNode::Syntax {
                    rule: child_rule,
                    text,
                    ..
                } = tc
                    && child_rule == "ctlop"
                    && text.trim() == ".within"
                {
                    ctlop_idx = Some(i);
                    break;
                }
            }

            if let Some(idx) = ctlop_idx {
                let lhs_type2 = type_children[..idx]
                    .iter()
                    .rev()
                    .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type2"));
                let rhs_type2 = type_children[idx.wrapping_add(1)..]
                    .iter()
                    .find(|c| matches!(c, WrappedNode::Syntax { rule, .. } if rule == "type2"));

                if let (Some(lhs_node), Some(rhs_node)) = (lhs_type2, rhs_type2) {
                    check_within_constraint(lhs_node, rhs_node, ctx, child, warnings);
                }
            }
        }
        if let WrappedNode::Syntax {
            children: inner, ..
        } = child
        {
            validate_within_in_children(inner, ctx, warnings);
        }
    }
}

/// Check a single `.within` constraint, emitting diagnostics on failure.
fn check_within_constraint(
    lhs_node: &WrappedNode,
    rhs_node: &WrappedNode,
    ctx: &WithinContext<'_>,
    ctlop_node: &WrappedNode,
    warnings: &mut Vec<Diagnostic>,
) {
    let lhs_type = resolve_type(lhs_node);
    let rhs_type = resolve_type(rhs_node);

    let lhs_resolved = if let ResolvedType::Named(name) = &lhs_type
        && is_socket_name(name)
    {
        let choices = collect_socket_choices(name, ctx.all_nodes);
        if choices.is_empty() {
            lhs_type
        } else {
            ResolvedType::Choice(choices)
        }
    } else {
        lhs_type
    };

    let conflicts = subtype_conflicts(&lhs_resolved, &rhs_type, ctx.defs);
    if conflicts.is_empty() {
        return;
    }

    let origin = ctlop_node.origin().clone();
    let span = match ctlop_node {
        WrappedNode::Syntax { span, .. } => span.clone(),
        _ => 0..0,
    };
    // Render the LHS and RHS using the concrete renderer so the
    // user sees real CDDL (folded constants, expanded sockets)
    // instead of the lossy `render_type(ResolvedType)` text.
    let lhs_concrete_str =
        concrete::render_subtree(lhs_node, ctx.resolution, &ConcretePolicy::for_lhs()).to_cddl();
    let rhs_concrete_str =
        concrete::render_subtree(rhs_node, ctx.resolution, &ConcretePolicy::for_rhs()).to_cddl();

    // Build the schema diff. If it produces at least one line, use
    // it as the primary diagnostic stream. Otherwise fall back to
    // the legacy LHS/RHS + per-conflict subdiag blocks.
    let diff = schema_diff::build_schema_diff(lhs_node, rhs_node, &conflicts, ctx.resolution);

    let related = build_within_related(
        &RelatedInputs {
            lhs_node,
            rhs_node,
            origin: &origin,
            lhs_concrete: lhs_concrete_str,
            rhs_concrete: rhs_concrete_str,
        },
        &diff,
        &conflicts,
    );

    // We already bailed out if conflicts is empty, so unwrap is safe.
    #[allow(clippy::indexing_slicing, reason = "guarded by is_empty check above")]
    let first = &conflicts[0];
    warnings.push(Diagnostic {
        code: "E030",
        level: DiagnosticLevel::Error,
        message: format!(
            ".within subtype check failed:\n{}",
            format_within_reason(&first.reason),
        ),
        source_file: Some(origin.source_path.clone()),
        span: Some(span),
        previous_origin: None,
        related,
    });
}

/// Format a nested subtype reason so complex `.within` failures are
/// readable in the top-level diagnostic message.
fn format_within_reason(reason: &str) -> String {
    let lines = format_within_reason_lines(reason);
    if let Some(line) = lines.first()
        && lines.len() == 1
    {
        return format!("  reason: {line}");
    }

    let mut out = String::from("  reason:\n");
    for line in lines {
        out.push_str("    ");
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// Break a nested subtype reason into physical lines for readable
/// diagnostic rendering.
fn format_within_reason_lines(reason: &str) -> Vec<String> {
    let Some((prefix, choices)) = reason.split_once(" not subtype of any choice arm: [") else {
        return split_nearest_reason_lines(reason);
    };
    let choices = choices.strip_suffix(']').unwrap_or(choices);

    let (context, subject) = prefix
        .rsplit_once(": ")
        .map_or((prefix, ""), |(context, subject)| (context, subject));

    let mut out = split_nearest_reason_lines(context);
    let choice_header = if subject.is_empty() {
        "not subtype of any choice arm:".to_owned()
    } else {
        format!("{subject} not subtype of any choice arm:")
    };
    out.push(choice_header);
    for choice in split_choice_reasons(choices) {
        out.push(format!("  - {}", choice.trim()));
    }
    out
}

/// Split `; nearest ...` suffixes into separate reason lines.
fn split_nearest_reason_lines(reason: &str) -> Vec<String> {
    let Some((first, rest)) = reason.split_once("; nearest ") else {
        return vec![reason.trim().to_owned()];
    };
    let mut out = vec![first.trim().to_owned()];
    for segment in rest.split("; nearest ") {
        out.push(format!("nearest {}", segment.trim()));
    }
    out
}

/// Split the `choice[i]: reason, choice[j]: reason` suffix while
/// preserving commas inside nested text.
fn split_choice_reasons(choices: &str) -> Vec<String> {
    choices
        .replace(", choice[", "\nchoice[")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Inputs needed to build related subdiagnostics for a failed
/// `.within` check.
struct RelatedInputs<'a> {
    /// Original LHS syntax node.
    lhs_node: &'a WrappedNode,
    /// Original RHS syntax node.
    rhs_node: &'a WrappedNode,
    /// Source origin of the `.within` operator.
    origin: &'a SourceOrigin,
    /// Concrete rendered LHS schema.
    lhs_concrete: String,
    /// Concrete rendered RHS schema.
    rhs_concrete: String,
}

/// Build related subdiagnostics for a failed `.within` check.
fn build_within_related(
    inputs: &RelatedInputs<'_>,
    diff: &[schema_diff::SchemaDiffLine],
    conflicts: &[WithinConflict],
) -> Vec<Subdiag> {
    let mut related = effective_schema_subdiags(inputs);
    if diff.is_empty() {
        related.extend(conflict_subdiags(conflicts));
    } else {
        related.extend(diff_subdiags(diff));
    }
    related
}

/// Concrete effective LHS/RHS schemas for diagnostic display.
fn effective_schema_subdiags(inputs: &RelatedInputs<'_>) -> Vec<Subdiag> {
    vec![
        Subdiag {
            kind: SubdiagKind::Lhs,
            snippet: effective_render(inputs.lhs_node, &inputs.lhs_concrete),
            origin: Some(inputs.origin.clone()),
        },
        Subdiag {
            kind: SubdiagKind::Rhs,
            snippet: effective_render(inputs.rhs_node, &inputs.rhs_concrete),
            origin: Some(inputs.origin.clone()),
        },
    ]
}

/// Use concrete rendering when available, falling back to source text
/// only if the concrete renderer produced no text.
fn effective_render(
    node: &WrappedNode,
    concrete: &str,
) -> String {
    if concrete.is_empty() {
        text_of_for_debug(node).trim().to_owned()
    } else {
        concrete.to_owned()
    }
}

/// Legacy conflict-only related subdiagnostics.
fn conflict_subdiags(conflicts: &[WithinConflict]) -> Vec<Subdiag> {
    conflicts
        .iter()
        .map(|c| {
            let conflict_kind = conflict_to_subdiag_kind(c.kind);
            let path_desc = conflict_path_summary(&c.path);
            let snippet = if path_desc.is_empty() {
                c.reason.clone()
            } else {
                format!("{}: {}", path_desc, c.reason)
            };
            Subdiag {
                kind: conflict_kind,
                snippet,
                origin: None,
            }
        })
        .collect()
}

/// Inline diff related subdiagnostics.
fn diff_subdiags(diff: &[schema_diff::SchemaDiffLine]) -> Vec<Subdiag> {
    diff.iter()
        .map(|line| {
            let kind = schema_diff_kind_to_subdiag(line.kind);
            let reason = (line.kind != schema_diff::SchemaDiffKind::Matched)
                .then_some(line.reason.as_deref())
                .flatten();
            let snippet = if line.text.is_empty() {
                reason.map(format_diff_reason).unwrap_or_default()
            } else if let Some(reason) = reason {
                format!("{}\n{}", line.text, format_diff_reason(reason))
            } else {
                line.text.clone()
            };
            Subdiag {
                kind,
                snippet,
                origin: None,
            }
        })
        .collect()
}

/// Format a conflict reason as CDDL-comment-style lines for the DIFF block.
fn format_diff_reason(reason: &str) -> String {
    let lines = format_within_reason_lines(reason);
    if let Some(line) = lines.first()
        && lines.len() == 1
    {
        return format!("; reason: {line}");
    }

    let mut out = String::from("; reason:");
    for line in lines {
        out.push('\n');
        out.push_str(";   ");
        out.push_str(&line);
    }
    out
}

/// Map a structured [`WithinConflictKind`] to the closest
/// [`SubdiagKind`] for CLI rendering.
fn conflict_to_subdiag_kind(kind: WithinConflictKind) -> SubdiagKind {
    match kind {
        WithinConflictKind::LhsNotAccepted
        | WithinConflictKind::MissingRequiredRhs
        | WithinConflictKind::TooManyMatches => SubdiagKind::Unmatched,
        WithinConflictKind::PrimitiveMismatch
        | WithinConflictKind::RangeMismatch
        | WithinConflictKind::ControlMismatch
        | WithinConflictKind::DifferentStructure
        | WithinConflictKind::UnresolvedName => SubdiagKind::Note,
    }
}

/// Map a [`SchemaDiffKind`] to the corresponding [`SubdiagKind`]
/// for the CLI diagnostic renderer.
///
/// The mapping is fixed by the Step 6 plan:
///
/// * [`SchemaDiffKind::Matched`] → [`SubdiagKind::Matched`]
/// * [`SchemaDiffKind::LhsRejected`] / [`SchemaDiffKind::RhsRequiredMissing`] →
///   [`SubdiagKind::Unmatched`]
/// * [`SchemaDiffKind::RhsOptional`] → [`SubdiagKind::Optional`]
/// * [`SchemaDiffKind::Context`] / [`SchemaDiffKind::Note`] → [`SubdiagKind::Note`]
fn schema_diff_kind_to_subdiag(kind: SchemaDiffKind) -> SubdiagKind {
    match kind {
        SchemaDiffKind::Matched => SubdiagKind::Matched,
        SchemaDiffKind::LhsRejected | SchemaDiffKind::RhsRequiredMissing => SubdiagKind::Unmatched,
        SchemaDiffKind::RhsOptional => SubdiagKind::Optional,
        SchemaDiffKind::Context | SchemaDiffKind::Note => SubdiagKind::Note,
    }
}

/// Build a short human-readable summary of a conflict path.
fn conflict_path_summary(path: &[PathSegment]) -> String {
    let parts: Vec<String> = path
        .iter()
        .map(|seg| {
            match seg {
                PathSegment::ArrayIndex(i) => format!("array[{i}]"),
                PathSegment::MapEntry(i) => format!("map[{i}]"),
                PathSegment::ChoiceArm(i) => format!("choice[{i}]"),
                PathSegment::ControlOp(op) => op.as_text().to_owned(),
                PathSegment::GroupEntry(_i) => String::new(),
            }
        })
        .filter(|p| !p.is_empty())
        .collect();
    parts.join(" → ")
}

/// Fully resolve all named and socket references for display.
///
/// Recursively replaces `Named` and `Socket` nodes with their concrete
/// definitions so the rendered type shows actual CDDL structures rather
/// than opaque names.
fn resolve_named_for_display(
    ty: &ResolvedType,
    defs: &DefinitionMap,
) -> String {
    use std::collections::HashSet as VisitedSet;
    let mut visited = VisitedSet::new();
    let resolved = resolve_named_deep(ty, defs, &mut visited);
    render_type(&resolved)
}

/// Recursively resolve all `Named` and `Socket` references.
fn resolve_named_deep(
    ty: &ResolvedType,
    defs: &DefinitionMap,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    match ty {
        ResolvedType::Named(name) | ResolvedType::Socket { name } => {
            if visited.contains(name) {
                return ty.clone();
            }
            visited.insert(name.clone());
            // Try //= socket choices first
            if let Some(choices) = defs.socket_choices_for(name)
                && let Some(choice) = choices.first()
            {
                let inner = unwrap_single_array(choice);
                return resolve_named_deep(inner, defs, visited);
            }
            // Try exact = definition
            if let Some(def_node) = defs.get(name) {
                let resolved = resolve_type(def_node);
                return resolve_named_deep(&resolved, defs, visited);
            }
            // Try prefix-tolerant lookup: imported names may be stored with
            // a prefix (e.g. "cose.Generic_Headers") but referenced bare.
            if let Some(def_node) = defs_find_suffix(defs, name) {
                let resolved = resolve_type(def_node);
                return resolve_named_deep(&resolved, defs, visited);
            }
            ty.clone()
        },
        ResolvedType::Map { entries } => {
            let resolved_entries: Vec<MapEntry> = entries
                .iter()
                .map(|e| {
                    MapEntry {
                        key: resolve_named_deep(&e.key, defs, visited),
                        value: resolve_named_deep(&e.value, defs, visited),
                        occurrence: e.occurrence,
                    }
                })
                .collect();
            ResolvedType::Map {
                entries: resolved_entries,
            }
        },
        ResolvedType::Array { elements } => {
            let resolved_elements: Vec<ArrayElement> = elements
                .iter()
                .map(|e| {
                    ArrayElement {
                        ty: resolve_named_deep(&e.ty, defs, visited),
                        occurrence: e.occurrence,
                    }
                })
                .collect();
            ResolvedType::Array {
                elements: resolved_elements,
            }
        },
        ResolvedType::Choice(alts) => {
            let resolved_alts: Vec<ResolvedType> = alts
                .iter()
                .map(|a| resolve_named_deep(a, defs, visited))
                .collect();
            ResolvedType::Choice(resolved_alts)
        },
        ResolvedType::Tag { tag, inner } => {
            ResolvedType::Tag {
                tag: *tag,
                inner: Box::new(resolve_named_deep(inner, defs, visited)),
            }
        },
        ResolvedType::Control {
            op,
            carrier,
            controller,
        } => {
            ResolvedType::Control {
                op: op.clone(),
                carrier: Box::new(resolve_named_deep(carrier, defs, visited)),
                controller: Box::new(resolve_named_deep(controller, defs, visited)),
            }
        },
        ResolvedType::Intersection(operands) => {
            let resolved: Vec<ResolvedType> = operands
                .iter()
                .map(|op| resolve_named_deep(op, defs, visited))
                .collect();
            ResolvedType::Intersection(resolved)
        },
        _ => ty.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use cbork_cddl_parser::parse_cddl;

    use super::*;
    use crate::preprocessor::{inject_directives, process_ast};

    /// Parse a CDDL snippet and return the enriched AST nodes.
    fn parse_snippet(source: &str) -> Vec<WrappedNode> {
        let pairs = parse_cddl(source).expect("parse should succeed");
        let pairs = process_ast(pairs).expect("preprocess should succeed");
        inject_directives(&std::path::PathBuf::from("<test>"), &pairs, source)
            .expect("directive injection should succeed")
    }

    /// Find the first `RuleLine` in parsed nodes and resolve its RHS type.
    fn resolve_first_rule_rhs(nodes: &[WrappedNode]) -> ResolvedType {
        for node in nodes {
            if let WrappedNode::RuleLine { .. } = node {
                return resolve_type(node);
            }
        }
        panic!("no RuleLine found in parsed nodes");
    }

    #[test]
    fn resolve_primitive_int() {
        let nodes = parse_snippet("x = int\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Primitive(PrimitiveKind::Int));
    }

    #[test]
    fn resolve_primitive_uint() {
        let nodes = parse_snippet("x = uint\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Primitive(PrimitiveKind::Uint));
    }

    #[test]
    fn resolve_primitive_tstr() {
        let nodes = parse_snippet("x = tstr\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Primitive(PrimitiveKind::Tstr));
    }

    #[test]
    fn resolve_any() {
        let nodes = parse_snippet("x = any\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Any);
    }

    #[test]
    fn resolve_range() {
        let nodes = parse_snippet("x = 0..255\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Range {
            lo: Some(0),
            hi: Some(255),
            is_float: false
        });
    }

    #[test]
    fn resolve_tagged() {
        let nodes = parse_snippet("x = #6.123(tstr)\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Tag {
            tag: 123,
            inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
        });
    }

    #[test]
    fn resolve_choice() {
        let nodes = parse_snippet("x = int / tstr\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(
            ty,
            ResolvedType::Choice(vec![
                ResolvedType::Primitive(PrimitiveKind::Int),
                ResolvedType::Primitive(PrimitiveKind::Tstr),
            ])
        );
    }

    #[test]
    fn resolve_array() {
        let nodes = parse_snippet("x = [a: int, b: tstr]\n");
        let ty = resolve_first_rule_rhs(&nodes);

        if let ResolvedType::Array { elements } = &ty {
            assert_eq!(elements.len(), 2);
        } else {
            panic!("expected Array, got {ty:?}");
        }
    }

    #[test]
    fn resolve_map() {
        let nodes = parse_snippet("x = { a => int, ? b => tstr }\n");
        let ty = resolve_first_rule_rhs(&nodes);

        if let ResolvedType::Map { entries } = &ty {
            assert_eq!(entries.len(), 2);

            // First entry: required int
            assert_eq!(entries[0].occurrence, Occurrence::One);
            assert_eq!(
                entries[0].value,
                ResolvedType::Primitive(PrimitiveKind::Int)
            );

            // Second entry: optional tstr
            assert_eq!(entries[1].occurrence, Occurrence::Optional);
            assert_eq!(
                entries[1].value,
                ResolvedType::Primitive(PrimitiveKind::Tstr)
            );
        } else {
            panic!("expected Map, got {ty:?}");
        }
    }

    #[test]
    fn resolve_named_ref() {
        let nodes = parse_snippet("x = my_type\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Named("my_type".to_owned()));
    }

    #[test]
    fn resolve_socket_ref() {
        let nodes = parse_snippet("x = $message\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Socket {
            name: "$message".to_owned()
        });
    }

    #[test]
    fn resolve_nested_choice() {
        let nodes = parse_snippet("x = int / tstr / bool\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(
            ty,
            ResolvedType::Choice(vec![
                ResolvedType::Primitive(PrimitiveKind::Int),
                ResolvedType::Primitive(PrimitiveKind::Tstr),
                ResolvedType::Primitive(PrimitiveKind::Bool),
            ])
        );
    }

    #[test]
    fn resolve_float_range() {
        let nodes = parse_snippet("x = 1.5..3.0\n");
        let ty = resolve_first_rule_rhs(&nodes);
        assert_eq!(ty, ResolvedType::Range {
            lo: Some(1),
            hi: Some(3),
            is_float: true
        });
    }

    #[test]
    fn resolve_occurrence_zero_or_more() {
        let nodes = parse_snippet("x = [*int]\n");
        let ty = resolve_first_rule_rhs(&nodes);
        if let ResolvedType::Array { elements } = &ty {
            assert_eq!(elements.len(), 1);
            assert_eq!(elements[0].occurrence, Occurrence::ZeroOrMore);
            assert_eq!(elements[0].ty, ResolvedType::Primitive(PrimitiveKind::Int));
        } else {
            panic!("expected Array, got {ty:?}");
        }
    }

    #[test]
    fn resolve_occurrence_one_or_more() {
        let nodes = parse_snippet("x = [+tstr]\n");
        let ty = resolve_first_rule_rhs(&nodes);
        if let ResolvedType::Array { elements } = &ty {
            assert_eq!(elements.len(), 1);
            assert_eq!(elements[0].occurrence, Occurrence::OneOrMore);
        } else {
            panic!("expected Array, got {ty:?}");
        }
    }

    #[test]
    fn resolve_occurrence_range() {
        let nodes = parse_snippet("x = [2*5 int]\n");
        let ty = resolve_first_rule_rhs(&nodes);
        if let ResolvedType::Array { elements } = &ty {
            assert_eq!(elements.len(), 1);
            assert_eq!(elements[0].occurrence, Occurrence::Range { lo: 2, hi: 5 });
        } else {
            panic!("expected Array, got {ty:?}");
        }
    }

    #[test]
    fn occurrence_min_max() {
        assert_eq!(Occurrence::One.min(), 1);
        assert_eq!(Occurrence::One.max(), Some(1));
        assert_eq!(Occurrence::Optional.min(), 0);
        assert_eq!(Occurrence::Optional.max(), Some(1));
        assert_eq!(Occurrence::ZeroOrMore.min(), 0);
        assert_eq!(Occurrence::ZeroOrMore.max(), None);
        assert_eq!(Occurrence::OneOrMore.min(), 1);
        assert_eq!(Occurrence::OneOrMore.max(), None);
        assert_eq!(Occurrence::Range { lo: 2, hi: 5 }.min(), 2);
        assert_eq!(Occurrence::Range { lo: 2, hi: 5 }.max(), Some(5));
    }

    #[test]
    fn resolve_map_with_occurrence() {
        let nodes = parse_snippet("x = { * int => tstr }\n");
        let ty = resolve_first_rule_rhs(&nodes);
        if let ResolvedType::Map { entries } = &ty {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].occurrence, Occurrence::ZeroOrMore);
            assert_eq!(entries[0].key, ResolvedType::Primitive(PrimitiveKind::Int));
            assert_eq!(
                entries[0].value,
                ResolvedType::Primitive(PrimitiveKind::Tstr)
            );
        } else {
            panic!("expected Map, got {ty:?}");
        }
    }

    // ------------------------------------------------------------------
    // Stage 2: Definition resolution tests
    // ------------------------------------------------------------------

    /// Build a simple definition map from parsed nodes.
    fn build_def_map(source: &str) -> DefinitionMap {
        let nodes = parse_snippet(source);
        DefinitionMap::from_nodes(&nodes)
    }

    #[test]
    fn defmap_contains_top_level_rule() {
        let defs = build_def_map("x = int\n");
        assert!(defs.contains("x"));
        let node = defs.get("x").expect("x should be in map");
        assert!(matches!(node, WrappedNode::RuleLine { .. }));
    }

    #[test]
    fn defmap_contains_multiple_rules() {
        let defs = build_def_map("x = int\ny = tstr\n");
        assert!(defs.contains("x"));
        assert!(defs.contains("y"));
    }

    #[test]
    fn defmap_missing_rule() {
        let defs = build_def_map("x = int\n");
        assert!(!defs.contains("nonexistent"));
    }

    #[test]
    fn resolve_definition_resolves_simple_type() {
        let nodes = parse_snippet("my_type = int\n");
        let defs = DefinitionMap::from_nodes(&nodes);
        let ty = resolve_definition("my_type", &defs).expect("my_type should resolve");
        assert_eq!(ty, ResolvedType::Primitive(PrimitiveKind::Int));
    }

    #[test]
    fn resolve_definition_resolves_array() {
        let nodes = parse_snippet("my_type = [int, tstr]\n");
        let defs = DefinitionMap::from_nodes(&nodes);
        let ty = resolve_definition("my_type", &defs).expect("my_type should resolve");
        if let ResolvedType::Array { elements } = &ty {
            assert_eq!(elements.len(), 2);
        } else {
            panic!("expected Array, got {ty:?}");
        }
    }

    #[test]
    fn collect_socket_choices_finds_augmentations() {
        let source = "root = $msg\n$msg /= [int]\n$msg /= [tstr]\n";
        let nodes = parse_snippet(source);
        let choices = collect_socket_choices("$msg", &nodes);
        assert_eq!(choices.len(), 2);
    }

    #[test]
    fn collect_socket_choices_empty_for_no_augmentations() {
        let nodes = parse_snippet("root = int\n");
        let choices = collect_socket_choices("$msg", &nodes);
        assert!(choices.is_empty());
    }

    #[test]
    fn is_socket_name_detects_socket() {
        assert!(is_socket_name("$message"));
        assert!(is_socket_name("$msg"));
        assert!(!is_socket_name("message"));
        assert!(!is_socket_name("int"));
    }

    // ------------------------------------------------------------------
    // Stage 3: Subtype checker tests
    // ------------------------------------------------------------------

    #[test]
    fn subtype_primitive_equal() {
        let defs = DefinitionMap::from_nodes(&[]);
        let t = ResolvedType::Primitive(PrimitiveKind::Int);
        assert!(is_subtype(&t, &t, &defs).is_ok());
    }

    #[test]
    fn subtype_uint_to_int() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Primitive(PrimitiveKind::Uint),
                &ResolvedType::Primitive(PrimitiveKind::Int),
                &defs
            )
            .is_ok()
        );
    }

    #[test]
    fn subtype_int_to_uint_fails() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Primitive(PrimitiveKind::Int),
                &ResolvedType::Primitive(PrimitiveKind::Uint),
                &defs
            )
            .is_err()
        );
    }

    #[test]
    fn subtype_range_within() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Range {
                    lo: Some(0),
                    hi: Some(100),
                    is_float: false,
                },
                &ResolvedType::Range {
                    lo: Some(0),
                    hi: Some(255),
                    is_float: false,
                },
                &defs
            )
            .is_ok()
        );
    }

    #[test]
    fn subtype_range_not_within() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Range {
                    lo: Some(0),
                    hi: Some(255),
                    is_float: false,
                },
                &ResolvedType::Range {
                    lo: Some(0),
                    hi: Some(100),
                    is_float: false,
                },
                &defs
            )
            .is_err()
        );
    }

    #[test]
    fn subtype_tag_match() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Tag {
                    tag: 123,
                    inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
                },
                &ResolvedType::Tag {
                    tag: 123,
                    inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
                },
                &defs
            )
            .is_ok()
        );
    }

    #[test]
    fn subtype_tag_mismatch() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Tag {
                    tag: 123,
                    inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
                },
                &ResolvedType::Tag {
                    tag: 456,
                    inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
                },
                &defs
            )
            .is_err()
        );
    }

    #[test]
    fn subtype_array_equal() {
        let defs = DefinitionMap::from_nodes(&[]);
        let arr = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Int),
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
            ],
        };
        assert!(is_subtype(&arr, &arr, &defs).is_ok());
    }

    #[test]
    fn subtype_array_length_mismatch() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Array {
            elements: vec![ArrayElement {
                ty: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        };
        let rhs = ResolvedType::Array { elements: vec![] };
        assert!(is_subtype(&lhs, &rhs, &defs).is_err());
    }

    #[test]
    fn subtype_choice_on_lhs() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Int),
            ResolvedType::Primitive(PrimitiveKind::Int),
        ]);
        let rhs = ResolvedType::Primitive(PrimitiveKind::Int);
        assert!(is_subtype(&lhs, &rhs, &defs).is_ok());
    }

    #[test]
    fn subtype_choice_on_rhs() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Primitive(PrimitiveKind::Int);
        let rhs = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Int),
            ResolvedType::Primitive(PrimitiveKind::Tstr),
        ]);
        assert!(is_subtype(&lhs, &rhs, &defs).is_ok());
    }

    #[test]
    fn subtype_any_accepts_everything() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Primitive(PrimitiveKind::Int),
                &ResolvedType::Any,
                &defs
            )
            .is_ok()
        );
    }

    #[test]
    fn subtype_any_not_subtype_of_anything() {
        let defs = DefinitionMap::from_nodes(&[]);
        assert!(
            is_subtype(
                &ResolvedType::Any,
                &ResolvedType::Primitive(PrimitiveKind::Int),
                &defs
            )
            .is_err()
        );
    }

    #[test]
    fn subtype_via_resolved_definitions() {
        let nodes = parse_snippet("my_int = int\nmy_uint = uint\n");
        let defs = DefinitionMap::from_nodes(&nodes);
        let lhs = ResolvedType::Named("my_uint".to_owned());
        let rhs = ResolvedType::Named("my_int".to_owned());
        assert!(is_subtype(&lhs, &rhs, &defs).is_ok());
    }

    #[test]
    fn occurrence_compatible_tests() {
        assert!(occurrence_compatible(Occurrence::One, Occurrence::One));
        assert!(occurrence_compatible(Occurrence::One, Occurrence::Optional));
        assert!(!occurrence_compatible(
            Occurrence::Optional,
            Occurrence::One
        ));
        assert!(occurrence_compatible(
            Occurrence::Optional,
            Occurrence::ZeroOrMore
        ));
        assert!(occurrence_compatible(
            Occurrence::Range { lo: 2, hi: 5 },
            Occurrence::Range { lo: 1, hi: 6 }
        ));
        assert!(!occurrence_compatible(
            Occurrence::Range { lo: 1, hi: 3 },
            Occurrence::Range { lo: 2, hi: 5 }
        ));
    }

    // ------------------------------------------------------------------
    // Regression coverage for the subtype / control / map pipeline
    // ------------------------------------------------------------------

    /// A specific integer value (resolved as `Range(lo..hi)`) is a
    /// subtype of `Primitive(Int)`.  The checker now has a
    /// `Range ⊆ Primitive` rule that accepts a singleton range as
    /// a member of the wider primitive type.
    #[test]
    fn range_value_is_subtype_of_int() {
        // `my_val = -19` resolves to Range { lo: Some(-19), hi: Some(-19) }
        let nodes = parse_snippet("my_val = -19\n");
        let defs = DefinitionMap::from_nodes(&nodes);
        let lhs = ResolvedType::Named("my_val".to_owned());
        let rhs = ResolvedType::Primitive(PrimitiveKind::Int);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "Range(-19..-19) should be a subtype of int"
        );
    }

    /// A map containing a socket plug (`one-pq-signature`) preserves
    /// the plug's `//=` choices through the resolver.  The map
    /// entry extractor handles group socket references, so the
    /// resulting carrier has a Socket-keyed entry.  The value
    /// types `ed25519_sig` and `ml-dsa-seed` are intentionally
    /// `bstr .size N`; the structural assertions verify the
    /// preserved Control shape end-to-end.
    #[test]
    fn map_with_socket_plug_preserves_choices() {
        // Step 1 preserves `bstr .size N` ctlops as
        // `ResolvedType::Control { op: Size, ... }`, and Step 2
        // wires `Control` into `is_subtype_impl` so the
        // preserved shape is sufficient for downstream checks.
        let source = concat!(
            "one-pq-signature //= (ml-dsa-44 => ml-dsa-seed)\n",
            "alg-map = {\n",
            "  ed25519 => ed25519_sig,\n",
            "  one-pq-signature\n",
            "} .within alg-generic\n",
            "alg-generic = { 2*2 int => bstr }\n",
            "ed25519 = -19\n",
            "ed25519_sig = bstr .size 64\n",
            "ml-dsa-44 = -48\n",
            "ml-dsa-seed = bstr .size 32\n",
        );
        let nodes = parse_snippet(source);
        let defs = DefinitionMap::from_nodes(&nodes);

        // Verify socket choices are populated
        let sock_choices = defs.socket_choices_for("one-pq-signature");
        assert!(
            sock_choices.is_some(),
            "socket choices should be populated, got {sock_choices:?}"
        );
        assert!(
            !sock_choices.unwrap().is_empty(),
            "socket choices should not be empty"
        );

        // The .within ctlop is preserved as `Control`; reach into the
        // carrier to inspect the underlying map.
        let alg_map_ty = resolve_definition("alg-map", &defs).expect("alg-map should resolve");
        let alg_map_carrier = if let ResolvedType::Control { carrier, op, .. } = &alg_map_ty {
            assert!(
                matches!(op, ControlOp::Within),
                "expected .within, got {op:?}"
            );
            carrier.as_ref()
        } else {
            panic!("alg-map should be a Control .within, got {alg_map_ty:?}");
        };
        if let ResolvedType::Map { entries } = alg_map_carrier {
            let has_socket = entries
                .iter()
                .any(|e| matches!(&e.key, ResolvedType::Socket { .. }));
            assert!(
                has_socket,
                "alg-map carrier should have a Socket-keyed entry, got: {entries:#?}"
            );
        } else {
            panic!("alg-map carrier should be a Map, got {alg_map_carrier:?}");
        }

        // `ed25519_sig = bstr .size 64` must resolve as a Control node.
        // This is the fixture's nested ctlop preservation guarantee and
        // must not be weakened to bare `bstr` to make other assertions
        // pass.
        let ed25519_sig_ty =
            resolve_definition("ed25519_sig", &defs).expect("ed25519_sig should resolve");
        match &ed25519_sig_ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Size);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Range {
                    lo: Some(64),
                    hi: Some(64),
                    is_float: false,
                });
            },
            other => panic!("ed25519_sig should be a Control .size 64, got {other:?}"),
        }
    }

    #[test]
    fn map_socket_choice_accepts_later_plug_arm_within() {
        let source = concat!(
            "root = lhs .within rhs\n",
            "lhs = { ed25519 => ed25519_sig, ml-dsa-65 => ml-dsa-65_sig }\n",
            "rhs = { ed25519 => ed25519_sig, one-pq-signature }\n",
            "one-pq-signature //= (ml-dsa-44 => ml-dsa-44_sig)\n",
            "one-pq-signature //= (ml-dsa-65 => ml-dsa-65_sig)\n",
            "one-pq-signature //= (ml-dsa-87 => ml-dsa-87_sig)\n",
            "ed25519 = -19\n",
            "ml-dsa-44 = -48\n",
            "ml-dsa-65 = -49\n",
            "ml-dsa-87 = -50\n",
            "ed25519_sig = bstr .size 64\n",
            "ml-dsa-44_sig = bstr .size 2420\n",
            "ml-dsa-65_sig = bstr .size 3309\n",
            "ml-dsa-87_sig = bstr .size 4627\n",
        );
        let nodes = parse_snippet(source);
        let defs = DefinitionMap::from_nodes(&nodes);
        let lhs = resolve_definition("lhs", &defs).expect("lhs should resolve");
        let rhs = resolve_definition("rhs", &defs).expect("rhs should resolve");

        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "concrete -49 map entry should match the second socket plug arm"
        );
    }

    // ------------------------------------------------------------------
    // render_type tests
    // ------------------------------------------------------------------

    #[test]
    fn render_primitive() {
        assert_eq!(
            render_type(&ResolvedType::Primitive(PrimitiveKind::Int)),
            "int"
        );
    }

    #[test]
    fn render_range() {
        assert_eq!(
            render_type(&ResolvedType::Range {
                lo: Some(0),
                hi: Some(255),
                is_float: false,
            }),
            "0..255"
        );
    }

    #[test]
    fn render_array() {
        let arr = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Int),
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::Optional,
                },
            ],
        };
        assert_eq!(render_type(&arr), "[int, ? tstr]");
    }

    #[test]
    fn render_map() {
        let map = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Primitive(PrimitiveKind::Int),
                value: ResolvedType::Primitive(PrimitiveKind::Bstr),
                occurrence: Occurrence::Range { lo: 2, hi: 2 },
            }],
        };
        assert_eq!(render_type(&map), "{ 2*2 int => bstr }");
    }

    #[test]
    fn render_choice() {
        let choice = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Int),
            ResolvedType::Primitive(PrimitiveKind::Tstr),
        ]);
        assert_eq!(render_type(&choice), "int / tstr");
    }

    #[test]
    fn render_tag() {
        let tag = ResolvedType::Tag {
            tag: 6,
            inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
        };
        assert_eq!(render_type(&tag), "#6.6(tstr)");
    }

    // ------------------------------------------------------------------
    // Bare group reference inside a map RHS
    // ------------------------------------------------------------------

    /// A bare group reference (`=`, not `//=`) inside a map RHS is
    /// expanded into the map by `expand_map_sockets` (Step 5.10).
    /// Here `Generic_Headers` is a `? alg => int, ? kid => bstr`
    /// group used as the first entry of `header_map`, so the
    /// resulting `header_map` requires both `alg` and `kid` keys.
    /// The LHS `my_headers` provides `alg => 1` (matches `int`)
    /// but `kid => 'example'` (a text literal) does not match the
    /// required `bstr` value type.  The checker should therefore
    /// report a `LhsNotAccepted` conflict on the `kid` entry.
    #[test]
    fn bare_group_reference_in_map_uses_expanded_type() {
        let source = concat!(
            "Generic_Headers = (\n",
            "  ? alg => int,\n",
            "  ? kid => bstr\n",
            ")\n",
            "header_map = {\n",
            "  Generic_Headers,\n",
            "  * label => values\n",
            "}\n",
            "my_headers = {\n",
            "  alg => 1,\n",
            "  kid => 'example'\n",
            "} .within header_map\n",
            "alg = 1\n",
            "kid = 2\n",
            "label = 5\n",
            "values = any\n",
        );
        let nodes = parse_snippet(source);
        let defs = DefinitionMap::from_nodes(&nodes);
        let my_headers_ty =
            resolve_definition("my_headers", &defs).expect("my_headers should resolve");
        let lhs = if let ResolvedType::Control { carrier, op, .. } = &my_headers_ty {
            assert!(
                matches!(op, ControlOp::Within),
                "expected .within, got {op:?}"
            );
            carrier.as_ref().clone()
        } else {
            panic!("my_headers should be a Control .within, got {my_headers_ty:?}");
        };
        let rhs = ResolvedType::Named("header_map".to_owned());
        // The `Generic_Headers` group reference has been expanded
        // into `header_map`; the resulting required entries see the
        // LHS `kid => 'example'` (text) failing to match the
        // required `bstr` value type.
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts
                .iter()
                .any(|c| matches!(c.kind, WithinConflictKind::LhsNotAccepted)),
            "expected LhsNotAccepted conflict, got {conflicts:#?}"
        );
    }

    #[test]
    fn within_diagnostic_contains_inline_diff_subdiags() {
        // Step 6: the diff builder now wires into
        // `check_within_constraint`. The resulting E030 diagnostic
        // must carry inline diff subdiags with at least one
        // Unmatched for the failing line, and all snippets
        // concrete and non-empty.
        //
        // Map with two keys, one of which (the float) is not present
        // in the RHS. The .within check fires because the LHS map
        // contains a key the RHS does not have.
        let source = concat!(
            "a = float\n",
            "rule = { a => int, 1 => int } .within { 1 => int, 2 => int }\n",
        );
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        assert!(
            !warnings.is_empty(),
            "expected at least one E030 warning: {warnings:?}"
        );
        let diag = warnings
            .iter()
            .find(|w| w.code == "E030")
            .expect("expected an E030 warning");

        // Step 6: the diff-based subdiags carry line-level
        // classifications (Unmatched / Matched / Optional / Note).
        let kinds: Vec<_> = diag.related.iter().map(|s| s.kind).collect();
        assert!(
            kinds.contains(&SubdiagKind::Unmatched),
            "expected at least one Unmatched subdiag (the failing line), got kinds: {kinds:?}"
        );

        // Snippets must be concrete (the diff builder renders actual
        // CDDL text, not abstract path summaries).
        for subdiag in &diag.related {
            assert!(
                !subdiag.snippet.is_empty(),
                "subdiag snippet should not be empty: {subdiag:?}"
            );
        }

        // At least one Unmatched subdiag must carry a concrete
        // rendered CDDL snippet (not a bare path summary).
        let unmatched = diag
            .related
            .iter()
            .filter(|s| s.kind == SubdiagKind::Unmatched)
            .collect::<Vec<_>>();
        assert!(
            !unmatched.is_empty(),
            "expected at least one Unmatched subdiag, got {kinds:?}"
        );
        for s in &unmatched {
            assert!(
                s.snippet.contains("=>")
                    || s.snippet.contains("int")
                    || s.snippet.contains("float"),
                "Unmatched snippet should contain concrete CDDL, got: {:?}",
                s.snippet
            );
        }
    }

    #[test]
    fn within_diagnostic_matched_when_identical() {
        // When LHS and RHS are identical maps, every rendered line
        // matches, so all diff subdiags are Matched.
        let source = "rule = { 1 => int, 2 => tstr } .within { 1 => int, 2 => tstr }\n";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        // No conflicts — no E030 emitted.
        assert!(
            warnings.is_empty(),
            "identical maps should not produce E030, got {warnings:#?}"
        );
    }

    // ------------------------------------------------------------------
    // Step 1: ctlop schema preservation
    // ------------------------------------------------------------------

    /// Find a `RuleLine` by name and return its resolved RHS type.
    fn resolve_rule_rhs(
        nodes: &[WrappedNode],
        name: &str,
    ) -> ResolvedType {
        for node in nodes {
            if let WrappedNode::RuleLine { .. } = node
                && extract_rule_name(node).as_deref() == Some(name)
            {
                return resolve_type(node);
            }
        }
        panic!("RuleLine {name} not found");
    }

    #[test]
    fn resolve_ctlop_schema() {
        // `bstr .cbor payload` should resolve to `Control { Cbor, bstr, payload }`.
        let source = concat!("payload = { 1 => int }\n", "x = bstr .cbor payload\n",);
        let nodes = parse_snippet(source);
        let ty = resolve_rule_rhs(&nodes, "x");
        match &ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Cbor);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Named("payload".to_owned()));
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_dtrm_schema() {
        // `bstr .dtrm payload` should resolve to `Control { Dtrm, bstr, payload }`.
        let source = concat!("payload = { 1 => int }\n", "x = bstr .dtrm payload\n",);
        let nodes = parse_snippet(source);
        let ty = resolve_rule_rhs(&nodes, "x");
        match &ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Dtrm);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Named("payload".to_owned()));
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_controller_resolves_named_schema() {
        // The controller of a Control node should be the *resolved* payload
        // schema, not an opaque string. After deep resolution, `payload` is
        // a Map with the int key.
        let source = concat!("payload = { 1 => int }\n", "x = bstr .cbor payload\n",);
        let nodes = parse_snippet(source);
        let defs = DefinitionMap::from_nodes(&nodes);
        let ty = resolve_rule_rhs(&nodes, "x");
        let mut visited = HashSet::new();
        let resolved = resolve_named_deep(&ty, &defs, &mut visited);
        let ResolvedType::Control {
            op,
            carrier,
            controller,
        } = resolved
        else {
            panic!("expected Control, got {ty:?}");
        };
        assert_eq!(op, ControlOp::Cbor);
        assert_eq!(*carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
        match controller.as_ref() {
            ResolvedType::Map { entries } => {
                assert_eq!(entries.len(), 1);
            },
            other => panic!("expected controller to resolve to a Map, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_cborseq() {
        // `bstr .cborseq payload` should produce `Control { CborSeq, ... }`.
        let source = concat!("payload = [* int]\n", "x = bstr .cborseq payload\n",);
        let nodes = parse_snippet(source);
        let ty = resolve_rule_rhs(&nodes, "x");
        match &ty {
            ResolvedType::Control { op, .. } => {
                assert_eq!(*op, ControlOp::CborSeq);
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_dtrmseq() {
        // `bstr .dtrmseq payload` should produce `Control { DtrmSeq, ... }`.
        let source = concat!("payload = [* int]\n", "x = bstr .dtrmseq payload\n",);
        let nodes = parse_snippet(source);
        let ty = resolve_rule_rhs(&nodes, "x");
        match &ty {
            ResolvedType::Control { op, .. } => {
                assert_eq!(*op, ControlOp::DtrmSeq);
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_size_preserved() {
        // `.size` is schema-relevant; the carrier length and the
        // bound must be preserved.
        let nodes = parse_snippet("x = bstr .size 64\n");
        let ty = resolve_first_rule_rhs(&nodes);
        match &ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Size);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                match controller.as_ref() {
                    ResolvedType::Range { lo, hi, is_float } => {
                        assert_eq!(*lo, Some(64));
                        assert_eq!(*hi, Some(64));
                        assert!(!*is_float);
                    },
                    other => panic!("expected Range controller, got {other:?}"),
                }
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ctlop_unknown_preserved_as_other() {
        // An unknown ctlop text should round-trip into `ControlOp::Other`
        // rather than collapsing to the carrier.
        let nodes = parse_snippet("x = bstr .plus 1\n");
        let ty = resolve_first_rule_rhs(&nodes);
        match &ty {
            ResolvedType::Control { op, .. } => {
                assert_eq!(*op, ControlOp::Other(".plus".to_owned()));
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn render_type_control() {
        let ty = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
        };
        assert_eq!(render_type(&ty), "bstr .cbor tstr");
    }

    #[test]
    fn type_name_control() {
        let ty = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Tstr)),
        };
        assert_eq!(type_name(&ty), "control(.dtrm)");
    }

    // ------------------------------------------------------------------
    // Stage 1 follow-up coverage (added during Step 2)
    // ------------------------------------------------------------------
    //
    // These tests cover control operators that Step 1 explicitly listed as
    // "do not collapse" but were not exercised by the original Step 1
    // fixture set. They are pure-preservation assertions; subtype
    // containment for `Control` is the responsibility of Step 2.

    #[test]
    fn resolve_and_ctlop_preserved() {
        // `int .and uint` must resolve to `Intersection([int, uint])`
        // rather than collapsing to the carrier or using `Control`.
        // Step 4 prefers a dedicated variant for easier subtyping.
        let nodes = parse_snippet("x = int .and uint\n");
        let ty = resolve_first_rule_rhs(&nodes);
        match &ty {
            ResolvedType::Intersection(operands) => {
                assert_eq!(operands.len(), 2);
                assert_eq!(operands[0], ResolvedType::Primitive(PrimitiveKind::Int));
                assert_eq!(operands[1], ResolvedType::Primitive(PrimitiveKind::Uint));
            },
            other => panic!("expected Intersection, got {other:?}"),
        }
    }

    #[test]
    fn resolve_within_ctlop_preserved() {
        // `int .within uint` must resolve to `Control { Within, int, uint }`
        // rather than collapsing to the carrier.
        let nodes = parse_snippet("x = int .within uint\n");
        let ty = resolve_first_rule_rhs(&nodes);
        match &ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Within);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Int));
                assert_eq!(**controller, ResolvedType::Primitive(PrimitiveKind::Uint));
            },
            other => panic!("expected Control .within, got {other:?}"),
        }
    }

    #[test]
    fn resolve_bits_ctlop_preserved() {
        // `bstr .bits flags` must resolve to `Control { Bits, bstr, flags }`
        // rather than collapsing to the carrier. The operand of `.bits`
        // is a CDDL map or set of bit names; using a simple group entry
        // here keeps the parse valid while still exercising the
        // preservation guarantee for the operator.
        let source = "flags = { a, b, c }\nx = bstr .bits flags\n";
        let nodes = parse_snippet(source);
        let ty = resolve_rule_rhs(&nodes, "x");
        match &ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Bits);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Named("flags".to_owned()));
            },
            other => panic!("expected Control .bits, got {other:?}"),
        }
    }

    #[test]
    fn resolve_nested_size_ctlop_in_map_value_preserved() {
        // The nested value `ed25519_sig = bstr .size 64` must resolve as
        // `Control { Size, bstr, 64 }` even when reached through a map
        // shape similar to a real COSE-style signature map. This is the
        // coverage hardening required by the Step 1 follow-up section:
        // do not weaken the fixture by replacing `.size` with bare `bstr`.
        let source = concat!(
            "ed25519_sig = bstr .size 64\n",
            "ml-dsa-44 = -48\n",
            "ml-dsa-seed = bstr .size 32\n",
            "alg-map = {\n",
            "  ed25519 => ed25519_sig,\n",
            "  ml-dsa-44 => ml-dsa-seed,\n",
            "} .within alg-generic\n",
            "alg-generic = { * int => bstr }\n",
            "ed25519 = -19\n",
        );
        let nodes = parse_snippet(source);
        let defs = DefinitionMap::from_nodes(&nodes);

        // The nested value must reach a `Control` node, not collapse.
        let ed25519_sig_ty =
            resolve_definition("ed25519_sig", &defs).expect("ed25519_sig should resolve");
        match &ed25519_sig_ty {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Size);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Range {
                    lo: Some(64),
                    hi: Some(64),
                    is_float: false,
                });
            },
            other => panic!("ed25519_sig should be a Control .size 64, got {other:?}"),
        }

        // The nested value inside the LHS map must also be reachable as
        // `Control` after deep named resolution. This proves the nested
        // ctlop survives a realistic socket-free map shape.
        let alg_map_ty = resolve_definition("alg-map", &defs).expect("alg-map should resolve");
        let alg_map_carrier = if let ResolvedType::Control { carrier, op, .. } = &alg_map_ty {
            assert!(matches!(op, ControlOp::Within));
            carrier.as_ref()
        } else {
            panic!("alg-map should be a Control .within, got {alg_map_ty:?}");
        };
        let ResolvedType::Map { entries } = alg_map_carrier else {
            panic!("alg-map carrier should be a Map, got {alg_map_carrier:?}");
        };
        let ed25519_value = entries
            .iter()
            .find(|e| matches!(&e.key, ResolvedType::Named(n) if n == "ed25519"))
            .expect("alg-map carrier should contain ed25519 entry")
            .value
            .clone();
        let mut visited = HashSet::new();
        let resolved_value = resolve_named_deep(&ed25519_value, &defs, &mut visited);
        match &resolved_value {
            ResolvedType::Control {
                op,
                carrier,
                controller,
            } => {
                assert_eq!(*op, ControlOp::Size);
                assert_eq!(**carrier, ResolvedType::Primitive(PrimitiveKind::Bstr));
                assert_eq!(**controller, ResolvedType::Range {
                    lo: Some(64),
                    hi: Some(64),
                    is_float: false,
                });
            },
            other => panic!("resolved ed25519 value should be a Control .size 64, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Step 2: directional ctlop containment
    // ------------------------------------------------------------------
    //
    // These tests build `ResolvedType::Control` values directly and
    // exercise `is_subtype` / `is_subtype_impl` through the wildcard
    // arm that now dispatches to `is_control_subtype`.

    /// Build `payload = { 1 => int }` as a `ResolvedType::Map`.
    fn map_int_1() -> ResolvedType {
        ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        }
    }

    /// Build `payload-wide = { 1 => int, ? 2 => tstr }` as a `ResolvedType::Map`.
    fn map_int_1_optional_tstr_2() -> ResolvedType {
        ResolvedType::Map {
            entries: vec![
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(1),
                        hi: Some(1),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Int),
                    occurrence: Occurrence::One,
                },
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(2),
                        hi: Some(2),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::Optional,
                },
            ],
        }
    }

    #[test]
    fn dtrm_within_cbor() {
        // `bstr .dtrm payload ⊆ bstr .cbor payload` — both controllers
        // are the same map, and `.dtrm` is the narrower serialization
        // operator.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .dtrm payload must be within bstr .cbor payload"
        );
    }

    #[test]
    fn dtrm_payload_within_broader_cbor_payload() {
        // `bstr .dtrm narrow ⊆ bstr .cbor wide` where `wide` is
        // structurally broader than `narrow`. The carrier stays `bstr`
        // and the controller narrows, so subtype must hold.
        let defs = DefinitionMap::from_nodes(&[]);
        let narrow = map_int_1();
        let wide = map_int_1_optional_tstr_2();
        let lhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(narrow),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(wide),
        };
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .dtrm narrow must be within bstr .cbor wide"
        );
    }

    #[test]
    fn dtrmseq_within_cborseq() {
        // `bstr .dtrmseq payload ⊆ bstr .cborseq payload` — sequence
        // variant of the dtrm/cbor directionality rule.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::DtrmSeq,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::CborSeq,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .dtrmseq payload must be within bstr .cborseq payload"
        );
    }

    #[test]
    fn cbor_not_within_dtrm() {
        // The reverse direction must fail with a specific reason
        // string: `.cbor is broader than .dtrm`.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .cbor payload must not be within bstr .dtrm payload");
        assert_eq!(
            err, ".cbor is broader than .dtrm",
            "unexpected reason: {err}"
        );
    }

    #[test]
    fn cborseq_not_within_dtrmseq() {
        // The reverse direction for the sequence variant must fail
        // with a specific reason string: `.cborseq is broader than .dtrmseq`.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::CborSeq,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::DtrmSeq,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .cborseq payload must not be within bstr .dtrmseq payload");
        assert_eq!(
            err, ".cborseq is broader than .dtrmseq",
            "unexpected reason: {err}"
        );
    }

    #[test]
    fn dtrm_payload_not_within_narrower_dtrm_payload() {
        // `bstr .dtrm broader ⊄ bstr .dtrm narrower` — equal operators
        // require the controller to subtype. The RHS controller has a
        // required `2 => tstr` entry the LHS does not satisfy, so the
        // subtype must fail.
        let defs = DefinitionMap::from_nodes(&[]);
        let broader = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        };
        let narrower = ResolvedType::Map {
            entries: vec![
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(1),
                        hi: Some(1),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Int),
                    occurrence: Occurrence::One,
                },
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(2),
                        hi: Some(2),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
            ],
        };
        let lhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(broader),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(narrower),
        };
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .dtrm broader must not be within bstr .dtrm narrower");
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts
                .iter()
                .any(|c| matches!(c.kind, WithinConflictKind::MissingRequiredRhs)),
            "expected MissingRequiredRhs conflict, got {conflicts:#?}"
        );
        // The legacy string API still surfaces a human-readable reason.
        assert!(
            err.contains("expected at least 1 matching entries"),
            "expected legacy reason, got {err:?}"
        );
    }

    #[test]
    fn unknown_ctlop_different_texts_not_compatible() {
        // `ControlOp::Other(".plus")` vs `ControlOp::Other(".cat")`
        // must fail with a reason naming the operators.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Control {
            op: ControlOp::Other(".plus".to_owned()),
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Int)),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Other(".cat".to_owned()),
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Int)),
        };
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("two different Other operators must not be compatible");
        assert!(
            err.contains(".plus is not within .cat"),
            "unexpected reason: {err}"
        );
    }

    #[test]
    fn unknown_ctlop_same_text_is_compatible() {
        // `ControlOp::Other(".plus")` on both sides with subtype
        // controllers must pass via the equal-operators branch.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Control {
            op: ControlOp::Other(".plus".to_owned()),
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Range {
                lo: Some(1),
                hi: Some(1),
                is_float: false,
            }),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Other(".plus".to_owned()),
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Range {
                lo: Some(0),
                hi: Some(2),
                is_float: false,
            }),
        };
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "matching Other operators with subtype controllers must pass"
        );
    }

    // ------------------------------------------------------------------
    // Step 5: narrowing-control carrier regression
    // ------------------------------------------------------------------
    //
    // RFC 9171 lints `$extension-block .within canonical-block-structure`
    // and uses `uint .gt 1` for the `block-number` field. Before this
    // fix, the subtype checker compared `Control(.gt, uint, _)` against
    // the bare `uint` carrier on the RHS and reported
    // `control(.gt) not subtype of Uint (different structure)`.
    //
    // The correct semantics: a narrowing control operator `op` means
    // `Control(op, T, _) ⊆ T`, and therefore
    // `Control(op, T, _) ⊆ R` whenever `T ⊆ R`. The set covers
    // numeric range refinements (`.gt`/`.ge`/`.lt`/`.le`),
    // length refinements (`.size`), bit-layout refinements (`.bits`),
    // and CBOR encoding refinements (`.cbor`/`.cborseq`/`.dtrm`/`.dtrmseq`).
    //
    // Direction-specific rules (`.dtrm ⊆ .cbor`, `.cbor ⊄ .dtrm`) are
    // preserved: they still apply when the RHS is also a `Control`.

    #[test]
    fn uint_gt_1_within_uint() {
        // The headline regression case. `.gt` narrows the carrier
        // range, so `uint .gt 1 ⊆ uint` must hold.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Control {
            op: ControlOp::Gt,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Uint)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Int)),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Uint);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "uint .gt 1 must be within uint"
        );
    }

    #[test]
    fn uint_bits_within_uint() {
        // `.bits layout` narrows `uint` to a specific bit-layout shape.
        // The controller (the layout) is irrelevant for the carrier
        // narrowing check.
        let defs = DefinitionMap::from_nodes(&[]);
        let layout = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(0),
                    hi: Some(0),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Bool),
                occurrence: Occurrence::One,
            }],
        };
        let lhs = ResolvedType::Control {
            op: ControlOp::Bits,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Uint)),
            controller: Box::new(layout),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Uint);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "uint .bits <layout> must be within uint"
        );
    }

    #[test]
    fn bstr_size_2_within_bstr() {
        // `.size` narrows the bstr length. The carrier stays `bstr`,
        // so `bstr .size 2 ⊆ bstr` must hold.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Control {
            op: ControlOp::Size,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Int)),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .size 2 must be within bstr"
        );
    }

    #[test]
    fn bstr_cbor_within_bstr() {
        // `.cbor` narrows a `bstr` to a CBOR-encoded bstr. The
        // carrier stays `bstr`, so the subtype holds.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .cbor <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_dtrm_within_bstr() {
        // `.dtrm` is a further narrowing of `.cbor`, but against a
        // bare `bstr` RHS the carrier check is what matters.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .dtrm <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_cbor_not_within_bstr_dtrm() {
        // The Step 2 direction rule is preserved: a broader encoding
        // is not a subtype of a narrower encoding.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .cbor <payload> must not be within bstr .dtrm <payload>");
        assert_eq!(
            err, ".cbor is broader than .dtrm",
            "unexpected reason: {err}"
        );
    }

    // ------------------------------------------------------------------
    // .x-enc / .x-hash annotation regression
    // ------------------------------------------------------------------
    //
    // `.x-enc` and `.x-hash` are unofficial annotations that say "this
    // byte string is the result of encrypting / hashing the RHS".  The
    // RHS can be any type (it is the plaintext or preimage), but the
    // value seen at the schema boundary is always a `bstr`.  Subtype
    // checks must therefore treat them as carrier-narrowing operators
    // on `bstr`, the same way `.cbor` and `.dtrm` are: the carrier
    // check alone is what matters, and the controller is irrelevant.

    #[test]
    fn bstr_x_enc_within_bstr() {
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::XEnc,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-enc <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_x_hash_within_bstr() {
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let lhs = ResolvedType::Control {
            op: ControlOp::XHash,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-hash <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_x_enc_within_choice_bstr_or_nil() {
        // The dntls-cose-encrypt regression: the parent type's RHS is
        // `bstr / nil`, and the LHS uses `bstr .x-enc ''`.  The LHS
        // must be accepted by the first RHS choice arm.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Control {
            op: ControlOp::XEnc,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
        };
        let rhs = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Bstr),
            ResolvedType::Primitive(PrimitiveKind::Nil),
        ]);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-enc <payload> must be within bstr / nil"
        );
    }

    #[test]
    fn x_enc_x_hash_round_trip() {
        // The carrier narrowing rule makes the two annotations
        // interchangeable when the carrier is the same `bstr`.  The
        // operators themselves are distinct (`is_control_subtype`
        // rejects mismatched operator names), but the carrier check
        // must still pass for either direction so the choice / `.and`
        // recursion does not get stuck on the controller.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let enc = ResolvedType::Control {
            op: ControlOp::XEnc,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let hash = ResolvedType::Control {
            op: ControlOp::XHash,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let bstr = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&enc, &bstr, &defs).is_ok(),
            "bstr .x-enc <payload> must be within bstr"
        );
        assert!(
            is_subtype(&hash, &bstr, &defs).is_ok(),
            "bstr .x-hash <payload> must be within bstr"
        );
    }

    #[test]
    fn x_enc_is_narrowing() {
        // Regression guard: the `is_narrowing` predicate must include
        // both `.x-enc` and `.x-hash` so the carrier-only subtype
        // short-circuit kicks in for the dntls-cose-encrypt case.
        assert!(ControlOp::XEnc.is_narrowing());
        assert!(ControlOp::XHash.is_narrowing());
        assert!(ControlOp::XEnc.is_schema_relevant());
        assert!(ControlOp::XHash.is_schema_relevant());
    }

    // BUG-010: the `.abnf` / `.abnfb` annotated forms must collapse
    // to the same `ControlOp` as the base operator.  Without the
    // normalization the textual `.x-enc.abnfb` falls through as
    // `Other(...)` and the structured subtype collector rejects it
    // structurally against plain `bstr` instead of using the
    // carrier wire type.
    #[test]
    fn bug_010_x_enc_abnfb_normalizes_to_x_enc() {
        assert_eq!(
            ControlOp::from_text(".x-enc"),
            ControlOp::XEnc,
            ".x-enc must normalize to ControlOp::XEnc"
        );
        assert_eq!(
            ControlOp::from_text(".x-enc.abnf"),
            ControlOp::XEnc,
            ".x-enc.abnf must normalize to ControlOp::XEnc"
        );
        assert_eq!(
            ControlOp::from_text(".x-enc.abnfb"),
            ControlOp::XEnc,
            ".x-enc.abnfb must normalize to ControlOp::XEnc"
        );
    }

    #[test]
    fn bug_010_x_hash_abnfb_normalizes_to_x_hash() {
        assert_eq!(
            ControlOp::from_text(".x-hash"),
            ControlOp::XHash,
            ".x-hash must normalize to ControlOp::XHash"
        );
        assert_eq!(
            ControlOp::from_text(".x-hash.abnf"),
            ControlOp::XHash,
            ".x-hash.abnf must normalize to ControlOp::XHash"
        );
        assert_eq!(
            ControlOp::from_text(".x-hash.abnfb"),
            ControlOp::XHash,
            ".x-hash.abnfb must normalize to ControlOp::XHash"
        );
    }

    #[test]
    fn bug_010_bstr_x_enc_abnfb_within_bstr() {
        // BUG-010: `(bstr .size 48) .x-enc.abnfb (<abnf>)` must subtype
        // `bstr` via the carrier-narrowing short-circuit, the same as
        // the base `.x-enc` operator.  Without the `.abnfb`
        // normalization the type fell through to `ControlOp::Other` and
        // was rejected structurally against `bstr`.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        // The carrier is the narrowing annotation `bstr .size 48`,
        // which itself is a `Control { op: Size, carrier: Bstr,
        // controller: Range(48,48) }`.  Both `.size` and `.x-enc`
        // are narrowing; the chain narrows all the way down to `bstr`.
        let size_48 = ResolvedType::Control {
            op: ControlOp::Size,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(ResolvedType::Range {
                lo: Some(48),
                hi: Some(48),
                is_float: false,
            }),
        };
        let lhs = ResolvedType::Control {
            op: ControlOp::XEnc,
            carrier: Box::new(size_48),
            controller: Box::new(payload),
        };
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "(bstr .size 48) .x-enc.abnfb <payload> must be within bstr"
        );
    }

    #[test]
    fn bug_010_x_enc_abnfb_not_within_x_hash_abnfb() {
        // BUG-010: the transform-family constraint must still apply
        // after the `.abnfb` normalization.  `.x-enc.abnfb` and
        // `.x-hash.abnfb` are different transform families and must
        // not subtype each other.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = map_int_1();
        let enc = ResolvedType::Control {
            op: ControlOp::XEnc,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let hash = ResolvedType::Control {
            op: ControlOp::XHash,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        assert!(
            is_subtype(&enc, &hash, &defs).is_err(),
            ".x-enc.abnfb must not subtype .x-hash.abnfb"
        );
    }

    // ------------------------------------------------------------------
    // Step 4.46: compression annotation ctlops
    // ------------------------------------------------------------------
    //
    // `.x-compressed`, `.x-brotli`, `.x-zstd`, `.x-gzip`, and
    // `.x-deflate` are carrier-narrowing operators on `bstr`.  The
    // generic `.x-compressed` is the algorithm-agnostic parent of the
    // four named algorithms; two different named algorithms are not
    // mutually within each other.

    fn bstr_with_compression_op(
        op: ControlOp,
        payload: ResolvedType,
    ) -> ResolvedType {
        ResolvedType::Control {
            op,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        }
    }

    fn compression_payload() -> ResolvedType {
        map_int_1()
    }

    #[test]
    fn bstr_x_brotli_within_bstr() {
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XBrotli, payload);
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-brotli <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_x_compressed_within_bstr() {
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XCompressed, payload);
        let rhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-compressed <payload> must be within bstr"
        );
    }

    #[test]
    fn bstr_x_brotli_within_bstr_x_compressed() {
        // Named algorithm ⊆ generic: the carrier is compatible
        // (`bstr ⊆ bstr`) and the controller must subtype.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XBrotli, payload.clone());
        let rhs = bstr_with_compression_op(ControlOp::XCompressed, payload);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-brotli <payload> must be within bstr .x-compressed <payload>"
        );
    }

    #[test]
    fn bstr_x_compressed_within_bstr_x_brotli_fails() {
        // The generic is broader than a named algorithm and is not
        // within it.  The diagnostic must mention this directionality.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XCompressed, payload.clone());
        let rhs = bstr_with_compression_op(ControlOp::XBrotli, payload);
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .x-compressed <payload> must NOT be within bstr .x-brotli <payload>");
        assert!(
            err.contains(".x-compressed is broader than a named compression algorithm"),
            "expected broader-than reason, got: {err}"
        );
    }

    #[test]
    fn bstr_x_brotli_within_bstr_x_zstd_fails() {
        // Two different named algorithms are not mutually within each
        // other.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XBrotli, payload.clone());
        let rhs = bstr_with_compression_op(ControlOp::XZstd, payload);
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr .x-brotli <payload> must NOT be within bstr .x-zstd <payload>");
        assert!(
            err.contains("compression algorithm"),
            "expected compression algorithm reason, got: {err}"
        );
        assert!(
            err.contains(".x-brotli") && err.contains(".x-zstd"),
            "expected both operator names in the reason, got: {err}"
        );
    }

    #[test]
    fn bstr_x_zstd_within_bstr_x_zstd() {
        // Equal named algorithms compare their controllers
        // structurally.  Same payload → success.
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = compression_payload();
        let lhs = bstr_with_compression_op(ControlOp::XZstd, payload.clone());
        let rhs = bstr_with_compression_op(ControlOp::XZstd, payload);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr .x-zstd <payload> must be within bstr .x-zstd <payload>"
        );
    }

    #[test]
    fn x_brotli_is_narrowing() {
        // Regression guard: the `is_narrowing` predicate must include
        // every named compression algorithm plus the generic
        // `.x-compressed`, otherwise the carrier-narrowing short-circuit
        // won't kick in for the service-record-v1.brotli regression.
        for op in [
            ControlOp::XCompressed,
            ControlOp::XBrotli,
            ControlOp::XZstd,
            ControlOp::XGzip,
            ControlOp::XDeflate,
        ] {
            assert!(op.is_narrowing(), "{op:?} must be narrowing");
            assert!(op.is_schema_relevant(), "{op:?} must be schema-relevant");
        }
    }

    #[test]
    fn compression_op_text_round_trip() {
        // The `from_text` parser must normalize every base name and
        // every `.abnf` / `.abnfb` annotated form to the same
        // `ControlOp` variant, since the carrier narrowing behavior is
        // identical regardless of the ABNF annotation.
        for (text, expected) in [
            (".x-compressed", ControlOp::XCompressed),
            (".x-compressed.abnf", ControlOp::XCompressed),
            (".x-compressed.abnfb", ControlOp::XCompressed),
            (".x-brotli", ControlOp::XBrotli),
            (".x-brotli.abnf", ControlOp::XBrotli),
            (".x-brotli.abnfb", ControlOp::XBrotli),
            (".x-zstd", ControlOp::XZstd),
            (".x-zstd.abnf", ControlOp::XZstd),
            (".x-zstd.abnfb", ControlOp::XZstd),
            (".x-gzip", ControlOp::XGzip),
            (".x-gzip.abnf", ControlOp::XGzip),
            (".x-gzip.abnfb", ControlOp::XGzip),
            (".x-deflate", ControlOp::XDeflate),
            (".x-deflate.abnf", ControlOp::XDeflate),
            (".x-deflate.abnfb", ControlOp::XDeflate),
        ] {
            let actual = ControlOp::from_text(text);
            assert_eq!(actual, expected, "from_text({text:?})");
            // The canonical `as_text` form is the base operator name;
            // the `.abnf` / `.abnfb` annotated forms collapse to it.
            let base = text.split(".abnf").next().unwrap_or(text);
            assert_eq!(actual.as_text(), base);
        }
    }

    // ------------------------------------------------------------------
    // Step 5: choice containment regression
    // ------------------------------------------------------------------
    //
    // Fixes the bug where `bstr / #6.24(bstr) ⊆ bstr / #6.24(bstr)`
    // was rejected even though every LHS choice arm is accepted by at
    // least one RHS choice arm. The fix ensures:
    //  * L ⊆ (A / B / C): conflicts from rejected arms are discarded when any arm accepts L.
    //  * (A / B / C) ⊆ R: each LHS alternative is checked against R.
    //  * (A / B / C) ⊆ (X / Y / Z): each LHS alternative passes if at least one RHS
    //    alternative accepts it.

    #[test]
    fn choice_containment_ident_matching_arms() {
        // `Choice ⊆ Choice` where every LHS arm has a matching RHS arm.
        let defs = DefinitionMap::from_nodes(&[]);
        let bstr = ResolvedType::Primitive(PrimitiveKind::Bstr);
        let tagged = ResolvedType::Tag {
            tag: 24,
            inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
        };
        let lhs = ResolvedType::Choice(vec![bstr.clone(), tagged.clone()]);
        let rhs = ResolvedType::Choice(vec![bstr, tagged]);
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts.is_empty(),
            "Choice ⊆ Choice with matching arms should have no conflicts, got {conflicts:#?}"
        );
    }

    #[test]
    fn bstr_within_choice_passes() {
        // `bstr ⊆ bstr / #6.24(bstr)` — the first RHS arm matches.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Primitive(PrimitiveKind::Bstr);
        let rhs = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Bstr),
            ResolvedType::Tag {
                tag: 24,
                inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            },
        ]);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "bstr must be within bstr / #6.24(bstr)"
        );
    }

    #[test]
    fn tagged_within_choice_passes() {
        // `#6.24(bstr) ⊆ bstr / #6.24(bstr)` — the second RHS arm matches.
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Tag {
            tag: 24,
            inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
        };
        let rhs = ResolvedType::Choice(vec![
            ResolvedType::Primitive(PrimitiveKind::Bstr),
            ResolvedType::Tag {
                tag: 24,
                inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            },
        ]);
        assert!(
            is_subtype(&lhs, &rhs, &defs).is_ok(),
            "#6.24(bstr) must be within bstr / #6.24(bstr)"
        );
    }

    #[test]
    fn choice_not_within_narrower_choice() {
        // `bstr / #6.24(bstr) ⊄ #6.24(bstr)` — the first LHS arm
        // (bstr) does not match the sole RHS arm.
        let defs = DefinitionMap::from_nodes(&[]);
        let bstr = ResolvedType::Primitive(PrimitiveKind::Bstr);
        let tagged = ResolvedType::Tag {
            tag: 24,
            inner: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
        };
        let lhs = ResolvedType::Choice(vec![bstr, tagged.clone()]);
        let rhs = tagged;
        let err = is_subtype(&lhs, &rhs, &defs)
            .expect_err("bstr / #6.24(bstr) must not be within #6.24(bstr)");
        assert!(!err.is_empty(), "expected a reason, got empty error");
    }

    #[test]
    fn choice_containment_via_validate_within_pass() {
        // Full-pipeline regression: parse → resolve → validate_within_pass.
        // Verifies that `Choice ⊆ Choice` works end-to-end.
        let source = "\
            block-type-specific-data = bstr / #6.24(bstr)\n\
            payload = [bstr / #6.24(bstr)] .within [bstr / #6.24(bstr)]\n\
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        assert!(warnings.is_empty(), "expected no E030, got {warnings:#?}");
    }

    #[test]
    fn choice_containment_with_named_ref() {
        // Uses a named reference to a choice, matching how rfc9171
        // defines `block-type-specific-data = bstr / #6.24(bstr)`.
        let source = "\
            block-type-specific-data = bstr / #6.24(bstr)\n\
            payload = [block-type-specific-data] .within [block-type-specific-data]\n\
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        assert!(warnings.is_empty(), "expected no E030, got {warnings:#?}");
    }

    #[test]
    fn rfc9171_payload_block_shape_passes() {
        // Matches the element types of payload-block-structure / canonical-block-structure
        // from rfc9171 §3.6.
        let source = r"
            block-type-specific-data = bstr / #6.24(bstr)
            crc-value = bstr .size 2 / bstr .size 4
            payload-block-structure = [
                1,
                1,
                uint .bits &(a: 0, b: 1, c: 2),
                [0, 1, 2],
                block-type-specific-data,
                ? crc-value
            ]
            canonical-block-structure = [
                uint,
                uint,
                uint .bits &(a: 0, b: 1, c: 2),
                [0, 1, 2],
                block-type-specific-data,
                ? crc-value
            ]
            rule = payload-block-structure .within canonical-block-structure
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        let e030: Vec<_> = warnings.iter().filter(|w| w.code == "E030").collect();
        assert!(e030.is_empty(), "expected no E030, got {e030:#?}");
    }

    #[test]
    fn rfc9171_socket_ref_passes() {
        // Reproduce the exact rfc9171 pattern where the LHS array
        // uses `$payload-block-data` (a socket) and the RHS uses
        // `block-type-specific-data` (the socket's plug target).
        let source = r"
            block-type-specific-data = bstr / #6.24(bstr)
            $payload-block-data /= block-type-specific-data
            crc-value = bstr .size 2 / bstr .size 4
            payload-block-structure = [
                1,
                1,
                uint .bits &(a: 0, b: 1, c: 2),
                [0, 1, 2],
                $payload-block-data,
                ? crc-value
            ]
            canonical-block-structure = [
                uint,
                uint,
                uint .bits &(a: 0, b: 1, c: 2),
                [0, 1, 2],
                block-type-specific-data,
                ? crc-value
            ]
            rule = payload-block-structure .within canonical-block-structure
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        let e030: Vec<_> = warnings.iter().filter(|w| w.code == "E030").collect();
        assert!(e030.is_empty(), "expected no E030, got {e030:#?}");
    }

    #[test]
    fn within_constraint_used_as_rhs_compares_against_carrier() {
        // A rule defined as `A .within B` still denotes values of
        // shape `A`. When it is used as the RHS of another `.within`,
        // the checker must compare against the carrier `A`, not treat
        // the `.within` assertion wrapper as a distinct structure.
        let source = r"
            generic-map = {
                2*2 int => bstr
            }

            one-signature //= (-48 => bstr .size 2420)
            one-signature //= (-49 => bstr .size 3309)

            sig-map = {
                -19 => bstr .size 64,
                one-signature
            } .within generic-map

            concrete-sig-map = {
                -19 => bstr .size 64,
                -48 => bstr .size 2420
            } .within sig-map
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        let e030: Vec<_> = warnings.iter().filter(|w| w.code == "E030").collect();
        assert!(e030.is_empty(), "expected no E030, got {e030:#?}");
    }

    // ------------------------------------------------------------------
    // Step 7.5: RFC9581 group-socket in a `.within` LHS
    // ------------------------------------------------------------------
    //
    // RFC9581 defines `etime-detailed = ({ $$BASE, * $$ELECTIVE, *
    // $$CRITICAL }) .within etime-framework`. The LHS supplies its
    // required base-time entries through `$$BASE //= (1: ...) / (4:
    // ...) / (5: ...)` group-socket augmentations. The RHS framework
    // is a map `{ uint => any, * nint / text => any, * uint => any
    // }`. Every concrete key in the LHS (1, 4, 5) is within `uint`
    // and every concrete value is within `any`, so the check should
    // pass. Before Step 7.5 it failed with
    //
    //     map[0]: LHS required entry has no matching RHS entry
    //
    // because the LHS's socket-driven group choice was compared
    // against a single RHS map entry by reference rather than by
    // distributing the choice arms.
    #[test]
    fn rfc9581_group_socket_lhs_passes() {
        // Direct model of the RFC9581 shape. The LHS supplies its
        // required base-time entries through a group socket whose
        // plugs use the `key: value` form (`1: T` etc.). The RHS is
        // a map whose entries have schema keys that should accept
        // the LHS concrete keys.
        let source = r"
            framework = {
                uint => any,
                * uint => any,
                * uint => any
            }

            detailed = ({
                $$BASE
                * $$ELECTIVE
                * $$CRITICAL
            }) .within framework

            $$BASE //= (1: 0)
            $$BASE //= (4: 0)
            $$BASE //= (5: 0)
            $$ELECTIVE //= (-3: 0)
            $$CRITICAL //= (13: 0)
        ";
        let nodes = parse_snippet(source);
        let mut warnings = Vec::new();
        validate_within_pass(&nodes, &mut warnings);
        let e030: Vec<_> = warnings.iter().filter(|w| w.code == "E030").collect();
        assert!(e030.is_empty(), "RFC9581 .within should pass: {e030:#?}");
    }

    // ------------------------------------------------------------------
    // Step 3: structured conflict kind tests
    // ------------------------------------------------------------------

    #[test]
    fn subtype_conflict_primitive_mismatch() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Primitive(PrimitiveKind::Tstr);
        let rhs = ResolvedType::Primitive(PrimitiveKind::Int);
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts
                .iter()
                .any(|c| matches!(c.kind, WithinConflictKind::PrimitiveMismatch)),
            "expected PrimitiveMismatch, got {conflicts:#?}"
        );
    }

    #[test]
    fn subtype_conflict_missing_map_key() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        };
        let rhs = ResolvedType::Map {
            entries: vec![
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(1),
                        hi: Some(1),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Int),
                    occurrence: Occurrence::One,
                },
                MapEntry {
                    key: ResolvedType::Range {
                        lo: Some(2),
                        hi: Some(2),
                        is_float: false,
                    },
                    value: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
            ],
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts
                .iter()
                .any(|c| matches!(c.kind, WithinConflictKind::MissingRequiredRhs)),
            "expected MissingRequiredRhs, got {conflicts:#?}"
        );
    }

    #[test]
    fn subtype_conflict_control_mismatch() {
        let defs = DefinitionMap::from_nodes(&[]);
        let payload = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        };
        let lhs = ResolvedType::Control {
            op: ControlOp::Cbor,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload.clone()),
        };
        let rhs = ResolvedType::Control {
            op: ControlOp::Dtrm,
            carrier: Box::new(ResolvedType::Primitive(PrimitiveKind::Bstr)),
            controller: Box::new(payload),
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts
                .iter()
                .any(|c| matches!(c.kind, WithinConflictKind::ControlMismatch)),
            "expected ControlMismatch, got {conflicts:#?}"
        );
    }

    // ------------------------------------------------------------------
    // Step 4: .and (intersection) semantics
    // ------------------------------------------------------------------

    /// `non-empty<M> = (M) .and ({ + any => any })` — the empty map
    /// must not satisfy the intersection because `{ + any => any }`
    /// requires at least one entry.
    #[test]
    fn and_intersection_empty_map_not_satisfied() {
        let defs = DefinitionMap::from_nodes(&[]);
        let empty = ResolvedType::Map { entries: vec![] };
        let non_empty_req = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Any,
                value: ResolvedType::Any,
                occurrence: Occurrence::OneOrMore,
            }],
        };
        let intersection = ResolvedType::Intersection(vec![empty.clone(), non_empty_req]);
        let conflicts = subtype_conflicts(&empty, &intersection, &defs);
        assert!(
            !conflicts.is_empty(),
            "empty map must not satisfy non-empty intersection"
        );
    }

    /// `non-empty-map = { 1 => int } .and ({ + any => any })` —
    /// a non-empty concrete map satisfies the intersection because
    /// it has entries matching the `+ any => any` operand.
    #[test]
    fn and_intersection_non_empty_map_satisfied() {
        let defs = DefinitionMap::from_nodes(&[]);
        let concrete = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                value: ResolvedType::Primitive(PrimitiveKind::Int),
                occurrence: Occurrence::One,
            }],
        };
        let non_empty_req = ResolvedType::Map {
            entries: vec![MapEntry {
                key: ResolvedType::Any,
                value: ResolvedType::Any,
                occurrence: Occurrence::OneOrMore,
            }],
        };
        let intersection = ResolvedType::Intersection(vec![concrete.clone(), non_empty_req]);
        let conflicts = subtype_conflicts(&concrete, &intersection, &defs);
        assert!(
            conflicts.is_empty(),
            "non-empty map must satisfy intersection, got {conflicts:#?}"
        );
    }

    /// `int .and tstr` is an impossible intersection: no value is
    /// both an int and a tstr. Check that `L ⊆ Intersection([int, tstr])`
    /// fails for any concrete L.
    #[test]
    fn and_intersection_impossible_fails() {
        let defs = DefinitionMap::from_nodes(&[]);
        let intersection = ResolvedType::Intersection(vec![
            ResolvedType::Primitive(PrimitiveKind::Int),
            ResolvedType::Primitive(PrimitiveKind::Tstr),
        ]);
        // int can't satisfy `tstr` arm
        {
            let lhs = ResolvedType::Primitive(PrimitiveKind::Int);
            let conflicts = subtype_conflicts(&lhs, &intersection, &defs);
            assert!(!conflicts.is_empty(), "int must not satisfy int .and tstr");
        }
        // tstr can't satisfy `int` arm
        {
            let lhs = ResolvedType::Primitive(PrimitiveKind::Tstr);
            let conflicts = subtype_conflicts(&lhs, &intersection, &defs);
            assert!(!conflicts.is_empty(), "tstr must not satisfy int .and tstr");
        }
    }

    /// `Intersection([int, uint]) ⊆ int` must hold (conservative
    /// check: both operands must be within the RHS, and uint ⊆ int).
    #[test]
    fn and_intersection_within_conservative() {
        let defs = DefinitionMap::from_nodes(&[]);
        let intersection = ResolvedType::Intersection(vec![
            ResolvedType::Primitive(PrimitiveKind::Int),
            ResolvedType::Primitive(PrimitiveKind::Uint),
        ]);
        let rhs = ResolvedType::Primitive(PrimitiveKind::Int);
        let conflicts = subtype_conflicts(&intersection, &rhs, &defs);
        assert!(
            conflicts.is_empty(),
            "Intersection([int, uint]) ⊆ int must hold, got {conflicts:#?}"
        );
    }

    // ------------------------------------------------------------------
    // Step 4: trailing-repeat array subtype tests
    // ------------------------------------------------------------------

    /// `[3, text, [* text]] ⊆ [0..255, * any]` — the trailing
    /// `* any` absorbs both the second and third LHS elements.
    #[test]
    fn array_trailing_star_absorbs_extra_elements() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(3),
                        hi: Some(3),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Array {
                        elements: vec![ArrayElement {
                            ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                            occurrence: Occurrence::ZeroOrMore,
                        }],
                    },
                    occurrence: Occurrence::One,
                },
            ],
        };
        let rhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(0),
                        hi: Some(255),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Any,
                    occurrence: Occurrence::ZeroOrMore,
                },
            ],
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts.is_empty(),
            "[3, text, [* text]] must be subtype of [0..255, * any], got {conflicts:#?}"
        );
    }

    /// `[4, text, text, bool] ⊆ [0..255, * any]` — the trailing
    /// `* any` absorbs three extra LHS elements.
    #[test]
    fn array_trailing_star_absorbs_many_extra() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(4),
                        hi: Some(4),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Primitive(PrimitiveKind::Bool),
                    occurrence: Occurrence::One,
                },
            ],
        };
        let rhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(0),
                        hi: Some(255),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Any,
                    occurrence: Occurrence::ZeroOrMore,
                },
            ],
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts.is_empty(),
            "[4, text, text, bool] must be subtype of [0..255, * any], got {conflicts:#?}"
        );
    }

    /// A shorter array satisfies a trailing `*` pattern: `[1] ⊆
    /// [0..255, * any]`. The trailing repeat's min is 0, so zero
    /// extra elements is fine.
    #[test]
    fn array_shorter_still_satisfies_trailing_star() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Array {
            elements: vec![ArrayElement {
                ty: ResolvedType::Range {
                    lo: Some(1),
                    hi: Some(1),
                    is_float: false,
                },
                occurrence: Occurrence::One,
            }],
        };
        let rhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(0),
                        hi: Some(255),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Any,
                    occurrence: Occurrence::ZeroOrMore,
                },
            ],
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            conflicts.is_empty(),
            "[1] must be subtype of [0..255, * any], got {conflicts:#?}"
        );
    }

    /// Arrays fail when the fixed prefix does not match, even with
    /// a trailing `*`. `[tstr] ⊄ [0..255, * any]` because `tstr`
    /// is not within `0..255`.
    #[test]
    fn array_trailing_star_prefix_mismatch_fails() {
        let defs = DefinitionMap::from_nodes(&[]);
        let lhs = ResolvedType::Array {
            elements: vec![ArrayElement {
                ty: ResolvedType::Primitive(PrimitiveKind::Tstr),
                occurrence: Occurrence::One,
            }],
        };
        let rhs = ResolvedType::Array {
            elements: vec![
                ArrayElement {
                    ty: ResolvedType::Range {
                        lo: Some(0),
                        hi: Some(255),
                        is_float: false,
                    },
                    occurrence: Occurrence::One,
                },
                ArrayElement {
                    ty: ResolvedType::Any,
                    occurrence: Occurrence::ZeroOrMore,
                },
            ],
        };
        let conflicts = subtype_conflicts(&lhs, &rhs, &defs);
        assert!(
            !conflicts.is_empty(),
            "[tstr] must not be subtype of [0..255, * any]"
        );
    }
}
