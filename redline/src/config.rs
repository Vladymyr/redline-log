use std::{
    error::Error,
    fmt, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
};

use redline_core::{EncodeConfig, FilterParseError, OutputFormat, TargetFilter};
use tracing::subscriber::SetGlobalDefaultError;

use crate::{
    pipeline::RedlinePipeline,
    sink::{SinkWorkerMessage, build_sender},
    subscriber::RedlineSubscriber,
};

/// Output sink for encoded log frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sink {
    /// Discard all output.
    Null,
    /// Write to standard output.
    Stdout,
    /// Write to standard error.
    Stderr,
    /// Append to a file.
    File(PathBuf),
}

impl Sink {
    /// Creates a file sink.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }
}

/// Handle returned by a built or installed subscriber.
#[derive(Clone)]
pub struct Handle {
    pub(crate) sender: SyncSender<SinkWorkerMessage>,
    pub(crate) stats: Arc<SharedStats>,
}

impl Handle {
    /// Flushes queued frames to the sink.
    pub fn flush(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.sender
            .send(SinkWorkerMessage::Flush(ack_tx))
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "redline sink thread is closed")
            })?;
        ack_rx.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "redline sink thread did not acknowledge flush",
            )
        })?
    }

    /// Returns a snapshot of runtime counters.
    pub fn stats(&self) -> Stats {
        Stats {
            written_frames: self.stats.written_frames.load(Ordering::Relaxed),
            dropped_frames: self.stats.dropped_frames.load(Ordering::Relaxed),
            dropped_bytes: self.stats.dropped_bytes.load(Ordering::Relaxed),
            write_errors: self.stats.write_errors.load(Ordering::Relaxed),
            heap_fallbacks: self.stats.heap_fallbacks.load(Ordering::Relaxed),
        }
    }
}

/// Runtime counters since subscriber creation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames written by the sink thread.
    pub written_frames: u64,
    /// Frames dropped before reaching the sink thread.
    pub dropped_frames: u64,
    /// Bytes dropped together with `dropped_frames`.
    pub dropped_bytes: u64,
    /// Sink write errors seen by the background worker.
    pub write_errors: u64,
    /// Heap allocations performed because no pooled frame buffer was available.
    pub heap_fallbacks: u64,
}

#[derive(Default)]
pub(crate) struct SharedStats {
    pub written_frames: AtomicU64,
    pub dropped_frames: AtomicU64,
    pub dropped_bytes: AtomicU64,
    pub write_errors: AtomicU64,
    pub heap_fallbacks: AtomicU64,
}

/// Configures and builds a [`RedlineSubscriber`].
pub struct Builder {
    filter: TargetFilter,
    format: OutputFormat,
    encode: EncodeConfig,
    sink: Sink,
    queue_capacity: usize,
    frame_buffer_size: usize,
    frame_buffer_count: usize,
}

impl Builder {
    /// Creates a builder with the default configuration.
    ///
    /// Defaults:
    /// - filter: `info`
    /// - format: NDJSON
    /// - sink: `stderr`
    /// - `include_current_span = true`
    /// - `include_span_list = false`
    pub fn new() -> Self {
        Self {
            filter: TargetFilter::parse("info").expect("hard-coded filter spec must parse"),
            format: OutputFormat::Ndjson,
            encode: EncodeConfig::default(),
            sink: Sink::Stderr,
            queue_capacity: 1024,
            frame_buffer_size: 1024,
            frame_buffer_count: 64,
        }
    }

    /// Sets a prebuilt target filter.
    pub fn filter(mut self, filter: TargetFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Parses and sets a target filter spec such as `info,app::db=debug`.
    pub fn filter_spec(mut self, spec: &str) -> Result<Self, FilterParseError> {
        self.filter = TargetFilter::parse(spec)?;
        Ok(self)
    }

    /// Selects the output format.
    pub fn format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }

    /// Includes the current span in encoded events.
    pub fn include_current_span(mut self, enabled: bool) -> Self {
        self.encode.include_current_span = enabled;
        self
    }

    /// Includes the full entered span chain in encoded events.
    pub fn include_span_list(mut self, enabled: bool) -> Self {
        self.encode.include_span_list = enabled;
        self
    }

    /// Sets the output sink.
    pub fn sink(mut self, sink: Sink) -> Self {
        self.sink = sink;
        self
    }

    /// Sets the bounded queue capacity.
    pub fn queue_capacity(mut self, queue_capacity: usize) -> Self {
        self.queue_capacity = queue_capacity.max(1);
        self
    }

    /// Sets the initial size of pooled frame buffers.
    pub fn frame_buffer_size(mut self, frame_buffer_size: usize) -> Self {
        self.frame_buffer_size = frame_buffer_size.max(256);
        self
    }

    /// Sets the number of retained pooled frame buffers.
    pub fn frame_buffer_count(mut self, frame_buffer_count: usize) -> Self {
        self.frame_buffer_count = frame_buffer_count.max(1);
        self
    }

    #[doc(hidden)]
    pub fn build_pipeline(self) -> io::Result<RedlinePipeline> {
        let stats = Arc::new(SharedStats::default());
        let (sender, frame_pool) = build_sender(
            self.sink,
            self.queue_capacity,
            self.frame_buffer_count,
            self.frame_buffer_size,
            Arc::clone(&stats),
        )?;
        Ok(RedlinePipeline::new(
            self.filter,
            self.format,
            self.encode,
            sender,
            frame_pool,
            stats,
        ))
    }

    /// Builds a subscriber without installing it globally.
    pub fn build(self) -> io::Result<RedlineSubscriber> {
        Ok(RedlineSubscriber::new(self.build_pipeline()?))
    }

    /// Builds the subscriber and installs it as the global default.
    ///
    /// ```no_run
    /// use redline::{Builder, Sink};
    /// use tracing::info;
    ///
    /// fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     let handle = Builder::new().sink(Sink::Stdout).install_global()?;
    ///     info!(target: "app", message = "started");
    ///     handle.flush()?;
    ///     Ok(())
    /// }
    /// ```
    pub fn install_global(self) -> Result<Handle, InstallError> {
        let subscriber = self.build().map_err(InstallError::Io)?;
        let handle = subscriber.handle();
        tracing::subscriber::set_global_default(subscriber).map_err(InstallError::Global)?;
        Ok(handle)
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned by [`Builder::install_global`].
#[derive(Debug)]
pub enum InstallError {
    /// Building the subscriber failed.
    Io(io::Error),
    /// Another global subscriber was already installed.
    Global(SetGlobalDefaultError),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Global(error) => write!(f, "{error}"),
        }
    }
}

impl Error for InstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Global(error) => Some(error),
        }
    }
}
