---
name: cddl-validation-vectors
description: |
  Use this skill when adding CBOR validation checks to a
  project that uses cbork. Covers `cbork validate`, `cbork decode`,
  positive and negative vectors, stdin/file input, detailed failure
  output, and practical CI layout.
---

# Validation Vectors With cbork

Use this sub-skill when the task is to prove that encoded CBOR bytes match a CDDL schema,
or to build a test workflow around example payloads.

## Basic validation

`cbork validate SCHEMA DATA` compiles the CDDL schema and validates the CBOR bytes in `DATA`.

<!-- rumdl-disable MD040 -->

```shell
cbork validate schemas/message.cddl test/vectors/message-ok.cbor
```

<!-- rumdl-enable MD040 -->

The schema argument is required.
The data argument may be omitted when bytes are piped on standard input:

<!-- rumdl-disable MD040 -->

```shell
producer --fixture message-ok | cbork validate schemas/message.cddl
```

<!-- rumdl-enable MD040 -->

Use stdin for one-off local checks.
Use files for committed regression vectors so failures are reproducible.

## Inspecting CBOR

Use `cbork decode DATA` when you need to see the CBOR structure before debugging the schema.

<!-- rumdl-disable MD040 -->

```shell
cbork decode --pretty test/vectors/message-ok.cbor
```

<!-- rumdl-enable MD040 -->

Use `cbork validate --detailed SCHEMA DATA` when validation fails and you need the decoded tree shown together with schema notes:

<!-- rumdl-disable MD040 -->

```shell
cbork validate --detailed schemas/message.cddl test/vectors/message-bad.cbor
```

<!-- rumdl-enable MD040 -->

`decode` answers "what bytes did the encoder produce?" `validate --detailed` answers "where do those bytes disagree with the
schema?"

## Positive and negative vectors

Keep both accepted and rejected vectors.
Positive vectors prove the encoder and schema agree.
Negative vectors prove important validation boundaries are enforced.

Recommended layout:

<!-- rumdl-disable MD040 -->

```text
schemas/
  message.cddl
test/
  vectors/
    positive/
      message-minimal.cbor
      message-full.cbor
    negative/
      message-missing-required-key.cbor
      message-wrong-tag.cbor
```

<!-- rumdl-enable MD040 -->

For positive vectors, CI should expect `cbork validate` to exit successfully.
For negative vectors, CI should expect `cbork validate` to fail.
Write the negative-vector harness in the project's test runner rather than using `--no-fail`,
because `--no-fail` turns failures into a successful process status.

## CI pattern

A portable shell-oriented pattern for positive vectors:

<!-- rumdl-disable MD040 -->

```shell
for vector in test/vectors/positive/*.cbor; do
  cbork validate schemas/message.cddl "$vector"
done
```

<!-- rumdl-enable MD040 -->

A portable shell-oriented pattern for negative vectors:

<!-- rumdl-disable MD040 -->

```shell
for vector in test/vectors/negative/*.cbor; do
  if cbork validate schemas/message.cddl "$vector"; then
    echo "negative vector unexpectedly passed: $vector" >&2
    exit 1
  fi
done
```

<!-- rumdl-enable MD040 -->

If the project has multiple wire formats, keep a small manifest that maps each schema to its vector directory.
Do not guess the schema from the file name in CI unless the naming convention is already enforced elsewhere.

## When schema warnings matter

By default, validation focuses on whether bytes match the compiled schema.
Use `--warn` when schema compiler warnings should be printed in full during vector checks:

<!-- rumdl-disable MD040 -->

```shell
cbork validate --warn schemas/message.cddl test/vectors/positive/message-full.cbor
```

<!-- rumdl-enable MD040 -->

If warnings should fail CI, run `cbork lint --strict` before the vector loop.
Do not rely on validation alone to enforce schema hygiene.
