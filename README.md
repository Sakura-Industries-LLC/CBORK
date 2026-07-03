<!-- rumdl-disable MD033 -->

<p align="center">
  <img src="assets/cbork.jpg" alt="cbork logo" width="220">
</p>

<!-- rumdl-enable MD033 -->

# C.B.O.R.K - CBOR Kit

`cbork` is a Rust workspace for CDDL and CBOR tooling: a CDDL linting CLI, a CDDL compiler with control-operator semantics,
and the supporting parser and shared-utility crates.

## Versioning

This project does **not** use [Semantic Versioning][semver].
See [FEELSVER.md](FEELSVER.md) for the versioning policy.

## Workspace

The repository is a Cargo workspace organised as:

* `cbork` — command-line tool for CDDL linting, rendering, and validation
* `cbork-cddl-compiler` — CDDL compiler and semantic resolver
* `cbork-cddl-parser` — CDDL parser and validate entry point
* `cbork-catalog` — compiled catalog of well-known CDDL module names
* `cbork-abnf-parser` — ABNF (RFC 5234) parser used by CDDL `.abnf` annotations
* `cbork-edn` — CBOR Extended Diagnostic Notation (EDN) support
* `cbork-utils` — shared CBOR encode/decode helpers

## Install (users)

The binary crate is `cbork`.
To build and install it from this repository:

```shell
cargo install --path crates/cbork
```

Or just run the CLI straight from the source tree:

```shell
cargo run -p cbork -- --help
```

## Quick start

```shell
# Lint a .cddl file
cargo run -p cbork -- lint path/to/schema.cddl

# Show the effective CDDL that the compiler actually reasons about
cargo run -p cbork -- render path/to/schema.cddl
```

The full usage surface is documented in the `cbork` crate README.

## Features

* **CDDL linting** — `cbork lint` checks CDDL documents for parse errors, semantic errors, and warnings.
* **Effective rendering** — `cbork render` expands named rules, generics, sockets, plug choices,
  and nested control operators into a readable concrete view; the same renderer drives `.within` and `.and` diagnostics.
* **Custom control operators** — support for the full RFC 8610 / RFC 9165 operator set plus an unofficial CBOR-ecosystem annotation
  family (`.x-enc`, `.x-hash`, `.x-compressed`, `.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate`).
* **CBORK compiler directives** — `;@ CBORK: Library` / `;@ CBORK: Export` / `;@ CBORK: Extern` extend CDDL with first-party
  library-level intent declarations.
* **EDN rendering for raw CBOR** — `cbork-edn` decodes raw CBOR bytes
  (single items or concatenated sequences) into an owned EDN-like tree.

## Custom control operators

`cbork-cddl-compiler` understands the full [RFC 8610][rfc8610] / [RFC 9165][rfc9165] control-operator set plus a handful of
unofficial annotations the CBOR community has standardised informally.
Each operator is recognised as a separate transform family so that `.within` checks compare both the carrier
and the transform identity.

[rfc8610]: https://www.rfc-editor.org/rfc/rfc8610
[rfc9165]: https://www.rfc-editor.org/rfc/rfc9165

### Why these exist

The standard CDDL control operators cover the cases CBOR-native data needs
(`.cbor`, `.size`, `.bits`, numeric bounds, …)
but not the cases the CBOR ecosystem has settled on informally: encrypted blobs (`.x-enc`), hashes (`.x-hash`),
and the many flavours of compression (`.x-brotli` / `.x-zstd` / `.x-gzip` / `.x-deflate`).
`cbork` lets CDDL authors express those without abandoning `.within` semantics, by giving each annotation its own transform family.

### Built-in (RFC 8610) operators

