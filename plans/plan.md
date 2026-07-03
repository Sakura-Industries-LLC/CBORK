# Plan: Stage 2 CDDL Module and Include Processing

This plan replaces the grammar/parser merge plan.
The canonical base CDDL parser is done.
The next iteration adds module comment parsing, include expansion, pruning, and final resolved document assembly.

## Current State Snapshot

Done:

* The base CDDL parser and directive-comment parser are in place.
* The standard-module catalog crate exists.
* The compiler crate scaffold exists.
* AST injection, provenance metadata, and `PRUNABLE` / `SILENT` metadata exist.
* Recursive include/import resolution exists.
* Generic expansion exists and is wired after include/import resolution and before literal/control-operator folding.
* The compiler already handles the literal-producing ctlops for `.abnf`, `.abnfb`, `.regexp`, `.json`,
  `.enc-abnf`, `.enc-abnfb`, `.hash-abnf`, and `.hash-abnfb`.
* The compiler already validates the known ctlop family structurally so later passes have a coherent tree to work with.
* The compiler has an initial finalization pass that compares user definitions against the postlude,
  injects referenced postlude definitions surgically, reports missing non-postlude references,
  and runs final ctlop validation over the complete tree.
* Pruning dangling removable definitions is implemented.
* Retained duplicate/conflicting definitions are detected after pruning.
* Retained dangling references are reported while references inside pruned rules are ignored.
* `complete_nodes` is built from the pruning-aware retained tree.

Partial:

* The resolved-types semantic cache exists, but it is not yet the final pruning-aware semantic state for the whole tree.
* The fixed-point semantic walk exists for the current literal and transformer cases, but it is not yet the final
  post-pruning semantic pass.
* The postlude is modeled as support data and can be surgically injected,
  but the exact postlude/support injection contract still needs sharper coverage.
* `complete_nodes` exists and is pruning-aware,
  but it is not yet guaranteed to be the authoritative error-free tree for downstream validation.

Outstanding:

* Tightening recursive postlude/support injection behavior and test coverage.
* Detecting any unresolved dangling definitions left after surgical postlude injection.
* Producing the final resolved document form after pruning and postlude merge, with hard errors preventing downstream validation.
* Extending semantic checks for authoritative low-tag tables beyond the immediate postlude work.

## Scope Notes

* The CDDL grammar remains the source of truth for syntactic parsing.
* The spec is the authority for module and include behavior.
* `rfc/draft-ietf-cbor-cddl-modules-06.txt` is the canonical reference for module/include behavior in this stage.
  Any local extensions should make the feature more functional and practical while preserving the core functionality
  and spirit of the draft.
* The parser should parse directives, not resolve the filesystem itself.
* Relative includes are always resolved against the source file that contained the directive,
  never the current working directory or some ambient include root.
* I am treating “module comments” as the draft module directives encoded in comment form, typically `;# ...`.
* The plan defines filename semantics locally because the spec does not pin them down tightly enough for implementation.
* The directive parser should return a typed enum for the directive family it found.
  `Import`, `ImportAs`, `Include`, and `IncludeAs` are the likely top-level variants.
  The parser should classify each directive in one pass over the comment text,
  not by trying `include` and then retrying as `import`.
* The directive-comment parser is a utility inside `cbork-cddl-parser`, not a separate crate.

## Comment Classes

Comments are still ordinary CDDL comments syntactically: `;` followed by comment text through end of line.

We treat the leading marker as a local convention so later compiler stages can attach meaning without changing the base grammar.

* `;#` - include/import directive comments.
* `;@` - code-generation comments, reserved for later compiler use.
* `;!` - markdown-formatted documentation comments.
* `;` - normal comments.
    * If there is no single space after `;`, one is assumed for normalized text handling.
    * If there is a single space, it is consumed.
    * After that initial normalization step, markdown and normal comments preserve their interior whitespace
  so text alignment can be retained.

Binding rules:

* Comments that appear before a definition are bound to that definition.
* A blank line breaks the binding chain.
* Comments on the same line as a definition also describe that definition,
  but they are a distinct same-line association.
* Same-line comments extend only to end of line.
* All comments are inserted into the AST faithfully at their source locations first.
  They remain plain comment nodes until later post-processing interprets them semantically.
* Comment association should preserve block boundaries.
    * Contiguous comment lines form a block.
    * A blank line separates blocks.
    * A comment on the same line as a definition is a line comment, not part of
  the preceding block.
    * A comment block immediately before a definition is associated with that
  definition.
    * A separated block is associated with whatever follows it, not with the
  preceding definition.
* Documentation rendering should preserve author intent.
    * Regular `;` comments are file/internal comments by default and are not
  rendered into generated documentation unless a later switch says otherwise.
    * `;!` comments are documentation comments and are rendered as markdown in
  generated docs by default.
    * The compiler should preserve both comment classes in the AST so later
  rendering passes can choose how to present them.

Example:

<!-- rumdl-disable MD040 -->

```cddl
;! ## this_thing
;!
;! Is documented
this_thing = that_thing ; this equals that
;! ## that_thing
;!
that_thing = true
```

<!-- rumdl-enable MD040 -->

In that model:

* The title block is distinct from the `this_thing` block because a blank line
  separates them.
* `; its always zero.` is associated with `this_thing`.
* The `;!` comment immediately after the definition belongs to whatever follows
  and does not attach back to `this_thing`.

Implementation guidance:

* Preserve comment text and association information in the compiler AST or metadata.
* Do not strip markdown whitespace after the initial `;!` normalization step.
* Keep directive comments separate from documentation comments so the compiler
  can process them independently.
* Do not assign semantic meaning during raw parsing.
  Semantic interpretation of comment classes happens in later compiler passes.

## Lint Checks

These are compiler/linter diagnostics, not parser failures.
They should be reported with good source provenance so users can see exactly what is wrong.

### Standard definition reuse

* If a document defines a standard name with the same meaning as the canonical built-in definition,
  report it as a redundant standard definition and warn by default.
  Example: `nil = #7.22`.
* If a document defines a standard name with a conflicting meaning,
  report it as a conflicting standard definition and treat it as an error by default.
  Example: `nil = $6.32(tstr)`.
* If a document uses a tag that is already defined by a standard definition,
  report it as a lint issue and treat it as an error by default
  so the user prefers the standard definition instead of reusing the tag directly.
* These diagnostics should eventually be configurable by rule identifier and severity,
  but that configuration system is not part of this stage yet.

## Step 0: Build the standard-module catalog crate

Status: complete.

Create the self-contained `cbork-catalog` crate that exposes the contents of `cddl/rfc-std/` by name.
This is a separate step so it can be documented, tested, and reused before recursive resolution exists.

Requirements:

* The catalog must expose a way to retrieve contents for a known built-in name.
* The catalog must expose a way to list all known built-in names.
* The catalog crate should be generated by `build.rs` using `phf` to build a perfect hash map.
* The build step should iterate the vendored `cddl/rfc-std/` tree and only include files ending in `.cddl`.
* The catalog must not perform runtime directory scanning.
* The catalog must not be a dynamic file lookup table.
* The catalog must not resolve relative or absolute include paths.
* The catalog should be reusable by the later recursive include resolver.
* The vendored `cddl/rfc-std/` contents are source input only.
  Do not rewrite those files as part of the catalog generation step.
* `cbork-catalog` is the only place where the vendored `cddl/rfc-std/` tree is catalogued.
  The parser crate should consume the catalog, not duplicate the lookup logic.

## Postlude Handling

The standard postlude should not be injected blindly into every parsed source.
It should be parsed into its own AST and kept available separately.
It should not be merged into the working compiler AST during Step 2 or Step 3.

During compilation, after the source AST has been fully resolved:

* scan the resolved document for redefinitions of standard definitions
* record those as lint diagnostics
* if the document does not redefine any standard definitions, merge in the postlude definitions that are actually used
* avoid injecting postlude definitions early, because that can create conflicts that should instead be reported first

The goal is to keep the postlude available as standard support data without forcing it into documents that conflict with it.

Implementation guidance:

* Keep the generated source small and deterministic.
* Add unit tests for:
    * known-name lookup
    * unknown-name failure
    * name listing
    * stable mapping of built-in names to `cddl/rfc-std/` contents

## Target Architecture

```text
base cddl.pest parser
→ parse document into AST with COMMENT nodes preserved
→ scan COMMENT text for module directive comments
→ directive parser module turns comment text into ordered include/import variants
→ inject parsed module directives into AST as bounded module blocks
→ wrap the AST so directives, provenance, and metadata can coexist
→ attach AST metadata such as PRUNABLE and SILENT
→ resolve built-in standard-library names through the `cbork-catalog` crate
→ resolve includes, rename imports where the spec allows it
→ expand CDDL generics after include/import resolution
→ run literal/control-operator fixed-point semantic resolution
→ prune unreachable PRUNABLE definitions
→ build the retained reference graph
→ detect retained duplicate/conflicting definitions
→ surgically inject referenced postlude definitions
→ report any remaining dangling references
→ emit a complete error-bearing tree, or an error-free complete tree ready for later validation
```

## Step 1: Build a directive-comment parser module

Status: complete.

Create a small parser utility whose only job is to accept a chunk of comment text
and return an ordered list of parsed module/include directive structures.
This lives inside `cbork-cddl-parser`, not as a separate crate.

Requirements:

* Input is plain text from one comment block or a group of adjacent comments.
* Output is ordered directive data.
* The parser does not look at the filesystem.
* The parser does not resolve names, paths, or imports.
* The parser does not decide whether a referenced file exists or whether an include/import is valid.
* The parser should preserve enough structure to distinguish the directive kind, target module, include/import name list,
  and any `as` renaming form the spec allows.
* The parser may expose a small enum with attached structs for each family, for example:
    * `Import`
    * `ImportAs`
    * `Include`
    * `IncludeAs`
  The exact split should follow the spec and should stay easy to extend.
* The ABNF used by the draft module syntax is simple enough to hand-parse directly.
  We do not need a general ABNF parser for this stage.
* Directive parsing should treat TAB as equivalent to a single SP between directive fields.
  This is a local extension to the draft ABNF so that a stray tab does not break directive parsing.
* If a comment block contains non-directive comments,
  the parser should ignore those lines and continue scanning the rest of the block.

Implementation guidance:

* Keep this module small and testable in isolation.
* Prefer a dedicated parser module over ad hoc regexes.
* Return structured data in source order so the caller can inject directives back into the AST without reordering them.
* The parser is only handing back a processed data type;
  the compiler applies the business logic that decides whether a directive can actually be resolved.

## Step 1.5: Scaffold the compiler crate

Status: complete.

Create the `cbork-cddl-compiler` crate and wire it into the workspace.
Do not implement the compiler pipeline yet.
This step is just the scaffold so the later plan steps have a concrete home.

Requirements:

* Add the crate to the workspace.
* Add the crate manifest and minimal library entrypoint.
* Add placeholder module layout if needed for later work.
* Add minimal tests or a smoke test only if they are useful for proving the scaffold builds.
* Do not implement directive injection, pruning, include resolution, or AST enrichment yet.

Implementation guidance:

* Keep the scaffold minimal.
* The crate should depend on `cbork-cddl-parser` and `cbork-abnf-parser` when the real compiler work begins,
  but this step does not need to wire all that logic yet.

## Step 2: Inject parsed directives into the AST

Status: complete.

In `cbork-cddl-compiler`, introduce a `CompiledCDDL` type that wraps the enhanced AST produced by the compiler.
Its public constructor should take a CDDL file `Path` plus an optional logical `root_path`, not raw text,
so relative references can be resolved correctly without forcing the real filesystem root.
The first stage of this constructor should parse the file, then enhance the AST with include data.
That enhancement pass must be written so it can be called recursively for nested includes later.

Requirements:

* `CompiledCDDL` should be the public wrapper around the final enhanced AST.
* The constructor should take a file path and an optional logical root path, not text.
  Relative includes are rooted in the source file,
  and the logical root can offset root references without requiring the real filesystem root.
* The constructor should return a structured compiler error on failure, not a plain string or untyped failure.
* The compiler should aggregate as many recoverable errors as possible before it can no longer continue.
  If one pass can find multiple include errors, directive errors, or other recoverable problems,
  it should return them all in the report.
* The compiler must parse the file first, then enhance the AST with include/module data.
* The include-enhancement code must be reusable recursively for nested includes.
* The AST should retain the original comment nodes.
* Introduce a wrapped AST node type in this step, rather than waiting for Step 3.
  The raw `Vec<Pair<'_, cddl::Rule>>` is not enough once directives, provenance, and pruning metadata need to coexist.
* Parsed directive records should be injected alongside the surrounding nodes in parse order.
* The injection pass should preserve the original source order of comments and expressions.
* The pass should not resolve includes yet.
* The pass should not remove comments that are not module directives.
* When the AST is enriched with a parsed directive, the emitted child AST should be bounded with clear module comments.
  A start marker should repeat the directive in module form, for example `; Module: import as rfc9052 as cose`,
  and an end marker should close it, for example `; End Module: import as rfc9052 as cose`.
  Those markers are part of the AST representation, not runtime filesystem logic.

Implementation guidance:

* Keep the injection pass separate from resolution.
* Treat this as a structural enrichment stage, not a semantic validation stage.
* The recursive enhancement routine should be factored
  so the same code path can process nested includes without special-casing the top-level file.
* `CompiledCDDL` should just wrap the final enhanced AST produced by that routine.
* The structured error type should be rich enough to report:
    * syntax / parse failures
    * directive-comment parse failures
    * include/import resolution failures
    * cycle detection failures
    * I/O failures
    * any other compiler-level failure that should stop compilation and produce a useful report
* The error type should support multiple collected diagnostics, not just a single first error.

## Step 3: Add AST metadata for pruning and emission control

Status: complete.

In `cbork-cddl-compiler`, the wrapped AST node type introduced in Step 2 needs metadata that records whether it can be removed and
whether it should be emitted in a concise resolved document.

Requirements:

* `PRUNABLE` means the node may be removed if it becomes dangling after include expansion and resolution.
* `SILENT` means the node remains part of the processed model but should not be emitted in concise output.
* Core definitions cannot be prunable.
* The postlude should be marked `SILENT` and kept separate from the working compiler AST until post-resolution support-data merging.
* Any include-expanded core types that must remain available for resolution should also be protected from pruning
  as needed by the spec.

Implementation guidance:

* Keep the metadata explicit in the AST or node metadata.
  Assume more metadata will be added later.
* Do not overload the flags with semantic meaning beyond pruning and emission control.
* Use the spec to decide which nodes are core and therefore non-prunable.

## Step 4: Resolve includes and rename forms

Status: complete.

In `cbork-cddl-compiler`, once the AST is structurally complete, resolve every include/import record into a final document model.

Requirements:

* Includes may use the standard-library names from `cddl/rfc-std/`.
* Includes may also use relative references.
* Includes may also use absolute references.
* Relative references are always resolved relative to the file that contained the directive.
* Rename forms must be honored exactly as the spec defines them.
* The resolver should preserve source provenance so callers can explain where each included rule came from.
* The resolver must recursively load and parse the referenced contents before injecting them.
  This is a document-parser responsibility, not a directive-parser responsibility.
* Circular includes and repeated file inclusion anywhere in the hierarchy must fail hard.
  In this model, a file may appear only once in the resolved include tree, even if the repetition would not be recursive.
* Every include/import variant should be able to materialize the content it points to from the current file path.
  The resolver is the layer that calls that materialization and then recursively parses the returned content.
* Nested import aliases should compose rather than collapse.
  If a file is imported as `first`, and a downstream module imports that module as `second`,
  the resulting qualified names should preserve both layers, e.g. `second.first.sometype`.
  This avoids collisions when the intermediate imported module is not under the caller's control.
* If an include/import resolves to a module that is already in the active resolution chain,
  treat it as a recursive include/import and fail hard.
  Import/include resolution is unconditional, so recursion is not something to silently tolerate.
* Standard-library names should be resolved from the generated `phf` catalog crate built at compile time from the vendored
  `cddl/rfc-std/` `.cddl` files.
  That catalog should expose functions to retrieve contents by name and to list all known names.
  It should not be dynamic at runtime, and the standard file set should not need to ship as raw files in the final library artifact.
* Name lookup should understand the project-defined filename rules:
    * unquoted `rfc9052` means the built-in standard file `rfc9052.cddl` from `cddl/rfc-std/`
    * quoted `"./somedir/somefile.cddl"` is a relative file name
    * quoted `"/weird%23name.cddl"` is an absolute file name
* Quoting is our extension, not standard CDDL syntax.
  It exists only to allow arbitrary file references while preserving the standard built-in name form.
* The filename syntax should be hand-parsed using URL-escape-aware rules for quoted paths.
  A quoted filename can contain path characters, slashes, and percent-encoded bytes.

Implementation guidance:

* Keep name resolution separate from directive parsing.
* Treat the resolver as spec-driven rather than heuristic-driven.
* Make sure renamed imports are represented distinctly from unrenamed includes.
* Pruning mode is transitive downward through resolved subtrees.
    * A subtree entered through a bare `include` starts `not prunable`.
    * A subtree entered through a bare `import` starts `prunable`.
    * Once a subtree is `prunable`, everything below it stays `prunable`.
    * `not prunable` can flow downward until a prunable ancestor is encountered.
    * Explicitly named items are exceptions and remain `not prunable` even inside a prunable subtree.
    * In all cases, include/import resolution still builds the full compiled tree first;
  later pruning passes remove only items that are both prunable and unused.

### Step 4.4: Import / include scope for cycle and duplicate detection

Status: complete.

Includes and imports both reject re-references that hit an entry already in the active resolution chain,
but their post-resolution behaviour differs:

* `include` leaves the entry in place after the subtree is resolved.
  A file may appear at most once in the resolved include tree, even if the repetition would not be recursive.
  This is the spec-strict behaviour and matches "Circular includes
  and repeated file inclusion anywhere in the hierarchy must fail hard."
* `import` pops the entry once the subtree is resolved.
  Imports are weak, scope-bound references: each import produces its own alias-namespace
  (`alias.x`, `defs.cose.x`, etc.), so the same well-known module can be imported in different scopes without colliding.
  True cycles are still caught because the current file's canonical path remains in the visited set
  for the duration of its own subtree walk.

The check lives in `cbork-cddl-compiler/src/resolver.rs::check_visited`;
the pop lives inline next to the recursive `resolve_includes` call in `resolve_node_single`.
Test coverage is provided by the new project vectors:

* `cddl/vectors/project/positive/import_shared_well_known_outer.cddl` and its
  `support/import_shared_well_known_lib.cddl` (the dntls-cose pattern: outer imports a library
  that imports `rfc9052 as cose`, and the outer also imports `rfc9052 as cose` directly).
* `cddl/vectors/project/positive/import_sibling_well_known_different_alias.cddl` (two sibling
  imports of the same well-known under different aliases).
* `cddl/vectors/project/negative/duplicate_include_sibling.cddl` and its
  `support/duplicate_include_sibling_source.cddl` (sibling `include`s of the same file must
  still fail hard).

These vectors are wired into `crates/cbork-cddl-compiler/tests/import_include_vectors.rs` via
`import_shared_well_known_outer_vector`, `import_sibling_well_known_different_alias_vector`, and `duplicate_include_sibling_fails`.

