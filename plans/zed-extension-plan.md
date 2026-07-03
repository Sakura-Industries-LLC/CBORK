# Zed Extension Plan

This extension provides editor support for CDDL.
The initial goal is reliable syntax highlighting via a Tree-sitter grammar.
The longer-term goal is to make Zed a useful front-end for CBORK's lint, decode, validate, and documentation workflows.

## Current Scope

### 1. Syntax highlighting

The first feature is a Tree-sitter grammar for CDDL.
It should recognize:

* rules and rule names
* groups and group entries
* numbers, text strings, byte strings, booleans, and simple values
* control operators
* built-in types
* comments and directives

This is the foundation for all other language features.

### 2. Bracket matching and rainbow grouping

The grammar should expose brackets, braces, parentheses, and angle brackets cleanly enough for Zed's bracket matching.
This makes nested CDDL much easier to read.

### 3. Outline and navigation

The extension should eventually provide an outline of top-level rules and imported symbols.
That would let the editor show the document structure in a sidebar or navigation panel.

### 4. Snippet and template support

Common CDDL forms should be available as snippets.
Useful examples include:

* top-level rule skeletons
* include/import directives
* array and map groups
* control operator templates
* common literal forms

### 5. Comment helpers

CDDL uses semicolon comments and documentation comments.
The extension should support toggling comments and preserving doc-comment formatting.
It should also distinguish:

* standard `;#` include/import directives
* doc comments `;!`
* CBORK custom directives `;@`
* ordinary `;` comments

### 6. Formatting integration

The extension should eventually expose CBORK formatting as an editor action.
That can support:

* format document
* format selection
* format-on-save

### 7. Lint and diagnostics integration

The best editor workflow is to surface CBORK lint diagnostics directly in Zed.
That would allow:

* inline errors and warnings
* code actions for fixable diagnostics
* hover help for warning/error codes
* quick navigation to the first failing path

### 8. Validation and decode helpers

CBORK can already parse and validate CBOR.
The extension can grow commands for:

* decoding raw CBOR to EDN-style output
* validating CBOR against a schema
* pretty-printing validation traces

### 9. Standards references and rationale

The extension should eventually expose CBORK's embedded standards corpus.
Useful editor actions include:

* hover over a ctlop and show the spec excerpt
* hover over a diagnostic code and show the rationale
* jump to the relevant RFC excerpt

### 10. Semantic tokens and richer editor help

If CBORK later exposes a language server or semantic token source, Zed can use that to improve:

* named entity highlighting
* unresolved-reference highlighting
* symbol completion
* rename and navigation support

## Future Features

These are not required for the first useful version, but they fit the same extension architecture:

* integrated CBORK task buttons in the command palette
* snippets for common RFC-defined patterns
* tree-sitter injections for embedded ABNF and regex payloads
* per-rule documentation hover cards
* cross-reference lookup for standards symbols and ctlops
* file templates for new CDDL modules

## Implementation Order

1. Tree-sitter grammar and highlight queries.
2. Bracket matching and outline queries.
3. Snippets and editor polish.
4. Diagnostics and command integration.
5. Standards hover and cross-reference features.
6. Full CBORK task integration.
