// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

use std::{fmt, str};

use thiserror::Error;

use super::byte::{
    ByteLiteralBytes, ByteLiteralError, decode_b32, decode_b45, decode_b64c, decode_b64c_sloppy,
    decode_b64u, decode_b64u_sloppy, decode_h32, decode_hex, decode_hex_lc, decode_hex_uc,
};

/// Error returned by [`TextLiteralBytes::parse`].
#[derive(Debug, Error)]
pub enum TextLiteralError {
    /// The input is not valid JSON.
    #[error("text is not a valid JSON string")]
    InvalidJsonString(#[from] serde_json::Error),
    /// The input contains an invalid CDDL string escape.
    #[error("text contains an invalid CDDL string escape: {0}")]
    InvalidCddlEscape(String),
    /// The unescaped bytes are not valid UTF-8.
    #[error("text is not UTF8")]
    InvalidUTF8,
    /// The text is not a valid base-10 integer literal.
    #[error("text is not a valid base10 integer")]
    InvalidBase10(String),
}

/// Parsed CDDL text literal.
///
/// The inner bytes are the unescaped content of a JSON string (the
/// characters between the `"` delimiters after escape resolution).
/// Valid UTF-8 is guaranteed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLiteralBytes(Vec<u8>);

#[allow(
    clippy::missing_errors_doc,
    clippy::double_must_use,
    clippy::must_use_candidate
)]
impl TextLiteralBytes {
    /// Build a text literal from already-normalized bytes.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Consume the text literal and return its raw bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Parse a JSON text literal.
    ///
    /// The input must be a valid JSON string: surrounded by `"`, with
    /// JSON escape sequences resolved, and the result must be valid
    /// UTF-8.
    pub fn parse(text: &[u8]) -> Result<Self, TextLiteralError> {
        let normalized = normalize_cddl_string_to_json(text)?;
        let s: String = serde_json::from_slice(&normalized)?;
        Ok(Self::from_bytes(s.into_bytes()))
    }

    /// Concatenate two text literals.
    #[must_use]
    pub fn cat(
        &self,
        rhs: &TextLiteralBytes,
    ) -> TextLiteralBytes {
        #[allow(clippy::arithmetic_side_effects, reason = "Safe due to bound ranges")]
        let mut out = Vec::with_capacity(self.0.len() + rhs.0.len());
        out.extend_from_slice(&self.0);
        out.extend_from_slice(&rhs.0);
        TextLiteralBytes::from_bytes(out)
    }

    /// Dedent and concatenate two text literals.
    #[must_use]
    pub fn det(
        &self,
        rhs: &TextLiteralBytes,
    ) -> TextLiteralBytes {
        let lhs = TextLiteralBytes::from_bytes(dedent(&self.0));
        let rhs = TextLiteralBytes::from_bytes(dedent(&rhs.0));
        lhs.cat(&rhs)
    }

    /// Check whether this text literal is valid JSON text.
    pub fn validate_json(&self) -> Result<(), TextLiteralError> {
        let _json: serde_json::Value = serde_json::from_slice(&self.0)?;
        Ok(())
    }

    /// Convert a decimal integer into a text literal.
    #[must_use]
    pub fn from_base10(value: i128) -> Self {
        Self::from_bytes(value.to_string().into_bytes())
    }

    /// Parse a decimal integer from this text literal.
    pub fn to_base10(&self) -> Result<i128, TextLiteralError> {
        let s = str::from_utf8(&self.0).map_err(|_| TextLiteralError::InvalidUTF8)?;
        let is_valid = s == "0"
            || s.strip_prefix('-').is_some_and(|rest| {
                rest != "0" && !rest.starts_with('0') && rest.chars().all(|c| c.is_ascii_digit())
            })
            || (!s.starts_with('-')
                && !s.starts_with('0')
                && s.chars().all(|c| c.is_ascii_digit()));
        if !is_valid {
            return Err(TextLiteralError::InvalidBase10(s.to_owned()));
        }
        s.parse::<i128>()
            .map_err(|_| TextLiteralError::InvalidBase10(s.to_owned()))
    }

