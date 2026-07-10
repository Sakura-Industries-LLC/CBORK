---
name: cddl-any-as-lhs
description: |
  Use this skill when writing `any` on the left of `.cbor`,
  `.cborseq`, `.dtrm`, `.dtrmseq`, `.prefp`, or `.prefpseq`.
  Covers why cbork accepts `any` as a permissive carrier, the
  list of operators that accept it, and when `any` is the right
  shape versus a `bstr`.
---

# `any` on the LHS of CBOR Control Operators

The serialization-oriented control operators (`.cbor`, `.cborseq`, `.prefp`, `.prefpseq`, `.dtrm`, `.dtrmseq`) accept either `any`
(an unofficial permissive carrier)
or `bstr` (the official carrier) on the left, with a controller that may be a scalar, array, map/group, or wildcard shape.

The `any` permissive carrier rule is intentional.
It supports schemas where the source text wants to say "some representation that serializes
as `T`" without first narrowing the carrier to `bstr`.

## Operators that accept `any`

| Operator         | Permissive carrier | Official carrier |
| ---------------- | ------------------ | ---------------- |
| `.cbor`          | `any`              | `bstr`           |
| `.cborseq`       | `any`              | `bstr`           |
| `.prefp`         | `any`              | `bstr`           |
| `.prefpseq`      | `any`              | `bstr`           |
| `.dtrm`          | `any`              | `bstr`           |
| `.dtrmseq`       | `any`              | `bstr`           |

All six follow the same rule: the carrier may be `any` or `bstr`; the controller may be a scalar, array, map/group, or wildcard.

## The canonical example

<!-- rumdl-disable MD040 -->

```cddl
; "some representation that deterministically serializes as type2"
wrapped = any .dtrm type2
```

<!-- rumdl-enable MD040 -->

This shape lets the CDDL author express the serialization constraint without forcing every consumer of `wrapped` to narrow the
carrier to `bstr` first.
The rendered CDDL shows the same wire shape either way;
the `any` form just doesn't add a redundant `bstr` constraint at the use site.

## When to use `any`

Use `any` on the LHS when:

* The rule is meant to convey "some serialization of `T`"
  without taking a position on whether the wrapping carrier
  is a byte string or some other CBOR item.
* The schema is being authored at a level where the carrier
  is not yet known and you want to defer that decision.
* You are writing a generic whose instantiation may use
  either `bstr` or `any` as the outer carrier and you want
  the generic definition to be permissive.

<!-- rumdl-disable MD040 -->

```cddl
wire<inner> = any .cbor inner
strict<inner> = any .dtrm inner

consumer = wire<message-payload>
producer = strict<message-payload>
```

<!-- rumdl-enable MD040 -->

## When to prefer `bstr`

Use `bstr` on the LHS when:

* The wire format carries the value as a byte string and the schema should reflect that.
  For example, a `bstr .cbor headers` field on a COSE envelope is a byte string on the wire and should be written as such.
* The `bstr` carrier narrows the type in a way the consumer
  relies on (for example, a hash or digest field).
* You are using the `.x-enc` / `.x-hash` / `.x-compressed` family — those operators expect `bstr` and reject `any`.
  See `experimental-ctlops.md` for that surface.

## `.within` with `any`

`.within` works the same way regardless of which carrier you chose.
The carrier (`any` or `bstr`) and the transform family are compared, just as they are with the `.x-` operators.

<!-- rumdl-disable MD040 -->

```cddl
ok = (any .dtrm type2-narrow) .within (any .dtrm type2-wide)
ok = (any .dtrm type2-narrow) .within (bstr .dtrm type2-wide)
```

<!-- rumdl-enable MD040 -->

A permissive `any` carrier on the LHS subtypes a `bstr` carrier on the RHS only when the controllers subtype;
the `any` carrier does not magically widen the LHS into a supertype of `bstr .dtrm T`.

## Debugging the permissive-carrier form

When a `.within` involving `any` raises an `E030`:

1. Run `cbork render FILE`.
   The rendered output makes the carrier visible —
   sometimes a permissive `any` was a leftover from an earlier draft and a `bstr` would resolve the `.within` cleanly.
2. Compare controllers, not carriers.
   The transform-family comparison is the same as for `bstr`, but the rendered carrier will read `any` rather than `bstr`,
   which can obscure the controller comparison.
3. Confirm the operator is in the table above.
   Operators outside that list — `.x-enc`, `.x-hash`, `.x-compressed`, the per-algorithm compression wrappers —
   do not accept `any` as the carrier.