| Operator     | Carrier    | Effect                                                                |
| ------------ | ---------- | --------------------------------------------------------------------- |
| `.cbor`      | `bstr`     | Validate the `bstr` as a well-formed CBOR document.                    |
| `.dtrm`      | `bstr`     | Stricter CBOR check (`.cbor` ⊇ `.dtrm` — narrower). *DRAFT OPERATOR*  |
| `.cborseq`   | `bstr`     | Validate as a definite-length CBOR sequence.                           |
| `.dtrmseq`   | `bstr`     | Stricter CBOR sequence check. *DRAFT OPERATOR*                         |
| `.prefp`     | `bstr`     | Preferred CBOR serialization check. *DRAFT OPERATOR*                   |
| `.prefpseq`  | `bstr`     | Preferred CBOR sequence serialization check. *DRAFT OPERATOR*          |
| `.size N`    | `bstr`/`tstr` | Length must equal `N`.                                             |
| `.bits NAME` | `uint`     | Bit-layout refinement via a named bit map.                             |
| `.gt` / `.ge` / `.lt` / `.le` | numeric | Strict / non-strict numeric bound.                       |

<!-- rumdl-disable MD013 -->

The serialization-oriented operators (`.cbor`, `.cborseq`, `.prefp`, `.prefpseq`, `.dtrm`, and `.dtrmseq`) accept (an unofficial) `any` or (official) `bstr` carriers and a controller that may be a scalar, array, map/group, or wildcard shape.
The `any` permissive carrier rule is intentional.
It supports schemas such as `wrapped = any .dtrm type2`, where the source text wants to say "some representation that deterministically serializes as `type2`" without first narrowing the carrier to `bstr`.

<!-- rumdl-enable MD013 -->

### Encryption / hash annotation family

`.x-enc` and `.x-hash` are unofficial wrappers from the CBOR ecosystem that narrow `bstr` to "the encryption of `T`"
and "the hash of `T`" respectively.
They share a single transform family each — `.x-enc` and `.x-hash` are not mutually within each other,
and neither is within any compression annotation.

<!-- rumdl-disable MD040 -->

```cddl
encrypted = bstr .x-enc payload
hashed     = bstr .x-hash payload
```

<!-- rumdl-enable MD040 -->

The LHS rule with a `.x-enc` controller subtypes the bare `bstr` carrier on the RHS:

<!-- rumdl-disable MD040 -->

```cddl
ok = (bstr .x-enc payload-narrow) .within bstr   ; always true (carrier narrows)
ok = (bstr .x-enc payload-narrow) .within (bstr .x-enc payload-wide)
```

<!-- rumdl-enable MD040 -->

### Compression annotation family

Compression annotations are organised into the generic wrapper (`.x-compressed`) and the per-algorithm wrappers
(`.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate`).
Each algorithm is within the generic wrapper when its controller subtypes the RHS controller:

<!-- rumdl-disable MD040 -->

```cddl
brotli-bstr = bstr .x-brotli payload
any-zstd    = bstr .x-zstd   payload-wide

ok = brotli-bstr .within (bstr .x-compressed payload-wide)
```

<!-- rumdl-enable MD040 -->

Two different algorithms are NOT mutually within each other
(`bstr .x-brotli T` is not within `bstr .x-zstd T`), and the generic wrapper is NOT within any specific algorithm.

The `.abnf` / `.abnfb` annotated forms (`.x-compressed.abnf`, `.x-brotli.abnfb`, …) collapse to the same transform family
for `.within` subtype purposes while still preserving enough detail for literal/ABNF validation.
This matches the unofficial CBOR convention.

### Transform compatibility matrix

The full compatibility matrix that drives `.within` subtype checks is summarised below.
Rows are the LHS operator, columns are the RHS operator:

|                   | `bstr` (carrier) | `.x-enc`           | `.x-hash`           | `.x-compressed`      | `.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate` |
| ----------------- | ---------------- | ------------------ | ------------------- | -------------------- | ------------------------------------------ |
| `.x-enc`          | ✓                | ✓ (controllers)    | ✗                   | ✗                    | ✗                                         |
| `.x-hash`         | ✓                | ✗                  | ✓ (controllers)     | ✗                    | ✗                                         |
| `.x-compressed`   | ✓                | ✗                  | ✗                   | ✓ (controllers)      | ✗                                         |
| `.x-brotli` etc.  | ✓                | ✗                  | ✗                   | ✓ (controllers)      | ✓ same algorithm, ✗ different algorithms  |

