# CBORK Product Feature Document

## 1. Product Summary

**CBORK** is a first-class CBOR/CDDL tooling suite for building, validating, documenting, testing,
and maintaining binary protocol contracts.

Its purpose is simple: make CDDL enforceable.

CDDL should make CBOR interoperability easier.
In practice, too much CDDL is written as loose schema prose, interpreted differently by tools,
and backed by a handful of happy-path examples.
CBORK fixes that by treating CDDL as a real source artifact: parsed conformantly, formatted, linted, documented,
validated against actual CBOR, measured for coverage, and presented clearly in API documentation and editors.

CBORK is not only a parser.
It is a complete protocol-contract toolchain.

## 2. Core Product Thesis

CBOR APIs should not fall back to JSON-shaped documentation when the actual wire format is binary.

If the payload is CBOR, then the contract must be expressed and enforced as CDDL.
The human-readable view should be diagnostic notation, enhanced by schema context, not fake JSON examples that hide byte strings,
tags, canonicalization, embedded constraints, binary keys, and CBOR-specific structure.

CBORK makes the binary contract visible, testable, and enforceable.

The goal is that an ecosystem can say:

> If it does not pass CBORK, it is not valid for this ecosystem.

That changes CDDL from decorative schema text into an executable interoperability boundary.

## 3. Product Principles

### 3.1 CDDL is the contract

CDDL is not an appendix to Markdown.
It is the machine-checkable contract for the binary payload.

Markdown can explain rationale, security considerations, migration policy, and implementation guidance,
but any rule that can be expressed mechanically should live in CDDL, validator logic, lint policy, or conformance vectors.

### 3.2 CBOR is not JSON

CBOR should be documented as CBOR.
JSON-style examples are at best a convenience view and at worst a second accidental protocol.

CBORK should prefer:

* CBOR diagnostic notation
* CDDL-informed diagnostic notation
* binary conformance vectors
* schema-linked examples
* explicit content-type and root-rule mapping

### 3.3 Strict by default

CBORK should be standards-conformant and strict by default.

If a schema is invalid, ambiguous, implementation-dependent, or relying on behaviour not actually defined by CDDL,
CBORK should say so plainly.

Compatibility modes may exist, but they should not define the default ecosystem contract.

### 3.4 Hard versioning over drift

CDDL should not grow a drifting surface area.

If a payload changes, define a new versioned rule.
V1 and V2 may be 90% identical, but they are still different contracts.

Lifecycle policy belongs in the surrounding API layer, not inside the CDDL contract itself.
The API documentation can say which payload versions are accepted, current, preview, or scheduled for retirement.

### 3.5 Maps are closed unless explicitly open

A map should be treated as exhaustive unless the CDDL explicitly defines an open key space.

Adding a new key to a closed map is a new canonical version.
It is not a harmless extension.

Open maps must be explicit and constrained by key type and value type.
For example, an open map may allow any number of binary UUID-like keys, but only if the schema actually defines that key space.

### 3.6 Source may be modular, documentation must be complete

CDDL includes are useful while authoring.
They are a source-level/compiler-level feature.

Rendered documentation should be self-contained.
Included definitions should be expanded so readers are never left staring at an unresolved type name and wondering
where the real definition lives.

Include provenance should be retained as comments so readers can still see commonality boundaries and source module structure.

### 3.7 Examples must come from vectors

Examples should not be hand-written documentation fiction.

CBORK examples should be generated from conformance vectors, validated against the selected CDDL root,
rendered as diagnostic notation, and annotated using comments from the CDDL itself.

That makes examples reusable as tests, docs, debugging artifacts, and interoperability evidence.

## 4. Target Users

CBORK is for people building or consuming CBOR-based protocols and APIs.

Primary users:

* protocol designers
* standards authors
* API designers using CBOR payloads
* implementers building clients or servers in any language
* maintainers of binary API documentation
* test and conformance engineers
* security reviewers
* editor and tooling users working with CDDL

Secondary users:

* developers debugging CBOR payloads
* teams migrating from ad hoc binary formats to CBOR/CDDL
* ecosystems that need a common conformance gate across multiple implementations

## 5. Major Product Areas

CBORK consists of the following product areas:

1. Conformant CDDL parser and semantic model
2. CDDL include handling and expansion
3. Markdown-aware documentation comments
4. CDDL formatter
5. CDDL linter
6. CBOR decoder and diagnostic notation dumper
7. CDDL-informed diagnostic notation renderer
8. CBOR/CDDL conformance checker
9. Embedded grammar and regex constraint validation
10. Test vector and schema coverage system
11. OpenAPI documentation integration for CBOR bodies
12. Language Server Protocol implementation
13. Tree-sitter grammar for editor syntax
14. Zed extension
15. CI and ecosystem enforcement workflows

## 6. Conformant CDDL Parser

### 6.1 Purpose

The parser is the foundation of CBORK.
It must parse CDDL conformantly and produce a semantic model that later tooling can rely on.

This parser is not a loose convenience parser.
It is the authority for CBORK’s interpretation of CDDL.

### 6.2 Required capabilities

The parser should support:

* all current CDDL definitions
* standard rule definitions
* type rules
* group rules
* type choices
* group choices
* type choice extensions
* group choice extensions
* occurrence markers
* arrays
* maps
* tags
* literals
* ranges
* controls
* sockets and plugs
* generics
* comments
* includes
* embedded regex constraints
* embedded grammar constraints

### 6.3 Semantic model

The parser should not stop at syntax.

CBORK needs a semantic model capable of answering questions such as:

* What is the root rule?
* Which rules are reachable?
* Which rules are unused?
* Which choices are extended?
* In what order are choice extensions applied?
* Is `/=` being used where a type choice extension is valid?
* Is `//=` being used where a group choice extension is valid?
* Does this map allow unknown keys?
* Are map keys exhaustive?
* Are alternatives shadowed by earlier broader alternatives?
* Does this schema normalize to the same effective contract after expansion?
* Does the rendered expanded CDDL remain valid CDDL?

The root rule is implicit for a concrete document.
There is no user-selectable alternate root for validation or decode.
If a file is intended to be reusable schema material rather than a standalone document,
it may be marked as a library and analyzed under library rules instead.
Library mode relaxes the single-root requirement for authoring and analysis,
but it does not make the file valid input for decode or validate.

### 6.4 Parser diagnostics

Diagnostics should distinguish clearly between:

* syntax errors
* invalid CDDL constructs
* invalid include resolution
* invalid regex syntax
* invalid embedded grammar syntax
* semantic errors
* linter warnings
* conformance failures

A user should not receive a vague “parse failed” message when the real issue is
that an embedded regex is not valid under the CDDL-required regex rules.

## 7. Include Handling and Expansion

### 7.1 Source includes

CBORK should support CDDL includes as source-level modularity.

Includes allow schema authors to split common definitions into reusable files without copying type definitions everywhere.

### 7.2 Rendered expansion

When rendering documentation, CBORK should produce a self-contained expanded form.

The rendered CDDL should:

* include all referenced definitions
* retain original comments
* preserve include provenance as comments
* remove active include dependencies
* remain valid standalone CDDL
* avoid unresolved fragments

The goal is that a reader can understand the payload contract without chasing files or external fragments.

### 7.3 Include provenance

When an include is expanded, CBORK should retain provenance in comments.

This helps readers understand commonality boundaries.
If a block came from a common schema module, that signal should survive in the rendered document.

This is useful because implementers can see which types are intended to be shared across operations, modules, or protocol surfaces.

## 8. Markdown Documentation Comments

### 8.1 Purpose

CBORK should allow comments attached to CDDL elements to be treated as Markdown documentation.

This lets real documentation live inside the CDDL file instead of being detached into a separate `.md` file
that can drift away from the schema.

### 8.2 Comment attachment model

Documentation comments should attach predictably to schema elements.

The preferred rule is:

> Documentation comments attach to the next syntactic item unless explicitly marked otherwise.

This makes comments stable under formatting, rendering, and include expansion.

### 8.3 Rendered documentation

Markdown comments should render into API documentation, editor hovers, generated schema pages, and diagnostic notation annotations.

Supported output should include:

* rule descriptions
* field descriptions
* security notes
* implementation notes
* rationale
* references to related rules
* notes on canonicalization or encoding requirements
* examples linked to vectors

