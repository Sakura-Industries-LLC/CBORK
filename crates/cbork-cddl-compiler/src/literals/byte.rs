// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

use std::{fmt, str};

use thiserror::Error;

use crate::literals::text::TextLiteralBytes;

/// Error returned by byte-string parsing functions.
#[derive(Debug, Error)]
pub enum ByteLiteralError {
    /// The input could not be parsed as a byte string.
    #[error("String is not a valid byte string.")]
    InvalidByteString(String),
}

/// Parsed CDDL byte-string literal.
///
/// The inner bytes are the raw content between `'` delimiters with
/// `\'` replaced by `'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteLiteralBytes(Vec<u8>);

#[allow(
    clippy::missing_errors_doc,
    clippy::double_must_use,
    clippy::must_use_candidate
)]
impl ByteLiteralBytes {
    /// Create a `ByteLiteralBytes` from raw bytes without validation.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Consume and return the inner bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Parse a CDDL byte-string literal.
    ///
    /// Detects the encoding prefix and delegates to the appropriate
    /// parser:
    ///
    /// * `h'...'` → hex-encoded bytes
    /// * `b64'...'` → URL-safe base64-encoded bytes
    /// * `'...'` → raw byte content (normalised and JSON-validated)
    /// # Errors
    ///
    /// Returns an error if the input does not match any known byte-
    /// string format.
    pub fn parse(text: &[u8]) -> Result<Self, ByteLiteralError> {
        #[allow(
            clippy::indexing_slicing,
            clippy::arithmetic_side_effects,
            reason = "Slicing bounded by prefix/suffix checks"
        )]
        {
            if text.starts_with(b"h'") && text.ends_with(b"'") {
                let bytes = decode_hex(&text[2..(text.len() - 1)])?;
                Ok(ByteLiteralBytes::from_bytes(bytes))
            } else if text.starts_with(b"b64'") && text.ends_with(b"'") {
                let bytes = decode_b64u(&text[4..(text.len() - 1)])?;
                Ok(ByteLiteralBytes::from_bytes(bytes))
            } else if text.starts_with(b"'") && text.ends_with(b"'") {
                let input = Self::normalize(&text[1..(text.len() - 1)]);
                let Ok(s) = TextLiteralBytes::parse(&input) else {
                    return Err(ByteLiteralError::InvalidByteString(
                        "Byte string is invalid JSON encoded.".into(),
                    ));
                };
                Ok(s.into())
            } else {
                Err(ByteLiteralError::InvalidByteString(format!(
                    "unknown byte-string format: {}",
                    String::from_utf8_lossy(text)
                )))
            }
        } // allow block
    }

    /// Normalize raw bytes (content between `'` delimiters) to
    /// JSON-encoded form suitable for parsing as a text literal.
    #[must_use]
    pub fn normalize(input: &[u8]) -> Vec<u8> {
        normalize_bytes_to_text(input)
    }

    /// Concatenate two byte literals.
    #[must_use]
    pub fn cat(
        &self,
        rhs: &ByteLiteralBytes,
    ) -> ByteLiteralBytes {
        #[allow(clippy::arithmetic_side_effects, reason = "Safe due to bound ranges")]
        let mut out = Vec::with_capacity(self.0.len() + rhs.0.len());
        out.extend_from_slice(&self.0);
        out.extend_from_slice(&rhs.0);
        ByteLiteralBytes::from_bytes(out)
    }

    /// Encode these bytes as URL-safe base64 without padding.
    #[must_use]
    pub fn to_b64u(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b64u(self.as_ref()))
    }

    /// Encode these bytes as URL-safe base64 without padding.
    #[must_use]
    pub fn to_b64u_sloppy(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b64u(self.as_ref()))
    }

    /// Encode these bytes as classic base64.
    #[must_use]
    pub fn to_b64c(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b64c(self.as_ref()))
    }

    /// Encode these bytes as classic base64.
    #[must_use]
    pub fn to_b64c_sloppy(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b64c(self.as_ref()))
    }

    /// Encode these bytes as lowercase hex.
    #[must_use]
    pub fn to_hex(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(hex::encode(self.as_ref()).into_bytes())
    }

    /// Encode these bytes as lowercase hex.
    #[must_use]
    pub fn to_hexlc(&self) -> TextLiteralBytes {
        self.to_hex()
    }

    /// Encode these bytes as uppercase hex.
    #[must_use]
    pub fn to_hexuc(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(hex::encode_upper(self.as_ref()).into_bytes())
    }

    /// Encode these bytes as RFC 4648 base32.
    #[must_use]
    pub fn to_b32(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b32(self.as_ref()))
    }

    /// Encode these bytes as RFC 4648 base32hex.
    #[must_use]
    pub fn to_h32(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_h32(self.as_ref()))
    }

    /// Encode these bytes as Base45.
    #[must_use]
    pub fn to_b45(&self) -> TextLiteralBytes {
        TextLiteralBytes::from_bytes(encode_b45(self.as_ref()).into_bytes())
    }
}

impl AsRef<[u8]> for ByteLiteralBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for ByteLiteralBytes {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        // Render as a CDDL byte-string literal (`h'...'`, lowercase hex).
        write!(f, "h'{}'", hex::encode(self.as_ref()))
    }
}

impl From<TextLiteralBytes> for ByteLiteralBytes {
    fn from(value: TextLiteralBytes) -> Self {
        Self::from_bytes(value.into_bytes())
    }
}

/// Decode URL-safe base64-encoded bytes into a byte string.
///
/// # Errors
///
/// Returns an error if the input is not valid base64.
pub fn decode_b64u(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base64url: {e}")))
}

