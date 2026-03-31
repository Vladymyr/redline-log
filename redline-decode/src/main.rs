use std::{
    collections::HashMap,
    env, fmt, fs,
    io::{self, Read, Write},
    path::Path,
    process::ExitCode,
};

use redline_core::{
    BinaryDecodeError, DecodedBinaryFrame, DecodedCallsiteMetadata, DecodedField, DecodedRecord,
    DecodedSpanSnapshot, FieldValue, decode_binary_frame,
};
use tracing_core::Level;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), CliError> {
    let mut args = env::args().skip(1);
    let Some(input_path) = args.next() else {
        return Err(CliError::Usage);
    };
    let output_path = args.next();
    if args.next().is_some() {
        return Err(CliError::Usage);
    }

    let input = read_input(&input_path)?;
    let ndjson = decode_bytes_to_ndjson(&input)?;
    write_output(output_path.as_deref(), &ndjson)?;
    Ok(())
}

fn decode_bytes_to_ndjson(input: &[u8]) -> Result<Vec<u8>, CliError> {
    let mut metadata = HashMap::new();
    let mut out = Vec::new();
    let mut cursor = input;

    while !cursor.is_empty() {
        let (frame, rest) = decode_binary_frame(cursor).map_err(CliError::Decode)?;
        cursor = rest;
        match frame {
            DecodedBinaryFrame::Metadata(frame) => {
                metadata.insert(frame.id, frame);
            }
            DecodedBinaryFrame::Event(frame) => {
                let callsite = metadata
                    .get(&frame.metadata_id)
                    .ok_or(CliError::UnknownMetadata(frame.metadata_id))?;
                write_event_ndjson(&mut out, callsite, &frame);
            }
        }
    }

    Ok(out)
}

fn write_event_ndjson(
    out: &mut Vec<u8>,
    callsite: &DecodedCallsiteMetadata,
    record: &DecodedRecord,
) {
    out.push(b'{');
    push_json_key(out, "timestamp_unix_ns");
    push_u128(out, record.timestamp.unix_nanos());
    out.push(b',');
    push_json_key(out, "level");
    push_json_string(out, level_str(callsite.level));
    out.push(b',');
    push_json_key(out, "target");
    push_json_string(out, &callsite.target);
    out.push(b',');
    push_json_key(out, "name");
    push_json_string(out, &callsite.name);
    out.push(b',');
    push_json_key(out, "metadata_id");
    push_u32(out, record.metadata_id);
    out.push(b',');
    push_json_key(out, "fields");
    push_fields(out, &record.fields);

    if let Some(span) = &record.current_span {
        out.push(b',');
        push_json_key(out, "span");
        push_span(out, span);
    }

    if !record.spans.is_empty() {
        out.push(b',');
        push_json_key(out, "spans");
        out.push(b'[');
        for (index, span) in record.spans.iter().enumerate() {
            if index > 0 {
                out.push(b',');
            }
            push_span(out, span);
        }
        out.push(b']');
    }

    out.extend_from_slice(b"}\n");
}

fn push_span(out: &mut Vec<u8>, span: &DecodedSpanSnapshot) {
    out.push(b'{');
    push_json_key(out, "id");
    push_u64(out, span.id);
    out.push(b',');
    push_json_key(out, "metadata_id");
    push_u32(out, span.metadata_id);
    out.push(b',');
    push_json_key(out, "name");
    push_json_string(out, &span.name);
    out.push(b',');
    push_json_key(out, "target");
    push_json_string(out, &span.target);
    out.push(b',');
    push_json_key(out, "level");
    push_json_string(out, level_str(span.level));
    out.push(b',');
    push_json_key(out, "fields");
    push_fields(out, &span.fields);
    out.push(b'}');
}

fn push_fields(out: &mut Vec<u8>, fields: &[DecodedField]) {
    out.push(b'{');
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        push_json_key(out, &field.name);
        push_field_value(out, &field.value);
    }
    out.push(b'}');
}

