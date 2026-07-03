# `cbork` CLI Plan

This plan describes the command surface for the `cbork` CLI.
The goal is not to make `cbork` a one-off linter wrapper.
It should become the user-facing entrypoint for the CBORK toolchain,
with the linter as the first fully useful command and the other capabilities layered on in the same CLI shape.

The CLI parser should use `bpaf`.
Terminal presentation should use `rusty-rich` so diagnostics, trees, and examples can be rendered with structured,
readable output instead of plain text only.

## Product Intent

`cbork` should expose the ecosystem features described in `docs/product-features.md` in a way that is:

* strict by default
* source-aware
* rich in diagnostics
* deterministic in output
* suitable for both humans and CI

The CLI should cover the major CBORK workflows:

* linting CDDL
* formatting CDDL
* compiling and inspecting enriched ASTs
* rendering expanded, documentation-friendly schemas
* validating CBOR against schema roots
* producing diagnostic notation
* measuring schema/vector coverage
* explaining schema meaning in human-readable form

Some ecosystem pieces will remain separate crates or binaries, such as the tree-sitter grammar and likely the LSP server.
`cbork` should still be the umbrella CLI for the user-facing contract tooling.

## Current State (updated 2026-06-16, `feat/within` branch)

### CLI — all subcommands parsed, most are real

| Command   | Status |
|-----------|--------|
| `lint`    | **Wired.** Compiles, resolves includes/imports, expands generics, resolves constants, runs ctlop pass, finalizes, validates `.within` constraints (including `.and` intersections via the subtype checker), prunes postlude, reports diagnostics with `--why`, `--stats`, `--summary`, `--strict`. |
| `compile` | **Wired.** Prints `dump_tree` from `CompiledCDDL`. |
| `why`     | **Wired.** Diagnostic codes → embedded RFC citations. |
| `xref`    | **Wired.** Grammar terms → embedded RFC citations. |
| `rfc`     | **Wired.** Lists or dumps embedded RFC text. |
| `render`  | **Wired.** Resolution-driven concrete CDDL view (see § Concrete Renderer below). |
| `decode`  | **Wired.** CBOR → EDN dump via `cbork-edn`. |
| `validate`| **Wired.** Validates CBOR against a compiled CDDL schema. |
| `fmt`     | Stub (prints "not yet implemented"). |
| `explain` | Stub. |
| `coverage`| Stub. |
| `docs`    | Stub. |
| `lsp`     | Stub. |

### Compiler pipeline

`CompiledCDDL::compile` → parse (pest) → preprocess → inject directives → resolve includes/imports → expand generics → normalize
definition strengths → resolve constants (fixed-point loop: seed, rangeop, ctlop passes) → finalize (prune, inject postlude,
validate `.within` constraints, including `.and` intersections used by those constraints).

* `complete_nodes`: the post-prune, post-include, post-generic tree.
* `ResolverCache`: maps names to `EntryState` (int, float, text, bytes, range, regex, abnf).
* `SymbolKind`/`AssignmentKind`/`RuleHead`: classify declarative vs augmenting rules.
* Sockets (`$name`, `$$name`) tracked via `//=` augmentations.

### `.within` — wired and usable for covered lint semantics

* `mod within` is wired in `finalize.rs` via `validate_within_pass(&complete_nodes)`.
* It builds its own `DefinitionMap` and socket-choice state parallel to the compiler's semantic cache.
  This is intentional for now; Step 9 keeps it until replacement parity is proven.
* It resolves both sides into `ResolvedType` and checks `lhs ⊆ rhs` with `is_subtype()`.
* It emits `E030` with structured related subdiagnostics derived from `schema_diff::build_schema_diff()`.
* Diagnostics use the concrete effective renderer for readable LHS/RHS diff lines.
* The CLI renderer in `cbork/src/diagnostics.rs` renders ordered diff subdiagnostics under a single `= DIFF:` block.
* The subtype checker preserves schema-level ctlops such as `.cbor`, `.dtrm`, `.size`, `.bits`, `.gt`, `.ge`, `.lt`, and `.le`.
* Directional ctlop containment is implemented:
  `.dtrm` is within `.cbor`, but `.cbor` is not within `.dtrm`.
* Current coverage includes focused unit tests, RFC9171/RFC9581 regressions, semantic-error fixtures, positive fixtures,
  and CLI lint tests.

### `.and` — wired as schema intersection for lint

* `.and` is accepted by `validate_ctlop_semantics()` as a relation-family ctlop.
* `resolve_type1()` lowers `A .and B` to `ResolvedType::Intersection(vec![A, B])`.
* The subtype checker implements intersection rules:
    * `L ⊆ (A .and B)` requires `L ⊆ A` and `L ⊆ B`.
    * `(A .and B) ⊆ R` uses the conservative current rule requiring every operand to be within `R`.
* `.and` failures surface through `.within` diagnostics and use the same concrete inline diff path.
* Covered behavior includes non-empty-map intersections, impossible primitive intersections, and RFC8610 `.within`/`.and`
  positive vectors.

### Standards corpus — embedded

`cbork/src/rfc.rs` embeds RFC 8610/8742/9090/9165/9682/9741 plus drafts.
`why.rs` maps diagnostic codes to cited line ranges.
`xref.rs` maps grammar concepts.

### Diagnostic infrastructure extended

`Diagnostic` now has `related: Vec<Subdiag>`.
`Subdiag`/`SubdiagKind` are defined in `error.rs` and are consumed by the CLI diagnostic renderer.
The current renderer prints ordered `.within` / `.and` diff subdiagnostics as a single inline `= DIFF:` block.
Legacy `LHS` / `RHS` related snippets still render as separate labelled blocks.

## Global CLI Shape

`cbork` should have a small set of global flags that apply across commands:

* `--color <auto|always|never>`
* `--quiet`
* `--verbose`
* `--format <rich|plain|json>`
* `--config <path>`
* `--no-rich`
* `--help`
* `--version`

The default presentation should be rich terminal output.
Machine-oriented formats should remain available for CI and scripting.

`bpaf` should parse the command tree and handle option composition.
`rusty-rich` should handle the actual rendering of warnings, errors, trees, tables, annotations, and summary blocks.

## Library Mode

`--library` is an explicit authoring mode for reusable schema modules.
It relaxes the requirement that a file has exactly one concrete document root,
but it does not make the file usable for every workflow.

In library mode:

* top-level dangling definitions are allowed as library exports
* `lint`, `compile`, `explain`, and `docs` may inspect the file as a library
* `validate`, `decode`, and `coverage` must reject library input because they require a concrete document root
* if a library does not define an explicit aggregate export rule, `lint` should warn and suggest one
* `lint-fix` should be able to insert that aggregate export rule when it can do so safely

The recommended library shape is an explicit top-level export such as:

<!-- rumdl-disable MD040 -->

```cddl
library = type1 / type2 / type3 / type4 / type5
```

<!-- rumdl-enable MD040 -->

That keeps the library surface obvious and gives downstream include/import users a single public entry point.

### CBORK library/export directives

CBORK-specific metadata comments should make reusable CDDL modules explicit without changing CDDL semantics.
The first directive is:

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Library
```

<!-- rumdl-enable MD040 -->

This marks the file as intended for import/include reuse.
It does not replace the normal CDDL top-level rule and does not make the file valid for workflows
that require one concrete document root.
It enables library-shape lint rules.

The second directive is:

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Export
location-references = [ +location-reference ]
```

<!-- rumdl-enable MD040 -->

`Export` marks the next CDDL rule as externally usable by consumers of the imported/included file.
The directive binds to the next rule while skipping only blank lines, regular comments (`; ...`), and doc comments (`;! ...`).
It must not cross import/include directives or another `;@ CBORK:` directive.
If it reaches EOF without finding a rule, lint should emit a dangling export-directive diagnostic.

Example with documentation between the directive and rule:

<!-- rumdl-disable MD040 -->

```cddl
;! # References to a link address used by services
;!
; Common CDDL definitions used consistently across various CDDL definitions.
;@ CBORK: Library

;@ CBORK: Export
;! Public list of location references.
location-references = [ +location-reference ]
```

<!-- rumdl-enable MD040 -->

This is semantically valid because `Export` crosses only whitespace and safe comments.
However, lint should also provide a style rule that recommends placing `;@ CBORK: Export` immediately next to the rule it exports.
That style rule should be warn-only by default and separately configurable from semantic export enforcement.

Import/include enforcement:

* importing or including a file that is not marked `;@ CBORK: Library` should warn
* referencing an imported/included rule that is not marked `;@ CBORK: Export` should warn
* `--strict` should promote those warnings to failures
* references inside the library file to its own private helper rules are allowed
* multiple exported rules are allowed in library mode
* zero exported rules in a library should warn
* a non-library file should still be checked for a single concrete document root

This gives CDDL libraries a visible public API without requiring new CDDL grammar.
It also lets the linter distinguish intended public rules from private helper rules instead of guessing from naming or reachability.

## Concrete CDDL Renderer — `cbork render` (IMPLEMENTED)

The renderer lives in `crates/cbork-cddl-compiler/src/concrete.rs` (~1480 lines, `feat/within` branch).
It shows "what the linter actually sees" — the effective CDDL after the compiler has processed it.

### Architecture

```text
complete_nodes (post-prune tree)
       │
       ▼
build_resolution(nodes) → ResolutionMap {
  definitions:      HashMap<name, RuleLine>,
  socket_plugs:     HashMap<name, Vec<RuleLine>>,
  cache:            ResolverCache,
  referenced_names: HashSet<name>,
}
       │
       ▼
render_cddl(nodes, resolution, policy) → Concrete { lines: Vec<Line> }
       │
       ▼
Concrete::to_cddl() → String
```

### Substitution rules

* **Named constants** (`name = 42`): folded to literal in rule body; not emitted as separate lines unless library mode.
* **Socket/group plugs** (`$name //= ...`): skipped at top level;
  inlined as `/`-joined choice in the group context that references the plug.
* **Type augmentations** (`name /= type`): kept verbatim.
* **Structural references** (`name = { ... }`): inlined into referencing rules with cycle detection.
* **Strong definitions** (`name := ...`): never inlined (user override).
* **Library mode**: controlled by `ConcretePolicy.library_mode: bool`.
  Emits the first top-level rule plus every unreferenced rule.
  Referenced helpers are inlined and suppressed.
  Auto-detected from `;@ CBORK: Library`.

### Key types

* `ConcretePolicy { provenance_comments, library_mode, target }`
  — `target` is `Full` (render), `Lhs`/`Rhs` (future diff renderer).
* `RenderCx { resolution, policy }` — threaded through recursion to avoid `only_used_in_recursion` lint.
  All rendering methods are on this struct.
* `Concrete { lines: Vec<Line> }`
* `Line { kind: LineKind, text, indent, origin }`
* `LineKind { RuleLine, KeptDefinition, Comment, ModuleBoundary, Blank, GroupEntry, Provenance }`

### Renderer state (2026-06-12, `feat/within` branch)

**Clippy**: clean (all passes: format-check, clippy, license advisories, spell-check).
**Tests**: 214/214 pass (193 unit + 5 render integration + 16 import/include + 1 doctest + other test files in the compiler crate).
`just fix-ci` is green end-to-end.
The `pqsig_style_socket_plug_expands_to_choice` failure was resolved by routing the `.within` LHS and RHS through the AST
(via a new `extract_within_operands` helper) instead of dumping raw source text.
Each operand recurses through `render_pretty_rhs`,
which sends `{...}` and `[...]` bodies through `render_brace_block` -> `render_grpent`, so socket plugs at use sites are expanded.
Integration tests in `tests/render_vectors.rs` were updated to reflect the new effective-view shape
(multi-line, named types folded to their CBOR tag values, inlined structural references).

### Effective-viewer (complete)

A new pretty-printing subsystem in `concrete.rs` produces multi-line, indented effective CDDL with provenance comments
(`; from <name>`).

Key additions:

* `render_pretty_rhs()` — walks the RHS type tree and emits structured lines:
  `/` choices one per line, `{ }` / `[ ]` as multi-line blocks,
  `.within` split across lines.
