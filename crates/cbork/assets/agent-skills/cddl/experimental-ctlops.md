---
name: cddl-experimental-ctlops
description: |
  Use this skill when using cbork's unofficial `.x-` family of
  control operators: `.x-enc`, `.x-hash`, `.x-compressed`, and
  the per-algorithm wrappers `.x-brotli`, `.x-zstd`, `.x-gzip`,
  `.x-deflate`. Covers the transform-family matrix, the
  annotated `.abnf` / `.abnfb` forms, and how each operator
  interacts with `.within`.
---

# Experimental `.x-` Control Operators

The standard CDDL control operators cover the cases CBOR-native data needs
(`.cbor`, `.size`, `.bits`, numeric bounds, …)
but not the cases the CBOR ecosystem has settled on informally: encrypted blobs, hashes, and the many flavours of compression.

cbork lets CDDL authors express those without abandoning `.within` semantics, by giving each annotation its own transform family.
Each operator is recognised as a separate transform family so that `.within` checks compare both the carrier
and the transform identity.

## Encryption: `.x-enc`

`.x-enc` narrows `bstr` to "the encryption of `T`".

<!-- rumdl-disable MD040 -->

```cddl
encrypted = bstr .x-enc payload
```

<!-- rumdl-enable MD040 -->

`bstr .x-enc T_narrow` is within `bstr .x-enc T_wide` whenever `T_narrow .within T_wide`.
`bstr .x-enc T` is always within the bare `bstr` carrier.

## Hashing: `.x-hash`

`.x-hash` narrows `bstr` to "the hash of `T`".

<!-- rumdl-disable MD040 -->

```cddl
hashed = bstr .x-hash payload
```

<!-- rumdl-enable MD040 -->

`.x-hash` is its own transform family.
A `bstr .x-enc T` is not within a `bstr .x-hash T`, and the reverse is also false.
The two operators do not subtype each other; the linter emits `E030` if you write that `.within`.

## Compression: `.x-compressed` and the per-algorithm wrappers

Compression annotations are organised into a generic wrapper (`.x-compressed`) and per-algorithm wrappers
(`.x-brotli`, `.x-zstd`, `.x-gzip`, `.x-deflate`).
Each algorithm is within the generic wrapper when its controller subtypes the RHS controller.

<!-- rumdl-disable MD040 -->

```cddl
brotli-bstr = bstr .x-brotli payload
any-zstd    = bstr .x-zstd   payload-wide

ok = brotli-bstr .within (bstr .x-compressed payload-wide)
```

<!-- rumdl-enable MD040 -->

Two different algorithms are NOT mutually within each other
(`bstr .x-brotli T` is not within `bstr .x-zstd T`), and the generic wrapper is NOT within any specific algorithm.

## Annotated forms: `.x-compressed.abnf` and friends

The `.abnf` / `.abnfb` annotated forms (`.x-compressed.abnf`, `.x-brotli.abnfb`, …) collapse to the same transform family
for `.within` subtype purposes while still preserving enough detail for literal/ABNF validation.
This matches the unofficial CBOR convention.

Treat the annotated forms as syntactic sugar over the un-annotated form when you reason about `.within`.
Read the form's literal/ABNF validity separately, with the renderer.

## Compatibility matrix

The full compatibility matrix that drives `.within` subtype checks is summarised below.
Rows are the LHS operator, columns are the RHS operator:

|                   | `bstr` (carrier) | `.x-enc`           | `.x-hash`           | `.x-compressed`      | `.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate` |
| ----------------- | ---------------- | ------------------ | ------------------- | -------------------- | ------------------------------------------ |
| `.x-enc`          | yes              | yes (controllers)  | no                  | no                   | no                                         |
| `.x-hash`         | yes              | no                 | yes (controllers)   | no                   | no                                         |
| `.x-compressed`   | yes              | no                 | no                  | yes (controllers)    | no                                         |
| `.x-brotli` etc.  | yes              | no                 | no                  | yes (controllers)    | yes same algorithm, no different algorithms |

A `yes` means the LHS subtypes the RHS.
A `no` means `.within` emits an `E030` control-mismatch diagnostic that names the incompatible operators.

For the underlying carrier-and-controller logic that drives this matrix, see `within.md`.

## When to reach for an `.x-` operator

Use `.x-enc` when the field is a ciphertext whose plaintext shape is part of the wire contract.
Use `.x-hash` when the field is a digest
and you want the schema to express "this digest is of `T`" rather than just "this is a `bstr` of length `N`".
Use `.x-compressed` when the schema needs to be algorithm-agnostic but still express "compressed payload of `T`".
Use the per-algorithm wrapper when the wire format commits to a specific algorithm.

Avoid the `.x-` operators when the wire format does not actually carry the encryption, hash,
or compression metadata in a way the schema can rely on.
A bare `bstr` plus an implementation note in a `;!` doc comment is the safer choice in that case.
