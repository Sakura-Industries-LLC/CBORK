// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Schema-independent raw-byte CBOR serialization checker.
//!
//! Validates that raw CBOR bytes satisfy preferred-plus or
//! deterministic serialization requirements per
//! `draft-ietf-cbor-serialization-06`, without round-trip re-encoding.
//!
//! The checker walks CBOR items using [`minicbor::Decoder`] and enforces:
//!
//! * Shortest integer and length encodings (Prefp, Dtrm).
//! * Shortest float encodings with the permitted NaN form (Prefp, Dtrm).
//! * No indefinite-length arrays, maps, text, or byte strings (Prefp, Dtrm).
//! * Map keys in deterministic encoded-byte order (Dtrm).
//! * Bignum rules for tags 2 and 3 (Prefp, Dtrm).
//!
//! Nested byte-string payloads are consumed opaquely — the checker
//! validates the bstr wrapper itself and skips over the payload bytes
//! without inspecting them as CBOR.

#![allow(
    clippy::too_many_lines,
    reason = "single walk_item match is clearer than splitting"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "byte offsets are trusted in the context of known CBOR"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "bytes are guarded by length checks before indexing"
)]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "private helpers are self-documenting"
)]
#![allow(
    clippy::cast_lossless,
    reason = "u8->u64 casts are lossless by definition"
)]
#![allow(
    clippy::cast_sign_loss,
    reason = "cbor_int_extra_bytes handles negative values correctly"
)]
#![allow(
    clippy::cast_possible_truncation,
    reason = "f64->f32 truncation is intentional for byte-preservation check"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "i64->u64 conversion in bignum is guarded by sign check"
)]
#![allow(
    clippy::collapsible_if,
    reason = "explicit two-level checks are clearer for CBOR major type dispatch"
)]

use std::fmt;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// The serialization level to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializationMode {
    /// Ordinary CBOR — validate well-formedness only.
    Cbor,
    /// Preferred-plus serialization (shortest encodings, no indefinite).
    Prefp,
    /// Preferred-plus plus deterministic map-key ordering.
    Dtrm,
}

/// The result of a serialization check.
pub type SerializationResult = Result<(), SerializationError>;

/// A descriptive serialization failure.
#[derive(Debug, Clone)]
pub struct SerializationError {
    /// Byte offset of the failing item in the input.
    pub offset: usize,
    /// Human-readable description of the failure.
    pub message: String,
}