    /// Decode a base64url text literal into bytes.
    pub fn from_b64u(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b64u(self.as_ref())?))
    }

    /// Decode a sloppy base64url text literal into bytes.
    pub fn from_b64u_sloppy(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b64u_sloppy(
            self.as_ref(),
        )?))
    }

    /// Decode a standard base64 text literal into bytes.
    #[must_use]
    pub fn from_b64c(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b64c(self.as_ref())?))
    }

    /// Decode a sloppy standard base64 text literal into bytes.
    #[must_use]
    pub fn from_b64c_sloppy(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b64c_sloppy(
            self.as_ref(),
        )?))
    }

    /// Decode a hex text literal into bytes.
    #[must_use]
    pub fn from_hex(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_hex(self.as_ref())?))
    }

    /// Decode a lowercase hex text literal into bytes.
    #[must_use]
    pub fn from_hexlc(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_hex_lc(self.as_ref())?))
    }

    /// Decode an uppercase hex text literal into bytes.
    #[must_use]
    pub fn from_hexuc(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_hex_uc(self.as_ref())?))
    }

    /// Decode a base32 text literal into bytes.
    #[must_use]
    pub fn from_b32(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b32(self.as_ref())?))
    }

    /// Decode a base32hex text literal into bytes.
    #[must_use]
    pub fn from_h32(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_h32(self.as_ref())?))
    }

    /// Decode a base45 text literal into bytes.
    #[must_use]
    pub fn from_b45(&self) -> Result<ByteLiteralBytes, ByteLiteralError> {
        Ok(ByteLiteralBytes::from_bytes(decode_b45(self.as_ref())?))
    }
}

impl AsRef<[u8]> for TextLiteralBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for TextLiteralBytes {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl TryFrom<ByteLiteralBytes> for TextLiteralBytes {
    type Error = TextLiteralError;

    fn try_from(value: ByteLiteralBytes) -> Result<Self, Self::Error> {
        if str::from_utf8(value.as_ref()).is_ok() {
            return Ok(Self::from_bytes(value.into_bytes()));
        }
        Err(TextLiteralError::InvalidUTF8)
    }
}

/// Normalize a CDDL string literal into a JSON string literal.
fn normalize_cddl_string_to_json(text: &[u8]) -> Result<Vec<u8>, TextLiteralError> {
    if !text.contains(&b'{') {
        return Ok(text.to_vec());
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "All indexing bounded by explicit length checks"
    )]
    {
        if text.len() < 2 || text[0] != b'"' || text[text.len() - 1] != b'"' {
            return Ok(text.to_vec());
        }

        let mut out = Vec::with_capacity(text.len());
        out.push(b'"');

        let mut pos = 1usize;
        let end = text.len() - 1;
        let mut last_copy = pos;

        while pos < end {
            if text[pos] == b'\\' && pos + 3 < end && text[pos + 1] == b'u' && text[pos + 2] == b'{'
            {
                let mut close = pos + 3;
                while close < end && text[close] != b'}' {
                    close += 1;
                }
                if close >= end {
                    return Err(TextLiteralError::InvalidCddlEscape(
                        "unterminated `\\u{...}` escape".to_owned(),
                    ));
                }

                let digits = &text[(pos + 3)..close];
                let scalar = parse_braced_scalar(digits)?;

                out.extend_from_slice(&text[last_copy..pos]);
                push_json_unicode_escape(&mut out, scalar);

                pos = close + 1;
                last_copy = pos;
                continue;
            }

            pos += 1;
        }

        out.extend_from_slice(&text[last_copy..end]);
        out.push(b'"');
        Ok(out)
    }
}

/// Parses a hex digit sequence from inside `\u{}` braces into a unicode scalar.
fn parse_braced_scalar(digits: &[u8]) -> Result<u32, TextLiteralError> {
    if digits.is_empty() {
        return Err(TextLiteralError::InvalidCddlEscape(
            "empty `\\u{}` escape".to_owned(),
        ));
    }

    if !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(TextLiteralError::InvalidCddlEscape(
            "non-hex character in `\\u{...}` escape".to_owned(),
        ));
    }

    let digits = std::str::from_utf8(digits).map_err(|_| {
        TextLiteralError::InvalidCddlEscape("non-UTF8 `\\u{...}` escape".to_owned())
    })?;
    let scalar = u32::from_str_radix(digits, 16).map_err(|_| {
        TextLiteralError::InvalidCddlEscape("invalid `\\u{...}` escape value".to_owned())
    })?;

    if scalar > 0x10_FFFF || (0xD800..=0xDFFF).contains(&scalar) {
        return Err(TextLiteralError::InvalidCddlEscape(
            "unicode scalar out of range".to_owned(),
        ));
    }

    Ok(scalar)
}

/// Encodes a unicode scalar as a JSON `\uXXXX` or `\uXXXX\uXXXX` surrogate-pair escape.
fn push_json_unicode_escape(
    out: &mut Vec<u8>,
    scalar: u32,
) {
    if scalar <= 0xFFFF {
        push_json_u16_escape(out, u16::try_from(scalar).unwrap_or(0));
        return;
    }

    let code = scalar.wrapping_sub(0x1_0000);
    let high = 0xD800_u16.wrapping_add(u16::try_from(code >> 10).unwrap_or(0));
    let low = 0xDC00_u16.wrapping_add(u16::try_from(code & 0x3FF).unwrap_or(0));
    push_json_u16_escape(out, high);
    push_json_u16_escape(out, low);
}