/// Decode URL-safe base64-encoded bytes into a byte string, allowing
/// sloppy trailing bits.
pub fn decode_b64u_sloppy(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    decode_b64u(input)
}

/// Decode classic base64-encoded bytes into a byte string.
pub fn decode_b64c(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base64: {e}")))
}

/// Decode classic base64-encoded bytes into a byte string, allowing
/// sloppy trailing bits.
pub fn decode_b64c_sloppy(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    decode_b64c(input)
}

/// Decode Base16 (hex) bytes into a byte string.
pub fn decode_hex(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    hex::decode(input).map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid hex: {e}")))
}

/// Decode lowercase hex bytes into a byte string.
pub fn decode_hex_lc(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    if input.iter().any(u8::is_ascii_uppercase) {
        return Err(ByteLiteralError::InvalidByteString(
            "hexlc string contains uppercase characters".into(),
        ));
    }
    decode_hex(input)
}

/// Decode uppercase hex bytes into a byte string.
pub fn decode_hex_uc(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    if input.iter().any(u8::is_ascii_lowercase) {
        return Err(ByteLiteralError::InvalidByteString(
            "hexuc string contains lowercase characters".into(),
        ));
    }
    decode_hex(input)
}

/// Decode RFC 4648 Base32 bytes into a byte string.
pub fn decode_b32(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    data_encoding::BASE32_NOPAD
        .decode(input)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base32: {e}")))
}

/// Decode RFC 4648 Base32hex bytes into a byte string.
pub fn decode_h32(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    data_encoding::BASE32HEX_NOPAD
        .decode(input)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base32hex: {e}")))
}

/// Decode Base45 bytes into a byte string.
pub fn decode_b45(input: &[u8]) -> Result<Vec<u8>, ByteLiteralError> {
    let text = str::from_utf8(input)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base45 utf8: {e}")))?;
    base45::decode(text)
        .map_err(|e| ByteLiteralError::InvalidByteString(format!("invalid base45: {e}")))
}

/// Encode bytes as URL-safe base64 without padding.
pub fn encode_b64u(input: &[u8]) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(input)
        .into_bytes()
}

/// Encode bytes as classic base64 with padding.
pub fn encode_b64c(input: &[u8]) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .encode(input)
        .into_bytes()
}

/// Encode bytes as Base32 without padding.
pub fn encode_b32(input: &[u8]) -> Vec<u8> {
    data_encoding::BASE32_NOPAD.encode(input).into_bytes()
}

/// Encode bytes as Base32hex without padding.
pub fn encode_h32(input: &[u8]) -> Vec<u8> {
    data_encoding::BASE32HEX_NOPAD.encode(input).into_bytes()
}

/// Encode bytes as Base45.
pub fn encode_b45(input: &[u8]) -> String {
    base45::encode(input)
}

/// Normalize raw bytes (content between `'` delimiters) to JSON-encoded
/// form suitable for parsing as a text literal.
fn normalize_bytes_to_text(input: &[u8]) -> Vec<u8> {
    #[allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "All indexing bounded by input length"
    )]
    {
        let capacity = (input.len() * 2) + 2;
        let mut out = Vec::with_capacity(capacity);
        out.push(b'"');

        let mut last_copy = 0;

        let mut pos = 0;
        while pos < input.len() {
            match input[pos] {
                b'"' => {
                    out.extend_from_slice(&input[last_copy..pos]);
                    out.extend_from_slice(br#"\""#);
                    pos += 1;
                    last_copy = pos;
                },
                b'\\' if pos + 1 < input.len() && input[pos + 1] == b'\'' => {
                    out.extend_from_slice(&input[last_copy..pos]);
                    out.push(b'\'');
                    pos += 2;
                    last_copy = pos;
                },
                b'\n' => {
                    let copy_end = if pos > last_copy && input[pos - 1] == b'\r' {
                        pos - 1
                    } else {
                        pos
                    };

                    out.extend_from_slice(&input[last_copy..copy_end]);
                    out.extend_from_slice(br"\n");

                    pos += 1;
                    last_copy = pos;
                },
                _ => pos += 1,
            }
        }

        out.extend_from_slice(&input[last_copy..]);
        out.push(b'"');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_byte_literal() {
        let value = ByteLiteralBytes::parse(b"h'48656c6c6f'").unwrap();
        assert_eq!(value.as_ref(), b"Hello");
    }

    #[test]
    fn parse_unprefixed_byte_literal() {
        let value = ByteLiteralBytes::parse(b"'it\\'s'").unwrap();
        assert_eq!(value.as_ref(), b"it's");
    }

    #[test]
    fn parse_unprefixed_byte_literal_with_braced_unicode_escape() {
        let value = ByteLiteralBytes::parse("'\\u{41}'".as_bytes()).unwrap();
        assert_eq!(value.as_ref(), b"A");
    }

    #[test]
    fn normalize_crlf_to_json_escape() {
        let result = ByteLiteralBytes::normalize(b"hello\r\nworld");
        assert_eq!(result, b"\"hello\\nworld\"");
    }

    #[test]
    fn b64u_roundtrip() {
        let bytes = ByteLiteralBytes::from_bytes(b"Hello".to_vec());
        let text = bytes.to_b64u();
        assert_eq!(text.from_b64u().unwrap().as_ref(), b"Hello");
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = ByteLiteralBytes::from_bytes(vec![0xAB, 0xCD]);
        let text = bytes.to_hexuc();
        assert_eq!(text.from_hexuc().unwrap().as_ref(), &[0xAB, 0xCD]);
    }
}
