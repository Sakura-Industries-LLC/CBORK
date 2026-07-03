# cbork-cddl-parser

Parser and validator for the CDDL grammar defined in RFC 8610 and its updates.

## Purpose

`cbork-cddl-parser` is the frontend for the workspace.
It turns a `.cddl` source string into a Pest-derived AST that `cbork-cddl-compiler` then resolves.
Its pest grammars cover:

* CDDL as defined in [RFC 8610][rfc8610] (updated by [RFC 9682][rfc9682])
* the additional control operators in [RFC 9165][rfc9165]
* the [CBOR Modules][cddl-modules] `import` / `include` directive syntax

The CDDL standard postlude is loaded as a separate `postlude.cddl` and injected at the start of every parse.

This crate also recognizes the `;@ CBORK: Library` / `Export` / `Extern` directive syntax
(parsed by `cbork-cddl-compiler`, but the parser recognizes the syntax in source for accurate span reporting).

## Usage

The two main public entry points are `validate_cddl` (quick parse/validate) and `try_extract_syntax_error`
(used by the CLI to report a parser error with a labeled span):

```rust
use cbork_cddl_parser::validate_cddl;

validate_cddl("person = { name: tstr }")?;
```

The crate's AST types are re-exported from `cbork-cddl-compiler` for users who need them.

## License

Licensed under `MIT OR Apache-2.0`.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) in this directory for the full texts.

This crate is part of the [cbork workspace](https://github.com/SakuraIndustries/cbork).

[rfc8610]: https://www.rfc-editor.org/rfc/rfc8610
[rfc9165]: https://www.rfc-editor.org/rfc/rfc9165
[rfc9682]: https://www.rfc-editor.org/rfc/rfc9682
[cddl-modules]: https://datatracker.ietf.org/doc/draft-ietf-cbor-cddl-modules/
