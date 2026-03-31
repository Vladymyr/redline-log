//! Core types for `redline`.
//!
//! This crate contains filter parsing, record capture, NDJSON encoding,
//! binary framing, and binary decoding. It can be used directly by custom
//! integrations that do not need the `std` subscriber crate.

#![no_std]

extern crate alloc;

mod decode;
mod encode;
mod filter;
mod record;

pub use decode::{
    BinaryDecodeError, DecodedBinaryFrame, DecodedCallsiteMetadata, DecodedField, DecodedRecord,
    DecodedSpanSnapshot, decode_binary_frame,
};
pub use encode::{
    BinaryFrameKind, CallsiteKind, CallsiteMetadata, EncodeConfig, OutputFormat,
    encode_binary_metadata, encode_binary_record, encode_ndjson_record,
};
pub use filter::{Directive, FilterParseError, TargetFilter};
pub use record::{
    FieldCapture, FieldValue, InlineString, OwnedField, OwnedFields, OwnedRecord, SpanSnapshot, Timestamp,
    capture_record_fields, capture_span_fields, merge_fields,
};

#[cfg(test)]
extern crate std;
