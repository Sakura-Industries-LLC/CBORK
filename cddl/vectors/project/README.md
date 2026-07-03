# Project Test Vectors

This directory contains project-specific CDDL fixtures that extend RFC baseline coverage.

## Layout

* `positive/` - cases that should parse and lint successfully
* `negative/` - cases that should fail parsing or linting as appropriate
* `positive/support/` and `negative/support/` - helper fixtures used by the
  higher-level import/include regression vectors
