---
name: cddl-within
description: |
  Use this skill when writing `.within` subtype constraints,
  including the cbork-specific compatibility matrix between
  carriers and the `.x-` transform families. Covers the basic
  carrier-and-controller comparison, the `E030` control-mismatch
  diagnostic, and the pattern for narrowing a generic or socket
  plug with `.within`.
---

# `.within` as a Type Constraint

`.within` is CDDL's "is a subtype of" predicate.
The LHS is the candidate type; the RHS is the target type.
The result is a type expression that is valid exactly when the LHS is a subtype of the RHS.

`cbork lint` and `cbork render` compare both the carrier and the transform identity when checking `.within`,
so the constraints are stricter than "the bstr shape matches".

## The basic comparison

For two plain CDDL types with no control operators,
`.within` asks whether every value accepted by the LHS is also accepted by the RHS.

<!-- rumdl-disable MD040 -->

```cddl
; int .within number     ; true: every int is a number
; number .within int     ; false: a number may not be an int
```

<!-- rumdl-enable MD040 -->

You will mostly see `.within` used on the LHS of a `=`:

<!-- rumdl-disable MD040 -->

```cddl
ok  = payload-narrow .within payload-wide
bad = payload-wide  .within payload-narrow
```

<!-- rumdl-enable MD040 -->

## Narrowing a generic with `.within`

The common pattern is "I have a concrete instantiation of a generic and I want to assert
that it is a subtype of a slightly broader instantiation":

<!-- rumdl-disable MD040 -->

```cddl
ok = (bstr .x-enc payload-narrow) .within (bstr .x-enc payload-wide)
```

<!-- rumdl-enable MD040 -->

The carrier (`bstr`) matches, the transform family (`.x-enc`) matches,
and the controller on the LHS (`payload-narrow`) subtypes the controller on the RHS (`payload-wide`).
The `.within` is true.

## The transform-family compatibility matrix

When a control operator appears on either side of `.within`, cbork compares the carrier and the transform identity.
The matrix below summarises when the LHS is within the RHS.

|                   | `bstr` (carrier) | `.x-enc`           | `.x-hash`           | `.x-compressed`      | `.x-brotli`/`.x-zstd`/`.x-gzip`/`.x-deflate` |
| ----------------- | ---------------- | ------------------ | ------------------- | -------------------- | ------------------------------------------ |
| `.x-enc`          | yes              | yes (controllers)  | no                  | no                   | no                                         |
| `.x-hash`         | yes              | no                 | yes (controllers)   | no                   | no                                         |
| `.x-compressed`   | yes              | no                 | no                  | yes (controllers)    | no                                         |
| `.x-brotli` etc.  | yes              | no                 | no                  | yes (controllers)    | yes same algorithm, no different algorithms |

A check means the LHS subtypes the RHS.
A cross means the LHS is not within the RHS and `.within` emits a control-mismatch diagnostic (`E030`)
that names the incompatible operators.

The full behaviour of each operator family is described in `experimental-ctlops.md`;
this matrix is the short summary to keep in mind when you write a `.within` against one of the `.x-` operators.

## The LHS-must-narrow-the-RHS rule

When the LHS is `bstr .x-enc T_narrow` and the RHS is the bare carrier `bstr`, the result is always true:
the LHS carrier narrows the RHS carrier, so any value accepted by the LHS is accepted by the RHS.

<!-- rumdl-disable MD040 -->

```cddl
ok = (bstr .x-enc payload-narrow) .within bstr
```

<!-- rumdl-enable MD040 -->

When the two sides differ in transform family — for example `bstr .x-enc T` versus `bstr .x-hash T` — the LHS is not within the RHS,
and `.within` emits `E030`.

## Common patterns

**Restrict a generic to a narrower controller.**

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Export
narrow-signed = COSE_Sign<envelope-headers, message-payload-narrow>
                 .within COSE_Sign<envelope-headers, message-payload-wide>
```

<!-- rumdl-enable MD040 -->

**Restrict a control-operator family to a specific algorithm.**

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Export
brotli-only = bstr .x-brotli payload
              .within bstr .x-compressed payload
```

<!-- rumdl-enable MD040 -->

**Reject an incompatible transform family.**

<!-- rumdl-disable MD040 -->

```cddl
; bad: E030 control-mismatch
bad = bstr .x-enc payload .within bstr .x-hash payload
```

<!-- rumdl-enable MD040 -->

## Debugging `E030`

When `cbork lint` raises `E030`:

1. Run `cbork render FILE` to see the effective CDDL on both sides of the `.within`.
   The rendered output makes the carrier and controller visible.
2. Compare the transform families on each side.
   A mismatch in the matrix above is the most common cause.
3. Compare the controllers.
   A subtype mismatch on the controller surfaces as the same `E030`; render the controllers to see whether one narrows the other.
4. If the LHS uses `any` as the carrier, see `any-as-lhs.md`
   for the special-case behaviour.
