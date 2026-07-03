# cbork-cddl-compiler

CDDL compiler and semantic resolver with control-operator support for CBOR schemas.

## Purpose

`cbork-cddl-compiler` is the heart of the workspace.
It takes the AST produced by `cbork-cddl-parser`, resolves named rules, generics, sockets, plug choices,
group/array/group-to-choice augmentation, postlude primitives, and `import` / `include` directives,
then runs a semantic fixed-point pass that:

* evaluates control operators (the full RFC 8610 / RFC 9165 set plus the
  unofficial CBOR-ecosystem annotations `.x-enc`, `.x-hash`, `.x-compressed`,
  `.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate`)
* validates `.within` and `.and` subtyping
* recognizes the `;@ CBORK: Library` / `;@ CBORK: Export` / `;@ CBORK: Extern`
  first-party compiler directives
* emits a `CompiledCDDL` ready for the `cbork` CLI's lint, render, and validate
  commands

It also embeds a markdown-lint pass (`doc_lint`) that surfaces `;@`-directive hygiene
and `rumdl` diagnostics back through the same `Diagnostic` channel.

## Usage

The public entry point is `CompiledCDDL::parse`:

```rust
use cbork_cddl_compiler::CompiledCDDL;

let cddl = CompiledCDDL::parse("person = { name: tstr, age: uint }")?;
let diags = cddl.diagnostics();
```

This crate is primarily consumed by the `cbork` CLI;
library users typically only need it to embed CDDL compilation in their own tooling.

## License

Licensed under `MPL-2.0`.
See [LICENSE](LICENSE) in this directory and the repository-root `LICENSE-MPL-2.0` for the full text.

This crate is part of the [cbork workspace](https://github.com/SakuraIndustries/cbork).
