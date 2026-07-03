# License Hygiene Plan

This plan breaks license cleanup into small release-safe steps.
The goal is to make the repo's licensing clear at the repository level, crate level, bundled-data level,
and dependency-audit level before publication.

## Current Snapshot

Workspace crates:

| Path | Package | Declared license | Local license files | Notes |
| --- | --- | --- | --- | --- |
| `crates/cbork` | `cbork` | `AGPL-3.0` | none in crate | CLI crate, root has `LICENSE-AGPL-3.0`. |
| `crates/cbork-abnf-parser` | `cbork-abnf-parser` | `MIT OR Apache-2.0` | `LICENSE-MIT`, `LICENSE-APACHE` | Dual-licensed parser crate. |
| `crates/cbork-catalog` | `cbork-catalog` | workspace `MPL-2.0` | `LICENSE` | Needs verification that file text matches MPL-2.0 or an intentional local notice. |
| `crates/cbork-cddl-compiler` | `cbork-cddl-compiler` | workspace `MPL-2.0` | `LICENSE` | Needs verification that file text matches MPL-2.0 or an intentional local notice. |
| `crates/cbork-cddl-parser` | `cbork-cddl-parser` | `MIT OR Apache-2.0` | `LICENSE-MIT`, `LICENSE-APACHE` | Dual-licensed parser crate. |
| `crates/cbork-edn` | `cbork-edn` | workspace `MPL-2.0` | `LICENSE` | New/changed license file appears present. |
| `crates/cbork-utils` | `cbork-utils` | `MIT OR Apache-2.0` | `LICENSE-MIT`, `LICENSE-APACHE` | New/changed dual-license files appear present. |

Other shipped components:

| Path | Declared or implied license | Notes |
| --- | --- | --- |
| repo root | mixed licenses | `LICENSE` summarizes the mixed-license structure; root also has `LICENSE-MPL-2.0` and `LICENSE-AGPL-3.0`. |
| `cddl/rfc-std` | MIT | Directory has a local `LICENSE` and README attribution to upstream `cabo/cddlc` data. |
| `rfc/` and `rfc/related/` | IETF Trust / RFC text terms | Bundled RFC and draft text needs explicit attribution and redistribution rationale. |
| `extension/zed` | local `LICENSE` | Verify it matches the Zed extension manifest and the tree-sitter grammar subpackage. |
| `extension/zed/grammars/cddl` | `MIT OR Apache-2.0` | Outside the workspace Cargo members; has its own `README.md` and nested package metadata. |

Existing tooling:

| File or command | Purpose | Gap |
| --- | --- | --- |
| `deny.toml` | Cargo dependency license allowlist and advisory checks | Allowlist includes project licenses, but release policy still needs to define expected direct dependency outcomes. |
| moon license tasks | `license-check` and `license-advisories` state files exist | Confirm current runnable task names and document the release command. |
| Cargo manifests | Package-level license declarations | Several crates lack `description` and `readme`, which affects crates.io release hygiene. |

## Phase 1: Intended License Matrix

1. Confirm the intended license for each first-party crate.
2. `cbork` should be `AGPL-3.0-only`.
3. MPL crates have full license text in `LICENSE-MPL-2.0`, local crates `LICENSE`,
   is the actual copyright notice which refers to the license.
4. Every publishable crate should include local license files in the crate directory for crates.io packaging.
5. Generated artifacts, fixtures, RFC copies, and editor-extension files are part of source distributions,
   and licensed as specified.

Expected output:

1. A short authoritative license table in the root `README.md`.
2. Matching Cargo `license` expressions.
3. Matching local license-file layout per crate.

## Phase 2: Normalize First-Party License Files

1. Verify `LICENSE`, `LICENSE-MPL-2.0`, and `LICENSE-AGPL-3.0` contain the intended exact texts.
2. Verify every crate-local `LICENSE`, `LICENSE-MIT`, and `LICENSE-APACHE` file matches its manifest declaration.
3. Normalize file naming policy:
    * MPL-only crates use either `LICENSE` or `LICENSE-MPL-2.0` consistently.
    * Dual MIT/Apache crates use `LICENSE-MIT` and `LICENSE-APACHE` consistently.
    * AGPL crate has an obvious pointer to `LICENSE-AGPL-3.0`.