`✓` means the LHS subtypes the RHS.
`✗` means the LHS is not within the RHS and `.within` emits a control-mismatch diagnostic (`E030`)
that names the incompatible operators.

<!-- rumdl-enable MD040 -->

is structurally well-formed.
The `any` on the controller side is the common way to write "a CBOR document holding anything" without forcing the CDDL author to
commit to a specific inner schema at the use site.

## `;!` documentation comments

A `;!`-prefixed comment line is a **markdown-formatted documentation comment**.
The body of the comment is interpreted as CommonMark.

A contiguous run of `;!` lines forms a *documentation block*; blank lines or CDDL definitions break that contiguity.

Comment blocks that appear at the top of a CDDL file, or disconnected from any CDDL definition are general document comments.

A comment block is bound to the next CDDL definition in the file if it is directly attached
(skipping any plain `;`, `;@`, and `;#` comments in between), which becomes the "documented definition" for that block.

This is what the user sees in the optional documentation lint pass
(`cbork lint --doc`):
the tool synthesizes the documentation blocks into a Markdown document
(with the definitions preserved at their annotated positions),
runs [`rumdl`][rumdl] against the result, and translates the lint warnings back to CDDL-anchored diagnostics.
A module-level (file-level) documentation block must open with a level-1 heading; exported
(`;@ CBORK: Export`) definitions must each carry their own definition-level block.

[rumdl]: https://github.com/Sakura-Industries-LLC/rumdl

Example:

<!-- rumdl-disable MD040 -->

```cddl
;! # Person types
;!
;! CDDL file which defines people data objects.
; ^^ This is a file level comment and does not attach to any definition.
; These NORMAL CDDL comments are not part of "documentation" and are internal comments only.

;! ## Payload
;!
;! These are **person** type payloads.
; ^^ Defining a section of the this CDDL file.

; vv This must be directly attached to `person` or it is not considered a comment for that definition.
;! ### `person` payload
;!
;! Carries a single person record with a canonical `name`, an `age` in
;! years, and an optional contact `email`.
; This comment is not documentation, but does not break linkage of the comment block to the definition
person = {
  name: tstr,    ; This is not a documentation comment, its ignored for documentation purposes.
  age:  uint,    ;! Documentation comments can not attach to the RHS of any definition,
  ? email: tstr, ;!   so these will emit a warning and be ignored for documentation purposes.
}
```

<!-- rumdl-enable MD040 -->

## `;#` module inclusion (`include` / `import`)

`cbork-cddl-compiler` implements the module-system syntax from the
[CDDL Modules draft][cddl-modules]: `;# include …` and `;# import …`,
both supporting the `from …` and `as …` clause options.

The target filename is classified by how it is written:

* **Unquoted base names** (e.g. `rfc9052`) are *default standard CDDL
  documents* and resolve through the compile-time catalog
  ([`cddl/rfc-std/`][cddl-rfc-std]).
  Two imports of the same base name produce rules with the same source
  origin (`catalog:<name>`), so re-imports compose cleanly.
* **Quoted relative paths** (e.g. `"./somedir/file.cddl"` or
  `"../common.cddl"`) are resolved **relative to the CDDL file in
  which the directive appears**, and the file content is read from the
  local filesystem.
* **Quoted absolute paths** (anything starting with `/`, e.g.
  `"/repo/root/file.cddl"`) are resolved against the logical `root_path`
  passed to the compiler, which anchors absolute includes to a known
  filesystem root so the build is reproducible.

Every form is read from the local filesystem
(or from the in-tree catalog for base names); `cbork` never downloads or fetches from a network.
An unresolved file produces `E009`; a failed read produces `E011`.

Example:

<!-- rumdl-disable MD040 -->

```cddl
;# include "./common/common-types.cddl"
;# import cose_sign from "rfc9052" as sign
;# import rfc8610
```

<!-- rumdl-enable MD040 -->

## `;@ CBORK:` compiler directives

CBORK uses `;@`-prefixed comment annotations to express library-level intents that the CDDL grammar cannot encode.
All CBORK directives live in the `CBORK:` namespace;
directives from any other namespace are surfaced as warnings so users notice that their tool annotation was ignored.

