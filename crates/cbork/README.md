# cbork

Command-line tool for CDDL linting, validation, and effective CDDL rendering.

## Purpose

`cbork` is the user-facing entry point of the workspace.
It wraps the parser, compiler, and EDN crates so that `cbork lint`, `cbork render`, `cbork validate`,
and the diagnostic helpers in the crate are all driven by a single binary distributed under `AGPL-3.0-only`.
The detailed operator matrices and compiler-directive rules are documented in the parent [`README.md`](../../README.md);
per-command help and command-level options live here.

## Usage

After `cargo install --path crates/cbork` (or `cargo run -p cbork --` from the workspace root):

```shell
# CDDL linting
cbork lint path/to/schema.cddl

# Show the effective CDDL (resolves named constants, generics, sockets, plug choices,
# nested control operators) into a readable concrete view.
cbork render path/to/schema.cddl

# Validate raw CBOR against a compiled CDDL schema.
cbork validate --schema path/to/schema.cddl path/to/data.cbor
```

`cbork --help` lists every subcommand and option.

### Advisory / CI runs with `--no-fail`

The global `--no-fail` switch forces the process to exit `0` even when a subcommand would normally report a failure.
Diagnostics and command output are unchanged; only the process exit code is forced to zero.
This is useful for advisory lint runs, staged adoption of new rules,
and local bulk audits where you want a complete report and a zero shell status:

```shell
# Always exit 0 even when individual schemas fail lint.
cbork --no-fail lint --recursive schemas/
```

Note: parse/usage errors that occur before a subcommand runs
(for example, an unknown flag rejected by the argument parser) still produce a non-zero exit.
`--no-fail` only overrides the command-result exit code.

## License

`cbork` is licensed under `AGPL-3.0-only`.
See [`crates/cbork/LICENSE`](LICENSE) in this directory and the repository-root [`LICENSE-AGPL-3.0`](../../LICENSE-AGPL-3.0).