### 8.4 Documentation coverage

CBORK should be able to report documentation coverage.

Useful metrics:

* public root rules documented
* named rules documented
* map fields documented
* choices documented
* extension points documented
* embedded constraints documented
* examples available per public root

The aim is not to force comments everywhere.
The aim is to prevent public binary contracts from being unreadable without tribal knowledge.

## 9. Formatter

### 9.1 Purpose

The formatter makes CDDL a stable source artifact.

Without a formatter, style drifts.
Drifting style makes reviews harder and hides semantic changes inside formatting noise.

### 9.2 Formatter behaviour

The formatter should:

* preserve comments
* preserve Markdown documentation comments
* preserve include provenance comments in rendered output
* normalize indentation
* normalize spacing around assignments and operators
* format maps, arrays, groups, and choices consistently
* keep comments attached to the correct schema elements
* avoid changing semantic meaning
* be deterministic

### 9.3 Formatting and expansion

Source formatting and rendered formatting are related but distinct.

Source formatting preserves modular authoring structure.

Rendered formatting presents the expanded, self-contained schema for documentation and review.

Both should be deterministic.

## 10. Linter

### 10.1 Purpose

The linter is where CBORK moves beyond “valid CDDL” into “good CDDL.”

A schema can be syntactically valid and still be a bad interoperability contract.
The linter should detect those cases.

### 10.2 Linter categories

The linter should support categories such as:

* correctness risks
* interoperability risks
* readability issues
* dead schema
* ambiguous schema
* over-broad schema
* weak documentation
* weak test coverage
* portability risks
* versioning mistakes

### 10.3 High-value lint rules

Useful lint rules include:

* concrete document has no reachable root
* library file has no explicit aggregate export rule
* named rule is never referenced
* public rule has no documentation
* map appears closed but receives undocumented extra keys in examples
* map appears intended to be open but has no explicit open key rule
* choice alternative is shadowed by an earlier broader alternative
* `/=` used where `//=` appears intended
* `//=` used where `/=` appears intended
* choice extension target has no clear base definition
* rule order affects effective choice composition
* optional field has no positive example
* optional field has no absent-field example
* extension branch has no vector coverage
* occurrence marker lacks boundary coverage
* field accepts far more than comments imply
* embedded regex accepts broader values than comments imply
* embedded grammar is valid but never exercised by vectors
* schema has no negative vectors

### 10.4 Library files

Library files should still encourage an explicit aggregate export rule such as:

<!-- rumdl-disable MD040 -->

```cddl
library = type1 / type2 / type3 / type4 / type5
```

<!-- rumdl-enable MD040 -->

That keeps the public surface obvious and makes include/import usage predictable.

When a file is marked as a library:

* top-level dangling definitions may be allowed as library exports
* `lint` may warn if there is no explicit aggregate export rule
* `lint-fix` may insert the aggregate export rule when it can do so safely
* `validate` and `decode` must reject the file unless it is being consumed as part of another concrete document
* versioned schemas are mixed without explicit API-level lifecycle

### 10.4 Explain mode

CBORK should include an explain mode.

This is not just a warning system.
It should explain what the CDDL actually means.

Examples of explain-mode output:

* “This map is closed.
  Unknown keys are invalid.”
* “This choice extension is applied in rule order.”
* “This optional key is optional within this version only.
  Adding new keys creates a new version unless an open key space is explicitly defined.”
* “This value is a byte string structurally, but must contain valid CBOR for this rule.”
* “This string is constrained by embedded regex and failed at this position.”

Explain mode is especially valuable because many CDDL errors come from authors thinking they wrote one thing
while the schema actually means something else.

## 11. CBOR Decoder and Diagnostic Notation

### 11.1 Plain diagnostic notation

CBORK should decode arbitrary CBOR and render it as diagnostic notation without needing a CDDL schema.

This is the baseline debugging mode.

It should support:

* byte strings
* text strings
* integers
* floats
* arrays
* maps
* tags
* simple values
* indefinite forms where relevant
* malformed CBOR reporting
* offsets and path information
* canonicalization warnings where applicable

### 11.2 Diagnostic quality

Plain diagnostic notation should be readable enough for humans and precise enough for debugging.

