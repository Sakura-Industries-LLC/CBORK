; Copyright (c) 2026 Sakura Industries LLC.
;
; SPDX-License-Identifier: GPL-3.0-or-later

; ── Comments & Directives ──────────────────────
(comment) @comment
(doc_comment) @comment.doc
(standard_directive) @preproc
(custom_directive) @keyword

; ── Literals ────────────────────────────────────
(number) @number
(string) @string
(byte_string) @string.special
(simple_value) @constant.builtin
(keyword) @type.builtin

; ── Rule Names (left-hand side of =) ────────────
(rule
  name: (rule_name
    (identifier) @constructor))
(rule
  name: (rule_name
    (dotted_identifier
      (identifier) @constructor)))
(rule
  name: (rule_name
    (dotted_identifier
      ("." @punctuation.delimiter))))

; ── Generic Parameters ──────────────────────────
(generic_params
  (identifier) @type.parameter)

; ── Member Keys in Map Groups ───────────────────
(map_entry
  key: (member_key
    (identifier) @property))
(map_entry
  key: (member_key
    (dotted_identifier
      (identifier) @property)))
(map_entry
  key: (member_key
    (dotted_identifier
      ("." @punctuation.delimiter))))

; ── Parameterized References ────────────────────
(parameterized_reference
  (reference
    (identifier) @variable))
(parameterized_reference
  (reference
    (dotted_identifier
      (identifier) @variable)))
(parameterized_reference
  (reference
    (dotted_identifier
      ("." @punctuation.delimiter))))
(generic_args
  (identifier) @type.parameter)

; ── References (identifiers used as values) ─────
(reference
  (identifier) @variable)
(reference
  (dotted_identifier
    (identifier) @variable))
(reference
  (dotted_identifier
    ("." @punctuation.delimiter)))

; ── ctlops in ctlop_expression ──────────────────
(ctlop_expression
  (ctlop) @operator)

; ── Tag Expressions ─────────────────────────────
(tag_expression
  ("#" @operator))

; ── Operators ───────────────────────────────────
(rule
  ("=" @operator))
(choice_expression
  ("/" @operator))

; ── Delimiters ──────────────────────────────────
(map_entry
  (":" @punctuation.delimiter))
(array_group
  ("," @punctuation.delimiter))
(map_group
  ("," @punctuation.delimiter))

; ── Brackets ────────────────────────────────────
("[" @punctuation.bracket)
("]" @punctuation.bracket)
("{" @punctuation.bracket)
("}" @punctuation.bracket)
("(" @punctuation.bracket)
(")" @punctuation.bracket)
("<" @punctuation.bracket)
(">" @punctuation.bracket)