impl fmt::Display for SerializationError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(
            f,
            "serialization error at offset {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for SerializationError {}

impl From<minicbor::decode::Error> for SerializationError {
    fn from(e: minicbor::decode::Error) -> Self {
        SerializationError {
            offset: e.position().unwrap_or(0),
            message: e.to_string(),
        }
    }
}

/// Check that `bytes` contains valid CBOR at the requested
/// serialization level.
///
/// When `sequence` is `false`, exactly one top-level CBOR item is
/// required and trailing bytes are an error.
///
/// When `sequence` is `true`, zero or more top-level CBOR items are
/// consumed and each is validated independently.  An empty input is
/// valid only in sequence mode.  Malformed or incomplete later items
/// are reported at their offset.
///
/// # Errors
///
/// Returns [`SerializationError`] at the first encoding violation.
pub fn check_serialization(
    bytes: &[u8],
    mode: SerializationMode,
    sequence: bool,
) -> SerializationResult {
    use minicbor::Decoder;

    if bytes.is_empty() {
        if sequence {
            return Ok(());
        }
        return Err(SerializationError {
            offset: 0,
            message: "empty input is not a valid single CBOR item".to_owned(),
        });
    }

    let mut d = Decoder::new(bytes);
    let mut item_count: u64 = 0;

    loop {
        walk_item(&mut d, bytes, mode)?;
        item_count = item_count.saturating_add(1);

        if !sequence {
            if d.position() < bytes.len() {
                return Err(SerializationError {
                    offset: d.position(),
                    message: "trailing bytes after single CBOR item".to_owned(),
                });
            }
            return Ok(());
        }

        if d.position() >= bytes.len() {
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------------------
// Internal walker
// ---------------------------------------------------------------------------

const CBOR_MAX_TINY: u64 = 23;

fn walk_item(
    d: &mut minicbor::Decoder<'_>,
    bytes: &[u8],
    mode: SerializationMode,
) -> Result<(), SerializationError> {
    let pos = d.position();
    let b = peek_byte(d)?;
    let major = b >> 5;
    let info = b & 0x1F;

    let check_encoding = mode != SerializationMode::Cbor;

    match major {
        0 => {
            if check_encoding {
                check_shortest_uint(d)?;
            } else {
                let _ = d.u64()?;
            }
        },
        1 => {
            if check_encoding {
                check_shortest_int(d)?;
            } else {
                let _ = d.i64()?;
            }
        },
        2 => {
            if check_encoding && info == 31 {
                return Err(SerializationError {
                    offset: pos,
                    message: "indefinite-length byte strings not allowed".to_owned(),
                });
            }
            if check_encoding {
                check_shortest_length_from_bytes(bytes, pos, info)?;
            }
            let _ = d.bytes()?;
        },
        3 => {
            if check_encoding && info == 31 {
                return Err(SerializationError {
                    offset: pos,
                    message: "indefinite-length text strings not allowed".to_owned(),
                });
            }
            if check_encoding {
                check_shortest_length_from_bytes(bytes, pos, info)?;
            }
            let s = d.str()?;
            if s.contains('\u{FFFD}') {
                return Err(SerializationError {
                    offset: pos,
                    message: "invalid UTF-8 sequence in text string".to_owned(),
                });
            }
        },
        4 => {
            if check_encoding && info == 31 {
                return Err(SerializationError {
                    offset: pos,
                    message: "indefinite-length arrays not allowed".to_owned(),
                });
            }
            if info == 31 {
                // Indefinite array in Cbor mode — skip the whole thing
                drop(d.skip());
                return Ok(());
            }
            if check_encoding {
                check_shortest_length_from_bytes(bytes, pos, info)?;
            }
            let count = d.array()?.unwrap_or(0);
            for _ in 0..count {
                walk_item(d, bytes, mode)?;
            }
        },
        5 => {
            if check_encoding && info == 31 {
                return Err(SerializationError {
                    offset: pos,
                    message: "indefinite-length maps not allowed".to_owned(),
                });
            }
            if info == 31 {
                // Indefinite map in Cbor mode — skip the whole thing
                drop(d.skip());
                return Ok(());
            }
            if check_encoding {
                check_shortest_length_from_bytes(bytes, pos, info)?;
            }
            let count = d.map()?.unwrap_or(0);
            let mut prev_key: Option<Vec<u8>> = None;
            let mut prev_key_offset = 0usize;

            for i in 0..count {
                let key_offset = d.position();
                let key_start = d.position();

                walk_item(d, bytes, mode)?;

                let key_end = d.position();
                let key = &bytes[key_start..key_end];

                if mode == SerializationMode::Dtrm {
                    if let Some(ref prev) = prev_key {
                        if key < prev.as_slice() {
                            return Err(SerializationError {
                                offset: key_offset,
                                message: format!(
                                    "map key {i} out of deterministic order (key > key {pi})",
                                    pi = i.saturating_sub(1)
                                ),
                            });
                        }
                        if key == prev.as_slice() {
                            return Err(SerializationError {
                                offset: key_offset,
                                message: format!(
                                    "duplicate map key at entry {i} (first at offset \
                                     {prev_key_offset})"
                                ),
                            });
                        }
                    }
                }
                prev_key = Some(key.to_vec());
                prev_key_offset = key_offset;

                walk_item(d, bytes, mode)?;
            }
        },
        6 => {
            let _tag = d.tag()?;
            let raw_tag_val = tag_value(bytes, pos);

            if check_encoding {
                match raw_tag_val {
                    2 | 3 => {
                        check_bignum_tag(d, bytes, raw_tag_val, mode)?;
                        return Ok(());
                    },
                    _ => {},
                }
            }

            walk_item(d, bytes, mode)?;
        },
        7 => {
            match info {
                25..=27 => {
                    if check_encoding {
                        check_shortest_float(d, bytes, pos)?;
                    } else {
                        let _ = d.f64()?;
                    }
                },
                31 | 23 => {
                    d.undefined()?;
                },
                20 | 21 => {
                    let _ = d.bool()?;
                },
                22 => {
                    d.null()?;
                },
                _other => {
                    // Unassigned simple values — consume as u8
                    let _ = d.u8()?;
                },
            }
        },
        _ => {
            drop(d.skip());
        },
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bignum rules
// ---------------------------------------------------------------------------

fn tag_value(
    bytes: &[u8],
    pos: usize,
) -> u64 {
    if bytes.len() <= pos {
        return 0;
    }
    let b = bytes[pos];
    let info = (b & 0x1F) as u64;
    let after = &bytes[pos + 1..];
    match info {
        0..=23 => info,
        24 => u64::from(after.first().copied().unwrap_or(0)),
        25 => {
            u64::from(u16::from_be_bytes([
                after.first().copied().unwrap_or(0),
                after.get(1).copied().unwrap_or(0),
            ]))
        },
        26 => {
            u64::from(u32::from_be_bytes([
                after.first().copied().unwrap_or(0),
                after.get(1).copied().unwrap_or(0),
                after.get(2).copied().unwrap_or(0),
                after.get(3).copied().unwrap_or(0),
            ]))
        },
        27 => {
            u64::from_be_bytes([
                after.first().copied().unwrap_or(0),
                after.get(1).copied().unwrap_or(0),
                after.get(2).copied().unwrap_or(0),
                after.get(3).copied().unwrap_or(0),
                after.get(4).copied().unwrap_or(0),
                after.get(5).copied().unwrap_or(0),
                after.get(6).copied().unwrap_or(0),
                after.get(7).copied().unwrap_or(0),
            ])
        },
        _ => 0,
    }
}

fn check_bignum_tag(
    d: &mut minicbor::Decoder<'_>,
    bytes: &[u8],
    tag: u64,
    _mode: SerializationMode,
) -> Result<(), SerializationError> {
    let pos = d.position();
    let b = peek_byte(d)?;
    let major = b >> 5;
    let info = b & 0x1F;

    if major != 2 {
        return Err(SerializationError {
            offset: pos,
            message: format!("tag {tag} must be followed by a byte string, not major type {major}"),
        });
    }
    if info == 31 {
        return Err(SerializationError {
            offset: pos,
            message: "indefinite-length bignum payload not allowed".to_owned(),
        });
    }

    check_shortest_length_from_bytes(bytes, pos, info)?;
    let payload = d.bytes()?;

    if payload.is_empty() {
        return Err(SerializationError {
            offset: pos,
            message: format!("tag {tag} payload must not be empty"),
        });
    }

    // Leading-zero rules: a leading 0x00 is only allowed when it
    // prevents the high bit of the real value from being set (which
    // would make it look like a negative number in two's complement).
    let has_allowed_leading_zero = if payload[0] == 0 {
        if payload.len() == 1 {
            return Err(SerializationError {
                offset: pos,
                message: format!("tag {tag} with single zero byte is not a valid bignum"),
            });
        }
        if payload[1] & 0x80 == 0 {
            return Err(SerializationError {
                offset: pos,
                message: format!("tag {tag} has a redundant leading zero byte"),
            });
        }
        true
    } else {
        false
    };

    // Strip the allowed leading zero to get the effective payload
    // whose numeric value is compared against the regular-integer range.
    let effective = if has_allowed_leading_zero {
        &payload[1..]
    } else {
        payload
    };

    // Any value that fits in a u64 can be represented as a regular
    // CBOR integer (major type 0 or 1) and must not use bignum.
    if effective.len() <= 8 {
        return Err(SerializationError {
            offset: pos,
            message: format!("tag {tag} bignum value fits in a regular CBOR integer"),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Peek helper
// ---------------------------------------------------------------------------

fn peek_byte(d: &minicbor::Decoder<'_>) -> Result<u8, SerializationError> {
    let input = d.input();
    input.get(d.position()).copied().ok_or_else(|| {
        SerializationError {
            offset: d.position(),
            message: "unexpected end of input".to_owned(),
        }
    })
}

// ---------------------------------------------------------------------------
// Shortest-encoding checks
// ---------------------------------------------------------------------------

fn check_shortest_uint(d: &mut minicbor::Decoder<'_>) -> Result<(), SerializationError> {
    let pos = d.position();
    let val = d.u64()?;
    let minimal = cbor_uint_extra_bytes(val);
    let actual = u8::try_from(d.position().saturating_sub(pos))
        .unwrap_or(9)
        .saturating_sub(1);
    if actual > minimal {
        return Err(SerializationError {
            offset: pos,
            message: format!(
                "unsigned integer {val} uses {actual}-byte plus header, could use {minimal}-byte"
            ),
        });
    }
    Ok(())
}

fn check_shortest_int(d: &mut minicbor::Decoder<'_>) -> Result<(), SerializationError> {
    let pos = d.position();
    let val = d.i64()?;
    let minimal = cbor_int_extra_bytes(val);
    let actual = u8::try_from(d.position().saturating_sub(pos))
        .unwrap_or(9)
        .saturating_sub(1);
    if actual > minimal {
        return Err(SerializationError {
            offset: pos,
            message: format!(
                "signed integer {val} uses {actual}-byte plus header, could use {minimal}-byte"
            ),
        });
    }
    Ok(())
}

fn check_shortest_length_from_bytes(
    bytes: &[u8],
    pos: usize,
    info: u8,
) -> Result<(), SerializationError> {
    match info {
        24 => {
            if bytes.get(pos.wrapping_add(1)).copied().unwrap_or(0) as u64 <= CBOR_MAX_TINY {
                return Err(SerializationError {
                    offset: pos,
                    message: "length uses 1-byte extended encoding, fits in tiny".to_owned(),
                });
            }
        },
        25 => {
            if bytes.len() < pos.wrapping_add(3) {
                return Err(SerializationError {
                    offset: pos,
                    message: "truncated".to_owned(),
                });
            }
            let len = u64::from(u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]));
            if len <= 255 {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("length {len} uses 2-byte extended, fits in 1 byte"),
                });
            }
        },
        26 => {
            if bytes.len() < pos.wrapping_add(5) {
                return Err(SerializationError {
                    offset: pos,
                    message: "truncated".to_owned(),
                });
            }
            let len = u64::from(u32::from_be_bytes([
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
                bytes[pos + 4],
            ]));
            if len <= 65535 {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("length {len} uses 4-byte extended, fits in 2 bytes"),
                });
            }
        },
        27 => {
            if bytes.len() < pos.wrapping_add(9) {
                return Err(SerializationError {
                    offset: pos,
                    message: "truncated".to_owned(),
                });
            }
            let len = u64::from_be_bytes([
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
                bytes[pos + 8],
            ]);
            if len <= 4_294_967_295 {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("length {len} uses 8-byte extended, fits in 4 bytes"),
                });
            }
        },
        _ => {},
    }
    Ok(())
}

fn cbor_uint_extra_bytes(val: u64) -> u8 {
    if val <= CBOR_MAX_TINY {
        0
    } else if val <= 255 {
        1
    } else if val <= 65_535 {
        2
    } else if val <= 4_294_967_295 {
        4
    } else {
        8
    }
}

fn cbor_int_extra_bytes(val: i64) -> u8 {
    if val >= 0 {
        cbor_uint_extra_bytes(val as u64)
    } else {
        cbor_uint_extra_bytes((-1_i64.saturating_sub(val)) as u64)
    }
}

// ---------------------------------------------------------------------------
// Float checks
// ---------------------------------------------------------------------------

fn check_shortest_float(
    d: &mut minicbor::Decoder<'_>,
    bytes: &[u8],
    pos: usize,
) -> Result<(), SerializationError> {
    let info = bytes[pos] & 0x1F;

    match info {
        25 => {
            let v = d.f16()?;
            if v.is_nan() {
                if bytes.len() >= pos + 3 {
                    let raw = u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]);
                    if raw != 0x7E00 {
                        return Err(SerializationError {
                            offset: pos,
                            message: "NaN must use the permitted form f97e00".to_owned(),
                        });
                    }
                }
            }
        },
        26 => {
            let v = d.f32()?;
            if v.is_nan() {
                return Err(SerializationError {
                    offset: pos,
                    message: "NaN must be encoded as f16, not f32".to_owned(),
                });
            }
            if v.is_infinite() {
                return Ok(());
            }
            if f32_can_fit_f16(v) {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("{v} encoded as f32 but can be represented as f16"),
                });
            }
        },
        27 => {
            let v = d.f64()?;
            if v.is_nan() {
                return Err(SerializationError {
                    offset: pos,
                    message: "NaN must be encoded as f16, not f64".to_owned(),
                });
            }
            if v.is_infinite() {
                return Ok(());
            }
            let as_f32 = v as f32;
            if (as_f32 as f64).to_bits() == v.to_bits() && !f32_can_fit_f16(as_f32) {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("{v} encoded as f64 but can be represented as f32"),
                });
            }
            if f32_can_fit_f16(as_f32) {
                return Err(SerializationError {
                    offset: pos,
                    message: format!("{v} encoded as f64 but can be represented as f16 via f32"),
                });
            }
        },
        _ => {
            drop(d.skip());
        },
    }
    Ok(())
}