A user should be able to inspect a binary payload and identify where a structure, tag, key, or value appears.

## 12. CDDL-Informed Diagnostic Notation

### 12.1 Purpose

Plain CBOR diagnostic notation tells users what the bytes are.

CDDL-informed diagnostic notation tells users what the bytes mean under a schema.

This is one of CBORK’s highest-value features.

### 12.2 Schema-aware rendering

Given a CDDL schema, a root rule, and a CBOR payload, CBORK should:

* validate the CBOR against the selected root
* render the CBOR as diagnostic notation
* annotate fields using CDDL comments
* show which rules matched
* show which choice alternatives matched
* show which optional fields are present or absent
* identify embedded constraints
* identify unknown but allowed open-map entries
* identify invalid or ignored entries where policy allows
* link rendered values back to CDDL rules

### 12.3 Interleaved comments

Generated diagnostic notation should support comments interleaved with values.

Those comments should be drawn from CDDL documentation comments wherever possible.

This makes examples self-explaining without inventing a second documentation language.

### 12.4 Variants and options

CDDL-informed examples should make protocol variants visible.

For each public root rule, documentation should be able to show:

* minimal valid payload
* common full payload
* each major choice branch
* each important optional field
* extension examples
* boundary examples
* invalid examples with rejection reasons

These examples should be generated from vectors, not manually written.

## 13. Conformance Checker

### 13.1 Purpose

The conformance checker validates actual CBOR payloads against actual CDDL schemas.

This is the enforcement core of CBORK.

### 13.2 Inputs

The checker should accept:

* CDDL schema
* selected root rule
* CBOR binary payload
* optional vector metadata
* optional policy profile
* optional known extension registries
* optional canonicalization requirements

### 13.3 Outputs

The checker should report:

* pass/fail
* matched root rule
* matched rule path
* failure path
* expected type or constraint
* actual value
* embedded regex or grammar failure details
* unknown open-map entries
* skipped entries where policy allows
* canonicalization warnings or failures
* coverage contribution
* diagnostic notation output

### 13.4 Support posture

CBORK enables a clean ecosystem support rule:

> Before opening an interoperability bug, show the payload passing the conformance tool,
> or provide the minimal failing schema and payload pair.

That turns disputes from interpretation arguments into concrete evidence.

### 13.5 Multi-language usefulness

CBORK should be language-neutral.

An implementation in Rust, Go, Python, JavaScript, C,
or any other language should be able to use CBORK as the reference conformance check.

If one implementation accepts a payload and CBORK rejects it,
either the implementation is wrong or CBORK has a minimal reproducible bug.

That is the correct dispute boundary.

## 14. Embedded Grammar and Regex Constraints

### 14.1 Purpose

Many binary protocols hide structure inside strings and byte strings.

Outer CBOR shape validation is not enough if a field contains an identifier, path, URI-like value, mini-language,
encoded substructure, or constrained byte sequence.

CBORK should validate those inner constraints.

### 14.2 Embedded grammar constraints

CBORK should support grammar constraints embedded in CDDL and validate data inside strings and byte fields against those grammars.

This allows CDDL to express not only that a field is a string or byte string,
but that its contents conform to a defined sub-language.

### 14.3 Regex constraints

CBORK should implement the regex semantics required by CDDL, rather than delegating to the host programming language’s regex engine.

This is important because host regex engines differ.

A schema should not mean one thing in Rust, another in JavaScript, and another in Python.

CBORK should validate regex syntax and matching according to the relevant CDDL-conformant regex rules.

### 14.4 Constraint diagnostics

Failures should identify the exact layer that failed.

Examples:

* CBOR decoded successfully
* field matched outer CDDL type
* embedded regex failed
* embedded grammar failed
* byte string was valid CBOR but inner schema validation failed

This makes debugging much clearer than a generic “message invalid” error.

## 15. Schema Coverage

### 15.1 Purpose

CBORK should measure how well a set of vectors exercises a CDDL schema.

This is analogous to code coverage.
It does not prove correctness.
It proves contact.

A schema with no coverage is suspicious.
A branch with no vector is suspicious.
An optional field with no present/absent examples is suspicious.

### 15.2 Coverage model

CBORK should measure coverage over the schema’s structural and semantic space.

