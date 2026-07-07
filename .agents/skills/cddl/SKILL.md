---
name: cddl
description: |
  Use this skill when authoring or revising CDDL (`.cddl`) files in a
  project that uses cbork for linting, rendering,
  validation, diagnostics, and standards lookup.
  This is the router for a focused set of sub-skills; load the
  sub-skill that matches the part of the task you are completing
  and stop there. Do not load every sub-skill for a single change.
---

# CDDL Authoring With cbork

This skill set is for projects that write CDDL and use cbork as a developer tool.
Copy the whole `.agents/skills/cddl/` directory into your own repository,
edit the `name` and `description` fields to match your project, and add or remove sub-skills as your schema grows.

## Routing

Read only the sub-skill that matches the task in front of you.
Do not load more than one sub-skill per change unless the change spans two clear domains
(for example, an exported rule that also needs a `.within` constraint).

| When you are…                                                                  | Load                            |
| ------------------------------------------------------------------------------ | ------------------------------- |
| Writing or revising `;!` documentation comments                                | `doc-comments.md`               |
| Improving the overall style and shape of CDDL rules                            | `clean-cddl.md`                 |
| Wiring `cbork lint`, `cbork render`, or `cbork validate` into your workflow    | `using-cbork.md`                |
| Debugging cbork diagnostics or looking up standards rationale                  | `diagnostics-and-reference.md`  |
| Building validation checks around CBOR test vectors or decoder output          | `validation-vectors.md`         |
| Adding or changing `;# include` or `;# import` directives                      | `includes-and-imports.md`       |
| Adding `;@ CBORK: Library`, `Export`, or `Extern`                             | `library-directives.md`         |
| Writing `.within` subtype constraints                                          | `within.md`                     |
| Using `.x-enc`, `.x-hash`, `.x-compressed`, or the per-algorithm `.x-brotli` / `.x-zstd` / `.x-gzip` / `.x-deflate` operators | `experimental-ctlops.md` |
| Putting `any` on the left of `.cbor`, `.cborseq`, `.dtrm`, `.dtrmseq`, `.prefp`, or `.prefpseq` | `any-as-lhs.md` |

If a task spans more than one row, list the rows you actually read in your summary so the user can audit
which sub-skills informed the change.

## Sub-skill catalogue

Each sub-skill is a self-contained reference; do not require the user to read the whole set.

* `doc-comments.md` — required file shape, definition comments, generic parameter documentation, list indentation.
* `clean-cddl.md` — general CDDL style and how cbork's render and lint audit it.
* `using-cbork.md` — workflow for `cbork lint`, `cbork render`, and `cbork validate` in a repository.
* `diagnostics-and-reference.md` — `cbork why`, `cbork xref`, `cbork rfc`, diagnostic triage, JSON output, and strict/advisory runs.
* `validation-vectors.md` — `cbork validate`, `cbork decode`, stdin/file input, positive and negative vectors, and CI layout.
* `includes-and-imports.md` — unquoted catalog base names, quoted relative paths, quoted absolute paths.
* `library-directives.md` — `;@ CBORK: Library` / `Export` / `Extern` and the direct-use export contract.
* `within.md` — `.within` as a subtyping predicate and the transform-family compatibility matrix.
* `experimental-ctlops.md` — the `.x-` operator family for encryption, hashing, and compression.
* `any-as-lhs.md` — `any .cbor …`, `any .cborseq …`, and the rest of the permissive carriers.

## Adapting this skill set

The descriptions and examples in this directory are written for a generic project that uses cbork.
Before adopting the skill set in your own repository:

1. Edit `name` and `description` in `SKILL.md` and each sub-skill so
   the frontmatter reflects your project, not the upstream defaults.
2. Add or remove rows from the routing table in `SKILL.md` so the
   mapping matches the sub-skills you actually ship.
3. Add a "Sub-skill catalogue" entry for any extra sub-skill you add.
4. Remove any sub-skill your project does not need (and its catalogue
   entry) instead of leaving it around as noise.
5. Re-run `cbork lint --doc` on the schema files your new sub-skills
   reference so the examples stay valid against the linter.