### Recognized directives

| Directive                              | Effect                                                                          |
| -------------------------------------- | ------------------------------------------------------------------------------- |
| `;@ CBORK: Library`                    | Marks this file as a reusable library module. Must appear before any non-comment content. |
| `;@ CBORK: Export`                     | Marks the **next rule** as part of the library's public export surface. The compiler tags the rule with `MetaData::Exported` and records the name in `CompiledCDDL::exported_names`. |
| `;@ CBORK: Extern <name>,...`          | Declares names that this library treats as external. Requires `;@ CBORK: Library` in the same file. |

Example:

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Library

;@ CBORK: Export
public-rule = uint

private-helper = bstr .size 16  ; not exported, internal use only
```

<!-- rumdl-enable MD040 -->

A `;@ CBORK: Export` must be followed (after any whitespace or doc comments) by a single rule.
The following situations are all rejected with `E022`:

* `;@ CBORK: Export` in a non-library file.
* `;@ CBORK: Export` with no following rule (EOF).
* `;@ CBORK: Export` immediately before an `import` / `include` directive comment.
* Two consecutive `;@ CBORK: Export` directives with no rule between them.

### Directive hygiene

Unknown CBORK directives (e.g. `;@ CBORK: Thing`) emit an `E021` diagnostic
because they look like active CBORK processing directives but are not recognized.
Directives from any other namespace (e.g. `;@ OTHER: ...`) emit a `W002` warning so users notice
that their tool annotation was ignored:

<!-- rumdl-disable MD040 -->

```cddl
;@ OTHER: do-something   ; ignored; emits W002
```

<!-- rumdl-enable MD040 -->

The recognized CBORK directive set is small and stable — see the table above.
Adding a new directive is a small change in `crates/cbork-cddl-compiler/src/compiled.rs`
(the `parse_cbork_comment` function and the `CborkDirective` enum).

### Cross-file direct-use export contract

When a file directly imports or includes another file that is marked as a CBORK library,
any reference from the consumer's own rules to a symbol defined in that library must point at an exported
(`;@ CBORK: Export`) or externally-declared (`;@ CBORK: Extern`) name.
Direct references to non-exported helpers emit a `W003` warning; strict mode fails on that warning.

The contract does not apply when:

* the consumer references the imported name transitively through an exported symbol
  (the helper is private but reachable through the library's own exported surface);
* the imported module is not marked as a CBORK library (unlabelled imports and unlabelled includes never participate);
* the consumer itself has declared the symbol as `;@ CBORK: Extern <name>` (the consumer has opted in to that name explicitly);
* the reference resolves to a postlude-injected primitive
  (e.g. `uint`, `bstr`, `any`) rather than a definition from the imported file.

### Unused import / include warnings

In addition to the direct-use contract,
the compiler emits warnings when a consumer imports or includes symbols that it never references:

| Code  | Trigger |
| ----- | ------- |
| `W004` | The whole `import` / `include` directive contributes no referenced symbol.  For `from` directives this fires when every name in the `from` clause is unused. |
| `W005` | A specific selected name on a `from` clause is never referenced.  Fires per name, even when the directive as a whole is partially used. |

The unused checks walk the consumer's own rules in the post-prune tree, so references that the reachability pruner dropped
(or references inside the imported rules themselves) do not falsely satisfy the "used" check.

Library exports are public API surface, not a required-use contract.
A whole-library import is valid when the consumer references any imported symbol it needs;
unused sibling exports from that library do not produce a consumer-side warning.

## Repository layout

```text
crates/                  first-party Rust workspace (cbork, cbork-cddl-*, cbork-*, cbork-utils)
cddl/                    bundled CDDL support data: standard-library CDDL (cddl/rfc-std/) and test vectors (cddl/vectors/)
rfc/                     literal copies of bundled RFC / Internet-Draft text (IETF Trust terms)
extension/zed/           Zed editor extension for CDDL (grammar is pulled at install time, not vendored — see extension/zed/README.md)
plans/                   release / restructure plans tracking in-flight work
```

## Develop

To work on this repository locally you need:

* [`just`](https://github.com/casey/just) — the task runner that drives everything below
* [Podman][podman] — the container runtime used by the shared validation flow
* the [`sakura-dev-tools`][sakura-dev-tools] build image, which provides
  `moon`, `rustup`/`cargo`, `cargo deny`, `cargo-nextest`, `rumdl`, and
  `cspell` (the `just` recipes below build it on demand)

The shared validation flow is containerized:

```shell
just fix-ci
```

This builds the shared tools image if needed, then runs `moon run :fix && moon run :ci` inside the container.
For a single side of that flow, use `just fix` or `just ci`.
To bypass the container and run moon directly on the host, use `just local fix` / `just local ci`.

The `cbork` Rust crate, the `cbork-cddl-*` Rust crates,
and the supporting documentation under `rfc/` and `cddl/` are worked on through the same container flow.
The tree-sitter grammar under `extension/zed/grammars/cddl/` is a separate subproject
(its own container toolchain; see its `AGENTS.md`).

[podman]: https://podman.io/
[sakura-dev-tools]: https://codeberg.org/SakuraIndustries/sakura-dev-tools

## Testing and validation

`just fix-ci` runs:

* `cargo fmt --check` and `cargo clippy -- -D warnings`
* license checks (`cargo deny check licenses bans` and `cargo deny check advisories`)
* the cbork crate's release build and the full `cargo nextest` suite
* markdown lint (`rumdl check`) and spell-check (`cspell`) across the documentation
* a release build of the `cbork` binary and a strict lint pass over `cddl/rfc-std/`

## License

This workspace is a mixed-license project.
Each first-party crate is licensed under the expression declared in its `Cargo.toml`,
which is the authoritative source for that crate's license.
The [LICENSE](LICENSE) at the repository root summarizes the structure and states the default license for directories
that do not carry their own `LICENSE` file.

| Crate                 | License             | Local license file(s)                            | Full text            |
| --------------------- | ------------------- | ------------------------------------------------ | -------------------- |
| `cbork`               | `AGPL-3.0-only`     | `crates/cbork/LICENSE`                           | root `LICENSE-AGPL-3.0` |
| `cbork-cddl-compiler` | `MPL-2.0`           | `crates/cbork-cddl-compiler/LICENSE`             | root `LICENSE-MPL-2.0` |
| `cbork-catalog`       | `MPL-2.0`           | `crates/cbork-catalog/LICENSE`                   | root `LICENSE-MPL-2.0` |
| `cbork-edn`           | `MPL-2.0`           | `crates/cbork-edn/LICENSE`                       | root `LICENSE-MPL-2.0` |
| `cbork-cddl-parser`   | `MIT OR Apache-2.0` | `crates/cbork-cddl-parser/LICENSE-MIT`, `LICENSE-APACHE` | each file |
| `cbork-abnf-parser`   | `MIT OR Apache-2.0` | `crates/cbork-abnf-parser/LICENSE-MIT`, `LICENSE-APACHE` | each file |
| `cbork-utils`         | `MIT OR Apache-2.0` | `crates/cbork-utils/LICENSE-MIT`, `LICENSE-APACHE` | each file |

Bundled RFC and Internet-Draft text under `rfc/`
and `rfc/related/` is redistributed verbatim under the [IETF Trust License: Terms for Reproducing RFCs and Drafts][ietf-trust];
see `rfc/README.md`.
The CDDL standard-library files under `cddl/rfc-std/` are from [`cabo/cddlc` data][cddlc-data]
and are covered by the MIT `LICENSE` in that directory.

[semver]: https://semver.org/
[ietf-trust]: https://trustee.ietf.org/license/
[cddlc-data]: https://github.com/cabo/cddlc/tree/master/data
[cddl-modules]: https://datatracker.ietf.org/doc/draft-ietf-cbor-cddl-modules/
[cddl-rfc-std]: https://github.com/cabo/cddlc/tree/master/data

## Contributing

Issues are intentionally disabled.

See [CONTRIBUTING](CONTRIBUTING.md) for details if you wish to contribute to this project.
