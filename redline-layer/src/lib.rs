//! `tracing-subscriber` layer for `redline`.
//!
//! # Example
//!
//! ```no_run
//! use redline::Sink;
//! use redline_layer::Builder;
//! use tracing::info;
//! use tracing_subscriber::{prelude::*, registry};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let layer = Builder::new().sink(Sink::Stdout).build()?;
//!     let handle = layer.handle();
//!     let subscriber = registry().with(layer);
//!
//!     tracing::subscriber::with_default(subscriber, || {
//!         info!(target: "app", message = "started");
//!     });
//!
//!     handle.flush()?;
//!     Ok(())
//! }
//! ```

use std::io;

use redline::{FilterParseError, Handle, OutputFormat, RedlinePipeline, Sink, TargetFilter};
use redline_core::{
    OwnedFields, OwnedRecord, SpanSnapshot, capture_record_fields, capture_span_fields,
    merge_fields,
};
use smallvec::SmallVec;
use tracing::{Event, Metadata, Subscriber, span};
use tracing_subscriber::{
    layer::{Context, Layer},
    registry::{LookupSpan, SpanRef},
};

/// Configures and builds a [`RedlineLayer`].
pub struct Builder {
    inner: redline::Builder,
}

impl Builder {
    /// Creates a builder with the same defaults as `redline::Builder`.
    pub fn new() -> Self {
        Self {
            inner: redline::Builder::new(),
        }
    }

    /// Sets a prebuilt target filter.
    pub fn filter(mut self, filter: TargetFilter) -> Self {
        self.inner = self.inner.filter(filter);
        self
    }

    /// Parses and sets a target filter spec such as `info,app::db=debug`.
    pub fn filter_spec(mut self, spec: &str) -> Result<Self, FilterParseError> {
        self.inner = self.inner.filter_spec(spec)?;
        Ok(self)
    }

    /// Selects the output format.
    pub fn format(mut self, format: OutputFormat) -> Self {
        self.inner = self.inner.format(format);
        self
    }

    /// Includes the current span in encoded events.
    pub fn include_current_span(mut self, enabled: bool) -> Self {
        self.inner = self.inner.include_current_span(enabled);
        self
    }

    /// Includes the full entered span chain in encoded events.
    pub fn include_span_list(mut self, enabled: bool) -> Self {
        self.inner = self.inner.include_span_list(enabled);
        self
    }

    /// Sets the output sink.
    pub fn sink(mut self, sink: Sink) -> Self {
        self.inner = self.inner.sink(sink);
        self
    }

    /// Sets the bounded queue capacity.
    pub fn queue_capacity(mut self, queue_capacity: usize) -> Self {
        self.inner = self.inner.queue_capacity(queue_capacity);
        self
    }

    /// Sets the initial size of pooled frame buffers.
    pub fn frame_buffer_size(mut self, frame_buffer_size: usize) -> Self {
        self.inner = self.inner.frame_buffer_size(frame_buffer_size);
        self
    }

    /// Sets the number of retained pooled frame buffers.
    pub fn frame_buffer_count(mut self, frame_buffer_count: usize) -> Self {
        self.inner = self.inner.frame_buffer_count(frame_buffer_count);
        self
    }

    /// Builds a `tracing-subscriber` layer.
    pub fn build(self) -> io::Result<RedlineLayer> {
        Ok(RedlineLayer {
            pipeline: self.inner.build_pipeline()?,
        })
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// `tracing-subscriber` layer that writes events through `redline`.
pub struct RedlineLayer {
    pipeline: RedlinePipeline,
}

impl RedlineLayer {
    /// Creates a builder.
    pub fn builder() -> Builder {
        Builder::new()
    }

    /// Returns a handle for flushing and reading counters.
    pub fn handle(&self) -> Handle {
        self.pipeline.handle()
    }

    fn build_span_context<S>(
        &self,
        event: &Event<'_>,
        ctx: Context<'_, S>,
    ) -> (Option<SpanSnapshot>, SmallVec<[SpanSnapshot; 4]>)
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        let encode = self.pipeline.encode_config();
        if !encode.include_current_span && !encode.include_span_list {
            return (None, SmallVec::new());
        }

        let Some(scope) = ctx.event_scope(event) else {
            return (None, SmallVec::new());
        };

        let mut chain = SmallVec::new();
        for span in scope.from_root() {
            if self.pipeline.enabled(span.metadata()) {
                chain.push(self.snapshot_span(&span));
            }
        }

        let current = if encode.include_current_span {
            chain.last().cloned()
        } else {
            None
        };

        let spans = if encode.include_span_list {
            chain
        } else {
            SmallVec::new()
        };

        (current, spans)
    }

    fn snapshot_span<S>(&self, span: &SpanRef<'_, S>) -> SpanSnapshot
    where
        S: for<'lookup> LookupSpan<'lookup>,
    {
        let metadata = span.metadata();
        let fields = span
            .extensions()
            .get::<SpanState>()
            .map(|data| data.fields.clone())
            .unwrap_or_default();

        SpanSnapshot {
            id: span.id().into_u64(),
            metadata_id: self.pipeline.metadata_id(metadata),
            name: metadata.name(),
            target: metadata.target(),
            level: *metadata.level(),
            fields,
        }
    }
}

impl<S> Layer<S> for RedlineLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing_core::subscriber::Interest {
        if self.pipeline.enabled(metadata) {
            self.pipeline.prepare_callsite(metadata);
        }
        tracing_core::subscriber::Interest::always()
    }

    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if !self.pipeline.enabled(attrs.metadata()) {
            return;
        }

        let Some(span) = ctx.span(id) else {
            return;
        };

        let state = SpanState {
            fields: capture_span_fields(attrs),
        };
        let mut extensions = span.extensions_mut();
        if let Some(existing) = extensions.get_mut::<SpanState>() {
            *existing = state;
        } else {
            extensions.insert(state);
        }
    }

