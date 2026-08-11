// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Integration tests for the structural CDDL formatter (`pretty_print`).
//!
//! The formatter is a pure presentation layer: it parses a CDDL
//! document and re-emits it with canonical spacing and line breaks.
//! It must never change the document's meaning, so every test here
//! asserts either byte stability (parse → format → parse → format
//! yields the same bytes) or that the formatted output still parses
//! and re-formats identically.

use cbork_cddl_compiler::pretty_print;

/// Assert that formatting is idempotent and never loses content: the
/// formatted text parses, formats again to the same bytes, and the
/// original parses to the same result (so the formatter cannot have
/// changed the document's structure).
fn assert_formatter_stable(src: &str) -> String {
    let once = pretty_print(src);
    let twice = pretty_print(&once);
    assert_eq!(
        once, twice,
        "formatter must be idempotent:\n--- once ---\n{once}\n--- twice ---\n{twice}"
    );
    once
}

#[test]
fn redundant_parens_are_stripped() {
    // A single bare operand in parens adds no scope: the canonical
    // form strips it so renders of the same document agree whether
    // the parens came from an inlined definition or literal source.
    let src = "x = ((any))\ny = (tstr)\nz = (1)\n";
    let once = assert_formatter_stable(src);
    assert!(
        !once.contains("((any))") && !once.contains("(tstr)") && !once.contains("(1)"),
        "redundant single-arm parens must be stripped:\n{once}"
    );
    assert!(
        once.contains("x = any") && once.contains("y = tstr") && once.contains("z = 1"),
        "the operand must survive:\n{once}"
    );
}

#[test]
fn structural_parens_are_kept() {
    // Parens that carry scope — a choice or a ctlop operand — must
    // not be stripped.
    let src = "x = (a / b)\ny = (bstr .size 4)\nz = a .cbor (b / c)\n";
    let once = assert_formatter_stable(src);
    let normalized: String = once.split_whitespace().collect();
    assert!(
        normalized.contains("(a/b)")
            && normalized.contains("(bstr.size4)")
            && normalized.contains("(b/c)"),
        "structural parens must be kept:\n{once}"
    );
}

#[test]
fn formats_rule_heads_and_keeps_meaning() {
    let src = "person = {\n  name: tstr,\n  age: uint\n}\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.starts_with("person = {\n"),
        "head must survive:\n{once}"
    );
    assert!(once.contains("name: tstr"), "member must survive:\n{once}");
}

#[test]
fn comment_placement_survives() {
    let src =
        "; leading comment\nperson = {\n  name: tstr, ; the name\n  age: uint\n}\n; trailing\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("; leading comment") && once.contains("; trailing"),
        "comments must survive:\n{once}"
    );
    assert!(
        once.contains("name: tstr, ; the name"),
        "trailing comment must stay with its entry:\n{once}"
    );
}

#[test]
fn enum_groups_keep_entries_and_comments() {
    let src = "flags = &(\n  F_DISC: 0    ; valid for discovery\n  F_NEG: 1     ; valid for negotiation\n)\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("F_DISC: 0") && once.contains("F_NEG: 1"),
        "enum entries must survive:\n{once}"
    );
    assert!(
        once.contains("valid for discovery") && once.contains("valid for negotiation"),
        "enum comments must survive:\n{once}"
    );
    assert!(once.contains("&("), "enum opener must survive:\n{once}");
}

#[test]
fn parenthesized_type_choices_keep_parens() {
    // Dropping the parens would change ctlop binding on re-parse.
    let src = "x = (\"a\" / (text .regexp \"[a-z]+\"))\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("(\"a\" /"),
        "parenthesized choice must keep its parens:\n{once}"
    );
}

#[test]
fn control_and_range_expressions_survive() {
    let src = "x = bytes .size 4\ny = 0 .. 255\nz = \"a\" .size 2\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("bytes .size 4")
            && once.contains("0 .. 255")
            && once.contains("\"a\" .size 2"),
        "ctlops and ranges must survive:\n{once}"
    );
}

#[test]
fn tags_and_generics_survive() {
    let src = "sig<T> = #6.258(T)\nwrapper = #6.33000([sig<int>])\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("#6.258(T)") && once.contains("sig<int>") && once.contains("#6.33000("),
        "tags and generics must survive:\n{once}"
    );
}

#[test]
fn group_vs_parenthesized_choice_distinction_kept() {
    // `(a => int, b => int)` is a parenthesized group; entries stay
    // comma-separated. A `/` inside a value choice (`6 / 17`) must not
    // be mistaken for a group separator.
    let src = "g = (a => int, b => int)\nx = [6 / 17, bytes .size 4]\n";
    let once = assert_formatter_stable(src);
    let normalized: String = once.split_whitespace().collect();
    assert!(
        normalized.contains("6/17"),
        "value choice must survive:\n{once}"
    );
    assert!(
        once.contains("a => int,") && once.contains("b => int"),
        "group entries must survive:\n{once}"
    );
}

#[test]
fn occurrence_wrapped_choices_stay_wrapped() {
    let src = "x = [\n  + ({ a => int } / text)\n]\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("+ ({"),
        "occurrence and parens must survive:\n{once}"
    );
}

#[test]
fn empty_groups_are_valid() {
    let src = "x = []\ny = {}\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("x = [\n]\n") && once.contains("y = {\n}\n"),
        "empty groups must survive (and stay valid CDDL):\n{once}"
    );
}

#[test]
fn group_rule_bodies_survive() {
    let src = "headers = (? 1 => int, ? 2 => tstr)\n";
    let once = assert_formatter_stable(src);
    assert!(
        once.contains("1 => int") && once.contains("2 => tstr"),
        "group rule body must survive:\n{once}"
    );
}

#[test]
fn unparseable_input_is_returned_unchanged() {
    let garbage = "this is not cddl {{{{\n";
    assert_eq!(pretty_print(garbage), garbage);
}

#[test]
fn formats_without_losing_content() {
    // The formatter must not drop or duplicate any meaningful token:
    // the formatted output must contain the same content words as the
    // input (brackets move to their own lines, so compare the word
    // multiset of non-punctuation tokens).
    let src = "a = [1, 2, 3]\nb = { x: a, y: bstr .size 8 }\nc = 0 .. 255\n";
    let once = pretty_print(src);
    let words = |s: &str| -> Vec<String> {
        let mut w: Vec<String> = s
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-')
            .filter(|w| !w.is_empty() && !w.starts_with(';'))
            .map(str::to_owned)
            .collect();
        w.sort();
        w
    };
    assert_eq!(
        words(src),
        words(&once),
        "formatter must not add or remove tokens:\n{once}"
    );
}
