# Zed extension: CDDL

A [Zed](https://zed.dev/) editor extension that adds CDDL syntax support and editor integrations for the CBORK project.

## Purpose

This extension provides the language client integration that makes CDDL editing in Zed work:

* **Grammar** — pulled at install time from [`github.com/Sakura-Industries-LLC/tree-sitter-cddl`][grammar-repo]
  (the `extension.toml` `[grammars.cddl]` section pins the source repo and ref).
  The grammar source is *not* vendored into this repository;
  the `grammars/` directory is gitignored and populated by the Zed extension loader on first install.
* **Language config** — `languages/cddl/config.toml` declares the
  language name, grammar, and file suffixes.
* **Editor queries** — `languages/cddl/{brackets,highlights,outline}.scm`
  are tree-sitter queries that drive bracket matching, syntax highlighting,
  and the document outline in Zed.

## Usage

Install the extension from inside Zed:

1. Open the command palette and choose `zed: install dev extension`.
2. Point Zed at this directory: `extension/zed/`.
3. Open any `.cddl` file — Zed will pull the grammar from the URL above
   and start highlighting and outlining the document.

## License

This extension is licensed under `GPL-3.0-or-later`.
See [`LICENSE`](LICENSE) in this directory for the full text.

The grammar that the extension pulls at install time is licensed under `MIT OR Apache-2.0` upstream;
that license travels with the grammar and is not affected by the license on this directory.

[grammar-repo]: https://github.com/Sakura-Industries-LLC/tree-sitter-cddl