4. Add short crate-local license notes only if needed to remove ambiguity for mixed-license packaging.
5. Avoid changing license terms as part of mechanical cleanup; any license-term change should be its own reviewed commit.

## Phase 3: Align Cargo Package Metadata

For every workspace crate:

1. Ensure `license` or `license.workspace` is correct.
2. Add a concise `description`.
3. Add `readme = "README.md"` after crate README files exist.
4. Confirm `homepage`, `repository`, `authors`, `edition`, and `rust-version` inheritance is intentional.
5. Decide whether any crates should have `publish = false` before release.
6. Run `cargo package --list -p <crate>` for crates intended for publication and inspect included license/readme files.

Crate-specific checks:

| Package | Metadata cleanup |
| --- | --- |
| `cbork` | Clarify AGPL expression and include README/license in package. |
| `cbork-abnf-parser` | Add description/readme, confirm dual-license files package cleanly. |
| `cbork-catalog` | Add description/readme, verify generated catalog data licensing. |
| `cbork-cddl-compiler` | Add description/readme, verify dependency and bundled fixture implications. |
| `cbork-cddl-parser` | Add description/readme, confirm dual-license files package cleanly. |
| `cbork-edn` | Add description/readme, verify MPL license file/package inclusion. |
| `cbork-utils` | Add description/readme, confirm dual-license files package cleanly. |

## Phase 4: Audit Bundled Third-Party Material

1. Inventory copied RFC and draft text under `rfc/` and `rfc/related/`.
   These should have a clear statement in the README.md that the files own license applies, not anything in the repo,
   as they are literal RFC copies.
2. Add or update a README/license note for the RFC text that references the applicable IETF Trust terms.
3. Verify CDDL fixture files copied or derived from RFCs have a clear source and license note.
4. Verify `cddl/rfc-std` remains covered by its local MIT license and attribution.
5. Check whether any generated files contain embedded upstream text that needs attribution.
6. Exclude vendored/transient build directories from release scans:
    * `target/`
    * `.moon/`
    * `.uv-cache/`
    * `extension/zed/grammars/cddl/node_modules/`

## Phase 5: Dependency License Audit

1. Run the existing license/advisory checks.
2. Confirm the exact command used by CI or release automation.
3. Review each allowed license in `deny.toml` and keep only licenses actually needed or intentionally allowed.
4. Investigate any dependency using:
    * copyleft licenses
    * unknown license expressions
    * unmaintained advisories
    * yanked or duplicate versions if relevant to release policy
5. Document any accepted exceptions in `deny.toml` with comments or a companion release note.

Suggested commands:

```sh
cargo deny check licenses advisories
moon run :license-check
moon run :license-advisories
```

Use the project-specific command that matches the current moon task graph once confirmed.

## Phase 6: Source Headers and Copyright Notices

Every source file needs an accurate copyright notice at the top.
Do this crate-by-crate so each commit is reviewable and license-specific.

General policy:

1. Preserve existing and historical copyright ownership.
2. Add notices per file, not only per crate.
3. Use the correct license expression for the file's crate.
4. Add `2026 (c) Sakura Industries LLC` to files owned by the current repo unless a file is purely copied third-party material.
5. For files that existed in commit `37c5492`, add the required `2023 (c) IOG` notice if the file still exists.
6. For files originally authored by Steven Johnson in 2023, preserve that ownership notice.
7. Keep generated files free of hand-written headers unless the generator supports stable header emission.
8. Do not mix header cleanup with substantive code changes.
9. After the crate passes review, record the policy in `AGENTS.md` or a release checklist so new files follow it.

Known historical ownership notes:

1. The original CDDL parser files below were copyright `2023 (c) Steven Johnson` and need an appropriate preserved notice:
    * `crates/cbork-cddl-parser/src/grammar/cddl.pest`
    * `crates/cbork-cddl-parser/src/grammar/cddl_test.pest`
    * `crates/cbork-cddl-parser/src/grammar/postlude.cddl`
    * `crates/cbork-cddl-parser/src/lib.rs`
2. All `src/*.rs` files present in commit `37c5492` require an individual `2023 (c) IOG` copyright notice if the file still exists.
3. The Apache-2.0/MIT crates may contain files with multiple copyright owners; preserve all applicable owners.

