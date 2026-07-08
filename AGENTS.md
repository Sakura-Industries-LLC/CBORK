# AGENTS – Guidelines for Automated Assistants

This file provides repo-wide guidance for automated coding assistants working in `cbork`.
It applies to the entire repository unless a more specific `AGENTS.md` exists in a subdirectory.

## General Principles

* Make **small, targeted changes** that directly address the user's request.
* Prefer **clarity and correctness** over cleverness; do not refactor broadly
  unless explicitly asked.
* Keep existing project structure, naming, and file layout intact unless there
  is a strong reason to change it.
* When in doubt about intent (spec vs implementation vs docs), **ask the user**
  rather than guessing.

## Crate Workspace

* The Rust workspace is the top-level `Cargo.toml`; crates live under `crates/`.
* Follow the existing module layout and crate boundaries; do not split or merge
  crates without explicit direction.
* Match the existing style and patterns.
  The workspace enforces a strict clippy/lints setup via `[workspace.lints]`; do not relax it locally.
* For validation, run `just fix-ci` (containerized, recommended).

## CDDL Fixtures and RFC Reference Material

* `cddl/rfc-std/` is a snapshot of [`cabo/cddlc` data][cddlc-data] and is covered by its local MIT `LICENSE`.
  Update via upstream rather than hand-editing in place.
* `cddl/vectors/` holds parser and linter test fixtures.
  The per-directory `README.md` files describe the layout.
  <!-- rumdl-disable MD013 -->
  `cddl/vectors/rfc/` holds RFC-derived CDDL vectors; keep source/provenance comments in the files or adjacent README text.
  ABNF fixtures derived from RFC text live in `crates/cbork-abnf-parser/tests/abnf/valid_abnf_*.abnf` with IETF Trust attribution
  in their own headers.
  <!-- rumdl-enable MD013 -->
* `rfc/` and `rfc/related/` contain verbatim copies of RFC and Internet-Draft text.
  The local `rfc/README.md` documents the [IETF Trust License][ietf-trust] that covers this material.

[cddlc-data]: https://github.com/cabo/cddlc/tree/master/data
[ietf-trust]: https://trustee.ietf.org/license/

## Documentation and README Style

* Follow existing Markdown style:
    * Respect the "one sentence per line" convention where it is used.
    * Keep headings, link styles, and admonitions consistent with nearby text.
* The repository is rumdl-linted; `just fix` auto-fixes what it can.
* When adding terminology that is likely to trigger the spell checker, add it
  to `.config/dictionaries/project.dic` in **sorted order**.
* If the user asks for validation, suggest or run:
    * `just fix-ci`

## Licensing and Release Hygiene

* Every **new** source file you create in a workspace crate must carry a `Copyright (c) 2026 Sakura Industries LLC.` line plus the
  SPDX-License-Identifier matching that crate's `Cargo.toml` `license` field (`AGPL-3.0-only`, `MPL-2.0`, or `MIT OR Apache-2.0`).
  The header goes at the very top of the file, above any `//!` module doc, `use` statement, or other content.

  ```rust
  // Copyright (c) 2026 Sakura Industries LLC.
  //
  // SPDX-License-Identifier: MPL-2.0      // or AGPL-3.0-only / MIT OR Apache-2.0
  ```

* **Do not remove any existing copyright notice** from a file.
  If a file already has a header, leave it as-is.

## Zed Extension and Tree-sitter Grammar

* The Zed extension lives under `extension/zed/` (a normal part of this project).
* The tree-sitter grammar under `extension/zed/grammars/cddl/` is *not* part of
  this repository's source — the `grammars/` directory is gitignored and the
  grammar is pulled at extension install time from
  `https://codeberg.org/SakuraIndustries/tree-sitter-cddl.git` (see
  `extension/zed/extension.toml` `[grammars.cddl]`).
* Files committed under `extension/zed/` (the manifest, the LICENSE, and the
  language client config in `languages/cddl/`) are licensed under
  `GPL-3.0-or-later`; the pulled grammar travels with its own `MIT OR Apache-2.0`
  license from the upstream tree-sitter-cddl project.

## Commands and Validation

The minimum local tooling is:

* [`just`](https://github.com/casey/just) — the task runner
* [Podman](https://podman.io/) — the container runtime
* the [`sakura-dev-tools`](https://codeberg.org/SakuraIndustries/sakura-dev-tools)
  build image, which provides `moon`, `rustup`/`cargo`, `cargo deny`,
  `cargo-nextest`, `rumdl`, and `cspell` (the `just` recipes build it on demand)

Primary check (recommended): `just fix-ci` — containerized, runs via Podman, includes license checks via `cargo deny`.

For markdown and spell checks, use `just ci`.
NEVER, run the underlying `moon run root:markdown-check root:spell-check`.

## Things to Avoid

* Do not:
    * Change licensing files, `CODE_OF_CONDUCT.md`, or security policies.
    * Introduce breaking API changes (in Rust or Python) without calling that
      out explicitly to the user.
    * Mass‑reformat the entire repo; limit formatting to files you touch.
* Avoid speculative “cleanup” in areas unrelated to the user’s request, even
  if you notice possible improvements; mention them in your summary instead.
