use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::RwLock;
use redline_core::{
    OwnedFields, OwnedRecord, SpanSnapshot, capture_record_fields, capture_span_fields,
    merge_fields,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use tracing_core::{
    Event, Metadata,
    metadata::LevelFilter,
    span::{self, Current, Id},
    subscriber::{Interest, Subscriber},
};

use crate::{config::Handle, pipeline::RedlinePipeline};

thread_local! {
    static CURRENT_SPANS: RefCell<SmallVec<[u64; 8]>> = RefCell::new(SmallVec::new());
}

pub struct RedlineSubscriber {
    pipeline: RedlinePipeline,
    spans: RwLock<SpanStore>,
    next_span_id: AtomicU64,
}

impl RedlineSubscriber {
    pub(crate) fn new(pipeline: RedlinePipeline) -> Self {
        Self {
            pipeline,
            spans: RwLock::new(SpanStore::default()),
            next_span_id: AtomicU64::new(1),
        }
    }

    pub fn builder() -> crate::Builder {
        crate::Builder::new()
    }

    pub fn handle(&self) -> Handle {
        self.pipeline.handle()
    }

    fn next_id(&self) -> u64 {
        self.next_span_id.fetch_add(1, Ordering::Relaxed)
    }

    fn current_span_id() -> Option<u64> {
        CURRENT_SPANS.with(|stack| stack.borrow().last().copied())
    }

    fn resolve_parent(attrs_parent: Option<&Id>, contextual: bool) -> Option<u64> {
        attrs_parent
            .map(Id::into_u64)
            .or_else(|| contextual.then(Self::current_span_id).flatten())
    }

    fn build_span_context(
        &self,
        current_span_id: Option<u64>,
    ) -> (Option<SpanSnapshot>, SmallVec<[SpanSnapshot; 4]>) {
        let encode = self.pipeline.encode_config();
        if !encode.include_current_span && !encode.include_span_list {
            return (None, SmallVec::new());
        }

        let Some(current_span_id) = current_span_id else {
            return (None, SmallVec::new());
        };

        if encode.include_current_span && !encode.include_span_list {
            let spans = self.spans.read();
            let current = spans
                .entries
                .get(&current_span_id)
                .map(|span_data| span_data.snapshot(current_span_id));
            return (current, SmallVec::new());
        }

        let spans = self.spans.read();
        let mut chain = SmallVec::new();
        let mut cursor = Some(current_span_id);

        while let Some(id) = cursor {
            let Some(span) = spans.entries.get(&id) else {
                break;
            };
            chain.push(span.snapshot(id));
            cursor = span.parent;
        }
        chain.reverse();

        let current = if encode.include_current_span {
            chain.last().cloned()
        } else {
            None
        };

        let all = if encode.include_span_list {
            chain
        } else {
            SmallVec::new()
        };

        (current, all)
    }

    fn event_parent(event: &Event<'_>) -> Option<u64> {
        if event.is_root() {
            None
        } else if let Some(parent) = event.parent() {
            Some(parent.into_u64())
        } else if event.is_contextual() {
            Self::current_span_id()
        } else {
            None
        }
    }
}

impl Subscriber for RedlineSubscriber {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        self.pipeline.register_callsite(metadata)
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.pipeline.enabled(metadata)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.pipeline.max_level_hint())
    }

    fn new_span(&self, attrs: &span::Attributes<'_>) -> Id {
        let id = self.next_id();
        let metadata = attrs.metadata();
        let metadata_id = self.pipeline.metadata_id(metadata);
        let parent = Self::resolve_parent(attrs.parent(), attrs.is_contextual());
        let fields = capture_span_fields(attrs);

        let span_data = SpanData {
            metadata,
            metadata_id,
            parent,
            fields,
            refs: 1,
        };
        self.spans.write().entries.insert(id, span_data);
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &span::Record<'_>) {
        let mut spans = self.spans.write();
        if let Some(data) = spans.entries.get_mut(&span.into_u64()) {
            merge_fields(&mut data.fields, capture_record_fields(values));
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event_enabled(&self, event: &Event<'_>) -> bool {
        self.pipeline.enabled(event.metadata())
    }

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let metadata_id = self.pipeline.metadata_id(metadata);
        let parent = Self::event_parent(event);
        let (current_span, spans) = self.build_span_context(parent);
        let record = OwnedRecord::from_event(
            RedlinePipeline::timestamp_now(),
            metadata_id,
            event,
            current_span,
            spans,
        );

        self.pipeline.emit_record(&record);
    }

    fn enter(&self, span: &Id) {
        let id = span.into_u64();
        CURRENT_SPANS.with(|stack| stack.borrow_mut().push(id));
    }

    fn exit(&self, span: &Id) {
        let id = span.into_u64();
        CURRENT_SPANS.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.last().copied() == Some(id) {
                stack.pop();
                return;
            }
            if let Some(index) = stack.iter().rposition(|candidate| *candidate == id) {
                stack.remove(index);
            }
        });
    }

    fn clone_span(&self, id: &Id) -> Id {
        if let Some(data) = self.spans.write().entries.get_mut(&id.into_u64()) {
            data.refs += 1;
        }
        id.clone()
    }

    fn try_close(&self, id: Id) -> bool {
        let mut spans = self.spans.write();
        let key = id.into_u64();
        if let Some(data) = spans.entries.get_mut(&key)
            && data.refs > 1
        {
            data.refs -= 1;
            return false;
        }
        spans.entries.remove(&key).is_some()
    }

    fn current_span(&self) -> Current {
        let Some(id) = Self::current_span_id() else {
            return Current::none();
        };
        let spans = self.spans.read();
        let Some(span) = spans.entries.get(&id) else {
            return Current::none();
        };
        Current::new(Id::from_u64(id), span.metadata)
    }
}

