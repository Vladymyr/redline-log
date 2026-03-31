use alloc::{string::String, vec::Vec};
use core::{fmt, str};
use smallvec::SmallVec;
use tracing_core::Level;

use crate::{BinaryFrameKind, CallsiteKind, FieldValue, InlineString, Timestamp};

/// Decoded callsite metadata from a binary metadata frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedCallsiteMetadata {
    pub id: u32,
    pub name: String,
    pub target: String,
    pub level: Level,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub module_path: Option<String>,
    pub fields: SmallVec<[String; 8]>,
    pub kind: CallsiteKind,
}

/// Decoded field from a binary event or span.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedField {
    pub name: String,
    pub value: FieldValue,
}

/// Small-vector storage for decoded fields.
pub type DecodedFields = SmallVec<[DecodedField; 8]>;

/// Decoded span snapshot from a binary frame.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedSpanSnapshot {
    pub id: u64,
    pub metadata_id: u32,
    pub name: String,
    pub target: String,
    pub level: Level,
    pub fields: DecodedFields,
}

/// Decoded event record from a binary frame.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedRecord {
    pub timestamp: Timestamp,
    pub metadata_id: u32,
    pub current_span_id: u64,
    pub fields: DecodedFields,
    pub current_span: Option<DecodedSpanSnapshot>,
    pub spans: SmallVec<[DecodedSpanSnapshot; 4]>,
}

/// Decoded binary frame.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum DecodedBinaryFrame {
    Metadata(DecodedCallsiteMetadata),
    Event(DecodedRecord),
}

/// Error returned when decoding binary frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinaryDecodeError {
    UnexpectedEof,
    InvalidFrameKind(u8),
    InvalidCallsiteKind(u8),
    InvalidLevel(u8),
    InvalidFieldType(u8),
    InvalidUtf8,
}

impl fmt::Display for BinaryDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("unexpected end of binary frame"),
            Self::InvalidFrameKind(kind) => write!(f, "invalid frame kind `{kind}`"),
            Self::InvalidCallsiteKind(kind) => write!(f, "invalid callsite kind `{kind}`"),
            Self::InvalidLevel(level) => write!(f, "invalid level code `{level}`"),
            Self::InvalidFieldType(kind) => write!(f, "invalid field type `{kind}`"),
            Self::InvalidUtf8 => f.write_str("invalid utf-8 string in binary frame"),
        }
    }
}