Useful coverage dimensions:

* rule coverage
* root rule coverage
* type choice coverage
* group choice coverage
* choice extension coverage
* occurrence marker coverage
* optional key coverage
* mandatory key coverage
* open-map key coverage
* embedded regex coverage
* embedded grammar coverage
* numeric boundary coverage
* string and byte-size boundary coverage
* tag coverage
* negative rejection coverage
* canonicalization coverage

### 15.3 Boundary coverage

Occurrence markers should be tested at meaningful boundaries.

Examples:

* absent
* present once
* minimum
* maximum
* above maximum
* empty array
* non-empty array
* missing mandatory key
* duplicate key where invalid
* unknown key in closed map
* valid open-map key
* invalid open-map key

### 15.4 Negative coverage

Negative vectors are first-class.

A schema with only valid examples is not enough.
Implementations also need to agree on rejection behaviour.

CBORK should report negative coverage separately,
because many interoperability bugs come from one implementation accepting malformed data that another rejects.

### 15.5 Coverage reporting

Coverage reports should be understandable by humans and usable in CI.

A useful report should show:

* total schema coverage
* coverage per root rule
* uncovered rules
* uncovered choices
* uncovered extension branches
* uncovered optional fields
* uncovered negative cases
* coverage deltas compared with previous releases

### 15.6 Coverage policy

Ecosystems can define their own thresholds.

For example:

* public root rules require 100% structural coverage
* all choices require at least one positive vector
* all optional fields require present and absent examples
* all open-map key spaces require valid and invalid key examples
* all embedded regexes require positive and negative examples
* release requires no coverage regression

CBORK should provide the measurement.
Policy can be configured by the ecosystem.

## 16. Test Vector System

### 16.1 Purpose

Vectors are the bridge between schema, implementation, documentation, and support.

CBORK should treat vectors as first-class artifacts.

### 16.2 Vector metadata

Each vector should be able to declare:

* name
* description
* schema version
* root rule
* valid or invalid expectation
* covered features
* expected diagnostic output
* expected failure reason for invalid vectors
* canonicalization requirements
* related rules or fields

### 16.3 Vector-driven documentation

API examples should be selected from vectors.

This prevents docs from drifting away from tests.

A vector can appear simultaneously as:

* a conformance test
* an example in docs
* a coverage contributor
* a debugging fixture
* a regression test
* an implementation support artifact

### 16.4 Valid and invalid vectors

Both should be documented.

Valid vectors show how to build correct messages.

Invalid vectors show what must be rejected and why.

For interoperability, invalid vectors are often just as important as valid ones.

## 17. Versioning Model

### 17.1 Hard-versioned CDDL

CBORK should encourage hard-versioned schemas.

A payload version should be represented as a distinct root rule or distinct schema artifact.

V1, V2, and V3 are separate contracts, even if they share many definitions.

### 17.2 No CDDL-level deprecation drift

CBORK should not encourage deprecation tags inside CDDL as a way to keep old fields floating around inside one expanding schema.

Version lifecycle belongs outside the payload contract.

### 17.3 API-level lifecycle

The API documentation layer should say which versions are accepted.

For example:

* this endpoint accepts PayloadV1 and PayloadV2
* PayloadV1 is supported until a specific date
* PayloadV2 is current
* PayloadV3 is preview or experimental

The CDDL defines exact payload shape.
The API layer defines lifecycle and acceptance policy.

### 17.4 Closed maps and version changes

Adding a new key to a closed map creates a new version.

This rule avoids accidental breakage where producers add keys that consumers were never required to tolerate.

### 17.5 Explicit extension points

If future keys are intended, the schema must define an explicit open key space.

Open key spaces must constrain:

* key type
* key format
* value type
* value validation rules
* handling of unknown entries
* preservation or discard rules where relevant

Extensibility should be deliberate, not accidental.

## 18. OpenAPI Documentation Integration

### 18.1 Purpose

CBORK should integrate with OpenAPI documentation tools so CBOR payloads are documented properly.

OpenAPI is useful for the HTTP envelope: paths, methods, headers, status codes, content types, authentication schemes,
and response codes.

CDDL is the correct contract for CBOR bodies.

### 18.2 Binary schema display