#[derive(Default)]
struct SpanStore {
    entries: FxHashMap<u64, SpanData>,
}

struct SpanData {
    metadata: &'static Metadata<'static>,
    metadata_id: u32,
    parent: Option<u64>,
    fields: OwnedFields,
    refs: usize,
}

impl SpanData {
    fn snapshot(&self, id: u64) -> SpanSnapshot {
        SpanSnapshot {
            id,
            metadata_id: self.metadata_id,
            name: self.metadata.name(),
            target: self.metadata.target(),
            level: *self.metadata.level(),
            fields: self.fields.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::RedlineSubscriber;
    use crate::{Builder, OutputFormat, Sink};
    use tracing::{info, info_span};

    fn temp_path(suffix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("redline-{nanos}-{suffix}.log"))
    }

    #[test]
    fn ndjson_writes_event_with_span_context() {
        let path = temp_path("ndjson");
        let subscriber = Builder::new()
            .sink(Sink::file(&path))
            .include_span_list(true)
            .build()
            .unwrap();
        let handle = subscriber.handle();

        tracing::subscriber::with_default(subscriber, || {
            let span = info_span!("request", request_id = 7u64);
            let _guard = span.enter();
            info!(target: "app::api", answer = 42u64, message = "hello");
        });

        handle.flush().unwrap();
        let output = fs::read_to_string(&path).unwrap();
        assert!(output.contains(r#""target":"app::api""#));
        assert!(output.contains(r#""answer":42"#));
        assert!(output.contains(r#""spans":["#));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn binary_writes_metadata_then_event_frames() {
        let path = temp_path("binary");
        let subscriber = Builder::new()
            .sink(Sink::file(&path))
            .format(OutputFormat::Binary)
            .build()
            .unwrap();
        let handle = subscriber.handle();

        tracing::subscriber::with_default(subscriber, || {
            info!(target: "app::binary", message = "hello");
        });

        handle.flush().unwrap();
        let output = fs::read(&path).unwrap();
        assert_eq!(output[0], 1);
        assert!(output.iter().skip(5).any(|byte| *byte == 2));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn builder_helper_is_exposed() {
        let _ = RedlineSubscriber::builder();
    }
}