/// Decodes the next binary frame and returns the remaining input.
///
/// This is useful when reading a stream containing multiple concatenated
/// metadata and event frames.
pub fn decode_binary_frame(input: &[u8]) -> Result<(DecodedBinaryFrame, &[u8]), BinaryDecodeError> {
    let (frame_kind, rest) = take_u8(input)?;
    let (payload_len, rest) = take_u32(rest)?;
    let payload_len = payload_len as usize;
    if rest.len() < payload_len {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (payload, remaining) = rest.split_at(payload_len);
    let frame = match frame_kind {
        kind if kind == BinaryFrameKind::Metadata as u8 => {
            DecodedBinaryFrame::Metadata(decode_metadata_payload(payload)?)
        }
        kind if kind == BinaryFrameKind::Event as u8 => {
            DecodedBinaryFrame::Event(decode_event_payload(payload)?)
        }
        other => return Err(BinaryDecodeError::InvalidFrameKind(other)),
    };
    Ok((frame, remaining))
}

fn decode_metadata_payload(mut input: &[u8]) -> Result<DecodedCallsiteMetadata, BinaryDecodeError> {
    let (id, rest) = take_u32(input)?;
    input = rest;
    let (kind, rest) = take_u8(input)?;
    input = rest;
    let kind = match kind {
        1 => CallsiteKind::Event,
        2 => CallsiteKind::Span,
        other => return Err(BinaryDecodeError::InvalidCallsiteKind(other)),
    };
    let (level_code, rest) = take_u8(input)?;
    input = rest;
    let level = decode_level(level_code)?;
    let (file, rest) = take_optional_string(input)?;
    input = rest;
    let (line, rest) = take_optional_u32(input)?;
    input = rest;
    let (module_path, rest) = take_optional_string(input)?;
    input = rest;
    let (name, rest) = take_string(input)?;
    input = rest;
    let (target, rest) = take_string(input)?;
    input = rest;
    let (field_count, mut rest) = take_u16(input)?;
    let mut fields = SmallVec::new();
    for _ in 0..field_count {
        let (field, next) = take_string(rest)?;
        rest = next;
        fields.push(field);
    }

    Ok(DecodedCallsiteMetadata {
        id,
        name,
        target,
        level,
        file,
        line,
        module_path,
        fields,
        kind,
    })
}

fn decode_event_payload(mut input: &[u8]) -> Result<DecodedRecord, BinaryDecodeError> {
    let (unix_seconds, rest) = take_u64(input)?;
    input = rest;
    let (subsec_nanos, rest) = take_u32(input)?;
    input = rest;
    let (metadata_id, rest) = take_u32(input)?;
    input = rest;
    let (current_span_id, rest) = take_u64(input)?;
    input = rest;
    let (fields, rest) = take_fields(input)?;
    input = rest;
    let (has_current_span, rest) = take_u8(input)?;
    input = rest;
    let current_span = if has_current_span == 1 {
        let (span, rest) = take_span(input)?;
        input = rest;
        Some(span)
    } else {
        None
    };

    let (span_count, mut rest) = take_u16(input)?;
    let mut spans = SmallVec::new();
    for _ in 0..span_count {
        let (span, next) = take_span(rest)?;
        rest = next;
        spans.push(span);
    }

    Ok(DecodedRecord {
        timestamp: Timestamp::new(unix_seconds, subsec_nanos),
        metadata_id,
        current_span_id,
        fields,
        current_span,
        spans,
    })
}

fn take_span(input: &[u8]) -> Result<(DecodedSpanSnapshot, &[u8]), BinaryDecodeError> {
    let (id, rest) = take_u64(input)?;
    let (metadata_id, rest) = take_u32(rest)?;
    let (level_code, rest) = take_u8(rest)?;
    let level = decode_level(level_code)?;
    let (name, rest) = take_string(rest)?;
    let (target, rest) = take_string(rest)?;
    let (fields, rest) = take_fields(rest)?;

    Ok((
        DecodedSpanSnapshot {
            id,
            metadata_id,
            name,
            target,
            level,
            fields,
        },
        rest,
    ))
}

fn take_fields(input: &[u8]) -> Result<(DecodedFields, &[u8]), BinaryDecodeError> {
    let (field_count, mut input) = take_u16(input)?;
    let mut fields = SmallVec::new();
    for _ in 0..field_count {
        let (name, rest) = take_string(input)?;
        input = rest;
        let (value, rest) = take_field_value(input)?;
        input = rest;
        fields.push(DecodedField { name, value });
    }
    Ok((fields, input))
}

fn take_field_value(input: &[u8]) -> Result<(FieldValue, &[u8]), BinaryDecodeError> {
    let (kind, input) = take_u8(input)?;
    match kind {
        1 => Ok((FieldValue::Bool(false), input)),
        2 => Ok((FieldValue::Bool(true), input)),
        3 => {
            let (value, input) = take_i64(input)?;
            Ok((FieldValue::I64(value), input))
        }
        4 => {
            let (value, input) = take_u64(input)?;
            Ok((FieldValue::U64(value), input))
        }
        5 => {
            let (value, input) = take_i128(input)?;
            Ok((FieldValue::I128(value), input))
        }
        6 => {
            let (value, input) = take_u128(input)?;
            Ok((FieldValue::U128(value), input))
        }
        7 => {
            let (value, input) = take_f64(input)?;
            Ok((FieldValue::F64(value), input))
        }
        8 => {
            let (value, input) = take_string(input)?;
            Ok((FieldValue::Str(InlineString::from(value)), input))
        }
        9 => {
            let (value, input) = take_string(input)?;
            Ok((FieldValue::Debug(InlineString::from(value)), input))
        }
        10 => {
            let (value, input) = take_bytes(input)?;
            Ok((FieldValue::Bytes(value), input))
        }
        other => Err(BinaryDecodeError::InvalidFieldType(other)),
    }
}

fn take_u8(input: &[u8]) -> Result<(u8, &[u8]), BinaryDecodeError> {
    let Some((first, rest)) = input.split_first() else {
        return Err(BinaryDecodeError::UnexpectedEof);
    };
    Ok((*first, rest))
}

fn take_u16(input: &[u8]) -> Result<(u16, &[u8]), BinaryDecodeError> {
    if input.len() < 2 {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(2);
    Ok((u16::from_le_bytes([bytes[0], bytes[1]]), rest))
}

fn take_u32(input: &[u8]) -> Result<(u32, &[u8]), BinaryDecodeError> {
    if input.len() < 4 {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(4);
    Ok((
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        rest,
    ))
}

fn take_u64(input: &[u8]) -> Result<(u64, &[u8]), BinaryDecodeError> {
    if input.len() < 8 {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(8);
    Ok((
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        rest,
    ))
}

fn take_i64(input: &[u8]) -> Result<(i64, &[u8]), BinaryDecodeError> {
    let (value, rest) = take_u64(input)?;
    Ok((value as i64, rest))
}

fn take_u128(input: &[u8]) -> Result<(u128, &[u8]), BinaryDecodeError> {
    if input.len() < 16 {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(16);
    let mut array = [0u8; 16];
    array.copy_from_slice(bytes);
    Ok((u128::from_le_bytes(array), rest))
}

fn take_i128(input: &[u8]) -> Result<(i128, &[u8]), BinaryDecodeError> {
    let (value, rest) = take_u128(input)?;
    Ok((value as i128, rest))
}

fn take_f64(input: &[u8]) -> Result<(f64, &[u8]), BinaryDecodeError> {
    let (value, rest) = take_u64(input)?;
    Ok((f64::from_le_bytes(value.to_le_bytes()), rest))
}

fn take_bytes(input: &[u8]) -> Result<(Vec<u8>, &[u8]), BinaryDecodeError> {
    let (len, input) = take_u32(input)?;
    let len = len as usize;
    if input.len() < len {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(len);
    Ok((bytes.to_vec(), rest))
}

fn take_string(input: &[u8]) -> Result<(String, &[u8]), BinaryDecodeError> {
    let (len, input) = take_u32(input)?;
    let len = len as usize;
    if input.len() < len {
        return Err(BinaryDecodeError::UnexpectedEof);
    }
    let (bytes, rest) = input.split_at(len);
    let value = str::from_utf8(bytes).map_err(|_| BinaryDecodeError::InvalidUtf8)?;
    Ok((value.into(), rest))
}

fn take_optional_string(input: &[u8]) -> Result<(Option<String>, &[u8]), BinaryDecodeError> {
    let (present, input) = take_u8(input)?;
    if present == 1 {
        let (value, rest) = take_string(input)?;
        Ok((Some(value), rest))
    } else {
        Ok((None, input))
    }
}

fn take_optional_u32(input: &[u8]) -> Result<(Option<u32>, &[u8]), BinaryDecodeError> {
    let (present, input) = take_u8(input)?;
    if present == 1 {
        let (value, rest) = take_u32(input)?;
        Ok((Some(value), rest))
    } else {
        Ok((None, input))
    }
}

fn decode_level(code: u8) -> Result<Level, BinaryDecodeError> {
    match code {
        1 => Ok(Level::ERROR),
        2 => Ok(Level::WARN),
        3 => Ok(Level::INFO),
        4 => Ok(Level::DEBUG),
        5 => Ok(Level::TRACE),
        other => Err(BinaryDecodeError::InvalidLevel(other)),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        CallsiteMetadata, EncodeConfig, FieldValue, OwnedField, OwnedRecord, SpanSnapshot,
        encode_binary_metadata, encode_binary_record,
    };

    #[test]
    fn decodes_binary_roundtrip() {
        let metadata = CallsiteMetadata {
            id: 11,
            name: "request",
            target: "app::http",
            level: Level::INFO,
            file: Some("src/main.rs"),
            line: Some(8),
            module_path: Some("app::http"),
            fields: SmallVec::from_slice(&["message", "answer"]),
            kind: CallsiteKind::Event,
        };
        let record = OwnedRecord {
            timestamp: Timestamp::new(10, 20),
            metadata_id: 11,
            name: "request",
            target: "app::http",
            level: Level::INFO,
            fields: SmallVec::from_vec(vec![
                OwnedField {
                    name: "message",
                    value: FieldValue::Str("ok".into()),
                },
                OwnedField {
                    name: "answer",
                    value: FieldValue::U64(42),
                },
            ]),
            current_span: Some(SpanSnapshot {
                id: 77,
                metadata_id: 2,
                name: "span",
                target: "app::http",
                level: Level::INFO,
                fields: SmallVec::new(),
            }),
            spans: SmallVec::new(),
        };

        let mut metadata_frame = Vec::new();
        let mut event_frame = Vec::new();
        encode_binary_metadata(&metadata, &mut metadata_frame);
        encode_binary_record(EncodeConfig::default(), &record, &mut event_frame);

        let mut stream = Vec::new();
        stream.extend_from_slice(&metadata_frame);
        stream.extend_from_slice(&event_frame);

        let (first, rest) = decode_binary_frame(&stream).unwrap();
        let (second, rest) = decode_binary_frame(rest).unwrap();
        assert!(rest.is_empty());

        match first {
            DecodedBinaryFrame::Metadata(frame) => {
                assert_eq!(frame.id, 11);
                assert_eq!(frame.name, "request");
                assert_eq!(frame.target, "app::http");
            }
            _ => panic!("expected metadata frame"),
        }

        match second {
            DecodedBinaryFrame::Event(frame) => {
                assert_eq!(frame.metadata_id, 11);
                assert_eq!(frame.fields.len(), 2);
                assert_eq!(frame.current_span.unwrap().id, 77);
            }
            _ => panic!("expected event frame"),
        }
    }
}
