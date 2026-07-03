# Negative Documentation Lint Fixtures

This directory contains CDDL fixtures that exercise the optional documentation lint pass (`cbork lint --doc`).

The fixtures here are valid CDDL and parse and compile cleanly.
They are designed to fail the doc-lint pass
when their content violates the documentation rules described in `crates/cbork/plan.md` § *Optional documentation linting*.

These fixtures are intentionally placed in a `doc_lint/` subdirectory
so they do not collide with the top-level `negative/` compiler-validation tests,
which require every file in `negative/` to fail compilation.

Each fixture documents the specific doc-lint rule it is expected to trigger.
