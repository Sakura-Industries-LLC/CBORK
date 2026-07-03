# README and Agent File Cleanup Plan

This plan covers release-oriented documentation cleanup for the root README, crate READMEs, fixture/tooling READMEs,
and assistant guidance files.
The goal is to make the repository understandable to new users, contributors, release reviewers,
and automated assistants without broad refactoring.

## Current Snapshot

Workspace crates:

| Path | Package | README status | Cleanup need |
| --- | --- | --- | --- |
| `crates/cbork` | `cbork` | Has `README.md` | Review for CLI usage, examples, license, and release accuracy. |
| `crates/cbork-abnf-parser` | `cbork-abnf-parser` | Missing | Add brief informative crate README. |
| `crates/cbork-catalog` | `cbork-catalog` | Missing | Add brief informative crate README. |
| `crates/cbork-cddl-compiler` | `cbork-cddl-compiler` | Missing | Add brief informative crate README. |
| `crates/cbork-cddl-parser` | `cbork-cddl-parser` | Missing | Add brief informative crate README. |
| `crates/cbork-edn` | `cbork-edn` | Missing | Add brief informative crate README. |
| `crates/cbork-utils` | `cbork-utils` | Missing | Add brief informative crate README. |

Other README and agent files:

| Path | Status | Cleanup need |
| --- | --- | --- |
| `README.md` | Exists | Needs release-ready structure, current crate list, install/use examples, status, license table, and links to crate docs. |
| `AGENTS.md` | Exists | Appears copied from another repo: references `catalyst-libs`, `rust/`, docs/spec paths that are not present here. |
| `extension/zed/grammars/cddl/AGENTS.md` | Exists | Review for consistency with the embedded grammar project and parent repo guidance. |
| `cddl/README.md` | Exists | Review for fixture/source organization and release relevance. |
| `cddl/rfc-std/README.md` | Exists | Keep attribution/licensing clear. |
| `cddl/vectors/**/README.md` | Exists in many fixture directories | Review only for stale or contradictory wording; avoid churn. |
| `tools/stage2/README.md` | Exists | Review if release workflow references stage2 tools. |

## Phase 1: Define Documentation Targets

1. The intended audience for the root README is all of these:
    * users installing or running `cbork`
    * Rust developers using the library crates
    * contributors working on CDDL/CBOR parsing
    * release reviewers checking licenses and package layout
2. Any crate present can be a public crate.
3. Currently no crate are for crates.io that will happen in a much later phase.
4. `extension/zed/grammars/cddl` MUST be documented as part of this repo.

Expected output:

1. A consistent README structure.
2. Crate READMEs that match package metadata.
3. Agent instructions that match this repository, not the source repo they were copied from.

## Phase 2: Clean Up the Root README

Recommended root README structure:

1. Project name and one-paragraph summary.
2. Current status and release maturity.
3. Installation or build instructions.
4. CLI quick start with one or two realistic commands.
5. Workspace crate overview.
6. Feature overview:
    * CDDL parsing and linting
    * compiler directives
    * `.within` and control-operator support
    * EDN/CBOR tooling if user-facing
    * Zed/tree-sitter support if in scope
7. Repository layout.
8. Development workflow.
9. Testing and validation commands.
10. License table.
11. Contributing/support policy.

Specific cleanup items:

1. Add missing `cbork-edn` to the workspace overview.
2. Keep the custom control-operator documentation,
   but consider moving detailed operator matrices into a dedicated docs file if the root README becomes too long.
3. Remove duplicate examples where the same `.cbor any` snippet appears twice.
4. Replace extraction-era language with release-ready status wording.
5. Make the license section match the final license plan exactly.
6. Link to each crate README once those exist.
7. Include commands that are known to work from the repo root.

## Phase 3: Add Crate READMEs

Every workspace crate should have a brief, informative `README.md`.
Each crate README should be short enough to maintain, but useful on crates.io and in source browsing.

Recommended template:

```md
# crate-name

One-sentence description of what the crate provides.

## Purpose

Explain how this crate fits into the CBORK workspace.

## Usage

Show the most important public entrypoint or state that this is primarily used by sibling crates.

## License

State the crate license and point to local license files.
```

Crate-specific content:

| Package | README should explain |
| --- | --- |
| `cbork` | CLI purpose, basic lint/validate commands, relationship to compiler/parser crates, AGPL license. |
| `cbork-abnf-parser` | ABNF parser role, RFC 5234/related grammar support, relationship to CDDL `.abnf` handling. |
| `cbork-catalog` | Generated or compiled well-known-name catalog role and whether it is an internal support crate. |
| `cbork-cddl-compiler` | Semantic compilation/resolution, directives, imports/includes, diagnostics, relation to parser crate. |
| `cbork-cddl-parser` | Syntax parsing entrypoints, AST role, grammar coverage, dual license. |
| `cbork-edn` | CBOR extended diagnostic notation support and relationship to CLI or future tooling. |
| `cbork-utils` | Shared CBOR encode/decode helpers and intended use by sibling crates. |

After adding crate READMEs:

1. Add `readme = "README.md"` to each crate manifest intended for publication.
2. Add `description` to each crate manifest.
3. Run `cargo package --list -p <crate>` for each publishable crate and verify README inclusion.

## Phase 4: Update `AGENTS.md`

The root `AGENTS.md` should be rewritten to match this repository.

Required corrections:

1. Replace `catalyst-libs` with `cbork`.
2. Replace references to a `rust/` workspace with the actual top-level Cargo workspace.
3. Remove or replace references to missing paths such as:
    * `docs/src/architecture`
    * `specs/generators/pages`
    * `specs/definitions`
    * `.config/dictionaries/project.dic`
4. Add guidance for this repo's actual areas:
    * Rust crates under `crates/`
    * CDDL fixtures under `cddl/`
    * copied RFC/reference text under `rfc/`
    * Zed extension and tree-sitter grammar under `extension/zed/`
    * release license hygiene
5. Keep the useful general guidance:
    * small targeted changes
    * no broad refactors without request
    * avoid generated/cache files
    * respect user changes in dirty work trees

Suggested root `AGENTS.md` sections:

1. General principles.
2. Rust workspace.
3. CDDL fixtures and RFC reference material.
4. Documentation and README style.
5. Licensing and release hygiene.
6. Zed extension/tree-sitter grammar.
7. Commands and validation.
8. Things to avoid.

## Phase 5: Review Nested Agent Guidance

1. Read `extension/zed/grammars/cddl/AGENTS.md`.
2. Confirm whether it should remain specific to the tree-sitter grammar.
3. Ensure it does not contradict the root guidance.
4. If the grammar is vendored or synced from another source, state that explicitly.
5. Avoid editing generated grammar outputs unless the grammar build process requires it.

## Phase 6: Review Supporting READMEs

Review existing non-crate READMEs with a light touch:

1. `cddl/README.md`: describe top-level CDDL fixture/data organization.
2. `cddl/rfc-std/README.md`: preserve upstream attribution and license clarity.
3. `cddl/vectors/README.md`: describe positive/negative/RFC fixture purpose.
4. `cddl/vectors/project/**/README.md`: only fix stale wording or contradictions.
5. `tools/stage2/README.md`: ensure it still reflects actual release/tooling workflow.
6. `extension/zed/grammars/cddl/README.md`: align with package metadata and parent README references.

## Phase 7: Validation

Run documentation and packaging checks after edits:

```sh
cargo metadata --format-version 1
cargo package --list -p cbork
cargo package --list -p cbork-abnf-parser
cargo package --list -p cbork-catalog
cargo package --list -p cbork-cddl-compiler
cargo package --list -p cbork-cddl-parser
cargo package --list -p cbork-edn
cargo package --list -p cbork-utils
```

If available in this repo, also run the markdown/spelling checks exposed through `just` or `moon`.
Confirm exact task names before treating them as release gates.

## Definition of Done

1. Root README gives a clear release-ready overview of the project.
2. Every workspace crate has a short, informative README.
3. Every publishable crate manifest has a `description` and `readme`.
4. License summaries in README files match Cargo metadata and license files.
5. Root `AGENTS.md` accurately describes this repo and removes stale copied guidance.
6. Nested `AGENTS.md` guidance is consistent with the root file.
7. Documentation validation and package-file inspection pass or have documented follow-up items.
