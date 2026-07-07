---
name: cddl-clean-style
description: |
  Use this skill when shaping, refactoring, or reviewing CDDL rules
  for readability and correctness in a project that uses cbork.
  Covers rule naming, group composition, generics, plug/socket,
  numeric bounds, size and bits, and how cbork's renderer and
  linter audit each of those decisions.
---

# Writing Clean CDDL With cbork

This sub-skill covers general CDDL authoring practice: how to write CDDL that other people
(and your own future self) can read, and how `cbork lint` and `cbork render` audit each of those decisions.

The rules below are not enforced by a single lint code;
they are the kind of choices that change what `cbork render` produces and what the documentation lint pass can find.

## Rule names

Use lowercase, kebab-case identifiers for rules that ship on the wire,
and reserve `SCREAMING_SNAKE_CASE` for generic parameters and well-known external CDDL identifiers (COSE, SPKI, etc.).

<!-- rumdl-disable MD040 -->

```cddl
message-envelope = [...]
message-signature = [...]
```

<!-- rumdl-enable MD040 -->

Avoid:

* Hungarian-style prefixes (`t_`, `s_`, `a_`); the CDDL type
  system already conveys "this is a `tstr`".
* Single-letter rule names except for very local helper types
  that are inlined into one rule and never cross-referenced.

A descriptive name lets `cbork render` show a meaningful expansion when the rule is substituted into a generic or socket.

## Group composition

Order keys in a group from most stable to most volatile.
Required keys come first, optional keys with `?` come after.

<!-- rumdl-disable MD040 -->

```cddl
person = {
  name: tstr,
  age:  uint,
  ? email: tstr,
  ? roles: [+ role],
}
```

<!-- rumdl-enable MD040 -->

Do not let a key's wire-order number (the `0:`, `1:` that the renderer shows) be the only thing that documents the intent.
Prefer named keys unless the wire protocol explicitly mandates integer keys.

`cbork render` prints groups in source order, so a stable source order is also a stable rendered order.

## Generics

Use generics when the same shape is reused with different inner types.
Do not use generics where the inner type is fixed — write the concrete type.

Good:

<!-- rumdl-disable MD040 -->

```cddl
COSE_Encrypt0<headers, payload> = [
  protected:   bstr .cbor headers,
  unprotected: {},
  ciphertext:  bstr .x-enc payload,
]

signed-message  = COSE_Sign<envelope-headers, message-payload>
encrypted-blob  = COSE_Encrypt0<envelope-headers, message-payload>
```

<!-- rumdl-enable MD040 -->

Weak:

<!-- rumdl-disable MD040 -->

```cddl
COSE_Encrypt0<headers, payload> = [...]  ; always called with one fixed pair
```

<!-- rumdl-enable MD040 -->

Document every generic parameter.
`cbork lint --doc` warns on undocumented parameters; see `doc-comments.md` for the parameter-block convention.

## Plug and socket

Use `$plugname` and `$socketname` to mark the substitution points in a generic rule.
Prefer explicit, named plugs over a single unnamed `$` so that `cbork render` shows which type fills each socket.

<!-- rumdl-disable MD040 -->

```cddl
signed-envelope<signatures> = {
  payload:     bstr .cbor payload,
  signatures:  [+ $signatures],
}
```

<!-- rumdl-enable MD040 -->

When you instantiate the generic with a concrete type, the renderer shows the substituted shape,
which is the easiest way to check that you filled every socket.

## Numeric bounds

Express numeric bounds with `.gt`, `.ge`, `.lt`, `.le` rather than splitting the rule into multiple choices.

<!-- rumdl-disable MD040 -->

```cddl
port = uint .le 65535
nonneg = int .ge 0
```

<!-- rumdl-enable MD040 -->

Do not write `uint .lt 65536` when you mean "16-bit unsigned"
if the consumer of the schema does not care about the upper bound exactly.
The renderer's bounds check works either way, but the intent is clearer with `.le 65535`.

## Size and bits

Use `.size N` on `bstr` and `tstr` to fix the length to a constant.
Use `.bits NAME` on a `uint` to refine it with a named bit map.

<!-- rumdl-disable MD040 -->

```cddl
uuid      = bstr .size 16
sha256    = bstr .size 32
flags     = uint .bits flags-bitmap
```

<!-- rumdl-enable MD040 -->

`.size N` is exact.
If you mean "up to N", prefer a separate length-bounded rule plus a CDDL-style socket;
do not paper over the difference with `.size N` plus a comment.

## Comments

Use `;` for inline implementation notes that are local to the rule.
Use `;!` for user-facing documentation that `cbork lint --doc` should pick up; see `doc-comments.md` for that surface.
Use `;@` only for directives that cbork understands (`CBORK:` namespace) — see `library-directives.md`.
Use `;#` only for `include` / `import` directives — see `includes-and-imports.md`.

## Reachability and prunability

`cbork lint` reports rules that no other rule references and that are not exported.
Keep that in mind:

* Helpers used by exactly one exported rule are fine to keep
  in the same file as private `=` rules.
* Helpers shared by multiple exported rules belong in their own
  library file with `;@ CBORK: Library` and `;@ CBORK: Export`;
  see `library-directives.md`.
* Rules you keep purely for documentation should be exported or
  the linter will eventually warn that they are unreachable.

## How cbork audits this

`cbork lint` reports:

* Unused or unreachable rules (the reachability pruner drops
  them; the linter notes the drop).
* Rules that break the export contract when the file is a
  CBORK library (`W003`); see `library-directives.md`.
* Documentation gaps (`cbork lint --doc`); see
  `doc-comments.md`.
* Subtype mismatches in `.within` (`E030`); see `within.md`.

`cbork render` produces the effective CDDL that the compiler actually reasons about.
Read it whenever a generic or socket expansion looks wrong — the rendered text shows the substituted shape,
which is the fastest way to debug a binding mistake.
