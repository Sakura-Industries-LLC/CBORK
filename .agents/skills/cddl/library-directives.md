---
name: cddl-library-directives
description: |
  Use this skill when deciding whether a CDDL file is a library,
  when marking rules as exported, or when declaring an external
  dependency. Covers `;@ CBORK: Library`, `;@ CBORK: Export`, and
  `;@ CBORK: Extern`, plus the direct-use export contract (`W003`)
  and the directive-hygiene diagnostics (`E021`, `E022`, `W002`).
---

# Library Directives (`;@ CBORK:`)

cbork uses `;@`-prefixed comment annotations to express library-level intents that the CDDL grammar cannot encode.
All CBORK directives live in the `CBORK:` namespace;
directives from any other namespace are surfaced as warnings so users notice that their tool annotation was ignored.

## Recognised directives

| Directive                     | Effect                                                                          |
| ----------------------------- | ------------------------------------------------------------------------------- |
| `;@ CBORK: Library`           | Marks this file as a reusable library module. Must appear before any non-comment content. |
| `;@ CBORK: Export`            | Marks the **next rule** as part of the library's public export surface. The compiler tags the rule with `MetaData::Exported` and records the name in `CompiledCDDL::exported_names`. |
| `;@ CBORK: Extern <name>,...` | Declares names that this library treats as external. Requires `;@ CBORK: Library` in the same file. |

Minimal example:

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Library

;@ CBORK: Export
public-rule = uint

private-helper = bstr .size 16  ; not exported, internal use only
```

<!-- rumdl-enable MD040 -->

## When to mark a file as a library

Mark a file as a library when other files will `include` or `import` from it.
The Library marker is what tells the consumer's linter to apply the direct-use export contract to that file's symbols.

A file without `;@ CBORK: Library` is treated as a regular schema file.
Consumers can still include it; they just do not get the `W003` direct-use check.

## When to export a rule

Export the rules that consumers are meant to reference directly.
Do not export every rule.
Helpers that exist only to back an exported rule stay private;
the `cbork lint` reachability pruner drops them in the consumer's tree,
and a `W003` warning is the sign that a consumer reached for a private helper it should not have known about.

## When to use `Extern`

Use `;@ CBORK: Extern` to declare a name that your library *uses* but does not *define*, when the name comes from a CDDL postlude
(for example, primitive types from another module) and you want to make that intent explicit.

<!-- rumdl-disable MD040 -->

```cddl
;@ CBORK: Library
;@ CBORK: Extern fancy-thing, other-thing

;@ CBORK: Export
wrapper = fancy-thing / other-thing
```

<!-- rumdl-disable MD040 -->

`Extern` is informational: it tells the linter that the name is expected to be unresolved inside this file,
so the linter does not raise a missing-definition diagnostic for it.
It does not pull the definition into the file; that still happens through `include` / `import` if the consumer needs it.

## Directive placement

`;@ CBORK: Library` must appear before any non-comment content in the file.
`;@ CBORK: Export` must be followed (after any whitespace or doc comments) by a single rule.

These situations are all rejected with `E022`:

* `;@ CBORK: Export` in a non-library file.
* `;@ CBORK: Export` with no following rule (EOF).
* `;@ CBORK: Export` immediately before an `import` / `include`
  directive comment.
* Two consecutive `;@ CBORK: Export` directives with no rule
  between them.

`;@ CBORK: Extern <name>,...` requires `;@ CBORK: Library` in the same file.
Putting `Extern` in a non-library file is a configuration mistake and the linter flags it.

## Directive hygiene

Unknown CBORK directives (for example `;@ CBORK: Thing`) emit an `E021` diagnostic
because they look like active CBORK processing directives but are not recognised.
Directives from any other namespace (for example `;@ OTHER: ...`) emit a `W002` warning so users notice
that their tool annotation was ignored:

<!-- rumdl-disable MD040 -->

```cddl
;@ OTHER: do-something   ; ignored; emits W002
```

<!-- rumdl-enable MD040 -->

The recognised CBORK directive set is small and stable.
If you need a new directive, that is a change in `crates/cbork-cddl-compiler/src/compiled.rs`
(`parse_cbork_comment` and the `CborkDirective` enum) — not a content-only change to your schema.

## The direct-use export contract

When a file directly imports or includes another file that is marked as a CBORK library,
any reference from the consumer's own rules to a symbol defined in that library must point at an exported
(`;@ CBORK: Export`) or externally-declared (`;@ CBORK: Extern`) name.
Direct references to non-exported helpers emit a `W003` warning; strict mode fails on that warning.

The contract does not apply when:

* The consumer references the imported name transitively
  through an exported symbol (the helper is private but
  reachable through the library's own exported surface).
* The imported module is not marked as a CBORK library
  (unlabelled imports and unlabelled includes never
  participate).
* The consumer itself has declared the symbol as
  `;@ CBORK: Extern <name>` (the consumer has opted in to that
  name explicitly).
* The reference resolves to a postlude-injected primitive
  (for example `uint`, `bstr`, `any`) rather than a definition
  from the imported file.

Library exports are public API surface, not a required-use contract.
A whole-library import is valid when the consumer references any imported symbol it needs;
unused sibling exports from that library do not produce a consumer-side warning.
