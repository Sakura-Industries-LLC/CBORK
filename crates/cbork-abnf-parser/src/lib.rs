// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Parser and owned document model for RFC 5234 ABNF.

// cspell: words Naur

#[doc(hidden)]
pub mod abnf {
    //! Pest grammar entrypoints for strict RFC 5234 ABNF parsing.

    pub use pest::Parser;

    #[doc(hidden)]
    /// Pest parser for RFC 5234 ABNF.
    #[derive(pest_derive::Parser)]
    #[grammar = "grammar/rfc_5234.pest"]
    pub struct ABNFParser;
}

#[doc(hidden)]
pub mod abnf_test {
    //! Pest grammar entrypoints used by the parser tests.

    pub use pest::Parser;

    #[doc(hidden)]
    /// Pest parser for RFC 5234 ABNF plus test-only helper rules.
    #[derive(pest_derive::Parser)]
    #[grammar = "grammar/rfc_5234.pest"]
    #[grammar = "grammar/abnf_test.pest"]
    pub struct ABNFTestParser;
}

/// Owned AST data structures for parsed ABNF.
mod ast;
#[doc(hidden)]
mod parser;

pub use ast::{
    AbnfDocument, AbnfElement, AbnfError, AbnfMatch, AbnfOption, AbnfRule, AbnfValidationError,
    Alternation, CharVal, Concatenation, DefinitionOperator, GroupedAlternation, NumBase, NumRange,
    NumVal, NumValue, ProseVal, Repeat, RepeatRange, Repetition, Rulename, SourceSpan,
};

/// Parse strict RFC 5234 ABNF and return an owned document model.
///
/// The returned document preserves the original source text and a typed AST.
///
/// # Errors
///
/// Returns an error if the input is not valid ABNF or if the parser cannot
/// convert the Pest parse tree into the owned document model.
pub fn parse_abnf(input: &str) -> Result<AbnfDocument, AbnfError> {
    parser::parse_abnf(input)
}