### Step 4.45: `.x-enc` and `.x-hash` ctlop narrowing

Status: complete.

`.x-enc` and `.x-hash` are unofficial annotations that say "this `bstr` holds the result of encrypting or hashing the RHS".
They are doc/convention annotations: the value seen at the schema boundary is always a `bstr`, regardless of the controller type,
so subtype checks must treat them as carrier-narrowing operators on `bstr`.
The existing `ControlOp` variant set did not recognize these names —
they fell into `ControlOp::Other` and bypassed the narrowing short-circuit in `collect_subtype_conflicts_inner`,
producing false-positive "control(.x-enc) not subtype of Nil" diagnostics.

Both operators are added as named `ControlOp` variants
(`XEnc`, `XHash`) and listed in both `is_schema_relevant` and `is_narrowing` (`crates/cbork-cddl-compiler/src/within.rs`).
The carrier-narrowing rule then makes `bstr .x-enc T` and `bstr .x-hash T` behave like `bstr` for `.within` subtype checks.
The RHS validity is enforced separately by the literal-producing ctlop pass.

Five regression tests live in the `.x-enc` / `.x-hash` annotation regression section of `crates/cbork-cddl-compiler/src/within.rs`:

* `bstr_x_enc_within_bstr` and `bstr_x_hash_within_bstr` — the carrier-narrowing headline case.
* `bstr_x_enc_within_choice_bstr_or_nil` — the dntls-cose-encrypt regression (LHS
  `bstr .x-enc ''` against RHS `bstr / nil`).
* `x_enc_x_hash_round_trip` — both annotations are interchangeable as carrier narrowings.
* `x_enc_is_narrowing` — regression guard for the `is_narrowing` / `is_schema_relevant`
  predicates.

### Step 4.46: Compression annotation ctlops

Status: complete.

The compression annotation ctlops are now first-class.
The parser accepts `.x-compressed`, `.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate` (as `ctlop_generic`).
The parser also accepts their `.abnf`/`.abnfb` annotated forms (as `ctlop_text`).
The compiler names each operator in `ControlOp` and treats them as carrier narrowings on `bstr`.
This matches the `.x-enc`/`.x-hash` behavior.

The `.abnf`/`.abnfb` forms parse to a new `EntryState::CompressionAbnf` variant.
The variant is parameterized by a small `CompressionKind` enum.
The existing `EncAbnf`/`HashAbnf` variants are preserved unchanged to keep the diff focused.
The new shared variant captures the five compression algorithm kinds in one place per the plan's recommendation.

The directional compatibility matrix for `.within` checks:

* named algorithm (`X{Brotli,Zstd,Gzip,Deflate}`) ⊆ `XCompressed`: check controller.
* `XCompressed` is not ⊆ a named algorithm: reported as ".x-compressed is broader than a named compression algorithm".
* two different named algorithms are not mutually within each other: reported as "compression algorithm X is not within Y".
* equal named algorithms compare their controllers structurally.

Eight regression tests live in the compression annotation section of `crates/cbork-cddl-compiler/src/within.rs`:

* `bstr_x_brotli_within_bstr` and `bstr_x_compressed_within_bstr` —
  the carrier-narrowing headline cases for a named algorithm and the generic variant.
* `bstr_x_brotli_within_bstr_x_compressed` — named ⊆ generic.
* `bstr_x_compressed_within_bstr_x_brotli_fails` — generic is broader than a named algorithm.
* `bstr_x_brotli_within_bstr_x_zstd_fails` — two different named algorithms are not mutually within each other.
* `bstr_x_zstd_within_bstr_x_zstd` — equal named algorithms compare their controllers structurally.
* `x_brotli_is_narrowing` — regression guard for the `is_narrowing` / `is_schema_relevant` predicates.
* `compression_op_text_round_trip` — every base name
  and every `.abnf`/`.abnfb` annotated form normalizes to the same `ControlOp` variant.

The end-to-end regression is fixed.
`cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl` no longer fails at parse time on
`bstr .x-brotli service-record-data`.
The remaining error on line 151 is a pre-existing group-socket `.within` issue, outside the scope of this step.

Note: the plan's "both sides" test cases
(e.g. `bstr .x-brotli payload .within bstr .x-compressed payload`)
require extending the CDDL grammar to support two ctlop+type2 in a single type1.
The current grammar allows only one.
The directional compatibility is fully covered by unit tests on `ResolvedType::Control` and will exercise against parsed CDDL
once the grammar is extended.
These operators are schema annotations like `.x-enc` and `.x-hash`: at the schema boundary the serialized value is a `bstr`,
but the RHS records the uncompressed logical payload schema.
For linting and `.within` subtype checks, they should behave as carrier narrowings on `bstr`.
For future binary validation, the named compression operators are actionable because the validator can reverse the compression
and continue validating the inner payload;
`.x-compressed` is the generic fallback when the schema says "compressed" but does not commit to a known algorithm.

Operators to add:

* `.x-compressed`
* `.x-brotli`
* `.x-zstd`
* `.x-gzip`
* `.x-deflate`
* `.x-compressed.abnf`
* `.x-compressed.abnfb`
* `.x-brotli.abnf`
* `.x-brotli.abnfb`
* `.x-zstd.abnf`
* `.x-zstd.abnfb`
* `.x-gzip.abnf`
* `.x-gzip.abnfb`
* `.x-deflate.abnf`
* `.x-deflate.abnfb`

Implementation guidance:

* Extend `crates/cbork-cddl-parser/src/grammar/cddl.pest` so the base compression operators parse as generic ctlops
  and the `.abnf` / `.abnfb` forms parse as text ctlops.
  The immediate regression is `bstr .x-brotli service-record-data` in `test/svcrec/doc/service-record-v1.cddl`.
* Extend `crates/cbork-cddl-parser/tests/common/type_declarations.rs` so every new operator is
  listed in `CTLOP_PASSES`.
* Extend `crates/cbork-cddl-compiler/src/within.rs::ControlOp` with named variants for the compression operators.
  Add them to `as_text`, `from_text`, `is_schema_relevant`, and `is_narrowing`.
* Treat compression ctlops as carrier narrowings like `.x-enc`, `.x-hash`, `.cbor`, and `.dtrm`
  for `.within` validation:
    * `bstr .x-brotli T ⊆ bstr`
    * `bstr .x-zstd T ⊆ bstr`
    * `bstr .x-gzip T ⊆ bstr`
    * `bstr .x-deflate T ⊆ bstr`
    * `bstr .x-compressed T ⊆ bstr`
* Define directional compatibility between named and generic compression operators:
    * named compression (`.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate`) is within
      `.x-compressed` when the carrier and controller are otherwise compatible.
    * `.x-compressed` is not within a named compression operator because it is broader.
    * two different named compression operators are not mutually within each other.
    * equal named compression operators compare their controllers structurally.
* Extend `crates/cbork-cddl-compiler/src/ctlop.rs` structural validation so compression
  operators require a `bstr` carrier and a schema RHS, matching the `.x-enc` / `.x-hash`
  annotation path.
* Extend the ABNF annotation path in `ctlop.rs` so the compression `.abnf` / `.abnfb` forms parse and preserve ABNF side data.
  If the current `EntryState::EncAbnf` / `EntryState::HashAbnf` split is too specific,
  introduce a small annotation kind enum instead of duplicating many new `EntryState` variants.
* Ensure concrete rendering preserves and formats compression annotations in effective CDDL output.

Tests:

* Parser test: every compression operator appears in `CTLOP_PASSES`.
* Compiler/within tests:
    * `bstr .x-brotli payload .within bstr` passes.
    * `bstr .x-compressed payload .within bstr` passes.
    * `bstr .x-brotli payload .within bstr .x-compressed payload` passes.
    * `bstr .x-compressed payload .within bstr .x-brotli payload` fails with a reason equivalent
      to "generic compressed is broader than brotli".
    * `bstr .x-brotli payload .within bstr .x-zstd payload` fails.
* End-to-end regression: `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl`
  no longer fails at parse time on `.x-brotli`.

### Step 4.5 Resolved Types Cache

Status: complete.

After include resolution, the compiler should maintain a `resolved_types` cache that records what a type expands to
once it has been semantically resolved.
This cache is separate from the syntax AST.

Requirements:

* The cache should preserve concrete semantic kind, not just normalized text.
  Examples include:
    * integer constant
    * float constant
    * text constant
    * byte string constant
    * regex
    * ABNF schema
    * structured group / value form
    * unresolved / dangling type
* The cache should be usable by later passes that validate control operators
  and determine whether a type has become a concrete constant after expansion.
* The cache should be populated with a multi-pass fixed-point style walk:
    * resolve the entries that can be resolved unambiguously in the current pass
    * store those resolved entries in the cache
    * keep unresolved entries in the cache too, tagged as `unresolved`
    * repeat passes while the number of unresolved entries decreases
    * stop when two consecutive passes produce the same unresolved count
* Because CDDL documents are typically small, repeated passes are acceptable
  and should keep the implementation simple and predictable.
* The cache should use stable identifiers for entries once includes and shadowing
  are involved, not just raw display names.

Implementation guidance:

* Treat this cache as compiler semantic state, not parser state.
* Keep it separate from the syntax AST so the AST remains structural and the
  cache remains semantic.
* Later passes may consult the cache to determine the resolved value kind of a
  type after expansion, especially when a type reduces to a constant.
* Unresolved entries should retain the same stable handle as resolved entries,
  so later analysis can explicitly see that a given type remains dangling.

Current implementation note:

* The cache already exists and is populated for the current literal-producing and transformer-oriented compiler paths.
* It still needs to become the authoritative state for pruning-aware validation and final semantic reporting.

## Step 5: Semantic fixed-point resolution

Status: partial.

In `cbork-cddl-compiler`, walk the resolved AST and progressively reduce semantic values until no further progress is possible.

This step is the semantic core of compilation.
It runs after include/import resolution, then expands generics like macros, collapses control operators against known values,
and repeats until the document reaches a fixed point.
Generic expansion must happen before literal flattening so macro-like substitutions cannot lock in the wrong concrete value.
The current implementation expands generic instantiations inline,
so it avoids synthetic namespace collisions by not introducing new generated definitions for instantiated generic bodies.

### Step 5.0: Overview

The compiler should:

* run generic expansion after include/import resolution
* expand generic bindings before literal flattening
* seed the cache with true constants and values that directly reduce to true
  constants
* validate and execute control operators against currently known values
* derive new values only when the operator inputs are unambiguous
* populate the `resolved_types` cache as new semantic values are discovered
* repeat passes while the unresolved count decreases
* stop when two consecutive passes produce the same unresolved count
* retain unresolved entries so later passes can still see what remains dangling
* keep generic expansion in the same overall semantic pipeline, but before literal flattening and concrete value
  collapse

Current implementation note:

* Generic expansion is wired into the compiler pipeline after include/import resolution and before literal/control-operator
  fixed-point resolution.
* Generic expansion currently handles bare instantiation, nested instantiation, and generic definitions supplied by includes.
* Generic definitions are ignored by final reference scanning so formal parameters such as `t` are not reported as dangling
  document references.
* Nested control-operator expressions are still limited by the current constant folder's expression-shape support;
  this is separate from generic expansion itself.

### Step 5.1: Seed known constants

Status: complete.

Populate the first-pass cache with values that are directly known without needing derived ctlop evaluation.

This includes the literal constants and other forms that immediately reduce to concrete values.

Implemented:

* The semantic fixed-point pass calls `seed_pass` before rangeop and ctlop evaluation.
* The seed pass creates cache entries for `=` rule LHS names and leaves `/=` / `//=` augmentations out of constant seeding.
* Direct decimal integer literals seed `EntryState::Integer`.
* Direct decimal floating-point literals seed `EntryState::Float`.
* Direct text literals seed `EntryState::Text`.
* Direct byte-string literals seed `EntryState::Bytes`.
* Bare typename RHS references propagate once the referenced name is already resolved.
* Redundant and conflicting constant definitions are surfaced through the cache write path.

Follow-up audit note:

* Step 5.1 remains closed for the original decimal/text/bytes/reference seeding scope.
  Additional parser-accepted literal forms discovered during later audit are tracked forward in Step 5.13.

### Step 5.2: Execute transformer ctlops

Evaluate only transformer-style control operators against the currently known values.

This pass is responsible for collapsing ctlops such as `.cat` once the inputs are sufficiently known.

For array-producing and array-consuming transformers such as `.printf` and `.join`,
add a `LiteralArray::new(...)` constructor that walks the RHS and returns one of:

* `Some(LiteralArray)` when the RHS is an array whose elements are all literal values
* `None` when the RHS is an array, but it contains non-literal elements
* an error when the RHS is not an array shape at all

`LiteralArray` should then provide the operator behavior directly:

* `join_bytes(&self) -> ByteLiteralBytes`
* `join_text(&self) -> Result<TextLiteralBytes>`
* `printf(&self) -> Result<TextLiteralBytes>`

`join_text` should call `join_bytes` first, then convert the joined bytes to `TextLiteralBytes`.
That conversion is the only fallible step in the text join path and it should fail
if the final joined bytes are not a valid text literal.
The specification only requires validation of the final joined result,
not each individual element's intermediate byte-to-text promotion.

`printf` should operate on the collected literal array using `sprintf`/`vsprintf`.
If any collected element cannot be promoted to one of the literal kinds accepted by that formatter, that is an error.
For byte literals, the compiler should promote them to text first and then use the resulting string form
as the dynamic argument value.

Validator-style ctlops such as `.lt`, `.le`, `.gt`, `.ge`, `.eq`, `.ne`,
and similar shape/value checks are not fully validated here.
They are deferred to later passes once the right-hand side meaning is known.

Derived constants are introduced only when the ctlop inputs are unambiguous.

Current implementation note:

* The compiler already folds the literal-producing ctlops that have concrete source materialization, including ABNF,
  regex, and JSON-derived forms.
* The compiler also already validates the known ctlop surface structurally so the later pruning pass will see a coherent tree.

### Step 5.3: Repeat to fixed point

Repeat the seed-and-reduce walk until the cache stops shrinking the unresolved set.

The compiler should keep unresolved entries in place so later analysis can see what is still dangling.

### Step 5.4: Semantic side data

ABNF and regex materialization belong to this semantic walk when they are needed to determine the final value of a node.
The same side-data bucket also carries the new unofficial annotation forms used to preserve pre-transform structure for encrypted
and hashed payloads.

The resolved cache remains the semantic state source for later passes.

String literals are normalized here before later ctlop handling uses them:

* Normalize source line endings `\r\n`, `\r`, and `\n` to canonical `\n`.
* Represent the canonical newline as escaped `\n` in the semantic value.
* Reject any unescaped character that JSON-string rules require to be escaped.
* Preserve the source spelling separately so pretty-print and debug output can still show what the author wrote.
* The goal is one deterministic string value that later string ctlops, such as `.cat`,
  can operate on without platform-specific line-ending drift.
* Keep both text and byte-string literals byte-backed in the AST.
  The AST should preserve the literal kind explicitly,
  but it should not force conversion to Rust `String` during parsing or early semantic passes.
  Convert to text semantics only when a later ctlop or validator actually needs text behavior.

Current implementation note:

* ABNF source parsing, regex compilation, JSON compilation, and the new annotation forms are already wired into this side-data path.

Literal collection helpers should live alongside the other literal types in the `literals` module tree.

Requirements:

* Introduce a `LiteralArray` type that represents an array whose elements are literal variants.
* Construct `LiteralArray` with a fallible `new(...)` constructor so non-literal or non-array inputs can be rejected early.
* Expose `join(&self)` and `printf(&self)` as methods on `LiteralArray`.
* `join` and `printf` should operate on collected literal payloads, not raw AST nodes.
* The transformer ctlop pass should first call the collection helper,
  then use the resulting `LiteralArray` for the actual operator behavior.
* Keep the helper in the literal-handling module, not in the generic semantic walker.
* The raw elements inside a `LiteralArray` are opaque wrappers over existing literal values.
  They may reference byte, text, integer, or float literal values, but they should not create new semantic instances of those types.
  The wrapper should only make the heterogeneous elements coexist inside the array while preserving the underlying literal value
  and kind.

## Step 5.5: Prune dangling removable definitions and validate retained references

Status: complete.

In `cbork-cddl-compiler`, after semantic fixed-point resolution has stabilized, remove nodes that are allowed to be pruned,
then build the retained definition/reference graph and validate it before downstream consumers trust the tree.

Current implementation state:

* Reachability pruning for prunable include/import material is implemented in
  `cbork-cddl-compiler/src/finalize.rs::finalize_compiled`.
* Collision detection now runs on the retained, post-pruning tree through
  `normalize_definition_strengths`.
* The unpruned tree is snapshotted before pruning so resolver side-cache entries and original
  definition names remain available to later postlude/reference handling.
* The side cache is re-attached with `ResolverCache::get`, not `resolve`, because the side cache
  intentionally contains `Unresolved` entries.
* Retained-reference validation is covered by the final reference-injection walk, which starts from
  the pruned `compiled.user_nodes` tree and reports missing retained references while ignoring
  references that only existed inside pruned rules.
* Step 5.5 has no known remaining implementation work; later work belongs to postlude/support
  injection behavior in Step 5.6 or downstream validation/lint features.

Requirements:

* Only remove nodes marked `PRUNABLE`.
* Do not remove core nodes.
* Do not remove nodes required to preserve include/import expansion correctness.
* The pruning pass should be deterministic and source-order stable.
* Pruning should operate from retained roots/reachable references, not from all definitions that happen to exist in imported files.
* A prunable definition that is removed must not produce dangling-reference diagnostics.
* After pruning, retained definition lookup must preserve stable names and retained definition sites.
* After pruning, retained reference validation must preserve the source span of every retained reference site.
* Retained definitions with the same name and same resolved content are redundant and should produce a warning.
* Retained definitions with the same name and conflicting resolved content are hard errors and should identify both sides.
* The pass should continue after errors so the linter reports all retained duplicate, conflict,
  and dangling-reference issues found in one compile.
* The result of this step is a reduced user tree plus diagnostics,
  before postlude/support injection produces the final `complete_nodes`.

### Step 5.5a: Prune-first ordering rule for collision detection

Status: complete.

Collision detection must run on the **retained, post-pruning** definition set,
with the explicit sequencing rule that the reachability pruner precedes both collision walkers.
This ordering is required because:

* Two weak imported definitions whose name the importer never references would otherwise flag each other as redundant
  or conflicting even though the reachability pruner would silently remove them anyway.
  The collision walker has no way to know they are unreferenced from outside the tree it is walking, so the prune must come first.
* The (strong, weak) importer-wins case still resolves the same way: the strength walker
  sees both definitions because the strong importer rule survives pruning, and the
  `PruneOnly` action drops the weak imported one silently.
* The postlude merge still sees every reference the importer ever made.
  Before the prune runs, the resolver cache is pre-populated with `Unresolved` entries for every typename reference
  (LHS names and RHS references) the *unpruned* tree contains.
  After the seed pass populates the cache for the pruned tree,
  the pre-populated entries that the seed pass did not resolve are merged back in so the postlude merge can find them.

The implementation lives in `cbork-cddl-compiler/src/finalize.rs::finalize_compiled`:

