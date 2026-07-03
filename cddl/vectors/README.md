# CDDL Test Vectors

This directory contains CDDL files used as parser and linter test fixtures.

## Layout

* `project/positive/` - project-specific passing cases (see `project/README.md` for sub-layout)
* `project/negative/` - project-specific failing cases
* `rfc/` - RFC-derived CDDL vectors used as baseline standards coverage

ABNF fixtures derived from RFC text live in `crates/cbork-abnf-parser/tests/abnf/` with their own IETF Trust attribution headers.
