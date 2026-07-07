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

# Validate against a specific top-level rule declared in the schema.
cbork validate --type payload path/to/schema.cddl path/to/data.cbor
```

`cbork --help` lists every subcommand and option.

### Selecting a validation root with `--type`

`cbork validate` defaults to the first top-level rule in the schema file as the validation root.
The `--type=<TYPE>` flag overrides that choice with a different top-level rule from the same file:

```shell
cbork validate --type=payload schema.cddl vector.cbor
cbork validate --type=signed-message schema.cddl signed-message.cbor
```

The selected type must be a concrete, non-generic rule declared directly in the schema file passed to `validate`.
Rules that arrive only through `;# include`, `;# import`, the standard postlude,
or generic templates (`wrapper<t>`) cannot be selected.
If a matching rule only exists via include / import / postlude, `cbork validate` exits non-zero and reports the conflicting origin.
Including `cbork --type` does not change how the schema is compiled, how includes and imports are resolved,
how warnings are emitted, or how the decoded CBOR is printed; it only changes the root rule the validator starts from.
Detailed dumps (`--detailed`) annotate the selected root type.

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

### Recursive `lint` discovery rules

Recursive directory scans honor ignore-file filtering by default:

* `.gitignore` files (including nested ones) are honored.
* `.ignore` files, Git global excludes, and Git exclude files are honored.
* Hidden entries (dotfiles and dot-directories) are skipped.
* `.git` directories are always skipped, even when ignore-file filtering is disabled.

Use these flags to override the defaults:

```shell
# Include hidden entries that would otherwise be skipped.
cbork lint --recursive --hidden path/

# Disable ignore-file filtering entirely (hidden entries and .git/ are still honored).
cbork lint --recursive --no-ignore path/
```

An explicit file path passed to `cbork lint` is always linted,
even if it would be skipped by an ignore rule during a recursive scan.

## License

`cbork` is licensed under `AGPL-3.0-only`.
See [`crates/cbork/LICENSE`](LICENSE) in this directory and the repository-root [`LICENSE-AGPL-3.0`](../../LICENSE-AGPL-3.0).