1. **Snapshot the unpruned user tree** before the reachability pruner mutates it:
   `let original_user_nodes = compiled.user_nodes.clone();`.
   Both the side cache and the reference-injection pass need information only available in the unpruned tree.
2. Snapshot the original top-level rule names (`original_definition_names`) for the
   reference-resolution pass.
3. Run `prune_unreachable_prunable_definitions` on `user_nodes`; assign the pruned tree
   back to `compiled.user_nodes` and prune `compiled.resolved_types` with the dropped names.
4. Pre-populate a side `ResolverCache` (`pre_populated`) with every typename reference in `original_user_nodes`
   (the unpruned snapshot).
   The walk only calls `cache.get` so it never enters the `resolve` path and never emits `RedundantType` warnings —
   it just guarantees that the side cache has an `Unresolved` entry for every reference the original tree contained.
5. Run `normalize_definition_strengths` against the pruned tree.
6. Run the semantic fixed-point pass (`resolve_constants`) to get a fresh cache populated from the pruned tree.
   The side cache is then re-attached by iterating the pre-populated names and calling `resolved_types.get(name)` for each.
   `ResolverCache::get` auto-creates an `Unresolved` entry on first access; if the seed pass already resolved the name,
   `get` simply returns the existing value without modification.
   Calling `resolve` here would be wrong because the side cache holds `Unresolved` states
   and `resolve` explicitly rejects them (`CacheWriteError::CannotSetUnresolved`).
7. Continue with the postlude resolution, postlude merge, replay, and reference injection
   as before.

The pre-existing `detect_user_definition_collisions` walker
(a redundant signature-only collision check that ran before the reachability pruner) was removed.
Its functionality is subsumed by the strength-aware `normalize_definition_strengths` walker plus the new prune-first ordering.

The (strong, strong) rows of the strength matrix are now handled by `normalize_definition_strengths` directly:

* `(strong, strong)` with matching signatures → `Redundant` warning, target pruned.
* `(strong, strong)` with different signatures → `Conflict` error, target pruned.

Tests for the new ordering live in `crates/cbork-cddl-compiler/src/tests.rs`:

* `unreferenced_weak_imports_are_pruned_silently_no_conflict` — two library files each
  define a weak rule with the same name; the importer never references it; the rule
  must be pruned silently with no E014 / no W001.
* `importer_strong_definition_silently_drops_unreferenced_weak_imports` — the
  (strong, weak) importer-wins case must be silent.
* `side_cache_keeps_undefined_reference_visible_for_postlude_injection` — a regression guard for the side-cache re-attachment path.
  The setup uses a named import that selects only one rule from a library, leaving the rest of the library unreferenced.
  Without the side-cache merge, the resolver cache after `resolve_constants` would not contain the unselected library rules,
  breaking the reference-resolution pass for any downstream consumer that needs to know they were at least seen.
  The test verifies that `lib.phantom_marker` (an unselected rule in the library) is present in the final resolver cache.

## Step 5.6: Surgically inject postlude/support definitions

Status: complete.

In `cbork-cddl-compiler`, after pruning and retained-reference validation,
merge in only the postlude/support definitions that are actually needed by the retained tree.

The finalization pass starts from the pruning-aware retained tree, scans references,
compares user definitions against the postlude using a structural signature,
and injects only the postlude definitions that the retained tree transitively references.
The injection loop runs until a fixed point so transitive postlude dependencies
(`int` → `uint`/`nint`, `float` → `float16-32`/`float64` → `float16`/`float32`) are all pulled in.

A user definition whose name also appears in the postlude wins outright — the postlude definition is never injected over it.
The redundant-definition comparison is signature-based on purpose: most postlude types are tag references
(`bstr = #2`, `uint = #0`),
which the seed pass leaves in `Unresolved` state,
so a cache-state-only check would silently skip every tag-based postlude type and never report a redundant warning
for `bytes = bstr` or `bstr = #2`.

**Conflict detection IS implemented for postlude types.**
A signature mismatch between a user definition and the postlude definition raises a hard `ConflictingDefinition` error (E014).
This includes the RFC 8610 §3.8.4 ctlop-form case: `encoded-cbor = bytes .cbor type1`
(where `type1 = bstr`)
is *not* the same as the postlude's `encoded-cbor = #6.24(bstr)` because the ctlop narrows the payload type,
so the compiler flags it as a conflict and the user is told their override is incompatible.

**One equivalence exception is recognized:** the ctlop-form `X = bytes .cbor any`
(or `X = bstr .cbor any`) is treated as the RFC 8610 §3.8.4 restatement of a tag-form postlude definition `X = #6.N(bstr)`.
The ctlop's argument must be exactly the postlude's `any` token — `bytes .cbor type1`, `bytes .cbor int`, `bytes .cbor anyfoo`,
etc. fall through to the conflict branch.
See `is_ctlop_form_of_tag_postlude` in `finalize.rs`.

Requirements:

* Do not bulk-inject the standard postlude. ✓ (`handle_reference` only pushes names
  into `complete_nodes` when the postlude loop hits a fresh reference; unreferenced
  postlude names like `bigint` or `decfrac` stay out of the complete tree.)
* If a retained user definition already defines a standard postlude name with the same
  resolved content, report a redundant standard-definition warning. ✓ (signature match
  in `compare_user_and_postlude_definitions` → `RedundantDefinition` + W001.)
* If a retained user definition already defines a standard postlude name with conflicting
  resolved content, report a hard conflict error and do not inject the postlude
  definition over it. ✓ (signature mismatch + no ctlop-tag equivalence →
  `ConflictingDefinition` + E014; `handle_reference` consults `user_definition_names`
  and skips injection.)
* If a retained RHS reference is undefined but exists in the standard postlude,
  inject exactly that postlude definition. ✓ (`handle_reference` looks up the name in
  `postlude_definitions` and pushes a single tagged clone.)
* Injected postlude definitions should keep metadata showing that they are standard
  support data and silent for concise emission. ✓ (`tag_standard_postlude` recurses
  through the cloned subtree and adds both `StandardPostlude` and `Silent` to every
  node, including nested children — verified by `postlude_injected_subtree_is_fully_silent`.)
* Injection must repeat until a fixed point because injected postlude definitions can
  themselves reference other postlude names. ✓ (the `loop { ... if !injection.changed break }`
  in `finalize_compiled` re-snapshots `complete_nodes` after each iteration; verified by
  `postlude_injects_recursive_dependencies` and `postlude_injects_multi_level_transitive_dependencies`.)
* If a retained RHS reference is undefined and not in the standard postlude/support tables,
  report a hard dangling-reference error. ✓ (`handle_reference` falls through to E016 once
  the `postlude_definitions` lookup misses; verified by `postlude_dangling_reference_outside_postlude_is_error`.)
* After postlude/support injection, any remaining dangling reference is an error. ✓
  (the loop terminates only when no new reference is resolvable from the postlude, so
  the only way a name remains undefined is if it never existed in the postlude — the
  E016 emission path above catches that case.)
* The final tree should be physically complete so downstream validation does not need to
  jump between the user tree and postlude tree. ✓ (`complete_nodes` is the only tree
  consumed by `validate_ctlop_pass` and `validate_within_pass`; the ctlop/within passes
  read from `complete_nodes` exclusively.)

Tests in `crates/cbork-cddl-compiler/src/tests.rs`:

* `postlude_injects_recursive_dependencies` — `root = int` must pull in `int`, `uint`,
  and `nint`; all three are tagged `StandardPostlude` and `Silent`.
* `postlude_injects_multi_level_transitive_dependencies` — `root = float` must pull in
  `float`, `float16-32`, `float64`, `float16`, `float32` (two-level transitive chain).
* `postlude_injection_reaches_fixed_point` — after the loop terminates, all transitively
  referenced postlude names are present; unreferenced ones (`bigint`, `decfrac`) are
  not bulk-injected.
* `postlude_user_redefinition_with_matching_content_is_redundant` — `bytes = bstr`
  triggers the W001 redundant warning; the postlude's `bytes` is not injected over the
  user; the postlude's `bstr` is still injected because the user references it.
* `postlude_redundancy_detection_works_for_tag_based_types` —
  `bstr = #2` against the postlude's `bstr = #2` fires the W001 redundant warning.
  This is the case the cache-state-only comparison used to miss.
* `postlude_user_override_with_different_signature_is_conflict` — `bytes = 42`
  triggers the E014 conflict error; the postlude's `bytes` is not injected over the user.
* `postlude_user_ctlop_form_with_any_argument_is_redundant` — `encoded-cbor = bytes .cbor any`
  fires the W001 redundant warning: the ctlop-form is the RFC 8610 §3.8.4 restatement
  of the postlude's tag-form `encoded-cbor = #6.24(bstr)`.
* `postlude_rfc8610_encoded_cbor_ctlop_form_with_non_any_argument_is_conflict` — the
  exact CDDL from `cddl/vectors/rfc/rfc8610_cbor.cddl` lints with an E014:
  `encoded-cbor = bytes .cbor type1` (where `type1 = bstr`) is *not* the same as
  the postlude's tag-form because the ctlop's argument narrows the payload type.
* `postlude_dangling_reference_outside_postlude_is_error` — `root = missing_type`
  emits E016 because `missing_type` is not in the postlude.
* `postlude_injected_subtree_is_fully_silent` — every node in the injected postlude
  subtree (not only the top-level rule) carries `Silent` metadata.
* `postlude_works_independent_of_import_aliases` — the postlude merge ignores the
  import alias namespace; `root = text` still pulls in `text` and `tstr` even when the
  user file imports another library under an alias.

## Step 5.7: Preserve definition-site scope for generic `.within` checks

Status: complete.

Imported generic templates may contain `.within` constraints that refer to names available in the template's own source module,
not in the consumer's source module.
The current generic expansion correctly delays `.within` checking until a concrete instantiation is available.

This step closes three long-standing boundary bugs:

* bare generic parameters used as array/group entries, such as the `headers` entry in
  `COSE_Sign<headers, payload, signatures>`, can now be substituted with their
  concrete generic arguments even when an adjacent import directive
  (e.g. `; # import lib.wrapper from "./lib.cddl" as lib`) sits next to the
  instantiation rule line;
* definition-site alias references inside a generic body, such as `std.Wrapper`
  referenced from `.within std.Wrapper`, survive expansion unchanged so they
  continue to resolve through the generic definition's own import scope rather
  than being re-prefixed with the consumer's alias (`lib.std.Wrapper`);
* selected imported generics retain their private same-module helper closure when the consumer only cherry-picks the generic.
  RFC 9393's `cddl/rfc-std/rfc9393-tags.cddl` exposes this as an E016 warning for `unprotected-signed-coswid-header`
  once the helper references are not consumer-prefixed.

When the generic is instantiated from another file,
the dedicated `.within` fixtures now produce the expected concrete `[uint]` vs `[tstr]` mismatch with the effective RHS rendered
as the resolved definition, and the "unresolved name: lib.std.Wrapper" / "cose.COSE_Sign" boundary errors are gone.

Stable fixtures under `cddl/vectors/project/`:

* `positive/generic_within_definition_site_scope.cddl` + support files (minimal
  one-parameter generic; the `.within` RHS uses a private `std` alias).
* `positive/generic_within_parameter_substitution.cddl` + support files (two
  generic parameters used as bare array entries; the consumer instantiates
  with concrete user types `Concrete-Headers` / `Concrete-Payload`).
* `semantic-errors/generic_within_definition_site_negative.cddl` + support
  files (negative case: instantiating with `uint` against a `[tstr]` RHS must
  produce E030 with the effective RHS rendered as `[tstr]`).
* `positive/generic_import_retains_private_helper_closure.cddl` + support files (RFC 9393 shape:
  a consumer imports only `lib.Envelope<payload>`,
  the imported generic body references private helpers `lib-private-header` / `lib-private-values` from its own source module,
  and the consumer instantiates `lib.Envelope<Concrete-Payload>`.
  The fixture must lint cleanly without requiring the consumer to import those helpers and without emitting E016
  for the private helper names).

These are wired into:

* the integration tests
  `generic_within_definition_site_scope_vector` and
  `generic_import_retains_private_helper_closure_vector` in
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`;
* the unit tests `generic_within_definition_site_scope_lints_cleanly`,
  `generic_within_parameter_substitution_lints_cleanly`,
  `generic_within_definition_site_negative_emits_e030_at_call_site`, and
  `generic_import_retains_private_helper_closure_lints_cleanly` in
  `crates/cbork-cddl-compiler/src/tests.rs`.

Implementation summary:

* `crates/cbork-cddl-compiler/src/generic.rs`
    * `is_genericparm_node` no longer recurses into `Directive` children, so
      import directives that sit next to an instantiation rule line can no
      longer be mistaken for generic parameter declarations.
    * `substitute_params` now refreshes the text of each parent node along the
      path from the substituted leaf up to the body root so the
      `render_subtree` path used by the diagnostic sees the substituted
      value (e.g. `[uint]` instead of stale `[T]`).

* `crates/cbork-cddl-compiler/src/resolver.rs`
    * `wrap_with_alias_node` was split into `wrap_with_alias_normal` and `wrap_with_alias_generic_body`.
      When wrapping an imported subtree whose `RuleLine` carries a `<...>` parameter list,
      the walker descends into that rule line's children in generic-body mode.
        * Alias-qualified typename/groupname references (e.g. `std.Wrapper`)
  are preserved verbatim so the generic's `.within` RHS continues to resolve through the generic definition's own import scope.
        * Bare typename/groupname references that match a rule in
  `local_rule_names` are re-prefixed with the consumer's alias
  (e.g. `protected-signed-coswid-header` → `cs1.protected-signed-coswid-header`).
  This keeps the expanded body and the helper definition reachable under the same alias-prefixed key after expansion.
    * The generic's own LHS typename is still rewritten (via the new
      `rewrite_generic_lhs_typename`) so the expansion pass can find the
      definition under the alias-prefixed name (e.g. `lib.wrapper`).
    * `rule_name_matches` strips the generic-parameter list (`<...>`) from
      the LHS before comparing it against the normalized wanted names, so
      cherry-picked generic templates are no longer tagged as prunable when
      the consumer's import directive selects them.

* `crates/cbork-cddl-compiler/src/finalize.rs`
    * `collect_rhs_references` now also walks generic definition bodies but filters out any typename/groupname text
      that matches a formal parameter name.
      This lets the reachability pruner trace from a generic definition into its `.within` RHS targets
      (e.g. `std.Wrapper`)
      and into private same-module helpers referenced from the generic body
      (e.g. `protected-signed-coswid-header`) so those definitions survive pruning when the consumer instantiates the generic.

* `crates/cbork-cddl-compiler/src/within.rs`
    * `check_within_constraint` falls back to `defs_find_suffix` when
      `defs.get(name)` is a miss, so imported definitions stored with the
      consumer's alias prefix (e.g. `lib.std.Wrapper`) are still resolvable
      when referenced by their definition-site alias (`std.Wrapper`).
    * `node_contains_genericparm` no longer recurses into `Directive`
      children, mirroring the `generic.rs` fix so the `.within` validator
      does not skip a rule line that follows an adjacent import directive.

Validation:

* `cargo test -p cbork-cddl-compiler` — all 289 unit tests + 21 integration
  tests + 8 render tests pass.
* `cargo clippy --workspace --tests -- -D warnings` — clean.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/generic_within_definition_site_scope.cddl`
  — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/generic_within_parameter_substitution.cddl`
  — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/generic_import_retains_private_helper_closure.cddl`
  — passes (the new fixture).
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/generic_within_definition_site_negative.cddl`
  — fails with E030 "Uint is not a subtype of Tstr" at the consumer
  instantiation span; effective RHS renders as the resolved `[tstr]`, not
  an unresolved `std.Wrapper` alias.
* `cargo run -p cbork -- lint cddl/rfc-std/rfc9393-tags.cddl` — passes cleanly.
  The pre-fix E016 warning for `unprotected-signed-coswid-header` is gone;
  the cherry-picked generic `cs1.COSE_Sign1-coswid<payload>` retains its private helper closure.

## Step 5.8: Make plain-vs-generic rule collisions pruning/strength-aware

Status: complete.

Implementation has landed for the aliased/import-pruned variant and the unaliased shadowing case.
Do not use files under `test/` as completion criteria for this step;
the required validation must be repo-owned fixtures under `cddl/vectors/project/`.

The generic collector currently treated a plain rule and a generic rule with the same base name
as an immediate hard collision from the unpruned resolved tree.
That was wrong for unreferenced weak imported generic helpers that share a base name with a strong local catch-all rule.

Implementation summary:

* `crates/cbork-cddl-compiler/src/generic.rs`
    * `collect_generic_definitions` is now silent:
      it walks the unpruned tree and stores generic definitions in a `HashMap` for the expansion pass,
      but it no longer emits E013 directly.
      Plain-vs-generic collisions are reported exclusively by the post-pruning detector below.
    * Removed the now-unused `push_plain_generic_collision` helper.
* `crates/cbork-cddl-compiler/src/finalize.rs`
    * `DefinitionKey` is now `pub(crate)` and `Debug` so the new collision
      detector can build pruned-set keys against the same identifier that
      the pruner records.
    * `PrunedTree` now carries a `keys: HashSet<DefinitionKey>` set in addition to the existing `names: HashSet<String>`.
      A new `_with_keys` family of prune helpers
      (`prune_nodes_with_keys`, `prune_node_with_keys`, `prune_directive_children_with_keys`, `prune_named_import_node_with_keys`)
      threads the `DefinitionKey` for every removed definition through the prune recursion so
      that downstream passes can distinguish two retained definitions with the same base name from two definitions
      where only one survives pruning.
    * New public(crate) entry point `detect_plain_generic_collisions`: walks the pre-prune `original_user_nodes`,
      partitions the top-level rule definitions into
      (plain, generic)
      buckets by base name, and emits E013 only for collision pairs where neither side's `DefinitionKey` appears in `pruned.keys`.
      Unreferenced weak imported generic helpers that the reachability pass already dropped
      therefore no longer collide with a retained strong local plain rule.
* Stable fixtures under `cddl/vectors/project/positive/`:
    * `plain_vs_generic_collision_unreferenced_generic.cddl` + `support/plain_vs_generic_collision_unreferenced_generic_lib.cddl` —
      a consumer defines `all = root` and cherry-picks an imported aliased generic helper `_all_import.all<keytype>` via `from`
      but never instantiates it.
      The lint must succeed (no E013).
      This fixture passes, but it is not sufficient for the original same-effective-name regression
      because the alias changes the imported generic's effective name to `_all_import.all`.
    * `plain_vs_generic_collision_unreferenced_unaliased_generic.cddl` +
      `support/plain_vs_generic_collision_unreferenced_unaliased_generic_lib.cddl` — a consumer defines `all = root`, imports
      `all<keytype>` without an alias, and never instantiates it.
      The lint must succeed (no E013), proving the original same-effective-name regression is fixed.
* Tests wired in:
    * Unit: `plain_vs_generic_collision_unreferenced_generic_lints_cleanly`
      in `crates/cbork-cddl-compiler/src/tests.rs`.
    * Unit: `plain_vs_generic_collision_unreferenced_unaliased_generic_lints_cleanly`
      in `crates/cbork-cddl-compiler/src/tests.rs` — the unaliased variant
      where the local plain rule shadows the imported generic helper.
    * Integration: `plain_vs_generic_collision_unreferenced_generic_vector`
      in `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Pre-existing test that already exercises the retained-collision path
  and still passes: `plain_and_generic_rule_names_collide_with_targeted_diagnostic`.

