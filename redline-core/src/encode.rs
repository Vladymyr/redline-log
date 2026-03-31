use alloc::vec::Vec;
use smallvec::SmallVec;
use tracing_core::{Level, Metadata};

use crate::record::{FieldValue, OwnedField, OwnedRecord, SpanSnapshot, Timestamp};

/// Output format used by `redline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// Newline-delimited JSON.
    Ndjson,
    /// Compact binary frames for lower write volume and later decoding.
    Binary,
}

/// Controls which span context is included during encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeConfig {
    /// Include the current span object.
    pub include_current_span: bool,
    /// Include the full entered span chain.
    pub include_span_list: bool,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            include_current_span: true,
            include_span_list: false,
        }
    }
}

/// Distinguishes event and span callsites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallsiteKind {
    Event,
    Span,
}

/// Static metadata sent once per callsite in binary mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallsiteMetadata {
    pub id: u32,
    pub name: &'static str,
    pub target: &'static str,
    pub level: Level,
    pub file: Option<&'static str>,
    pub line: Option<u32>,
    pub module_path: Option<&'static str>,
    pub fields: SmallVec<[&'static str; 8]>,
    pub kind: CallsiteKind,
}

impl CallsiteMetadata {
    /// Builds callsite metadata from `tracing` metadata.
    pub fn from_metadata(id: u32, metadata: &'static Metadata<'static>) -> Self {
        let mut fields = SmallVec::new();
        for field in metadata.fields().iter() {
            fields.push(field.name());
        }

        Self {
            id,
            name: metadata.name(),
            target: metadata.target(),
            level: *metadata.level(),
            file: metadata.file(),
            line: metadata.line(),
            module_path: metadata.module_path(),
            fields,
            kind: if metadata.is_span() {
                CallsiteKind::Span
            } else {
                CallsiteKind::Event
            },
        }
    }
}

/// Binary frame type identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryFrameKind {
    Metadata = 1,
    Event = 2,
}

const KEY_TIMESTAMP_UNIX_NS: &[u8] = br#""timestamp_unix_ns":"#;
const KEY_LEVEL: &[u8] = br#""level":"#;
const KEY_TARGET: &[u8] = br#""target":"#;
const KEY_NAME: &[u8] = br#""name":"#;
const KEY_METADATA_ID: &[u8] = br#""metadata_id":"#;
const KEY_FIELDS: &[u8] = br#""fields":"#;
const KEY_SPAN: &[u8] = br#""span":"#;
const KEY_SPANS: &[u8] = br#""spans":"#;
const KEY_ID: &[u8] = br#""id":"#;

/// Encodes one owned record as NDJSON.
pub fn encode_ndjson_record(config: EncodeConfig, record: &OwnedRecord, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(estimate_ndjson_record_len(config, record));
    out.push(b'{');
    out.extend_from_slice(KEY_TIMESTAMP_UNIX_NS);
    push_timestamp_unix_nanos(out, record.timestamp);
    out.push(b',');
    out.extend_from_slice(KEY_LEVEL);
    push_json_string(out, level_str(record.level));
    out.push(b',');
    out.extend_from_slice(KEY_TARGET);
    push_json_string(out, record.target);
    out.push(b',');
    out.extend_from_slice(KEY_NAME);
    push_json_string(out, record.name);
    out.push(b',');
    out.extend_from_slice(KEY_METADATA_ID);
    push_json_u32(out, record.metadata_id);
    out.push(b',');
    out.extend_from_slice(KEY_FIELDS);
    push_fields_object(out, &record.fields);

    if config.include_current_span {
        if let Some(span) = &record.current_span {
            out.push(b',');
            out.extend_from_slice(KEY_SPAN);
            push_span(out, span);
        }
    }

    if config.include_span_list {
        out.push(b',');
        out.extend_from_slice(KEY_SPANS);
        push_spans(out, &record.spans);
    }

    out.extend_from_slice(b"}\n");
}

/// Encodes one callsite metadata frame for binary output.
pub fn encode_binary_metadata(metadata: &CallsiteMetadata, out: &mut Vec<u8>) {
    out.clear();
    out.push(BinaryFrameKind::Metadata as u8);
    let len_offset = reserve_length(out);
    push_u32_le(out, metadata.id);
    out.push(match metadata.kind {
        CallsiteKind::Event => 1,
        CallsiteKind::Span => 2,
    });
    out.push(level_code(metadata.level));
    push_optional_string(out, metadata.file);
    push_optional_u32(out, metadata.line);
    push_optional_string(out, metadata.module_path);
    push_string(out, metadata.name);
    push_string(out, metadata.target);
    push_u16_le(out, metadata.fields.len() as u16);
    for field in &metadata.fields {
        push_string(out, field);
    }
    write_length(out, len_offset);
}