### Phase 6.1: `cbork`

License: `AGPL-3.0`.

1. Inventory all source files under `crates/cbork/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
4. Use an AGPL-compatible header matching the crate's final Cargo license expression.
5. Check CLI examples, embedded text, and diagnostics for copied upstream text that may require separate attribution.
6. Run formatting after changes and avoid touching behavior.

### Phase 6.2: `cbork-abnf-parser`

License: `MIT OR Apache-2.0`.

1. Inventory all source and grammar files under `crates/cbork-abnf-parser/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Preserve any existing or historical `2023 (c) Steven Johnson` ownership if identified during file review.
4. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
5. Use a dual-license header matching `MIT OR Apache-2.0`.
6. Check ABNF fixtures and RFC-derived grammar content for attribution requirements before adding first-party-only headers.

### Phase 6.3: `cbork-catalog`

License: workspace `MPL-2.0`.

1. Inventory all source, build, and generated-catalog input files under `crates/cbork-catalog/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
4. Use an MPL-2.0 header matching the crate's final Cargo license expression.
5. Identify generated files separately and prefer adding headers through the generator if any generated output is checked in.
6. Confirm catalog source data does not embed third-party material without attribution.

### Phase 6.4: `cbork-cddl-compiler`

License: workspace `MPL-2.0`.

1. Inventory all source files under `crates/cbork-cddl-compiler/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
4. Use an MPL-2.0 header matching the crate's final Cargo license expression.
5. Check files containing RFC-derived semantics, diagnostic text, control-operator descriptions,
   or copied examples for attribution needs.
6. Keep semantic/compiler behavior unchanged while adding headers.

### Phase 6.5: `cbork-cddl-parser`

License: `MIT OR Apache-2.0`.

1. Inventory all source, grammar, and postlude files under `crates/cbork-cddl-parser/`.
2. Add preserved `2023 (c) Steven Johnson` notices to:
    * `crates/cbork-cddl-parser/src/grammar/cddl.pest`
    * `crates/cbork-cddl-parser/src/grammar/cddl_test.pest`
    * `crates/cbork-cddl-parser/src/grammar/postlude.cddl`
    * `crates/cbork-cddl-parser/src/lib.rs`
3. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
4. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
5. Use a dual-license header matching `MIT OR Apache-2.0`.
6. Check grammar and postlude files for RFC-derived text before applying first-party-only headers.

### Phase 6.6: `cbork-edn`

License: workspace `MPL-2.0`.

1. Inventory all source files under `crates/cbork-edn/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
4. Use an MPL-2.0 header matching the crate's final Cargo license expression.
5. Check EDN/CDN examples or embedded test vectors for copied upstream material before adding first-party-only headers.
6. Keep parser/encoder behavior unchanged while adding headers.

### Phase 6.7: `cbork-utils`

License: `MIT OR Apache-2.0`.

1. Inventory all source files under `crates/cbork-utils/`.
2. For every file still present from commit `37c5492`, add the required `2023 (c) IOG` notice.
3. Add the appropriate `2026 (c) Sakura Industries LLC` notice to current first-party files.
4. Use a dual-license header matching `MIT OR Apache-2.0`.
5. Verify utility tests and examples do not contain copied third-party snippets needing separate notices.
6. Run formatting after changes and avoid touching behavior.

This is critical because many files have file based copyright
(MPL, MIT or Apache2 are all file based copyrights) And every editor of the file SHARES copyright in these cases.
But the license cant be changed within them.

## Phase 7: Release Verification

Run final checks after the README and manifest work lands:

1. `cargo metadata --format-version 1`.
2. `cargo package --list -p <crate>` for each publishable crate.
3. `cargo deny check`.
4. Project moon/just CI targets that include license checks.
5. Manual review of root README license section and crate README license sections.
6. Confirm no release archive includes `target/`, caches, or vendored `node_modules/` unexpectedly.

## Definition of Done

1. Root README has an accurate mixed-license summary.
2. Every workspace crate has a correct Cargo license expression.
3. Every publishable crate package includes the expected license files and README.
4. Bundled RFC, CDDL, and editor-extension materials have explicit attribution/licensing notes.
5. Dependency license/advisory checks pass or have documented release-approved exceptions.
6. The repo has a documented policy for future license additions.
