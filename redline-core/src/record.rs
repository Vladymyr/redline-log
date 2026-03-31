use alloc::{format, string::String, vec::Vec};
use core::{fmt, str};
use smallvec::SmallVec;
use tracing_core::{
    Event, Level,
    field::{Field, Visit},
    span,
};

/// UTF-8 string storage optimized for short values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct InlineString(SmallVec<[u8; 24]>);

impl InlineString {
    /// Returns the value as `&str`.
    pub fn as_str(&self) -> &str {
        str::from_utf8(&self.0).expect("inline string must always contain valid utf-8")
    }
}

impl From<&str> for InlineString {
    fn from(value: &str) -> Self {
        Self(SmallVec::from_slice(value.as_bytes()))
    }
}

impl From<String> for InlineString {
    fn from(value: String) -> Self {
        Self(SmallVec::from_slice(value.as_bytes()))
    }
}

impl fmt::Debug for InlineString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for InlineString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Owned field value captured from a `tracing` event or span.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Bool(bool),
    I64(i64),
    U64(u64),
    I128(i128),
    U128(u128),
    F64(f64),
    Str(InlineString),
    Bytes(Vec<u8>),
    Debug(InlineString),
}

/// One named structured field.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedField {
    pub name: &'static str,
    pub value: FieldValue,
}

/// Small-vector field storage used by records and spans.
pub type OwnedFields = SmallVec<[OwnedField; 8]>;

/// Unix timestamp used by `redline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    /// Whole seconds since the Unix epoch.
    pub unix_seconds: u64,
    /// Nanoseconds within `unix_seconds`.
    pub subsec_nanos: u32,
}

impl Timestamp {
    /// Creates a timestamp from seconds and nanoseconds.
    pub const fn new(unix_seconds: u64, subsec_nanos: u32) -> Self {
        Self {
            unix_seconds,
            subsec_nanos,
        }
    }

    /// Returns the timestamp as Unix nanoseconds.
    pub const fn unix_nanos(&self) -> u128 {
        (self.unix_seconds as u128) * 1_000_000_000 + self.subsec_nanos as u128
    }
}

/// Snapshot of one span included in an encoded record.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanSnapshot {
    pub id: u64,
    pub metadata_id: u32,
    pub name: &'static str,
    pub target: &'static str,
    pub level: Level,
    pub fields: OwnedFields,
}

/// Owned event record ready for encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedRecord {
    pub timestamp: Timestamp,
    pub metadata_id: u32,
    pub name: &'static str,
    pub target: &'static str,
    pub level: Level,
    pub fields: OwnedFields,
    pub current_span: Option<SpanSnapshot>,
    pub spans: SmallVec<[SpanSnapshot; 4]>,
}

impl OwnedRecord {
    /// Captures an event into an owned record.
    pub fn from_event(
        timestamp: Timestamp,
        metadata_id: u32,
        event: &Event<'_>,
        current_span: Option<SpanSnapshot>,
        spans: SmallVec<[SpanSnapshot; 4]>,
    ) -> Self {
        let mut capture = FieldCapture::default();
        event.record(&mut capture);
        let metadata = event.metadata();
        Self {
            timestamp,
            metadata_id,
            name: metadata.name(),
            target: metadata.target(),
            level: *metadata.level(),
            fields: capture.finish(),
            current_span,
            spans,
        }
    }
}

/// `tracing` field visitor that captures owned values.
#[derive(Default)]
pub struct FieldCapture {
    fields: OwnedFields,
}

impl FieldCapture {
    /// Finishes capture and returns the collected fields.
    pub fn finish(self) -> OwnedFields {
        self.fields
    }

    fn push(&mut self, field: &Field, value: FieldValue) {
        self.fields.push(OwnedField {
            name: field.name(),
            value,
        });
    }
}

impl Visit for FieldCapture {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, FieldValue::F64(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, FieldValue::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, FieldValue::U64(value));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.push(field, FieldValue::I128(value));
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.push(field, FieldValue::U128(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, FieldValue::Bool(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, FieldValue::Str(value.into()));
    }

    fn record_bytes(&mut self, field: &Field, value: &[u8]) {
        self.push(field, FieldValue::Bytes(value.to_vec()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, FieldValue::Debug(format!("{value:?}").into()));
    }
}

/// Captures fields from span attributes.
pub fn capture_span_fields(attributes: &span::Attributes<'_>) -> OwnedFields {
    let mut capture = FieldCapture::default();
    attributes.record(&mut capture);
    capture.finish()
}

/// Captures fields from a span record update.
pub fn capture_record_fields(values: &span::Record<'_>) -> OwnedFields {
    let mut capture = FieldCapture::default();
    values.record(&mut capture);
    capture.finish()
}

/// Applies field updates, replacing existing values with the same name.
pub fn merge_fields(existing: &mut OwnedFields, updates: OwnedFields) {
    for update in updates {
        if let Some(slot) = existing.iter_mut().find(|field| field.name == update.name) {
            slot.value = update.value;
        } else {
            existing.push(update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    #[test]
    fn merge_fields_overwrites_existing_keys() {
        let mut fields = smallvec![OwnedField {
            name: "message",
            value: FieldValue::Str("before".into()),
        }];
        merge_fields(
            &mut fields,
            smallvec![OwnedField {
                name: "message",
                value: FieldValue::Str("after".into()),
            }],
        );

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value, FieldValue::Str("after".into()));
    }
}
