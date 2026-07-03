// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Character Set Tests
// cspell: words PCHAR pchar BCHAR bchar SESC sesc SCHAR schar fffd fffe

mod common;
use common::Rule;

/// Test if the `WHITESPACE` rule passes properly.
#[test]
fn check_whitespace() {
    common::check_tests_rule(Rule::WHITESPACE, &[" ", "\t", "\r", "\n", "\r\n"], &["not"]);
}

/// Test if the `PCHAR` rule passes properly.
#[test]
fn check_pchar() {
    common::check_tests_rule(
        Rule::PCHAR,
        &[" ", "~", "\u{a0}", "\u{d7ff}", "\u{e000}", "\u{10fffd}"],
        &["\t", "\r", "\n", "\u{7f}", "\u{9f}"],
    );
}

/// Test if the `BCHAR` rule passes properly.
#[test]
fn check_bchar() {
    common::check_tests_rule(
        Rule::BCHAR,
        &[" ", "&", "(", "~", "\n", "\r", "\u{a0}", "\\'", "\\u0041"],
        &["\t", "'", "\\", "\u{7f}", "\u{9f}"],
    );
}

/// Test if the `SESC` rule passes properly.
#[test]
fn check_sesc() {
    common::check_tests_rule(
        Rule::SESC,
        &[
            "\\\"",
            "\\/",
            "\\\\",
            "\\b",
            "\\f",
            "\\n",
            "\\r",
            "\\t",
            "\\u0041",
            "\\u{41}",
            "\\uD83C\\uDC73",
        ],
        &["\\'", "\\x41", "\\uD800", "\\u{110000}", "\\u{}"],
    );
}

/// Test if the `ASCII_VISIBLE` rule passes properly.
#[test]
fn check_ascii_visible() {
    let passes = (' '..='~').map(String::from).collect::<Vec<_>>();
    common::check_tests_rule(Rule::ASCII_VISIBLE, &passes, &["\r", "\u{80}"]);
}

/// Test if the `SCHAR_ASCII_VISIBLE` rule passes properly.
#[test]
fn check_schar_ascii_visible() {
    let passes = (' '..='~')
        .filter(|c| c != &'"' && c != &'\\')
        .map(String::from)
        .collect::<Vec<_>>();
    common::check_tests_rule(Rule::SCHAR_ASCII_VISIBLE, &passes, &[
        "\"", "\\", "\r", "\u{80}",
    ]);
}

/// Test if the `BCHAR_ASCII_VISIBLE` rule passes properly.
#[test]
fn check_bchar_ascii_visible() {
    let passes = (' '..='~')
        .filter(|c| c != &'\'' && c != &'\\')
        .map(String::from)
        .collect::<Vec<_>>();
    common::check_tests_rule(Rule::BCHAR_ASCII_VISIBLE, &passes, &[
        "'", "\\", "\r", "\u{80}",
    ]);
}

/// Test if the `UNICODE_CHAR` rule passes properly.
#[test]
fn check_unicode() {
    common::check_tests_rule(
        Rule::UNICODE_CHAR,
        &["\u{80}", "\u{10fffd}", "\u{7ffff}"],
        &["\r", "\u{10fffe}"],
    );
}
