// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Parse raw CBOR into an owned EDN-like tree.

use core::fmt;

use minicbor::{
    Decoder, Encoder,
    data::{Int, Tag, Type},
    encode::Write,
};

/// Parse raw CBOR bytes into an owned document tree.
///
/// This accepts a single CBOR item or a concatenated CBOR sequence.
///
/// # Errors
///
/// Returns an error if the input is empty, contains invalid CBOR, or ends
/// with trailing bytes that are not valid CBOR.
pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    Document::parse(bytes)
}

/// A parsed CBOR document.
///
/// The document preserves top-level concatenation by storing each parsed item
/// in source order.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Parsed top-level items in source order.
    items: Vec<Value>,
}

impl Document {
    /// Parse raw CBOR bytes into a document.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is empty, contains invalid CBOR, or ends
    /// with trailing bytes that are not valid CBOR.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut decoder = Decoder::new(bytes);
        let mut items = Vec::new();

        while decoder.position() < bytes.len() {
            items.push(parse_value(&mut decoder)?);
        }

        if items.is_empty() {
            return Err(Error::empty_input());
        }

        Ok(Self { items })
    }

    /// Top-level parsed CBOR items in source order.
    #[must_use]
    pub fn items(&self) -> &[Value] {
        &self.items
    }

    /// Consume the document and return the parsed items.
    #[must_use]
    pub fn into_items(self) -> Vec<Value> {
        self.items
    }

    /// Returns true when the document contains more than one top-level CBOR item.
    #[must_use]
    pub fn is_sequence(&self) -> bool {
        self.items.len() > 1
    }

    /// Encode the document using a deterministic CBOR ordering.
    ///
    /// Maps are sorted by canonical key encoding before serialization.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree cannot be serialized.
    pub fn to_deterministic_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes);
            for item in &self.items {
                encode_value(&mut encoder, item)?;
            }
        }
        Ok(bytes)
    }
}

impl fmt::Display for Document {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self.items.as_slice() {
            [item] => write!(f, "{item}"),
            items => {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{item}")?;
                }
                Ok(())
            },
        }
    }
}

/// A parsed CBOR value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Any CBOR integer.
    Integer(Int),
    /// Any CBOR floating-point value.
    Float(Float),
    /// A CBOR boolean.
    Bool(bool),
    /// CBOR `null`.
    Null,
    /// CBOR `undefined`.
    Undefined,
    /// CBOR simple value.
    Simple(u8),
    /// A CBOR byte string.
    Bytes(Vec<u8>),
    /// A CBOR text string.
    Text(String),
    /// A CBOR array.
    Array(Vec<Value>),
    /// A CBOR map, preserving source order.
    Map(Vec<MapEntry>),
    /// A CBOR tagged item.
    Tag(u64, Box<Value>),
}

/// A parsed CBOR floating-point value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Float {
    /// A decoded `f16` value.
    F16(f32),
    /// A decoded `f32` value.
    F32(f32),
    /// A decoded `f64` value.
    F64(f64),
}

/// A parsed CBOR map entry.
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    /// The map key.
    pub key: Value,
    /// The map value.
    pub value: Value,
}

impl fmt::Display for Float {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::F16(value) | Self::F32(value) => write!(f, "{value}"),
            Self::F64(value) => write!(f, "{value}"),
        }
    }
}

impl fmt::Display for MapEntry {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.value)
    }
}

impl fmt::Display for Value {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Integer(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Null => f.write_str("null"),
            Self::Undefined => f.write_str("undefined"),
            Self::Simple(value) => write!(f, "simple({value})"),
            Self::Bytes(value) => write_bytes(f, value),
            Self::Text(value) => write!(f, "{value:?}"),
            Self::Array(values) => {
                f.write_str("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{value}")?;
                }
                f.write_str("]")
            },
            Self::Map(entries) => {
                f.write_str("{")?;
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{entry}")?;
                }
                f.write_str("}")
            },
            Self::Tag(tag, value) => write!(f, "{tag}({value})"),
        }
    }
}

impl Value {
    /// Encode this value using a deterministic CBOR ordering.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be serialized.
    pub fn to_deterministic_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes);
            encode_value(&mut encoder, self)?;
        }
        Ok(bytes)
    }
}