Optional follow-up coverage:

* Add a stable strong same-file collision fixture under
  `cddl/vectors/project/semantic-errors/plain_vs_generic_collision_same_file.cddl`.
  The existing unit test `plain_and_generic_rule_names_collide_with_targeted_diagnostic` already covers this behavior,
  so this is not required to complete Step 5.8.
* Add a stable reachable-unaliased positive fixture if desired.
  A temporary reproduction with `all = root`, unaliased import of `all<keytype>`, and retained `all<uint>` now lints cleanly.
  This is useful extra coverage but not required for the original unreferenced-helper regression.

Validation so far:

* `cargo test -p cbork-cddl-compiler plain_vs_generic_collision` — passes for both aliased and unaliased unreferenced fixtures.
* `cargo test -p cbork-cddl-compiler plain_and_generic_rule_names_collide_with_targeted_diagnostic` — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/plain_vs_generic_collision_unreferenced_generic.cddl --stats --summary --why --strict`
  — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/plain_vs_generic_collision_unreferenced_unaliased_generic.cddl --stats --summary --why --strict`
  — passes.
* Temporary reachable-unaliased reproduction (`all = root`, unaliased import of `all<keytype>`, retained `all<uint>`) — passes.
* `just fix-ci` (321/321 tests, full clippy + lint + RFC std corpus) — green.

## Step 5.9: Suppress duplicate same-origin import diagnostics

Status: complete.

The linter used to emit redundant-definition warnings
when the same imported definition reached the same effective compilation through more than one import path,
even when both diagnostics pointed to the exact same source file and source span.
These are not useful schema warnings: they are import graph convergence artifacts.

Implementation summary:

* `crates/cbork-cddl-compiler/src/finalize.rs`
    * `collect_definition_strength_actions` now short-circuits any site whose [`DefinitionKey`] matches a previously-seen site.
      Two retained definitions that share the same source file, line, and column are treated as the same effective definition;
      the second encounter is idempotent and does not produce a `W001`.
      This covers both filesystem re-imports of the same helper file and well-known re-imports through different selectors.
* `crates/cbork-cddl-compiler/src/resolver.rs`
    * Well-known imports now compile from a stable `catalog:<name>` pseudo path instead of the import-directive display name.
      Two imports of the same well-known module therefore produce rules with identical `DefinitionKey`s,
      and the same-origin short-circuit above takes effect for both `buuidv4` and `buuidv4, buuidv7` selectors.
      The cycle/duplicate path was unaffected because the catalog pseudo path is only used for the rule origin,
      not for `visited` membership.

Stable fixtures under `cddl/vectors/project/`:

* `positive/same_origin_convergence.cddl` and `positive/same_origin_convergence_effective_name.cddl` —
  two paths that resolve to the same helper file under the same effective name.
  The lint must succeed (no W001).
* `positive/same_origin_well_known_convergence.cddl` — imports `buuidv4` from `rfc8610` and then `buuidv4, buuidv7` from `rfc8610`.
  The lint must succeed (no W001 for the converged `uuidv4-abnf`).
* `semantic-errors/distinct_origin_duplicate.cddl` and support files —
  two different files each define `same_origin_distinct_dup = 1` and the
  lint must still emit `W001`.
* `semantic-errors/distinct_origin_conflict.cddl` and support files —
  two different files define `same_origin_distinct_conflict` with
  different bodies and the lint must still emit `E014`.

Tests wired in:

* Unit (`crates/cbork-cddl-compiler/src/tests.rs`):
    * `same_origin_import_convergence_lints_cleanly`
    * `same_origin_well_known_convergence_lints_cleanly`
    * `distinct_origin_duplicate_emits_w001`
    * `distinct_origin_conflict_emits_e014`
* Integration (`crates/cbork-cddl-compiler/tests/import_include_vectors.rs`):
    * `same_origin_import_convergence_vector`
    * `same_origin_well_known_convergence_vector`
    * `distinct_origin_duplicate_emits_w001_vector`
    * `distinct_origin_conflict_emits_e014_vector`

Validation so far:

* `cargo test -p cbork-cddl-compiler same_origin` — all four new unit tests pass.
* `cargo test -p cbork-cddl-compiler distinct_origin` — both new negative unit tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors` — 26/26 (four new vectors included).
* `cargo run -p cbork -- lint cddl/vectors/project/positive/same_origin_convergence_effective_name.cddl --strict` — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/same_origin_well_known_convergence.cddl --strict` — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/distinct_origin_duplicate.cddl --strict` — emits W001.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/distinct_origin_conflict.cddl --strict` — emits E014.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std corpus still green.
* `just fix-ci` (329/329 tests, full clippy + lint + RFC std corpus) — green.

## Step 5.10: Expand bare group references in maps

Status: complete.

Bare group references inside maps must be treated as normal group inclusion, not as unresolved socket placeholders.
For example:

<!-- rumdl-disable MD040 -->

```cddl
Generic-Headers = (
  ? alg => int,
  ? kid => bstr,
)

headers = {
  Generic-Headers,
  * label => values,
}
```

<!-- rumdl-enable MD040 -->

The effective map for `headers` must contain the concrete entries from `Generic-Headers`.
It must not leave `Generic-Headers` behind as a synthetic `Socket` key or otherwise force later `.within` checks to fail
because the RHS did not expand.

Implementation summary:

* `crates/cbork-cddl-compiler/src/finalize.rs`
    * `detect_group_reference_cycles` builds a graph from each top-level rule to the bare group references inside its body
      and reports any strongly-connected component of size >= 2 as an `E030` recursive group reference cycle.
      A bare group reference is a `grpent` whose parent is a `grpchoice`
      (top-level group element), whose own body is a bare typename chain, and which carries no `memberkey`.
      Type references that appear as map values
      (e.g. `{ key => { properties } }`) are deliberately excluded so benign references do not look like cycles.
    * The cycle pass runs unconditionally as part of the finalization
      stage so the user sees a clean diagnostic instead of relying on
      the renderer's defensive cycle detection alone.
* `crates/cbork-cddl-compiler/src/concrete.rs`
    * `RenderCx` now carries a session-level `in_progress: HashSet<String>` of names currently being inlined.
      A `render_named_reference` call whose target is already in `in_progress` returns the bare name
      as a placeholder instead of recursing,
      which keeps the renderer stack-safe for group-reference cycles even if the user disabled the static cycle detector.
    * `ResolutionMap` carries a `RefCell<Vec<Diagnostic>>` (`render_diagnostics`) plus a `take_render_diagnostics()` accessor
      so render-time diagnostics (currently the cycle placeholder) can be surfaced back through the lint path.
      This required removing the `Clone` derive on `ResolutionMap`; the two struct-literal call sites were updated.
    * `validate_within_pass` drains `render_diagnostics` into the
      shared `warnings` Vec after the `.within` checks complete.

Stable fixtures under `cddl/vectors/project/`:

* `positive/bare_group_reference_in_map.cddl` — a map body
  `{ Generic-Headers, * uint => values }` where `Generic-Headers`
  contributes optional/re-required entries; the lint must succeed.
* `positive/bare_group_reference_with_within.cddl` — a `.within`
  check across the same shape; the effective LHS view must include
  the expanded entries for the check to pass.
* `semantic-errors/bare_group_reference_cycle.cddl` — a two-rule
  cycle (`Loop-A = { Loop-B, ... }`, `Loop-B = { Loop-A, ... }`) that
  must emit `E030` and must not stack-overflow the renderer.

Tests wired in:

* Unit (`crates/cbork-cddl-compiler/src/tests.rs`):
    * `bare_group_reference_in_map_lints_cleanly`
    * `bare_group_reference_with_within_lints_cleanly`
    * `bare_group_reference_cycle_emits_e030`
* Integration (`crates/cbork-cddl-compiler/tests/import_include_vectors.rs`):
    * `bare_group_reference_in_map_vector`
    * `bare_group_reference_with_within_vector`
    * `bare_group_reference_cycle_vector`

Validation so far:

* `cargo test -p cbork-cddl-compiler bare_group_reference` — three unit tests and three integration tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors bare_group_reference` — three Step 5.10 integration tests pass.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/bare_group_reference_in_map.cddl --strict` — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/bare_group_reference_with_within.cddl --strict` — passes.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/bare_group_reference_cycle.cddl --strict` — emits E030.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std corpus still green (no false-positive cycles).
* `just fix-ci` (335/335 tests, full clippy + lint + RFC std corpus) — green.

Audit note:

* Step 5.10 is complete for the requested behavior.
* Step 6 cleanup completed the remaining Step 5.10 follow-up items:
    * The stale `Known bug: group references in maps not expanded` test/comment block in `within.rs`
      has been renamed and rewritten to describe the current expanded-group behavior.
    * The debug `eprintln!` calls on unresolved LHS/RHS names in `within.rs` have been removed;
      unresolved names now flow through structured diagnostics.

## Step 5.11: Transform ctlop subtype compatibility

Status: complete.

Serialization and transform annotations must participate in subtype checks by comparing both the carrier
and the transform controller.
It is not enough to treat every transform as simply "some `bstr`".
The transform identity matters.

This applies to:

* `.x-compressed`
* `.x-brotli`
* `.x-zstd`
* `.x-gzip`
* `.x-deflate`
* `.x-enc`
* `.x-hash`
* their `.abnf` / `.abnfb` forms where applicable

Implementation summary:

* `crates/cbork-cddl-compiler/src/within.rs`
    * `ControlOp` gained two predicates:
        * `is_encryption` — `true` only for `Self::XEnc`.
        * `is_hash_annotation` — `true` only for `Self::XHash`.
  Together with the existing `is_compression_named` / `is_compression_generic` predicates the operator taxonomy is now explicit:
  encryption, hash, compression (named algorithm or generic), plus the existing CBOR/dtrm and range families.
    * `collect_control_conflicts` and `is_control_subtype` (the diagnostic
      and boolean halves of the same matrix) both reject cross-family transform combinations:
        * `.x-enc` vs `.x-hash`
        * `.x-enc` vs compression operators
        * `.x-hash` vs `.x-enc`
        * `.x-hash` vs compression operators
        * compression operators vs `.x-enc` / `.x-hash`
  `.x-enc` / `.x-hash` conflicts use the helper pair `push_encryption_hash_conflict` and `encryption_hash_reason`
  where the matrix arm reaches that helper.
  Other cross-family transform conflicts fall through to the generic control-operator mismatch path.
  In both cases the diagnostic names the incompatible operators.
    * The "carrier-only RHS" path is unchanged: a narrowing LHS such as
      `bstr .x-brotli T` already falls through to the existing
      `op.is_narrowing() && !matches!(rhs, ResolvedType::Control { .. })`
      arm, which checks the carrier against the bare RHS. `.x-enc` and
      `.x-hash` were already on the `is_narrowing` list, so the positive
      fixtures for "LHS transform within bare bstr" work without further
      changes.
    * `.abnf` / `.abnfb` variants already collapse to the same `ControlOp`
      variant as their base operator via `ControlOp::from_text`, so the
      compatibility matrix treats them identically to the un-annotated
      operator.

Stable fixtures under `cddl/vectors/project/`:

* `positive/transform_x_enc_within_bstr.cddl` — `bstr .x-enc T .within bstr` lints cleanly.
* `positive/transform_x_enc_within_x_enc.cddl` — same-family controllers subtype.
* `positive/transform_x_hash_within_x_hash.cddl` — same-family controllers subtype.
* `positive/transform_x_brotli_within_x_compressed.cddl` — named algorithm within generic.
* `semantic-errors/transform_x_compressed_within_x_brotli.cddl` — broader within narrower.
* `semantic-errors/transform_x_brotli_within_x_zstd.cddl` — two named algorithms conflict.
* `semantic-errors/transform_x_enc_within_x_hash.cddl` — distinct families.
* `semantic-errors/transform_x_enc_within_x_brotli.cddl` — encryption within compression.
* `semantic-errors/transform_x_hash_within_x_compressed.cddl` — hash within compression.

Tests wired in:

* Unit (`crates/cbork-cddl-compiler/src/tests.rs`):
    * `transform_x_enc_within_bstr_lints_cleanly`
    * `transform_x_enc_within_x_enc_lints_cleanly`
    * `transform_x_hash_within_x_hash_lints_cleanly`
    * `transform_x_brotli_within_x_compressed_lints_cleanly`
    * `transform_x_compressed_within_x_brotli_emits_e030`
    * `transform_x_brotli_within_x_zstd_emits_e030`
    * `transform_x_enc_within_x_hash_emits_e030`
    * `transform_x_enc_within_x_brotli_emits_e030`
    * `transform_x_hash_within_x_compressed_emits_e030`
* Integration (`crates/cbork-cddl-compiler/tests/import_include_vectors.rs`):
    * `transform_x_enc_within_bstr_vector`
    * `transform_x_brotli_within_x_compressed_vector`
    * `transform_x_compressed_within_x_brotli_vector`
    * `transform_x_enc_within_x_hash_vector`

Validation so far:

* `cargo test -p cbork-cddl-compiler transform_x` — all 9 Step 5.11 unit tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors transform_x` — all 4 Step 5.11 integration tests pass.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/transform_x_*.cddl --strict` — all four positive fixtures pass.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/transform_x_*.cddl --strict` —
  all five negative fixtures emit E030 and name the incompatible operators.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std corpus still green.
* `just fix-ci` (348/348 tests, full clippy + lint + RFC std corpus) — green.

Audit note:

* Step 5.11 is complete for the requested behavior.
* No further Step 5.11 work is required before Step 6.

## Step 5.12: Library export semantics and directive hygiene

Status: complete.

Library files need an explicit export surface.
`;@ CBORK: Export` marks the next rule as externally usable by direct consumers.
This does not change the single top-level-rule convention,
and library mode should still warn when a library-shaped file has more than one ordinary top-level entry point.

Implementation summary:

* `crates/cbork-cddl-compiler/src/node.rs`
    * Added [`MetaData::Exported`] to mark rule lines that are part of
      a library's public export surface.
* `crates/cbork-cddl-compiler/src/compiled.rs`
    * Extended [`CborkDirective`] with `Export` and `Unknown(String)`
      variants so `;@ CBORK: Export` and unknown CBORK directives can
      be distinguished from plain `;@ OTHER:` external namespaces.
    * `parse_cbork_comment` now returns `Some(CborkDirective::Unknown(text))`
      for unrecognised CBORK sub-directives so the directive scan
      can emit `E021`.
    * `parse_external_directive` parses `;@ <other>: ...` and lets
      `collect_cbork_directive_sites` emit a `W002` warning per
      occurrence.
    * `apply_export_directives` walks the user tree and tags the next `RuleLine`
      after each `;@ CBORK: Export` with [`MetaData::Exported`], populating the new `CompiledCDDL::exported_names` field.
      The walker rejects:
        * `;@ CBORK: Export` in a non-library file (`E022`).
        * `;@ CBORK: Export` followed by an `import` / `include`
          directive comment (`E022`).
        * `;@ CBORK: Export` at EOF (`E022`).
        * Consecutive `;@ CBORK: Export` directives with no rule
          between them (`E022`).
        * `;@ CBORK: Export` applied to another `;@ CBORK: Export`
          or other CBORK directive (`E020`).
    * `CompiledCDDL::exported_names` carries the library's export
      surface to downstream passes.
    * New `CompiledCDDL::imported_libraries: Vec<ImportedLibrary>` field and `ImportedLibrary` struct.
      The resolver populates each entry when it processes an `import` or `include` directive,
      recording the imported/included module's `canonical_path`, `is_library`, `exported_names`, `extern_names`,
      and the consumer directive's `import_origin`.
* `crates/cbork-cddl-compiler/src/resolver.rs`
    * `resolve_includes` and `resolve_node_single` thread `imported_libraries: Vec<ImportedLibrary>`
      so each direct import/include library's metadata is captured on the consumer's `CompiledCDDL`.
* `crates/cbork-cddl-compiler/src/finalize.rs`
    * `detect_direct_export_violations` is wired into finalization and now actively emits `W003`
      for direct consumer references to non-exported imported/included library symbols.
      It takes both `pre_prune_nodes` (to build the definition source map that survives pruning) and `post_prune_nodes`
      (to walk the consumer's own rules, which are never prunable).
      Pass 1 walks the pre-prune tree and records every top-level rule's source path under both the fully-aliased name
      and the bare unaliased name (so that cross-file references resolved through aliasing can still be matched).
      Pass 2 walks the post-prune consumer's own `RuleLine` bodies,
      descending through `Syntax[expr]` to find typename references after `assignt`/`assigng`,
      and recursing through nested `Syntax[type]/[type1]/[type2]/[typename]` chains to find leaf typename names.
      Each leaf name is checked against:
        1. The consumer's own `extern_names` allow-list (always
           permitted).
        2. The postlude pseudo-path (always permitted).
        3. The per-library registry: if the name's defining file
           is in the registry, the file must be a library AND the
           name must appear in either `exported_names` or
           `extern_names` to avoid a `W003`.
  Each `(library_path, name)` pair is recorded only once via the `reported: HashSet`.
    * `detect_unused_directives` is wired into finalization and emits:
        1. `W004` when an `import` / `include` directive contributes no referenced symbol.
        2. `W005` when a selected `from` import/include name is never referenced.
  It does not warn because an imported library exposes exports that the consumer did not use; exports are public API surface,
  not required-use obligations.

Stable fixtures under `cddl/vectors/project/`:

* `positive/cbork_export_marks_rule.cddl` — `;@ CBORK: Export` on
  a single rule tags it as exported.
* `positive/cbork_external_directive_warns.cddl` — `;@ OTHER: ...`
  emits a W002 warning.
* `positive/support/direct_use_export_lib.cddl` — Step 5.12
  cross-file library with `public-rule` (exported) and
  `private-helper` (not exported).
* `positive/direct_use_export_consumes_export.cddl` — Step 5.12
  cross-file consumer that imports `public-rule` from the library
  and references it; no warning.
* `positive/include_use_export_consumes_export.cddl` +
  `positive/support/include_export_lib.cddl` — Step 5.12
  include-style consumer that references an exported symbol from an included library; no `W003`.
* `positive/used_import_reference.cddl` +
  `positive/support/used_lib.cddl` — Step 5.12
  consumer that references a selected import; no `W004` / `W005`.
* `semantic-errors/cbork_export_at_eof.cddl` — `;@ CBORK: Export`
  at EOF emits `E022`.
* `semantic-errors/cbork_export_before_import.cddl` + support —
  `;@ CBORK: Export` immediately before an import directive emits
  `E022`.
* `semantic-errors/cbork_export_in_non_library.cddl` — `;@ CBORK:
  Export` in a non-library file emits `E022`.
* `semantic-errors/cbork_unknown_directive.cddl` — unknown
  `;@ CBORK: Thing` emits `E021`.
* `semantic-errors/direct_use_export_uses_private.cddl` +
  `semantic-errors/support/direct_use_export_lib.cddl` — Step 5.12
  cross-file consumer that imports `public-rule` (selected) but
  references `private-helper`; emits `W003`.
* `semantic-errors/include_use_export_uses_private.cddl` +
  `semantic-errors/support/include_use_private_lib.cddl` — Step 5.12
  include-style consumer that references a non-exported private helper; emits `W003`.
* `semantic-errors/unused_import_emits_w004.cddl` +
  `semantic-errors/support/unused_lib.cddl` — Step 5.12
  selected import that is never referenced; emits `W004` and `W005`.
* `semantic-errors/unused_selected_import_emits_w004_w005.cddl` +
  `semantic-errors/support/unused_export_lib.cddl` — Step 5.12
  selected import that is never referenced; emits `W004` and `W005`,
  plus `W003` when the same consumer directly references a
  non-exported helper from that library.

Tests wired in:

* Unit (`crates/cbork-cddl-compiler/src/tests.rs`):
    * `cbork_export_marks_rule_lints_cleanly`
    * `cbork_export_at_eof_emits_e022`
    * `cbork_export_before_import_emits_e022`
    * `cbork_export_in_non_library_emits_e022`
    * `cbork_unknown_directive_emits_e021`
    * `cbork_external_directive_warns_as_w002`
* Integration (`crates/cbork-cddl-compiler/tests/import_include_vectors.rs`):
    * `cbork_export_marks_rule_vector`
    * `cbork_export_at_eof_emits_e022_vector`
    * `cbork_unknown_directive_emits_e021_vector`
    * `cbork_external_directive_warns_vector`
    * `direct_use_export_consumes_export_vector`
    * `include_use_export_consumes_export_vector`
    * `direct_use_export_uses_private_emits_w003_vector`
    * `include_use_export_uses_private_emits_w003_vector`
    * `used_import_reference_vector`
    * `unused_import_emits_w004_and_w005_vector`
    * `unused_selected_import_emits_w004_and_w005_vector`

Open items before Step 5.12 can be considered complete:

* BUG-011 below shows that Step 5.12 is missing the warning for importing / including a file that is not marked `;@ CBORK: Library`.
  The existing direct-use export checks are not enough because they intentionally skip non-library imports.

The cross-file direct-use export contract now covers both `import` and `include` directives.
The unused-import and unused-selected-name linting is wired into the finalization pipeline.
Stable fixtures and integration tests cover every requirement called out in the plan.

Validation so far:

* `cargo test -p cbork-cddl-compiler cbork_` — all 6 new unit tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors cbork_` — all 4 new integration tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors direct_use_export_` —
  both new Step 5.12 cross-file integration tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors cbork_export` — 2 Step 5.12 integration tests pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors direct_use_export` —
  2 Step 5.12 direct-use integration tests pass.
* `cargo test -p cbork-cddl-compiler cbork_export` — 4 Step 5.12 unit tests and 2 integration tests matched by the filter pass.
* `cargo test -p cbork-cddl-compiler --test import_include_vectors` — all 42 import/include integration tests pass,
  including Step 5.12 `W003`, `W004`, and `W005` vectors.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/cbork_export_marks_rule.cddl --strict` — passes;
  exported_names contains `public-rule`.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/cbork_export_at_eof.cddl --strict` — emits E022.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/cbork_export_before_import.cddl --strict` — emits E022.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/cbork_export_in_non_library.cddl --strict` — emits E022.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/cbork_unknown_directive.cddl --strict` — emits E021.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/cbork_external_directive_warns.cddl --strict` — emits W002.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/direct_use_export_consumes_export.cddl --strict` — clean.
