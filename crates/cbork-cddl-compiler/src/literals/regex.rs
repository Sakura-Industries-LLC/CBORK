// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

use std::{fmt, str, sync::Arc};

use regexml::Regex;
use thiserror::Error;

/// Error returned when compiling or validating a regular expression literal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegexLiteralError {
    /// The input is not valid UTF-8.
    #[error("regex pattern is not UTF-8")]
    InvalidUTF8,
    /// The pattern is not a valid XSD 1.1 regular expression.
    #[error("regex pattern is invalid: {0}")]
    InvalidPattern(String),
}

/// Error returned when validating input against a compiled regular expression.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegexValidationError {
    /// The input does not match the pattern.
    #[error("input does not match the regular expression")]
    Mismatch,
    /// The input is not valid UTF-8.
    #[error("input is not UTF-8")]
    InvalidUTF8,
}

/// Parsed CDDL `regexp` literal.
///
/// The original source text is preserved and compiled to an XSD 1.1 regular
/// expression so it can be reused for later validation.
#[derive(Debug, Clone)]
pub struct RegexLiteral {
    /// The original source pattern.
    source: String,
    /// The compiled XSD 1.1 regular expression.
    compiled: Arc<Regex>,
}

impl PartialEq for RegexLiteral {
    fn eq(
        &self,
        other: &Self,
    ) -> bool {
        self.source == other.source
    }
}

impl Eq for RegexLiteral {}

impl RegexLiteral {
    /// Parse and compile a regular-expression literal from raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the pattern is not valid UTF-8 or if the regex
    /// compiler rejects it.
    pub fn parse(pattern: impl AsRef<[u8]>) -> Result<Self, RegexLiteralError> {
        let source = str::from_utf8(pattern.as_ref())
            .map_err(|_| RegexLiteralError::InvalidUTF8)?
            .to_owned();
        let compiled = Regex::xsd(&source, "")
            .map_err(|e| RegexLiteralError::InvalidPattern(format!("{e:?}")))?;

        Ok(Self {
            source,
            compiled: Arc::new(compiled),
        })
    }

    /// Return the original source text for this regex.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Return the compiled regex.
    #[must_use]
    pub fn compiled(&self) -> &Regex {
        self.compiled.as_ref()
    }

    /// Validate a text string against the regex.
    ///
    /// # Errors
    ///
    /// Returns [`RegexValidationError::Mismatch`] if the input does not
    /// match.
    pub fn validate_text(
        &self,
        input: impl AsRef<str>,
    ) -> Result<(), RegexValidationError> {
        if self.compiled.is_match(input.as_ref()) {
            Ok(())
        } else {
            Err(RegexValidationError::Mismatch)
        }
    }

    /// Validate a byte string against the regex.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not UTF-8 or does not match.
    pub fn validate_bytes(
        &self,
        input: impl AsRef<[u8]>,
    ) -> Result<(), RegexValidationError> {
        let text = str::from_utf8(input.as_ref()).map_err(|_| RegexValidationError::InvalidUTF8)?;
        self.validate_text(text)
    }
}

impl fmt::Display for RegexLiteral {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::{RegexLiteral, RegexValidationError};

    #[test]
    fn parse_and_validate_text() {
        let regex = RegexLiteral::parse(b"[a-z]+").unwrap();
        assert_eq!(regex.source(), "[a-z]+");
        assert!(regex.validate_text("abc").is_ok());
        assert!(matches!(
            regex.validate_text("ABC"),
            Err(RegexValidationError::Mismatch)
        ));
    }

    #[test]
    fn validate_bytes_promotes_utf8() {
        let regex = RegexLiteral::parse(b"[a-z]+").unwrap();
        assert!(regex.validate_bytes(b"abc").is_ok());
        assert!(matches!(
            regex.validate_bytes(b"\xff"),
            Err(RegexValidationError::InvalidUTF8)
        ));
    }
}
