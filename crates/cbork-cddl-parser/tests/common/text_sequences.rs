// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

#![allow(dead_code)] // TODO: find a way to remove this.

pub(crate) const S_PASSES: &[&str] = &[" ", "  ", " \t \t", " \t  \r \n \r\n   "];
pub(crate) const S_FAILS: &[&str] = &[" a ", "zz", " \t d \t", " \t  \r \n \t \r\n  x"];
pub(crate) const TEXT_PASSES: &[&str] = &[
    r#""""#,
    r#""abc""#,
    "\"abc\\n\"",
    "\"\\u0041\"",
    "\"\\u{41}\"",
    "\"\\uD83C\\uDC73\"",
];
pub(crate) const TEXT_FAILS: &[&str] =
    &["", "''", "\"abc\n\"", "\"\\'\"", "\"\\x41\"", "\"\\uD800\""];