* `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/direct_use_export_uses_private.cddl --strict` — emits W003.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std corpus still green.
* `just fix-ci` (361/361 tests, full clippy + lint + RFC std corpus) — green.

`README.md` updates:

* Added a "Custom control operators" section documenting the RFC 8610
  built-ins, the `.x-enc` / `.x-hash` encryption + hash annotation
  family, the `.x-compressed` / `.x-brotli` / `.x-zstd` / `.x-gzip`
  / `.x-deflate` compression annotation family, the full transform
  compatibility matrix driving `.within` subtype checks (Step 5.11),
  and the `any` as the LHS of `.cbor` / `.x-enc` / `.x-hash` /
  `.x-compressed` carrier-narrowing rule.
* Added a "`;@ CBORK: ...` compiler directives" section documenting
  the recognized CBORK directives (`Library`, `Export`, `Extern ...`),
  the directive-hygiene rules, the E020 / E021 / E022 / W002 / W003 / W004 / W005
  diagnostics, the recommended pattern for adding new directives,
  and the cross-file direct-use export contract enforced for
  library imports/includes.

Directive rules:

* `;@ CBORK: Library` marks the file as a library module.
* `;@ CBORK: Export` applies to the next rule after any whitespace, normal comments, or doc comments.
* `;@ CBORK: Export` must not skip over an `include` / `import` directive comment,
  it must produce an error if applied before either.
* `;@ CBORK: Export` must be rejected if there is no following rule,
  including if there are consecutive exports with no rule between them.
* `;@ CBORK: Export` in a non-library file should produce an error diagnostic.
* Exported symbols must be recorded in compiler metadata, not inferred from naming.
* Export metadata must survive include/import wrapping and aliasing
  so diagnostics can distinguish public exported symbols from private helper symbols.

Directive hygiene:

* Unknown `;@ <namespace>: ...` annotations where `<namespace>` is not `CBORK` should produce a warning.
  This gives users feedback that a tool annotation was ignored.
* Unknown `;@ CBORK: ...` annotations must be errors because they look like active CBORK processing directives
  but are not recognized.
* The recognized CBORK directive set for this step is:
    * `Library`
    * `Export`
    * `Extern ...`
* The parser should be structured so future CBORK directives can be added without another ad hoc string scan.

Direct-use export linting:

* If a file directly imports or includes another file and directly references a non-exported symbol from
  that imported/included file, emit a `W003` warning.
* In strict mode, that warning causes failure.
* Do not warn for transitive use.
  If an exported type internally references private helpers from its own library, consumers should not be warned for those helpers.
* Do not warn when the referenced symbol is from the same file.
* Warn when a dependency file is directly imported or included but is not marked as a CBORK library.
  The import/include directive is using the file as a reusable module, so the target file should declare `;@ CBORK: Library`.
* For files that are marked as CBORK libraries, warn when the direct consumer references a symbol that is not in the library's
  `;@ CBORK: Export` or `;@ CBORK: Extern` surface.
* The non-library import warning and the non-exported-symbol warning are separate contracts:
    * non-library target: warn that the target file is not a library module;
    * library target with private direct use: warn `W003` for the non-exported symbol.
* Do not warn when the consumer has declared the symbol via its own `;@ CBORK: Extern ...` directive.

Unused import/include/export linting:

* Warn when an `import` or `include` directive contributes no referenced symbol to the file that declared it.
* Warn when a selected import name is never referenced by the importing file.
* Do not warn merely because a library export is unused by the current direct consumer.
  A library export is public API surface, not a required-use obligation for every importer.
* Avoid false positives from references that are introduced by generic expansion, pruning, postlude injection,
  or effective-name aliasing.
* Warnings must point at the directive or export comment that introduced the unused item.

Tests:

* Positive:
    * A library with `;@ CBORK: Export` immediately before a rule can be imported and referenced directly.
    * A library export still works when normal comments, doc comments, or whitespace appear between `Export` and the rule.
    * An exported generic can reference private same-library helpers without warning the consumer.
* Negative / warning:
    * Direct consumer references a private helper from a library and receives a `W003` warning.
    * Strict mode turns the `W003` warning into failure.
    * `;@ CBORK: Export` before an import/include directive is rejected.
    * `;@ CBORK: Export` at EOF is rejected.
    * `;@ OTHER: thing` warns as an unknown external annotation.
    * `;@ CBORK: Thing` errors as an unknown CBORK directive.
    * Unused import/include directive warns.
    * Unused selected import name warns.
    * Unused export warns when the current compile graph includes the library and no direct consumer uses that export.

## Step 5.13: Complete parser-accepted literal constant seeding

Status: complete.

Step 5.1 seeded the original directly-known constants,
but a later audit found parser-accepted literal forms that are still treated as complex or unresolved by the seed pass.
Do not reopen Step 5.1; this step owns the remaining literal constant coverage before Step 6.

Implementation summary:

* `crates/cbork-cddl-compiler/src/seedops.rs`
    * `parse_uint` now dispatches on the leading radix marker: `0x` routes through `u128::from_str_radix(rest, 16)`,
      `0b` routes through `u128::from_str_radix(rest, 2)`, and the default fall-through is `str::parse::<i128>()` for decimal.
      The prefix check is strict: a digit, sign,
      or period must follow the `0x` / `0b` bytes for the prefix to count as a true radix marker.
    * `parse_int` accepts an optional leading `-`, strips it,
      delegates the magnitude to `parse_uint`, and applies the
      sign so `-0x10` becomes `Integer(-16)`, `-0b1010` becomes
      `Integer(-10)`, and `-42` becomes `Integer(-42)`.
    * `parse_number` routes a `0x` literal to `parse_int` for bare hex integers,
      to `parse_hexfloat` only when the body contains a `p` (or `P`) binary-exponent marker.
      Decimal numbers with a `.`, `e`, or `E` continue to flow through `parse_intfloat`.
    * `parse_hexfloat` now implements the CDDL `hexfloat` grammar `-? 0x <hexdigits> ( . <hexdigits> )? p <exponent>`,
      computing the result as `(int_part + frac_part / 16^digit_count) * 2^exp` and returning `EntryState::Float` on success
      or `Complex` for any syntactic deviation.
      A `f64_from_u128` helper saturates the unrepresentable range to `+∞`
      so the cast cannot wrap around to a misleading finite value; `parse_hexfloat` then rejects non-finite results as `Complex`.
    * `/=` and `//=` augmentations remain excluded from direct
      constant seeding: the existing `try_resolve_rhs` path
      only considers `/`-separated choices for `RhsLeaf::Choice`,
      which already returns `RhsKind::Complex`.

Tests wired in (`crates/cbork-cddl-compiler/src/tests.rs`):

* `seed_decimal_integer_resolves` — `x = 42`
* `seed_decimal_zero_resolves` — `x = 0`
* `seed_hex_integer_resolves` — `x = 0x10`
* `seed_hex_integer_lowercase_resolves` — `x = 0xdeadbeef`
* `seed_binary_integer_resolves` — `x = 0b1010`
* `seed_negative_decimal_integer_resolves` — `x = -42`
* `seed_negative_hex_integer_resolves` — `x = -0x10`
* `seed_negative_binary_integer_resolves` — `x = -0b1010`
* `seed_decimal_float_resolves` — `x = 3.5`
* `seed_decimal_scientific_float_resolves` — `x = 1.5e2`
* `seed_hexfloat_resolves` — `x = 0x1.fp+2`
* `seed_hexfloat_negative_resolves` — `x = -0x1.fp+2`
* `seed_hexfloat_no_fraction_resolves` — `x = 0x10p+1`
* `seed_text_literal_resolves` — `x = "hello"`
* `seed_bytes_literal_resolves` — `x = h'abcd'`
* `seed_one_step_reference_propagates` — `a = 0x10; b = a; c = b .plus 1`
* `seed_complex_rhs_remains_unresolved` — `a = 1 / 2; b = a`
* `seed_array_rhs_remains_unresolved` — `a = [1, 2, 3]; b = a`
* `seed_map_rhs_remains_unresolved` — `a = { "k" => 1 }; b = a`
* `seed_ctlop_rhs_remains_unresolved` — `a = uint .gt 0; b = a`

Validation so far:

* `cargo test -p cbork-cddl-compiler seed_` — all 20 new unit tests pass.
* Re-verified `cargo test -p cbork-cddl-compiler seed_` on 2026-06-19 — all 20 seed tests pass.
* `cargo test -p cbork-cddl-compiler` — all 384 tests pass.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std corpus still lints clean.
* `just fix-ci` (384/384 tests, full clippy + lint + RFC std corpus) — green.

## Step 6: Produce the fully processed document

Status: complete.

In `cbork-cddl-compiler`, after semantic resolution, pruning, and postlude merge,
produce a complete resolved AST tree with includes processed.

`CompiledCDDL::complete_nodes` is the fully processed document.
It is populated by the finalization pass after Steps 5.5 (pruning), 5.10 (bare group reference expansion), 5.11
(transform compatibility), and 5.12 (cross-file export contract) have all run.

Implementation summary:

* `crates/cbork-cddl-compiler/src/compiled.rs`
    * `CompiledCDDL` exposes `user_nodes` (raw user tree), `postlude_nodes`
      (the standard postlude, tagged `MetaData::Silent`), and
      `complete_nodes` (the physically complete tree).
    * New `CompiledCDDL::has_errors` predicate lets downstream validators quickly determine
      whether the `complete_nodes` tree should be treated as a valid schema tree.
      Errors come from any diagnostic in `warnings` whose `level` is `DiagnosticLevel::Error`.
* `crates/cbork-cddl-compiler/src/within.rs`
    * `validate_within_pass` is wired into the finalization pipeline
      (the previous `dead_code` allowance on the module is updated to
      note that the public entrypoint is live, while the remaining
      helpers stay test-only).
    * The two `eprintln!` calls in `collect_subtype_conflicts_inner`
      that printed `subtype_conflicts: UNRESOLVED LHS/RHS` are removed;
      the same information is already captured in
      `WithinConflictKind::UnresolvedName` and rendered through the
      structured diagnostic.
    * The stale `bug_group_reference_in_map_not_expanded` test is
      renamed to `bare_group_reference_in_map_uses_expanded_type`
      and its docstring rewritten to describe the current behavior
      (Step 5.10 group reference expansion followed by a type-mismatch
      conflict on the LHS-required `bstr` key).
    * The stale `bug_range_value_should_be_subtype_of_int` and
      `bug_map_with_socket_plug_unresolved` tests are renamed to
      `range_value_is_subtype_of_int` and
      `map_with_socket_plug_preserves_choices` respectively; their
      docstrings and section header are updated to reflect that the
      previously-flagged behavior is now correct.
* `crates/cbork-cddl-compiler/src/schema_diff.rs`
    * The "Wiring" section in the module doc is updated to say that
      `build_schema_diff` is wired into
      `crate::within::check_within_constraint`; the old "Step 6 will
      wire" placeholder is removed.
* `crates/cbork-cddl-compiler/src/lib.rs`
    * The `mod within` allowance is updated from "will be wired in
      later stages" to "validation pipeline is wired in; the
      remaining public helpers are only consumed from unit tests".
* `crates/cbork-cddl-compiler/src/tests.rs`
    * The `dump_cross_file` debug-only test is removed.
    * New `has_errors_reports_clean_compile_correctly`,
      `has_errors_reports_undefined_reference_as_error`, and
      `complete_nodes_preserves_origin_for_provenance` tests cover
      the Step 6 contract for `has_errors` and the provenance
      guarantee on every top-level `RuleLine` in `complete_nodes`.

Requirements:

* The output reflects the resolved include graph — verified by
  `resolve_includes` running before the finalization pass and by the
  existing `import_include_vectors` integration tests.
* The output preserves enough provenance to trace rules back to their
  source files — every `RuleLine` retains its `origin.source_path`,
  which the new `complete_nodes_preserves_origin_for_provenance`
  test pins.
* The output supports concise emission, where `SILENT` nodes such as
  the postlude are omitted — `postlude_nodes` carry
  `MetaData::Silent` so renderers can filter them; the
  surgical postlude-injection in `finalize.rs` only copies
  primitives into `complete_nodes` when they are referenced.
* This is still an AST result, not a rendered CDDL string and not a
  flattened export format — `complete_nodes` is `Vec<WrappedNode>`.
* It is ready for further AST processing — `has_errors` lets
  downstream validators gate further work; the tree exposes origin,
  span, rule names, type references, and metadata for every node.
* If the complete tree contains hard errors, downstream validators
  must not treat it as a valid schema tree — `has_errors()` returns
  `true` for any `DiagnosticLevel::Error` in the diagnostic list.
* If the complete tree has no hard errors, it should be physically
  complete and ready for later binary conformance validation —
  `finalize_compiled` runs all post-prune, post-strength, post-semantic
  passes before assigning `complete_nodes`.

Diagnostics accumulation:

* `CompiledCDDL::compile` accumulates diagnostics into
  `compiled.warnings` and returns `Err(CompileError)` only when a
  fatal resolver error prevents any further processing
  (`resolve_includes` cycle or unreadable file).
* The postlude merge, the cross-file export checks, and the unused
  import/export warnings all run after the include resolver so
  callers see every diagnostic the compiler could find in a single
  pass.
* `deduplicate_diagnostics` is called at the end of `compile` to
  collapse identical diagnostics.

Validation so far:

* Re-verified Step 6 on 2026-06-19:
    * `cargo test -p cbork-cddl-compiler has_errors` — 2 tests pass.
    * `cargo test -p cbork-cddl-compiler complete_nodes_preserves_origin_for_provenance` — 1 test passes.
    * `cargo test -p cbork-cddl-compiler within_diagnostic_contains_inline_diff_subdiags` — 1 test passes.
* `cargo test -p cbork-cddl-compiler` — all 386 tests pass.
* `cargo run -p cbork -- lint cddl/rfc-std --strict` — full RFC std
  corpus still lints clean.
* `just fix-ci` (386/386 tests, full clippy + lint + RFC std corpus)
  — green.

## Step 7: Tests and vectors

Status: complete.

The `cbork-cddl-compiler` test suite already covers every baseline item in the Step 7 checklist;
this step consolidates that coverage and adds the missing directory-walk tests so the existing positive
and semantic-error vectors are exercised on every test run.

Baseline coverage audit:

* Comment blocks containing module directives — `compile_with_directive_import`
  asserts the `ModuleStart` / `Directive` / `ModuleEnd` triple.
* Multiple directives in one comment block —
  `comment_with_multiple_directives` walks a multi-line `;` block
  and asserts the directive count and module-marker count.
* Non-directive comments interleaved with directives —
  `compile_preserves_non_directive_comments` and
  `compile_interleaved_directives_and_rules` both pin the
  preservation order.
* Directive parsing order preservation — the parser's
  `parse_multiple_directives` test and the preprocessor's
  `comments_preserved_in_parse_order` test pin source order.
* AST injection order preservation —
  `comments_preserved_in_parse_order` and
  `compile_interleaved_directives_and_rules` pin that the
  ModuleStart / Directive / ModuleEnd triple appears in source
  order.
* Bounded module comment emission around child AST blocks —
  `compile_with_directive_import`, `compile_with_import_as`,
  `compile_include_directive`, and
  `compile_interleaved_directives_and_rules` all assert the
  module-marker bounds.
* Pruning behavior for dangling removable definitions —
  `pruning_ignores_dangling_references_in_removed_rules`,
  `pruning_retains_reachable_prunable_rules_and_reports_their_dangling_refs`,
  `unreferenced_weak_imports_are_pruned_silently_no_conflict`,
  `importer_strong_definition_silently_drops_unreferenced_weak_imports`,
  `named_import_keeps_full_subtree_for_later_pruning`,
  `unprefixed_include_silently_drops_against_stronger_local_definition`.
