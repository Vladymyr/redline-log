use std::{
    cell::RefCell,
    sync::{Arc, mpsc::SyncSender},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use redline_core::{
    CallsiteMetadata, EncodeConfig, OutputFormat, OwnedRecord, TargetFilter, Timestamp,
    encode_binary_metadata, encode_binary_record, encode_ndjson_record,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use tracing_core::{Metadata, callsite::Identifier, metadata::LevelFilter, subscriber::Interest};

use crate::{
    config::{Handle, SharedStats},
    sink::{FramePool, SinkWorkerMessage, try_send_frame},
};

thread_local! {
    static CALLSITE_CACHE: RefCell<SmallVec<[(Identifier, u32); 8]>> = RefCell::new(SmallVec::new());
    static LAST_CALLSITE: RefCell<Option<(Identifier, u32)>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub struct RedlinePipeline {
    filter: TargetFilter,
    format: OutputFormat,
    encode: EncodeConfig,
    sender: SyncSender<SinkWorkerMessage>,
    frame_pool: FramePool,
    stats: Arc<SharedStats>,
    callsites: Mutex<CallsiteRegistry>,
}

impl RedlinePipeline {
    #[inline]
    pub(crate) fn new(
        filter: TargetFilter,
        format: OutputFormat,
        encode: EncodeConfig,
        sender: SyncSender<SinkWorkerMessage>,
        frame_pool: FramePool,
        stats: Arc<SharedStats>,
    ) -> Self {
        Self {
            filter,
            format,
            encode,
            sender,
            frame_pool,
            stats,
            callsites: Mutex::new(CallsiteRegistry::default()),
        }
    }

    #[inline(always)]
    pub fn handle(&self) -> Handle {
        Handle {
            sender: self.sender.clone(),
            stats: Arc::clone(&self.stats),
        }
    }

    #[inline(always)]
    pub fn filter(&self) -> &TargetFilter {
        &self.filter
    }

    #[inline(always)]
    pub fn encode_config(&self) -> EncodeConfig {
        self.encode
    }

    #[inline(always)]
    pub fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.enabled(metadata)
    }

    #[inline(always)]
    pub fn max_level_hint(&self) -> LevelFilter {
        self.filter.max_level()
    }

    #[inline(always)]
    pub fn prepare_callsite(&self, metadata: &'static Metadata<'static>) {
        let _ = self.ensure_callsite(metadata);
    }

    #[inline(always)]
    pub fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if self.filter.enabled(metadata) {
            self.prepare_callsite(metadata);
            Interest::always()
        } else {
            Interest::never()
        }
    }

    #[inline(always)]
    pub fn metadata_id(&self, metadata: &'static Metadata<'static>) -> u32 {
        if let Some(id) = Self::cached_metadata_id(metadata) {
            return id;
        }

        let (id, cacheable) = match self.format {
            OutputFormat::Ndjson => (self.ensure_callsite(metadata).id, true),
            OutputFormat::Binary => self.emit_metadata_frame(metadata),
        };

        if cacheable {
            Self::cache_metadata_id(metadata, id);
        }

        id
    }

    #[inline(always)]
    pub fn emit_record(&self, record: &OwnedRecord) {
        let mut frame = self.frame_pool.checkout(&self.stats);
        match self.format {
            OutputFormat::Ndjson => encode_ndjson_record(self.encode, record, &mut frame),
            OutputFormat::Binary => encode_binary_record(self.encode, record, &mut frame),
        }
        try_send_frame(&self.sender, frame, &self.stats, &self.frame_pool);
    }

    #[inline(always)]
    pub fn timestamp_now() -> Timestamp {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Timestamp::new(duration.as_secs(), duration.subsec_nanos())
    }

    fn ensure_callsite(&self, metadata: &'static Metadata<'static>) -> CallsiteRegistration {
        let mut registry = self.callsites.lock();
        if let Some(entry) = registry.entries.get(&metadata.callsite()) {
            return CallsiteRegistration {
                id: entry.id,
                metadata: if self.format == OutputFormat::Binary && !entry.emitted {
                    Some(entry.metadata.clone())
                } else {
                    None
                },
            };
        }

        let id = registry.next_id;
        registry.next_id = registry.next_id.saturating_add(1);
        let callsite_metadata = CallsiteMetadata::from_metadata(id, metadata);
        registry.entries.insert(
            metadata.callsite().clone(),
            RegisteredCallsite {
                id,
                metadata: callsite_metadata.clone(),
                emitted: false,
            },
        );
        CallsiteRegistration {
            id,
            metadata: (self.format == OutputFormat::Binary).then_some(callsite_metadata),
        }
    }

    fn mark_callsite_emitted(&self, callsite: Identifier) {
        let mut registry = self.callsites.lock();
        if let Some(entry) = registry.entries.get_mut(&callsite) {
            entry.emitted = true;
        }
    }

    fn emit_metadata_frame(&self, metadata: &'static Metadata<'static>) -> (u32, bool) {
        let registration = self.ensure_callsite(metadata);
        if let Some(callsite_metadata) = registration.metadata {
            let mut frame = self.frame_pool.checkout(&self.stats);
            encode_binary_metadata(&callsite_metadata, &mut frame);
            if try_send_frame(&self.sender, frame, &self.stats, &self.frame_pool) {
                self.mark_callsite_emitted(metadata.callsite().clone());
                return (registration.id, true);
            }
            return (registration.id, false);
        }
        (registration.id, true)
    }

    fn cached_metadata_id(metadata: &'static Metadata<'static>) -> Option<u32> {
        let callsite = metadata.callsite();
        if let Some(id) = LAST_CALLSITE.with(|last| {
            last.borrow().as_ref().and_then(|(cached_callsite, id)| {
                if *cached_callsite == callsite {
                    Some(*id)
                } else {
                    None
                }
            })
        }) {
            return Some(id);
        }

        CALLSITE_CACHE.with(|cache| {
            cache.borrow().iter().find_map(|(cached_callsite, id)| {
                if *cached_callsite == callsite {
                    LAST_CALLSITE.with(|last| {
                        *last.borrow_mut() = Some((cached_callsite.clone(), *id));
                    });
                    Some(*id)
                } else {
                    None
                }
            })
        })
    }

    fn cache_metadata_id(metadata: &'static Metadata<'static>, id: u32) {
        let callsite = metadata.callsite();
        LAST_CALLSITE.with(|last| *last.borrow_mut() = Some((callsite.clone(), id)));
        CALLSITE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((_, cached_id)) = cache
                .iter_mut()
                .find(|(cached_callsite, _)| *cached_callsite == callsite)
            {
                *cached_id = id;
                return;
            }

            if cache.len() == cache.capacity() {
                cache.remove(0);
            }

            cache.push((callsite, id));
        });
    }
}

#[derive(Default)]
struct CallsiteRegistry {
    next_id: u32,
    entries: FxHashMap<Identifier, RegisteredCallsite>,
}

struct RegisteredCallsite {
    id: u32,
    metadata: CallsiteMetadata,
    emitted: bool,
}

struct CallsiteRegistration {
    id: u32,
    metadata: Option<CallsiteMetadata>,
}