/// Parse a single CBOR value from the decoder.
fn parse_value(decoder: &mut Decoder<'_>) -> Result<Value, Error> {
    let start = decoder.position();
    let value = match decoder.datatype() {
        Ok(Type::Bool) => Value::Bool(decoder.bool().map_err(|error| wrap_error(start, &error))?),
        Ok(Type::Null) => {
            decoder.null().map_err(|error| wrap_error(start, &error))?;
            Value::Null
        },
        Ok(Type::Undefined) => {
            decoder
                .undefined()
                .map_err(|error| wrap_error(start, &error))?;
            Value::Undefined
        },
        Ok(Type::Simple) => {
            Value::Simple(
                decoder
                    .simple()
                    .map_err(|error| wrap_error(start, &error))?,
            )
        },
        Ok(
            Type::Int
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::I8
            | Type::I16
            | Type::I32
            | Type::I64,
        ) => Value::Integer(decoder.int().map_err(|error| wrap_error(start, &error))?),
        Ok(Type::F16) => {
            Value::Float(Float::F16(
                decoder.f16().map_err(|error| wrap_error(start, &error))?,
            ))
        },
        Ok(Type::F32) => {
            Value::Float(Float::F32(
                decoder.f32().map_err(|error| wrap_error(start, &error))?,
            ))
        },
        Ok(Type::F64) => {
            Value::Float(Float::F64(
                decoder.f64().map_err(|error| wrap_error(start, &error))?,
            ))
        },
        Ok(Type::Bytes) => {
            Value::Bytes(
                decoder
                    .bytes()
                    .map_err(|error| wrap_error(start, &error))?
                    .to_vec(),
            )
        },
        Ok(Type::BytesIndef) => Value::Bytes(parse_indef_bytes(decoder, start)?),
        Ok(Type::String) => {
            Value::Text(
                decoder
                    .str()
                    .map_err(|error| wrap_error(start, &error))?
                    .to_owned(),
            )
        },
        Ok(Type::StringIndef) => Value::Text(parse_indef_text(decoder, start)?),
        Ok(Type::Array | Type::ArrayIndef) => Value::Array(parse_array(decoder, start)?),
        Ok(Type::Map | Type::MapIndef) => Value::Map(parse_map(decoder, start)?),
        Ok(Type::Tag) => {
            let tag = decoder.tag().map_err(|error| wrap_error(start, &error))?;
            let nested = parse_value(decoder)?;
            Value::Tag(u64::from(tag), Box::new(nested))
        },
        Ok(Type::Break) => {
            return Err(Error::decode(start, "unexpected CBOR break marker"));
        },
        Ok(Type::Unknown(value)) => {
            return Err(Error::decode(
                start,
                format!("unknown CBOR data type {value:#x}"),
            ));
        },
        Err(error) => return Err(wrap_error(start, &error)),
    };

    Ok(value)
}

/// Parse a CBOR array, preserving nested items in source order.
fn parse_array(
    decoder: &mut Decoder<'_>,
    start: usize,
) -> Result<Vec<Value>, Error> {
    let mut items = Vec::new();
    let len = decoder.array().map_err(|error| wrap_error(start, &error))?;

    match len {
        Some(count) => {
            let capacity = usize::try_from(count)
                .map_err(|_| Error::decode(start, "array length too large for this platform"))?;
            items.reserve(capacity);
            for _ in 0..count {
                items.push(parse_value(decoder)?);
            }
        },
        None => {
            loop {
                match decoder.datatype() {
                    Ok(Type::Break) => {
                        decoder
                            .skip()
                            .map_err(|error| wrap_error(decoder.position(), &error))?;
                        break;
                    },
                    Ok(_) => items.push(parse_value(decoder)?),
                    Err(error) => return Err(wrap_error(decoder.position(), &error)),
                }
            }
        },
    }

    Ok(items)
}

