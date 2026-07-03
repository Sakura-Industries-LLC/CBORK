# cbork-utils

Shared CBOR utility helpers used across the CBORK workspace.

## Purpose

`cbork-utils` is a small support crate that bundles the CBOR encode/decode helpers
(`array`, `map`, `decode_helper`, `decode_context`, `deterministic_helper`, `with_cbor_bytes`) shared by the rest of the workspace.
They wrap `minicbor` and add:

* a typed `Array` / `Map` view that preserves original CBOR bytes
  alongside the decoded value (via `WithCborBytes`)
* a deterministic-encoding validator (used by the `cbork` CLI's
  `--deterministic` / strict-mode passes)
* helper functions for typed decoding of common CBOR shapes

This crate is primarily consumed by `cbork`, `cbork-cddl-compiler`, and `cbork-edn`;
library users typically only need it when they want the same helpers outside the workspace.

## Usage

```rust
use cbork_utils::with_cbor_bytes::WithCborBytes;
use minicbor::Decoder;

let bytes = b"\xa2\x61a\x01\x61b\x02".to_vec();
let (value, original) = WithCborBytes::decode(&mut Decoder::new(&bytes), &())?;
assert_eq!(original, bytes);
```

## License

Licensed under `MIT OR Apache-2.0`.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) in this directory for the full texts.

This crate is part of the [cbork workspace](https://github.com/SakuraIndustries/cbork).