For operations using CBOR payloads, the documentation should show:

* content type
* selected CDDL root rule
* accepted payload versions
* fully rendered CDDL
* source module provenance
* examples from vectors
* diagnostic notation
* validation status
* schema coverage
* conformance command
* known lifecycle policy from the API layer

### 18.3 Expanded CDDL display

The rendered schema should be self-contained.

Includes should be expanded, comments retained, and include references preserved as comments for traceability.

Readers should not be forced into a fragment hunt.

### 18.4 Focused root view

Large schemas can become noisy.

The documentation tool should support a focused root view that shows only definitions reachable from the selected request
or response root.

A full expanded view should still be available.

### 18.5 Example display

Examples should be rendered from vectors as annotated diagnostic notation.

The documentation should show major variations, optional fields, choice branches, and invalid examples.

The reader should be able to see not only what the bytes are, but why they match the schema.

### 18.6 Documentation authority

The documentation should make it clear that the CDDL is authoritative for the body.

JSON-like views, if provided, must be explicitly marked as non-authoritative convenience views.

## 19. Language Server

### 19.1 Purpose

The CBORK language server brings the toolchain into editors.

Syntax highlighting makes CDDL pleasant to read.
The language server makes it productive and safe to write.

### 19.2 Core features

The language server should provide:

* parse diagnostics
* semantic diagnostics
* lint diagnostics
* format-on-save
* hover documentation
* go-to-definition
* find references
* rename rule
* document symbols
* workspace symbols
* completion for rule names
* completion for built-in types and controls
* include resolution
* diagnostics for unresolved includes
* diagnostics for invalid regexes
* diagnostics for invalid embedded grammars
* root-rule awareness
* quick explanation for CDDL constructs

### 19.3 Schema-aware editor features

Higher-value features include:

* show effective expanded rule
* show reachable rules from root
* show unused rules
* show choice extension composition
* show map closed/open status
* show coverage status inline
* link vectors to rules
* run conformance check from editor command
* render selected CBOR as diagnostic notation
* render selected CBOR using selected CDDL root

### 19.4 LSP and CLI alignment

The LSP should use the same parser, semantic model, linter, formatter, and validator as the CLI.

There should not be one interpretation in the editor and another in CI.

## 20. Tree-sitter Grammar

### 20.1 Purpose

The Tree-sitter grammar is for editor syntax and structure.

It should not be the authoritative CDDL parser.
CBORK’s Rust parser remains the conformant parser.

Tree-sitter’s job is fast, resilient, incremental parsing for editor features.

### 20.2 Why Tree-sitter is still necessary

Even with an LSP, Tree-sitter is valuable for:

* syntax highlighting
* bracket matching
* structural selections
* outline support
* indentation
* injections
* basic navigation
* editor responsiveness while typing malformed CDDL

The LSP can provide semantic truth.
Tree-sitter provides immediate syntactic structure.

### 20.3 Grammar design

The grammar should be tolerant.

CDDL files are often incomplete while being edited.
A strict compiler parser can reject a half-written rule, but the editor still needs to highlight the rest of the file.

The Tree-sitter grammar should recover well from:

* incomplete rules
* missing braces
* missing commas
* partial strings
* partial comments
* incomplete includes
* half-written choices
* incomplete controls
* incomplete regex or grammar literals

### 20.4 Syntax coverage

The Tree-sitter grammar should recognize:

* rule names
* type names
* built-in types
* assignments
* type choices
* group choices
* choice extensions
* controls
* occurrence markers
* ranges
* maps
* arrays
* groups
* tags
* literals
* comments
* documentation comments
* include statements
* generics
* sockets and plugs
* regex bodies where syntactically visible
* embedded grammar bodies where syntactically visible

### 20.5 Query files

The Tree-sitter package should support editor query files for:

* highlights
* brackets
* outline
* indentation
* injections
* text objects if supported by the target editor
* redactions if useful

### 20.6 Highlighting goals

Highlighting should distinguish:

* comments
* documentation comments
* rule definitions
* rule references
* built-in types
* strings
* byte strings
* numbers
* booleans
* operators
* occurrence markers
* controls
* tags
* map keys
* include provenance
* invalid or error nodes where available

