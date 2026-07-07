---
name: cddl-using-cbork
description: |
  Use this skill when adding cbork to a project, wiring it into
  pre-commit or CI, or deciding which cbork subcommand (`lint`,
  `render`, `validate`, or `lint --doc`) to run for a given task.
  Covers the meaning of each subcommand, the `--no-fail` advisory
  switch, recursive directory mode, and the config-file lookup.
---

# Using cbork Effectively

This sub-skill covers the operational side of cbork: which subcommand to run when, how to wire it into CI or pre-commit,
and how the advisory `--no-fail` mode fits in.

## Choose a subcommand

| Subcommand         | Use it when…                                                                                                       |
| ------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `cbork lint FILE`  | You want parse errors, semantic errors, and lint warnings on one or more `.cddl` files.                            |
| `cbork lint --doc` | You also want the documentation lint pass; required when the file uses `;!` doc comments and is meant to ship.    |
| `cbork render FILE`| You want the effective CDDL that the compiler actually reasons about (named rules, generics, sockets, plug choices, nested control operators expanded). |
| `cbork validate --schema FILE DATA` | You have raw CBOR bytes and want to check them against a compiled schema.                          |

When in doubt, run `cbork lint FILE` first.
It catches the bulk of mistakes before you reach for `render` or `validate`.

## Lint modes

`cbork lint` has two modes:

* **Default lint** — parse + semantic checks.
  Run this on every schema file in CI.
* **`--doc` lint** — runs the documentation lint pass via `rumdl`.
  Run this on schema files that carry `;!` documentation comments.
  See `doc-comments.md` for the comment conventions the doc lint pass enforces.

You can pass `--doc` alongside `--recursive` to lint a whole directory's documentation in one go.

`--recursive` walks a directory and lints every `.cddl` file under it.
Use this in CI rather than maintaining a hand-written file list.

## Render mode

`cbork render FILE` prints the effective CDDL that the compiler will type-check against.
Read it when:

* A generic expansion looks wrong.
* A socket plug fills a different type than you expected.
* A nested control operator hides the real carrier and you
  need to see the rendered shape.
* `.within` produces an `E030` diagnostic and you want to see
  what the compiler actually compared.

The rendered output is also what drives the `.within` and `.and` subtype diagnostics —
so a clean render and a clean lint are closely related.

## Validate mode

`cbork validate --schema FILE DATA` compiles the schema and checks the CBOR bytes in `DATA` against it.
Use it for round-trip tests in CI:

<!-- rumdl-disable MD040 -->

```shell
cbork validate --schema schemas/person.cddl test/vectors/person.cbor
```

<!-- rumdl-enable MD040 -->

`validate` is the closest thing cbork has to a "did the encoder do its job" check.
Keep at least one validate run per wire format in your test suite.

## CI integration

Two patterns work well:

**Strict CI**: fail the build on any diagnostic.

<!-- rumdl-disable MD040 -->

```shell
cbork lint --recursive schemas/
cbork lint --doc --recursive schemas/
cbork render --recursive schemas/ > /dev/null   # exits non-zero on render errors
```

<!-- rumdl-enable MD040 -->

**Advisory CI**: surface diagnostics but keep the build green with `--no-fail`.
Use this while you adopt new lint rules across a large schema.

<!-- rumdl-disable MD040 -->

```shell
cbork --no-fail lint --recursive schemas/
cbork --no-fail lint --doc --recursive schemas/
```

<!-- rumdl-enable MD040 -->

`--no-fail` does not suppress diagnostics, change command output, skip work, or downgrade errors —
it only overrides the process exit code.
Use it when you want diagnostics visible without breaking the job:

* Advisory or reporting runs in CI where the diagnostics
  should be visible but should not break the job.
* Staged adoption of new lint rules.
* Local bulk-audit workflows where you want a complete report
  and a zero shell status.

Parse/usage errors that occur before a subcommand runs
(for example, an unknown flag rejected by the argument parser) still produce a non-zero exit.
`--no-fail` only overrides the command-result exit code.

## Pre-commit integration

A minimal pre-commit hook:

<!-- rumdl-disable MD040 -->

```yaml
# .pre-commit-config.yaml
repos:
  - repo: local
    hooks:
      - id: cbork-lint
        name: cbork lint
        entry: cbork lint
        language: system
        files: '\.cddl$'
        pass_filenames: true
```

<!-- rumdl-enable MD040 -->

Add a second hook for the doc lint pass:

<!-- rumdl-disable MD040 -->

```yaml
      - id: cbork-lint-doc
        name: cbork lint --doc
        entry: cbork lint --doc
        language: system
        files: '\.cddl$'
        pass_filenames: true
```

<!-- rumdl-enable MD040 -->

Keep these hooks strict locally.
The CI side can run with `--no-fail` while the schema is being cleaned up;
the local side should stay strict so contributors see the failures before pushing.
