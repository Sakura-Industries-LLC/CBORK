---
name: cddl-diagnostics-reference
description: |
  Use this skill when debugging cbork diagnostics in a
  CDDL project or when using cbork's standards-reference helpers.
  Covers diagnostic triage, `cbork why`, `cbork xref`, `cbork rfc`,
  render-oriented output, strict versus advisory runs, and when to
  inspect the effective schema.
---

# Diagnostics and Reference Lookup With cbork

Use this sub-skill when cbork reports a diagnostic and the next step is not obvious,
or when a schema author needs standards context without leaving the command line.

## Triage order

Use this order when debugging a schema:

1. Run `cbork lint FILE` to get parse, semantic, and lint diagnostics.
2. If the diagnostic mentions an effective schema, run `cbork render FILE`.
   This is especially useful for `.within`, `.and`, generics, sockets, and imported rules.
3. If the diagnostic code is unfamiliar, run `cbork why CODE`.
4. If the issue is standards terminology or a control operator, run `cbork xref TERM`.
5. If you need the embedded standards text, run `cbork rfc DOC`.

`cbork render` is the most useful next command when the source text is compact but the compiler is checking expanded rules.
It shows named-rule expansion, generic substitution, socket plugs, and schema-relevant control operators.

## `cbork why`

`cbork why CODE` explains why a diagnostic exists and prints the embedded standards citations cbork uses for that rationale.

<!-- rumdl-disable MD040 -->

```shell
cbork why E030
cbork why W003 W005
```

<!-- rumdl-enable MD040 -->

Use `why` before changing a schema only to silence a warning.
If the rationale shows that the warning protects a real interoperability contract,
fix the schema shape instead of suppressing the rule.

## `cbork xref`

`cbork xref TERM` looks up CDDL terms, control operators, and standards concepts in cbork's embedded reference index.

<!-- rumdl-disable MD040 -->

```shell
cbork xref within
cbork xref cbor
cbork xref "deterministic encoding"
```

<!-- rumdl-enable MD040 -->

Use `xref` when deciding whether a construct is standard CDDL, a cbork extension, or a standards-derived convention.
For example, use it before replacing a precise control operator with a loose `bstr` plus a comment.

## `cbork rfc`

`cbork rfc` lists the embedded standards corpus.
`cbork rfc DOC` prints one embedded document.

<!-- rumdl-disable MD040 -->

```shell
cbork rfc
cbork rfc rfc8610
```

<!-- rumdl-enable MD040 -->

Use this for local reference while editing a schema.
Do not treat it as a license to copy long standards excerpts into project documentation;
summarize the relevant constraint and cite the standard instead.

## Strict and advisory runs

Use strict lint when diagnostics should block merging:

<!-- rumdl-disable MD040 -->

```shell
cbork lint --strict schemas/message.cddl
cbork lint --doc --strict schemas/message.cddl
```

<!-- rumdl-enable MD040 -->

Use advisory lint while adopting cbork across an existing schema set:

<!-- rumdl-disable MD040 -->

```shell
cbork --no-fail lint --summary --recursive schemas/
```

<!-- rumdl-enable MD040 -->

`--no-fail` only changes the process exit code.
It does not hide diagnostics or make invalid schemas valid.

Use render JSON output when another tool needs the effective schema as a string:

<!-- rumdl-disable MD040 -->

```shell
cbork render --json schemas/message.cddl
```

<!-- rumdl-enable MD040 -->

Keep human-readable render output for local debugging.
Use `render --json` only at integration boundaries where a script actually consumes it.
