# Positive Documentation Lint Fixtures

This directory contains CDDL fixtures that exercise the optional documentation lint pass (`cbork lint --doc`).

The fixtures here are valid CDDL and should parse and compile cleanly.
They are designed to fail the doc-lint pass only
when their content violates the documentation rules described in `crates/cbork/plan.md` § *Optional documentation linting*.

The fixtures live in a `doc_lint/` subdirectory so they do not collide with the top-level `positive/` parser tests,
which require every file to parse without errors.