/// Encodes one owned record as a binary event frame.
pub fn encode_binary_record(config: EncodeConfig, record: &OwnedRecord, out: &mut Vec<u8>) {
    out.clear();
    out.push(BinaryFrameKind::Event as u8);
    let len_offset = reserve_length(out);
    push_u64_le(out, record.timestamp.unix_seconds);
    push_u32_le(out, record.timestamp.subsec_nanos);
    push_u32_le(out, record.metadata_id);
    push_u64_le(
        out,
        record
            .current_span
            .as_ref()
            .map(|span| span.id)
            .unwrap_or_default(),
    );
    push_fields(out, &record.fields);

    if config.include_current_span {
        match &record.current_span {
            Some(span) => {
                out.push(1);
                push_span_binary(out, span);
            }
            None => out.push(0),
        }
    } else {
        out.push(0);
    }

    if config.include_span_list {
        push_u16_le(out, record.spans.len() as u16);
        for span in &record.spans {
            push_span_binary(out, span);
        }
    } else {
        push_u16_le(out, 0);
    }

    write_length(out, len_offset);
}

fn push_span(out: &mut Vec<u8>, span: &SpanSnapshot) {
    out.push(b'{');
    out.extend_from_slice(KEY_ID);
    push_json_u64(out, span.id);
    out.push(b',');
    out.extend_from_slice(KEY_METADATA_ID);
    push_json_u32(out, span.metadata_id);
    out.push(b',');
    out.extend_from_slice(KEY_NAME);
    push_json_string(out, span.name);
    out.push(b',');
    out.extend_from_slice(KEY_TARGET);
    push_json_string(out, span.target);
    out.push(b',');
    out.extend_from_slice(KEY_LEVEL);
    push_json_string(out, level_str(span.level));
    out.push(b',');
    out.extend_from_slice(KEY_FIELDS);
    push_fields_object(out, &span.fields);
    out.push(b'}');
}

fn push_spans(out: &mut Vec<u8>, spans: &[SpanSnapshot]) {
    out.push(b'[');
    for (index, span) in spans.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        push_span(out, span);
    }
    out.push(b']');
}

fn push_fields_object(out: &mut Vec<u8>, fields: &[OwnedField]) {
    out.push(b'{');
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        push_json_key(out, field.name);
        push_field_value(out, &field.value);
    }
    out.push(b'}');
}

fn push_field_value(out: &mut Vec<u8>, value: &FieldValue) {
    match value {
        FieldValue::Bool(value) => push_bool(out, *value),
        FieldValue::I64(value) => push_i64(out, *value),
        FieldValue::U64(value) => push_json_u64(out, *value),
        FieldValue::I128(value) => push_i128(out, *value),
        FieldValue::U128(value) => push_u128(out, *value),
        FieldValue::F64(value) => push_f64(out, *value),
        FieldValue::Str(value) | FieldValue::Debug(value) => push_json_string(out, value.as_str()),
        FieldValue::Bytes(value) => push_json_hex_bytes(out, value),
    }
}

fn push_json_key(out: &mut Vec<u8>, key: &str) {
    push_json_string(out, key);
    out.push(b':');
}

fn push_json_string(out: &mut Vec<u8>, value: &str) {
    out.push(b'"');
    let bytes = value.as_bytes();
    if bytes.iter().all(|byte| !needs_json_escape(*byte)) {
        out.extend_from_slice(bytes);
        out.push(b'"');
        return;
    }

    let mut start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if !needs_json_escape(byte) {
            continue;
        }

        if start < index {
            out.extend_from_slice(&bytes[start..index]);
        }

        match byte {
            b'"' => out.extend_from_slice(br#"\""#),
            b'\\' => out.extend_from_slice(br#"\\"#),
            b'\n' => out.extend_from_slice(br#"\n"#),
            b'\r' => out.extend_from_slice(br#"\r"#),
            b'\t' => out.extend_from_slice(br#"\t"#),
            0x00..=0x1f => {
                out.extend_from_slice(br#"\u00"#);
                let high = byte >> 4;
                let low = byte & 0x0f;
                out.push(hex_digit(high));
                out.push(hex_digit(low));
            }
            _ => out.push(byte),
        }
        start = index + 1;
    }
    if start < bytes.len() {
        out.extend_from_slice(&bytes[start..]);
    }
    out.push(b'"');
}

fn push_json_hex_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out.push(b'"');
}

fn hex_digit(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        _ => b'a' + (value - 10),
    }
}

fn push_bool(out: &mut Vec<u8>, value: bool) {
    out.extend_from_slice(if value { b"true" } else { b"false" });
}

fn push_i64(out: &mut Vec<u8>, value: i64) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_i128(out: &mut Vec<u8>, value: i128) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_json_u32(out: &mut Vec<u8>, value: u32) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_json_u64(out: &mut Vec<u8>, value: u64) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_u128(out: &mut Vec<u8>, value: u128) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_f64(out: &mut Vec<u8>, value: f64) {
    let mut buffer = ryu::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_timestamp_unix_nanos(out: &mut Vec<u8>, timestamp: Timestamp) {
    push_json_u64(out, timestamp.unix_seconds);

    let mut nanos = timestamp.subsec_nanos;
    let mut digits = [b'0'; 9];
    for index in (0..9).rev() {
        digits[index] = b'0' + (nanos % 10) as u8;
        nanos /= 10;
    }
    out.extend_from_slice(&digits);
}

fn level_str(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

fn level_code(level: Level) -> u8 {
    match level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

fn reserve_length(out: &mut Vec<u8>) -> usize {
    let offset = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    offset
}

fn write_length(out: &mut Vec<u8>, len_offset: usize) {
    let len = (out.len() - len_offset - 4) as u32;
    out[len_offset..len_offset + 4].copy_from_slice(&len.to_le_bytes());
}

fn push_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            push_string(out, value);
        }
        None => out.push(0),
    }
}

fn push_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            out.push(1);
            push_u32_le(out, value);
        }
        None => out.push(0),
    }
}