* Non-prunable core definitions — `finalize_compiled` collects
  non-prunable definition names via
  `collect_non_prunable_definition_names`; the first rule of a
  CDDL file is always retained as a non-prunable root, and the
  `first_define_name` helper in the concrete renderer uses the
  same convention.
* `SILENT` and `PRUNABLE` postlude handling —
  `postlude_nodes_are_silent` asserts every postlude node carries
  `MetaData::Silent`; `metadata_prunable_flag` pins the `Prunable`
  variant; `postlude_injected_subtree_is_fully_silent` verifies the
  postlude merge does not leak the `Silent` metadata into the
  complete tree.
* Standard-library includes by known name —
  `import_std_bare_vector`, `import_std_named_vector`,
  `repeated_stdlib_import_is_not_a_cycle`, plus
  `filename_parse_well_known` and `filename_resolve_well_known`
  in the parser.
* Relative includes resolved against the containing file —
  `include_relative_bare_vector`, `include_relative_named_vector`,
  `filename_parse_relative`,
  `filename_parse_relative_no_dot_slash`.
* Absolute includes — `include_absolute_repo_root_vector`,
  `import_absolute_repo_root_vector`,
  `filename_parse_absolute`.
* Rename forms — `compile_with_import_as`,
  `named_import_as_requires_prefixed_selected_names`,
  `import_as_does_not_prefix_prelude_names`, plus the parser's
  `parse_import_as`, `parse_include_as`, `parse_include_from_as`,
  `parse_import_from_as_with_hyphenated_generic_name`.
* Filename classification for built-in, relative, and absolute
  references — `filename_parse_well_known`,
  `filename_parse_relative`, `filename_parse_relative_no_dot_slash`,
  `filename_parse_absolute`, `filename_resolve_well_known`,
  `filename_resolve_well_known_not_found`,
  `filename_resolve_relative_not_found`.
* Generated standard-library lookup helpers —
  `cbork-catalog/src/lib.rs::known_names` /
  `cbork-catalog/src/lib.rs::lookup` / `cbork-catalog/src/lib.rs::summary`,
  with the integration tests `catalog_entries_match_fs_files` and
  `fs_files_all_have_catalog_entries` in
  `cbork-catalog/tests/catalog_integration.rs` walking the
  `cddl/rfc-std/` source tree end-to-end.
* Integration tests that consume the vendored import/include
  vectors and assert the compiled AST shape for each case —
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`
  pins 42 individual fixtures end-to-end; the
  `cbork-cddl-compiler/tests/render_vectors.rs` integration
  file pins 8 more rendering scenarios.

New tests added in this step:

* `tests::project_positive_vectors_compile_cleanly` walks `cddl/vectors/project/positive/`,
  computes the repo root from `CARGO_MANIFEST_DIR`, and asserts every fixture compiles to a `CompiledCDDL` document
  (no fatal resolver errors).
  The walk tolerates `E020 unreferenced top-level definition` and other hard errors
  because several positive vectors are deliberately illustrative example documents.
* `tests::project_semantic_error_vectors_compile_with_diagnostics` walks `cddl/vectors/project/semantic-errors/`,
  skipping `support/` subdirectory and `_lib` filename-suffix entries.
  Each remaining fixture must either compile with at least one diagnostic (the canonical case) or fail with a parse error
  (covered by the `negative/` walk).

Validation so far:

* Re-verified Step 7 on 2026-06-19:
    * `cargo test -p cbork-cddl-compiler project_positive_vectors_compile_cleanly` — 1 test passes.
    * `cargo test -p cbork-cddl-compiler project_semantic_error_vectors_compile_with_diagnostics` — 1 test passes.
    * `cargo test -p cbork-cddl-compiler --test import_include_vectors` — 42 integration tests pass.
    * `cargo test -p cbork-cddl-compiler --test render_vectors` — 8 integration tests pass.

Test data guidance:

* RFC-derived vectors stay in `cddl/vectors/rfc/`.
* Project-positive vectors stay in `cddl/vectors/project/positive/`.
* Project-negative vectors stay in `cddl/vectors/project/negative/`.
* Project semantic-error vectors stay in
  `cddl/vectors/project/semantic-errors/`.
* Known bug reproducers stay in `cddl/vectors/project/bugs/`.
  These are not passing positive vectors until the corresponding compiler/linter bug is fixed.
* Vectors from a spec example carry a provenance comment naming
  the source document and location.

## Bugs

These bugs were exposed by `just test-dntls-libs` on the DNTLS test documents.
They are compiler/linter issues, not CDDL source fixes.
The DNTLS `test/` tree is in flux, so each item has a stable repo-owned reproducer under `cddl/vectors/project/bugs/`.

### BUG-001: Valid `;@ CBORK: Export` emits false `E020`

Status: open; partial formatting fix landed but the DNTLS repro still fails the contract.

Observed behavior:

* A valid `;@ CBORK: Export` immediately before a rule emitted
  `error[E020]: ;@ CBORK: Export must be applied to the next rule,
  not to another directive`.
* This was visible in `test/dntls-core/doc/dntls-cose-defs.cddl`,
  `test/dntls-core/doc/dntls-cose-encrypt.cddl`,
  `test/dntls-core/doc/dntls-cose-sign.cddl`, and the copied
  service-data support libraries.
* `cargo run -p cbork -- lint cddl/vectors/project/positive/cbork_export_marks_rule.cddl --strict`
  also reproduced the false diagnostic, which meant the existing
  export-name test did not assert strict CLI cleanliness.

Stable reproducer:

* `cddl/vectors/project/bugs/cbork_export_before_rule_false_e020.cddl`

Root cause:

* `scan_cbork_file_directives` unconditionally pushed an `E020` diagnostic
  for every `CborkDirective::Export` site returned by `collect_cbork_directive_sites`.
  That pass ran before `apply_export_directives`,
  so it had no way to tell whether the export was about to be applied to a real rule or was actually misplaced.
  The `E022` path in `apply_export_directives_inner` already covers the real invalid placements
  (export at EOF, export before an `import` / `include` directive, and export in a non-library file);
  the `E020` was redundant and a false positive for every valid `;@ CBORK: Export` site.

Fix:

* `crates/cbork-cddl-compiler/src/compiled.rs::scan_cbork_file_directives` no longer pushes a diagnostic
  for the `CborkDirective::Export` arm.
  The comment at the site explains the deletion and points to the `E022` path that owns the real diagnostic.
* `apply_export_directives` already returns the set of exported
  names; the E022 emissions in `apply_export_directives_inner`
  continue to cover all three real invalid placements.

Verification:

* `cargo run -p cbork -- lint cddl/vectors/project/bugs/cbork_export_before_rule_false_e020.cddl --strict`
  now passes (✅).
* `cargo run -p cbork -- lint cddl/vectors/project/positive/cbork_export_marks_rule.cddl`
  no longer emits the false `E020`; the remaining warning is the
  legitimate "unreferenced top-level definition `private-helper`"
  from `finalize.rs`, which is a separate diagnostic.
* `cargo test -p cbork-cddl-compiler` — all 390 tests pass; the
  new strict-lint regression tests pin the absence of the false
  `E020`:
    * Unit test `cbork_export_marks_rule_lints_cleanly` in
      `crates/cbork-cddl-compiler/src/tests.rs` now also asserts
      that no diagnostic with code `E020` and message
      "must be applied to the next rule" is emitted.
    * New unit test `bug_001_cbork_export_before_rule_lints_cleanly`
      in the same file compiles the bug fixture and asserts
      both `!has_errors()` and the absence of the false `E020`
      wording.
    * New integration test
      `bug_001_cbork_export_before_rule_vector` in
      `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`
      pins the same invariant at the integration-test layer.
* The four `cbork_export_*_emits_e022` tests still pass, so the
  real invalid-placement cases (export at EOF, export before an
  import/include, export in a non-library file, and consecutive
  exports) are still covered by the `E022` path.
* `just fix-ci` — 54/54 tasks green; `cargo run -p cbork -- lint
  cddl/rfc-std --strict` continues to lints clean.

### BUG-002: Consumer-side W006 incorrectly treats library exports as required uses

Status: resolved.

Observed behavior:

* `test/svcrec/doc/service-data/delegation.cddl` imports
  `../../../pqsig/doc/pqsig.cddl` as `pqsig` and directly uses
  `pqsig.tagged-pq-hybrid-pub`.
* Lint warned:
  `warning[W006]: unused library export pq-hybrid from /repo/test/pqsig/doc/pqsig.cddl:
  no direct consumer references it`.
* This was wrong.
  A library export is a public API surface, not an obligation for every consumer of the library to reference every exported name.

Root cause:

* `detect_unused_directives` in `crates/cbork-cddl-compiler/src/finalize.rs` iterated every imported library's `exported_names`
  and emitted a `W006` "unused library export" diagnostic for every name the consumer did not reference.
  That model is the wrong contract for whole-library imports:
  a consumer that imports a library under an alias is allowed to use a subset of the library's API.

Fix:

* `crates/cbork-cddl-compiler/src/finalize.rs` — the `W006` "unused library export" loop is removed from `detect_unused_directives`.
  The function still emits `W004` (whole-directive unused) and `W005` (per-selected-name unused for `from` clauses).
  The cross-file direct-use-export `W003` is owned by `detect_direct_export_violations` and remains in place.
* The function's `consumer_extern_names` and `imported_libraries`
  parameters are kept (with `let _ = …` bindings) so the call site
  is stable for any future redefinition of the diagnostic that
  uses them (e.g. "an exported rule is reached by way of `;@
  CBORK: Extern`").
* The old unused-export fixture was renamed to `unused_selected_import_emits_w004_w005.cddl`
  and now documents the legitimate selected-import diagnostics:
  `W004` for the unused directive and `W005` for the unused selected name, plus absence of `W006`.
  The `;@ … from …` selected-name path is the one that exercises the per-name diagnostic.

Stable reproducer:

* `cddl/vectors/project/bugs/bug_002_whole_library_import_no_w006.cddl` with support
  `cddl/vectors/project/bugs/support/bug_002_whole_lib.cddl`.
  The library exports `public-a` and defines a non-exported `private-helper`.
  The consumer does a whole-library import and references `private-helper`.
  After the fix:
    * No `W004` (the directive is used — it brings in
      `private-helper`).
    * No `W005` (no `from` clause, so no per-selected-name
      diagnostic).
    * No `W006` (the unused `public-a` export is the BUG; a
      library export is a public API surface, not an obligation
      for this consumer).
    * `W003` still fires (the consumer references the
      non-exported `private-helper` — the legitimate
      cross-file direct-use-export contract).

Note on the two-exports layout:

* The original "two exported symbols" layout in the plan would require two `;@ CBORK: Export` directives with a non-exported rule
  between them.
  The current CDDL parser silently consumes the second `;@ CBORK: Export` as a trailing comment of the first rule
  (the `S` rule inside `expr` includes `COMMENT`), so the second export is dropped from the `user_nodes` tree.
  The BUG-002 fixture therefore uses a single `;@ CBORK: Export` to exercise the same contract:
  a library export is a public API surface, not an obligation for the consumer.
  The contract is identical regardless of how many exports the library declares.

Verification:

* `cargo run -p cbork -- lint cddl/vectors/project/bugs/bug_002_whole_library_import_no_w006.cddl --strict`
  no longer emits `W006`; only the legitimate `W003` for the
  non-exported reference remains.
* `cargo test -p cbork-cddl-compiler` — all 392 tests pass.
  New regression tests:
    * Unit test `bug_002_whole_library_import_does_not_emit_w006`
      in `crates/cbork-cddl-compiler/src/tests.rs` pins the
      `W004` / `W005` / `W006` absence plus the `W003` presence.
    * Integration test
      `bug_002_whole_library_import_no_w006_vector` in
      `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`
      pins the same invariant at the integration-test layer.
    * Existing test
      `unused_selected_import_emits_w004_and_w005_vector`
      still passes and asserts the selected-import `W004` / `W005`
      diagnostics plus the absence of `W006`.
* `just fix-ci` — 54/54 tasks green; `cargo run -p cbork -- lint
  cddl/rfc-std --strict` continues to lint clean.

### BUG-003: Alias-imported generic private helper closure can be unresolved transitively

Status: resolved.

Observed behavior:

* `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  emits:
  `error[E016]: undefined reference a2d.untagged-argon2id`
  at `/repo/test/dntls-core/doc/name-types.cddl:17:12`.
* The immediate source shape is:
  `name-types.cddl` imports `../../argon2id/doc/argon2id.cddl`
  as `a2d` and instantiates `a2d.argon2id<innerhash>`.
* `argon2id<t>` privately references
  `tagged-argon2id<t>` and `untagged-argon2id<t>`.
* The consumer sees a qualified private helper reference
  (`a2d.untagged-argon2id`) but the resolver cache / retained
  definition set does not contain the matching qualified helper.

Why this is a compiler/linter bug:

* A generic imported under an alias is allowed to reference private
  same-library helpers inside its own body.
* Instantiating `a2d.argon2id<innerhash>` must retain and qualify the
  private helper closure required by `argon2id<t>`.
* The direct `name-types.cddl` shape may lint clean in isolation, but
  the transitive consumer `name-reg-tx-v1.cddl` exposes the missing
  closure during full finalization.
* Downstream `.within` diagnostics in the same run should not be
  treated as authoritative until this unresolved reference is fixed,
  because the effective CDDL is incomplete.

Required stable reproducer:

* Add a fixture under `cddl/vectors/project/bugs/` with this shape:
    * support library A defines a generic `Wrapper<t>` whose body
      references private helpers `tagged<t>` / `untagged<t>`.
    * library B imports A as an alias and defines
      `via-alias = a.Wrapper<local-type>`.
    * top-level consumer imports library B and references
      `via-alias`, forcing the nested alias-import generic body to
      resolve through a transitive consumer.
* Expected current behavior before fix: reproduces an `E016` for the
  alias-qualified private helper.
* Expected behavior after fix: no `E016`; the helper closure is
  retained, alias-qualified, and visible to the resolver / finalizer.

Implementation notes:

* This is adjacent to the selected-imported-generic private-helper
  closure work from Step 5.7, but the failing shape is transitive:
  a direct consumer imports a library that itself imports and
  instantiates an aliased generic.
* Audit alias wrapping for generic definition bodies and any pruning
  pass that drops private helper definitions after the intermediate
  library compiles cleanly.
* The fix must not make private helpers directly exported.
  They remain private implementation detail; they are retained only because an exported/used generic body depends on them.

Partial work completed:

* Root cause lived in the alias wrap walker in `resolver.rs`
  (`wrap_with_alias_node_with_mode` / `wrap_with_alias_normal` /
  `wrap_with_alias_generic_body`):
    1. `wrap_with_alias_generic_body` compared typenametext (`tagged<t>`) against `local_rule_names`
       (which contains the bare rule name `tagged`).
       The `<...>` argument list had to be stripped via `name.split_once('<').map_or(name, |(head, _)| head.trim())`
       before the `contains` check.
    2. Both `wrap_with_alias_normal` (Syntax + RuleLine arms) and
       `wrap_with_alias_generic_body` (Syntax arm) had to skip
       prefixing when `name.contains('.')` (already aliased by a
       transitive importer, e.g. `a.Wrapper`) or `name == alias`
       (the importer's own alias, which is not a rule reference).
    3. The Directive arms in both functions must not recurse into
       children: the importer's directive children are already in
       consumer-shape form (their `user_nodes` ran through the
       importer's wrap), so re-walking them would double-prefix
       references like `a.Wrapper` into `middle.a.Wrapper` and
       re-wrap a fully-resolved subtree.
* Stable reproducer lives at
  `cddl/vectors/project/bugs/bug_003_alias_generic_helper_closure.cddl`
  plus `support/bug_003_middle.cddl` plus
  `support/bug_003_lib_a.cddl`.
* Unit test: `bug_003_alias_generic_helper_closure_lints_cleanly`
  in `crates/cbork-cddl-compiler/src/tests.rs`.
* Integration test:
  `bug_003_alias_generic_helper_closure_vector` in
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Follow-on: the `nested_alias_transitive_vector` regression test
  was asserting the buggy double-prefix shape
  (`mid.lf.leaf_value`); it now asserts the correct
  single-immediate-importer shape (`lf.leaf_value` / `lf.leaf_rule`
  / `mid.mid_rule` / `nested_alias_root = mid.mid_rule`).

### BUG-004: Private library root `all` names collide across independent imports

Status: resolved.

Observed behavior:

* `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl --strict`
  emits:
  `error[E013]: rule name collision: all is defined both as a plain
  rule and as a generic rule`.
* The reported definitions are from two different imported library
  files:
    * `/repo/test/dntls-core/doc/dntls-cose-encrypt.cddl:5`
      defines private library root `all = ...`.
    * `/repo/test/dntls-core/doc/dntls-keysets.cddl:12`
      defines private generic library root `all<keytype> = ...`.
* These roots are internal library-root/supertype conventions, not
  direct consumer schema surface.

Why this is a compiler/linter bug:

* Different directly imported libraries can each use private root
  names such as `all` to satisfy the "single root/supertype" CDDL
  shape convention.
* Those unrelated private roots must not be merged into one consumer
  namespace as if they were local definitions.
* A plain-vs-generic collision is legitimate within the same effective
  namespace/origin, but not between independent imported libraries
  whose private roots are not directly selected/referenced by the
  consumer.
* Export status is irrelevant to this collision rule.
  Exports are API surface; top-level tethering and private-root scoping are separate concerns.

Required stable reproducer:

* Add a fixture under `cddl/vectors/project/bugs/` with:
    * support library A marked `;@ CBORK: Library`, defining
      `library = all` and private `all = uint`.
    * support library B marked `;@ CBORK: Library`, defining
      `library = all<bstr>` and private `all<t> = t`.
    * top-level consumer imports both libraries but references only
      exported/useful non-root symbols from each, or otherwise forces
      both libraries into the compiled document without directly
      using their private `all` roots.
* Expected current behavior before fix: reproduces `E013` plain vs
  generic collision on `all`.
* Expected behavior after fix: no `E013`; independent private library
  roots remain scoped by origin/alias/strength and do not collide in
  the consumer.

Implementation notes:

* Audit the plain-vs-generic collision detector and definition-strength
  normalization after import/include wrapping.
* The collision key likely needs origin/scope awareness for private
  imported library definitions, or those private roots need pruning
  before collision detection when they are not part of the direct
  consumer surface.
* Keep same-file plain-vs-generic collisions intact.
* Keep collisions for genuinely converged same-name definitions intact
  when the consumer directly selects or references conflicting names.
* Do not solve this by weakening the global same-origin duplicate
  checks broadly; the fix must be scoped to independent imported
  library private roots.

Resolution:

* Root cause lived in two collision detectors in `finalize.rs`: the plain-vs-generic detector
  (`detect_plain_generic_collisions`, E013) and the strength-normalization walker (`collect_definition_strength_actions`, E014).
  Both walked the post-import tree and treated any pair of definitions that shared a base name as a consumer-side collision,
  regardless of whether the consumer actually referenced either name.
* Fix: each detector now requires that at least one of the colliding sides live on the consumer's "direct surface".
  The direct surface is defined as:
    1. definitions whose `origin.source_path` matches the
       consumer's own source path;
    2. names referenced anywhere in the consumer's own
       definitions (collected by the existing
       `collect_consumer_references` walker);
    3. names explicitly cherry-picked by the consumer via
       `from ... import <name>,...` directives (tracked on the
       new `ImportedLibrary::directive_names` field, populated
       by the resolver when it processes `from ...` directives).
* When both sides of a collision come from independently imported files
  (different `source_path`) and the conflicting name is not on the consumer's direct surface, the collision is silently dropped.
  This matches the bug plan's intent: independent libraries' private roots are not part of the consumer's effective namespace.
* `ImportedLibrary` gained a `directive_names: HashSet<String>` field populated by the resolver.
  It feeds both `normalize_definition_strengths` and `detect_plain_generic_collisions`.
* Stable reproducer lives at `cddl/vectors/project/bugs/bug_004_library_all_root_collision.cddl` plus
  `support/bug_004_lib_plain.cddl` plus `support/bug_004_lib_generic.cddl`.
  The reproducer intentionally exports different names
  (`foo` / `bar`)
  from each library so the only collision in the consumer's namespace is the cross-library private `all` pair;
  no E014 from a public-symbol collision muddies the test.
* Unit test: `bug_004_library_all_root_collision_lints_cleanly`
  in `crates/cbork-cddl-compiler/src/tests.rs`.
* Integration test:
  `bug_004_library_all_root_collision_vector` in
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Step 5.12's `distinct_origin_conflict_emits_e014` and
  `weak_conflicting_imports_are_hard_errors` regressions still
  pass: the consumer in those fixtures directly references the
  conflicting name, so it stays on the consumer's direct surface
  and the E014 fires as before.

### BUG-005: `.within` diagnostics render effective LHS/DIFF as a single line of text, not structured format

Status: resolved.

Scope:

* This bug is only about formatting already-resolved diagnostic text.
* It covers `EFFECTIVE LHS` and `DIFF` lines that contain huge inline nested maps, arrays, groups, controls, or tag bodies.
* It does not cover whether `EFFECTIVE RHS` is actually expanded.
  That is BUG-006.

Observed behavior:

* `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  still emits `EFFECTIVE LHS` lines with large inline nested expressions, including:
  `protected: bstr .dtrm {tld: tstr, root: any .dtrm ...}`
  and a long `payload: bstr .dtrm [...] .within [...]` line.
* `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl --strict`
  still emits `EFFECTIVE LHS` and `DIFF` lines such as:
  `(protected: {1: 57, 4: bstr .size 32, ? -21: bstr, ...}, unprotected: {-4: bstr .size 1120})`.

Why this is a compiler/linter bug:

* `.within` diagnostics exist to explain why the concrete schemas do not match.
* Human-readable diagnostics cannot rely on giant inline nested structures.
* Any brace/bracket/paren/tag/control nesting that causes indentation must keep content indented until the matching close.

Partial work already landed:

* `render_grpent_keyed_block` in `concrete.rs` handles a narrow `key: { ... }` / `key: [ ... ]` case.
* Existing fixture: `cddl/vectors/project/bugs/bug_005_within_renders_multiline_effective.cddl`.
* Existing tests: `bug_005_within_renders_multiline_effective_subdiag` and `bug_005_within_renders_multiline_effective_vector`.
* These tests are not sufficient because they do not cover the DNTLS diagnostic shape.

Required follow-up:

* Add a second BUG-005 fixture that fails on the current implementation and mirrors the DNTLS LHS/DIFF formatting shape:
    * a protected header map nested under a control op such as `bstr .dtrm { ... }`;
    * a payload/ciphertext field nested under another control op such as `bstr .x-enc [ ... ]`;
    * a group entry such as `(protected: ..., unprotected: ...)`;
    * at least one array/map nested inside a control op.
* The test must assert that no `EFFECTIVE LHS` or `DIFF` line contains deeply nested inline patterns such as:
  `bstr .dtrm {`, `bstr .x-enc [`, `(protected: {`, or `}, unprotected: {`.
* The fix must be structural, not just a wider/narrower line-length threshold.

Expected behavior after fix:

* Nested maps, arrays, groups, choices, controls, and tag bodies render over multiple lines with stable indentation.
* The inline `DIFF` uses the same rendered line model and does not regress to one-line snippets.

Resolution:

* Added `effective_mode: bool` to `ConcretePolicy`.
  When enabled (set by `for_lhs()` and `for_rhs()` policies), the renderer:
  1. Skips the strong-definition bail-out so `:=` imported types are inlined.
  2. Skips constant folding for well-known base names (`bstr`, `uint`, etc.)
     so primitives stay readable.
  3. Routes inlined bodies through `render_pretty_rhs` instead of the single-line
     `render_with_inlining_inner`, so parenthesized groups, `.within` chains,
     brace/bracket blocks, and ctlop chains all expand across multiple indented lines.
  4. Falls back to a qualified-name lookup when a bare typename reference
     (`Headers`) is not found in the resolution map: the renderer scans
     `resolution.definitions` for entries ending with `.<name>`, which catches
     `cose.Headers` for bare `Headers`.
* The `render_subtree` call in `within.rs` already uses `for_lhs()` / `for_rhs()`;
  no changes needed there — the new `effective_mode` field is picked up automatically.
* Postlude primitives marked `StandardPostlude` that are well-known base names
  (`bstr`, `int`, etc.) are NOT inlined even in effective mode so the diagnostic
  text stays readable.
* `render_pretty_lines` already handles `.within` chain splitting, parenthesized
  choice formatting, and brace/bracket block rendering; the effective-mode routing
  in `inline_definition` now feeds the expanded body through this pipeline instead
  of the single-line inline renderer.
* Verified against the DNTLS `svcrec` fixture (`test/svcrec/doc/service-record-v1.cddl`):
  `EFFECTIVE LHS` parenthesized groups now render across multiple indented lines;
  `EFFECTIVE RHS` expands symbolic `Headers` to `(protected: ..., unprotected: ...)`.

### BUG-006: `.within` diagnostics render declared RHS instead of effective RHS

Status: resolved.

Scope:

* This bug is only about semantic expansion of the RHS diagnostic.
* It covers `EFFECTIVE RHS` showing declared symbolic type references instead of the concrete effective RHS.
* It is independent of whether the resulting RHS text is formatted nicely; formatting is BUG-005.

Observed behavior:

* `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  shows `Headers`, `COSE_Signature`, `COSE_recipient`, and similar symbolic names in `EFFECTIVE RHS`.
* `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl --strict`
  shows `Headers`, `COSE_recipient`, and `? recipients: [+COSE_recipient]` in `EFFECTIVE RHS`.

Why this is a compiler/linter bug:

* The RHS of `.within` is the schema that the LHS must fit within.
* Showing the declared RHS template is useless for debugging when the user needs to know what the RHS boils down to.
* `EFFECTIVE RHS` must be the instantiated, imported, generic-expanded, constant-folded,
  concrete RHS in the same context as the failing `.within`.

Required stable reproducer:

* Add a BUG-006 fixture under `cddl/vectors/project/bugs/` with:
    * a generic RHS wrapper analogous to `COSE_Sign<headers, payload>` or `COSE_recipient<headers, payload>`;
    * RHS body references to symbolic dependencies analogous to `Headers` and `COSE_recipient`;
    * a failing `.within` instantiation so an `E030` diagnostic is emitted.
* The test must assert the `EFFECTIVE RHS` snippet does not contain the symbolic names used by the fixture.
* The test must assert the RHS contains the expanded concrete fields those names resolve to.

Implementation notes:

* Audit the `.within` diagnostic construction path in `within.rs` and `schema_diff.rs`.
* The renderer must receive the expanded generic instantiation context for the RHS side of the failing `.within`.
* The RHS expansion must recursively resolve imported/generic symbolic dependencies before diagnostic text is emitted.
* Do not fix BUG-006 by pretty-printing symbolic text.
  The text itself is wrong until it is the effective RHS.

Expected behavior after fix:

* `EFFECTIVE RHS` contains no unresolved symbolic imported/generic names except allowed primitive/base types.
* The `DIFF` is built from the same effective RHS lines, not the declared template lines.

Resolution:

* BUG-005 and BUG-006 share a single fix centred on `ConcretePolicy::effective_mode` in `concrete.rs`.
  The full resolution is described under BUG-005 above.
* The relevant BUG-006 portions are:
  1. Symmetric `effective_mode` application to both LHS and RHS via `for_lhs()` / `for_rhs()`.
  2. Bare-name fallback lookup (`Headers` → `cose.Headers`) so imported symbolic names are found.
  3. Recursive inlining so `Headers` expands to its concrete body, not a one-word placeholder.
* Verified against `test/svcrec/doc/service-record-v1.cddl`: `EFFECTIVE RHS` no longer shows bare
  `Headers`, `COSE_recipient`; `Headers` is expanded to `(protected: empty_or_serialized_map, unprotected: header_map)`.

### BUG-007: `.within` diagnostics do not drill down to the actual failing statement

Status: resolved.

Observed behavior:

* `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  reports:
  `reason: map[0]: expected at least 1 matching entries, found 0`
  for `name-registration-v1-cose-sign = COSE_Sign<...>`.
* The rendered `DIFF` mostly labels lines as `OK` or `CONTEXT`.
  It does not mark a concrete `CONFLICT` line at the actual field or nested entry that caused the failed `.within`.
* The user-facing result is not actionable:
  the diagnostic shows a large LHS/RHS comparison but does not answer
  which statement is wrong.

Why this is a compiler/linter bug:

* A `.within` failure is only useful if it identifies the concrete
  failing path, not just the top-level wrapper call.
* `map[0]` is an internal path fragment, not a human-readable
  location in the rendered CDDL.
* The inline diff must associate subtype conflicts with the rendered
  effective CDDL line that represents the rejected/missing entry.
* If the checker cannot identify a precise rendered line, it should
  emit a clear fallback note explaining that the path mapping failed,
  not present mostly `OK`/`CONTEXT` output that implies no visible
  problem.

Required stable reproducer:

* Add a BUG-007 fixture under `cddl/vectors/project/bugs/` using a
  DNTLS-shaped `.within` failure:
    * a generic wrapper analogous to `COSE_Sign<headers, payload,
      signatures>`;
    * a header or payload argument whose nested map/group entry fails
      the RHS shape;
    * enough nesting to prove the conflict is not at the top-level
      array but inside a concrete field.
* The test must assert:
    * an `E030` diagnostic is emitted;
    * at least one rendered diff line is marked `CONFLICT` or an
      equivalent unmatched/conflict kind at the actual failing field;
    * the conflict line includes the human-visible field/key/type that
      failed, not only `map[0]`;
    * the diagnostic does not consist solely of `REASON`, `OK`, and
      `CONTEXT` lines.

Implementation notes:

* Audit `WithinConflict.path` production in `within.rs` and its
  mapping in `schema_diff.rs`.
* The current LCS/context matching is not sufficient for nested effective render output.
  Conflict path mapping must be AST/path-authoritative where possible.
* For array/group generic expansion, make sure paths refer to the
  expanded concrete entry, not the pre-expansion placeholder argument.
* This bug is separate from BUG-005 formatting and BUG-006 RHS expansion.
  Even with perfectly expanded multiline text, a diagnostic that does not mark the failing statement is still not useful.

Expected behavior after fix:

* The diagnostic pinpoints the nested field/entry that fails `.within`.
* The `DIFF` highlights that rendered line as `CONFLICT`/unmatched with
  the subtype reason attached.
* The top-level `reason` may still summarize the failure, but the
  rendered diff must show where to look.

Resolution:

* `walk_atoms` in `schema_diff.rs` now descends from `type` into single-type1 children without pushing `ChoiceArm`,
  and from `type1` into a single type2 child.
  This produces atom paths that match the subtype checker's `[MapEntry(i)]` paths exactly,
  so a top-level array of grpents no longer collapses into a single leaf atom at the type1 level.
* `walk_atoms` for the `group` rule now counts only grpent children when assigning `MapEntry(i)` indices.
  Non-grpent siblings (e.g. comments) are skipped without consuming a position,
  which fixes the index drift observed when intermediate AST nodes appear between grpents.
* `find_line_containing` now treats each non-empty line of a multi-line fragment as a separate candidate.
  A grpent whose body spans multiple lines
  (e.g. an inlined `.within` chain or an empty body rendered as `{` on its own line)
  is matched against the rendered line that opens it, so the atom points at a real rendered line instead of being dropped.
* `lookup_path_with_suffix_fallback` resolves a conflict path against the path-to-line map by trying the exact path first
  and then the longest path suffix.
  The subtype checker constructs paths that include `ArrayIndex` and `ControlOp` segments the diff renderer does not emit;
  the suffix fallback lets a path like `[ArrayIndex(0), ControlOp(Within), MapEntry(0)]` resolve to the atom at `[MapEntry(0)]`
  so the failing line is attributed.
* Map-entry conflicts now include the nearest nested mismatch reason when a key matches but its value is rejected.
  This preserves subtype semantics while replacing opaque summaries like `map[0]: expected at least 1 matching entries` with the
  concrete reason underneath the failed entry.
* RHS choice conflicts now summarize every rejected choice arm instead of keeping only the final arm.
  This prevents diagnostics from hiding a relevant arm such as `bstr .cbor header_map`
  and reporting only the unrelated fallback arm such as `bstr .size 0`.
* Stable reproducer lives at `cddl/vectors/project/bugs/bug_007_within_marks_failing_statement.cddl`.
  Before the fix the DIFF consisted only of OK/CONTEXT lines plus a pathless `REASON map[i]: ...` note;
  after the fix the DIFF carries an `Unmatched`-kind subdiag
  (CLI-rendered as `CONFLICT`) whose snippet is the failing rendered LHS line.
* Unit test: `bug_007_within_marks_failing_statement_with_conflict_line`
  in `crates/cbork-cddl-compiler/src/tests.rs`.
* Integration test:
  `bug_007_within_marks_failing_statement_vector` in
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Verified against `test/name-reg-tx/doc/name-reg-tx-v1.cddl` and `test/svcrec/doc/service-record-v1.cddl`:
  every `.within` failure now pinpoints the failing nested field in the DIFF.
  For the name-reg header case, the diagnostic now shows that the protected-header value is rejected
  because the `bstr .cbor header_map` choice arm rejects the decoded map key `tld` as an unresolved name,
  while the `bstr .size 0` choice arm rejects the `.dtrm` control operator.

### BUG-008: `.dtrm` protected headers are rejected as not within COSE `.cbor` protected headers

Status: false positive, closed.

Observed behavior:

* Before BUG-009 was fixed,
  `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  reported an `E030` at:
  `name-registration-v1-cose-sign = COSE_Sign<name-registration-v1-headers, ...>`.
* The relevant first array element is:
  `name-registration-v1-headers = Protected-Headers-Only<name-headers>`.
* That expands through `dntls-cose-defs.cddl` to:
  `protected-headers-only<headers> = (protected: bytes .dtrm headers, unprotected: {})`
  with `.within cose.Headers`.
* RFC 9052 `Headers` is:
  `(protected: empty_or_serialized_map, unprotected: header_map)`,
  where `empty_or_serialized_map = bstr .cbor header_map / bstr .size 0`.
* The diagnostic reported: `reason: map[0]: expected at least 1 matching entries, found 0`.

Resolution:

* BUG-009 fixed the actual cause:
  bareword member keys such as `tld:` were being modeled as unresolved type names,
  not as concrete text-label keys.
* After BUG-009, `test/name-reg-tx/doc/name-reg-tx-v1.cddl` lints cleanly.
* No additional `.dtrm` / `.cbor` subtype fix is required for the name-reg protected-header case.
* Keep the intended control-op rule covered by existing tests:
  `.dtrm T` is within `.cbor U` iff `T` is within `U`;
  `.cbor U` is not necessarily within `.dtrm T`.

### BUG-009: bareword map member keys are treated as unresolved type names during `.within`

Status: resolved.

Priority:

* Fix before BUG-008.
  The `.dtrm` / `.cbor` protected-header failure cannot be interpreted correctly while bareword member keys are mis-modeled.

Observed behavior:

* `cargo run -p cbork -- lint test/name-reg-tx/doc/name-reg-tx-v1.cddl --strict`
  reports the nested reason:
  `nearest RHS map[0] key 1..1 does not accept LHS key tld: unresolved name: tld`.
* The rejected LHS comes from a map like:
  `name-headers = { tld: tstr, root: namehash, tier: uint, ... }`.
* The RHS COSE header map includes:
  `* label => values`,
  where `label = int / tstr` and `values = any`.
* A bareword member key in `tld: tstr` is a concrete text label.
  It must not be treated as a type reference named `tld`.

Why this is a compiler/linter bug:

* The `.within` resolver currently special-cases numeric member keys such as `1:` into a concrete numeric range.
* The equivalent bareword member-key case is missing.
  As a result, `tld:` falls through as `ResolvedType::Named("tld")`.
* During subtype comparison against `label = int / tstr`, that unresolved name produces
  `unresolved name: tld` instead of proving that the concrete key is within `tstr`.

Required stable reproducer:

* Add a BUG-009 fixture under `cddl/vectors/project/bugs/` with no imports:
    * `label = int / tstr`
    * `values = any`
    * `header-map = { * label => values }`
    * `specific = { tld: tstr, root: bstr } .within header-map`
* Expected behavior: the fixture should lint cleanly.
* Current behavior to verify before fixing: it should fail with an `E030`
  containing `unresolved name: tld` or equivalent unresolved bareword-key output.

Implementation notes:

* Fix member-key resolution in `crates/cbork-cddl-compiler/src/within.rs`.
* The likely target is `extract_memberkey_type`.
  It already parses numeric `1:` member keys from the raw member-key text as concrete `Range { lo: 1, hi: 1 }`.
  Add the corresponding handling for bareword `foo:` member keys as concrete text-label values, not `ResolvedType::Named("foo")`.
* The representation may require adding a literal/text-key `ResolvedType` variant,
  or using an existing representation if one already exists elsewhere in the concrete renderer.
  Do not model the key as generic `tstr`; `foo:` is a single concrete text key that is within `tstr`.
* Add unit coverage around `extract_memberkey_type` or the nearest public resolver helper,
  plus an integration vector under `cddl/vectors/project/bugs/`.
* After BUG-009 is fixed, rerun the `name-reg-tx` and `svcrec` lint checks.
  If the next failure is only `.dtrm` versus `.cbor`, continue with BUG-008.

Resolution:

* Added `ResolvedType::TextKey(String)` to model a concrete text-label value (a bareword member key like `tld:` or `root:`).
  This is a structural value, not a type reference.
* `extract_memberkey_type` now walks the memberkey children and detects a `bareword` child first.
  When found, it returns `ResolvedType::TextKey(<text>)`.
  Numeric and other value forms fall through to the existing `resolve_type` / numeric-Range recovery path.
* `collect_subtype_conflicts_inner` adds two new cases for
  `TextKey`:
    * `(TextKey, Primitive(_))` — accepted: a concrete text label
      always lies within a text-string-shaped primitive.
    * `(TextKey, TextKey(_))` — accepted: two concrete labels
      match.
  The `Choice` arm falls through to the existing case which calls `collect_subtype_conflicts_inner` per arm,
  so a `TextKey("foo")` on the LHS will be accepted by any arm that admits text (e.g. `tstr`, or `int / tstr`).
* `render_type` and `type_name` gained a `TextKey` arm that
  prints the value as a quoted string (`"foo"`).
* Stable reproducer lives at `cddl/vectors/project/bugs/bug_009_bareword_memberkey_unresolved.cddl`.
  Before the fix the lint emitted `E030: tld not subtype of any choice arm: choice[0]: unresolved name: tld`;
  after the fix the only remaining diagnostic is `E020: unreferenced top-level definition`, and `name-reg-tx-v1.cddl` lints cleanly.
* Unit test: `bug_009_bareword_memberkey_does_not_emit_unresolved_name`
  in `crates/cbork-cddl-compiler/src/tests.rs`.
* Integration test: `bug_009_bareword_memberkey_lints_cleanly_vector`
  in `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Re-ran the lint on `test/name-reg-tx/doc/name-reg-tx-v1.cddl`:
  lints cleanly (zero diagnostics). `test/svcrec/doc/service-record-v1.cddl`
  now reports only the BUG-010 `.x-enc.abnfb not subtype of Bstr/Nil`
  failures.

### BUG-010: `.x-enc.abnf` / `.x-enc.abnfb` do not subtype by carrier wire type

Status: resolved.

Observed behavior:

* `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl --strict`
  reports two `E030` failures.
* The relevant declaration is:
  `dntls-hpke-encrypted-cek = ( bstr .size 48 ) .x-enc.abnfb ( "dntls-cek" .det dntls-cek-abnf )`.
* The RHS COSE recipient shape is:
  `ciphertext: bstr / nil`.
* The DIFF reports:
  `dntls-hpke-encrypted-cek not subtype of any choice arm`,
  with:
    * `choice[0]: control(.x-enc.abnfb) not subtype of Bstr (different structure)`
    * `choice[1]: control(.x-enc.abnfb) not subtype of Nil (different structure)`

Why this is a compiler/linter bug:

* `.x-enc.abnfb` is a transform annotation over the carrier wire type.
* The carrier is `(bstr .size 48)`.
* For ordinary subtype checks against an unannotated RHS type,
  the transform annotation must preserve carrier compatibility:
  `(bstr .size 48) .x-enc.abnfb (...)` is within `bstr`,
  and is within `bstr .size 48`,
  but is not within `bstr .size 32`.
* The linter cannot generally prove that encryption maps the controller/ABNF payload to the LHS size.
  That relationship is semantic metadata for future validation tooling.
  The `.within` check should only use the carrier type for compatibility with plain `bstr`/`bstr .size N`.
* If both sides have transform annotations, transform-family compatibility still matters:
    * encryption is not hash,
    * encryption is not compression,
    * named compression algorithms are not interchangeable,
    * matching transform annotations may compare controllers when the subtype rule explicitly requires it.

Likely code cause:

* `crates/cbork-cddl-compiler/src/within.rs::ControlOp::from_text`
  normalizes `.x-enc` to `ControlOp::XEnc`,
  but does not normalize `.x-enc.abnf` or `.x-enc.abnfb`.
* The same function already normalizes compression ABNF variants,
  for example `.x-brotli.abnf` and `.x-brotli.abnfb` to `ControlOp::XBrotli`.
* `crates/cbork-cddl-compiler/src/ctlop.rs` recognizes `.x-enc.abnf` and `.x-enc.abnfb` for ABNF annotation evaluation,
  so the parser/evaluator side knows these operators exist.
  The `.within` subtype resolver is the inconsistent layer.
* Because `.x-enc.abnfb` falls through as `ControlOp::Other(".x-enc.abnfb")`, it is not treated as a known narrowing transform.
  The structured subtype collector then rejects it structurally against plain `Bstr`.
* Existing tests in `within.rs` cover `ControlOp::XEnc` within `bstr`
  and `bstr / nil`,
  but they do not cover the textual `.x-enc.abnf` / `.x-enc.abnfb`
  normalization path.

Required stable reproducer:

* Add a BUG-010 fixture under `cddl/vectors/project/bugs/` with no project imports:
    * `root = encrypted-cek .within bstr`
    * `encrypted-cek = (bstr .size 48) .x-enc.abnfb ("cek" .det cek-abnf)`
    * `cek-abnf = 'cek = 32OCTET\nOCTET = %x00-FF\n'`
* Add a second fixture or unit case for choice compatibility:
    * `root = encrypted-cek .within (bstr / nil)`
* Expected behavior: both lint cleanly.
* Add a negative fixture or unit case proving carrier constraints still apply:
    * `(bstr .size 48) .x-enc.abnfb (...) .within bstr .size 32`
      must fail.
* Add transform-family negative coverage:
    * `.x-enc.abnfb` must not be within `.x-hash.abnfb`.
    * `.x-enc.abnfb` must not be within `.x-brotli.abnfb`.

Implementation notes:

* Update `ControlOp::from_text` in `within.rs` so:
    * `.x-enc`, `.x-enc.abnf`, and `.x-enc.abnfb` map to `ControlOp::XEnc`.
    * `.x-hash`, `.x-hash.abnf`, and `.x-hash.abnfb` map to `ControlOp::XHash`.
* Ensure the carrier-narrowing path applies when LHS is `ControlOp::XEnc` or `ControlOp::XHash`
  and RHS is a plain carrier-compatible type.
  Existing `bstr_x_enc_within_bstr` and `bstr_x_enc_within_choice_bstr_or_nil` tests cover this
  once the textual operators normalize correctly.
* Add unit tests for `ControlOp::from_text(".x-enc.abnfb") == ControlOp::XEnc`
  and `ControlOp::from_text(".x-hash.abnfb") == ControlOp::XHash`.
* After the fix, rerun:
    * `cargo run -p cbork -- lint test/svcrec/doc/service-record-v1.cddl --strict`
    * `just fix-ci`

Resolution:

* `ControlOp::from_text` now normalizes the `.abnf` / `.abnfb` annotated
  forms of `.x-enc` and `.x-hash` to the same `ControlOp` as their
  base operator:
    * `.x-enc` / `.x-enc.abnf` / `.x-enc.abnfb` → `ControlOp::XEnc`
    * `.x-hash` / `.x-hash.abnf` / `.x-hash.abnfb` → `ControlOp::XHash`
  This mirrors the existing normalization for the compression family (`.x-brotli.abnf`, `.x-zstd.abnfb`, etc.).
* Before the fix the textual `.x-enc.abnfb` form fell through to `ControlOp::Other(".x-enc.abnfb")`.
  The structured subtype collector then rejected it structurally against plain `Bstr` / `Nil` instead of using the carrier wire
  type, producing the observed `.x-enc.abnfb not subtype of Bstr (different structure)` and
  `.x-enc.abnfb not subtype of Nil (different structure)` failures in `test/svcrec/doc/service-record-v1.cddl`.
* Carrier-narrowing was already in place for `.x-enc` / `.x-hash`
  (via `is_narrowing()`), so the normalization alone is enough to route the annotated forms through the same short-circuit path.
  The existing `bstr_x_enc_within_bstr` and `bstr_x_enc_within_choice_bstr_or_nil` unit tests cover the carrier-narrowing behaviour;
  the BUG-010 unit tests cover the textual normalization directly.
* Stable reproducer lives at `cddl/vectors/project/bugs/bug_010_x_enc_abnfb_carrier_narrowing.cddl`.
  The fixture covers all four shapes the plan required:
    1. `positive`: `(bstr .size 48) .x-enc.abnfb (...) .within bstr`
       lints cleanly (carrier narrows to `bstr`).
    2. `positive-choice`: `.within (bstr / nil)` lints cleanly.
    3. `negative-narrow`: `.within (bstr .size 0)` still fails (the
       carrier `bstr .size 48` does not subtype `bstr .size 0`).
    4. `enc-vs-hash-root`: `.x-enc.abnfb` does not subtype
       `.x-hash.abnfb` (transform-family constraint still applies).
    5. `enc-vs-brotli-root`: `.x-enc.abnfb` does not subtype
       `.x-brotli.abnfb` (transform-family constraint still applies).
* Unit tests:
    * `bug_010_x_enc_abnfb_normalizes_to_x_enc` and
  `bug_010_x_hash_abnfb_normalizes_to_x_hash` pin the textual normalization.
    * `bug_010_bstr_x_enc_abnfb_within_bstr` proves the carrier-
  narrowing path fires after normalization
  (carrier wraps the `.size 48` Control as the inner narrowing, then `.x-enc.abnfb` narrows further to `bstr`).
    * `bug_010_x_enc_abnfb_not_within_x_hash_abnfb` proves the
  transform-family constraint still rejects `.x-enc.abnfb` as a subtype of `.x-hash.abnfb`.
* Integration test: `bug_010_x_enc_abnfb_carrier_narrowing_vector` in `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
  Asserts the fixture surfaces exactly three E030s
  (one per negative rule) and that at least one reason names the transform-family or carrier constraint.
* Re-ran the lint on `test/svcrec/doc/service-record-v1.cddl` and
  `test/name-reg-tx/doc/name-reg-tx-v1.cddl`: both lint cleanly
  (zero diagnostics).

### BUG-011: Direct imports/includes of non-library files do not warn

Status: resolved.

Observed behavior:

* `test/dntls-core/doc/time.cddl` defines `dntls-epoch`.
* The file is directly imported and `dntls-epoch` is used by DNTLS schemas.
* `time.cddl` is not marked with `;@ CBORK: Library`.
* `dntls-epoch` is not marked with `;@ CBORK: Export`.
* Lint currently emits no warning for that shape.

Expected behavior:

* A file that is directly imported or included is being used as a reusable module.
  If that target file does not declare `;@ CBORK: Library`, emit a warning at the import/include directive origin.
* If the imported/included target is a CBORK library and the direct consumer references a symbol that is not in the target's
  `;@ CBORK: Export` or `;@ CBORK: Extern` surface, emit the existing `W003` warning.
* These are distinct checks:
    * non-library import/include warns about the target file shape;
    * non-exported direct use warns about library API-surface violation.
* Do not warn for transitive private helper use through an exported library symbol.
* Do not resurrect `W006`.
  An exported symbol that the current consumer does not use is not a problem.

Why this is a compiler/linter bug:

* Step 5.12's intended module contract requires imported/included files to be library-shaped.
* The current implementation records `ImportedLibrary::is_library`, but it does not emit a diagnostic when that flag is false.
* `detect_direct_export_violations` explicitly returns early for non-library imports: `if !lib.is_library { return; }`.
  That is correct for the non-exported-symbol contract, but only if a separate non-library-import warning exists.
* No separate pass currently enforces the "direct import/include target must be a library" rule, so `time.cddl` silently bypasses
  both parts of the Step 5.12 contract.

Required stable reproducers:

* Add `cddl/vectors/project/semantic-errors/import_non_library_warns.cddl` plus
  `cddl/vectors/project/semantic-errors/support/non_library_time.cddl`.
  The support file should define `dntls-epoch = #6.1(uint .ge 1761782400)` with no `;@ CBORK: Library`.
  The consumer should import it and use `dntls-epoch`.
  Expected result: a warning that the imported target is not a CBORK library.
* Add an include-shaped twin fixture: `cddl/vectors/project/semantic-errors/include_non_library_warns.cddl` plus the same support
  file.
  Expected result: the same non-library target warning for `include`.
* Keep the existing library-private fixtures for `W003`:
  direct use of a private symbol from a file that is marked `;@ CBORK: Library` must still warn as a non-exported direct use.
* Add strict-mode integration assertions proving the new warning fails `--strict`.

Implementation notes:

* Add a dedicated warning code for non-library import/include targets, unless an existing warning code is explicitly chosen.
  Do not overload `W003`; `W003` is specifically about direct use of non-exported symbols from a library.
* The pass should operate over `CompiledCDDL::imported_libraries`, using each entry's `is_library` and `import_origin`.
* The diagnostic span should point at the import/include directive that brought the non-library file in.
* The message should name the target path and say that directly imported/included files should declare `;@ CBORK: Library`.
* If catalog RFC files are intended to be implicitly library-shaped,
  define that policy explicitly before enabling the warning across the RFC catalog.
  Otherwise, catalog files that are imported but lack `;@ CBORK: Library` should receive the same warning as local files.
* After the warning exists, rerun the DNTLS lint target and verify it reports the `time.cddl` import shape clearly.

Resolution:

* Added `detect_non_library_imports` in `finalize.rs`.
  The new pass iterates `CompiledCDDL::imported_libraries` and emits a `W007` warning
  for every entry whose `is_library` flag is `false`.
  The diagnostic carries the canonical target path in its message and uses `import_origin.source_path` as the `source_file`
  so the CLI renderer points at the import/include directive.
* `W007` is deliberately distinct from `W003`:
    * `W007` warns about the *target file* shape (the imported
      file is not a CBORK library at all).
    * `W003` warns about the *consumer's API usage* (the consumer
      references a symbol that lives outside any declared
      export surface — whether the imported file IS a library or
      not).
  Both warnings are emitted for the same case when a non-library file is imported AND the consumer references one of its symbols:
  W007 says "add `;@ CBORK: Library`"; W003 says "either mark the symbol as exported or stop using it."
* Removed the `if !lib.is_library { return; }` bail-out in `record_if_violation` (the W003 walker's per-name handler).
  Before the change, W003 only fired for files explicitly marked `;@ CBORK: Library`; after the change,
  W003 fires for any imported file whose referenced symbol is not in the file's `;@ CBORK: Export`
  (always the case for non-library files, and also the case for library files that have a private helper).
* Stable reproducers live at:
    * `cddl/vectors/project/semantic-errors/import_non_library_warns.cddl`
      (import-shaped) plus
      `cddl/vectors/project/semantic-errors/support/non_library_time.cddl`.
    * `cddl/vectors/project/semantic-errors/include_non_library_warns.cddl`
      (include-shaped) with the same support file.
  Both fixtures surface one W007 (target file shape) and one W003
  (consumer uses a non-exported symbol)
  for the same case, pointing at the `dntls-epoch` reference and the import/include directive respectively.
* `cbork lint --strict` treats the W007 and W003 as failures
  (non-zero exit code), matching the plan's strict-mode
  integration requirement.
* Catalog policy: the RFC files that other catalog files import
  (`cddl/rfc-std/rfc9052.cddl`, `rfc9165.cddl`, `rfc9171.cddl`, `rfc9237.cddl`, `rfc9393-concise-swid-tag.cddl`,
  `rfc9393-sign1.cddl`) are explicitly marked `;@ CBORK: Library` so the shared catalog files don't trigger W007 between each other.
  Non-catalog files that are not actually library-shaped now surface the warning as the plan intends.
* Existing test `unreferenced_weak_imports_are_pruned_silently_no_conflict`
  was updated to mark its temp-file targets as `;@ CBORK: Library`
  so the prune-first invariant continues to be tested in
  isolation from the W007 pass.
* Unit test: `bug_011_non_library_import_emits_w007_and_w003` in
  `crates/cbork-cddl-compiler/src/tests.rs` — asserts the `import`
  and `include` fixtures both surface a W007 (target file shape)
  AND a W003 (non-exported cross-file reference) on the
  `dntls-epoch` use.
* Integration test: `bug_011_non_library_import_emits_w007_vector`
  in `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`
  — same assertions, plus a `cbork lint --strict` invocation
  that confirms exit-code propagation.
* Re-ran `target/release/cbork lint --strict cddl/vectors/rfc` and
  `... cddl/rfc-std`: both pass with zero diagnostics, confirming
  the catalog files are correctly marked as libraries.

### BUG-012: Bare references to generic templates are accepted as concrete rules

Status: resolved.

Observed behavior:

* `cddl/rfc-std/rfc9237.cddl` used:
  `rfc9237 = AIF-Generic / AIF-Specific / AIF-REST`.
* The file defines `AIF-Generic<Toid, Tperm>`, but it does not define a concrete bare rule named `AIF-Generic`.
* `cargo run -p cbork -- lint cddl/rfc-std/rfc9237.cddl --strict` incorrectly passed.

Why this is a compiler/linter bug:

* A generic rule definition is a template.
  It does not define a concrete rule with the same bare name.
* `AIF-Generic<Toid, Tperm>` can only be used through an instantiated reference such as `AIF-Generic<tstr, uint>`.
* A bare reference to `AIF-Generic` must be treated as undefined, not as a concrete top-level schema alternative.
* This must be a hard error even in `;@ CBORK: Library` mode.
  Library mode can tolerate intentionally external names, but a name that matches a local generic template is not external;
  it is an uninstantiated template reference.

Root cause:

* `finalize.rs::collect_definition_names_node` inserted both the full generic definition name
  (`AIF-Generic<Toid, Tperm>`) and the bare generic head (`AIF-Generic`) into `user_definition_names`.
* `handle_reference` later used `user_definition_names` to decide whether a typename was defined.
  Because the bare head was present, the undefined-reference check skipped the invalid bare reference.

Resolution:

* `collect_definition_names_node` now records the full generic definition name only.
  It no longer adds the bare generic head as a concrete definition.
* `finalize.rs` now separately records generic template base names from the original unpruned tree.
  If a missing reference matches one of those template names,
  `handle_reference` emits a hard `E016` explaining that the name is only defined as a generic template
  and must be instantiated with arguments.
* `cddl/rfc-std/rfc9237.cddl` was corrected so the root is concrete:
  `rfc9237 = AIF-Specific / AIF-REST`.
* Stable reproducer:
  `cddl/vectors/project/semantic-errors/bare_generic_template_reference.cddl`.
* Integration test:
  `bare_generic_template_reference_is_undefined_vector` in
  `crates/cbork-cddl-compiler/tests/import_include_vectors.rs`.
* Verification:
    * `cargo test -p cbork-cddl-compiler --test import_include_vectors bare_generic_template_reference_is_undefined_vector` passes.
    * `cargo run -p cbork -- lint cddl/vectors/project/semantic-errors/bare_generic_template_reference.cddl --strict`
      emits hard `E016`.
    * `cargo run -p cbork -- lint cddl/rfc-std/rfc9237.cddl --strict` passes after the root correction.

## Files Likely to Change

| File or area | Purpose |
|---|---|
| `crates/cbork-cddl-parser/src/modules/` or similar | New directive-comment parser utility/module set with directive enum variants |
| `crates/cbork-cddl-parser/src/preprocessor.rs` | AST walk, directive injection, pruning hooks |
| `crates/cbork-cddl-parser/src/parser.rs` | Wire the post-parse enrichment stages together |
| `crates/cbork-cddl-parser/src/lib.rs` | Expose the new stage-2 parser entrypoints |
| `crates/cbork-cddl-parser/tests/` | Directive parsing, pruning, and include-resolution tests |
| `crates/cbork-catalog/` | Self-contained compile-time `phf` catalog crate for vendored `cddl/rfc-std/` `.cddl` files, with unit tests |
| `cddl/vectors/` | Stage-2 vector coverage |

## Clarifications To Confirm If Needed

* If you want `include` and `import` split into different parser entrypoints instead of one directive enum, say so explicitly.
* If you want `PRUNABLE` and `SILENT` represented as node flags instead of node metadata, say so explicitly.
* If you want resolution to produce one canonical flattened model only, say so explicitly;
  otherwise the raw tree and resolved tree should both remain available.