/// Writes a single `\uXXXX` escape sequence for a 16-bit value.
fn push_json_u16_escape(
    out: &mut Vec<u8>,
    value: u16,
) {
    use std::fmt::Write as _;

    out.extend_from_slice(br"\u");
    let mut buf = String::with_capacity(4);
    let _ = write!(&mut buf, "{value:04X}");
    out.extend_from_slice(buf.as_bytes());
}
/// Dedent a byte string according to the CDDL dedenting algorithm.
fn dedent(input: &[u8]) -> Vec<u8> {
    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "All indexing bounded by input length"
    )]
    {
        let min_indent = compute_min_indent(input);

        let mut out = Vec::with_capacity(input.len());
        let mut i: usize = 0;
        let bytes = input;

        while i < bytes.len() {
            let line = line_from(bytes, i);
            let is_blank = line_is_blank(line);

            if is_blank {
                let stripped = count_leading_spaces(line);
                out.extend_from_slice(&line[stripped..]);
            } else {
                out.extend_from_slice(&line[min_indent..]);
            }

            i += line.len();
        }

        out
    }
}

/// Compute the minimum leading-space indent across all non-blank lines.
fn compute_min_indent(input: &[u8]) -> usize {
    let mut min: usize = usize::MAX;
    let mut i: usize = 0;

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "All indexing bounded by input length"
    )]
    while i < input.len() {
        let line = line_from(input, i);
        if !line_is_blank(line) {
            let indent = count_leading_spaces(line);
            if indent < min {
                min = indent;
            }
        }
        i += line.len();
    }

    if min == usize::MAX { 0 } else { min }
}

/// Extract one line (up to and including `\n`) from `pos`.
fn line_from(
    input: &[u8],
    pos: usize,
) -> &[u8] {
    #[allow(clippy::indexing_slicing, reason = "Positions bounded by input length")]
    {
        let remaining = &input[pos..];
        if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
            &remaining[..=nl]
        } else {
            remaining
        }
    }
}

/// Check whether a line is blank (empty or only spaces).
fn line_is_blank(line: &[u8]) -> bool {
    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "Line length bounded by input"
    )]
    {
        let content = if line.ends_with(b"\n") {
            &line[..line.len() - 1]
        } else {
            line
        };
        content.iter().all(|&b| b == b' ')
    }
}

/// Count leading space characters in a line.
fn count_leading_spaces(line: &[u8]) -> usize {
    line.iter().take_while(|&&b| b == b' ').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_text_literal() {
        let value = TextLiteralBytes::parse(br#""hello""#).unwrap();
        assert_eq!(value.as_ref(), b"hello");
    }

    #[test]
    fn parse_json_text_literal_with_escape() {
        let value = TextLiteralBytes::parse(br#""a\nb""#).unwrap();
        assert_eq!(value.as_ref(), b"a\nb");
    }

    #[test]
    fn parse_cddl_text_literal_with_braced_unicode_escape() {
        let value = TextLiteralBytes::parse(br#""\u{41}\u{1F073}""#).unwrap();
        assert_eq!(std::str::from_utf8(value.as_ref()).unwrap(), "A🁳");
    }

    #[test]
    fn reject_invalid_braced_unicode_escape() {
        assert!(TextLiteralBytes::parse(br#""\u{}""#).is_err());
        assert!(TextLiteralBytes::parse(br#""\u{110000}""#).is_err());
    }

    #[test]
    fn cat_concatenates_text() {
        let lhs = TextLiteralBytes::parse(br#""foo""#).unwrap();
        let rhs = TextLiteralBytes::parse(br#""bar""#).unwrap();
        assert_eq!(lhs.cat(&rhs).as_ref(), b"foobar");
    }

    #[test]
    fn det_dedents_before_concatenation() {
        let lhs = TextLiteralBytes::parse(br#""  foo\n  bar""#).unwrap();
        let rhs = TextLiteralBytes::parse(br#""  baz""#).unwrap();
        assert_eq!(lhs.det(&rhs).as_ref(), b"foo\nbarbaz");
    }

    #[test]
    fn base10_roundtrip() {
        let text = TextLiteralBytes::from_base10(12345);
        assert_eq!(text.as_ref(), b"12345");
        assert_eq!(text.to_base10().unwrap(), 12345);
    }

    #[test]
    fn validate_json_accepts_json() {
        let value = TextLiteralBytes::from_bytes(br#"{"iss":"text"}"#.to_vec());
        assert!(value.validate_json().is_ok());
    }

    #[test]
    fn validate_json_rejects_invalid_json() {
        let value = TextLiteralBytes::from_bytes(b"not json".to_vec());
        assert!(value.validate_json().is_err());
    }
}