The goal is not colorful decoration.
The goal is to make CDDL structure visible.

### 20.7 Outline goals

The outline should show named rules.

Ideally it should distinguish:

* root rule
* type rule
* group rule
* extension rule
* imported/included rules
* public/exported rules if the project defines that concept

This lets a large CDDL file become navigable.

### 20.8 Injections

If documentation comments are Markdown,
the Tree-sitter grammar and editor queries should support Markdown injection into documentation comments.

If embedded regex or grammar fragments are syntactically represented,
injections can eventually support specialized highlighting for those fragments too.

### 20.9 Relationship to the LSP

Tree-sitter should not try to validate all CDDL semantics.

It should not decide final conformance.

That belongs to the CBORK parser and language server.

The split is:

* Tree-sitter: fast editor structure
* CBORK parser: conformant syntax and semantic model
* LSP: diagnostics, navigation, formatting, validation, and schema intelligence

## 21. Zed Extension

### 21.1 Purpose

The Zed extension should make CDDL feel native in Zed.

The minimum useful version should provide syntax highlighting and basic structural navigation.
The full version should connect to the CBORK language server.

### 21.2 Extension components

The Zed extension should provide:

* CDDL language registration
* file association for `.cddl`
* Tree-sitter grammar registration
* highlight queries
* bracket matching queries
* outline queries
* indentation queries
* Markdown injection for documentation comments
* language server integration
* formatter integration through the language server
* diagnostics through the language server
* commands for CBORK-specific actions where supported

### 21.3 Minimum viable Zed support

Minimum support:

* `.cddl` files open as CDDL
* syntax highlighting works
* comments and documentation comments are distinct
* rule names are highlighted
* built-in types are highlighted
* strings, byte strings, numbers, booleans, and operators are highlighted
* maps, arrays, and groups are structurally recognized
* outline lists rule definitions

This is the first credibility layer.

### 21.4 Proper Zed support

Proper support:

* LSP diagnostics
* hover documentation from Markdown comments
* go-to-definition for rule references
* find references
* format-on-save
* lint warnings
* include resolution
* embedded regex/grammar diagnostics
* command to show expanded CDDL
* command to run conformance check
* command to render selected CBOR as diagnostic notation
* command to show coverage for the current schema

### 21.5 Zed extension philosophy

The Zed extension should not be a separate implementation of CDDL.

It should be a client for CBORK.

That keeps editor behaviour aligned with CLI, CI, and documentation generation.

## 22. CLI Surface

### 22.1 Purpose

The CLI is the automation and CI interface.

It should expose the core product without requiring an editor or documentation tool.

### 22.2 Core commands

The CLI should support operations equivalent to:

* format CDDL
* lint CDDL
* parse CDDL
* expand includes
* render documentation CDDL
* validate CBOR against CDDL
* dump CBOR as diagnostic notation
* dump CBOR as CDDL-informed diagnostic notation
* run vector suite
* generate coverage report
* run CI policy checks
* start language server
* generate OpenAPI documentation assets

### 22.3 CLI design principle

CLI output should be human-readable by default and machine-readable when requested.

CI systems need stable output.
Humans need clear explanations.

Both matter.

## 23. CI and Ecosystem Enforcement

### 23.1 Purpose

CBORK should make enforcement easy.

A project should be able to gate releases on CDDL validity, formatting, linting, conformance, documentation, and coverage.

### 23.2 CI checks

Useful CI checks:

* all CDDL parses
* all CDDL formats cleanly
* no linter errors
* no forbidden warnings
* all includes resolve
* expanded rendered CDDL is valid
* all vectors pass
* invalid vectors fail for expected reasons
* coverage threshold met
* documentation comments meet threshold
* OpenAPI CBOR bodies reference valid CDDL roots
* examples in generated docs come from passing vectors

### 23.3 Support policy

CBORK allows ecosystems to enforce a simple support rule:

> If your payload is rejected, first run it through CBORK with the correct schema version and root rule.

Bug reports become far more useful because they include:

* schema version
* root rule
* binary payload
* diagnostic notation
* conformance result
* expected behaviour
* minimal failing case if claiming CBORK is wrong

That reduces implementation disputes and makes actual tool bugs easier to fix.

