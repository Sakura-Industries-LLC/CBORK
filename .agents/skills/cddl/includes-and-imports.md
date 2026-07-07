---
name: cddl-includes-imports
description: |
  Use this skill when adding or changing `;# include` or `;# import`
  directives. Covers unquoted catalog base names (RFCs and other
  catalog entries), quoted relative paths, quoted absolute paths,
  the `from` and `as` clauses, the unused-directive warnings
  (`W004`, `W005`), and the read errors (`E009`, `E011`).
---

# `;# include` and `;# import` in cbork

cbork implements the module-system syntax from the CDDL Modules draft.
The target of an `include` or `import` directive is classified by how it is written,
and the classification decides how cbork resolves it.

## Three forms of target

### Unquoted base names

A bare identifier (for example `rfc9052`, `rfc8610`) is a *default standard CDDL document*
and resolves through the compile-time catalog (`cddl/rfc-std/` in the cbork source tree, or its equivalent in your installation).

<!-- rumdl-disable MD040 -->

```cddl
;# import rfc8610
;# import cose_sign from "rfc9052"
```

<!-- rumdl-enable MD040 -->

Two imports of the same base name produce rules with the same source origin (`catalog:<name>`), so re-imports compose cleanly.
Prefer unquoted base names for RFC CDDL — you get reproducible catalog versions and no path surprises across machines.

### Quoted relative paths

A quoted path that does not start with `/` is resolved **relative to the CDDL file in which the directive appears**,
and the file content is read from the local filesystem.

<!-- rumdl-disable MD040 -->

```cddl
;# include "./common/common-types.cddl"
;# include "../shared/util.cddl"
;# import foo from "./local.cddl"
```

<!-- rumdl-enable MD040 -->

Relative paths are the right form for sharing definitions inside one repository.
They keep the build reproducible within the repo without anchoring the build to a host filesystem layout.

### Quoted absolute paths

A quoted path that starts with `/` is resolved against the logical `root_path` passed to the compiler.
The compiler anchors absolute includes to a known filesystem root so the build is reproducible.

<!-- rumdl-disable MD040 -->

```cddl
;# include "/schemas/common.cddl"
```

<!-- rumdl-enable MD040 -->

Prefer relative paths in committed schemas.
Use absolute paths only when the build system itself sets `root_path` to a stable value
(for example, a monorepo with a known workspace root).

## `from` and `as` clauses

The `from` clause selects specific names from the imported module.
The `as` clause renames them on import.

<!-- rumdl-disable MD040 -->

```cddl
;# import cose_sign from "rfc9052" as sign
;# import { cose_sign, cose_verify } from "rfc9052"
```

<!-- rumdl-enable MD040 -->

Whole-library imports (no `from` clause) are valid; cbork only warns on the names you never use,
not on names exported by the library you imported.

## Diagnostics

| Code  | Trigger                                                                  |
| ----- | ------------------------------------------------------------------------ |
| `E009` | The target file could not be resolved (unknown base name or missing path). |
| `E011` | The target file was resolved but the read failed.                        |
| `W004` | The whole `import` / `include` directive contributes no referenced symbol. For `from` directives this fires when every name in the `from` clause is unused. |
| `W005` | A specific selected name on a `from` clause is never referenced. Fires per name, even when the directive as a whole is partially used. |

The unused checks walk the consumer's own rules in the post-prune tree, so references that the reachability pruner dropped
(or references inside the imported rules themselves) do not falsely satisfy the "used" check.

If the imported module is marked as a CBORK library, see `library-directives.md` —
the direct-use export contract adds an extra check (`W003`) on top of `W004` and `W005`.

## Best practices

* Use unquoted catalog base names for RFC CDDL; they resolve
  through the bundled catalog and stay portable across
  machines.
* Use quoted relative paths for definitions that live in your
  own repository; they keep the build reproducible inside the
  repo without depending on a host filesystem layout.
* Avoid quoted absolute paths unless the build system sets
  `root_path` to a stable value.
* Keep `from` clauses narrow.
  A wide `from` clause combined with many `W005` warnings is a smell
  that the consumer does not actually need the imported module's surface.
* Re-run `cbork render` after adding a new directive so you
  can confirm the imported names land in your rules.
