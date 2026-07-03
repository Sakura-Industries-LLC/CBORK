# cbork-abnf-parser

Parser for ABNF (Augmented Backus-Naur Form) grammars used across the CBORK workspace.

## Purpose

`cbork-abnf-parser` is the ABNF frontend for the workspace.
It implements a strict parser for [RFC 5234][rfc5234] ABNF used by:

* CDDL `.abnf` and `.abnfb` annotations (handled by `cbork-cddl-compiler`)
* the corpus of `tests/abnf/*.abnf` test vectors under this crate

The grammar lives in `src/grammar/rfc_5234.pest`; a test-only extension set lives in `src/grammar/abnf_test.pest.frag`.

## Usage

The public entry point is `parse_abnf`, which returns either a `Document` (or an `Error`):

```rust
use cbork_abnf_parser::parse_abnf;

let doc = parse_abnf("rule = ALPHA *(ALPHA / DIGIT / \"-\")\n")?;
```

This crate is primarily consumed by `cbork-cddl-compiler`;
direct users typically only need it for testing CDDL control-operator rules that involve `.abnf` annotations.

## License

Licensed under `MIT OR Apache-2.0`.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) in this directory for the full texts.

This crate is part of the [cbork workspace](https://github.com/SakuraIndustries/cbork).

[rfc5234]: https://www.rfc-editor.org/rfc/rfc5234