* `render_brace_block()` — emits map/array entries one per line.
* `ConcretePolicy.library_mode: bool` replaces `keep_individual_defs`.
* `ResolutionMap.referenced_names` computed by `collect_referenced_names()`
  — determines which rules are transitively referenced, used by library-mode
  to suppress inlined helpers.
* `render_define()` folds short single-line bodies back onto the head line
  (e.g. `A = 1` stays on one line).
* `find_within_split()` — locates `.within` in source text outside brackets.
* `annotate_inline_refs()` — appends `; from <name>` to inlined references.

### Files involved

| File | Role |
|------|------|
| `crates/cbork-cddl-compiler/src/concrete.rs` | Renderer (~1480 lines) |
| `crates/cbork-cddl-compiler/src/error.rs` | `Subdiag`/`SubdiagKind` types added |
| `crates/cbork-cddl-compiler/src/resolver_cache.rs` | `peek()` method added |
| `crates/cbork-cddl-compiler/src/lib.rs` | Exports `ResolutionMap`, `Subdiag`, etc. |
| `crates/cbork/src/render.rs` | `cbork render` CLI handler |
| `crates/cbork/src/cli.rs` | `Render` struct (replaces stub) |
| `crates/cbork/src/main.rs` | `mod render` added |
| `crates/cbork-cddl-compiler/tests/render_vectors.rs` | Integration tests (6) |
| `cddl/vectors/project/positive/render_concrete_fold.cddl` | Test vector |
| `cddl/vectors/project/positive/render_library_preserves_constants.cddl` | Test vector |
| `cddl/vectors/project/positive/render_complex_structure.cddl` | Test vector |
| `cddl/vectors/project/positive/render_plug_inline.cddl` | Test vector (pqsig-style plugs) |
| `crates/cbork/session-summary.md` | Conversation history (appended, not committed) |
| `.config/dictionaries/project.dic` | Added subdiag, xhead, inlines, lhss, typenames, grpents entries |

### Build command

```sh
cd cbork && just fix-ci
```

## `.within` / `.and` Rewrite Plan

The concrete renderer is the shared foundation for lint diagnostics.
This rewrite makes `.within` and `.and` semantically precise for the covered lint cases and renders failures as an inline diff,
not as two unrelated concrete snippets.

Current status: COMPLETE for the planned lint behavior and end-to-end fixtures.
Unknown edge cases may still require follow-up fixes, but the core `.within` / `.and` lint path is usable.

The key design rule is:

* subtype checking owns semantic truth
* the concrete renderer owns readable CDDL text
* the diff builder joins those two outputs into an explanation

Do not put semantic decisions in the CLI renderer.
The CLI renderer should only format diagnostics that the compiler has already classified.

### Known Bugs

#### Bug 001: Generic `.within` validation can run after incomplete parameter substitution

Status: COMPLETE.

Observed failure:

```text
error[E030]: .within subtype check failed:
  reason: unresolved name: headers
  --> test/dntls-core/doc/dntls-cose-sign.cddl:57:18
```

The failing source shape is:

<!-- rumdl-disable MD040 -->

```cddl
Null-COSE-Sign = COSE_Sign<Null-Headers, nil, Null-COSE-Signature>

COSE_Sign<headers, dntls-payload, dntls-signatures> = [
  headers,
  payload: dntls-payload,
  signatures: [+ dntls-signatures]
] .within cose.COSE_Sign
```

<!-- rumdl-enable MD040 -->

Expected behavior:

* Before validating `.within cose.COSE_Sign`, the instantiated LHS must substitute every formal parameter.
* The effective LHS should contain `Null-Headers`, `nil`, and `Null-COSE-Signature`.
* No formal generic parameter such as `headers` or `dntls-signatures` should remain in the schema given to `.within`.

Actual behavior:

* The diagnostic renders an incomplete effective LHS:

  <!-- rumdl-disable MD040 -->

  ```cddl
  [
    headers,
    payload: nil,
    signatures: [+ dntls-signatures]
  ]
  ```

  <!-- rumdl-enable MD040 -->
* `dntls-payload` is substituted, but the bare array entry `headers` and occurrence entry `+ dntls-signatures` remain symbolic.
* The subtype checker then tries to resolve `headers` as a real rule name and emits a false-positive unresolved-name `E030`.

Likely cause:

* `generic.rs` replaced child nodes during generic expansion but left ancestor `text` fields stale.
  Downstream render and `.within` paths still consult those ancestor text fields for nested groups and occurrences.
* Generic-template detection also treated any nested `genericparm` descendant as making the current rule an open generic.
  If an import/include subtree containing a generic definition was attached under a concrete alias rule,
  that concrete alias could be skipped instead of expanded.
* The `.within` check was correctly running at the instantiation site, but it could receive an incompletely expanded
  generic body or resolve through an unexpanded generic alias.

Required fix:

* Make generic substitution replace formal parameters in all valid schema positions before recursive expansion
  and `.within` validation.
* Add a stable regression fixture outside `test/` for this exact shape.
* The fixture should prove that `COSE_Sign<Null-Headers, nil, Null-COSE-Signature>`-style instantiation validates
  after substitution.
* The fixture should also assert the effective rendered LHS contains no remaining generic formal names.

Completion notes:

* `generic.rs` now substitutes while the generic template still has its original spans, then re-origins the expanded tree
  to the call site.
* Ancestor syntax text is updated from the substituted child spans, so member-key labels are not blindly rewritten.
* Generic expansion now updates ancestor text after any generic-instantiation replacement, not only after parameter
  substitution inside a cloned generic body.
* Generic-template skipping now checks only the rule's own LHS `genericparm`, not arbitrary nested descendants.
  Nested concrete rules attached under a generic rule are still walked.
* `.within` validation uses the same narrowed own-LHS generic check and still walks nested concrete rules under generic
  wrappers.
* Added `cddl/vectors/project/positive/valid_generic_within_substitutes_occurrence_params.cddl`.
  The fixture intentionally includes an unrelated generic helper after a concrete alias
  so it covers the attached-subtree misclassification case.
* Added a lint regression and render regression proving the instantiated schema validates and renders without leaked
  formal parameters such as `headers`, `dntls-payload`, or `dntls-signatures`.

### Target diagnostic shape

```text
error[E030]: .within subtype check failed
  --> file.cddl:18:1
   |
18 | map = { ... } .within template
   |       -- OK       ed25519 => bstr
   |       -- OK       1 => int / tstr          (optional)
   |       -- CONFLICT  2.5 => 'test'           (no matching RHS key)
   |       -- OPTIONAL  5 => bstr // 6 => bstr  (not present in LHS)
   |       -- OK       * label => values
```

### Step 1: Preserve ctlop schemas in `ResolvedType` — COMPLETE

Reasoning:

`ResolvedType` currently cannot represent a schema such as `bstr .cbor payload`.
`resolve_type1()` sees a ctlop and returns only the first `type2`.
That makes `.cbor`, `.dtrm`, `.size`, `.bits`, `.and`, and other schema-level ctlops invisible to `.within`.
This is wrong for containment because some ctlops are narrower than others.

Concrete work:

* In `crates/cbork-cddl-compiler/src/within.rs`, add a ctlop-aware variant:

  ```rust
  Control {
      op: ControlOp,
      carrier: Box<ResolvedType>,
      controller: Box<ResolvedType>,
  }
  ```

* Add a private enum in `within.rs`:

  ```rust
  enum ControlOp {
      Cbor,
      Dtrm,
      CborSeq,
      DtrmSeq,
      And,
      Within,
      Size,
      Bits,
      Other(String),
  }
  ```

* Update `resolve_type1()`:
    * keep range handling as-is
    * when a `type1` has exactly two `type2` operands and a `ctlop`, return `ResolvedType::Control`
    * normalize known operator text into `ControlOp`
    * only collapse to the LHS for ctlops that are proven irrelevant to subtype checking
    * do not collapse `.cbor`, `.dtrm`, `.cborseq`, `.dtrmseq`, `.and`, `.within`, `.size`, or `.bits`

* Update `render_type()` and `type_name()` to display `Control`.
  This is fallback text only; diagnostics should still prefer concrete snippets.

* Update `resolve_named_deep()` to recurse through `Control.carrier` and `Control.controller`.

Tests:

* Add unit tests in `within.rs`:
    * `resolve_type1("x = bstr .cbor payload")` produces `ControlOp::Cbor`
    * `resolve_type1("x = bstr .dtrm payload")` produces `ControlOp::Dtrm`
    * the controller resolves to the `payload` schema, not to an opaque string

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::resolve_ctlop_schema
cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
```

Completion status:

* Implemented in `crates/cbork-cddl-compiler/src/within.rs`.
* `ResolvedType::Control { op, carrier, controller }` exists and is used by `resolve_type1()` for two-operand ctlops.
* `ControlOp` exists with `.cbor`, `.dtrm`, `.cborseq`, `.dtrmseq`, `.and`, `.within`, `.size`, `.bits`, and `Other(String)`.
* `render_type()`, `type_name()`, and `resolve_named_deep()` handle `ResolvedType::Control`.
* Validation run:

  ```sh
  cargo test -p cbork-cddl-compiler within::tests -- --nocapture
  cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
  ```

* Result: both commands passed.

Stage 1 follow-up coverage to add during Step 2:

* Add explicit `within.rs` unit tests proving that these required operators do not collapse:
    * `x = int .and uint` resolves to `ResolvedType::Control { op: ControlOp::And, ... }`.
    * `x = int .within uint` resolves to `ResolvedType::Control { op: ControlOp::Within, ... }`.
    * `x = bstr .bits flags` resolves to `ResolvedType::Control { op: ControlOp::Bits, ... }`.
* Add a nested ctlop preservation test using a realistic map/socket shape.
  The test must keep a nested value such as `ed25519_sig = bstr .size 64` or an inline equivalent and assert
  that the nested value resolves as `ResolvedType::Control { op: ControlOp::Size, carrier: bstr, controller: 64 }`.
  Do not weaken this by replacing `.size` with bare `bstr`.
* These tests are coverage hardening for the completed Step 1 model.
  They are not a new semantic subtype implementation; subtype behavior for `Control` remains Step 2.

### Step 2: Implement directional ctlop containment — COMPLETE

Reasoning:

Some ctlops form a refinement hierarchy.
For serialization operators, deterministic CBOR is a subset of general CBOR.
Therefore `.dtrm` is within `.cbor`, but `.cbor` is not within `.dtrm`.
This must be represented in `is_subtype_impl()`, because `.within` is a subtype check.

Required semantics:

* `bstr .dtrm T ⊆ bstr .cbor T`
* `bstr .cbor T ⊄ bstr .dtrm T`
* `bstr .dtrm A ⊆ bstr .dtrm B` iff `A ⊆ B` and carrier is compatible
* `bstr .cbor A ⊆ bstr .cbor B` iff `A ⊆ B` and carrier is compatible
* `bstr .dtrmseq A ⊆ bstr .cborseq A`
* `bstr .cborseq A ⊄ bstr .dtrmseq A`
* unknown `ControlOp::Other` is not assumed compatible unless both sides are the same operator and both operands subtype

Concrete work:

* Add `is_control_subtype(lhs, rhs, defs, visited) -> Result<(), String>` in `within.rs`.
* Call it from `is_subtype_impl()` before the final different-structure fallback.
* It must first check carrier compatibility with `is_subtype_impl(lhs.carrier, rhs.carrier, ...)`.
* It must then check operator compatibility:
    * equal operators: controller must subtype
    * `Dtrm -> Cbor`: controller must subtype
    * `DtrmSeq -> CborSeq`: controller must subtype
    * reverse directions must fail with a specific reason string
* Reason strings should be explicit:
    * `.cbor is broader than .dtrm`
    * `.cborseq is broader than .dtrmseq`
    * `control operator .foo is not within .bar`

Tests:

* Add positive tests:
    * `dtrm_within_cbor`
    * `dtrm_payload_within_broader_cbor_payload`
    * `dtrmseq_within_cborseq`
* Add negative tests:
    * `cbor_not_within_dtrm`
    * `cborseq_not_within_dtrmseq`
    * `dtrm_payload_not_within_narrower_dtrm_payload`

Suggested CDDL snippets:

```text
payload = { 1 => int }
payload-wide = { 1 => int, ? 2 => tstr }
ok = bstr .dtrm payload .within bstr .cbor payload-wide
bad = bstr .cbor payload .within bstr .dtrm payload
```

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::dtrm
cargo test -p cbork-cddl-compiler within::tests::cbor
cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
```