fn push_string(out: &mut Vec<u8>, value: &str) {
    push_u32_le(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn push_fields(out: &mut Vec<u8>, fields: &[OwnedField]) {
    push_u16_le(out, fields.len() as u16);
    for field in fields {
        push_string(out, field.name);
        push_field_binary(out, &field.value);
    }
}

fn push_span_binary(out: &mut Vec<u8>, span: &SpanSnapshot) {
    push_u64_le(out, span.id);
    push_u32_le(out, span.metadata_id);
    out.push(level_code(span.level));
    push_string(out, span.name);
    push_string(out, span.target);
    push_fields(out, &span.fields);
}

fn push_field_binary(out: &mut Vec<u8>, value: &FieldValue) {
    match value {
        FieldValue::Bool(false) => out.push(1),
        FieldValue::Bool(true) => out.push(2),
        FieldValue::I64(value) => {
            out.push(3);
            out.extend_from_slice(&value.to_le_bytes());
        }
        FieldValue::U64(value) => {
            out.push(4);
            out.extend_from_slice(&value.to_le_bytes());
        }
        FieldValue::I128(value) => {
            out.push(5);
            out.extend_from_slice(&value.to_le_bytes());
        }
        FieldValue::U128(value) => {
            out.push(6);
            out.extend_from_slice(&value.to_le_bytes());
        }
        FieldValue::F64(value) => {
            out.push(7);
            out.extend_from_slice(&value.to_le_bytes());
        }
        FieldValue::Str(value) => {
            out.push(8);
            push_string(out, value.as_str());
        }
        FieldValue::Debug(value) => {
            out.push(9);
            push_string(out, value.as_str());
        }
        FieldValue::Bytes(value) => {
            out.push(10);
            push_u32_le(out, value.len() as u32);
            out.extend_from_slice(value);
        }
    }
}

fn push_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64_le(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn estimate_ndjson_record_len(config: EncodeConfig, record: &OwnedRecord) -> usize {
    let mut estimate = 96 + record.target.len() + record.name.len();
    estimate += estimate_fields_len(&record.fields);

    if config.include_current_span {
        if let Some(span) = &record.current_span {
            estimate += 64 + span.target.len() + span.name.len();
            estimate += estimate_fields_len(&span.fields);
        }
    }

    if config.include_span_list {
        for span in &record.spans {
            estimate += 64 + span.target.len() + span.name.len();
            estimate += estimate_fields_len(&span.fields);
        }
    }

    estimate
}

fn estimate_fields_len(fields: &[OwnedField]) -> usize {
    fields
        .iter()
        .map(|field| field.name.len() + estimate_field_value_len(&field.value) + 8)
        .sum()
}

fn estimate_field_value_len(value: &FieldValue) -> usize {
    match value {
        FieldValue::Bool(_) => 5,
        FieldValue::I64(_) | FieldValue::U64(_) => 24,
        FieldValue::I128(_) | FieldValue::U128(_) => 40,
        FieldValue::F64(_) => 32,
        FieldValue::Str(value) | FieldValue::Debug(value) => value.as_str().len() + 2,
        FieldValue::Bytes(value) => value.len() * 2 + 2,
    }
}

fn needs_json_escape(byte: u8) -> bool {
    matches!(byte, b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x00..=0x1f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use crate::record::{FieldValue, OwnedField, OwnedRecord, Timestamp};
    use smallvec::smallvec;

    #[test]
    fn ndjson_escapes_control_characters() {
        let record = OwnedRecord {
            timestamp: Timestamp::new(1, 2),
            metadata_id: 7,
            name: "event",
            target: "app",
            level: Level::INFO,
            fields: smallvec![OwnedField {
                name: "message",
                value: FieldValue::Str("line\nbreak".into()),
            }],
            current_span: None,
            spans: SmallVec::new(),
        };

        let mut out = Vec::new();
        encode_ndjson_record(EncodeConfig::default(), &record, &mut out);

        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains(r#""message":"line\nbreak""#));
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn binary_frame_prefixes_length() {
        let mut fields = SmallVec::new();
        fields.push("message");
        let metadata = CallsiteMetadata {
            id: 1,
            name: "event",
            target: "app",
            level: Level::INFO,
            file: None,
            line: None,
            module_path: None,
            fields,
            kind: CallsiteKind::Event,
        };

        let mut out = Vec::new();
        encode_binary_metadata(&metadata, &mut out);
        assert_eq!(out[0], BinaryFrameKind::Metadata as u8);
        let length = u32::from_le_bytes([out[1], out[2], out[3], out[4]]) as usize;
        assert_eq!(length + 5, out.len());
    }
}
