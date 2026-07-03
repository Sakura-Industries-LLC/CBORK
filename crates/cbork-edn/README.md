# cbork-edn

CBOR Extended Diagnostic Notation (EDN) parser and encoder for the CBORK workspace.

## Purpose

`cbork-edn` is a small, focused library that decodes raw CBOR bytes
(single items or concatenated sequences) into an owned, in-memory EDN-like tree, and re-encodes that tree back into canonical CBOR.
The CBOR codec itself is delegated to `minicbor`; `cbork-edn` provides the EDN data model
(`Document` / `Value` / `MapEntry` / `Float`) and a deterministic encoder for the value tree.

It is consumed by the `cbork` CLI's `decode` subcommand and is suitable for use in any project that needs a lossless,
human-readable view of binary CBOR without pulling in the rest of the CDDL compiler.

## Usage

The two main public entry points are `parse` (CBOR bytes → `Document`) and the `Encode` trait on `Value` (Document → CBOR bytes):

```rust
use cbork_edn::{parse, Value};

let doc = parse(&[0xa2, 0x61, b'a', 0x01, 0x61, b'b', 0x02])?;
assert_eq!(doc.to_string(), "{ \"a\": 1, \"b\": 2 }");
```

## License

Licensed under `MPL-2.0`.
See [LICENSE](LICENSE) in this directory and the repository-root `LICENSE-MPL-2.0` for the full text.

This crate is part of the [cbork workspace](https://github.com/SakuraIndustries/cbork).