/// Parse a CBOR map, preserving entry order.
fn parse_map(
    decoder: &mut Decoder<'_>,
    start: usize,
) -> Result<Vec<MapEntry>, Error> {
    let mut entries = Vec::new();
    let len = decoder.map().map_err(|error| wrap_error(start, &error))?;

    match len {
        Some(count) => {
            let capacity = usize::try_from(count)
                .map_err(|_| Error::decode(start, "map length too large for this platform"))?;
            entries.reserve(capacity);
            for _ in 0..count {
                let key = parse_value(decoder)?;
                let value = parse_value(decoder)?;
                entries.push(MapEntry { key, value });
            }
        },
        None => {
            loop {
                match decoder.datatype() {
                    Ok(Type::Break) => {
                        decoder
                            .skip()
                            .map_err(|error| wrap_error(decoder.position(), &error))?;
                        break;
                    },
                    Ok(_) => {
                        let key = parse_value(decoder)?;
                        let value = parse_value(decoder)?;
                        entries.push(MapEntry { key, value });
                    },
                    Err(error) => return Err(wrap_error(decoder.position(), &error)),
                }
            }
        },
    }

    Ok(entries)
}

/// Parse an indefinite-length byte string into owned bytes.
fn parse_indef_bytes(
    decoder: &mut Decoder<'_>,
    start: usize,
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let iter = decoder
        .bytes_iter()
        .map_err(|error| wrap_error(start, &error))?;

    for chunk in iter {
        let chunk = chunk.map_err(|error| wrap_error(start, &error))?;
        bytes.extend_from_slice(chunk);
    }

    Ok(bytes)
}

/// Parse an indefinite-length text string into an owned `String`.
fn parse_indef_text(
    decoder: &mut Decoder<'_>,
    start: usize,
) -> Result<String, Error> {
    let mut text = String::new();
    let iter = decoder
        .str_iter()
        .map_err(|error| wrap_error(start, &error))?;

    for chunk in iter {
        let chunk = chunk.map_err(|error| wrap_error(start, &error))?;
        text.push_str(chunk);
    }

    Ok(text)
}

/// Convert a `minicbor` decode error into a crate error with source offset.
fn wrap_error(
    offset: usize,
    error: &minicbor::decode::Error,
) -> Error {
    Error::decode(offset, error.to_string())
}

/// Render a byte slice as CBOR diagnostic notation bytes.
fn write_bytes(
    f: &mut fmt::Formatter<'_>,
    bytes: &[u8],
) -> fmt::Result {
    f.write_str("h'")?;
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{byte:02x}")?;
    }
    f.write_str("'")
}

/// Encode a parsed value back to CBOR using deterministic ordering.
fn encode_value<W>(
    encoder: &mut Encoder<W>,
    value: &Value,
) -> Result<(), Error>
where
    W: Write,
{
    match value {
        Value::Integer(value) => encode_integer(encoder, *value)?,
        Value::Float(value) => encode_float(encoder, *value)?,
        Value::Bool(value) => encode_bool(encoder, *value)?,
        Value::Null => encode_null(encoder)?,
        Value::Undefined => encode_undefined(encoder)?,
        Value::Simple(value) => encode_simple(encoder, *value)?,
        Value::Bytes(value) => encode_bytes(encoder, value)?,
        Value::Text(value) => encode_text(encoder, value)?,
        Value::Array(values) => encode_array(encoder, values)?,
        Value::Map(entries) => encode_map(encoder, entries)?,
        Value::Tag(tag, inner) => encode_tag(encoder, *tag, inner)?,
    }

    Ok(())
}