fn push_field_value(out: &mut Vec<u8>, value: &FieldValue) {
    match value {
        FieldValue::Bool(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
        FieldValue::I64(value) => push_i64(out, *value),
        FieldValue::U64(value) => push_u64(out, *value),
        FieldValue::I128(value) => push_i128(out, *value),
        FieldValue::U128(value) => push_u128(out, *value),
        FieldValue::F64(value) => push_f64(out, *value),
        FieldValue::Str(value) | FieldValue::Debug(value) => push_json_string(out, value.as_str()),
        FieldValue::Bytes(value) => push_hex_bytes(out, value),
    }
}

fn read_input(path: &str) -> Result<Vec<u8>, CliError> {
    if path == "-" {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input)?;
        Ok(input)
    } else {
        Ok(fs::read(Path::new(path))?)
    }
}

fn write_output(path: Option<&str>, bytes: &[u8]) -> Result<(), CliError> {
    match path {
        Some("-") | None => {
            io::stdout().write_all(bytes)?;
            Ok(())
        }
        Some(path) => {
            fs::write(Path::new(path), bytes)?;
            Ok(())
        }
    }
}

fn push_json_key(out: &mut Vec<u8>, key: &str) {
    push_json_string(out, key);
    out.push(b':');
}

fn push_json_string(out: &mut Vec<u8>, value: &str) {
    out.push(b'"');
    for byte in value.bytes() {
        match byte {
            b'"' => out.extend_from_slice(br#"\""#),
            b'\\' => out.extend_from_slice(br#"\\"#),
            b'\n' => out.extend_from_slice(br#"\n"#),
            b'\r' => out.extend_from_slice(br#"\r"#),
            b'\t' => out.extend_from_slice(br#"\t"#),
            0x00..=0x1f => {
                out.extend_from_slice(br#"\u00"#);
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0x0f));
            }
            _ => out.push(byte),
        }
    }
    out.push(b'"');
}

fn push_hex_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
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

fn push_i64(out: &mut Vec<u8>, value: i64) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn push_u128(out: &mut Vec<u8>, value: u128) {
    out.extend_from_slice(value.to_string().as_bytes());
}

fn push_i128(out: &mut Vec<u8>, value: i128) {
    out.extend_from_slice(value.to_string().as_bytes());
}

fn push_f64(out: &mut Vec<u8>, value: f64) {
    let mut buffer = ryu::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
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

#[derive(Debug)]
enum CliError {
    Usage,
    Io(io::Error),
    Decode(BinaryDecodeError),
    UnknownMetadata(u32),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("usage: redline-decode <input.bin|-> [output.ndjson|-]"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Decode(error) => write!(f, "{error}"),
            Self::UnknownMetadata(id) => {
                write!(f, "event frame referenced unknown metadata id `{id}`")
            }
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use redline_core::{
        CallsiteKind, CallsiteMetadata, EncodeConfig, OwnedField, OwnedRecord, SpanSnapshot,
        Timestamp, encode_binary_metadata, encode_binary_record,
    };
    use tracing_core::Level;

    use super::*;

    #[test]
    fn renders_binary_stream_as_ndjson() {
        let metadata = CallsiteMetadata {
            id: 3,
            name: "event",
            target: "app::decode",
            level: Level::INFO,
            file: None,
            line: None,
            module_path: None,
            fields: ["message"].into_iter().collect(),
            kind: CallsiteKind::Event,
        };
        let record = OwnedRecord {
            timestamp: Timestamp::new(1, 2),
            metadata_id: 3,
            name: "event",
            target: "app::decode",
            level: Level::INFO,
            fields: vec![OwnedField {
                name: "message",
                value: FieldValue::Str("hello".into()),
            }]
            .into_iter()
            .collect(),
            current_span: Some(SpanSnapshot {
                id: 9,
                metadata_id: 4,
                name: "span",
                target: "app::decode",
                level: Level::INFO,
                fields: Default::default(),
            }),
            spans: Default::default(),
        };

        let mut bytes = Vec::new();
        let mut metadata_frame = Vec::new();
        let mut event_frame = Vec::new();
        encode_binary_metadata(&metadata, &mut metadata_frame);
        encode_binary_record(EncodeConfig::default(), &record, &mut event_frame);
        bytes.extend_from_slice(&metadata_frame);
        bytes.extend_from_slice(&event_frame);

        let ndjson = decode_bytes_to_ndjson(&bytes).unwrap();
        let rendered = String::from_utf8(ndjson).unwrap();
        assert!(rendered.contains(r#""target":"app::decode""#));
        assert!(rendered.contains(r#""message":"hello""#));
        assert!(rendered.contains(r#""span":{"id":9"#));
    }
}