Completion status:

* Implemented in `crates/cbork-cddl-compiler/src/within.rs`.
* `is_subtype_impl()` dispatches `ResolvedType::Control` vs `ResolvedType::Control` to `is_control_subtype()`.
* `is_control_subtype()` first checks carrier compatibility, then applies the ctlop compatibility matrix.
* Implemented directional containment:
    * `.dtrm` is within `.cbor`
    * `.cbor` is not within `.dtrm`
    * `.dtrmseq` is within `.cborseq`
    * `.cborseq` is not within `.dtrmseq`
    * equal operators require controller subtype
    * unknown `ControlOp::Other` values only match when the operator text is identical and the controllers subtype
* Reverse-direction errors use the required explicit reason strings:
    * `.cbor is broader than .dtrm`
    * `.cborseq is broader than .dtrmseq`
    * `control operator .foo is not within .bar`
* Validation run:

  ```sh
  cargo test -p cbork-cddl-compiler dtrm -- --nocapture
  cargo test -p cbork-cddl-compiler cbor -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests -- --nocapture
  cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
  ```

* Result: all commands passed; `within::tests` passed with 72 tests.
* Additional CLI smoke validation:
    * `ok = (bstr .dtrm payload) .within (bstr .cbor payload-wide)` did not emit `E030`.
    * `bad = (bstr .cbor payload) .within (bstr .dtrm payload)` emitted `E030` with reason `.cbor is broader than .dtrm`.

### Step 3: Replace string-only subtype failures with structured conflicts

Reasoning:

`is_subtype()` currently returns `Result<(), String>`.
That is enough to say a check failed, but not enough to build a useful inline diff.
The checker must return where and why it failed in a structured form.

Concrete work:

* Add a conflict type in `within.rs` or a new `crates/cbork-cddl-compiler/src/schema_diff.rs`:

  ```rust
  struct WithinConflict {
      path: Vec<PathSegment>,
      kind: WithinConflictKind,
      lhs: Option<ResolvedType>,
      rhs: Option<ResolvedType>,
      reason: String,
  }
  ```

* Suggested conflict kinds:
    * `MissingRequiredRhs`
    * `LhsNotAccepted`
    * `TooManyMatches`
    * `PrimitiveMismatch`
    * `RangeMismatch`
    * `ControlMismatch`
    * `DifferentStructure`
    * `UnresolvedName`

* Keep `is_subtype()` as a convenience wrapper if useful.
  Internally add:

  ```rust
  fn subtype_conflicts(lhs, rhs, defs) -> Vec<WithinConflict>
  ```

* Convert each current `Err(String)` branch into a conflict with enough context to map back to a line:
    * array index
    * map entry index
    * choice arm index
    * control operator

* `check_within_constraint()` should use the structured conflicts.
  It should still emit `E030`.
  The message should summarize the first conflict and the subdiags should carry detailed diff lines.

Tests:

* Existing tests that only assert `Err(...)` should either continue through `is_subtype()` or be migrated to assert conflict kind.
* Add targeted conflict tests:
    * bad primitive produces `PrimitiveMismatch`
    * missing map key produces `LhsNotAccepted` or `MissingRequiredRhs`
    * `.cbor` within `.dtrm` produces `ControlMismatch`

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::subtype
cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
```

Current status: COMPLETE.

Implemented:

* `PathSegment`, `WithinConflictKind`, and `WithinConflict` exist in `crates/cbork-cddl-compiler/src/within.rs`.
* `subtype_conflicts(lhs, rhs, defs) -> Vec<WithinConflict>` exists.
* The structured collector covers:
    * primitives via `PrimitiveMismatch`
    * ranges via `RangeMismatch`
    * arrays via `ArrayIndex`
    * maps via `MissingRequiredRhs`, `TooManyMatches`, and `LhsNotAccepted`
    * choices via `ChoiceArm`
    * named/socket resolution via `UnresolvedName`
    * controls via `ControlMismatch` and `ControlOp(...)` path segments
    * structurally different shapes via `DifferentStructure`
* `is_subtype_impl()` now derives its legacy `Result<(), String>` from the first structured conflict.
  This keeps existing callers/tests working while making structured data available.
* Some existing tests were migrated to assert structured map conflicts.
* `check_within_constraint()` now calls `subtype_conflicts()` directly instead of routing through `is_subtype()`.
* `check_within_constraint()` emits LHS/RHS related subdiagnostics plus one conflict-derived subdiagnostic per `WithinConflict`.
* Conflict-derived subdiagnostics include a path summary such as `map[1]` or `.cbor` when a path is available.
* Added targeted conflict-kind tests for:
    * bad primitive -> `WithinConflictKind::PrimitiveMismatch`
    * missing map key -> `WithinConflictKind::MissingRequiredRhs`
    * `.cbor` within `.dtrm` -> `WithinConflictKind::ControlMismatch`
* Validation run:

  ```sh
  cargo test -p cbork-cddl-compiler within::tests::subtype -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests -- --nocapture
  cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
  ```

* Result: all commands passed; `within::tests` passed with 75 tests.

Completion status:

* The parsed `validate_within_pass()` diagnostic test now asserts actual conflict-derived related output.
  It verifies that at least one related subdiagnostic:
    * has kind `SubdiagKind::Unmatched` or `SubdiagKind::Note`
    * contains a path or control-operator summary such as `map[`, `.cbor`, `.dtrm`, or `.and`
* The legacy `check_map_subtype_with_sockets()` helper was removed.
* Step 3 is complete.
* The RFC standard lint blocker noted during Step 3 was resolved by later Step 5 and Step 7.5 fixes.

### Step 4: Implement real `.and` semantics

Reasoning:

`.and` is schema intersection.
Accepting the ctlop syntax is not enough.
The linter must validate impossible or failing intersections and explain failures using the same diff machinery as `.within`.

Concrete work:

* Add `ResolvedType::Intersection(Vec<ResolvedType>)` or represent `.and` as `ControlOp::And`.
* Preferred implementation: use a dedicated `Intersection` variant after resolving `A .and B`.
  It is easier to subtype than a generic control operator.
* Update `resolve_type1()`:
    * `A .and B` becomes `Intersection(vec![A, B])`
* Add subtype rules:
    * `L ⊆ (A .and B)` requires `L ⊆ A` and `L ⊆ B`
    * `(A .and B) ⊆ R` is valid if the intersection is known to be within `R`
    * conservative first implementation may require both `A ⊆ R` and `B ⊆ R`
    * if either side is unknown, avoid false positives
    * preserve an explanatory `Unknown` conflict only when the whole check cannot be decided
* Add a validation pass for `.and` only if subtype rules alone are not enough.
  Do not duplicate traversal unless needed.

Tests:

* Use the existing positive vector `cddl/vectors/project/positive/valid_rfc8610_ctrl_within_and.cddl`.
* Add focused tests:
    * `non-empty<M> = (M) .and ({ + any => any })`
    * empty map should not satisfy `non-empty`
    * non-empty concrete map should satisfy `non-empty`
    * an impossible intersection should emit a diagnostic

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::and
cargo run -p cbork -- lint cddl/vectors/project/positive/valid_rfc8610_ctrl_within_and.cddl --strict
```

Current status: COMPLETE.

Implemented:

* Added `ResolvedType::Intersection(Vec<ResolvedType>)`.
* `resolve_type1()` resolves `A .and B` to `ResolvedType::Intersection(vec![A, B])`.
* `render_type()`, `type_name()`, and `resolve_named_deep()` handle `Intersection`.
* Structured subtype collection handles intersections:
    * `L ⊆ (A .and B)` checks `L ⊆ A` and `L ⊆ B`
    * `(A .and B) ⊆ R` uses the conservative first implementation requiring every operand to be within `R`
* Array subtype matching handles trailing repeated RHS entries such as `* any`.
  This is required by the RFC8610 `.within`/`.and` positive vector,
  where `[message_type, *message_option]` must accept concrete message arrays with more than two elements.
* Added focused tests:
    * empty map does not satisfy a non-empty intersection
    * non-empty map satisfies a non-empty intersection
    * impossible primitive intersection fails
    * conservative `Intersection([int, uint]) ⊆ int` passes
    * `[3, text, [* text]] ⊆ [0..255, * any]`
    * `[4, text, text, bool] ⊆ [0..255, * any]`
    * shorter arrays still satisfy trailing `*` when the required fixed prefix matches
    * arrays fail when the required fixed prefix does not match
* Validation run:

  ```sh
  cargo test -p cbork-cddl-compiler within::tests::and -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests::array -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests -- --nocapture
  cargo run -p cbork -- lint cddl/vectors/project/positive/valid_rfc8610_ctrl_within_and.cddl --strict
  cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
  ```

* Result: all commands passed; `within::tests` passed with 83 tests.
* Step 4 is complete.

### Step 5: Build inline diff data from concrete LHS/RHS

Reasoning:

The subtype checker should explain what failed.
The concrete renderer should explain what the schema actually looks like.
The diff builder must combine them into a stable user-facing explanation.

Concrete work:

* Add a new module:

  ```text
  crates/cbork-cddl-compiler/src/schema_diff.rs
  ```

* Inputs:
    * `lhs_node: &WrappedNode`
    * `rhs_node: &WrappedNode`
    * `conflicts: &[WithinConflict]`
    * `resolution: &ResolutionMap`
* Render inputs:
    * `concrete::render_subtree(lhs_node, resolution, &ConcretePolicy::for_lhs())`
    * `concrete::render_subtree(rhs_node, resolution, &ConcretePolicy::for_rhs())`
* Output:

  ```rust
  struct SchemaDiffLine {
      kind: SchemaDiffKind,
      text: String,
      reason: Option<String>,
  }
  ```

* Suggested line kinds:
    * `Context`
    * `Matched`
    * `LhsRejected`
    * `RhsRequiredMissing`
    * `RhsOptional`
    * `Note`
* Use path-aware conflict-to-line association as the authoritative source:
    * rendered structural lines must carry or be derivable from a schema path
    * `WithinConflict.path` entries such as `MapEntry(i)`, `ArrayIndex(i)`,
      and `ControlOp(...)` must map to specific concrete rendered lines
    * `LhsRejected` and `RhsRequiredMissing` must only be attached to lines whose path matches a conflict path
    * if no rendered line can be matched to a conflict path, append a `Note` rather than guessing
* Text alignment may still be used for context only:
    * normalize whitespace but preserve displayed text
    * exact line match may produce `Matched`
    * LCS must not be the authority for conflict placement
* Do not overfit the first version to perfect tree edit distance.
  It is acceptable for v1 to be conservative as long as it does not lie or attach a conflict to the wrong line.

Tests:

* Add pure diff tests:
    * identical maps produce matched/context lines only
    * extra LHS key produces `LhsRejected`
    * missing required RHS key produces `RhsRequiredMissing`
    * optional RHS key produces `RhsOptional`
* Add pqsig-style nested fixture test to ensure concrete rendering stays indented inside diff output.

Validation:

```sh
cargo test -p cbork-cddl-compiler schema_diff
cargo test -p cbork-cddl-compiler within::tests::within_diagnostic
```

Current status: COMPLETE.

Implemented:

* `crates/cbork-cddl-compiler/src/schema_diff.rs` exists.
* `SchemaDiffKind`, `SchemaDiffLine`, and `build_schema_diff()` exist as `pub(crate)`.
* The module is crate-private and is not re-exported from `crates/cbork-cddl-compiler/src/lib.rs`.
* Pure schema diff tests exist for:
    * identical maps producing only matched/context lines
    * extra LHS key producing `LhsRejected`
    * missing required RHS key producing `RhsRequiredMissing`
    * optional RHS key producing `RhsOptional`
    * pqsig-style nested fixture preserving indentation
    * pathless conflicts become `Note`
    * conflicts with unmapped paths become `Note`
* `build_schema_diff()` is now path-authoritative for conflict placement:
    * it builds path-to-line maps from the rendered LHS/RHS ASTs
    * `WithinConflict.path` decides which line receives `LhsRejected` or `RhsRequiredMissing`
    * LCS is retained only for `Matched` / context alignment
    * unmapped conflicts become `Note` instead of being guessed onto arbitrary lines
* The lib-level `schema_diff` dead-code allowance was removed.
* The control-refinement subtype regression discovered during Step 5 was fixed:
    * `ControlOp` now includes `.gt`, `.ge`, `.lt`, and `.le`
    * known narrowing operators can subtype their carrier
    * `uint .gt 1 ⊆ uint` passes
    * `uint .bits [...] ⊆ uint` passes
    * `bstr .size 2 ⊆ bstr` passes
    * `bstr .cbor T ⊆ bstr` passes
    * `bstr .dtrm T ⊆ bstr` passes
    * `bstr .cbor T ⊄ bstr .dtrm T` still fails as required
* The choice-containment regression discovered during Step 5 was fixed:
    * `bstr ⊆ bstr / #6.24(bstr)` passes
    * `#6.24(bstr) ⊆ bstr / #6.24(bstr)` passes
    * `bstr / #6.24(bstr) ⊆ bstr / #6.24(bstr)` passes
    * `bstr / #6.24(bstr) ⊄ #6.24(bstr)` still fails as required
    * RFC9171 payload-block shape and socket-reference regression tests pass
* Validation run:

  ```sh
  cargo test -p cbork-cddl-compiler schema_diff -- --nocapture
  cargo test -p cbork-cddl-compiler choice_containment -- --nocapture
  cargo test -p cbork-cddl-compiler rfc9171 -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests::within_diagnostic -- --nocapture
  cargo test -p cbork-cddl-compiler within::tests -- --nocapture
  cargo run -p cbork -- lint cddl/rfc-std/rfc9171.cddl --strict
  cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
  ```

* Result: all commands passed.
  `schema_diff` passed with 10 tests and `within::tests` passed with 97 tests.
* Step 5 is complete.

CI note:

* The `root:cbork-rfc-std-lint` moon target runs `target/release/cbork lint --strict cddl/rfc-std`.
  If `target/release/cbork` is stale, it can still show the old RFC9171 errors:
    * `control(.gt) not subtype of Uint`
    * `block-type-specific-data not subtype of any choice arm`
* Rebuilding release with `cargo build --release -p cbork` makes `target/release/cbork lint --strict cddl/rfc-std/rfc9171.cddl`
  pass.
* The later RFC9581 false positive was tracked separately as Step 7.5 and is now resolved.
  It was not an RFC9171 Step 5 regression.

### Step 6: Carry diff lines through `Diagnostic.related`

Current status: COMPLETE for `.within` diagnostic transport.

Reasoning:

The current diagnostic model already has `Subdiag` and `SubdiagKind`.
Use that rather than inventing another CLI-only channel.

Concrete work:

* Extend `SubdiagKind` in `crates/cbork-cddl-compiler/src/error.rs` only if the current variants are insufficient.
* Preferred mapping without adding variants:
    * `SchemaDiffKind::Matched` -> `SubdiagKind::Matched`
    * `SchemaDiffKind::LhsRejected` -> `SubdiagKind::Unmatched`
    * `SchemaDiffKind::RhsRequiredMissing` -> `SubdiagKind::Unmatched`
    * `SchemaDiffKind::RhsOptional` -> `SubdiagKind::Optional`
    * `SchemaDiffKind::Note` -> `SubdiagKind::Note`
* If side information is ambiguous, add a narrowly named variant such as `SubdiagKind::Diff`.
  Do not add broad or display-specific variants.
* Update `check_within_constraint()`:
    * build conflicts
    * build schema diff lines
    * attach ordered `Subdiag`s
    * keep raw LHS/RHS subdiags only as fallback when diff construction fails

Tests:

* Update `within_diagnostic_contains_lhs_and_rhs_subdiags` or replace it with:
    * `within_diagnostic_contains_inline_diff_subdiags`
    * asserts at least one `Matched`
    * asserts at least one `Unmatched` for the failing line
    * asserts snippets are concrete and non-empty

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::within_diagnostic
cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings
```

Completion notes:

* `check_within_constraint()` now calls `schema_diff::build_schema_diff()` with the structured conflicts.
* Non-empty diff output is converted into ordered `Diagnostic.related` subdiags.
* `SchemaDiffKind` maps to existing `SubdiagKind` variants:
    * `Matched` -> `Matched`
    * `LhsRejected` / `RhsRequiredMissing` -> `Unmatched`
    * `RhsOptional` -> `Optional`
    * `Context` / `Note` -> `Note`
* The legacy raw `LHS` / `RHS` related snippets are retained only as fallback when diff construction returns no lines.
* `within_diagnostic_contains_inline_diff_subdiags` verifies
  that `.within` diagnostics carry concrete non-empty diff subdiags with an `Unmatched` line.
* `within_diagnostic_matched_when_identical` verifies that identical maps do not emit an `E030` diagnostic.
* Validation passed:
    * `cargo test -p cbork-cddl-compiler within::tests::within_diagnostic -- --nocapture`
    * `cargo test -p cbork-cddl-compiler schema_diff -- --nocapture`
    * `cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings`
* Step 6 is complete.

### Step 7: Render inline diff in the CLI

Current status: COMPLETE.

Reasoning:

The compiler should classify diff lines.
The CLI should only format them.
Existing `write_related()` prints separate blocks.
For `.within`/`.and` diagnostics, that should become one inline diff block.

Concrete work:

* In `crates/cbork/src/diagnostics.rs`, update `write_related()`.
* For diagnostics containing ordered diff subdiags, render:

  ```text
     = DIFF:
         -- OK        <line>
         -- CONFLICT  <line>  ; reason
         -- OPTIONAL  <line>
         -- MISSING   <line>  ; reason
  ```

* Keep existing `= LHS:` / `= RHS:` rendering for non-diff diagnostics.
* Add tests under the existing diagnostics tests:
    * diff subdiags render in order
    * conflict reason is printed
    * fallback LHS/RHS block rendering still works

Validation:

```sh
cargo test -p cbork diagnostics::tests::subdiags
cargo run -p cbork -- lint <focused failing within fixture> --why --strict
```

Completion notes:

* Implemented in `crates/cbork/src/diagnostics.rs`.
* Ordered `Matched`, `Unmatched`, and `Optional` subdiagnostics render under one `= DIFF:` block.
* `Unmatched` lines render as `-- CONFLICT`.
* Conflict reasons encoded in the snippet remain visible on the conflict line.
* Legacy `SubdiagKind::Lhs` / `SubdiagKind::Rhs` output still renders as separate `= LHS:` / `= RHS:` blocks.
* Added diagnostics tests:
    * `diff_subdiags_render_in_order`
    * `diff_conflict_includes_reason`
    * `legacy_lhs_rhs_not_detected_as_diff`
* Step 7 is covered by the Step 8 fixture-backed CLI lint tests and the `just fix-ci` validation recorded below.

### Step 7.5: Fix RFC9581 map-entry false positive before end-to-end fixtures

Current status: COMPLETE.

Reasoning:

The rendered effective schema for `cddl/rfc-std/rfc9581.cddl` shows a false positive in the `.within` checker.
`etime-detailed` is checked against `etime-framework`.
The RHS framework requires one base-time entry:

<!-- rumdl-disable MD040 -->

```cddl
{
  uint => any,
  * nint / text => any,
  * uint => any
}
```

<!-- rumdl-enable MD040 -->

The effective LHS supplies required base-time entries through `$$ETIME-BASETIME`:

<!-- rumdl-disable MD040 -->

```cddl
(
  (1: #6.1(int / float)) /
  (4: #6.4([e10: int, m: integer])) /
  (5: #6.5([e2: int, m: integer]))
)
```

<!-- rumdl-enable MD040 -->

Each key is a concrete positive integer and therefore within `uint`.
Each value is within `any`.
Therefore this `.within` should pass.
The previous diagnostic:

```text
map[0]: LHS required entry has no matching RHS entry
```

is a semantic checker bug, not a rendering bug.

Concrete work:

* In `crates/cbork-cddl-compiler/src/within.rs`,
  fix `collect_map_conflicts()` / `map_entry_matches()`
  so required LHS group-choice entries are accepted by broader RHS map entries such as `uint => any`.
* Verify how `$$ETIME-BASETIME //= (1: ~time) / ...` is represented after `expand_map_sockets()`:
    * If it remains one `Choice`-shaped map entry, distribute the choice arms before map-entry matching.
    * If it expands to individual `MapEntry` values, verify each concrete key is compared against the RHS key schema.
* Normalize `:` and `=>` map-entry forms for subtype matching.
  A concrete key entry like `1: T` must match a schema entry `uint => any` when `1 ⊆ uint` and `T ⊆ any`.
* Add a focused unit test in `within.rs` that models the RFC9581 shape without requiring the full RFC file:

<!-- rumdl-disable MD040 -->

  ```cddl
  framework = {
    uint => any,
    * nint / text => any,
    * uint => any
  }

  detailed = ({
    $$BASE
    * $$ELECTIVE
    * $$CRITICAL
  }) .within framework

  $$BASE //= (1: #6.1(int / float))
  $$BASE //= (4: #6.4([e10: int, m: integer]))
  $$BASE //= (5: #6.5([e2: int, m: integer]))
  $$ELECTIVE //= (-3: uint)
  $$CRITICAL //= (13 => 0 / 1)
  ```

<!-- rumdl-enable MD040 -->

* Add a regression assertion that `validate_within_pass()` emits no `E030` for the focused fixture.
* Add a second assertion at the lower level, if practical, that `map_entry_matches()` accepts:
    * LHS key `1` within RHS key `uint`
    * LHS value `#6.1(int / float)` within RHS value `any`
* Confirm the full standard corpus reaches past RFC9581 after the fix.

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests::rfc9581
cargo test -p cbork-cddl-compiler within::tests -- --nocapture
cargo run -p cbork -- lint cddl/rfc-std/rfc9581.cddl --strict
moon run root:cbork-rfc-std-lint
```

Completion notes:

* Implemented in `crates/cbork-cddl-compiler/src/within.rs`.
* `resolve_type()` now handles bare `value` nodes such as the `1` in `1: T` as concrete ranges instead of unresolved names.
* Group resolution now carries delimiter context through `resolve_group_with_delimiter()` / `resolve_grpchoice_with_delimiter()`:
    * `{ ... }`, `&( ... )`, and parenthesized group-entry contexts can recognize `:` as a map-entry separator.
    * array context does not treat `:` as a map-entry separator.
* Parenthesized `key: value` group entries such as `(1: T)` resolve to single-entry maps,
  which lets socket plugs contribute real `MapEntry` values.
* `collect_map_conflicts()` no longer emits `TooManyMatches` for multiple LHS concrete entries accepted by one RHS schema entry.
  This avoids the RFC9581 case where `1`, `4`, and `5` are all valid `uint` keys for the same required RHS entry.
* Added `within::tests::rfc9581_group_socket_lhs_passes`.
* Validation passed:
    * `cargo test -p cbork-cddl-compiler within::tests::rfc9581 -- --nocapture`
    * `cargo test -p cbork-cddl-compiler within::tests -- --nocapture` (`99 passed`)
    * `cargo run -p cbork -- lint cddl/rfc-std/rfc9581.cddl --strict`
    * `cargo clippy -p cbork-cddl-compiler --all-targets -- -D warnings`
    * `moon run root:cbork-rfc-std-lint`

### Step 8: Add end-to-end fixtures

Current status: COMPLETE.

Reasoning:

This feature is easy to regress.
It crosses parsing, semantic resolution, concrete rendering, diagnostics, and CLI formatting.
End-to-end fixtures are required.

Concrete work:

* Add compiler tests for semantic correctness.
* Add CLI tests for rendered diagnostics if the current test harness supports command-level output.
* Add the minimum semantic-error fixtures under `cddl/vectors/project/semantic-errors/`:
    * `invalid_within_cbor_dtrm_direction.cddl`
      — proves the asymmetric ctlop rule:
      `.cbor` is broader than `.dtrm` and therefore not within `.dtrm`.
    * `invalid_within_missing_map_key.cddl`
      — proves a required LHS map entry rejected by the RHS is still reported after the RFC9581 map-entry fix.
    * `invalid_and_empty_map.cddl`
      — proves `.and` intersection failure is reported when a concrete schema cannot satisfy a required non-empty map shape.
    * `invalid_within_required_rhs_missing.cddl`
      — proves a required RHS entry that the LHS does not provide emits `RhsRequiredMissing` / `-- CONFLICT`.
    * `invalid_within_choice_arm_rejected.cddl` —
      proves a bad LHS choice arm is attributed to the rejected arm rather than collapsing the whole choice into an opaque failure.
* Add positive fixtures under `cddl/vectors/project/positive/`:
    * `valid_within_dtrm_cbor.cddl`
      — proves `.dtrm` is accepted as within the broader `.cbor`.
    * `valid_and_non_empty_map.cddl`
      — proves a concrete non-empty map satisfies the `.and` non-empty-map constraint.
    * `valid_within_rfc9581_group_socket_map.cddl`
      — proves the RFC9581 shape remains accepted end-to-end:
      required LHS group-socket entries using `key: value` match RHS `uint => any`.
    * `valid_within_optional_rhs_map_key.cddl` —
      proves optional RHS map entries do not force a failure and are rendered as optional/context
      when diagnostics are produced elsewhere.

Minimum fixture contents:

#### `invalid_within_cbor_dtrm_direction.cddl`

```text
payload = { 1 => int }
payload-wide = { 1 => int, ? 2 => tstr }

bad = bstr .cbor payload .within bstr .dtrm payload
```

Expected result:

* `bad` fails with `E030`.
* The reason mentions `.cbor is broader than .dtrm`.
* The diagnostic includes a concrete inline diff.

#### `valid_within_dtrm_cbor.cddl`

```text
payload = { 1 => int }
payload-wide = { 1 => int, ? 2 => tstr }

ok = bstr .dtrm payload .within bstr .cbor payload-wide
```

Expected result:

* `ok` passes under `--strict`.

#### `invalid_within_missing_map_key.cddl`

```text
lhs = { 1 => int, 2 => tstr }
rhs = { 1 => int }

bad = lhs .within rhs
```

Expected result:

* `bad` fails with `E030`.
* The diff marks `2 => tstr` as a conflict / LHS rejected line.

#### `invalid_within_required_rhs_missing.cddl`

```text
lhs = { 1 => int }
rhs = { 1 => int, 2 => tstr }

bad = lhs .within rhs
```

Expected result:

* `bad` fails with `E030`.
* The diff marks `2 => tstr` as required by RHS and missing from LHS.

#### `valid_within_optional_rhs_map_key.cddl`

```text
lhs = { 1 => int }
rhs = { 1 => int, ? 2 => tstr }

ok = lhs .within rhs
```

Expected result:

* `ok` passes under `--strict`.

#### `invalid_and_empty_map.cddl`

```text
non-empty<M> = (M) .and ({ + any => any })

empty = {}
bad = empty .within non-empty<empty>
```

Expected result:

* `bad` fails with `E030`.
* The reason points at the non-empty map requirement.

#### `valid_and_non_empty_map.cddl`

```text
non-empty<M> = (M) .and ({ + any => any })

payload = { 1 => int }
ok = payload .within non-empty<payload>
```

Expected result:

* `ok` passes under `--strict`.

#### `invalid_within_choice_arm_rejected.cddl`

```text
lhs = { 1 => int } / { 1 => tstr }
rhs = { 1 => int }

bad = lhs .within rhs
```

Expected result:

* `bad` fails with `E030`.
* The diff identifies the rejected `{ 1 => tstr }` arm, not only the whole `lhs` choice.

#### `valid_within_rfc9581_group_socket_map.cddl`

```text
framework = {
  uint => any,
  * nint / text => any,
  * uint => any
}

detailed = ({
  $$BASE
  * $$ELECTIVE
  * $$CRITICAL
}) .within framework

$$BASE //= (1: #6.1(int / float))
$$BASE //= (4: #6.4([e10: int, m: integer]))
$$BASE //= (5: #6.5([e2: int, m: integer]))
$$ELECTIVE //= (-3: uint)
$$CRITICAL //= (13 => 0 / 1)
```

Expected result:

* `detailed` passes under `--strict`.
* This fixture prevents regression of Step 7.5.

Validation:

```sh
cargo test -p cbork-cddl-compiler
cargo test -p cbork
cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/invalid_within_cbor_dtrm_direction.cddl --why --strict
cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/invalid_within_missing_map_key.cddl --why --strict
cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/invalid_within_required_rhs_missing.cddl --why --strict
cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/invalid_and_empty_map.cddl --why --strict
cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/invalid_within_choice_arm_rejected.cddl --why --strict
cargo run -p cbork -- lint cddl/vectors/project/positive/valid_within_dtrm_cbor.cddl --strict
cargo run -p cbork -- lint cddl/vectors/project/positive/valid_and_non_empty_map.cddl --strict
cargo run -p cbork -- lint cddl/vectors/project/positive/valid_within_optional_rhs_map_key.cddl --strict
cargo run -p cbork -- lint cddl/vectors/project/positive/valid_within_rfc9581_group_socket_map.cddl --strict
just fix-ci
```

Completion notes:

* Added semantic-error fixtures:
    * `cddl/vectors/project/semantic-errors/invalid_within_cbor_dtrm_direction.cddl`
    * `cddl/vectors/project/semantic-errors/invalid_within_missing_map_key.cddl`
    * `cddl/vectors/project/semantic-errors/invalid_within_required_rhs_missing.cddl`
    * `cddl/vectors/project/semantic-errors/invalid_and_empty_map.cddl`
    * `cddl/vectors/project/semantic-errors/invalid_within_choice_arm_rejected.cddl`
* Added positive fixtures:
    * `cddl/vectors/project/positive/valid_within_dtrm_cbor.cddl`
    * `cddl/vectors/project/positive/valid_and_non_empty_map.cddl`
    * `cddl/vectors/project/positive/valid_within_optional_rhs_map_key.cddl`
    * `cddl/vectors/project/positive/valid_within_rfc9581_group_socket_map.cddl`
* Added fixture-backed CLI lint tests in `crates/cbork/src/lint.rs`.
  Negative fixtures assert `E030` and expected reason / diff content.
  Positive fixtures assert clean strict lint.
* Validation passed:
    * `cargo test -p cbork lint::tests::lint_invalid_within -- --nocapture` (`4 passed`)
    * `cargo test -p cbork lint::tests::lint_valid -- --nocapture` (`4 passed`)
    * `cargo test -p cbork-cddl-compiler` (`252 passed`, plus integration/doc tests)
    * `cargo test -p cbork` (`42 passed`)
    * explicit CLI fixture smoke run:
        * all five `semantic-errors` fixtures failed as expected with `E030`
        * all four `positive` fixtures passed under `--strict`
    * `just fix-ci`

### Step 9: Clean up stale parallel state only after behavior is covered

Reasoning:

`within.rs` currently has its own `DefinitionMap`.
It works well enough for current checks and has tests.
Replacing it too early risks breaking semantics while the diff work is still moving.

Concrete work:

* Do not remove `DefinitionMap` as part of the first ctlop/diff implementation unless tests prove replacement parity.
* Once structured conflicts and diff diagnostics are green, evaluate whether `DefinitionMap`
  can be replaced by canonical compiler state.
* If replacing it:
    * add socket state to the compiler cache or finalization context
    * migrate all existing `within.rs` tests first
    * prove `//=` socket choices and `=` group references still expand correctly
* Keep old tests that describe known bugs until they are either fixed or deliberately reclassified.

Validation:

```sh
cargo test -p cbork-cddl-compiler within::tests
just fix-ci
```

Completion notes:

* `DefinitionMap` (in `crates/cbork-cddl-compiler/src/within.rs`) is **kept as-is**.
  It currently owns the socket_choices and definition tables that `.within` checks consult.
  Moving socket state into the canonical compiler cache / finalization context is a non-trivial refactor
  and is explicitly deferred to a later step.
* `DefinitionMap` is `pub(crate)` (not exported from `cbork-cddl-compiler`),
  defined in exactly one place, and has dedicated tests:
    * `defmap_contains_top_level_rule`
    * `defmap_contains_multiple_rules`
    * `defmap_missing_rule`
    * `resolve_definition_resolves_simple_type`
    * `resolve_definition_resolves_array`
* The historical `bug_*` tests are **kept** as regression / limitation guards.
  None are commented out and none are `#[ignore]`d.
  Their names are historical; they do not all assert buggy behavior now.
    * `within::tests::bug_range_value_should_be_subtype_of_int` now asserts the fixed behavior
      (`Range(-19..-19)` is accepted as a subtype of `int`) and should eventually be renamed /
      re-commented.
    * `within::tests::bug_map_with_socket_plug_unresolved` documents the current socket-plug
      carrier shape and nested ctlop preservation while socket expansion remains intentionally
      separate from this cleanup step.
    * `within::tests::bug_group_reference_in_map_not_expanded` documents the current
      group-reference expansion limitation and verifies it reports a structured
      `LhsNotAccepted` conflict.
* `//=` socket choices and `=` group references are exercised by Step 8 fixtures
  (`valid_within_rfc9581_group_socket_map.cddl`, `invalid_within_choice_arm_rejected.cddl`)
  and the explicit `cbork::lint::tests::lint_*` test suite.
  The Step 8 fixtures are run in CI via `cbork-cddl-parser::parse_cddl_files` (positive)
  and the `cbork::lint::tests::lint_invalid_within_*` tests (semantic-errors).
* No stale fixtures introduced.
  The pre-existing `cddl/vectors/project/semantic-errors/invalid_within_range.cddl` was already broken
  (parse error on `0..100` after `.within` without parens)
  and is outside Step 8/9 scope; it has no test in `lint.rs` and is left untouched.
* Validation passed:
    * `cargo test -p cbork-cddl-compiler within::tests::bug_ -- --nocapture` (`3 passed`)
    * Previously verified `cargo test -p cbork-cddl-compiler within::tests -- --nocapture`
      (`99 passed`)
    * Full compiler suite was verified in Step 8 with `cargo test -p cbork-cddl-compiler`
      (`252 passed`, plus integration/doc tests)
    * `just fix-ci` (54/54 tasks green)

## Proposed Subcommands

### 1. `cbork lint`

This is the first command that must be excellent.
It is the immediate user-facing value of the toolchain.

Purpose:

* parse CDDL
* resolve includes/imports
* expand generics
* derive literals
* run semantic validation
* apply pruning and postlude injection
* report all diagnostics in one pass
* accept `--library` for reusable modules
* warn on redundant choice arms
* warn on unreachable choice arms caused by earlier arms shadowing later ones

Suggested options:

* `cbork lint <path...>`
* `--stdin`
* `--recursive`
* `--fix` for future fixable lint classes
* `--strict`
* `--warn <rule>`
* `--deny <rule>`
* `--allow <rule>`
* `--json`
* `--stats`
* `--summary`
* `--why`

This command should be capable of linting a single file, a directory tree, or standard input.
When `--why` is present, every warning or error should be followed by one or more cited standards snippets that explain the rule.

Deferred lint work:

* implement general choice analysis after compilation and generic expansion
* treat socket and group plug accumulation as ordered choice arms feeding the same analysis
* detect exact duplicate choice arms as `redundant choice arm`
* detect later arms fully subsumed by earlier arms as `unreachable choice arm`
* emit the warning on the later arm and reference the earlier dominating arm
* support safe autofix for redundant declarations and redundant plug assignments
* support risky autofix for choice reordering aimed at maximizing arm reachability
* consider a file-level policy directive such
  as `;@ CBORK: match-reorder=false` to disable risky reorder fixes without suppressing the underlying warning
* implement `;@ CBORK: Library` and `;@ CBORK: Export` lint rules from § CBORK library/export directives
* warn when an imported/included module is not marked as a library
* warn when external consumers reference imported/included rules that are not marked as exports
* add a separate style lint that recommends placing `;@ CBORK: Export` immediately next to the rule it exports

#### Optional documentation linting

`cbork lint` should support optional documentation-comment linting.
This is not part of the default CDDL semantic lint pass.
It should run only when explicitly requested, for example with `--doc`.

Execution order:

* Parse, compile, resolve imports/includes, expand generics, and run normal CDDL lint first.
* If the CDDL pass has errors, skip documentation linting.
* If the CDDL pass has only warnings, documentation linting may run.
* In `--strict` mode, documentation warnings should contribute to command failure the same way other warnings do.
* If `--fix --doc` is present, run documentation fixes only after the CDDL source is known to be syntactically
  and semantically usable.

Markdown engine:

* Use the `rumdl` crate directly for Markdown linting and fixing.
* Use the user's `.rumdl.toml` discovery/configuration so CDDL documentation follows the same Markdown style
  as the rest of the repository.
* Do not introduce a parallel cbork-specific Markdown configuration format.
* Use `rumdl`'s programmatic API against the synthetic Markdown document in memory.
* Do not fork/exec the `rumdl` command-line tool for this feature.
* Do not add a shell-out adapter.
  Process execution would make source mapping, fix safety, and test isolation worse than using the crate API directly.

CDDL-to-Markdown transform:

* Convert the CDDL source into a synthetic Markdown document.
* Strip only the documentation marker from `;!` lines.
  Preserve the remaining Markdown text exactly, including indentation after the marker.
* Replace every non-documentation CDDL span with a generated HTML splice marker.
* The transform is a single-file operation over an already captured pre-transform CDDL buffer.
* The marker should identify only the original line range inside that captured buffer, for example:

  ```markdown
  <!-- CBORK CDDL FROM 12-27 -->
  ```

* The marker means "when reversing the transform,
  insert lines 12 through 27 inclusive from the captured pre-transform CDDL buffer here."
* Keep the actual non-documentation CDDL text out of the synthetic Markdown document.
* Whitespace-only spans between separate documentation blocks are still non-documentation CDDL spans.
  Preserve them as splice markers so the Markdown transform does not accidentally merge separate doc blocks.
* Insert one clear blank line above and below every generated splice marker.
* Treat those blank lines as generated wrapper lines.
  Remove them together with the splice marker when reversing the transform.
* Reject documentation comments that contain an opened but unclosed multiline HTML comment.
  Otherwise a user-authored comment could swallow generated splice markers.
* Reject user-authored documentation comments that contain the reserved `CBORK CDDL FROM` marker prefix.
* Do not use fenced `cddl` code blocks for generated non-documentation source.
  HTML splice markers avoid code-fence language linting and avoid collisions with user-authored code fences inside doc comments.

Source mapping:

* Every generated Markdown line must map back to either an original CDDL source line or a synthetic generated line.
* Diagnostics from documentation comment lines should report against the original CDDL line containing the corresponding `;!`.
* Diagnostics from generated splice markers should normally be suppressed.
* Diagnostics from generated blank lines around splice markers should normally be suppressed.
* If a Markdown diagnostic lands on a generated wrapper line that cannot be mapped to a user doc line,
  hide it unless it represents an internal transform bug.
* Capture the actual `rumdl` warning/error rule, message, line, column, and severity.
* Report `rumdl` diagnostics as cbork diagnostics at the original CDDL source line and column.
* Apply the stripped-marker column offset so rendered carets line up with the user's CDDL source,
  not with the synthetic Markdown representation.

Reverse transform for `--fix --doc`:

* Apply `rumdl` fixes to the synthetic Markdown document.
* Reverse-map only changed Markdown lines that originated from documentation comments.
* Re-emit those lines as `;!` comments.
* Preserve non-doc CDDL source byte-for-byte.
* Preserve regular comments, directive comments, whitespace, and CDDL formatting byte-for-byte.
* Restore non-documentation CDDL spans from the captured pre-transform CDDL line table using the transform map,
  not from the text of the splice marker alone.
* Do not reread the on-disk source file during reverse transform.
  The final output is derived from `pre_transform_cddl_lines` plus the fixed synthetic Markdown lines.
* Refuse to apply a fix if it deletes, duplicates, reorders, or materially changes generated splice markers.
  In that case, report a non-fixable documentation-lint error rather than risking source corruption.
* After reconstructing the fixed CDDL, run a final line-cleanup pass that collapses multiple consecutive blank lines
  to one blank line.
* That blank-line cleanup is only a final `--fix` output policy.
  It must not affect the front-end Markdown transform or the splice markers used to preserve separate doc blocks.

Comment marker classification:

* The parser should continue preserving all CDDL comments as comment nodes.
* cbork semantic classification should recognize documentation comments, CBORK directive comments,
  and include/import directive comments only when the marker appears on a standalone comment line.
* Leading whitespace before the marker is allowed.
* `;!`, `;@`, and `;#` markers should be flushed left by formatting/fix output.
* Any `;!`, `;@`, or `;#` marker that appears after non-whitespace CDDL source text on the same line is a marker misuse.
* Marker misuse should be a lint diagnostic, not a parser error.
* The diagnostic should explain that special comment markers are recognized only after leading whitespace
  and are treated as ordinary CDDL comments when used as trailing comments.
* This warning is required even when documentation linting is disabled,
  because a trailing `;@` or `;#` would otherwise silently fail to apply a directive.
* A misused marker should not bind documentation, apply a CBORK directive, or apply an include/import directive.
* Trailing regular comments remain valid regular comments.

Documentation binding model:

* The first documentation block before any other non-whitespace source content is the file/module-level documentation block.
* A documentation block is a contiguous run of `;!` lines.
* Blank lines or CDDL definitions break documentation contiguity.
* `;@` directive comments, include/import comments, and regular `;` comments do not break documentation contiguity.
* Documentation association may skip over `;@` directive comments, include/import comments, and regular `;` comments.
* Documentation association may also skip over whitespace-only lines that are part of an uninterrupted comment/directive
  preamble before the next definition.
* A documentation block documents the next CDDL definition when no blank line or prior CDDL definition separates the block
  from that definition.
* Directives placed between a documentation block and a definition still apply normally and do not steal the documentation.
* Regular comments placed between a documentation block and a definition remain source comments and do not become documentation.
* Same-line regular comments remain source comments and are not documentation comments.

Semantic documentation checks:

* File/module documentation should start with a level-1 Markdown heading.
  `rumdl` should catch the general heading style, but cbork should attach the failure to the file-level doc block.
* Definition documentation should start with a level-3 Markdown heading.
* Level-2 headings are reserved for sectioning inside file/module documentation.
* Exported definitions must have documentation.
* Generic definitions that have documentation must document every generic parameter.
* Exported generic definitions must document every generic parameter.
* Internal definitions may omit documentation by default.
* Add a policy option for internal definition documentation:
    * `no` - do not require docs for internal definitions.
    * `warn` - warn when internal definitions lack docs.
    * `yes` - error when internal definitions lack docs.
* The default should be `no` for internal definitions
  so enabling `--doc` does not force every private helper rule to be documented immediately.

Suggested CLI surface:

* `cbork lint --doc <path>` enables documentation linting.
* `cbork lint --doc --fix <path>` applies safe documentation-comment fixes.
* `cbork lint --doc-internal no|warn|yes <path>` controls internal definition documentation requirements.
* `cbork lint --doc-json <path>` is not needed initially;
  normal `--json` should include documentation diagnostics once JSON output exists.

Required fixtures:

* Positive file-level documentation with a `#` title and generated splice markers.
* Positive exported definition documentation with a level-3 heading.
* Positive doc comment containing a literal ```` ```cddl ```` fenced block.
* Negative file-level documentation missing a level-1 heading.
* Negative exported definition missing documentation.
* Negative definition documentation using a level-2 heading.
* Negative exported generic missing one generic parameter description.
* Negative trailing `;!` marker warning that proves it is treated as a regular comment, not documentation.
* Negative trailing `;@` marker warning that proves it is treated as a regular comment, not a CBORK directive.
* Negative trailing `;#` marker warning that proves it is treated as a regular comment, not an include/import directive.
* Fix fixture proving two doc comment blocks separated only by blank lines remain separate after `--doc --fix`.
* Fix fixture proving multiple consecutive blank lines in the final fixed CDDL are reduced to one blank line.
* Fix fixture proving `--doc --fix` rewrites only `;!` lines and leaves CDDL source unchanged.

Implementation steps:

1. Add stable fixtures first.
   Place positive and negative CDDL documentation fixtures under `cddl/vectors/project`.
   Include fixtures for file docs, exported definition docs, generic parameter docs, literal Markdown code fences,
   generated splice marker behavior, trailing special-marker misuse, and safe fix output.
   Status: complete.
   Stable fixtures now exist under `cddl/vectors/project/positive/doc_lint` and `cddl/vectors/project/negative/doc_lint`.
   They cover module docs, exported definition docs, generic parameter documentation failures, literal fenced `cddl` blocks,
   trailing `;!`/`;@`/`;#` marker misuse, doc-fix input/expected pairs, separated doc blocks, and final blank-line collapse.
   Later steps still need to wire all semantic doc fixtures into the actual `--doc` implementation.

2. Add comment marker classification.
   Keep the parser permissive and preserve every CDDL comment as a comment node.
   Add a compiler or linter classification helper that decides whether a comment is a standalone `;!`, `;@`,
   or `;#` semantic marker.
   Leading whitespace before the marker is allowed.
   Any marker after non-whitespace CDDL source text on the same line must produce the marker-misuse warning.
   Status: complete.
   `cbork-cddl-compiler::marker` classifies `;!`, `;@`, and `;#` markers,
   distinguishes standalone markers from trailing markers using original source-line context,
   and emits W030 marker-misuse warnings during the normal compile/lint path.
   Unit tests cover marker classification, indented standalone markers, trailing markers, regular comments, and source-line lookup.
   CLI/lint tests cover W030 on trailing `;!`, `;@`, and `;#`, plus no W030 for standalone doc and CBORK directive markers.
   Step 3 remains responsible for making directive application itself use this standalone-only classification.

3. Route standalone-only classification into existing directive handling.
   Ensure CBORK directives and include/import directives are only applied
   when the marker classification says the marker is standalone.
   A trailing `;@` or `;#` must remain an ordinary comment and must not affect compilation.
   Add focused tests proving trailing directive markers warn and do not apply.
   Status: complete.
   `inject_directives` now receives the original source text and uses `is_trailing_marker_comment`
   before parsing `;#` include/import directives.
   Trailing `;# include ...` comments are preserved as ordinary comments, so they do not attempt include/import resolution.
   CBORK file-directive scanning and directive-site collection also use `is_trailing_marker_comment`
   before applying `;@ CBORK: Library` or `;@ CBORK: Extern ...`.
   Trailing `;@ CBORK: Library` therefore does not set `is_library`
   and does not produce misplaced/duplicate CBORK directive diagnostics.
   Tests cover trailing `;@ CBORK: Library` not applying, trailing `;# include "./nonexistent.cddl"` not attempting resolution,
   and both still producing W030 marker-misuse warnings.

4. Add the documentation block scanner.
   Scan the captured pre-transform CDDL text line by line.
   Build doc blocks from standalone `;!` lines.
   Preserve original source line numbers for every doc line.
   Track intervening regular comments, directive comments, include/import comments, whitespace,
   and CDDL definition lines so binding follows the documented association rules.
   Status: complete.
   `cbork-cddl-compiler::doc_block` provides `scan_doc_blocks`, `DocBlock`, `DocLine`, `DocBinding`, `DocScan`,
   and `doc_block_range`.
   The scanner operates on the captured pre-transform CDDL text, groups standalone `;!` lines into doc blocks,
   strips only the `;!` marker, preserves source line numbers for each doc line, and records binding metadata.
   Regular comments, `;@` directives, and `;#` include/import directives are transparent for binding.
   Blank lines and CDDL definition lines break binding as documented.
   Unit tests cover blank lines, regular comments, CBORK directives, include/import directives, multiple doc blocks, orphan docs,
   source-line ranges, and marker stripping.
   CLI/lint tests cover the stable doc-lint fixtures and verify block grouping and binding behavior.
   The scanner is deliberately line-based and treats standalone `;!` lines inside multi-line CDDL bodies as doc lines;
   later transform and semantic passes must use the recorded source-line binding data rather than infer AST ownership.

5. Build the CDDL-to-Markdown transform.
   Produce a synthetic Markdown string from the captured pre-transform CDDL line table.
   Strip the standalone `;!` marker from documentation lines,
   then remove the common leading-space indent from each contiguous documentation block before passing it to Markdown linting.
   Replace every non-doc CDDL span with a generated splice marker of the form `<!-- CBORK CDDL FROM start-end -->`.
   Treat whitespace-only spans between separate doc blocks as non-doc CDDL spans and preserve them with splice markers.
   Insert generated blank lines above and below each splice marker.
   Record a generated-line map that distinguishes original doc lines, splice markers, generated blank lines,
   and other synthetic lines.
   Status: complete.
   `cbork-cddl-compiler::transform` provides `transform_to_markdown`, `SyntheticMarkdown`, `SyntheticLine`, `SyntheticLineKind`,
   `SPLICE_MARKER_PREFIX`, `splice_span`, and `source_line`.
   The transform operates on the captured pre-transform CDDL source,
   emits standalone `;!` blocks as Markdown after stripping the marker and dedenting common leading space,
   collapses contiguous non-doc source spans into `<!-- CBORK CDDL FROM start-end -->` markers,
   and wraps each splice marker with generated blank lines.
   Reverse transform writes fixed documentation back with uniform `;!` prefixes while preserving relative Markdown indentation.
   The generated line map distinguishes doc lines, splice markers, and generated wrapper blanks.
   Whitespace-only spans between separate doc blocks are preserved as splice markers so Markdown linting cannot merge blocks.
   Unit tests cover no-doc sources, doc-line marker stripping, non-doc span coalescing, whitespace-only spans between doc blocks,
   adjacent doc lines, inline doc lines inside multi-line rules, stable splice-marker formatting, one-indexed output lines,
   and source-line helpers.
   CLI/lint tests cover the stable doc-lint fixtures.
   Step 6 remains responsible for validating reserved marker text and unsafe user-authored HTML comments.

6. Add transform safety validation.
   Reject doc comments containing the reserved `CBORK CDDL FROM` marker prefix.
   Reject doc comments that open but do not close a multiline HTML comment.
   Unit-test that user-authored fenced code blocks inside doc comments survive without interacting with generated markers.
   Status: complete.
   `cbork-cddl-compiler::doc_lint::validate_doc_source` scans the captured pre-transform CDDL source,
   validates every documentation block, and emits error diagnostics for unsafe transform input.
   `E040` rejects documentation comments containing the reserved `CBORK CDDL FROM` splice-marker prefix.
   `E041` rejects documentation comments that open more `<!--` HTML comments than they close.
   The validation deliberately ignores matching text outside documentation comments,
   so ordinary CDDL comments and CDDL source remain unaffected.
   Unit tests cover clean sources, reserved-marker rejection, unclosed HTML-comment rejection,
   closed and balanced multiline HTML comments, multiple unclosed comments, and reserved-marker text outside doc comments.
   CLI/lint tests exercise the public API against stable doc-lint fixtures.

7. Integrate `rumdl` through its crate API.
   Add the `rumdl` crate dependency needed for programmatic lint/fix.
   Load the user's `.rumdl.toml` using `rumdl`'s configuration discovery API.
   Run `rumdl` against the synthetic Markdown string in memory.
   Do not fork or exec the `rumdl` command.
   Do not add a shell-out fallback.
   Status: complete.
   `cbork-cddl-compiler` depends on `rumdl = { version = "0.2.24", default-features = false }` so the `native`
   (LSP / file watcher / notify / blake3 / etc.) and `parallel` features are not pulled in.
   `doc_lint::lint_synthetic_markdown` runs `rumdl_lib::lint` directly against the in-memory synthetic Markdown string.
   `doc_lint::apply_rumdl_fixes` calls `rumdl_lib::utils::fix_utils::apply_warning_fixes` for later reverse-transform work.
   There is no shell-out adapter.
   `resolve_rumdl_config_path` discovers the rumdl config rooted at the CDDL source file's directory
   (walking up looking for `.rumdl.toml`, `rumdl.toml`, `.config/rumdl.toml`, or a `[tool.rumdl]` section in `pyproject.toml`)
   before falling back to rumdl's own discovery.
   Unit tests cover: same-directory hit, nearest-config-wins, no-config-found, `pyproject.toml` without/with `[tool.rumdl]`,
   explicit `config_path` override, source-directory walk-up, no-config-found fallthrough,
   and an end-to-end test where a fixture-local `.rumdl.toml` that disables MD013 silences the noisy line-length rule for that file.
   `just fix-ci` is green after disabling rumdl's `native` feature,
   which removes the `cargo-deny` license-check and duplicate-version warnings introduced by the dependency graph.
   Re-verified with `just fix-ci` on 2026-06-29 after the Step 7 status update.

8. Map `rumdl` diagnostics back to CDDL diagnostics.
   Diagnostics on original doc lines must report the original CDDL line containing the corresponding `;!`.
   Diagnostics on generated splice markers and generated blank lines should be suppressed
   unless they reveal an internal transform bug.
   Preserve `rumdl` rule IDs, messages, severities, line numbers, and column numbers.
   Apply the stripped-marker column offset so cbork's diagnostic underline points at the original CDDL source column,
   not the synthetic Markdown source column.
   Status: complete at the compiler-library API level.
   `cbork-cddl-compiler::doc_lint::map_rumdl_diagnostics` maps rumdl `LintWarning`s through the synthetic line map produced by
   `transform_to_markdown`.
   Warnings on `DocLine` entries become cbork diagnostics on the original CDDL source file,
   preserving the rumdl rule ID as the diagnostic code, preserving the message,
   mapping rumdl `Error` to cbork error and rumdl `Warning`/`Info` to cbork warning,
   and applying the stripped-marker column offset including leading source indentation.
   Warnings on generated splice markers and generated wrapper blank lines are suppressed and recorded as `SuppressedWarning`s.
   Unit tests cover splice-marker suppression, generated-blank suppression, doc-line mapping with column offset,
   UTF-8-safe column-to-byte conversion, and severity mapping.
   CLI/lint tests exercise the Step 6 -> Step 5 -> Step 7 -> Step 8 API path on a stable fixture
   and assert every rumdl warning is either mapped to a CDDL diagnostic or explicitly suppressed.
   This step is not yet exposed through `cbork lint --doc`; CLI flag wiring, execution ordering, strict-mode behavior,
   and end-to-end diagnostic rendering remain Step 10 and Step 12 work.

9. Implement semantic documentation checks.
   Check that file/module docs start with a level-1 heading.
   Check that definition docs start with a level-3 heading.
   Check that exported definitions have docs.
   Check that documented generic definitions document every generic parameter.
   Check that exported generic definitions document every generic parameter.
   Implement `--doc-internal no|warn|yes`, defaulting to `no`.
   Status: complete.
   `cbork-cddl-compiler::doc_semantics` provides `check_doc_semantics`, `DocSemanticsConfig`, `DocInternalPolicy`,
   and `DocSemanticsReport`.
   The pass walks the AST from `CompiledCDDL::user_nodes` plus the `DocScan` produced by `scan_doc_blocks`.
   It emits `E030` when the file-level doc block does not start with a level-1 heading.
   It emits `E031` when a definition's doc block does not start with a level-3 heading.
   It emits `E032` when an exported definition has no documentation comment.
   It emits `E033` for every generic parameter on a documented definition that the doc block does not mention by name.
   The generic-parameter match is word-based
   (delimited by non-word characters) so single-letter parameter names like `a` are not satisfied by incidental mentions.
   It emits `W040` / `E034` for undocumented internal definitions under `--doc-internal warn` / `--doc-internal yes`.
   The default `--doc-internal no` is silent for internal definitions.
   A doc block that is both file-level and bound to a definition receives both the file-doc and definition-doc checks.
   The exported set comes from the real `;@ CBORK: Export` directive model
   (see the `Export` arm of `CborkDirective` in `compiled.rs`), not from `;@ CBORK: Extern` declarations.
   The compiler's directive scanner resolves each Export site to the next rule definition while skipping blank lines,
   regular comments, and doc comments; it must not cross import/include directives or another `;@ CBORK:` directive.
   A dangling Export (EOF or cancelled before a rule) emits `E021`.
   A duplicate Export for the same rule name emits `E022`. 18 unit tests cover the heading levels, exported coverage,
   internal policy, generic-parameter word matching, and the file/definition coverage paths.
   Re-checked on 2026-06-29: the previous `extern_names`/export confusion is fixed.
   `run_doc_lint` now passes `compiled.exported_names` into `DocSemanticsConfig`,
   and the compiler exposes `exported_names` from the real `;@ CBORK: Export` directive model.

10. Wire the CLI flags and execution order.
    Add `cbork lint --doc`.
    Add `cbork lint --doc --fix`.
    Add `cbork lint --doc-internal no|warn|yes`.
    Run normal CDDL lint first.
    Skip documentation linting if normal CDDL lint has errors.
    Allow documentation linting to run when normal CDDL lint only has warnings.
    In `--strict`, documentation warnings must fail the command.
    Status: complete.
    `crates/cbork/src/cli.rs` adds `--doc` and `--doc-internal no|warn|yes` to the `Lint` subcommand.
    `crates/cbork/src/lint.rs` adds a `DocLintOptions` struct and a `LintRunOptions` wrapper
    that carries both the `PrintOptions` bitmask and the doc-lint options.
    `check_file_with_print` runs the normal CDDL lint first; when `--doc` is set and the normal lint did not produce errors,
    it dispatches to `run_doc_lint`, which executes `validate_doc_source` (step 6), `transform_to_markdown`
    (step 5), `lint_synthetic_markdown` (step 7), `map_rumdl_diagnostics` (step 8), and `check_doc_semantics` (step 9) in order.
    When `--fix` is also set, the doc-lint pass calls `apply_rumdl_fixes` in memory and emits a `W032` notice that the file is not
    yet written to disk; the conservative reverse transform that writes the fixed CDDL back is step 11 of the plan.
    A failed fix apply emits `W033`.
    `--strict` causes the combined normal + doc-lint warning count to fail the command.
    Per-file results are combined with `&&` so a directory run fails when any child file fails.
    The default `--doc-internal no` policy is silent for internal definitions,
    matching the plan's "so enabling `--doc` does not force every private helper rule to be documented immediately" requirement.
    The doc-lint pipeline filters the rumdl rule set through `rumdl_lib::rules::filter_rules` so a fixture-local `.rumdl.toml`
    that disables noisy style rules actually silences them.
    A fixture-local `cddl/vectors/project/.rumdl.toml` disables the by-default style rules
    so the doc-lint fixtures under `cddl/vectors/project/{positive,negative}/doc_lint/` exercise the *semantic* checks
    (E030/E031/E032/E033/W040/E034) rather than incidental Markdown style noise.
    `resolve_rumdl_config_path` (step 7) walks up from each fixture's directory to discover this file. 5 CLI integration tests in
    `crates/cbork/src/lint.rs` invoke `cbork lint --doc <fixture>` on the positive and negative doc-lint fixtures and assert the
    intended diagnostic codes (`E030`, `E031`, `E032`, `E033`) appear, and the positive path stays clean.
    Re-checked on 2026-06-29:
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/positive/doc_lint/doc_exported_definition_with_h3.cddl`
      succeeds cleanly.
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/negative/doc_lint/doc_exported_missing_docs.cddl`
      emits `E032` for `location-references`.
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/negative/doc_lint/doc_exported_generic_missing_param.cddl`
      emits `E033` for the missing `key` parameter.
    * `cargo run -p cbork -- lint --doc --strict cddl/vectors/project/negative/doc_lint/doc_file_missing_h1.cddl`
      fails the command because doc warnings are treated as failures under `--strict`.

    `check_doc_semantics` now takes `source_text: &str` and `source_path: &Path` parameters.
    `compute_line_offsets` converts line numbers to byte offsets
    so the `Diagnostic::span` field carries a proper byte offset range rather than a line-number range.
    Every semantic diagnostic sets `source_file: Some(source_path.to_path_buf())`.
    The CLI output now includes `--> path:line:column` pointers and source-snippet carets for every semantic diagnostic,
    matching the rumdl diagnostic rendering.
    `doc_block_covers_line` checks only `binding.definition_line`
    (the file-level flag does not declare every line covered — that was a bug fixed in this pass).
    Re-checked on 2026-06-29 after the diagnostic-location fix:
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/negative/doc_lint/doc_exported_missing_docs.cddl`
      emits `E032` at `doc_exported_missing_docs.cddl:10:1` with a source snippet.
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/negative/doc_lint/doc_exported_generic_missing_param.cddl`
      emits `E033` at `doc_exported_generic_missing_param.cddl:11:1` with related definition context at line 13.
    * `cargo run -p cbork -- lint --doc cddl/vectors/project/negative/doc_lint/doc_definition_h2_heading.cddl`
      emits `E031` at `doc_definition_h2_heading.cddl:11:1` with related definition context at line 13.
    * `cargo run -p cbork -- lint --doc --strict cddl/vectors/project/negative/doc_lint/doc_file_missing_h1.cddl`
      emits `E030` at `doc_file_missing_h1.cddl:2:1` and fails the command under `--strict`.

    No Step 10 implementation blockers remain.
    Step 12 should still harden the CLI integration tests so they assert the expected source-line rendering,
    not only diagnostic codes/messages.

11. Implement conservative reverse transform for `--fix --doc`.
    Apply `rumdl` fixes to the synthetic Markdown in memory.
    Verify every generated splice marker still exists exactly once and in the original order.
    Reject the fix if generated splice markers were deleted, duplicated, reordered, or materially changed.
    Reconstruct final CDDL from fixed doc lines plus original non-doc spans from `pre_transform_cddl_lines`.
    Do not reread the on-disk file during reverse transform.
    Preserve non-doc CDDL byte-for-byte.
    Then apply the final CDDL output cleanup that collapses multiple consecutive blank lines to one blank line.
    This cleanup runs only after the reverse transform has reconstructed valid CDDL.
    Status: complete.
    The reverse transform now filters *both* the fixed synthetic and the original line map
    for blank-content lines (`text.trim().is_empty()`), not just generated wrapper blanks.
    This alignment handles the case where `rumdl` adds or removes blank Markdown lines
    (e.g. MD022, MD023) without changing the splice-marker span set.
    The `doc_fix_input.cddl` smoke check outside `cddl/vectors/project/.rumdl.toml` now succeeds with `W035` instead of rejecting
    with `E035`. 27 unit tests in `transform.rs` cover roundtrip fidelity, blank-doc-line handling when `rumdl` removes blank lines,
    and blank-line insertion by `rumdl`. 1 CLI test in `crates/cbork/src/lint.rs` (`cli_doc_lint_fix_writes_modified_cddl_to_disk`)
    creates a temporary fixture with a fixture-local `.rumdl.toml` that enables MD022 (blanks-around-headings) and MD023
    (heading-start-left) and proves `--doc --fix` writes the fixed CDDL to disk without rejecting.
    `just fix-ci` is green after this step.

12. Add integration tests.
    Add positive CLI tests proving `--doc` accepts valid file docs, exported definition docs,
    literal fenced code blocks inside docs, and generated splice markers.
    Add negative CLI tests proving missing file title, missing exported docs, bad heading level, missing generic parameter docs,
    and trailing special-marker misuse are reported.
    Add diagnostic tests proving `rumdl` line and column diagnostics point at the original CDDL doc comment location.
    Add fix tests proving blank-line-only gaps between doc blocks remain gaps and multiple consecutive blank lines collapse to one.
    Add a fix test proving `--doc --fix` changes only `;!` lines and preserves all CDDL source spans exactly.
    Status: complete.
    CLI integration tests in `crates/cbork/src/lint.rs`:
    * Positive: `cli_doc_lint_positive_file_with_title_passes`,
      `cli_doc_lint_positive_fenced_cddl_block_passes`,
      `cli_doc_lint_positive_exported_definition_with_h3_passes`.
    * Negative: `cli_doc_lint_negative_file_missing_h1_emits_e030`,
      `cli_doc_lint_negative_definition_h2_emits_e031`,
      `cli_doc_lint_negative_exported_missing_docs_emits_e032`,
      `cli_doc_lint_negative_exported_generic_missing_param_emits_e033`.
    * Trailing marker misuse: `cli_doc_lint_trailing_doc_marker_emits_w030`,
      `cli_doc_lint_trailing_cbork_directive_marker_emits_w030`,
      `cli_doc_lint_trailing_include_marker_emits_w030`.
    * Fix preservation: `cli_doc_lint_fix_writes_modified_cddl_to_disk`,
      `cli_doc_lint_fix_preserves_blank_line_gaps_between_blocks`,
      `cli_doc_lint_fix_does_not_alter_non_doc_cddl_source`.
    Re-checked on 2026-06-29: 78 cbork tests pass, 27 transform tests pass, 374 compiler unit tests pass,
    plus 19 import/include vectors, 8 render vectors, and the compiler doctest. (Previously: 70 cbork tests; +8 new.)

13. Validate the full workspace.
    Run focused doc-lint tests first.
    Run the normal lint test suite.
    Run `just fix-ci`.
    Do not mark the feature complete until the stable fixtures pass and `just fix-ci` is clean.
    Status: complete.
    Re-checked on 2026-06-29: `cargo test --workspace` is green across all workspace test targets.
    `just fix-ci` is green (54/54 tasks).
    All doc-lint stable fixtures pass.

### 2. `cbork fmt`

Formatter command for source CDDL.

Purpose:

* normalize whitespace
* preserve comments and provenance
* keep the source tree stable
* optionally write back to disk
* optionally check formatting only

Suggested options:

* `cbork fmt <path...>`
* `--check`
* `--write`
* `--stdin`
* `--recursive`
* `--diff`

This should remain source-formatting, not rendered-document formatting.

### 3. `cbork compile`

Compiler inspection and debugging command.

Purpose:

* compile a CDDL document through all compiler stages
* print the enriched AST
* show warnings, metadata, and injection results
* help debug semantic passes and postlude handling
* accept `--library` for library analysis output

Suggested options:

* `cbork compile <path>`
* `--dump-user`
* `--dump-complete`
* `--dump-cache`
* `--no-tree`
* `--json`

This is a development and inspection command.
It should not be the primary user workflow for schema authors.

### 3a. `cbork why`

Explain why a diagnostic code exists.

Purpose:

* look up a diagnostic code such as `E016` or `W001`
* print the diagnostic rationale
* show the embedded RFC snippets that justify the rule

Suggested options:

* `cbork why <code...>`
* `--list`

This command is the standards-backed explanation layer for lint diagnostics.

### 3b. `cbork xref`

Cross-reference a schema term, ctlop, or diagnostic code to the standards corpus.

Purpose:

* look up control operators, grammar concepts, and diagnostic codes
* show the authoritative RFC excerpts that define or motivate them
* support future hover/help surfaces and language-server integration

Suggested options:

* `cbork xref <query...>`
* `--list`

This is the generic standards lookup layer that `why` builds on.

### 3c. `cbork rfc`

Inspect the embedded standards corpus directly.

Purpose:

* list the RFCs and drafts embedded in the binary
* dump the text of one embedded RFC to stdout
* keep the tool self-contained for CI, hover help, and offline reference

Suggested options:

* `cbork rfc`
* `cbork rfc <doc>`

This command should print the embedded file contents from `rfc.rs`, not read them from disk at runtime.

### 4. `cbork render`

Rendered schema command.

Purpose:

* produce a self-contained expanded schema
* preserve comments and provenance
* show included definitions in a readable format
* support documentation output

Suggested options:

* `cbork render <path>`
* `--expand-includes`
* `--provenance`
* `--markdown`
* `--cddl`
* `--doc`

This is the command that turns modular schema source into reader-friendly output.

### 5. `cbork decode`

Plain CBOR diagnostic notation command.

Purpose:

* decode raw CBOR without a schema
* show arrays, maps, tags, byte strings, text strings, and canonicalization details
* provide offsets and error location information
* understand reversible byte-transform annotations such as unofficial
  `.x-*` compression ctlops when schema context is available

Suggested options:

* `cbork decode <path|stdin>`
* `--hex`
* `--bytes`
* `--annotate`
* `--offsets`
* `--json`

This command is the baseline binary debugging tool.

### 6. `cbork validate`

Schema-aware CBOR validation command.

Purpose:

* validate a CBOR payload against a compiled CDDL schema
* report the matched path or failure path
* emit schema-aware diagnostics before validation starts
* render the decoded EDN tree on success only when `--detailed` is set
* render the decoded EDN tree on failure with the first failing path highlighted
* integrate embedded regex and ABNF checks
* recurse through reversible `bstr` transforms such as unofficial `.x-*`
  compression ctlops by following the RHS shape after decoding the payload

Suggested options:

* `cbork validate <schema> <payload>`
* `--warn`
* `--detailed`
* `--no-color`

This is the enforcement command for interoperability.

### 7. `cbork explain`

Schema explanation command.

Purpose:

* explain what the schema means in human terms
* report whether maps are open or closed
* describe choice shadowing and extension order
* explain the effect of control operators and embedded constraints
* accept `--library` for library-oriented explanation output

Suggested options:

* `cbork explain <path>`
* `--focused`
* `--examples`
* `--text`
* `--rich`

This should be built on the same semantic model as lint and validate.

### 8. `cbork coverage`

Coverage reporting command.

Purpose:

* measure schema and vector coverage
* show uncovered rules, branches, and constraints
* report positive and negative coverage
* require a concrete document root and reject `--library`

Suggested options:

* `cbork coverage <schema-or-vectors>`
* `--schema <path>`
* `--vectors <path>`
* `--threshold <percent>`
* `--json`
* `--summary`

### 9. `cbork docs`

Documentation export command.

Purpose:

* generate schema-linked documentation output
* include rendered examples
* include provenance and comments
* support downstream doc generation
* accept `--library` for library documentation output

Suggested options:

* `cbork docs <path>`
* `--markdown`
* `--html`
* `--vectors <path>`

This is close to `render`, but optimized for publication rather than source expansion.

### 10. `cbork lsp`

Editor-facing language server launcher.

Purpose:

* start the CDDL language server
* reuse parser/compiler semantics from the same libraries

This may remain a separate binary crate, but it should still be part of the `cbork` ecosystem.

## Command Grouping

The CLI should group functionality by user intent:

* `lint`, `fmt`, and `compile` for schema authoring
* `render`, `docs`, and `explain` for schema reading and publication
* `decode` and `validate` for CBOR payload checking
* `coverage` for ecosystem quality gating
* `lsp` for editor integration

This grouping keeps `cbork` coherent while still allowing feature growth.

## Output Policy

The CLI should use rich output where it helps:

* colorized diagnostics
* structured error blocks
* tree rendering
* summary tables
* coverage summaries
* rule-by-rule lint reporting

Plain output should still exist for CI and scripting.
JSON output should exist only where a machine actually benefits from it.

## Implementation Order

1. Keep `lint` as the main stable command.
2. Move the CLI parsing to `bpaf`.
3. Replace ad hoc terminal printing with `rusty-rich`.
4. Add formatter and compile inspection subcommands.
5. Add schema rendering and explanation commands.
6. Add CBOR decode and validate commands.
7. Add coverage reporting.
8. Expose or delegate LSP and editor workflows separately.

## Notes

The CLI should not try to own all runtime behavior.
It should orchestrate the compiler, parser, formatter, and validator crates.

The long-term shape is one umbrella `cbork` command with several focused subcommands,
plus a few specialist binaries where the UX or runtime model is better separated.

---

> **Note:** The completed `.within` / `.and` rewrite plan is in § `.within` / `.and` Rewrite Plan above.
> The original implementation plan below is retained only as historical context.
> The current implementation lives in `crates/cbork-cddl-compiler/src/within.rs`, `crates/cbork-cddl-compiler/src/schema_diff.rs`,
> and `crates/cbork/src/diagnostics.rs`.

## `.within` and `.and` Control Operator Implementation Plan (ORIGINAL, SUPERSEDED)

(The original 570-line implementation plan was removed.
See crates/cbork-cddl-compiler/src/within.rs for the current implementation.)