/// Encode a parsed integer value.
fn encode_integer<W>(
    encoder: &mut Encoder<W>,
    value: Int,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .int(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode a parsed floating-point value.
fn encode_float<W>(
    encoder: &mut Encoder<W>,
    value: Float,
) -> Result<(), Error>
where
    W: Write,
{
    match value {
        Float::F16(value) => encode_f16(encoder, value),
        Float::F32(value) => encode_f32(encoder, value),
        Float::F64(value) => encode_f64(encoder, value),
    }
}

/// Encode an `f16` value.
fn encode_f16<W>(
    encoder: &mut Encoder<W>,
    value: f32,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .f16(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode an `f32` value, downscaling to `f16` when it is exact.
fn encode_f32<W>(
    encoder: &mut Encoder<W>,
    value: f32,
) -> Result<(), Error>
where
    W: Write,
{
    if value.is_finite() && fits_in_f16(value) {
        encode_f16(encoder, value)
    } else {
        encoder
            .f32(value)
            .map(|_| ())
            .map_err(|_| Error::decode(0, "encode error"))
    }
}

/// Encode an `f64` value, downscaling when the value is exactly representable.
fn encode_f64<W>(
    encoder: &mut Encoder<W>,
    value: f64,
) -> Result<(), Error>
where
    W: Write,
{
    if value.is_finite() {
        let narrowed_f32 = narrow_f64_to_f32(value);
        if fits_in_f16_f64(value) {
            encode_f16(encoder, narrowed_f32)
        } else if fits_in_f32(value) {
            encode_f32(encoder, narrowed_f32)
        } else {
            encoder
                .f64(value)
                .map(|_| ())
                .map_err(|_| Error::decode(0, "encode error"))
        }
    } else {
        encoder
            .f64(value)
            .map(|_| ())
            .map_err(|_| Error::decode(0, "encode error"))
    }
}

/// Encode a boolean.
fn encode_bool<W>(
    encoder: &mut Encoder<W>,
    value: bool,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .bool(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode CBOR `null`.
fn encode_null<W>(encoder: &mut Encoder<W>) -> Result<(), Error>
where W: Write {
    encoder
        .null()
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode CBOR `undefined`.
fn encode_undefined<W>(encoder: &mut Encoder<W>) -> Result<(), Error>
where W: Write {
    encoder
        .undefined()
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode a CBOR simple value.
fn encode_simple<W>(
    encoder: &mut Encoder<W>,
    value: u8,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .simple(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode a CBOR byte string.
fn encode_bytes<W>(
    encoder: &mut Encoder<W>,
    value: &[u8],
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .bytes(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode a CBOR text string.
fn encode_text<W>(
    encoder: &mut Encoder<W>,
    value: &str,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .str(value)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))
}

/// Encode a CBOR array in source order.
fn encode_array<W>(
    encoder: &mut Encoder<W>,
    values: &[Value],
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .array(values.len() as u64)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))?;
    for item in values {
        encode_value(encoder, item)?;
    }
    Ok(())
}

/// Encode a CBOR map in deterministic key order.
fn encode_map<W>(
    encoder: &mut Encoder<W>,
    entries: &[MapEntry],
) -> Result<(), Error>
where
    W: Write,
{
    let mut sorted = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut key_bytes = Vec::new();
        {
            let mut key_encoder = Encoder::new(&mut key_bytes);
            encode_value(&mut key_encoder, &entry.key)?;
        }
        sorted.push((index, key_bytes, entry));
    }

    sorted.sort_by(|left, right| {
        left.1
            .len()
            .cmp(&right.1.len())
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.0.cmp(&right.0))
    });

    encoder
        .map(entries.len() as u64)
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))?;
    for (_, _, entry) in sorted {
        encode_value(encoder, &entry.key)?;
        encode_value(encoder, &entry.value)?;
    }
    Ok(())
}

/// Encode a CBOR tag.
fn encode_tag<W>(
    encoder: &mut Encoder<W>,
    tag: u64,
    inner: &Value,
) -> Result<(), Error>
where
    W: Write,
{
    encoder
        .tag(Tag::new(tag))
        .map(|_| ())
        .map_err(|_| Error::decode(0, "encode error"))?;
    encode_value(encoder, inner)
}

/// Return whether an `f32` value round-trips exactly through `f16`.
fn fits_in_f16(value: f32) -> bool {
    half::f16::from_f32(value).to_f32().to_bits() == value.to_bits()
}

/// Return whether an `f64` value round-trips exactly through `f16`.
fn fits_in_f16_f64(value: f64) -> bool {
    half::f16::from_f64(value).to_f64().to_bits() == value.to_bits()
}

// Narrowing is intentional here because we only use the result to test exact
// representability before choosing a compact CBOR float width.
/// Return whether an `f64` value round-trips exactly through `f32`.
#[allow(clippy::cast_possible_truncation)]
fn fits_in_f32(value: f64) -> bool {
    let narrowed = value as f32;
    f64::from(narrowed).to_bits() == value.to_bits()
}

// Narrowing is intentional here because we only use the result to test exact
// representability before choosing a compact CBOR float width.
/// Narrow an `f64` to `f32` for exact representability checks.
#[allow(clippy::cast_possible_truncation)]
fn narrow_f64_to_f32(value: f64) -> f32 {
    value as f32
}

/// Errors returned while parsing CBOR into an EDN-like tree.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input contained no CBOR data at all.
    #[error("empty input")]
    EmptyInput,
    /// The input was not valid CBOR at the reported byte offset.
    #[error("invalid CBOR at byte {offset}: {message}")]
    Decode {
        /// Byte offset where parsing failed.
        offset: usize,
        /// Human-readable decode failure.
        message: String,
    },
}

impl Error {
    /// Construct the empty-input error.
    fn empty_input() -> Self {
        Self::EmptyInput
    }

    /// Construct a decode error at the given byte offset.
    fn decode(
        offset: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::Decode {
            offset,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use minicbor::{Encoder, data::Tag};

    use super::*;

    /// Parse a single integer item.
    #[test]
    fn parses_single_item() {
        let bytes = [0x01];
        let document = parse(&bytes).expect("parse");

        assert_eq!(document.items().len(), 1);
        assert_eq!(document.items()[0], Value::Integer(Int::from(1u8)));
        assert_eq!(document.to_string(), "1");
    }

    /// Parse concatenated top-level CBOR items as a sequence.
    #[test]
    fn parses_concatenated_sequence() {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.u8(1).expect("encode int");
        encoder.str("hello").expect("encode text");

        let document = parse(&bytes).expect("parse");

        assert_eq!(document.items().len(), 2);
        assert_eq!(document.items()[0], Value::Integer(Int::from(1u8)));
        assert_eq!(document.items()[1], Value::Text(String::from("hello")));
    }

    /// Parse indefinite-length byte and text strings into owned values.
    #[test]
    fn parses_indefinite_bytes_and_text() {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.begin_bytes().expect("begin bytes");
        encoder.bytes(b"ab").expect("chunk");
        encoder.bytes(b"cd").expect("chunk");
        encoder.end().expect("end bytes");
        encoder.begin_str().expect("begin text");
        encoder.str("hello").expect("chunk");
        encoder.str(" ").expect("chunk");
        encoder.str("world").expect("chunk");
        encoder.end().expect("end text");

        let document = parse(&bytes).expect("parse");

        assert_eq!(document.items(), &[
            Value::Bytes(b"abcd".to_vec()),
            Value::Text(String::from("hello world")),
        ]);
    }

    /// Parse nested tags, arrays, and maps into a recursive tree.
    #[test]
    fn parses_nested_tags_arrays_and_maps() {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.tag(Tag::new(24)).expect("tag");
        encoder.array(2).expect("array");
        encoder.u8(1).expect("array value");
        encoder.map(1).expect("map");
        encoder.str("k").expect("map key");
        encoder.bool(true).expect("map value");

        let document = parse(&bytes).expect("parse");

        assert_eq!(document.items().len(), 1);
        assert_eq!(
            document.items()[0],
            Value::Tag(
                24,
                Box::new(Value::Array(vec![
                    Value::Integer(Int::from(1u8)),
                    Value::Map(vec![MapEntry {
                        key: Value::Text(String::from("k")),
                        value: Value::Bool(true),
                    }]),
                ]))
            )
        );
    }

    /// Deterministically re-encode maps in canonical key order.
    #[test]
    fn deterministic_encoding_sorts_map_keys() {
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes);
        encoder.map(2).expect("map");
        encoder.u8(2).expect("key");
        encoder.u8(2).expect("value");
        encoder.u8(1).expect("key");
        encoder.u8(1).expect("value");

        let document = parse(&bytes).expect("parse");
        let deterministic = document.to_deterministic_bytes().expect("encode");

        let mut expected = Vec::new();
        let mut encoder = Encoder::new(&mut expected);
        encoder.map(2).expect("map");
        encoder.u8(1).expect("key");
        encoder.u8(1).expect("value");
        encoder.u8(2).expect("key");
        encoder.u8(2).expect("value");

        assert_eq!(deterministic, expected);
    }

    /// Reject input with trailing bytes that are not a valid CBOR item.
    #[test]
    fn rejects_trailing_garbage() {
        let bytes = [0x01, 0x1C];
        let error = parse(&bytes).expect_err("error");

        match error {
            Error::Decode { offset, .. } => assert_eq!(offset, 1),
            Error::EmptyInput => panic!("unexpected empty input"),
        }
    }

    /// Reject empty input as invalid CBOR.
    #[test]
    fn rejects_empty_input() {
        let error = parse(&[]).expect_err("error");
        assert!(matches!(error, Error::EmptyInput));
    }
}
