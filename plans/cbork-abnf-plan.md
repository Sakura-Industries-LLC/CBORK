# ABNF Parser Plan

This plan tracks the work needed to support ABNF literals end to end in `cbork`.
The crate now already exposes an owned, queryable ABNF document model instead of a thin `pest::Pairs` wrapper.
The remaining work is about integration with the compiler and the postlude-aware validation path.

## Goals

* Parse ABNF literal strings into a real typed AST, not just a Pest pair tree.
* Keep the original source text available alongside the parsed representation.
* Preserve enough span and text information to produce useful diagnostics.
* Keep the grammar RFC 5234 compatible.
* Support a physical, merged compiler tree that includes standard postlude entries marked with metadata.
* Add tests that exercise the exact source form expected from `.abnf` and `.abnfb` inputs.

## Non-goals

* Rewriting the grammar backend away from Pest.
* Normalizing ABNF into a different syntax.
* Relaxing RFC 5234 newline rules to accept non-conformant input.
* Implementing semantic validation of ABNF meaning beyond syntax parsing.

## Current State

The ABNF crate now provides:

* `AbnfDocument`, an owned document model with source retention
* `AbnfRule`, `AbnfElement`, and related typed tree nodes
* `SourceSpan` on meaningful nodes
* `AbnfDocument::validate_bytes(...)`
* `AbnfDocument::validate_text(...)`
* `AbnfValidationError`
* A Pest grammar in `src/grammar/rfc_5234.pest`
* Parser tests plus owned-document and validation tests

The compiler now also:

* parses `.abnf` and `.abnfb` literals
* validates that the LHS target is `text` or `bytes`
* promotes RHS byte literals to UTF-8 text before parsing ABNF
* stores parsed ABNF in the resolver cache
* parses `.regexp` literals
* compiles `.regexp` patterns with `regexml`
* stores the original regex source plus compiled regex in the cache
* keeps the standard postlude as a separate parsed tree in `CompiledCDDL`

That means literal parsing and compilation are effectively done.
The remaining work is the postlude-aware semantic pass and a physical complete-tree merge.

## Proposed API Shape

The crate should expose an owned document type, something along these lines:

* `AbnfDocument`
    * owns the original source string
    * owns the parsed AST
    * may also keep source metadata such as an optional origin label
* `AbnfRule`
    * one parsed rule definition
* `AbnfElement`
    * one parsed piece of a rule expression
* `AbnfValue`
    * terminal value forms such as literal strings and numeric values
* `AbnfSpan`
    * byte offsets, line/column information, or both
* `AbnfParseError`
    * parse error plus context useful to upstream callers

The important design point is ownership.
The parsed document should not borrow from the caller’s string if the caller needs to keep both the source text
and the parsed form around for later passes.

This part is already in place.

## Work Items

### 1. Owned document model

Status: done.

The crate now exposes an owned ABNF document model rather than `pest::Pairs`.

The model includes:

* document level source retention
* parsed rules in source order
* typed nodes for the main ABNF constructs
* spans on meaningful nodes
* validation helpers for bytes and text input

### 2. Pest-to-AST conversion

Status: done.

The Pest tree is already converted into the owned ABNF document model in a separate parser pass.

### 3. Source retention

Status: done.

The original source string is preserved on the document.

That allows the compiler to keep the original literal text around while also holding the parsed structure.

### 4. Strict grammar

Status: done.

RFC 5234 newline behavior remains strict.

The parser should continue to reject non-conformant trailing-EOF ABNF.

### 5. Focused tests

Status: done.

The crate already has parser tests plus owned-document and validation tests.

The compiler also has `.abnf` and `.abnfb` integration tests.

### 6. Consumer-facing entrypoint

Status: done.

The parser crate already exposes a downstream-friendly entrypoint that accepts input text and returns the owned document on success.

### 7. ABNF compiler wiring

Status: done for literal compilation, partial for postlude-aware resolution.

The compiler now:

* validates `.abnf` / `.abnfb` LHS targets
* accepts RHS text or UTF-8-promotable bytes
* parses ABNF immediately
* stores the parsed document in the cache

What is still partial here is the full postlude-aware resolution pass.
Right now the compiler keeps `user_nodes` and `postlude_nodes` separate.

### 8. Regex literal wiring

Status: done.

The compiler now also parses `.regexp`, compiles it with `regexml`,
and stores both the original pattern and the compiled regex in the cache.

### 8.1. Unofficial annotation and transform ctlops

Status: partially implemented, with additional extensions planned.

The compiler already supports unofficial annotation-style ctlops such as:

* `.x-enc`
* `.x-enc.abnf`
* `.x-enc.abnfb`
* `.x-hash`
* `.x-hash.abnf`
* `.x-hash.abnfb`

These preserve structure for nested payloads that would otherwise collapse to opaque `bstr` values.

The same extension family should grow to include reversible compression transforms:

* `bstr .x-zip any` with .abnf and .abnfb forms
* `bstr .x-gz any` with .abnf and .abnfb forms
* `bstr .x-lz99 any` with .abnf and .abnfb forms
* `bstr .x-brotli any` with .abnf and .abnfb forms
* `bstr .x-zstd any` with .abnf and .abnfb forms
* `bstr .x-compressed any` with .abnf and .abnfb forms (This means unknown/unspecified compression)

The intended model is:

* the wire value is still a `bstr`
* the bytes contain a compressed encoding of the RHS payload
* the transform kind is part of the stored compiled metadata
* if the RHS is concrete enough to validate, a validator should decompress the
  byte string and continue walking the RHS tree
* if the RHS is `any`, the operator still remains useful as a structural and
  documentation annotation even when recursive validation stops there

This is a useful distinction from `.x-enc` and `.x-hash`:

* compression transforms are reversible without extra application knowledge
* validators can therefore keep walking the schema after decompression
* encryption and hashing remain annotation-preserving wrappers unless higher
  layers can reverse or interpret them

These compression ctlops should be documented as unofficial extensions for now.
They are intended to support advanced nested payload specifications such as compressed-then-encrypted service records
and may later feed an RFC draft on advanced data annotations.

### 9. Physical complete tree

Status: new requirement.

The compiler should build a physically complete AST for validation.

That means:

* the standard postlude should be materialized into the tree only when it is
  referenced by the user document
* postlude-injected nodes should be tagged with metadata indicating they came
  from the standard postlude
* postlude entries should not be injected if the user already defines the name
  with the same content or with conflicting content
* validation should run over one complete tree only after the semantic pass has
  determined the tree is error free enough to validate
* if the AST still has errors, it is not a valid validation input and should
  remain a compilation failure rather than being used for downstream checks

This is the new shape that should drive the next compiler pass.

### 10. Postlude-aware semantic pass

Status: not yet implemented.

Add a postlude-aware pass after literal derivation and compilation.

This pass should:

* detect redundant user definitions with the same content and warn
* detect conflicting user definitions and error on both
* compare user definitions against standard postlude definitions
* resolve missing RHS references from the standard postlude when possible
* error on unresolved RHS references that are not in the postlude
* keep enough provenance so diagnostics can point to both sides of a conflict

This pass should operate on the complete tree described above.

### 11. Authoritative tag tables

Status: future extension.

The same pass will likely need to grow beyond the standard postlude and check against other authoritative tag tables as well.

Examples include:

* IANA-controlled CBOR tag definitions
* standards-defined tags below `32768`
* other registry-backed definitions that should behave like postlude entries

The intended model is:

* `0-32767` is built-in authoritative tag space
* if a low tag is known to the tool, treat it as the standard definition
* if a low tag is unknown, warn or error depending on lint mode
* if a reference resolves only through an authoritative table, inject that
  definition into the complete tree the same way the postlude is injected
* if the document redefines an authoritative tag with different content, flag
  it as a hard error
* if the document repeats an authoritative definition with the same content,
  flag it as a warning
* if the tool lags the standards for a low tag, whitelist/update the built-in
  table rather than treating the range as user-defined space

This is not part of the immediate postlude pass.
It is the next semantic layer to preserve for later work.

## Suggested Implementation Order

1. Keep the current ABNF and regex literal plumbing stable.
2. Add the postlude injection step that produces one physically complete tree.
3. Tag injected standard postlude nodes with metadata.
4. Add the postlude-aware semantic pass over the complete tree.
5. Extend diagnostics so conflicts and redundant definitions point at both sides.
6. Recheck the compiler with the new pass ordering.

## Acceptance Criteria

The work is complete when:

* `cbork-abnf-parser` continues to return a real owned document type.
* The original source text is preserved alongside the AST.
* The tree contains enough structure and span information for later validation.
* The compiler has a physically complete tree that includes the standard postlude.
* Postlude-derived nodes are tagged with metadata.
* The postlude-aware semantic pass handles the six cases listed above.
* The plan records that `0-32767` is built-in authoritative tag space.
* The plan leaves room for additional authoritative tag tables such as IANA
  registries.
* `cbork` can consume the parsed ABNF and regex literals without re-parsing the source string.
