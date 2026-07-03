// Copyright (c) 2023 Steven Johnson.
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A parser for CDDL, utilized for parsing in accordance with RFC 8610.

pub mod modules;
mod parser;

/// Raw generated CDDL grammar symbols.
pub use parser::cddl;
pub use parser::{parse_cddl, parse_postlude};

/// Verifies semantically a CDDL input string.
///
/// # Errors
///
/// This function may return an error in the following cases:
///
/// - If there is an issue with parsing the CDDL input.
pub fn validate_cddl(input: &str) -> anyhow::Result<()> {
    let _ast = parser::parse_cddl(input)?;
    Ok(())
}

/// Information about a CDDL syntax error extracted from a parse failure.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxErrorInfo {
    /// Number of syntax errors reported (always 1 for pest parse errors).
    pub error_count: usize,
}

/// Try to extract syntax error information from an error.
///
/// Returns [`Some`] if the error is a CDDL parse (syntax) error.
///
/// Returns [`None`] if the error is not a CDDL syntax error (e.g. I/O
/// error, compiler error, etc.).
pub fn try_extract_syntax_error(
    error: &(dyn std::error::Error + 'static)
) -> Option<SyntaxErrorInfo> {
    if error
        .downcast_ref::<pest::error::Error<cddl::Rule>>()
        .is_some()
    {
        Some(SyntaxErrorInfo { error_count: 1 })
    } else {
        None
    }
}
