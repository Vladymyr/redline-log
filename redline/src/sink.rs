use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

use crossbeam_queue::ArrayQueue;

use crate::config::{SharedStats, Sink};

pub(crate) enum SinkWorkerMessage {
    Frame(Vec<u8>),
    Flush(mpsc::Sender<io::Result<()>>),
}

enum SinkWriter {
    Null(io::Sink),
    Stdout(io::Stdout),
    Stderr(io::Stderr),
    File(std::fs::File),
}

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Null(writer) => writer.write(buf),
            Self::Stdout(writer) => writer.write(buf),
            Self::Stderr(writer) => writer.write(buf),
            Self::File(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Null(writer) => writer.flush(),
            Self::Stdout(writer) => writer.flush(),
            Self::Stderr(writer) => writer.flush(),
            Self::File(writer) => writer.flush(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct FramePool {
    buffers: Arc<ArrayQueue<Vec<u8>>>,
    buffer_size: usize,
}

impl FramePool {
    fn new(frame_buffer_count: usize, buffer_size: usize) -> Self {
        let buffers = Arc::new(ArrayQueue::new(frame_buffer_count.max(1)));
        Self {
            buffers,
            buffer_size,
        }
    }

    pub(crate) fn checkout(&self, stats: &SharedStats) -> Vec<u8> {
        if let Some(mut bytes) = self.buffers.pop() {
            bytes.clear();
            bytes
        } else {
            stats.heap_fallbacks.fetch_add(1, Ordering::Relaxed);
            Vec::with_capacity(self.buffer_size)
        }
    }

    pub(crate) fn checkin(&self, mut bytes: Vec<u8>) {
        if bytes.capacity() > self.buffer_size.saturating_mul(4) {
            bytes = Vec::with_capacity(self.buffer_size);
        }
        bytes.clear();
        let _ = self.buffers.push(bytes);
    }
}

pub(crate) fn build_sender(
    sink: Sink,
    queue_capacity: usize,
    frame_buffer_count: usize,
    frame_buffer_size: usize,
    stats: Arc<SharedStats>,
) -> io::Result<(SyncSender<SinkWorkerMessage>, FramePool)> {
    let writer = make_writer(sink)?;
    let frame_pool = FramePool::new(frame_buffer_count, frame_buffer_size);
    let (sender, receiver) = mpsc::sync_channel(queue_capacity);
    let worker_pool = frame_pool.clone();
    thread::Builder::new()
        .name("redline-sink".into())
        .stack_size(256 * 1024)
        .spawn(move || run_worker(writer, receiver, worker_pool, stats))
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    Ok((sender, frame_pool))
}

pub(crate) fn try_send_frame(
    sender: &SyncSender<SinkWorkerMessage>,
    bytes: Vec<u8>,
    stats: &SharedStats,
    frame_pool: &FramePool,
) -> bool {
    match sender.try_send(SinkWorkerMessage::Frame(bytes)) {
        Ok(()) => true,
        Err(TrySendError::Full(SinkWorkerMessage::Frame(bytes)))
        | Err(TrySendError::Disconnected(SinkWorkerMessage::Frame(bytes))) => {
            stats.dropped_frames.fetch_add(1, Ordering::Relaxed);
            stats
                .dropped_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            frame_pool.checkin(bytes);
            false
        }
        Err(TrySendError::Full(SinkWorkerMessage::Flush(_)))
        | Err(TrySendError::Disconnected(SinkWorkerMessage::Flush(_))) => unreachable!(),
    }
}

fn make_writer(sink: Sink) -> io::Result<SinkWriter> {
    match sink {
        Sink::Null => Ok(SinkWriter::Null(io::sink())),
        Sink::Stdout => Ok(SinkWriter::Stdout(io::stdout())),
        Sink::Stderr => Ok(SinkWriter::Stderr(io::stderr())),
        Sink::File(path) => {
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            Ok(SinkWriter::File(file))
        }
    }
}

fn run_worker(
    writer: SinkWriter,
    receiver: Receiver<SinkWorkerMessage>,
    frame_pool: FramePool,
    stats: Arc<SharedStats>,
) {
    let mut writer = BufWriter::new(writer);
    let mut written_frames = 0u64;

    'outer: while let Ok(message) = receiver.recv() {
        let mut flush_ack = None;
        match message {
            SinkWorkerMessage::Frame(bytes) => {
                process_frame(&mut writer, bytes, &frame_pool, &stats, &mut written_frames);
            }
            SinkWorkerMessage::Flush(ack) => flush_ack = Some(ack),
        }

        loop {
            match receiver.try_recv() {
                Ok(SinkWorkerMessage::Frame(bytes)) => {
                    process_frame(&mut writer, bytes, &frame_pool, &stats, &mut written_frames);
                }
                Ok(SinkWorkerMessage::Flush(ack)) => {
                    flush_ack = Some(ack);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }

        publish_written_frames(&stats, &mut written_frames);

        if let Some(ack) = flush_ack {
            let result = writer.flush();
            if result.is_err() {
                stats.write_errors.fetch_add(1, Ordering::Relaxed);
            }
            let _ = ack.send(result);
        }
    }

    publish_written_frames(&stats, &mut written_frames);
    if writer.flush().is_err() {
        stats.write_errors.fetch_add(1, Ordering::Relaxed);
    }
}

fn process_frame(
    writer: &mut BufWriter<SinkWriter>,
    bytes: Vec<u8>,
    frame_pool: &FramePool,
    stats: &SharedStats,
    written_frames: &mut u64,
) {
    if let Err(_error) = writer.write_all(&bytes) {
        stats.write_errors.fetch_add(1, Ordering::Relaxed);
        frame_pool.checkin(bytes);
        return;
    }

    *written_frames += 1;
    frame_pool.checkin(bytes);
}

fn publish_written_frames(stats: &SharedStats, written_frames: &mut u64) {
    if *written_frames == 0 {
        return;
    }

    stats
        .written_frames
        .fetch_add(*written_frames, Ordering::Relaxed);
    *written_frames = 0;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::FramePool;
    use crate::config::SharedStats;

    #[test]
    fn pool_records_heap_fallback_when_exhausted() {
        let pool = FramePool::new(1, 256);
        let stats = SharedStats::default();

        let first = pool.checkout(&stats);
        let second = pool.checkout(&stats);

        assert_eq!(stats.heap_fallbacks.load(Ordering::Relaxed), 2);

        pool.checkin(first);
        pool.checkin(second);
    }

    #[test]
    fn pool_starts_empty_and_allocates_lazily() {
        let pool = FramePool::new(4, 256);
        let stats = SharedStats::default();

        let bytes = pool.checkout(&stats);

        assert_eq!(bytes.capacity(), 256);
        assert_eq!(stats.heap_fallbacks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn oversized_buffers_are_not_retained() {
        let pool = FramePool::new(1, 256);
        let stats = SharedStats::default();

        let mut bytes = pool.checkout(&stats);
        bytes.reserve(2048);
        let grown = bytes.capacity();
        pool.checkin(bytes);

        let bytes = pool.checkout(&stats);

        assert!(grown > 256);
        assert!(bytes.capacity() <= grown);
        assert!(bytes.capacity() <= 256 * 2);
    }
}