    fn on_record(&self, span: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(span) else {
            return;
        };
        if !self.pipeline.enabled(span.metadata()) {
            return;
        }

        let updates = capture_record_fields(values);
        let mut extensions = span.extensions_mut();
        if let Some(existing) = extensions.get_mut::<SpanState>() {
            merge_fields(&mut existing.fields, updates);
        } else {
            extensions.insert(SpanState { fields: updates });
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if !self.pipeline.enabled(event.metadata()) {
            return;
        }

        let metadata_id = self.pipeline.metadata_id(event.metadata());
        let (current_span, spans) = self.build_span_context(event, ctx);
        let record = OwnedRecord::from_event(
            RedlinePipeline::timestamp_now(),
            metadata_id,
            event,
            current_span,
            spans,
        );
        self.pipeline.emit_record(&record);
    }
}

struct SpanState {
    fields: OwnedFields,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::SystemTime,
    };

    use super::{Builder, RedlineLayer};
    use redline::{OutputFormat, Sink};
    use tracing::{Event, Subscriber, info, info_span};
    use tracing_subscriber::{Layer, layer::Context, prelude::*, registry};

    fn temp_path(suffix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("redline-layer-{nanos}-{suffix}.log"))
    }

    #[test]
    fn ndjson_writes_event_with_span_context() {
        let path = temp_path("ndjson");
        let layer = Builder::new()
            .sink(Sink::file(&path))
            .include_span_list(true)
            .build()
            .unwrap();
        let handle = layer.handle();
        let subscriber = registry().with(layer);

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
        let layer = Builder::new()
            .sink(Sink::file(&path))
            .format(OutputFormat::Binary)
            .build()
            .unwrap();
        let handle = layer.handle();
        let subscriber = registry().with(layer);

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
        let _ = RedlineLayer::builder();
    }

    #[test]
    fn filtered_spans_do_not_appear_in_event_context() {
        let path = temp_path("filtered-span");
        let layer = Builder::new()
            .sink(Sink::file(&path))
            .filter_spec("off,app=info")
            .unwrap()
            .include_current_span(true)
            .include_span_list(true)
            .build()
            .unwrap();
        let handle = layer.handle();
        let subscriber = registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(target: "noise", "inner", skipped = true);
            let _guard = span.enter();
            info!(target: "app", message = "visible_event");
        });

        handle.flush().unwrap();
        let output = fs::read_to_string(&path).unwrap();
        assert!(output.contains(r#""target":"app""#));
        assert!(output.contains(r#""spans":[]"#));
        assert!(!output.contains(r#""span":{"#));
        assert!(!output.contains("inner"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn filter_does_not_disable_other_layers() {
        #[derive(Clone)]
        struct CountingLayer {
            events: Arc<AtomicUsize>,
        }

        impl<S> Layer<S> for CountingLayer
        where
            S: Subscriber,
        {
            fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
                self.events.fetch_add(1, Ordering::Relaxed);
            }
        }

        let path = temp_path("other-layer");
        let events = Arc::new(AtomicUsize::new(0));
        let redline = Builder::new()
            .sink(Sink::file(&path))
            .filter_spec("off")
            .unwrap()
            .build()
            .unwrap();
        let handle = redline.handle();
        let subscriber = registry().with(redline).with(CountingLayer {
            events: Arc::clone(&events),
        });

        tracing::subscriber::with_default(subscriber, || {
            info!(target: "app", message = "kept_for_other_layer");
        });

        handle.flush().unwrap();
        assert_eq!(events.load(Ordering::Relaxed), 1);
        assert!(fs::read(&path).unwrap().is_empty());
        let _ = fs::remove_file(path);
    }
}
