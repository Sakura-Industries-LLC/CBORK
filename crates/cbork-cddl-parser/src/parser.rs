// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A parser for CDDL using the [pest](https://github.com/pest-parser/pest).
//! Utilized for parsing in accordance with RFC-8610, RFC-9165, RFC-9741, RFC-9090.

use pest::{Parser, iterators::Pair};

/// Unified CDDL parser.
#[allow(missing_docs)]
pub mod cddl {
    /// A Pest parser for CDDL.
    #[derive(pest_derive::Parser)]
    #[grammar = "grammar/cddl.pest"]
    pub(crate) struct Parser;
}

/// CDDL Standard Postlude — the built-in type definitions required by the
/// CDDL spec, read from an external file at compile time.
const POSTLUDE: &str = include_str!("grammar/postlude.cddl");

/// Parses a CDDL input string.
///
/// # Arguments
///
/// * `input` - A string containing the CDDL input to be parsed.
///
/// # Returns
///
/// Returns the parsed pairs if successful, otherwise returns an `Err`
/// containing a boxed error indicating the parsing error.
///
/// # Errors
///
/// This function may return an error in the following cases:
///
/// - If there is an issue with parsing the CDDL input.
pub fn parse_cddl(input: &str) -> anyhow::Result<Vec<Pair<'_, cddl::Rule>>> {
    cddl::Parser::parse(cddl::Rule::cddl, input)
        .map(Iterator::collect)
        .map_err(Into::into)
}

/// Parse the built-in standard postlude as a standalone raw CDDL tree.
///
/// This keeps the postlude separate from user input so the compiler can
/// decide when to merge or suppress it.
///
/// # Errors
///
/// Returns an error if the postlude file itself fails to parse (should
/// never happen with a correct postlude).
pub fn parse_postlude() -> anyhow::Result<Vec<Pair<'static, cddl::Rule>>> {
    cddl::Parser::parse(cddl::Rule::cddl, POSTLUDE)
        .map(Iterator::collect)
        .map_err(Into::into)
}
