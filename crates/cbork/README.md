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

## License

`cbork` is licensed under `AGPL-3.0-only`.
See [`crates/cbork/LICENSE`](LICENSE) in this directory and the repository-root [`LICENSE-AGPL-3.0`](../../LICENSE-AGPL-3.0).