## 24. Product Differentiators

CBORK is differentiated by combining features that are usually separate or missing:

* conformant CDDL parsing
* strict CDDL semantic analysis
* CDDL-conformant regex validation
* embedded grammar validation
* CBOR diagnostic notation
* schema-informed diagnostic notation
* vector-driven documentation
* schema coverage
* comment-preserving include expansion
* Markdown documentation inside CDDL
* OpenAPI integration for binary CBOR bodies
* editor support through Tree-sitter and LSP
* CI-first conformance workflows

The important point is that CBORK is not “a CDDL parser.”

It is a CBOR/CDDL interoperability toolchain.

## 25. Suggested Development Phases

### Phase 1: Core correctness

Deliver:

* conformant parser
* AST and semantic model
* formatter
* include resolver
* expanded self-contained rendering
* plain CBOR diagnostic notation
* basic conformance checker

Success criterion:

> CBORK can parse, format, expand, and validate real CDDL and real CBOR payloads.

### Phase 2: Enforcement

Deliver:

* linter
* strict map/open-map rules
* `/=` and `//=` semantic diagnostics
* embedded regex validation
* embedded grammar validation
* vector runner
* coverage report
* CI output

Success criterion:

> A project can use CBORK as a release gate for CDDL and CBOR conformance.

### Phase 3: Documentation

Deliver:

* Markdown documentation comments
* rendered CDDL docs
* vector-derived examples
* CDDL-informed diagnostic notation
* OpenAPI documentation integration
* focused root views
* coverage display in docs

Success criterion:

> Binary CBOR API documentation becomes self-contained, readable, and generated from validated artifacts.

### Phase 4: Editor support

Deliver:

* Tree-sitter grammar
* highlight queries
* outline queries
* indentation queries
* Markdown comment injection
* Zed extension
* LSP integration
* editor commands for expansion, validation, and diagnostic notation

Success criterion:

> CDDL becomes pleasant and safe to write in a modern editor.

### Phase 5: Ecosystem hardening

Deliver:

* compatibility profiles
* richer lint policies
* registry support for known extension keys
* richer coverage analysis
* conformance badge/report generation
* regression vector management
* multi-version API support reports

Success criterion:

> CBORK becomes the reference tool for maintaining a serious CBOR/CDDL ecosystem.

## 26. Non-Goals

CBORK should not:

* treat JSON as the authoritative representation of CBOR payloads
* use host regex behaviour where CDDL requires different regex semantics
* encourage drifting schemas with in-place deprecation
* silently accept unknown keys in closed maps
* make Tree-sitter the authoritative parser
* maintain separate editor, CLI, and CI interpretations
* generate hand-written examples detached from vectors
* hide unresolved CDDL fragments in rendered documentation

## 27. Product Positioning

CBORK should be positioned as:

> A first-class conformance and documentation toolchain for CBOR/CDDL protocols.

Alternate shorter positioning:

> Make CDDL enforceable.

Or:

> Stop treating binary API contracts as prose.

The strongest message is not that CBORK highlights CDDL, formats files, or dumps CBOR.
Those are features.

The real message is:

> CBORK gives CBOR-based ecosystems one authoritative way to define, validate, document, test,
> and enforce their binary payload contracts.

## 28. Definition of Done

CBORK is successful when a project can do all of the following:

1. Write its payload contracts entirely in CDDL.
2. Keep the CDDL modular during authoring.
3. Render self-contained CDDL for documentation.
4. Preserve comments and include provenance.
5. Use Markdown comments as first-class documentation.
6. Validate real CBOR payloads against selected root rules.
7. Dump arbitrary CBOR as diagnostic notation.
8. Dump schema-valid CBOR as annotated CDDL-informed diagnostic notation.
9. Maintain valid and invalid conformance vectors.
10. Measure schema coverage from those vectors.
11. Gate CI on parsing, formatting, linting, conformance, and coverage.
12. Show CBOR/CDDL bodies properly in OpenAPI documentation.
13. Edit CDDL with syntax highlighting, outline, diagnostics, formatting, and navigation.
14. Tell external implementers to run the conformance tool before opening interoperability bugs.

At that point, CDDL stops being a loose schema sketch and becomes an enforceable binary API contract.
