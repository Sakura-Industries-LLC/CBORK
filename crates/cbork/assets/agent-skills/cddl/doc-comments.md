---
name: cddl-doc-comments
description: |
  Use this skill when writing or revising `;!` documentation comments
  in `.cddl` files that are checked by `cbork lint --doc`. Covers the
  required file shape (one L1 heading, L2 sections, L3 per definition),
  definition comments, generic parameter documentation, list
  indentation, section sizing, and common mistakes.
---

# CDDL Documentation With cbork

Use this skill when writing or revising documentation comments in `.cddl` files that are checked by `cbork lint --doc`.

## Required File Shape

Every documented CDDL file should start with a file-level documentation block.
The first documentation heading in the file must be exactly one level-1 heading.

Preferred form:

<!-- rumdl-disable MD040 -->

```cddl
;! # File Or Module Name
;!
;! Short description of what this CDDL file defines.
;! Explain the protocol, package, or data model scope in concrete terms.
;! State important interoperability or security context when relevant.
```

<!-- rumdl-enable MD040 -->

Use level-2 headings to split the file into major sections.
Each section should cover one coherent part of the schema.
There should be one level-2 heading per conceptual section,
not several adjacent level-2 headings for fragments that belong together.

Preferred form:

<!-- rumdl-disable MD040 -->

```cddl
;! ## Public Envelope
;!
;! Describe the group of definitions in this section.
;! Explain how this section fits into the file-level model.
```

<!-- rumdl-enable MD040 -->

Use level-3 headings for definitions.
Each documented definition should have a level-3 heading immediately before the definition it documents.
The heading should include the exact definition name.

Preferred form:

<!-- rumdl-disable MD040 -->

```cddl
;! ### message-envelope
;!
;! `message-envelope` is the outer wire representation for a signed message.
;! It carries protected headers, the encoded payload, and one or more signatures.
;! Implementations use this rule when serializing or validating top-level
;! message objects.
message-envelope = [
  protected: bstr .cbor protected-headers,
  payload: bstr .cbor message-payload,
  signatures: [+ message-signature],
]
```

<!-- rumdl-enable MD040 -->

## Definition Comments

A definition comment should cover three things:

1. What the definition is.
2. How it is used by the surrounding protocol or data model.
3. Any constraints that are not obvious from the CDDL shape alone.

Prefer direct wording.
Do not repeat the CDDL mechanically.
Explain the intent behind the shape.

Good:

<!-- rumdl-disable MD040 -->

```cddl
;! ### recipient-info
;!
;! `recipient-info` describes the key agreement material for one recipient.
;! The sender includes one entry per recipient that can decrypt the content
;! encryption key.
;! The encrypted key field is opaque to the outer envelope and is interpreted by
;! the selected key agreement algorithm.
recipient-info = [
  recipient-id: bstr,
  encrypted-key: bstr,
]
```

Weak:

```cddl
;! ### recipient-info
;!
;! A list with a recipient id and encrypted key.
recipient-info = [
  recipient-id: bstr,
  encrypted-key: bstr,
]
```

<!-- rumdl-enable MD040 -->

## Generic Definitions

Generic definitions must document each type parameter.
Use a short "Parameters" subsection inside the level-3 definition comment.
Each parameter entry should say what the parameter represents and how the definition uses it.

Preferred form:

<!-- rumdl-disable MD040 -->

```cddl
;! ### COSE_Encrypt0<headers, payload>
;!
;! `COSE_Encrypt0` is a single-recipient encrypted COSE object.
;! It binds protected headers to an encrypted payload and carries the ciphertext
;! as a byte string.
;!
;! Parameters:
;!
;! - `headers`: the protected header map shape encoded into the first field.
;! - `payload`: the plaintext CDDL type represented by the encrypted ciphertext.
COSE_Encrypt0<headers, payload> = [
  protected: bstr .cbor headers,
  unprotected: {},
  ciphertext: bstr .x-enc payload,
]
```

<!-- rumdl-enable MD040 -->

## Comment Formatting

Use `;!` documentation comments for user-facing documentation.
Use a blank `;!` line between headings and body text.
Use one sentence per line where the surrounding file already follows that style.
Keep Markdown structurally valid as if the `;!` marker were removed from each line.

For list indentation, write the Markdown indentation you would write in a normal Markdown file after removing the `;!` prefix.
Do not align nested list text to the visual CDDL comment column.

Preferred list form:

<!-- rumdl-disable MD040 -->

```cddl
;! * First item.
;! * Second item:
;!     * Nested item using Markdown indentation.
;!     * Another nested item.
```

Avoid:

```cddl
;! - First item.
;! - Second item:
;!   - Ambiguous nested indentation.
```

<!-- rumdl-enable MD040 -->

## Section Sizing

A section should explain a coherent part of the schema.
If a section has only one small definition, consider whether it should be merged into a neighboring section.
If a section has many unrelated definitions, split it into multiple level-2 sections.

Useful section names include:

<!-- rumdl-disable MD040 -->

```cddl
;! ## Public Envelope

;! ## Protected Headers

;! ## Payload Model

;! ## Signature Model

;! ## Validation Notes
```

<!-- rumdl-enable MD040 -->

## Validation Notes

Use a final level-2 "Validation Notes" section when the file has cross-field rules, policy checks,
or security requirements that are not fully expressible in CDDL.
Keep these notes specific and testable.

Preferred form:

<!-- rumdl-disable MD040 -->

```cddl
;! ## Validation Notes
;!
;! - The `version` field identifies the wire format version and must be checked
;!   before interpreting extension fields.
;! - Implementations must reject duplicate labels in protected header maps.
;! - The `payload` field is authenticated by the surrounding signature envelope.
```

<!-- rumdl-enable MD040 -->

## Common Mistakes

Do not start a file with a level-2 or level-3 heading.
Do not document a definition with a level-1 or level-2 heading.
Do not put several definition names in one level-3 heading unless the following text genuinely documents a tightly coupled group.
Do not use documentation comments for ordinary inline implementation notes.
Use regular `;` comments for local notes that are not part of the generated documentation.
Do not omit parameter documentation for generic definitions.