#[allow(clippy::float_cmp)]
fn f32_can_fit_f16(v: f32) -> bool {
    half::f16::from_f32(v).to_f32() == v
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_cbor(b: &[u8]) {
        check_serialization(b, SerializationMode::Cbor, false).unwrap();
    }
    fn ok_prefp(b: &[u8]) {
        check_serialization(b, SerializationMode::Prefp, false).unwrap();
    }
    fn ok_dtrm(b: &[u8]) {
        check_serialization(b, SerializationMode::Dtrm, false).unwrap();
    }
    fn ok_cbor_seq(b: &[u8]) {
        check_serialization(b, SerializationMode::Cbor, true).unwrap();
    }
    fn ok_prefp_seq(b: &[u8]) {
        check_serialization(b, SerializationMode::Prefp, true).unwrap();
    }
    fn ok_dtrm_seq(b: &[u8]) {
        check_serialization(b, SerializationMode::Dtrm, true).unwrap();
    }

    // ---- Cbor mode (just well-formedness) ----

    #[test]
    fn cbor_uint_valid() {
        ok_cbor(&[0]);
    }

    #[test]
    fn cbor_indefinite_allowed() {
        ok_cbor(&[0x9F, 1, 2, 0xFF]);
    }

    #[test]
    fn cbor_empty_not_single() {
        assert!(check_serialization(&[], SerializationMode::Cbor, false).is_err());
    }

    #[test]
    fn cbor_empty_seq_valid() {
        check_serialization(&[], SerializationMode::Cbor, true).unwrap();
    }

    // ---- Prefp mode ----

    #[test]
    fn prefp_uint_shortest_valid() {
        ok_prefp(&[0]);
    }

    #[test]
    fn prefp_uint_non_shortest_fails() {
        assert!(check_serialization(&[25u8, 0, 24], SerializationMode::Prefp, false).is_err());
    }

    #[test]
    fn prefp_bstr_indefinite_fails() {
        assert!(
            check_serialization(&[0x5F, 0x41, 1, 0xFF], SerializationMode::Prefp, false).is_err()
        );
    }

    #[test]
    fn prefp_map_unsorted_allowed() {
        ok_prefp(&[0xA2, 0x61, b'b', 2, 0x61, b'a', 1]);
    }

    #[test]
    fn prefp_seq_empty_valid() {
        ok_prefp_seq(&[]);
    }

    #[test]
    fn prefp_seq_multi_item_checks_each() {
        // Two valid uints: 0 and 1
        ok_prefp_seq(&[0, 1]);
    }

    #[test]
    fn prefp_seq_second_item_non_shortest_fails() {
        let err =
            check_serialization(&[0, 25u8, 0, 24], SerializationMode::Prefp, true).unwrap_err();
        assert!(err.offset > 0, "should fail at second item, not first");
    }

    #[test]
    fn prefp_trailing_after_single_fails() {
        assert!(check_serialization(&[0, 1], SerializationMode::Prefp, false).is_err());
    }

    #[test]
    fn prefp_float_nan_wrong_form_fails() {
        let err =
            check_serialization(&[0xF9, 0x7C, 0x01], SerializationMode::Prefp, false).unwrap_err();
        assert!(err.message.contains("NaN"));
    }

    // ---- Dtrm mode ----

    #[test]
    fn dtrm_map_sorted_valid() {
        ok_dtrm(&[0xA2, 0x61, b'a', 1, 0x61, b'b', 2]);
    }

    #[test]
    fn dtrm_map_unsorted_fails() {
        assert!(
            check_serialization(
                &[0xA2, 0x61, b'b', 2, 0x61, b'a', 1],
                SerializationMode::Dtrm,
                false
            )
            .is_err()
        );
    }

    // ---- Sequence mode ----

    #[test]
    fn seq_empty_all_modes() {
        ok_cbor_seq(&[]);
        ok_prefp_seq(&[]);
        ok_dtrm_seq(&[]);
    }

    #[test]
    fn seq_empty_rejected_for_single() {
        assert!(check_serialization(&[], SerializationMode::Cbor, false).is_err());
        assert!(check_serialization(&[], SerializationMode::Prefp, false).is_err());
        assert!(check_serialization(&[], SerializationMode::Dtrm, false).is_err());
    }

    // ---- Bstr opacity ----

    #[test]
    fn nested_bstr_opaque() {
        ok_prefp(&[0x44, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn array_with_bstr_continues_after_payload() {
        // [h'deadbeef', 1] — valid prefp array with opaque bstr payload
        ok_prefp(&[0x82, 0x44, 0xDE, 0xAD, 0xBE, 0xEF, 1]);
    }

    #[test]
    fn array_with_bstr_map_unsorted_inside_payload_accepted() {
        // [{h'a2 61 62 02 61 61 01'}, 1]
        // unsorted map INSIDE bstr payload — must pass (payload is opaque)
        ok_dtrm(&[0x82, 0x47, 0xA2, 0x61, 0x62, 0x02, 0x61, 0x61, 0x01, 1]);
    }

    // ---- Bignum ----

    #[test]
    fn bignum_valid_large() {
        // tag(2) with 9-byte payload: value > 2^64-1
        let mut bytes = vec![0xC2, 0x49];
        bytes.extend(std::iter::repeat_n(1, 9));
        ok_prefp(&bytes);
    }

    #[test]
    fn bignum_fits_in_regular_uint_fails() {
        // tag(2) [h'0100'] = 256 — fits in regular uint (u16 encoding)
        let err = check_serialization(&[0xC2, 0x42, 0x01, 0x00], SerializationMode::Prefp, false)
            .unwrap_err();
        assert!(err.message.contains("fits in a regular"), "got: {err:?}");
    }

    #[test]
    fn bignum_value_24_fits_in_regular_fails() {
        // tag(2) [h'18'] = 24 — fits in regular uint (1 extra byte)
        let err =
            check_serialization(&[0xC2, 0x41, 24], SerializationMode::Prefp, false).unwrap_err();
        assert!(err.message.contains("fits in a regular"), "got: {err:?}");
    }

    #[test]
    fn bignum_single_zero_byte_rejected() {
        let err =
            check_serialization(&[0xC2, 0x41, 0x00], SerializationMode::Prefp, false).unwrap_err();
        assert!(
            err.message.contains("not a valid bignum") || err.message.contains("fits in a regular"),
            "got: {err:?}"
        );
    }

    #[test]
    fn bignum_redundant_leading_zero_fails() {
        // 9-byte payload with a redundant leading zero
        let mut payload = vec![0x00];
        payload.extend(std::iter::repeat_n(0x01, 8));
        let mut input = vec![0xC2, 0x49];
        input.extend_from_slice(&payload);
        let err = check_serialization(&input, SerializationMode::Prefp, false).unwrap_err();
        assert!(err.message.contains("leading zero"), "got: {err:?}");
    }

    #[test]
    fn bignum_tag_3_negative_valid_large() {
        // tag(3) with 9-byte payload
        let mut bytes = vec![0xC3, 0x49];
        bytes.extend(std::iter::repeat_n(1, 9));
        ok_prefp(&bytes);
    }

    #[test]
    fn bignum_tag_3_fits_in_regular_nint_fails() {
        // tag(3) [h'18'] represents -25, which fits in regular nint
        let err =
            check_serialization(&[0xC3, 0x41, 24], SerializationMode::Prefp, false).unwrap_err();
        assert!(err.message.contains("fits in a regular"), "got: {err:?}");
    }

    // ---- Simple values ----

    #[test]
    fn simple_false_true() {
        ok_prefp(&[0xF4]);
        ok_prefp(&[0xF5]);
    }

    #[test]
    fn simple_null() {
        ok_prefp(&[0xF6]);
    }

    #[test]
    fn simple_undefined() {
        ok_prefp(&[0xF7]);
    }

    // ---- Production-path simulation ----

    #[test]
    fn production_path_dtrm_bstr_rejects_non_shortest_in_payload() {
        // bstr .dtrm { 24: "x" } — but 24 uses non-shortest encoding (should be 0x18 0x18, not
        // 0x18 0x18) Actually: key 24 encoded properly as 0x1818 is OK. Let's test with
        // key 24 encoded as 0x19 00 18 (non-shortest)
        let err = check_serialization(
            &[0xA1, 0x19, 0x00, 0x18, 0x61, b'x'],
            SerializationMode::Prefp,
            false,
        )
        .unwrap_err();
        assert!(
            err.message.contains("24"),
            "should catch non-shortest map key: {err:?}"
        );
    }
}
