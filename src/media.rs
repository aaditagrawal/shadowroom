//! Bounded, asynchronous photo decoding. The render thread only touches memory.
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs::{self, File},
    io::{BufReader, BufWriter, Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use image::RgbaImage;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, imageops::FilterType};
use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Profile, Transform};
use serde::{Deserialize, Serialize};

// v9 uses paired/embedded JPEGs for full-size inspection as well as Fit.
const CACHE_VERSION: u32 = 9;
const CACHE_MAGIC: &[u8; 8] = b"SHDWRM09";
const QUEUE_CAPACITY: usize = 160;
const ERROR_CAPACITY: usize = 128;
const RECENT_ERROR_CAPACITY: usize = 32;
const ERROR_MESSAGE_LIMIT: usize = 2048;
const RAW_SCAN_LIMIT: u64 = 128 * 1024 * 1024;
const SOURCE_LIMIT: u64 = 512 * 1024 * 1024;
const CACHE_FILE_LIMIT: usize = 64 * 1024 * 1024;
const MAX_PIXELS: u64 = 160_000_000;
const DEFAULT_DISK_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PreviewKind {
    Thumbnail,
    Fit,
    Full,
}

impl PreviewKind {
    fn edge(self) -> Option<u32> {
        match self {
            Self::Thumbnail => Some(384),
            Self::Fit => Some(2560),
            Self::Full => None,
        }
    }
    fn priority(self) -> u8 {
        // Fit is the immediately useful view; Full is refinement; thumbnails are background work.
        match self {
            Self::Fit => 3,
            Self::Full => 2,
            Self::Thumbnail => 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ColorStatus {
    /// No embedded profile: RGB values are assumed to be sRGB.
    AssumedSrgb,
    ConvertedToSrgb,
    /// Pixels are displayed, but color fidelity cannot be guaranteed.
    InvalidProfile(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Histogram {
    pub red: Vec<u64>,
    pub green: Vec<u64>,
    pub blue: Vec<u64>,
    pub luminance: Vec<u64>,
    pub samples: u64,
}

#[derive(Clone, Debug)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha, eight-bit, sRGB RGBA, in display orientation.
    pub rgba: Vec<u8>,
    pub source_width: u32,
    pub source_height: u32,
    pub kind: PreviewKind,
    pub histogram: Histogram,
    pub dhash: u64,
    pub color_status: ColorStatus,
    pub embedded_raw: bool,
    pub rendered_raw: bool,
    pub rendering_note: Option<String>,
}

impl Preview {
    pub fn byte_len(&self) -> usize {
        self.rgba.len() + 4 * 256 * 8
    }
}

#[derive(Clone, Debug)]
pub struct MediaError {
    pub message: String,
    pub kind: PreviewKind,
}

/// Bounded diagnostic history, independent of failures cleared by retry/success.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentMediaError {
    pub timestamp_unix_ms: u64,
    pub path: PathBuf,
    pub modified_at: i64,
    pub size_bytes: u64,
    /// Requested preview tier; admission failures retain this tier too.
    pub kind: PreviewKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MediaStats {
    pub queued: usize,
    pub active: usize,
    pub memory_bytes: usize,
    pub memory_budget_bytes: usize,
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub source_reads: u64,
    pub decoded: u64,
    pub dropped_requests: u64,
    pub errors: u64,
    pub disk_bytes: u64,
    pub inflight_bytes: usize,
    pub retired_bytes: usize,
    pub peak_estimated_bytes: usize,
    pub disk_writes: u64,
    pub persistence_dropped: u64,
    pub display_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct SourceKey {
    remote: Option<RemoteMediaSource>,
    sensor_raw: bool,
    path: PathBuf,
    modified: i64,
    size: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct RemoteMediaSource {
    source: crate::model::RemoteSource,
    width: u32,
    height: u32,
}
fn photo_key(photo: &crate::model::Photo, kind: PreviewKind) -> Key {
    let companion = photo.companion_path.is_some();
    let mut key = make_key(
        photo.preview_path(),
        if companion {
            photo.companion_modified_at
        } else {
            photo.modified_at
        },
        if companion {
            photo.companion_size_bytes
        } else {
            photo.size_bytes
        },
        kind,
    );
    if let Some(source) = photo.remote_source() {
        let (width, height) = if (5..=8).contains(&photo.orientation) {
            (photo.height, photo.width)
        } else {
            (photo.width, photo.height)
        };
        key.source.remote = Some(RemoteMediaSource {
            source: source.clone(),
            width,
            height,
        });
    }
    key
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct Key {
    source: SourceKey,
    kind: PreviewKind,
}
#[derive(Clone)]
struct Task {
    key: Key,
    generation: u64,
    sequence: u64,
}
struct Entry<T> {
    value: T,
    touched: u64,
    bytes: usize,
    fresh: bool,
    published: std::time::Instant,
}

enum Retired {
    Preview(Weak<Preview>, usize),
    Source(Weak<Vec<u8>>, usize),
}
impl Retired {
    fn bytes(&self) -> usize {
        match self {
            Self::Preview(_, b) | Self::Source(_, b) => *b,
        }
    }
    fn alive(&self) -> bool {
        match self {
            Self::Preview(v, _) => v.strong_count() > 0,
            Self::Source(v, _) => v.strong_count() > 0,
        }
    }
}
struct PersistTask {
    task: Task,
    preview: Arc<Preview>,
}

struct State {
    queue: Vec<Task>,
    pending: HashMap<Key, u64>,
    previews: HashMap<Key, Entry<Arc<Preview>>>,
    sources: HashMap<SourceKey, Entry<Arc<Vec<u8>>>>,
    errors: HashMap<Key, MediaError>,
    error_order: VecDeque<Key>,
    recent_errors: VecDeque<RecentMediaError>,
    memory: usize,
    budget: usize,
    clock: u64,
    stats: MediaStats,
    retired: Vec<Retired>,
    inflight: usize,
    display: usize,
    persist: VecDeque<PersistTask>,
    persist_bytes: usize,
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
    fn evict_for(&mut self, bytes: usize) -> bool {
        self.reap();
        if bytes > self.budget {
            return false;
        }
        while self
            .memory
            .saturating_add(self.inflight)
            .saturating_add(bytes)
            > self.budget
        {
            let preview = self
                .previews
                .iter()
                .filter(|(_, entry)| {
                    !entry.fresh
                        || entry.published.elapsed() > std::time::Duration::from_millis(300)
                })
                .min_by_key(|(_, v)| v.touched)
                .map(|(k, v)| (k.clone(), v.touched));
            let source = self
                .sources
                .iter()
                .min_by_key(|(_, v)| v.touched)
                .map(|(k, v)| (k.clone(), v.touched));
            match (preview, source) {
                (Some((key, p)), Some((source, s))) if s < p => {
                    let entry = self.sources.remove(&source).unwrap();
                    if Arc::strong_count(&entry.value) > 1 {
                        self.retired
                            .push(Retired::Source(Arc::downgrade(&entry.value), entry.bytes));
                    } else {
                        self.memory -= entry.bytes;
                    }
                    let _ = key;
                }
                (Some((key, _)), _) => {
                    let entry = self.previews.remove(&key).unwrap();
                    if Arc::strong_count(&entry.value) > 1 {
                        self.retired
                            .push(Retired::Preview(Arc::downgrade(&entry.value), entry.bytes));
                    } else {
                        self.memory -= entry.bytes;
                    }
                }
                (_, Some((source, _))) => {
                    let entry = self.sources.remove(&source).unwrap();
                    if Arc::strong_count(&entry.value) > 1 {
                        self.retired
                            .push(Retired::Source(Arc::downgrade(&entry.value), entry.bytes));
                    } else {
                        self.memory -= entry.bytes;
                    }
                }
                _ => break,
            }
            self.reap();
        }
        self.memory
            .saturating_add(self.inflight)
            .saturating_add(bytes)
            <= self.budget
    }
    fn reap(&mut self) {
        let mut released = 0;
        self.retired.retain(|entry| {
            if entry.alive() {
                true
            } else {
                released += entry.bytes();
                false
            }
        });
        self.memory = self.memory.saturating_sub(released);
    }
    fn evict_with_persistence(&mut self, bytes: usize) -> bool {
        if self.evict_for(bytes) {
            return true;
        }
        while let Some(old) = self.persist.pop_front() {
            self.persist_bytes -= old.preview.byte_len();
            self.stats.persistence_dropped += 1;
            drop(old);
            if self.evict_for(bytes) {
                return true;
            }
        }
        false
    }
    fn observe_peak(&mut self) {
        self.stats.peak_estimated_bytes = self
            .stats
            .peak_estimated_bytes
            .max(self.memory.saturating_add(self.inflight));
    }
    fn satisfying(&self, key: &Key) -> Option<Key> {
        let kinds: &[PreviewKind] = match key.kind {
            PreviewKind::Thumbnail => {
                &[PreviewKind::Thumbnail, PreviewKind::Fit, PreviewKind::Full]
            }
            PreviewKind::Fit => &[PreviewKind::Fit, PreviewKind::Full],
            PreviewKind::Full => &[PreviewKind::Full],
        };
        let mut candidate = key.clone();
        for &kind in kinds {
            candidate.kind = kind;
            if self.previews.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    fn insert_preview(&mut self, key: Key, preview: Arc<Preview>) -> bool {
        let bytes = preview.byte_len();
        if let Some(old) = self.previews.remove(&key) {
            if Arc::strong_count(&old.value) > 1 {
                self.retired
                    .push(Retired::Preview(Arc::downgrade(&old.value), old.bytes));
            } else {
                self.memory -= old.bytes;
            }
        }
        if !self.evict_for(bytes) {
            return false;
        }
        let touched = self.tick();
        self.memory += bytes;
        self.observe_peak();
        self.errors.remove(&key);
        self.previews.insert(
            key,
            Entry {
                value: preview,
                touched,
                bytes,
                fresh: false,
                published: std::time::Instant::now(),
            },
        );
        true
    }
    fn record_error(&mut self, key: Key, mut message: String) {
        if message.len() > ERROR_MESSAGE_LIMIT {
            let mut end = ERROR_MESSAGE_LIMIT;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message = message[..end].to_owned();
        }
        if self.recent_errors.len() == RECENT_ERROR_CAPACITY {
            self.recent_errors.pop_front();
        }
        self.recent_errors.push_back(RecentMediaError {
            timestamp_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            path: key.source.path.clone(),
            modified_at: key.source.modified,
            size_bytes: key.source.size,
            kind: key.kind,
            message: message.clone(),
        });
        self.stats.errors += 1;
        if !self.errors.contains_key(&key) {
            self.error_order.push_back(key.clone());
        }
        self.errors.insert(
            key.clone(),
            MediaError {
                message,
                kind: key.kind,
            },
        );
        while self.error_order.len() > ERROR_CAPACITY {
            if let Some(old) = self.error_order.pop_front() {
                self.errors.remove(&old);
            }
        }
    }
}

#[derive(Default)]
struct DiskState {
    initialized: bool,
    entries: HashMap<PathBuf, (SystemTime, u64)>,
    total: u64,
    order: BTreeSet<(SystemTime, PathBuf)>,
    fit_order: BTreeSet<(SystemTime, PathBuf)>,
    fit_bytes: u64,
}

struct Shared {
    state: Mutex<State>,
    remote: OnceLock<Arc<RemotePreviewProvider>>,
    wake: Condvar,
    stop: AtomicBool,
    generation: AtomicU64,
    cancel_requested: AtomicBool,
    cache_dir: PathBuf,
    disk_budget: AtomicU64,
    disk_bytes: AtomicU64,
    disk_write: Mutex<DiskState>,
    source_locks: Vec<Mutex<()>>,
    raw_read: Mutex<()>,
    released_display: AtomicUsize,
}

/// Lifetime reservation for an external CPU BGRA/display buffer. Keep this token
/// with that buffer. Drop never waits for the media mutex (including on GPUI).
pub struct DisplayReservation {
    shared: Arc<Shared>,
    bytes: usize,
}
impl DisplayReservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}
impl std::fmt::Debug for DisplayReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayReservation")
            .field("bytes", &self.bytes)
            .finish()
    }
}
impl Drop for DisplayReservation {
    fn drop(&mut self) {
        self.shared
            .released_display
            .fetch_add(self.bytes, Ordering::Release);
        self.shared.wake.notify_all();
    }
}

pub struct MediaPipeline {
    shared: Arc<Shared>,
}

impl MediaPipeline {
    pub fn new(cache_dir: PathBuf, memory_budget_bytes: usize) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: Vec::with_capacity(QUEUE_CAPACITY),
                pending: HashMap::new(),
                previews: HashMap::new(),
                sources: HashMap::new(),
                errors: HashMap::new(),
                error_order: VecDeque::new(),
                recent_errors: VecDeque::with_capacity(RECENT_ERROR_CAPACITY),
                memory: 0,
                budget: memory_budget_bytes,
                clock: 0,
                stats: MediaStats::default(),
                retired: Vec::new(),
                inflight: 0,
                display: 0,
                persist: VecDeque::new(),
                persist_bytes: 0,
            }),
            remote: OnceLock::new(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            cancel_requested: AtomicBool::new(false),
            cache_dir: cache_dir.join(format!("previews-v{CACHE_VERSION}")),
            disk_budget: AtomicU64::new(DEFAULT_DISK_BUDGET),
            disk_bytes: AtomicU64::new(0),
            disk_write: Mutex::new(DiskState::default()),
            source_locks: (0..32).map(|_| Mutex::new(())).collect(),
            raw_read: Mutex::new(()),
            released_display: AtomicUsize::new(0),
        });
        // Two decoders keep CPU and peak decode allocations bounded and leave a core for the UI.
        for index in 0..2 {
            let worker = shared.clone();
            let _ = thread::Builder::new()
                .name(format!("shadowroom-media-{index}"))
                .spawn(move || worker_loop(worker));
        }
        let persist = shared.clone();
        let _ = thread::Builder::new()
            .name("shadowroom-persist".into())
            .spawn(move || persistence_loop(persist));
        Self { shared }
    }

    /// Nonblocking; retries on subsequent UI frames are harmless and deduplicated.
    pub fn request(&self, path: &Path, modified_at: i64, kind: PreviewKind) {
        self.request_with_size(path, modified_at, 0, kind);
    }

    /// Supplying catalog size makes invalidation exact without stat'ing an external drive.
    pub fn request_with_size(
        &self,
        path: &Path,
        modified_at: i64,
        size_bytes: u64,
        kind: PreviewKind,
    ) {
        self.request_key(make_key(path, modified_at, size_bytes, kind));
    }
    fn request_key(&self, key: Key) {
        let kind = key.kind;
        let Ok(mut state) = self.shared.state.try_lock() else {
            return;
        };
        apply_cancellation(&self.shared, &mut state);
        if state.satisfying(&key).is_some()
            || state.errors.contains_key(&key)
            || state.pending.contains_key(&key)
        {
            return;
        }
        if state.queue.len() >= QUEUE_CAPACITY {
            let lowest = state
                .queue
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| (t.key.kind.priority(), t.sequence))
                .map(|(i, t)| (i, t.key.kind.priority()));
            if let Some((index, priority)) = lowest {
                if priority > kind.priority() {
                    state.stats.dropped_requests += 1;
                    return;
                }
                let evicted = state.queue.swap_remove(index);
                state.pending.remove(&evicted.key);
                state.stats.dropped_requests += 1;
            }
        }
        let sequence = state.tick();
        let generation = self.shared.generation.load(Ordering::Relaxed);
        state.pending.insert(key.clone(), generation);
        state.queue.push(Task {
            key,
            generation,
            sequence,
        });
        drop(state);
        self.shared.wake.notify_all();
    }

    pub fn get(&self, path: &Path, modified_at: i64, kind: PreviewKind) -> Option<Arc<Preview>> {
        self.get_with_size(path, modified_at, 0, kind)
    }

    pub fn get_with_size(
        &self,
        path: &Path,
        modified_at: i64,
        size_bytes: u64,
        kind: PreviewKind,
    ) -> Option<Arc<Preview>> {
        self.get_key(make_key(path, modified_at, size_bytes, kind))
    }
    fn get_key(&self, requested: Key) -> Option<Arc<Preview>> {
        let mut state = self.shared.state.try_lock().ok()?;
        let touched = state.tick();
        let key = state.satisfying(&requested)?;
        let entry = state.previews.get_mut(&key)?;
        entry.touched = touched;
        entry.fresh = false;
        let preview = entry.value.clone();
        state.stats.memory_hits += 1;
        self.shared.wake.notify_all();
        Some(preview)
    }

    pub fn best_available(
        &self,
        path: &Path,
        modified_at: i64,
        size_bytes: u64,
    ) -> Option<Arc<Preview>> {
        self.best_key(make_key(path, modified_at, size_bytes, PreviewKind::Full))
    }
    fn best_key(&self, mut key: Key) -> Option<Arc<Preview>> {
        let mut state = self.shared.state.try_lock().ok()?;
        let touched = state.tick();
        for kind in [PreviewKind::Full, PreviewKind::Fit, PreviewKind::Thumbnail] {
            key.kind = kind;
            key.source.sensor_raw = false;
            if let Some(entry) = state.previews.get_mut(&key) {
                entry.touched = touched;
                entry.fresh = false;
                let preview = entry.value.clone();
                state.stats.memory_hits += 1;
                self.shared.wake.notify_all();
                return Some(preview);
            }
        }
        None
    }

    pub fn error(&self, path: &Path, modified_at: i64, kind: PreviewKind) -> Option<MediaError> {
        self.error_with_size(path, modified_at, 0, kind)
    }

    pub fn error_with_size(
        &self,
        path: &Path,
        modified_at: i64,
        size_bytes: u64,
        kind: PreviewKind,
    ) -> Option<MediaError> {
        self.shared
            .state
            .try_lock()
            .ok()?
            .errors
            .get(&make_key(path, modified_at, size_bytes, kind))
            .cloned()
    }

    pub fn set_remote_provider(&self, provider: Arc<RemotePreviewProvider>) {
        let _ = self.shared.remote.set(provider);
    }
    pub fn request_photo(&self, photo: &crate::model::Photo, kind: PreviewKind) {
        self.request_key(photo_key(photo, kind));
    }
    pub fn get_photo(
        &self,
        photo: &crate::model::Photo,
        kind: PreviewKind,
    ) -> Option<Arc<Preview>> {
        self.get_key(photo_key(photo, kind))
    }
    pub fn best_photo(&self, photo: &crate::model::Photo) -> Option<Arc<Preview>> {
        [PreviewKind::Full, PreviewKind::Fit, PreviewKind::Thumbnail]
            .into_iter()
            .find_map(|kind| self.get_photo(photo, kind))
    }
    pub fn photo_error(
        &self,
        photo: &crate::model::Photo,
        kind: PreviewKind,
    ) -> Option<MediaError> {
        self.shared
            .state
            .try_lock()
            .ok()?
            .errors
            .get(&photo_key(photo, kind))
            .cloned()
    }
    pub fn retry_photo(&self, photo: &crate::model::Photo, kind: PreviewKind) {
        let key = photo_key(photo, kind);
        if let Ok(mut state) = self.shared.state.try_lock() {
            state.errors.remove(&key);
        }
        self.request_key(key);
    }
    /// Retry a speculative admission failure when this photo becomes visible.
    /// Missing/corrupt sources stay failed until an explicit user retry.
    /// Returns true only when a memory error was cleared. Mutex contention is
    /// nonblocking, so the foreground caller may try again on a later frame.
    pub fn retry_photo_memory_error(&self, photo: &crate::model::Photo, kind: PreviewKind) -> bool {
        let key = photo_key(photo, kind);
        let Ok(mut state) = self.shared.state.try_lock() else {
            return false;
        };
        if !state
            .errors
            .get(&key)
            .is_some_and(|error| error.message.to_ascii_lowercase().contains("memory"))
        {
            return false;
        }
        state.errors.remove(&key);
        drop(state);
        self.request_key(key);
        true
    }
    /// O(bounded errors), called on connection changes, never per frame.
    pub fn retry_remote_errors(&self) {
        if let Ok(mut state) = self.shared.state.try_lock() {
            state.errors.retain(|key, _| key.source.remote.is_none());
        }
    }

    /// Oldest-to-newest failures, at most 32 entries. None means lock contention,
    /// not an empty history; callers may retry on a later poll. Performs no I/O.
    pub fn recent_errors(&self) -> Option<Vec<RecentMediaError>> {
        Some(
            self.shared
                .state
                .try_lock()
                .ok()?
                .recent_errors
                .iter()
                .cloned()
                .collect(),
        )
    }

    /// Clear a cached failure explicitly after reconnecting a drive; failures otherwise avoid retry storms.
    pub fn retry(&self, path: &Path, modified_at: i64, size_bytes: u64, kind: PreviewKind) {
        if let Ok(mut state) = self.shared.state.try_lock() {
            state
                .errors
                .remove(&make_key(path, modified_at, size_bytes, kind));
        }
        self.request_with_size(path, modified_at, size_bytes, kind);
    }

    /// Cancel queued work on folder/selection changes. Active codecs finish but stale results are discarded.
    pub fn cancel_pending(&self) {
        self.shared.cancel_requested.store(true, Ordering::Release);
        if let Ok(mut state) = self.shared.state.try_lock() {
            apply_cancellation(&self.shared, &mut state);
        }
        self.shared.wake.notify_all();
    }

    pub fn stats(&self) -> MediaStats {
        let Ok(mut state) = self.shared.state.try_lock() else {
            return MediaStats::default();
        };
        settle_display(&self.shared, &mut state);
        MediaStats {
            queued: state.queue.len(),
            memory_bytes: state.memory,
            memory_budget_bytes: state.budget,
            disk_bytes: self.shared.disk_bytes.load(Ordering::Relaxed),
            inflight_bytes: state.inflight,
            retired_bytes: state.retired.iter().map(Retired::bytes).sum(),
            display_bytes: state.display,
            ..state.stats
        }
    }

    /// Enforcement happens on the next cache write; this never performs disk work on the caller.
    pub fn set_disk_budget(&self, bytes: u64) {
        self.shared.disk_budget.store(bytes, Ordering::Relaxed);
    }

    /// False only means mutex contention: retry on a later poll. Existing live
    /// consumers/reservations remain charged if a lower budget cannot reclaim them.
    pub fn set_memory_budget(&self, bytes: usize) -> bool {
        let Ok(mut state) = self.shared.state.try_lock() else {
            return false;
        };
        settle_display(&self.shared, &mut state);
        state.budget = bytes;
        state
            .errors
            .retain(|_, error| !error.message.to_ascii_lowercase().contains("memory"));
        state.evict_with_persistence(0);
        drop(state);
        self.shared.wake.notify_all();
        true
    }

    /// Acquire before allocating an external CPU display copy. None means the
    /// shared budget cannot currently admit it; retry only when usage changes.
    pub fn reserve_display_bytes(&self, bytes: usize) -> Option<DisplayReservation> {
        let mut state = self.shared.state.try_lock().ok()?;
        settle_display(&self.shared, &mut state);
        if !state.evict_with_persistence(bytes) {
            return None;
        }
        state.inflight += bytes;
        state.display += bytes;
        state.observe_peak();
        Some(DisplayReservation {
            shared: self.shared.clone(),
            bytes,
        })
    }
}

impl Drop for MediaPipeline {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        // Do not join codecs on the render thread. Their Shared storage lives until they exit.
    }
}

fn make_key(path: &Path, modified: i64, size: u64, kind: PreviewKind) -> Key {
    Key {
        source: SourceKey {
            remote: None,
            sensor_raw: false,
            path: path.to_path_buf(),
            modified,
            size,
        },
        kind,
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let task = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            apply_cancellation(&shared, &mut state);
            while state.queue.is_empty() && !shared.stop.load(Ordering::Relaxed) {
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                apply_cancellation(&shared, &mut state);
            }
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            let index = state
                .queue
                .iter()
                .enumerate()
                .max_by_key(|(_, t)| (t.key.kind.priority(), t.sequence))
                .unwrap()
                .0;
            let task = state.queue.swap_remove(index);
            state.stats.active += 1;
            task
        };
        let result = process_task(&shared, &task);
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending.get(&task.key) == Some(&task.generation) {
            state.pending.remove(&task.key);
        }
        state.stats.active = state.stats.active.saturating_sub(1);
        shared.wake.notify_all();
        if !current(&shared, &task) {
            continue;
        }
        if let Err(error) = result {
            state.record_error(task.key, format!("{error:#}"));
        }
    }
}

fn current(shared: &Shared, task: &Task) -> bool {
    !shared.stop.load(Ordering::Relaxed)
        && !shared.cancel_requested.load(Ordering::Acquire)
        && task.generation == shared.generation.load(Ordering::Relaxed)
}

fn apply_cancellation(shared: &Shared, state: &mut State) {
    settle_display(shared, state);
    if shared.cancel_requested.swap(false, Ordering::AcqRel) {
        shared.generation.fetch_add(1, Ordering::Release);
        state.queue.clear();
        state.pending.clear();
        state.persist.clear();
        state.persist_bytes = 0;
        for entry in state.previews.values_mut() {
            entry.fresh = false;
        }
        state.reap();
    }
}

fn settle_display(shared: &Shared, state: &mut State) {
    let released = shared.released_display.swap(0, Ordering::AcqRel);
    state.inflight = state.inflight.saturating_sub(released);
    state.display = state.display.saturating_sub(released);
    state.reap();
}

struct MemoryPermit {
    shared: Arc<Shared>,
    bytes: usize,
}
impl MemoryPermit {
    fn transfer(&mut self, state: &mut State, bytes: usize) {
        assert!(
            bytes <= self.bytes,
            "reservation must cover published output"
        );
        self.bytes -= bytes;
        state.inflight -= bytes;
    }
}
impl Drop for MemoryPermit {
    fn drop(&mut self) {
        if self.bytes > 0 {
            let mut state = self.shared.state.lock().unwrap();
            state.inflight = state.inflight.saturating_sub(self.bytes);
            state.reap();
        }
        self.shared.wake.notify_all();
    }
}

fn reserve(shared: &Arc<Shared>, task: &Task, bytes: usize) -> Result<MemoryPermit> {
    reserve_growth(shared, task, bytes, 0)
}

fn reserve_growth(
    shared: &Arc<Shared>,
    task: &Task,
    bytes: usize,
    owned: usize,
) -> Result<MemoryPermit> {
    let release_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let mut state = shared.state.lock().unwrap();
    if bytes.saturating_add(owned) > state.budget {
        bail!(
            "Image working memory requires approximately {} MiB; increase memory budget",
            bytes.saturating_add(owned).div_ceil(1024 * 1024)
        );
    }
    loop {
        settle_display(shared, &mut state);
        if bytes.saturating_add(owned) > state.budget {
            bail!("Requested working memory exceeds the updated memory budget");
        }
        if !current(shared, task) {
            bail!("Preview request superseded");
        }
        if state.evict_with_persistence(bytes) {
            state.inflight += bytes;
            state.observe_peak();
            return Ok(MemoryPermit {
                shared: shared.clone(),
                bytes,
            });
        }
        if owned > 0 {
            bail!("RAW compaction cannot grow its held buffer within the current memory budget");
        }
        if state.inflight.saturating_sub(state.display) <= owned
            && ((state.retired.is_empty() && state.display == 0)
                || std::time::Instant::now() > release_deadline)
            && !state.previews.values().any(|entry| {
                entry.fresh && entry.published.elapsed() < std::time::Duration::from_millis(300)
            })
        {
            bail!(
                "Memory budget is occupied by live image buffers; increase memory budget or release other views"
            );
        }
        let (next, _) = shared
            .wake
            .wait_timeout(state, std::time::Duration::from_millis(20))
            .unwrap();
        state = next;
    }
}

fn process_task(shared: &Arc<Shared>, task: &Task) -> Result<()> {
    if !current(shared, task) {
        return Ok(());
    }
    // Serialize work on the same original, so progressive requests reuse the compressed-source cache.
    let digest = blake3::hash(task.key.source.path.as_os_str().as_encoded_bytes());
    let stripe = digest.as_bytes()[0] as usize % shared.source_locks.len();
    let _source_guard = shared.source_locks[stripe]
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !current(shared, task) {
        return Ok(());
    }
    // A thumbnail worker may already have left the queue when the loupe requests
    // Fit. Check again under the source lock, including another worker's pending
    // Fit task. Publishing Fit also publishes its thumbnail, so that worker will
    // find both tiers ready instead of decoding again.
    if task.key.kind == PreviewKind::Thumbnail {
        let mut fit_task = task.clone();
        fit_task.key.kind = PreviewKind::Fit;
        let fit_pending =
            shared.state.lock().unwrap().pending.get(&fit_task.key) == Some(&task.generation);
        if fit_pending && process_source_task(shared, &fit_task).is_ok() {
            return Ok(());
        }
        // A large Fit can fail admission while a scaled thumbnail fits. Keep
        // background analysis/grid work available in that case.
    }
    process_source_task(shared, task)
}

// Caller holds the source stripe for the entire cache/read/decode/publication.
fn process_source_task(shared: &Arc<Shared>, task: &Task) -> Result<()> {
    if !current(shared, task) {
        return Ok(());
    }
    if shared.state.lock().unwrap().satisfying(&task.key).is_some() {
        return Ok(());
    }
    if let Some(remote) = &task.key.source.remote {
        return process_remote_task(shared, task, remote);
    }
    if let Some(cache) = open_disk(shared, &task.key) {
        let estimate = (cache.meta.width as usize)
            .saturating_mul(cache.meta.height as usize)
            .saturating_mul(20)
            .saturating_add(cache.bytes)
            .saturating_add(256 * 1024);
        let mut permit = reserve(shared, task, estimate)?;
        if let Some(preview) = read_open_disk(shared, &task.key, cache, estimate) {
            let mut state = shared.state.lock().unwrap();
            if current(shared, task) {
                permit.transfer(&mut state, preview.byte_len());
                state.stats.disk_hits += 1;
                if !state.insert_preview(task.key.clone(), Arc::new(preview)) {
                    bail!("Cached preview exceeds available memory");
                }
                state.previews.get_mut(&task.key).unwrap().fresh = true;
            }
            return Ok(());
        }
    }
    if !current(shared, task) {
        return Ok(());
    }
    let bytes = source_bytes(shared, &task.key.source, task)?;
    if !current(shared, task) {
        return Ok(());
    }
    let sensor = task.key.source.sensor_raw
        || (is_raw(&task.key.source.path) && extract_raw_jpeg(&bytes).is_none());
    let header_bytes = if sensor {
        crate::raw_render::inspection_working_bytes(bytes.len())?
    } else {
        metadata_working_bytes(&bytes)
    };
    let header_permit = reserve(shared, task, header_bytes)?;
    let estimate = decode_working_bytes(&bytes, &task.key.source.path, task.key.kind)?;
    drop(header_permit);
    let mut permit = reserve(shared, task, estimate)?;
    let budget = shared.state.lock().unwrap().budget;
    let preview = decode_preview_bytes_budget(
        &bytes,
        &task.key.source.path,
        task.key.kind,
        estimate.min(budget),
    )?;
    if !current(shared, task) {
        return Ok(());
    }
    // Derive both cheaper levels without another original read/decode.
    let mut previews = Vec::new();
    if task.key.kind == PreviewKind::Full {
        previews.push(derive_preview(&preview, PreviewKind::Thumbnail));
        previews.push(derive_preview(&preview, PreviewKind::Fit));
    } else if task.key.kind == PreviewKind::Fit {
        previews.push(derive_preview(&preview, PreviewKind::Thumbnail));
    }
    previews.push(preview);
    {
        let mut state = shared.state.lock().unwrap();
        if !current(shared, task) {
            return Ok(());
        }
        state.stats.decoded += 1;
        for preview in previews {
            let mut source = task.key.source.clone();
            source.sensor_raw = false;
            let key = Key {
                source,
                kind: preview.kind,
            };
            let preview = Arc::new(preview);
            permit.transfer(&mut state, preview.byte_len());
            if !state.insert_preview(key.clone(), preview.clone()) {
                if key.kind == task.key.kind {
                    bail!("Preview exceeds available memory");
                }
                continue;
            }
            state.previews.get_mut(&key).unwrap().fresh = key.kind == task.key.kind;
            if key.kind != PreviewKind::Full {
                enqueue_persist(&mut state, task, key, preview);
            }
        }
    }
    shared.wake.notify_all();
    Ok(())
}

fn process_remote_task(
    shared: &Arc<Shared>,
    task: &Task,
    remote: &RemoteMediaSource,
) -> Result<()> {
    let provider = shared
        .remote
        .get()
        .cloned()
        .context("Unavailable offline: remote preview provider is not ready")?;
    let compressed = reserve(shared, task, crate::remote::protocol::MAX_PAYLOAD)?;
    let result = provider.fetch(&remote.source, task.key.kind, || !current(shared, task))?;
    if !current(shared, task) {
        return Ok(());
    }
    // Agent JPEGs already have color/orientation baked in. Never use the RAW
    // filename here, and never invoke the original-file or local disk provider.
    let path = Path::new("remote-preview.jpg");
    let estimate =
        decode_working_bytes(&result.jpeg, path, task.key.kind)?.saturating_add(result.jpeg.len());
    let additional = estimate.saturating_sub(compressed.bytes);
    let mut growth = reserve_growth(shared, task, additional, compressed.bytes)?;
    let mut permit = compressed;
    permit.bytes += growth.bytes;
    growth.bytes = 0; // transfer the reservation into one RAII owner
    let budget = shared.state.lock().unwrap().budget;
    let mut preview = decode_preview_bytes_budget(&result.jpeg, path, task.key.kind, budget)?;
    preview.rendered_raw = result.rendered_raw;
    preview.embedded_raw = result.embedded_raw;
    preview.rendering_note = result.rendering_note;
    if task.key.kind != PreviewKind::Full
        && !preview.rendered_raw
        && remote.width > 0
        && remote.height > 0
    {
        preview.source_width = remote.width;
        preview.source_height = remote.height;
    }
    let mut previews = Vec::new();
    if task.key.kind == PreviewKind::Full {
        previews.push(derive_preview(&preview, PreviewKind::Fit));
    }
    if task.key.kind != PreviewKind::Thumbnail {
        previews.push(derive_preview(&preview, PreviewKind::Thumbnail));
    }
    previews.push(preview);
    let mut state = shared.state.lock().unwrap();
    if !current(shared, task) {
        return Ok(());
    }
    state.stats.decoded += 1;
    if result.cache_hit {
        state.stats.disk_hits += 1;
    }
    for preview in previews {
        let mut source = task.key.source.clone();
        source.sensor_raw = false;
        let key = Key {
            source,
            kind: preview.kind,
        };
        let bytes = preview.byte_len();
        permit.transfer(&mut state, bytes);
        if !state.insert_preview(key.clone(), Arc::new(preview)) {
            if key.kind == task.key.kind {
                bail!("Remote preview exceeds available memory");
            }
            continue;
        }
        state.previews.get_mut(&key).unwrap().fresh = key.kind == task.key.kind;
    }
    shared.wake.notify_all();
    Ok(())
}

fn source_bytes(shared: &Arc<Shared>, key: &SourceKey, task: &Task) -> Result<Arc<Vec<u8>>> {
    {
        let mut state = shared.state.lock().unwrap();
        let touched = state.tick();
        if let Some(entry) = state.sources.get_mut(key) {
            entry.touched = touched;
            return Ok(entry.value.clone());
        }
    }
    let metadata = fs::metadata(&key.path)
        .with_context(|| format!("Original unavailable: {}", key.path.display()))?;
    if key.size != 0 && key.size != metadata.len() {
        bail!("Original changed size; reindex this folder");
    }
    let raw = is_raw(&key.path);
    let _raw_guard = if raw {
        Some(shared.raw_read.lock().unwrap())
    } else {
        None
    };
    if !current(shared, task) {
        bail!("Preview request superseded");
    }
    if (!raw || key.sensor_raw) && metadata.len() > SOURCE_LIMIT {
        bail!("File exceeds the 512 MiB compressed-source safety limit");
    }
    let limit = if raw && !key.sensor_raw {
        RAW_SCAN_LIMIT
    } else {
        SOURCE_LIMIT
    };
    let length = metadata.len().min(limit) as usize;
    let mut permit = reserve(shared, task, length)?;
    let mut bytes = vec![0; length];
    File::open(&key.path)?.read_exact(&mut bytes)?;
    if raw && !key.sensor_raw && extract_raw_jpeg(&bytes).is_some() {
        let jpeg = extract_raw_jpeg(&bytes).unwrap();
        let orientation = tiff_orientation(&bytes);
        let compact_length = jpeg.len() + if orientation.is_some() { 36 } else { 0 };
        let compact_permit = reserve_growth(shared, task, compact_length, permit.bytes)?;
        let mut compact = Vec::with_capacity(compact_length);
        if let Some(orientation) = orientation {
            compact.extend_from_slice(&[0xff, 0xd8, 0xff, 0xe1, 0, 34]);
            compact.extend_from_slice(b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0");
            compact.extend_from_slice(&[orientation as u8, 0, 0, 0, 0, 0, 0, 0]);
            compact.extend_from_slice(&jpeg[2..]);
        } else {
            compact.extend_from_slice(jpeg);
        }
        drop(bytes);
        drop(permit);
        bytes = compact;
        permit = compact_permit;
    } else if raw && !key.sensor_raw && metadata.len() > limit {
        if metadata.len() > SOURCE_LIMIT {
            bail!("RAW without an embedded preview exceeds the 512 MiB source limit");
        }
        drop(bytes);
        drop(permit);
        let length = metadata.len() as usize;
        permit = reserve(shared, task, length)?;
        bytes = vec![0; length];
        File::open(&key.path)?.read_exact(&mut bytes)?;
    }
    let bytes = Arc::new(bytes);
    let mut state = shared.state.lock().unwrap();
    state.stats.source_reads += 1;
    permit.transfer(&mut state, bytes.len());
    let touched = state.tick();
    state.memory += bytes.len();
    state.observe_peak();
    state.sources.insert(
        key.clone(),
        Entry {
            value: bytes.clone(),
            touched,
            bytes: bytes.len(),
            fresh: false,
            published: std::time::Instant::now(),
        },
    );

    Ok(bytes)
}

fn decode_working_bytes(bytes: &[u8], path: &Path, kind: PreviewKind) -> Result<usize> {
    if is_raw(path) && extract_raw_jpeg(bytes).is_none() {
        return Ok(crate::raw_render::inspect(bytes)?.working_bytes);
    }
    if crate::jxl::is_jxl(path) {
        return crate::jxl::working_bytes(bytes, kind.edge().unwrap_or(u32::MAX));
    }
    if crate::tiff_media::is_tiff(path) {
        return crate::tiff_media::working_bytes(bytes, kind.edge());
    }
    if crate::heif::is_heif(path) {
        return crate::heif::working_bytes(bytes, kind.edge());
    }
    let encoded = if is_raw(path) {
        extract_raw_jpeg(bytes)
            .context("RAW has no supported embedded JPEG in its first 128 MiB")?
    } else {
        bytes
    };
    let mut coefficient_bytes = 0;
    let mut bytes_per_pixel =
        if encoded.starts_with(b"\x89PNG\r\n\x1a\n") && encoded.get(24) == Some(&16) {
            36 // native 16-bit color, grayscale expansion, alpha, orientation/resize
        } else {
            20
        };
    let (width, height) = if encoded.starts_with(&[0xff, 0xd8]) {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(encoded));
        match decoder.read_info() {
            Ok(()) => {}
            Err(jpeg_decoder::Error::Unsupported(_)) => {
                // The image decoder may support a format the scaled decoder
                // does not. Admit its full-size buffers before using it.
                let (w, h) = image_dimensions(encoded)?;
                validate_dimensions(w, h)?;
                return Ok((w as usize)
                    .saturating_mul(h as usize)
                    .saturating_mul(28)
                    .saturating_add(metadata_working_bytes(encoded)));
            }
            Err(error) => return Err(error.into()),
        }
        let info = decoder.info().context("JPEG has no image")?;
        validate_dimensions(info.width as u32, info.height as u32)?;
        if matches!(
            info.pixel_format,
            jpeg_decoder::PixelFormat::RGB24 | jpeg_decoder::PixelFormat::L8
        ) {
            // Eight-bit JPEGs use the row resize and opaque RGB buffers.
            // Keep the larger allowance for CMYK/16-bit fallback decoders.
            bytes_per_pixel = 12;
        }
        coefficient_bytes = jpeg_coefficient_bytes(&info);
        jpeg_decode_dimensions(&mut decoder, &info, kind)?
    } else {
        image_dimensions(encoded)?
    };
    validate_dimensions(width, height)?;
    // Scaled pixels do not bound progressive coefficient storage: the decoder
    // retains every original-resolution DCT block until its scans complete.
    // The pixel allowance covers orientation/color, resize, derived tiers and
    // one pending display copy; coefficients are admitted separately.
    Ok((width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(bytes_per_pixel)
        .saturating_add(coefficient_bytes)
        .saturating_add(metadata_working_bytes(encoded)))
}

fn image_dimensions(encoded: &[u8]) -> Result<(u32, u32)> {
    let mut reader = image::ImageReader::new(Cursor::new(encoded)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.into_dimensions()?)
}

fn jpeg_coefficient_bytes(info: &jpeg_decoder::ImageInfo) -> usize {
    let components = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 | jpeg_decoder::PixelFormat::L16 => 1,
        jpeg_decoder::PixelFormat::RGB24 => 3,
        jpeg_decoder::PixelFormat::CMYK32 => 4,
    };
    // Sampling factors can pad each component to a 32-pixel MCU boundary.
    let padded_width = usize::from(info.width).div_ceil(32) * 32;
    if info.coding_process == jpeg_decoder::CodingProcess::DctProgressive {
        padded_width * usize::from(info.height).div_ceil(32) * 32 * components * 2
    } else {
        // Sequential decoding only retains a row of coefficients.
        padded_width * 32 * components * 2
    }
}

fn jpeg_decode_dimensions(
    decoder: &mut jpeg_decoder::Decoder<Cursor<&[u8]>>,
    info: &jpeg_decoder::ImageInfo,
    kind: PreviewKind,
) -> Result<(u32, u32)> {
    let scalable = info.coding_process != jpeg_decoder::CodingProcess::Lossless
        && matches!(
            info.pixel_format,
            jpeg_decoder::PixelFormat::RGB24 | jpeg_decoder::PixelFormat::L8
        );
    if let Some(edge) = kind.edge().filter(|_| scalable) {
        // scale() promises >= the request in *one* axis: a square request
        // selects the smallest IDCT with a long edge >= our bounding box.
        // A Fit source under 2x target automatically stays at full IDCT.
        let (w, h) = decoder.scale(edge as u16, edge as u16)?;
        Ok((u32::from(w), u32::from(h)))
    } else {
        Ok((u32::from(info.width), u32::from(info.height)))
    }
}

fn metadata_working_bytes(encoded: &[u8]) -> usize {
    if !encoded.starts_with(&[0xff, 0xd8]) {
        return 16 * 1024 * 1024;
    }
    let mut cursor = 2;
    let mut metadata = 0usize;
    while cursor + 1 < encoded.len() {
        // Match the decoder's tolerance for padding between marker segments.
        let Some(offset) = memchr::memchr(0xff, &encoded[cursor..encoded.len() - 1]) else {
            break;
        };
        cursor += offset;
        while cursor < encoded.len() && encoded[cursor] == 0xff {
            cursor += 1;
        }
        let Some(&marker) = encoded.get(cursor) else {
            break;
        };
        cursor += 1;
        if marker == 0xd9 || marker == 0xd8 {
            break;
        }
        if marker == 0x00 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if cursor + 2 > encoded.len() {
            break;
        }
        let length = u16::from_be_bytes([encoded[cursor], encoded[cursor + 1]]) as usize;
        if length < 2 || cursor + length > encoded.len() {
            break;
        }
        if marker == 0xe1 || marker == 0xe2 || marker == 0xed {
            metadata = metadata.saturating_add(length);
        }
        cursor += length;
        if marker == 0xda {
            // ICC/EXIF/Photoshop APP segments can occur between scans or
            // between the final scan and EOI. Skip stuffed entropy bytes and
            // restart markers without treating them as length-delimited boxes.
            while cursor + 1 < encoded.len() {
                let Some(offset) = memchr::memchr(0xff, &encoded[cursor..encoded.len() - 1]) else {
                    break;
                };
                cursor += offset;
                match encoded[cursor + 1] {
                    0x00 | 0xd0..=0xd7 => cursor += 2,
                    0xff => cursor += 1,
                    _ => break,
                }
            }
        }
    }
    metadata.saturating_mul(3).saturating_add(256 * 1024)
}

fn enqueue_persist(state: &mut State, task: &Task, key: Key, preview: Arc<Preview>) {
    let limit = (state.budget / 4).min(64 * 1024 * 1024);
    let bytes = preview.byte_len();
    if bytes > limit {
        state.stats.persistence_dropped += 1;
        return;
    }
    if state.persist.iter().any(|p| p.task.key == key) {
        return;
    }
    while state.persist_bytes.saturating_add(bytes) > limit {
        if let Some(old) = state.persist.pop_front() {
            state.persist_bytes -= old.preview.byte_len();
            state.stats.persistence_dropped += 1;
        } else {
            break;
        }
    }
    state.persist_bytes += bytes;
    state.persist.push_back(PersistTask {
        task: Task {
            key,
            ..task.clone()
        },
        preview,
    });
}

fn persistence_loop(shared: Arc<Shared>) {
    loop {
        let item = {
            let mut state = shared.state.lock().unwrap();
            loop {
                apply_cancellation(&shared, &mut state);
                if shared.stop.load(Ordering::Relaxed) {
                    return;
                }
                if !state.persist.is_empty() && state.queue.is_empty() && state.stats.active == 0 {
                    break;
                }
                state = shared
                    .wake
                    .wait_timeout(state, std::time::Duration::from_millis(30))
                    .unwrap()
                    .0;
            }
            let item = state.persist.pop_front().unwrap();
            state.persist_bytes -= item.preview.byte_len();
            item
        };
        if !current(&shared, &item.task) || cache_path(&shared, &item.task.key).is_file() {
            continue;
        }
        // The PNG encoder buffers compressed output and may keep a fallback copy;
        // reserve for worst-case Vec growth plus rows/filter/zlib workspace.
        let Ok(_permit) = reserve(
            &shared,
            &item.task,
            item.preview.rgba.len().saturating_mul(4)
                + item.preview.width as usize * 32
                + 256 * 1024,
        ) else {
            continue;
        };
        if current(&shared, &item.task) {
            let _ = write_disk(&shared, &item.task.key, &item.preview);
        }
    }
}

struct CachedFile {
    file: BufReader<File>,
    meta: DiskMeta,
    bytes: usize,
}

// Keep the header and buffered file together: admission must precede pixel
// allocation, but does not require reopening/rereading the same cache file.
fn open_disk(shared: &Shared, key: &Key) -> Option<CachedFile> {
    if key.kind == PreviewKind::Full {
        return None;
    }
    let path = cache_path(shared, key);
    let file = File::open(&path).ok()?;
    let cache = open_disk_header(file, key);
    if cache.is_none() {
        // Invalid headers must be removed too: persistence skips existing files,
        // so leaving one here prevents the original from repairing its cache.
        let _ = fs::remove_file(path);
    }
    cache
}

fn open_disk_header(file: File, key: &Key) -> Option<CachedFile> {
    let bytes = usize::try_from(file.metadata().ok()?.len()).ok()?;
    // A Fit PNG is at most 2560 squared RGBA pixels. Reject excess data before
    // admission so a corrupt/sparse cache cannot block a healthy original.
    if bytes > CACHE_FILE_LIMIT {
        return None;
    }
    let mut reader = BufReader::new(file);
    let mut magic = [0; 8];
    reader.read_exact(&mut magic).ok()?;
    if &magic != CACHE_MAGIC {
        return None;
    }
    let mut length = [0; 4];
    reader.read_exact(&mut length).ok()?;
    let length = u32::from_le_bytes(length) as usize;
    if length > 16 * 1024 {
        return None;
    }
    let mut data = vec![0; length];
    reader.read_exact(&mut data).ok()?;
    let meta: DiskMeta = serde_json::from_slice(&data).ok()?;
    if meta.version != CACHE_VERSION || meta.kind != key.kind || meta.source_size != key.source.size
    {
        return None;
    }
    let edge = key.kind.edge()?;
    if meta.width == 0 || meta.height == 0 || meta.width > edge || meta.height > edge {
        return None;
    }
    validate_dimensions(meta.source_width, meta.source_height).ok()?;
    Some(CachedFile {
        file: reader,
        meta,
        bytes,
    })
}

fn is_raw(path: &Path) -> bool {
    crate::indexer::is_raw(path)
}

/// Shared between the catalog connection actors and media workers. No network
/// or cache I/O is performed by its UI-facing methods.
pub struct RemotePreviewProvider {
    cache: crate::remote::PreviewCache,
    cache_dir: PathBuf,
    hosts: Mutex<HashMap<String, crate::remote::RemoteHost>>,
}
impl std::fmt::Debug for RemotePreviewProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemotePreviewProvider")
    }
}
impl RemotePreviewProvider {
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            cache: crate::remote::PreviewCache::new(cache_dir.clone())?,
            cache_dir,
            hosts: Mutex::new(HashMap::new()),
        })
    }
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
    pub fn cache(&self) -> crate::remote::PreviewCache {
        self.cache.clone()
    }
    pub fn set_budget(&self, bytes: u64) -> Result<()> {
        self.cache.set_budget(bytes)
    }
    pub fn connect(&self, device: String, host: crate::remote::RemoteHost) {
        self.hosts.lock().unwrap().insert(device, host);
    }
    pub fn disconnect(&self, device: &str) {
        self.hosts.lock().unwrap().remove(device);
    }
    pub fn fetch(
        &self,
        source: &crate::model::RemoteSource,
        kind: PreviewKind,
        cancelled: impl Fn() -> bool,
    ) -> Result<crate::remote::PreviewResult> {
        let key = source.key();
        let candidates: &[PreviewKind] = match kind {
            PreviewKind::Thumbnail => {
                &[PreviewKind::Thumbnail, PreviewKind::Fit, PreviewKind::Full]
            }
            PreviewKind::Fit => &[PreviewKind::Fit, PreviewKind::Full],
            PreviewKind::Full => &[PreviewKind::Full],
        };
        for &cached_kind in candidates {
            if let Some(preview) = self.cache.get(
                &source.device_id,
                &key,
                cached_kind,
                crate::remote::protocol::MAX_PAYLOAD,
            )? {
                return Ok(preview);
            }
        }
        let host = self.hosts.lock().unwrap().get(&source.device_id).cloned();
        let Some(host) = host.filter(|h| h.is_connected() && source.available) else {
            bail!("Unavailable offline: this preview kind is not cached on this computer");
        };
        match host.preview_cancellable(&key, kind, crate::remote::protocol::MAX_PAYLOAD, cancelled)
        {
            Err(error) if !host.is_connected() => bail!(
                "Unavailable offline: this preview kind is not cached on this computer ({error})"
            ),
            result => result,
        }
    }
}

/// CPU/disk blocking helper for offline tools and tests. UI callers must use MediaPipeline instead.
pub fn decode_preview(path: &Path, kind: PreviewKind) -> Result<Preview> {
    let limit = SOURCE_LIMIT;
    let mut data = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        bail!("Compressed source exceeds safety limit");
    }
    data.truncate(limit as usize);
    decode_preview_bytes(&data, path, kind)
}

fn decode_preview_bytes(bytes: &[u8], path: &Path, kind: PreviewKind) -> Result<Preview> {
    decode_preview_bytes_budget(bytes, path, kind, 512 * 1024 * 1024)
}

fn decode_preview_bytes_budget(
    bytes: &[u8],
    path: &Path,
    kind: PreviewKind,
    budget: usize,
) -> Result<Preview> {
    if is_raw(path) && extract_raw_jpeg(bytes).is_none() {
        let decoded = crate::raw_render::decode(bytes, budget)?;
        let note = format!(
            "LibRaw {} · {}",
            decoded.provenance.libraw_version,
            match decoded.provenance.white_balance {
                crate::raw_render::RawWhiteBalance::Camera => "camera white balance",
                crate::raw_render::RawWhiteBalance::DaylightFallback =>
                    "daylight white balance fallback",
            }
        );
        let mut image = decoded.image;
        if let Some(edge) = kind.edge() {
            if image.width().max(image.height()) > edge {
                image = resize_triangle_edge(&image, edge);
            }
        }
        let mut preview = make_preview(
            image,
            kind,
            decoded.source_width,
            decoded.source_height,
            ColorStatus::ConvertedToSrgb,
            false,
        );
        preview.rendered_raw = true;
        preview.rendering_note = Some(note);
        return Ok(preview);
    }
    if crate::jxl::is_jxl(path) {
        let info = crate::jxl::inspect(bytes)?;
        let decoded = crate::jxl::decode(bytes, kind.edge().unwrap_or(u32::MAX), budget)?;
        let mut image = decoded.image;
        let color = if decoded.converted_to_srgb {
            ColorStatus::ConvertedToSrgb
        } else {
            convert_to_srgb(&mut image, decoded.icc.as_deref())
        };
        let mut preview = make_preview(
            image,
            kind,
            decoded.source_width,
            decoded.source_height,
            color,
            false,
        );
        if info.animated {
            preview.rendering_note = Some("JPEG XL animation: first displayed frame only".into());
        }
        return Ok(preview);
    }
    if crate::tiff_media::is_tiff(path) {
        let info = crate::tiff_media::inspect(bytes)?;
        let decoded = crate::tiff_media::decode(bytes, kind.edge(), budget)?;
        let (width, height) = if (5..=8).contains(&info.orientation) {
            (decoded.source_height, decoded.source_width)
        } else {
            (decoded.source_width, decoded.source_height)
        };
        let mut preview = make_preview(
            decoded.image,
            kind,
            width,
            height,
            ColorStatus::ConvertedToSrgb,
            false,
        );
        let mut notes = Vec::new();
        if info.has_more_images {
            notes.push("TIFF: first page only");
        }
        if info.sample_format == 3 && !info.has_icc {
            notes.push("float TIFF: linear sRGB assumed, clipped to SDR");
        }
        if !notes.is_empty() {
            preview.rendering_note = Some(notes.join(" · "));
        }
        return Ok(preview);
    }
    if crate::heif::is_heif(path) {
        let decoded = crate::heif::decode(bytes, kind.edge(), budget)?;
        let mut image = decoded.image;
        let color_status = if decoded.converted_to_srgb {
            ColorStatus::ConvertedToSrgb
        } else {
            convert_to_srgb(&mut image, decoded.icc.as_deref())
        };
        // Native HEIF decoding already applies container geometry (and ImageIO
        // thumbnail transforms on macOS). Reapplying EXIF would rotate twice.
        if let Some(edge) = kind.edge() {
            if image.width().max(image.height()) > edge {
                image = resize_triangle_edge(&image, edge);
            }
        }
        return Ok(make_preview(
            image,
            kind,
            decoded.source_width,
            decoded.source_height,
            color_status,
            false,
        ));
    }
    let embedded_raw = is_raw(path);
    let source_orientation = exif_orientation(bytes);
    let encoded = if embedded_raw {
        extract_raw_jpeg(bytes).context("RAW embedded preview unavailable")?
    } else {
        bytes
    };
    let orientation = source_orientation
        .or_else(|| exif_orientation(encoded))
        .unwrap_or(1);
    let (mut image, icc, source_width, source_height) = decode_pixels(encoded, kind, budget)?;
    let color_status = convert_to_srgb(&mut image, icc.as_deref());
    image = apply_orientation(image, orientation);
    let (source_width, source_height) = if orientation >= 5 && orientation <= 8 {
        (source_height, source_width)
    } else {
        (source_width, source_height)
    };
    // IDCT scaling rounds each dimension up independently. Preserve the
    // original aspect ratio, including odd sizes and orientations 5..=8.
    let (width, height) = preview_dimensions(source_width, source_height, kind);
    if (image.width(), image.height()) != (width, height) {
        image = resize_triangle(&image, width, height);
    }
    Ok(make_preview(
        image,
        kind,
        source_width,
        source_height,
        color_status,
        embedded_raw,
    ))
}

fn decode_pixels(
    bytes: &[u8],
    kind: PreviewKind,
    budget: usize,
) -> Result<(DynamicImage, Option<Vec<u8>>, u32, u32)> {
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
        decoder.set_max_decoding_buffer_size(budget.min(512 * 1024 * 1024));
        match decoder.read_info() {
            Ok(()) => {}
            Err(jpeg_decoder::Error::Unsupported(_)) => {
                return decode_image_pixels(bytes, kind, budget);
            }
            Err(error) => return Err(error.into()),
        }
        let original = decoder.info().context("JPEG contains no image")?;
        validate_dimensions(original.width as u32, original.height as u32)?;
        validate_preview_budget(original.width as u32, original.height as u32, kind, budget)?;
        if matches!(
            original.pixel_format,
            jpeg_decoder::PixelFormat::RGB24 | jpeg_decoder::PixelFormat::L8
        ) {
            let (width, height) = jpeg_decode_dimensions(&mut decoder, &original, kind)?;
            let working = width as usize * height as usize * 12
                + jpeg_coefficient_bytes(&original)
                + metadata_working_bytes(bytes);
            if working > budget {
                bail!("JPEG working memory exceeds the configured memory budget");
            }
            let data = decoder.decode()?;
            let info = decoder.info().unwrap();
            let image = match info.pixel_format {
                jpeg_decoder::PixelFormat::RGB24 => DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(info.width as u32, info.height as u32, data)
                        .context("Invalid JPEG pixel buffer")?,
                ),
                jpeg_decoder::PixelFormat::L8 => DynamicImage::ImageLuma8(
                    image::GrayImage::from_raw(info.width as u32, info.height as u32, data)
                        .context("Invalid grayscale JPEG buffer")?,
                ),
                _ => unreachable!(),
            };
            // APP2 chunks may follow SOF or occur between progressive scans.
            // read_info() stops at SOF, so assemble ICC only after decode.
            return Ok((
                image,
                decoder.icc_profile(),
                original.width as u32,
                original.height as u32,
            ));
        }
    }
    decode_image_pixels(bytes, kind, budget)
}

fn decode_image_pixels(
    bytes: &[u8],
    kind: PreviewKind,
    budget: usize,
) -> Result<(DynamicImage, Option<Vec<u8>>, u32, u32)> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut header_limits = image::Limits::default();
    header_limits.max_alloc = Some((budget as u64 / 4).min(4 * 1024 * 1024));
    reader.limits(header_limits);
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    validate_preview_budget(width, height, kind, budget)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(100_000);
    limits.max_image_height = Some(100_000);
    limits.max_alloc = Some(budget.min(512 * 1024 * 1024) as u64);
    decoder.set_limits(limits)?;
    let icc = decoder.icc_profile().unwrap_or_default();
    Ok((DynamicImage::from_decoder(decoder)?, icc, width, height))
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        bail!("Image exceeds the 160 megapixel decode safety limit");
    }
    Ok(())
}

fn validate_preview_budget(
    width: u32,
    height: u32,
    kind: PreviewKind,
    budget: usize,
) -> Result<()> {
    if kind == PreviewKind::Full && u64::from(width) * u64::from(height) * 4 + 8192 > budget as u64
    {
        bail!(
            "Full-resolution image exceeds the configured memory budget; increase memory budget for 100% viewing"
        );
    }
    Ok(())
}

fn exif_orientation(bytes: &[u8]) -> Option<u32> {
    if bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        return tiff_orientation(bytes);
    }
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(&bytes[..bytes.len().min(4 * 1024 * 1024)]))
        .ok()?;
    let value = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    (1..=8).contains(&value).then_some(value)
}

fn tiff_orientation(bytes: &[u8]) -> Option<u32> {
    let little = bytes.starts_with(b"II");
    if !little && !bytes.starts_with(b"MM") {
        return None;
    }
    let word = |offset: usize| -> Option<u16> {
        let b: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let long = |offset: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if word(2)? != 42 {
        return None;
    }
    let ifd = long(4)? as usize;
    let count = word(ifd)?.min(4096) as usize;
    for i in 0..count {
        let offset = ifd.checked_add(2 + i * 12)?;
        if word(offset)? == 0x0112 && word(offset + 2)? == 3 && long(offset + 4)? == 1 {
            let value = word(offset + 8)? as u32;
            return (1..=8).contains(&value).then_some(value);
        }
    }
    None
}

/// All eight EXIF orientations, including transposed and mirrored camera files.
pub fn apply_orientation(mut image: DynamicImage, orientation: u32) -> DynamicImage {
    if let Ok(value) = u8::try_from(orientation) {
        if let Some(orientation) = image::metadata::Orientation::from_exif(value) {
            // image performs mirrors/180 degrees in place, and the mirror after
            // a transpose in place too. Preserve the owned pixel allocation.
            image.apply_orientation(orientation);
        }
    }
    image
}

fn convert_to_srgb(image: &mut DynamicImage, icc: Option<&[u8]>) -> ColorStatus {
    let Some(icc) = icc else {
        return ColorStatus::AssumedSrgb;
    };
    let result = (|| -> Result<()> {
        let source = Profile::new_icc(icc).context("Cannot parse embedded ICC profile")?;
        let target = Profile::new_srgb();
        let grayscale = source.color_space() == ColorSpaceSignature::GrayData;
        if grayscale
            && matches!(
                image,
                DynamicImage::ImageLuma16(_)
                    | DynamicImage::ImageLumaA16(_)
                    | DynamicImage::ImageRgb16(_)
                    | DynamicImage::ImageRgba16(_)
            )
        {
            let transform = Transform::<u16, [u16; 3]>::new_flags(
                &source,
                PixelFormat::GRAY_16,
                &target,
                PixelFormat::RGB_16,
                Intent::RelativeColorimetric,
                // LCMS's optimized 16-bit gray LUT rounds away dark tones.
                // Retain its analytic curve pipeline until final quantization.
                lcms2::Flags::NO_OPTIMIZE,
            )
            .context("Embedded grayscale profile is incompatible with decoded pixels")?;
            let alpha = image.color().has_alpha().then(|| {
                image
                    .to_rgba16()
                    .pixels()
                    .map(|p| p[3])
                    .collect::<Vec<u16>>()
            });
            let mut rgb =
                image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(image.width(), image.height());
            transform.transform_pixels(
                image.to_luma16().as_raw(),
                rgb.as_mut().as_chunks_mut::<3>().0,
            );
            if let Some(alpha) = alpha {
                let mut rgba = DynamicImage::ImageRgb16(rgb).into_rgba16();
                for (pixel, alpha) in rgba.pixels_mut().zip(alpha) {
                    pixel[3] = alpha;
                }
                *image = DynamicImage::ImageRgba16(rgba);
            } else {
                *image = DynamicImage::ImageRgb16(rgb);
            }
            return Ok(());
        }
        if !grayscale {
            match image {
                DynamicImage::ImageRgb16(rgb) => {
                    let transform = Transform::<[u16; 3], [u16; 3]>::new(
                        &source,
                        PixelFormat::RGB_16,
                        &target,
                        PixelFormat::RGB_16,
                        Intent::RelativeColorimetric,
                    )
                    .context("Embedded profile is incompatible with decoded RGB pixels")?;
                    transform.transform_in_place(rgb.as_mut().as_chunks_mut::<3>().0);
                    return Ok(());
                }
                DynamicImage::ImageRgba16(rgba) => {
                    let transform = Transform::<[u16; 4], [u16; 4]>::new_flags(
                        &source,
                        PixelFormat::RGBA_16,
                        &target,
                        PixelFormat::RGBA_16,
                        Intent::RelativeColorimetric,
                        lcms2::Flags::COPY_ALPHA,
                    )
                    .context("Embedded profile is incompatible with decoded RGB pixels")?;
                    transform.transform_in_place(rgba.as_mut().as_chunks_mut::<4>().0);
                    return Ok(());
                }
                _ => (),
            }
        }
        if !grayscale {
            let format = match image {
                DynamicImage::ImageRgb8(_) => Some(PixelFormat::RGB_8),
                DynamicImage::ImageRgba8(_) => Some(PixelFormat::RGBA_8),
                _ => None,
            };
            if let Some(format) = format {
                let transform = Transform::<u8, u8>::new_flags(
                    &source,
                    format,
                    &target,
                    format,
                    Intent::RelativeColorimetric,
                    if format == PixelFormat::RGBA_8 {
                        lcms2::Flags::COPY_ALPHA
                    } else {
                        lcms2::Flags::default()
                    },
                )
                .context("Embedded profile is incompatible with decoded RGB pixels")?;
                match image {
                    DynamicImage::ImageRgb8(rgb) => transform.transform_in_place(rgb.as_mut()),
                    DynamicImage::ImageRgba8(rgba) => transform.transform_in_place(rgba.as_mut()),
                    _ => unreachable!(),
                }
                return Ok(());
            }
        }
        let transform = Transform::<u8, u8>::new(
            &source,
            if grayscale {
                PixelFormat::GRAY_8
            } else {
                PixelFormat::RGB_8
            },
            &target,
            PixelFormat::RGB_8,
            Intent::RelativeColorimetric,
        )
        .context("Embedded profile is incompatible with decoded RGB pixels")?;
        let alpha = image
            .color()
            .has_alpha()
            .then(|| image.to_rgba8().pixels().map(|p| p[3]).collect::<Vec<u8>>());
        let mut rgb = image.to_rgb8();
        if grayscale {
            transform.transform_pixels(image.to_luma8().as_raw(), rgb.as_mut());
        } else {
            transform.transform_in_place(rgb.as_mut());
        }
        if let Some(alpha) = alpha {
            let mut rgba = DynamicImage::ImageRgb8(rgb).to_rgba8();
            for (pixel, a) in rgba.pixels_mut().zip(alpha) {
                pixel[3] = a;
            }
            *image = DynamicImage::ImageRgba8(rgba);
        } else {
            *image = DynamicImage::ImageRgb8(rgb);
        }
        Ok(())
    })();
    match result {
        Ok(()) => ColorStatus::ConvertedToSrgb,
        Err(error) => ColorStatus::InvalidProfile(format!("{error:#}")),
    }
}

fn make_preview(
    image: DynamicImage,
    kind: PreviewKind,
    source_width: u32,
    source_height: u32,
    color_status: ColorStatus,
    embedded_raw: bool,
) -> Preview {
    let dhash = difference_hash(&image);
    let histogram = histogram(&image);
    let rgba = image.into_rgba8();
    Preview {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        source_width,
        source_height,
        kind,
        histogram,
        dhash,
        color_status,
        embedded_raw,
        rendered_raw: false,
        rendering_note: None,
    }
}

// Same separable Triangle weights, vertical-first accumulation, and final
// rounding as image::imageops::resize. Process contiguous rows so the compiler
// can vectorize the vertical pass; only one float row is live, rather than an
// entire width * output_height RGBA32F intermediate. Other pixel depths keep
// image's implementation and its established conversion behavior.
fn triangle_weights(source: u32, target: u32, position: u32) -> (usize, Vec<f32>) {
    let ratio = source as f32 / target as f32;
    let support = ratio.max(1.0);
    let center = (position as f32 + 0.5) * ratio;
    let first = ((center - support).floor() as i64).clamp(0, i64::from(source) - 1) as usize;
    let end =
        ((center + support).ceil() as i64).clamp(first as i64 + 1, i64::from(source)) as usize;
    let center = center - 0.5;
    let mut weights: Vec<f32> = (first..end)
        .map(|i| {
            let distance = ((i as f32 - center) / support).abs();
            if distance < 1.0 { 1.0 - distance } else { 0.0 }
        })
        .collect();
    let sum: f32 = weights.iter().sum();
    for weight in &mut weights {
        *weight /= sum;
    }
    (first, weights)
}

fn resize_triangle_u8<const CHANNELS: usize>(
    pixels: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    if (source_width, source_height) == (width, height) {
        return pixels.to_vec();
    }
    let source_stride = source_width as usize * CHANNELS;
    let stride = width as usize * CHANNELS;
    let horizontal: Vec<_> = (0..width)
        .map(|x| triangle_weights(source_width, width, x))
        .collect();
    let mut row = vec![0f32; source_stride];
    let mut output = vec![0u8; stride * height as usize];
    for (y, target) in output.chunks_exact_mut(stride).enumerate() {
        row.fill(0.0);
        let (first, weights) = triangle_weights(source_height, height, y as u32);
        for (i, weight) in weights.into_iter().enumerate() {
            let start = (first + i) * source_stride;
            for (sum, value) in row.iter_mut().zip(&pixels[start..start + source_stride]) {
                *sum += f32::from(*value) * weight;
            }
        }
        for (pixel, (first, weights)) in target.chunks_exact_mut(CHANNELS).zip(&horizontal) {
            let mut sums = [0f32; CHANNELS];
            for (i, weight) in weights.iter().enumerate() {
                let start = (first + i) * CHANNELS;
                for channel in 0..CHANNELS {
                    sums[channel] += row[start + channel] * weight;
                }
            }
            for channel in 0..CHANNELS {
                pixel[channel] = sums[channel].clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    output
}

trait AlphaSample: Copy + PartialEq {
    const MAX: f32;
    fn float(self) -> f32;
    fn quantize(value: f32) -> Self;
}
impl AlphaSample for u8 {
    const MAX: f32 = 255.;
    fn float(self) -> f32 {
        f32::from(self)
    }
    fn quantize(value: f32) -> Self {
        value.clamp(0., 255.).round() as u8
    }
}
impl AlphaSample for u16 {
    const MAX: f32 = 65535.;
    fn float(self) -> f32 {
        f32::from(self)
    }
    fn quantize(value: f32) -> Self {
        value.clamp(0., 65535.).round() as u16
    }
}
impl AlphaSample for f32 {
    const MAX: f32 = 1.;
    fn float(self) -> f32 {
        self
    }
    fn quantize(value: f32) -> Self {
        value.clamp(0., 1.)
    }
}

// Filter associated color and coverage together (Porter/Duff), then return
// straight alpha. A row of float scratch preserves the existing memory bound;
// no full-resolution float/premultiplied image is allocated.
fn resize_alpha<T: AlphaSample, const CHANNELS: usize>(
    pixels: &[T],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Vec<T> {
    if (source_width, source_height) == (width, height) {
        return pixels.to_vec();
    }
    let stride = width as usize * CHANNELS;
    let source_stride = source_width as usize * CHANNELS;
    let horizontal: Vec<_> = (0..width)
        .map(|x| triangle_weights(source_width, width, x))
        .collect();
    let mut row = vec![0f32; source_stride];
    let mut output = vec![T::quantize(0.); stride * height as usize];
    for (y, target) in output.chunks_exact_mut(stride).enumerate() {
        row.fill(0.);
        let (first, weights) = triangle_weights(source_height, height, y as u32);
        for (i, weight) in weights.into_iter().enumerate() {
            let start = (first + i) * source_stride;
            for (sum, pixel) in row
                .chunks_exact_mut(CHANNELS)
                .zip(pixels[start..start + source_stride].chunks_exact(CHANNELS))
            {
                let alpha = pixel[CHANNELS - 1].float() / T::MAX;
                if alpha > 0. {
                    for c in 0..CHANNELS - 1 {
                        sum[c] += pixel[c].float() * alpha * weight;
                    }
                    sum[CHANNELS - 1] += pixel[CHANNELS - 1].float() * weight;
                }
            }
        }
        for (pixel, (first, weights)) in target.chunks_exact_mut(CHANNELS).zip(&horizontal) {
            let mut sums = [0f32; CHANNELS];
            for (i, weight) in weights.iter().enumerate() {
                for c in 0..CHANNELS {
                    sums[c] += row[(first + i) * CHANNELS + c] * weight;
                }
            }
            let alpha = sums[CHANNELS - 1];
            pixel[CHANNELS - 1] = T::quantize(alpha);
            if pixel[CHANNELS - 1] != T::quantize(0.) && alpha > 0. {
                for c in 0..CHANNELS - 1 {
                    pixel[c] = T::quantize(sums[c] * T::MAX / alpha);
                }
            }
        }
    }
    output
}

fn resize_rgba8(pixels: &[u8], w: u32, h: u32, width: u32, height: u32) -> Vec<u8> {
    if pixels.chunks_exact(4).all(|p| p[3] == 255) {
        resize_triangle_u8::<4>(pixels, w, h, width, height)
    } else {
        resize_alpha::<u8, 4>(pixels, w, h, width, height)
    }
}

fn resize_triangle(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 || width == 0 || height == 0 {
        return image.resize_exact(width, height, FilterType::Triangle);
    }
    match image {
        DynamicImage::ImageRgb8(rgb) => DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(
                width,
                height,
                resize_triangle_u8::<3>(rgb.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageRgba8(rgba) => DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(
                width,
                height,
                resize_rgba8(rgba.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageLuma8(gray) => DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(
                width,
                height,
                resize_triangle_u8::<1>(gray.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageLumaA8(gray) => DynamicImage::ImageLumaA8(
            image::GrayAlphaImage::from_raw(
                width,
                height,
                resize_alpha::<u8, 2>(gray.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageRgba16(rgba) => DynamicImage::ImageRgba16(
            image::ImageBuffer::from_raw(
                width,
                height,
                resize_alpha::<u16, 4>(rgba.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageLumaA16(gray) => DynamicImage::ImageLumaA16(
            image::ImageBuffer::from_raw(
                width,
                height,
                resize_alpha::<u16, 2>(gray.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        DynamicImage::ImageRgba32F(rgba) => DynamicImage::ImageRgba32F(
            image::ImageBuffer::from_raw(
                width,
                height,
                resize_alpha::<f32, 4>(rgba.as_raw(), w, h, width, height),
            )
            .unwrap(),
        ),
        _ => image.resize_exact(width, height, FilterType::Triangle),
    }
}

pub(crate) fn resize_triangle_edge(image: &DynamicImage, edge: u32) -> DynamicImage {
    let ratio = f64::from(edge) / f64::from(image.width().max(image.height()));
    let width = (f64::from(image.width()) * ratio).round().max(1.0) as u32;
    let height = (f64::from(image.height()) * ratio).round().max(1.0) as u32;
    resize_triangle(image, width, height)
}

fn preview_dimensions(width: u32, height: u32, kind: PreviewKind) -> (u32, u32) {
    let scale = kind
        .edge()
        .map_or(1., |edge| (edge as f64 / width.max(height) as f64).min(1.));
    (
        (width as f64 * scale).round().max(1.) as u32,
        (height as f64 * scale).round().max(1.) as u32,
    )
}

fn derive_preview(preview: &Preview, kind: PreviewKind) -> Preview {
    // Borrow the full buffer while resizing: cloning a 100 MP frame here doubles peak memory.
    let (width, height) = preview_dimensions(preview.source_width, preview.source_height, kind);
    let image = DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(
            width,
            height,
            resize_rgba8(&preview.rgba, preview.width, preview.height, width, height),
        )
        .unwrap(),
    );
    let mut derived = make_preview(
        image,
        kind,
        preview.source_width,
        preview.source_height,
        preview.color_status.clone(),
        preview.embedded_raw,
    );
    derived.rendered_raw = preview.rendered_raw;
    derived.rendering_note = preview.rendering_note.clone();
    derived
}

/// Stable 64-bit horizontal dHash on display-oriented sRGB pixels; compare with xor.count_ones().
pub fn difference_hash(image: &DynamicImage) -> u64 {
    let small = resize_triangle(image, 9, 8).into_luma8();
    let mut hash = 0;
    for y in 0..8 {
        for x in 0..8 {
            if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                hash |= 1 << (y * 8 + x);
            }
        }
    }
    hash
}

/// Counts display-space pixel values, with at most 512 samples per axis.
/// Larger images use deterministic stratified point samples: averaging pixels
/// before counting would hide clipping and invent tones absent from the image.
/// Fully transparent pixels are excluded; partial alpha counts once, without
/// choosing a background. This describes the preview, not RAW exposure.
pub fn histogram(image: &DynamicImage) -> Histogram {
    let (width, height) = image.dimensions();
    match image {
        DynamicImage::ImageRgb8(rgb) => sampled_histogram(width, height, |x, y| {
            let p = rgb.get_pixel(x, y).0;
            [p[0], p[1], p[2], 255]
        }),
        DynamicImage::ImageRgba8(rgba) => {
            sampled_histogram(width, height, |x, y| rgba.get_pixel(x, y).0)
        }
        // Convert only sampled pixels, including alpha, rather than allocating
        // an entire quantized copy of a high-bit-depth/float input.
        _ => sampled_histogram(width, height, |x, y| image.get_pixel(x, y).0),
    }
}

fn sampled_histogram(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Histogram {
    let mut h = Histogram {
        red: vec![0; 256],
        green: vec![0; 256],
        blue: vec![0; 256],
        luminance: vec![0; 256],
        samples: 0,
    };
    if width == 0 || height == 0 {
        return h;
    }
    let mut count = |p: [u8; 4]| {
        if p[3] == 0 {
            return;
        }
        h.samples += 1;
        h.red[p[0] as usize] += 1;
        h.green[p[1] as usize] += 1;
        h.blue[p[2] as usize] += 1;
        let luma = (54 * u32::from(p[0]) + 183 * u32::from(p[1]) + 19 * u32::from(p[2])) >> 8;
        h.luminance[luma as usize] += 1;
    };
    if width <= 512 && height <= 512 {
        for y in 0..height {
            for x in 0..width {
                count(pixel(x, y));
            }
        }
        return h;
    }
    let columns = width.min(512);
    let rows = height.min(512);
    for row in 0..rows {
        let top = (u64::from(row) * u64::from(height) / u64::from(rows)) as u32;
        let bottom = (u64::from(row + 1) * u64::from(height) / u64::from(rows)) as u32;
        for column in 0..columns {
            let left = (u64::from(column) * u64::from(width) / u64::from(columns)) as u32;
            let right = (u64::from(column + 1) * u64::from(width) / u64::from(columns)) as u32;
            // Jitter each cell independently to avoid fixed-stride aliasing of
            // repeated texture (Cook, 1986). SplitMix64 gives a repeatable mix;
            // no process RNG, image allocation, or source-file reads are needed.
            let mut jitter = (u64::from(row) * u64::from(columns) + u64::from(column))
                .wrapping_add(0x9e3779b97f4a7c15);
            jitter = (jitter ^ (jitter >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            jitter = (jitter ^ (jitter >> 27)).wrapping_mul(0x94d049bb133111eb);
            jitter ^= jitter >> 31;
            let x = left + jitter as u32 % (right - left);
            let y = top + (jitter >> 32) as u32 % (bottom - top);
            count(pixel(x, y));
        }
    }
    h
}

/// Conservative, bounded RAW preview extraction. Never claims to develop sensor data.
fn extract_raw_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    let mut cursor = 0;
    let mut best: Option<(&[u8], u64)> = None;
    let mut candidates = 0;
    while cursor + 3 < bytes.len() && candidates < 64 {
        let Some(offset) = memchr::memmem::find(&bytes[cursor..], &[0xff, 0xd8, 0xff]) else {
            break;
        };
        let start = cursor + offset;
        candidates += 1;
        let end_limit = bytes.len().min(start + 32 * 1024 * 1024);
        let Some(end) = jpeg_end(bytes, start, end_limit) else {
            cursor = start + 3;
            continue;
        };
        let candidate = &bytes[start..end];
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(candidate));
        if decoder.read_info().is_ok() {
            if let Some(info) = decoder.info() {
                let pixels = u64::from(info.width) * u64::from(info.height);
                if pixels >= 160 * 120
                    && pixels <= MAX_PIXELS
                    && best.as_ref().is_none_or(|(_, p)| pixels > *p)
                {
                    best = Some((candidate, pixels));
                }
            }
        }
        cursor = end;
    }
    best.map(|(data, _)| data)
}

// Skip length-delimited APP/EXIF segments instead of mistaking an EXIF thumbnail's
// EOI for the outer JPEG's EOI. Entropy data uses byte stuffing and restart markers.
fn jpeg_end(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let mut cursor = start + 2;
    while cursor + 1 < limit {
        if bytes[cursor] != 0xff {
            return None;
        }
        let marker_start = cursor;
        while cursor < limit && bytes[cursor] == 0xff {
            cursor += 1;
        }
        let marker = *bytes.get(cursor)?;
        cursor += 1;
        match marker {
            0xd9 => return Some(cursor),
            0x01 | 0xd0..=0xd7 => continue,
            0x00 | 0xd8 => return None,
            _ => {}
        }
        if cursor + 2 > limit {
            return None;
        }
        let length = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
        if length < 2 || cursor + length > limit {
            return None;
        }
        cursor += length;
        if marker == 0xda {
            loop {
                if cursor + 1 >= limit {
                    return None;
                }
                cursor += memchr::memchr(0xff, &bytes[cursor..limit - 1])?;
                if bytes[cursor] == 0xff {
                    let next = bytes[cursor + 1];
                    if next == 0x00 || (0xd0..=0xd7).contains(&next) {
                        cursor += 2;
                        continue;
                    }
                    if next == 0xff {
                        cursor += 1;
                        continue;
                    }
                    break;
                }
                cursor += 1;
            }
        }
        if cursor <= marker_start {
            return None;
        }
    }
    None
}

#[derive(Serialize, Deserialize)]
struct DiskMeta {
    version: u32,
    source_size: u64,
    width: u32,
    height: u32,
    source_width: u32,
    source_height: u32,
    kind: PreviewKind,
    color_status: ColorStatus,
    embedded_raw: bool,
    #[serde(default)]
    rendered_raw: bool,
    #[serde(default)]
    rendering_note: Option<String>,
}

fn cache_path(shared: &Shared, key: &Key) -> PathBuf {
    let mut hash = blake3::Hasher::new();
    hash.update(key.source.path.as_os_str().as_encoded_bytes());
    hash.update(&key.source.modified.to_le_bytes());
    hash.update(&key.source.size.to_le_bytes());
    hash.update(&[u8::from(key.source.sensor_raw)]);
    hash.update(&[key.kind.priority()]);
    shared.cache_dir.join(format!(
        "{}-{}.srp",
        hash.finalize().to_hex(),
        if key.kind == PreviewKind::Thumbnail {
            "t"
        } else {
            "f"
        }
    ))
}

#[cfg(test)]
fn read_disk(shared: &Shared, key: &Key) -> Option<Preview> {
    read_disk_with_limit(shared, key, 512 * 1024 * 1024)
}

#[cfg(test)]
fn read_disk_with_limit(shared: &Shared, key: &Key, budget: usize) -> Option<Preview> {
    let cache = open_disk(shared, key)?;
    read_open_disk(shared, key, cache, budget)
}

fn read_open_disk(shared: &Shared, key: &Key, cache: CachedFile, budget: usize) -> Option<Preview> {
    let path = cache_path(shared, key);
    let read = (|| -> Result<Preview> {
        let CachedFile { file, meta, bytes } = cache;
        let mut encoded = Vec::with_capacity(bytes);
        file.take(CACHE_FILE_LIMIT as u64)
            .read_to_end(&mut encoded)?;
        if encoded.len() < 24
            || !encoded.starts_with(b"\x89PNG\r\n\x1a\n")
            || u32::from_be_bytes(encoded[16..20].try_into().unwrap()) != meta.width
            || u32::from_be_bytes(encoded[20..24].try_into().unwrap()) != meta.height
        {
            bail!("Cache pixel dimensions do not match header");
        }
        let (image, _, w, h) = decode_pixels(&encoded, key.kind, budget)?;
        if w != meta.width || h != meta.height {
            bail!("Cache dimension mismatch");
        }
        let mut preview = make_preview(
            image,
            key.kind,
            meta.source_width,
            meta.source_height,
            meta.color_status,
            meta.embedded_raw,
        );
        preview.rendered_raw = meta.rendered_raw;
        preview.rendering_note = meta.rendering_note;
        Ok(preview)
    })();
    match read {
        Ok(preview) => Some(preview),
        Err(_) => {
            let _ = fs::remove_file(path);
            None
        }
    }
}

fn write_disk(shared: &Shared, key: &Key, preview: &Preview) -> Result<()> {
    let mut disk = shared.disk_write.lock().unwrap();
    let budget = shared.disk_budget.load(Ordering::Relaxed);
    if budget == 0 {
        return Ok(());
    }
    fs::create_dir_all(&shared.cache_dir)?;
    let path = cache_path(shared, key);
    if path.is_file() {
        update_and_prune_disk(shared, &mut disk, &path, budget)?;
        return Ok(());
    }
    let temp = path.with_extension("tmp");
    let metadata = serde_json::to_vec(&DiskMeta {
        version: CACHE_VERSION,
        source_size: key.source.size,
        width: preview.width,
        height: preview.height,
        source_width: preview.source_width,
        source_height: preview.source_height,
        kind: key.kind,
        color_status: preview.color_status.clone(),
        embedded_raw: preview.embedded_raw,
        rendered_raw: preview.rendered_raw,
        rendering_note: preview.rendering_note.clone(),
    })?;
    let result = (|| -> Result<()> {
        let mut file = BufWriter::with_capacity(64 * 1024, File::create(&temp)?);
        file.write_all(CACHE_MAGIC)?;
        file.write_all(&(metadata.len() as u32).to_le_bytes())?;
        file.write_all(&metadata)?;
        // Photographs are normally opaque. Omitting the redundant alpha plane
        // remains lossless PNG and reduces both compression work and cache I/O.
        let opaque = preview.rgba.chunks_exact(4).all(|p| p[3] == 255);
        let rgb;
        let (pixels, color) = if opaque {
            rgb = preview
                .rgba
                .chunks_exact(4)
                .flat_map(|p| p[..3].iter().copied())
                .collect::<Vec<u8>>();
            (rgb.as_slice(), image::ExtendedColorType::Rgb8)
        } else {
            (preview.rgba.as_slice(), image::ExtendedColorType::Rgba8)
        };
        image::codecs::png::PngEncoder::new_with_quality(
            &mut file,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(pixels, preview.width, preview.height, color)?;
        file.flush()?;
        drop(file);
        fs::rename(&temp, &path)?;
        update_and_prune_disk(shared, &mut disk, &path, budget)?;
        shared.state.lock().unwrap().stats.disk_writes += 1;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn is_fit_cache(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.ends_with("-f"))
}

fn update_and_prune_disk(
    shared: &Shared,
    disk: &mut DiskState,
    written: &Path,
    budget: u64,
) -> Result<()> {
    if !disk.initialized {
        for entry in fs::read_dir(&shared.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "srp") {
                continue;
            }
            let meta = entry.metadata()?;
            let time = meta.modified().unwrap_or(UNIX_EPOCH);
            let size = meta.len();
            disk.total += size;
            disk.entries.insert(path.clone(), (time, size));
            disk.order.insert((time, path.clone()));
            if is_fit_cache(&path) {
                disk.fit_order.insert((time, path));
                disk.fit_bytes += size;
            }
        }
        disk.initialized = true;
    }
    let size = fs::metadata(written)?.len();
    let time = SystemTime::now();
    if let Some((old_time, old_size)) = disk.entries.insert(written.to_path_buf(), (time, size)) {
        disk.total = disk.total.saturating_sub(old_size);
        disk.order.remove(&(old_time, written.to_path_buf()));
        if is_fit_cache(written) {
            disk.fit_order.remove(&(old_time, written.to_path_buf()));
            disk.fit_bytes = disk.fit_bytes.saturating_sub(old_size);
        }
    }
    disk.total += size;
    disk.order.insert((time, written.to_path_buf()));
    if is_fit_cache(written) {
        disk.fit_order.insert((time, written.to_path_buf()));
        disk.fit_bytes += size;
    }
    if disk.total > budget {
        let low_water = budget.saturating_mul(9) / 10;
        while disk.total > low_water {
            // Preserve a third of the budget for the far smaller, often reused thumbnails.
            let oldest = if disk.fit_bytes > budget.saturating_mul(2) / 3 {
                disk.fit_order.first().cloned()
            } else {
                disk.order.first().cloned()
            };
            let Some((time, path)) = oldest else {
                break;
            };
            if fs::remove_file(&path).is_err() && path.exists() {
                break;
            }
            if let Some((_, size)) = disk.entries.remove(&path) {
                disk.total = disk.total.saturating_sub(size);
                if is_fit_cache(&path) {
                    disk.fit_bytes = disk.fit_bytes.saturating_sub(size);
                }
            }
            disk.order.remove(&(time, path.clone()));
            disk.fit_order.remove(&(time, path));
        }
    }
    shared.disk_bytes.store(disk.total, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    #[test]
    fn opaque_and_transparent_disk_caches_round_trip_exact_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(directory.path().to_path_buf(), 8 * 1024 * 1024);
        for opaque in [true, false] {
            let source = DynamicImage::ImageRgba8(RgbaImage::from_fn(97, 71, |x, y| {
                image::Rgba([
                    (x * 3) as u8,
                    (y * 5) as u8,
                    ((x * 7 + y * 11) % 256) as u8,
                    if opaque { 255 } else { ((x + y) % 256) as u8 },
                ])
            }));
            let preview = make_preview(
                source,
                PreviewKind::Thumbnail,
                97,
                71,
                ColorStatus::AssumedSrgb,
                false,
            );
            let key = make_key(
                Path::new(if opaque { "opaque" } else { "alpha" }),
                1,
                0,
                PreviewKind::Thumbnail,
            );
            write_disk(&pipeline.shared, &key, &preview).unwrap();
            let actual = read_disk(&pipeline.shared, &key).unwrap();
            assert_eq!(actual.rgba, preview.rgba);
            assert_eq!(actual.dhash, preview.dhash);
            assert_eq!(actual.histogram.red, preview.histogram.red);
            assert_eq!(actual.histogram.green, preview.histogram.green);
            assert_eq!(actual.histogram.blue, preview.histogram.blue);
            assert_eq!(actual.histogram.luminance, preview.histogram.luminance);
        }
    }

    #[test]
    fn full_and_fit_prefer_paired_jpeg_without_opening_invalid_raw() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("frame.erf");
        let companion = directory.path().join("frame.jpg");
        let encoded = jpeg(320, 200);
        std::fs::write(&primary, b"RAW not needed for reduced preview").unwrap();
        std::fs::write(&companion, &encoded).unwrap();
        let photo = crate::model::Photo {
            path: primary.clone(),
            companion_path: Some(companion.clone()),
            size_bytes: 34,
            modified_at: 1,
            companion_size_bytes: encoded.len() as u64,
            companion_modified_at: 2,
            ..Default::default()
        };
        let full = photo_key(&photo, PreviewKind::Full);
        assert_eq!(full.source.path, companion);
        assert!(!full.source.sensor_raw);
        let fit = photo_key(&photo, PreviewKind::Fit);
        assert_eq!(fit.source.path, companion);
        assert!(!fit.source.sensor_raw);
        let pipeline = MediaPipeline::new(directory.path().join("cache"), 64 * 1024 * 1024);
        pipeline.request_photo(&photo, PreviewKind::Full);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let preview = loop {
            if let Some(preview) = pipeline.best_photo(&photo) {
                break preview;
            }
            assert!(
                std::time::Instant::now() < until,
                "Paired Fit fallback disappeared"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(preview.kind, PreviewKind::Full);
        assert!(!preview.rendered_raw);
        assert_eq!((preview.width, preview.height), (320, 200));
    }

    #[test]
    fn full_raw_inspection_uses_native_embedded_jpeg_without_libraw() {
        let jpeg = jpeg(1024, 768);
        let mut raw = b"not sensor data; only an embedded camera JPEG follows".to_vec();
        raw.extend_from_slice(&jpeg);
        let preview = decode_preview_bytes_budget(
            &raw,
            Path::new("frame.nef"),
            PreviewKind::Full,
            32 * 1024 * 1024,
        )
        .unwrap();
        assert!(preview.embedded_raw);
        assert!(!preview.rendered_raw);
        assert_eq!((preview.width, preview.height), (1024, 768));
        assert_eq!((preview.source_width, preview.source_height), (1024, 768));
    }

    #[test]
    fn grayscale_sixteen_bit_icc_keeps_dark_tones_and_alpha() {
        let profile = Profile::new_gray(
            &lcms2::CIExyY {
                x: 0.3457,
                y: 0.3585,
                Y: 1.0,
            },
            &lcms2::ToneCurve::new(1.0),
        )
        .unwrap()
        .icc()
        .unwrap();
        for mut image in [
            DynamicImage::ImageLumaA16(
                image::ImageBuffer::from_raw(1, 1, vec![100u16, 32768]).unwrap(),
            ),
            DynamicImage::ImageRgba16(
                image::ImageBuffer::from_raw(1, 1, vec![100u16, 100, 100, 32768]).unwrap(),
            ),
        ] {
            assert_eq!(
                convert_to_srgb(&mut image, Some(&profile)),
                ColorStatus::ConvertedToSrgb
            );
            let p = image.to_rgba8().get_pixel(0, 0).0;
            assert!(
                p[..3].iter().all(|v| (4..=6).contains(v)),
                "gray precision lost: {p:?}"
            );
            assert_eq!(image.to_rgba16().get_pixel(0, 0).0[3], 32768);
        }
    }

    #[test]
    #[ignore = "set SHADOWROOM_HEIF_ORACLE_DIR; native libheif with uncompressed support required"]
    fn native_heif_depth_alpha_and_color_oracle() {
        let directory = PathBuf::from(
            std::env::var_os("SHADOWROOM_HEIF_ORACLE_DIR").expect("oracle fixture directory"),
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("oracle-manifest.json")).unwrap())
                .unwrap();
        let mut checked = 0;
        for fixture in manifest["fixtures"].as_array().unwrap() {
            let name = Path::new(fixture["file"].as_str().unwrap())
                .file_name()
                .unwrap();
            let path = directory.join(name);
            let result = decode_preview(&path, PreviewKind::Full);
            if fixture["expect_app_rejection"].as_bool().unwrap() {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Premultiplied-alpha"),
                    "{name:?}"
                );
            } else {
                let preview = result.unwrap_or_else(|e| panic!("{name:?}: {e:#}"));
                assert!(
                    !matches!(preview.color_status, ColorStatus::InvalidProfile(_)),
                    "{name:?}"
                );
                if let Some(expected) = fixture["expected_display_rgba8"].as_array() {
                    let expected: Vec<u8> =
                        expected.iter().map(|v| v.as_u64().unwrap() as u8).collect();
                    assert!(
                        preview
                            .rgba
                            .chunks_exact(4)
                            .all(|p| p.iter().zip(&expected).all(|(a, b)| a.abs_diff(*b) <= 1)),
                        "{name:?}: actual {:?}, expected {expected:?}",
                        &preview.rgba[..4]
                    );
                }
            }
            checked += 1;
        }
        assert_eq!(checked, 22);
    }

    #[test]
    fn sixteen_bit_icc_converts_before_display_quantization_and_retains_alpha() {
        let curve = lcms2::ToneCurve::new(1.0);
        let profile = Profile::new_rgb(
            &lcms2::CIExyY {
                x: 0.3127,
                y: 0.3290,
                Y: 1.0,
            },
            &lcms2::CIExyYTRIPLE {
                Red: lcms2::CIExyY {
                    x: 0.64,
                    y: 0.33,
                    Y: 1.0,
                },
                Green: lcms2::CIExyY {
                    x: 0.30,
                    y: 0.60,
                    Y: 1.0,
                },
                Blue: lcms2::CIExyY {
                    x: 0.15,
                    y: 0.06,
                    Y: 1.0,
                },
            },
            &[&curve, &curve, &curve],
        )
        .unwrap();
        let pixels = image::ImageBuffer::from_raw(1, 1, vec![100u16, 32768, 65535, 32768]).unwrap();
        let mut image = DynamicImage::ImageRgba16(pixels);
        assert_eq!(
            convert_to_srgb(&mut image, Some(&profile.icc().unwrap())),
            ColorStatus::ConvertedToSrgb
        );
        let p = image.to_rgba8().get_pixel(0, 0).0;
        // Quantizing linear 100/65535 to 8-bit before ICC would become zero.
        assert!(
            (4..=6).contains(&p[0]),
            "dark-channel precision lost: {p:?}"
        );
        assert!((187..=189).contains(&p[1]));
        assert!(p[2] >= 254);
        assert_eq!(image.to_rgba16().get_pixel(0, 0).0[3], 32768);
    }

    #[test]
    fn in_place_icc_matches_rgb_reference_and_preserves_alpha() {
        let white = lcms2::CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = lcms2::CIExyYTRIPLE {
            Red: lcms2::CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.0,
            },
            Green: lcms2::CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.0,
            },
            Blue: lcms2::CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.0,
            },
        };
        let curve = lcms2::ToneCurve::new(1.0);
        for profile in [
            Profile::new_srgb(),
            Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve]).unwrap(),
        ] {
            let icc = profile.icc().unwrap();
            let source = Profile::new_icc(&icc).unwrap();
            let transform = Transform::<u8, u8>::new(
                &source,
                PixelFormat::RGB_8,
                &Profile::new_srgb(),
                PixelFormat::RGB_8,
                Intent::RelativeColorimetric,
            )
            .unwrap();
            let pixels = RgbaImage::from_fn(256, 256, |x, y| {
                image::Rgba([
                    x as u8,
                    y as u8,
                    ((x * 31 + y * 7) % 256) as u8,
                    ((x + y) % 256) as u8,
                ])
            });
            let mut expected = DynamicImage::ImageRgba8(pixels.clone()).to_rgb8();
            transform.transform_in_place(expected.as_mut());
            for mut image in [
                DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(pixels.clone()).to_rgb8()),
                DynamicImage::ImageRgba8(pixels.clone()),
            ] {
                assert_eq!(
                    convert_to_srgb(&mut image, Some(&icc)),
                    ColorStatus::ConvertedToSrgb
                );
                assert_eq!(image.to_rgb8().as_raw(), expected.as_raw());
                if let DynamicImage::ImageRgba8(actual) = image {
                    for (actual, source) in actual.pixels().zip(pixels.pixels()) {
                        assert_eq!(actual[3], source[3]);
                    }
                }
            }
        }
    }

    #[test]
    fn row_triangle_matches_image_for_opaque_rgb_rgba_and_grayscale() {
        for (w, h) in [
            (1, 1),
            (1, 37),
            (43, 1),
            (3, 2),
            (17, 31),
            (127, 89),
            (1024, 768),
        ] {
            let rgba = RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([
                    (x.wrapping_mul(71).wrapping_add(y * 31) % 256) as u8,
                    ((x * 13 + y * 59) % 256) as u8,
                    ((x * 113 + y * 7) % 256) as u8,
                    255,
                ])
            });
            let rgba = DynamicImage::ImageRgba8(rgba);
            for source in [
                DynamicImage::ImageRgb8(rgba.to_rgb8()),
                rgba.clone(),
                DynamicImage::ImageLuma8(rgba.to_luma8()),
            ] {
                for (width, height) in [
                    (1, 1),
                    (9, 8),
                    (7, 29),
                    (w, h),
                    (w / 2 + 1, h / 2 + 1),
                    (384, 256),
                ] {
                    let expected = source.resize_exact(width, height, FilterType::Triangle);
                    let actual = resize_triangle(&source, width, height);
                    assert_eq!(
                        actual.as_bytes(),
                        expected.as_bytes(),
                        "{w}x{h} -> {width}x{height} {:?}",
                        source.color()
                    );
                }
            }
        }
    }

    #[test]
    fn transparent_resampling_matches_an_independent_associated_float_reference() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x504f52544552);
        for _ in 0..40 {
            let (w, h) = (rng.random_range(1..40), rng.random_range(1..40));
            let original =
                image::RgbaImage::from_fn(w, h, |_, _| image::Rgba(rng.random::<[u8; 4]>()));
            let premultiplied = image::Rgba32FImage::from_fn(w, h, |x, y| {
                let p = original.get_pixel(x, y).0;
                let a = f32::from(p[3]) / 255.;
                image::Rgba([
                    f32::from(p[0]) / 255. * a,
                    f32::from(p[1]) / 255. * a,
                    f32::from(p[2]) / 255. * a,
                    a,
                ])
            });
            let (width, height) = (rng.random_range(1..40), rng.random_range(1..40));
            if (width, height) == (w, h) {
                continue;
            }
            let expected =
                image::imageops::resize(&premultiplied, width, height, FilterType::Triangle);
            let actual = resize_rgba8(original.as_raw(), w, h, width, height);
            for (actual, expected) in actual.chunks_exact(4).zip(expected.pixels()) {
                let alpha = (expected[3].clamp(0., 1.) * 255.).round() as u8;
                assert!(actual[3].abs_diff(alpha) <= 1);
                for channel in 0..3 {
                    let value = if alpha == 0 {
                        0
                    } else {
                        (expected[channel] * 255. / expected[3])
                            .clamp(0., 255.)
                            .round() as u8
                    };
                    assert!(
                        actual[channel].abs_diff(value) <= 1,
                        "{w}x{h} -> {width}x{height}: {actual:?} / {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn hidden_color_cannot_bleed_through_resized_rgba16_or_gray_alpha() {
        let source = DynamicImage::ImageRgba16(image::ImageBuffer::from_fn(4, 1, |x, _| {
            if x < 2 {
                image::Rgba([65535, 0, 0, 0])
            } else {
                image::Rgba([0, 0, 65535, 65535])
            }
        }));
        let resized = resize_triangle(&source, 1, 1);
        assert_eq!(
            resized.as_rgba16().unwrap().get_pixel(0, 0).0,
            [0, 0, 65535, 32768]
        );
        let gray = DynamicImage::ImageLumaA8(image::GrayAlphaImage::from_fn(4, 1, |x, _| {
            if x < 2 {
                image::LumaA([255, 0])
            } else {
                image::LumaA([30, 255])
            }
        }));
        let resized = resize_triangle(&gray, 1, 1);
        assert_eq!(
            resized.as_luma_alpha8().unwrap().get_pixel(0, 0).0,
            [30, 128]
        );
        let invisible = resize_rgba8(&[255, 0, 255, 0, 255, 255, 0, 0], 2, 1, 1, 1);
        assert_eq!(invisible, [0, 0, 0, 0]);
    }

    fn preview_error(reference: &Preview, actual: &Preview) -> (f64, u8, [f64; 3]) {
        let pixels = resize_triangle_u8::<4>(
            &reference.rgba,
            reference.width,
            reference.height,
            actual.width,
            actual.height,
        );
        let mut sum = 0u64;
        let mut max = 0;
        let mut bias = [0i64; 3];
        for (a, b) in pixels.chunks_exact(4).zip(actual.rgba.chunks_exact(4)) {
            for c in 0..3 {
                let error = a[c].abs_diff(b[c]);
                sum += u64::from(error);
                max = max.max(error);
                bias[c] += i64::from(b[c]) - i64::from(a[c]);
            }
        }
        let count = f64::from(actual.width) * f64::from(actual.height);
        (
            sum as f64 / (count * 3.),
            max,
            bias.map(|v| v as f64 / count),
        )
    }

    fn profiled_jpeg(image: &DynamicImage, icc: &[u8], orientation: u8) -> Vec<u8> {
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 94)
            .encode_image(image)
            .unwrap();
        // Put multipart ICC *after* SOF, in reverse chunk order. This catches
        // both reading ICC too early and concatenating APP2 in file order.
        let sos = encoded.windows(2).position(|b| b == [0xff, 0xda]).unwrap();
        let mut metadata = vec![0xff, 0xe1, 0, 34];
        metadata.extend_from_slice(b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0");
        metadata.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        let split = icc.len() / 2;
        for (sequence, chunk) in [(2, &icc[split..]), (1, &icc[..split])] {
            metadata.extend_from_slice(&[0xff, 0xe2]);
            metadata.extend_from_slice(&((chunk.len() + 16) as u16).to_be_bytes());
            metadata.extend_from_slice(b"ICC_PROFILE\0");
            metadata.extend_from_slice(&[sequence, 2]);
            metadata.extend_from_slice(chunk);
        }
        encoded.splice(sos..sos, metadata);
        encoded
    }

    #[test]
    fn scaled_jpeg_tiers_avoid_full_idct_and_keep_original_dimensions() {
        let bytes = jpeg(6000, 4001);
        for (kind, expected) in [
            (PreviewKind::Thumbnail, (750, 501)),
            (PreviewKind::Fit, (3000, 2001)),
        ] {
            let (pixels, _, w, h) = decode_pixels(&bytes, kind, 512 * 1024 * 1024).unwrap();
            assert_eq!((pixels.width(), pixels.height()), expected);
            assert_eq!((w, h), (6000, 4001));
        }
        let thumbnail =
            decode_working_bytes(&bytes, Path::new("large.jpg"), PreviewKind::Thumbnail).unwrap();
        let full = decode_working_bytes(&bytes, Path::new("large.jpg"), PreviewKind::Full).unwrap();
        assert!(thumbnail < full / 16);
        let info = jpeg_decoder::ImageInfo {
            width: 6000,
            height: 4001,
            pixel_format: jpeg_decoder::PixelFormat::RGB24,
            coding_process: jpeg_decoder::CodingProcess::DctProgressive,
        };
        assert!(jpeg_coefficient_bytes(&info) >= 6000 * 4001 * 6);
    }

    #[test]
    fn jpeg_admission_charges_icc_chunks_after_entropy_data() {
        let mut bytes = jpeg(320, 200);
        assert!(bytes.ends_with(&[0xff, 0xd9]));
        bytes.truncate(bytes.len() - 2);
        let count = 12u8;
        let payload_length = 60_000;
        for sequence in 1..=count {
            bytes.extend_from_slice(b"padding\xff\0");
            // Legal fill bytes before APP2 also need to preserve marker parsing.
            bytes.extend_from_slice(&[0xff, 0xff, 0xe2]);
            bytes.extend_from_slice(&((payload_length + 16) as u16).to_be_bytes());
            bytes.extend_from_slice(b"ICC_PROFILE\0");
            bytes.extend_from_slice(&[sequence, count]);
            bytes.resize(bytes.len() + payload_length, 0);
        }
        bytes.extend_from_slice(&[0xff, 0xd9]);
        let expected_metadata = (payload_length + 16) * usize::from(count);
        assert!(metadata_working_bytes(&bytes) >= expected_metadata * 3 + 256 * 1024);
        let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(&bytes));
        decoder.decode().unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap().len(),
            payload_length * usize::from(count)
        );
        let required =
            decode_working_bytes(&bytes, Path::new("late.jpg"), PreviewKind::Thumbnail).unwrap();
        let error = decode_pixels(&bytes, PreviewKind::Thumbnail, required / 2)
            .unwrap_err()
            .to_string();
        assert!(error.contains("working memory"), "{error}");
    }

    #[test]
    fn scaled_jpeg_quality_preserves_late_icc_grayscale_and_all_orientations() {
        let rgb = DynamicImage::ImageRgb8(image::RgbImage::from_fn(1537, 1025, |x, y| {
            image::Rgb([
                (x * 220 / 1536 + 15) as u8,
                (y * 220 / 1024 + 15) as u8,
                ((x + y) * 220 / 2560 + 15) as u8,
            ])
        }));
        let white = lcms2::CIExyY::d50();
        let primaries = lcms2::CIExyYTRIPLE {
            Red: lcms2::CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.,
            },
            Green: lcms2::CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.,
            },
            Blue: lcms2::CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.,
            },
        };
        let curve = lcms2::ToneCurve::new(1.0);
        let linear = Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve]).unwrap();
        let gray = Profile::new_gray(&white, &curve).unwrap();
        for (image, profile) in [
            (&rgb, linear),
            (&DynamicImage::ImageLuma8(rgb.to_luma8()), gray),
        ] {
            for orientation in 1..=8 {
                let bytes = profiled_jpeg(image, &profile.icc().unwrap(), orientation);
                let full = decode_preview_bytes(&bytes, Path::new("color.jpg"), PreviewKind::Full)
                    .unwrap();
                let actual =
                    decode_preview_bytes(&bytes, Path::new("color.jpg"), PreviewKind::Thumbnail)
                        .unwrap();
                assert_eq!(actual.color_status, ColorStatus::ConvertedToSrgb);
                let reference = derive_preview(&full, PreviewKind::Thumbnail);
                assert_eq!(
                    (actual.width, actual.height),
                    (reference.width, reference.height)
                );
                let (mae, max, bias) = preview_error(&reference, &actual);
                assert!(
                    mae < 1.0 && max <= 20 && bias.iter().all(|v| v.abs() < 0.5),
                    "orientation {orientation}: MAE={mae}, max={max}, bias={bias:?}"
                );
                // A missing linear profile leaves the midtones near 125,
                // whereas the required linear -> sRGB transfer lifts them.
                let center = ((actual.height / 2 * actual.width + actual.width / 2) * 4) as usize;
                assert!(actual.rgba[center] > 165);
            }
        }
    }

    #[test]
    fn concurrent_thumbnail_and_fit_share_one_source_read_and_decode() {
        for reverse in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.jpg");
            let encoded = jpeg(1537, 1025);
            fs::write(&path, &encoded).unwrap();
            let pipeline = MediaPipeline::new(directory.path().join("cache"), 128 * 1024 * 1024);
            let guards: Vec<_> = pipeline
                .shared
                .source_locks
                .iter()
                .map(|lock| lock.lock().unwrap())
                .collect();
            let kinds = if reverse {
                [PreviewKind::Fit, PreviewKind::Thumbnail]
            } else {
                [PreviewKind::Thumbnail, PreviewKind::Fit]
            };
            for kind in kinds {
                pipeline.request_with_size(&path, 1, encoded.len() as u64, kind);
            }
            drop(guards);
            let fit = await_preview(&pipeline, &path, encoded.len() as u64, PreviewKind::Fit);
            let thumbnail = await_preview(
                &pipeline,
                &path,
                encoded.len() as u64,
                PreviewKind::Thumbnail,
            );
            assert_eq!(thumbnail.kind, PreviewKind::Thumbnail);
            assert_eq!(
                thumbnail.rgba,
                derive_preview(&fit, PreviewKind::Thumbnail).rgba
            );
            let stats = pipeline.stats();
            assert_eq!(stats.source_reads, 1);
            assert_eq!(stats.decoded, 1);
        }
    }

    #[test]
    fn selected_thumbnail_promotes_to_pending_fit_and_survives_fit_admission_failure() {
        for budget in [128 * 1024 * 1024, 8 * 1024 * 1024] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source.jpg");
            let encoded = jpeg(1537, 1025);
            fs::write(&path, &encoded).unwrap();
            let pipeline = MediaPipeline::new(directory.path().join("cache"), budget);
            let key = make_key(&path, 1, encoded.len() as u64, PreviewKind::Thumbnail);
            let mut fit_key = key.clone();
            fit_key.kind = PreviewKind::Fit;
            let generation = pipeline.shared.generation.load(Ordering::Relaxed);
            // Simulate the precise race: Thumbnail has already been selected,
            // and a Fit request arrives before its source lock is acquired.
            pipeline
                .shared
                .state
                .lock()
                .unwrap()
                .pending
                .insert(fit_key.clone(), generation);
            process_task(
                &pipeline.shared,
                &Task {
                    key: key.clone(),
                    generation,
                    sequence: 1,
                },
            )
            .unwrap();
            let state = pipeline.shared.state.lock().unwrap();
            assert!(state.previews.contains_key(&key));
            assert_eq!(
                state.previews.contains_key(&fit_key),
                budget > 8 * 1024 * 1024
            );
            assert_eq!(state.stats.source_reads, 1);
            assert_eq!(state.stats.decoded, 1);
        }
    }

    // Optional external fixtures are generated outside /tmp; never launches the app.
    #[test]
    #[ignore = "round 2 format, quality, and latency benchmark"]
    fn round2_media_benchmark() {
        use std::time::Instant;
        let fixtures = PathBuf::from(std::env::var("SHADOWROOM_BENCH_DIR").unwrap());
        let saved = std::env::var("SHADOWROOM_BENCH_SAVE")
            .ok()
            .map(PathBuf::from);
        if let Some(saved) = &saved {
            fs::create_dir_all(saved).unwrap();
        }
        let mut paths = vec![
            PathBuf::from("/tmp/shadowroom-test-shoot/DSC_0003.jpg"),
            PathBuf::from("/tmp/shadowroom-ui-fixture/DSC_0100.jpg"),
            PathBuf::from("/tmp/shadowroom-ui-fixture/DSC_0200.jpg"),
        ];
        paths.extend(
            [
                "synthetic.jpg",
                "progressive.jpg",
                "gray.jpg",
                "cmyk.jpg",
                "embedded.nef",
            ]
            .map(|name| fixtures.join(name)),
        );
        for (index, path) in paths.iter().enumerate() {
            let bytes = fs::read(path).unwrap();
            let full = decode_preview_bytes(&bytes, path, PreviewKind::Full).unwrap();
            for kind in [PreviewKind::Thumbnail, PreviewKind::Fit] {
                let result = decode_preview_bytes(&bytes, path, kind).unwrap();
                let reference = derive_preview(&full, kind);
                let (mae, max, bias) = preview_error(&reference, &result);
                assert!(
                    mae < 5. && max <= 96 && bias.iter().all(|v| v.abs() < 2.),
                    "{} {kind:?}: MAE={mae}, max={max}, bias={bias:?}",
                    path.display()
                );
                let start = Instant::now();
                for _ in 0..3 {
                    std::hint::black_box(decode_preview(path, kind).unwrap());
                }
                println!(
                    "R2 {} {kind:?}: {:.3} ms, source_bytes={}, {}x{}, full_mae={mae:.4}, max={max}, bias={bias:.4?}, color={:?}",
                    path.file_name().unwrap().to_string_lossy(),
                    start.elapsed().as_secs_f64() * 1000. / 3.,
                    bytes.len().min(if is_raw(path) {
                        RAW_SCAN_LIMIT as usize
                    } else {
                        usize::MAX
                    }),
                    result.width,
                    result.height,
                    result.color_status
                );
                if let Some(saved) = &saved {
                    let name = saved.join(format!("{index}-{kind:?}"));
                    fs::write(name.with_extension("rgba"), &result.rgba).unwrap();
                    fs::write(
                        name.with_extension("json"),
                        serde_json::to_vec(&(result.width, result.height)).unwrap(),
                    )
                    .unwrap();
                }
                if let Ok(prior) = std::env::var("SHADOWROOM_BENCH_PRIOR") {
                    let name = Path::new(&prior).join(format!("{index}-{kind:?}"));
                    let (width, height) =
                        serde_json::from_slice(&fs::read(name.with_extension("json")).unwrap())
                            .unwrap();
                    let mut previous = result.clone();
                    previous.width = width;
                    previous.height = height;
                    previous.rgba = fs::read(name.with_extension("rgba")).unwrap();
                    let (mae, max, bias) = preview_error(&previous, &result);
                    println!("  round1_mae={mae:.4}, max={max}, bias={bias:.4?}");
                }
            }
            let start = Instant::now();
            for _ in 0..3 {
                let thumb = decode_preview_bytes(&bytes, path, PreviewKind::Thumbnail).unwrap();
                let fit = decode_preview_bytes(&bytes, path, PreviewKind::Fit).unwrap();
                std::hint::black_box((thumb, fit));
            }
            let separate_ms = start.elapsed().as_secs_f64() * 1000. / 3.;
            let start = Instant::now();
            for _ in 0..3 {
                let fit = decode_preview_bytes(&bytes, path, PreviewKind::Fit).unwrap();
                let thumb = derive_preview(&fit, PreviewKind::Thumbnail);
                std::hint::black_box((thumb, fit));
            }
            println!(
                "  paired decode+derive: {separate_ms:.3} -> {:.3} ms; source reads 1 -> 1 (compressed source retained), decodes 2 -> 1",
                start.elapsed().as_secs_f64() * 1000. / 3.
            );
        }
    }

    // Opt-in, repeatable CPU/cache benchmark; all generated data stays in a temp dir.
    #[test]
    #[ignore = "preview pipeline timing harness"]
    fn preview_pipeline_timing() {
        use std::time::Instant;
        fn timed<T>(label: &str, n: usize, mut f: impl FnMut() -> T) {
            let start = Instant::now();
            for _ in 0..n {
                std::hint::black_box(f());
            }
            println!(
                "{label}: {:.6} ms",
                start.elapsed().as_secs_f64() * 1000. / n as f64
            );
        }
        fn reference_preview(bytes: &[u8], kind: PreviewKind) -> Preview {
            let orientation = exif_orientation(bytes).unwrap_or(1);
            let (mut image, icc, mut w, mut h) =
                decode_pixels(bytes, kind, 512 * 1024 * 1024).unwrap();
            let color = if let Some(icc) = icc {
                let source = Profile::new_icc(&icc).unwrap();
                let target = Profile::new_srgb();
                let grayscale = source.color_space() == ColorSpaceSignature::GrayData;
                let transform = Transform::<u8, u8>::new(
                    &source,
                    if grayscale {
                        PixelFormat::GRAY_8
                    } else {
                        PixelFormat::RGB_8
                    },
                    &target,
                    PixelFormat::RGB_8,
                    Intent::RelativeColorimetric,
                )
                .unwrap();
                let alpha = image
                    .color()
                    .has_alpha()
                    .then(|| image.to_rgba8().pixels().map(|p| p[3]).collect::<Vec<_>>());
                let mut rgb = image.to_rgb8();
                if grayscale {
                    transform.transform_pixels(image.to_luma8().as_raw(), rgb.as_mut());
                } else {
                    transform.transform_in_place(rgb.as_mut());
                }
                image = if let Some(alpha) = alpha {
                    let mut rgba = DynamicImage::ImageRgb8(rgb).to_rgba8();
                    for (pixel, a) in rgba.pixels_mut().zip(alpha) {
                        pixel[3] = a;
                    }
                    DynamicImage::ImageRgba8(rgba)
                } else {
                    DynamicImage::ImageRgb8(rgb)
                };
                ColorStatus::ConvertedToSrgb
            } else {
                ColorStatus::AssumedSrgb
            };
            image = match orientation {
                2 => image.fliph(),
                3 => image.rotate180(),
                4 => image.flipv(),
                5 => image.rotate90().fliph(),
                6 => image.rotate90(),
                7 => image.rotate90().flipv(),
                8 => image.rotate270(),
                _ => image,
            };
            if let Some(edge) = kind.edge() {
                if image.width().max(image.height()) > edge {
                    image = image.resize(edge, edge, FilterType::Triangle);
                }
            }
            if (5..=8).contains(&orientation) {
                std::mem::swap(&mut w, &mut h);
            }
            let small = image.resize_exact(9, 8, FilterType::Triangle).to_luma8();
            let mut dhash = 0;
            for y in 0..8 {
                for x in 0..8 {
                    if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                        dhash |= 1 << (y * 8 + x);
                    }
                }
            }
            let small = if image.width().max(image.height()) > 512 {
                image.thumbnail(512, 512).to_rgb8()
            } else {
                image.to_rgb8()
            };
            let mut histogram = Histogram {
                red: vec![0; 256],
                green: vec![0; 256],
                blue: vec![0; 256],
                luminance: vec![0; 256],
                samples: u64::from(small.width()) * u64::from(small.height()),
            };
            for p in small.pixels() {
                histogram.red[p[0] as usize] += 1;
                histogram.green[p[1] as usize] += 1;
                histogram.blue[p[2] as usize] += 1;
                let luma =
                    (54 * u32::from(p[0]) + 183 * u32::from(p[1]) + 19 * u32::from(p[2])) >> 8;
                histogram.luminance[luma as usize] += 1;
            }
            let rgba = image.to_rgba8();
            Preview {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
                source_width: w,
                source_height: h,
                kind,
                histogram,
                dhash,
                color_status: color,
                embedded_raw: false,
                rendered_raw: false,
                rendering_note: None,
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let synthetic = DynamicImage::ImageRgb8(image::RgbImage::from_fn(6000, 4000, |x, y| {
            image::Rgb([
                ((x * 7 + y * 3) % 256) as u8,
                ((x / 8 + y) % 256) as u8,
                ((x + y / 4) % 256) as u8,
            ])
        }));
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
            .encode_image(&synthetic)
            .unwrap();
        let profile = Profile::new_srgb().icc().unwrap();
        let mut app = b"ICC_PROFILE\0\x01\x01".to_vec();
        app.extend(&profile);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe2];
        jpeg.extend(((app.len() + 2) as u16).to_be_bytes());
        jpeg.extend(app);
        jpeg.extend(&encoded[2..]);
        println!("synthetic 6000x4000 JPEG+ICC: {} bytes", jpeg.len());
        let real = fs::read("/tmp/shadowroom-test-shoot/DSC_0003.jpg").unwrap();
        for (name, bytes) in [("real", real.as_slice()), ("synthetic", jpeg.as_slice())] {
            timed(&format!("{name} image JPEG full pixels"), 3, || {
                image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap()
            });
            for kind in [PreviewKind::Thumbnail, PreviewKind::Fit] {
                let expected = reference_preview(bytes, kind);
                let actual = decode_preview_bytes(bytes, Path::new("test.jpg"), kind).unwrap();
                let (mae, max, _) = preview_error(&expected, &actual);
                assert!(mae < 4. && max < 80, "MAE={mae}, max={max}");
                assert_eq!(actual.color_status, expected.color_status);
                timed(&format!("{name} {kind:?} reference end-to-end"), 3, || {
                    reference_preview(bytes, kind)
                });
                timed(&format!("{name} {kind:?} end-to-end"), 3, || {
                    decode_preview_bytes(bytes, Path::new("test.jpg"), kind).unwrap()
                });
                timed(&format!("{name} {kind:?} JPEG pixels"), 3, || {
                    decode_pixels(bytes, kind, 512 * 1024 * 1024).unwrap()
                });
            }
        }
        let preview = decode_preview_bytes(&jpeg, Path::new("test.jpg"), PreviewKind::Fit).unwrap();
        timed("24MP Triangle resize reference -> 2560", 3, || {
            synthetic.resize(2560, 2560, FilterType::Triangle)
        });
        timed("24MP Triangle resize rows -> 2560", 3, || {
            resize_triangle_edge(&synthetic, 2560)
        });
        let rgb = synthetic.to_rgb8();
        timed("24MP ICC reference (including image clone)", 3, || {
            let image = DynamicImage::ImageRgb8(rgb.clone());
            let source = Profile::new_icc(&profile).unwrap();
            let target = Profile::new_srgb();
            let transform = Transform::<u8, u8>::new(
                &source,
                PixelFormat::RGB_8,
                &target,
                PixelFormat::RGB_8,
                Intent::RelativeColorimetric,
            )
            .unwrap();
            let mut output = image.to_rgb8();
            transform.transform_in_place(output.as_mut());
            DynamicImage::ImageRgb8(output)
        });
        timed("24MP ICC conversion (including image clone)", 3, || {
            let mut image = DynamicImage::ImageRgb8(rgb.clone());
            convert_to_srgb(&mut image, Some(&profile));
            image
        });
        let rgba = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(preview.width, preview.height, preview.rgba.clone()).unwrap(),
        );
        timed("Fit make_preview (including image clone)", 5, || {
            make_preview(
                rgba.clone(),
                PreviewKind::Fit,
                6000,
                4000,
                ColorStatus::AssumedSrgb,
                false,
            )
        });
        timed("Fit histogram", 5, || histogram(&rgba));
        timed(
            "24MP orientation mirror reference (including image clone)",
            3,
            || synthetic.clone().fliph(),
        );
        timed(
            "24MP orientation transpose reference (including image clone)",
            3,
            || synthetic.clone().rotate90().fliph(),
        );
        timed("24MP orientation mirror (including image clone)", 3, || {
            apply_orientation(synthetic.clone(), 2)
        });
        timed(
            "24MP orientation transpose (including image clone)",
            3,
            || apply_orientation(synthetic.clone(), 5),
        );
        timed("Fit dhash", 5, || difference_hash(&rgba));
        let pipeline = MediaPipeline::new(directory.path().join("cache"), 512 * 1024 * 1024);
        timed("best_available reference miss", 200_000, || {
            [PreviewKind::Full, PreviewKind::Fit, PreviewKind::Thumbnail]
                .into_iter()
                .find_map(|kind| pipeline.get_with_size(Path::new("bench.jpg"), 1, 0, kind))
        });
        timed("best_available miss", 200_000, || {
            pipeline.best_available(Path::new("bench.jpg"), 1, 0)
        });
        let key = make_key(
            Path::new("bench.jpg"),
            1,
            jpeg.len() as u64,
            PreviewKind::Fit,
        );
        timed("Fit disk write", 3, || {
            let _ = fs::remove_file(cache_path(&pipeline.shared, &key));
            write_disk(&pipeline.shared, &key, &preview).unwrap();
        });
        println!(
            "Fit cache bytes: {}",
            fs::metadata(cache_path(&pipeline.shared, &key))
                .unwrap()
                .len()
        );
        timed("Fit disk read", 3, || {
            read_disk(&pipeline.shared, &key).unwrap()
        });
        let rgb = rgba.to_rgb8();
        for filter in [
            image::codecs::png::FilterType::Adaptive,
            image::codecs::png::FilterType::Sub,
            image::codecs::png::FilterType::Paeth,
        ] {
            for (channels, raw, color) in [
                (
                    "RGBA",
                    preview.rgba.as_slice(),
                    image::ExtendedColorType::Rgba8,
                ),
                (
                    "RGB",
                    rgb.as_raw().as_slice(),
                    image::ExtendedColorType::Rgb8,
                ),
            ] {
                let mut len = 0;
                timed(&format!("PNG {channels} {filter:?}"), 3, || {
                    let mut bytes = Vec::new();
                    image::codecs::png::PngEncoder::new_with_quality(
                        &mut bytes,
                        image::codecs::png::CompressionType::Fast,
                        filter,
                    )
                    .write_image(raw, preview.width, preview.height, color)
                    .unwrap();
                    len = bytes.len();
                });
                println!("  encoded bytes: {len}");
            }
        }
    }

    #[test]
    fn recent_error_history_is_bounded_serializable_and_nonblocking() {
        let directory = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(directory.path().to_path_buf(), 1024);
        {
            let mut state = pipeline.shared.state.lock().unwrap();
            for index in 0..40 {
                state.record_error(
                    make_key(
                        Path::new(&format!("photo-{index}.arw")),
                        index,
                        123,
                        PreviewKind::Full,
                    ),
                    "é".repeat(ERROR_MESSAGE_LIMIT),
                );
            }
            assert!(pipeline.recent_errors().is_none());
            // Clearing current failures must not erase the independent history.
            state.errors.clear();
        }
        let history = pipeline.recent_errors().unwrap();
        assert_eq!(history.len(), RECENT_ERROR_CAPACITY);
        assert_eq!(history[0].path, Path::new("photo-8.arw"));
        assert_eq!(history[31].path, Path::new("photo-39.arw"));
        assert!(
            history
                .iter()
                .all(|entry| entry.message.len() <= ERROR_MESSAGE_LIMIT)
        );
        assert_eq!(history[31].kind, PreviewKind::Full);
        assert_eq!(history[31].modified_at, 39);
        assert_eq!(history[31].size_bytes, 123);
        assert!(history[31].timestamp_unix_ms > 0);
        assert!(serde_json::to_value(&history).is_ok());
    }

    #[test]
    fn background_admission_failure_has_identifiable_recent_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("too-large.jpg");
        fs::write(&path, [0; 128]).unwrap();
        let pipeline = MediaPipeline::new(directory.path().join("cache"), 32);
        pipeline.request_with_size(&path, 42, 128, PreviewKind::Fit);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            pipeline.request_with_size(&path, 42, 128, PreviewKind::Fit);
            if let Some(history) = pipeline
                .recent_errors()
                .filter(|history| !history.is_empty())
            {
                assert_eq!(history.len(), 1);
                assert_eq!(history[0].path, path);
                assert_eq!(history[0].kind, PreviewKind::Fit);
                assert!(history[0].message.contains("memory"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker failure was not recorded"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn heif_pixel_admission_precedes_native_decode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.heic");
        let encoded = crate::heif::test_header(6000, 4000, 0, 8);
        fs::write(&path, &encoded).unwrap();
        let budget = 64 * 1024 * 1024;
        let pipeline = MediaPipeline::new(directory.path().join("cache"), budget);
        pipeline.request_with_size(&path, 1, encoded.len() as u64, PreviewKind::Fit);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            pipeline.request_with_size(&path, 1, encoded.len() as u64, PreviewKind::Fit);
            if let Some(history) = pipeline
                .recent_errors()
                .filter(|entries| !entries.is_empty())
            {
                assert!(
                    history[0].message.contains("memory"),
                    "{}",
                    history[0].message
                );
                let state = pipeline.shared.state.lock().unwrap();
                assert_eq!(state.stats.decoded, 0);
                assert!(state.stats.peak_estimated_bytes <= budget);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "HEIF admission did not terminate"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn numbered() -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([(1 + y * 3 + x) as u8, 0, 0, 255])
        }))
    }
    fn ids(image: DynamicImage) -> Vec<u8> {
        image.to_rgba8().pixels().map(|p| p[0]).collect()
    }

    #[test]
    fn all_exif_orientations_include_mirrors() {
        let expected = [
            vec![1, 2, 3, 4, 5, 6],
            vec![3, 2, 1, 6, 5, 4],
            vec![6, 5, 4, 3, 2, 1],
            vec![4, 5, 6, 1, 2, 3],
            vec![1, 4, 2, 5, 3, 6],
            vec![4, 1, 5, 2, 6, 3],
            vec![6, 3, 5, 2, 4, 1],
            vec![3, 6, 2, 5, 1, 4],
        ];
        for (i, values) in expected.into_iter().enumerate() {
            let result = apply_orientation(numbered(), (i + 1) as u32);
            assert_eq!(result.dimensions(), if i >= 4 { (2, 3) } else { (3, 2) });
            assert_eq!(ids(result), values, "orientation {}", i + 1);
        }
    }

    #[test]
    fn srgb_profile_roundtrip_preserves_rgb_and_alpha() {
        let icc = Profile::new_srgb().icc().unwrap();
        let mut image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, image::Rgba([103, 57, 211, 77])));
        assert_eq!(
            convert_to_srgb(&mut image, Some(&icc)),
            ColorStatus::ConvertedToSrgb
        );
        for pixel in image.to_rgba8().pixels() {
            for (channel, expected) in pixel.0.into_iter().zip([103, 57, 211, 77]) {
                assert!((i16::from(channel) - expected).abs() <= 1);
            }
        }
    }

    #[test]
    fn linear_grayscale_profile_changes_midtones_to_srgb() {
        let white = lcms2::CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let curve = lcms2::ToneCurve::new(1.0);
        let profile = Profile::new_gray(&white, &curve).unwrap().icc().unwrap();
        let mut image =
            DynamicImage::ImageLuma8(image::GrayImage::from_pixel(1, 1, image::Luma([128])));
        assert_eq!(
            convert_to_srgb(&mut image, Some(&profile)),
            ColorStatus::ConvertedToSrgb
        );
        let pixel = image.to_rgb8().get_pixel(0, 0).0;
        for channel in pixel {
            assert!(
                (185..=191).contains(&channel),
                "linear midgray should become about 188, got {channel}"
            );
        }
    }

    #[test]
    fn invalid_profile_is_reported_without_losing_pixels() {
        let mut image = numbered();
        assert!(matches!(
            convert_to_srgb(&mut image, Some(b"bad ICC")),
            ColorStatus::InvalidProfile(_)
        ));
        assert_eq!(ids(image), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn cache_is_strictly_byte_bounded_and_keeps_recent_entry() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().to_path_buf(), 20_000);
        let mut state = pipeline.shared.state.lock().unwrap();
        let first = make_key(Path::new("one"), 0, 0, PreviewKind::Thumbnail);
        let second = make_key(Path::new("two"), 0, 0, PreviewKind::Thumbnail);
        let preview = Arc::new(make_preview(
            numbered(),
            PreviewKind::Thumbnail,
            3,
            2,
            ColorStatus::AssumedSrgb,
            false,
        ));
        assert!(state.insert_preview(first.clone(), preview.clone()));
        assert!(state.insert_preview(second.clone(), preview));
        assert!(state.memory <= state.budget);
        assert!(state.previews.contains_key(&second));
        // Each histogram is 8192 bytes, so a 20KB budget admits two; a third evicts the oldest.
        let third = make_key(Path::new("three"), 0, 0, PreviewKind::Thumbnail);
        let preview = Arc::new(make_preview(
            numbered(),
            PreviewKind::Thumbnail,
            3,
            2,
            ColorStatus::AssumedSrgb,
            false,
        ));
        state.insert_preview(third.clone(), preview);
        assert!(!state.previews.contains_key(&first));
        assert!(state.memory <= state.budget);
    }

    #[test]
    fn persistent_preview_survives_original_going_offline() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.png");
        numbered().save(&source).unwrap();
        let size = fs::metadata(&source).unwrap().len();
        let pipeline = MediaPipeline::new(temp.path().join("cache"), 1_000_000);
        let key = make_key(&source, 123, size, PreviewKind::Fit);
        let preview = decode_preview(&source, PreviewKind::Fit).unwrap();
        write_disk(&pipeline.shared, &key, &preview).unwrap();
        fs::remove_file(&source).unwrap();
        let cached = read_disk(&pipeline.shared, &key).unwrap();
        assert_eq!(cached.rgba, preview.rgba);
        assert!(
            read_disk(
                &pipeline.shared,
                &make_key(&source, 124, size, PreviewKind::Fit)
            )
            .is_none()
        );
        assert!(
            read_disk(
                &pipeline.shared,
                &make_key(&source, 123, size + 1, PreviewKind::Fit)
            )
            .is_none()
        );
    }

    #[test]
    fn histogram_counts_and_hash_are_stable() {
        let image =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(32, 32, image::Rgb([255, 0, 0])));
        let h = histogram(&image);
        assert_eq!(h.red[255], 1024);
        assert_eq!(h.green[0], 1024);
        assert_eq!(h.red.iter().sum::<u64>(), h.samples);
        assert_eq!(difference_hash(&image), 0);
    }

    #[test]
    fn queue_is_bounded_and_fit_displaces_thumbnail() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().to_path_buf(), 1_000_000);
        // Hold both source stripes so workers cannot consume more than two requests.
        let guards: Vec<_> = pipeline
            .shared
            .source_locks
            .iter()
            .map(|lock| lock.lock().unwrap())
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut i = 0;
        while pipeline.shared.state.lock().unwrap().queue.len() < QUEUE_CAPACITY {
            pipeline.request(
                Path::new(&format!("file-{i}.jpg")),
                0,
                PreviewKind::Thumbnail,
            );
            i += 1;
            assert!(std::time::Instant::now() < deadline);
        }
        loop {
            pipeline.request(Path::new("selected.jpg"), 0, PreviewKind::Fit);
            if pipeline
                .shared
                .state
                .lock()
                .unwrap()
                .pending
                .contains_key(&make_key(Path::new("selected.jpg"), 0, 0, PreviewKind::Fit))
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
        }
        let state = pipeline.shared.state.lock().unwrap();
        assert!(state.queue.len() <= QUEUE_CAPACITY);
        assert!(state.pending.contains_key(&make_key(
            Path::new("selected.jpg"),
            0,
            0,
            PreviewKind::Fit
        )));
        drop(state);
        drop(guards);
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([90, 130, 170]),
        ));
        let mut bytes = Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn raw_extraction_selects_largest_and_ignores_nested_exif_eoi() {
        let small = jpeg(160, 120);
        let large = jpeg(640, 480);
        // APP1 contains a JPEG-looking sequence; its EOI must not end the outer preview.
        let mut nested = large[..2].to_vec();
        nested.extend_from_slice(&[0xff, 0xe1, 0, 10, 0xff, 0xd8, 0xff, 0xd9, 1, 2, 3, 4]);
        nested.extend_from_slice(&large[2..]);
        let mut raw = b"RAW container prefix".to_vec();
        raw.extend(small);
        raw.extend_from_slice(b"gap");
        raw.extend_from_slice(&nested);
        raw.extend_from_slice(b"container trailing bytes with no more JPEG markers");
        let best = extract_raw_jpeg(&raw).unwrap();
        assert_eq!(best, nested);
        let preview = decode_preview_bytes(&raw, Path::new("photo.nef"), PreviewKind::Fit).unwrap();
        assert!(preview.embedded_raw);
        assert_eq!((preview.width, preview.height), (640, 480));
    }

    #[test]
    fn disk_quota_and_progressive_source_reads_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.jpg");
        fs::write(&source, jpeg(320, 200)).unwrap();
        let size = fs::metadata(&source).unwrap().len();
        let pipeline = MediaPipeline::new(temp.path().join("cache"), 8_000_000);
        for kind in [PreviewKind::Fit, PreviewKind::Full] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                pipeline.request_with_size(&source, 1, size, kind);
                if pipeline.get_with_size(&source, 1, size, kind).is_some() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "pipeline did not complete: {:?}",
                    pipeline.error_with_size(&source, 1, size, kind)
                );
                thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // stats() is try_lock-based for the UI and may report zeros while a
        // worker holds the lock; assert under the real lock instead.
        assert_eq!(
            pipeline.shared.state.lock().unwrap().stats.source_reads,
            1,
            "progressive decode should reuse compressed original"
        );
        pipeline.set_disk_budget(1);
        let preview = decode_preview(&source, PreviewKind::Fit).unwrap();
        write_disk(
            &pipeline.shared,
            &make_key(&source, 1, size, PreviewKind::Fit),
            &preview,
        )
        .unwrap();
        assert!(pipeline.shared.disk_bytes.load(Ordering::Relaxed) <= 1);
    }

    fn await_preview(
        pipeline: &MediaPipeline,
        path: &Path,
        size: u64,
        kind: PreviewKind,
    ) -> Arc<Preview> {
        await_preview_with_timeout(
            pipeline,
            path,
            size,
            kind,
            std::time::Duration::from_secs(15),
        )
    }

    fn await_preview_with_timeout(
        pipeline: &MediaPipeline,
        path: &Path,
        size: u64,
        kind: PreviewKind,
        timeout: std::time::Duration,
    ) -> Arc<Preview> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            pipeline.request_with_size(path, 1, size, kind);
            if let Some(preview) = pipeline.get_with_size(path, 1, size, kind) {
                return preview;
            }
            if let Some(error) = pipeline.error_with_size(path, 1, size, kind) {
                panic!("{}", error.message);
            }
            assert!(std::time::Instant::now() < deadline, "pipeline timeout");
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn evicted_external_arcs_remain_charged_until_released() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 20_000);
        let held = Arc::new(make_preview(
            numbered(),
            PreviewKind::Full,
            3,
            2,
            ColorStatus::AssumedSrgb,
            false,
        ));
        let mut state = pipeline.shared.state.lock().unwrap();
        state.insert_preview(
            make_key(Path::new("one"), 1, 0, PreviewKind::Full),
            held.clone(),
        );
        state.insert_preview(
            make_key(Path::new("two"), 1, 0, PreviewKind::Full),
            Arc::new(make_preview(
                numbered(),
                PreviewKind::Full,
                3,
                2,
                ColorStatus::AssumedSrgb,
                false,
            )),
        );
        assert!(!state.evict_for(17_000));
        assert_eq!(state.memory, held.byte_len());
        assert_eq!(state.retired.len(), 1);
        drop(held);
        assert!(state.evict_for(17_000));
        assert_eq!(state.memory, 0);
    }

    #[test]
    fn retained_full_satisfies_fit_without_redecoding() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 1_000_000);
        let path = Path::new("offline.jpg");
        pipeline.shared.state.lock().unwrap().insert_preview(
            make_key(path, 1, 3, PreviewKind::Full),
            Arc::new(make_preview(
                numbered(),
                PreviewKind::Full,
                3,
                2,
                ColorStatus::AssumedSrgb,
                false,
            )),
        );
        let preview = await_preview(&pipeline, path, 3, PreviewKind::Fit);
        assert_eq!(preview.kind, PreviewKind::Full);
        assert_eq!(pipeline.shared.state.lock().unwrap().stats.source_reads, 0);
        assert!(pipeline.shared.state.lock().unwrap().queue.is_empty());
    }

    #[test]
    fn cancellation_applies_generation_and_queue_atomically_after_contention() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 1_000_000);
        let state = pipeline.shared.state.lock().unwrap();
        let generation = pipeline.shared.generation.load(Ordering::Relaxed);
        pipeline.cancel_pending();
        assert_eq!(
            pipeline.shared.generation.load(Ordering::Relaxed),
            generation
        );
        drop(state);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while pipeline.shared.cancel_requested.load(Ordering::Relaxed) {
            pipeline.request(Path::new("current.jpg"), 1, PreviewKind::Fit);
            assert!(std::time::Instant::now() < deadline);
        }
        assert_eq!(
            pipeline.shared.generation.load(Ordering::Relaxed),
            generation + 1
        );
        let state = pipeline.shared.state.lock().unwrap();
        assert!(
            state
                .queue
                .iter()
                .all(|task| task.generation == generation + 1)
        );
    }

    #[test]
    fn simultaneous_large_requests_share_total_working_budget() {
        let temp = tempfile::tempdir().unwrap();
        let encoded = jpeg(1024, 1024);
        let paths = [
            temp.path().join("first.jpg"),
            temp.path().join("second.jpg"),
        ];
        for path in &paths {
            fs::write(path, &encoded).unwrap();
        }
        let budget = 32 * 1024 * 1024;
        let pipeline = MediaPipeline::new(temp.path().join("cache"), budget);
        for path in &paths {
            pipeline.request_with_size(path, 1, encoded.len() as u64, PreviewKind::Full);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut done = [false; 2];
        while !done.iter().all(|v| *v) {
            for (i, path) in paths.iter().enumerate() {
                if !done[i] {
                    pipeline.request_with_size(path, 1, encoded.len() as u64, PreviewKind::Full);
                    done[i] = pipeline
                        .get_with_size(path, 1, encoded.len() as u64, PreviewKind::Full)
                        .is_some();
                    assert!(
                        pipeline
                            .error_with_size(path, 1, encoded.len() as u64, PreviewKind::Full)
                            .is_none()
                    );
                }
            }
            let stats = pipeline.stats();
            assert!(stats.memory_bytes + stats.inflight_bytes <= budget);
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let stats = pipeline.stats();
        assert!(stats.peak_estimated_bytes <= budget);
        assert_eq!(stats.source_reads, 2);
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            eprintln!(
                "simultaneous decode: reserved peak {} MiB; {}",
                stats.peak_estimated_bytes / 1024 / 1024,
                status
                    .lines()
                    .find(|line| line.starts_with("VmHWM:"))
                    .unwrap_or("RSS unavailable")
            );
        }
    }

    #[test]
    fn twenty_megapixel_jpeg_comparison_fits_explicit_512_mib_budget() {
        let temp = tempfile::tempdir().unwrap();
        // A normal 20 MP camera frame, rather than a small image standing in
        // for it. Keep both source previews and both display copies alive.
        let dimensions = (5472, 3648);
        let encoded = jpeg(dimensions.0, dimensions.1);
        let paths = [
            temp.path().join("first.jpg"),
            temp.path().join("second.jpg"),
        ];
        for path in &paths {
            fs::write(path, &encoded).unwrap();
        }
        let budget = 512 * 1024 * 1024;
        let pipeline = MediaPipeline::new(temp.path().join("cache"), budget);
        let mut held = Vec::new();
        for path in &paths {
            // This verifies memory admission, not decode latency. Debug builds
            // under concurrent CI/compiler load can take >15 s for 20 MP.
            let preview = await_preview_with_timeout(
                &pipeline,
                path,
                encoded.len() as u64,
                PreviewKind::Full,
                std::time::Duration::from_secs(60),
            );
            assert_eq!((preview.width, preview.height), dimensions);
            assert!(!preview.rendered_raw);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let reservation = loop {
                if let Some(reservation) = pipeline.reserve_display_bytes(preview.rgba.len()) {
                    break reservation;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "display admission timeout"
                );
                thread::sleep(std::time::Duration::from_millis(5));
            };
            let mut display = preview.rgba.clone();
            for pixel in display.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            held.push((preview, display, reservation));
        }
        let state = pipeline.shared.state.lock().unwrap();
        assert_eq!(held.len(), 2);
        assert_eq!(
            state.display,
            2 * dimensions.0 as usize * dimensions.1 as usize * 4
        );
        assert!(state.memory + state.inflight <= budget);
        assert!(state.stats.peak_estimated_bytes <= budget);
        assert_eq!(state.stats.source_reads, 2);
        assert!(state.recent_errors.is_empty());
    }

    #[test]
    fn foreground_retry_recovers_memory_failure_but_keeps_corrupt_source_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("next.jpg");
        let encoded = jpeg(320, 200);
        fs::write(&path, &encoded).unwrap();
        let photo = crate::model::Photo {
            path: path.clone(),
            size_bytes: encoded.len() as u64,
            modified_at: 1,
            ..Default::default()
        };
        let budget = 4 * 1024 * 1024;
        let pipeline = MediaPipeline::new(temp.path().join("cache"), budget);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let occupied = loop {
            if let Some(reservation) = pipeline.reserve_display_bytes(budget) {
                break reservation;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "display contention timeout"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        };
        pipeline.request_photo(&photo, PreviewKind::Full);
        loop {
            pipeline.request_photo(&photo, PreviewKind::Full);
            if let Some(error) = pipeline.photo_error(&photo, PreviewKind::Full) {
                assert!(error.message.to_ascii_lowercase().contains("memory"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "admission failure timeout"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(occupied);
        while !pipeline.retry_photo_memory_error(&photo, PreviewKind::Full) {
            assert!(
                std::time::Instant::now() < deadline,
                "retry contention timeout"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let preview = await_preview(&pipeline, &path, encoded.len() as u64, PreviewKind::Full);
        assert_eq!((preview.width, preview.height), (320, 200));

        let corrupt = crate::model::Photo {
            path: temp.path().join("corrupt.jpg"),
            size_bytes: 4,
            modified_at: 1,
            ..Default::default()
        };
        fs::write(&corrupt.path, b"\xff\xd8\xff\xd9").unwrap();
        pipeline.request_photo(&corrupt, PreviewKind::Full);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let original_error = loop {
            pipeline.request_photo(&corrupt, PreviewKind::Full);
            if let Some(error) = pipeline.photo_error(&corrupt, PreviewKind::Full) {
                break error.message;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "corrupt-source failure timeout"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(!pipeline.retry_photo_memory_error(&corrupt, PreviewKind::Full));
        assert_eq!(
            pipeline.shared.state.lock().unwrap().errors[&photo_key(&corrupt, PreviewKind::Full)]
                .message,
            original_error,
        );
    }

    #[test]
    fn offline_pipeline_uses_disk_without_original_reads_and_skips_rewrites() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("offline.png");
        numbered().save(&source).unwrap();
        let size = fs::metadata(&source).unwrap().len();
        let cache = temp.path().join("cache");
        let pipeline = MediaPipeline::new(cache.clone(), 8_000_000);
        let key = make_key(&source, 1, size, PreviewKind::Fit);
        let preview = decode_preview(&source, PreviewKind::Fit).unwrap();
        write_disk(&pipeline.shared, &key, &preview).unwrap();
        write_disk(&pipeline.shared, &key, &preview).unwrap();
        assert_eq!(pipeline.shared.state.lock().unwrap().stats.disk_writes, 1);
        drop(pipeline);
        fs::remove_file(&source).unwrap();
        let pipeline = MediaPipeline::new(cache, 8_000_000);
        let result = await_preview(&pipeline, &source, size, PreviewKind::Fit);
        assert_eq!(result.rgba, preview.rgba);
        assert_eq!(pipeline.shared.state.lock().unwrap().stats.source_reads, 0);
        assert_eq!(pipeline.shared.state.lock().unwrap().stats.disk_hits, 1);
    }

    #[test]
    fn raw_source_retains_only_compact_preview_and_container_orientation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("camera.nef");
        let mut raw = vec![0; 4 * 1024 * 1024];
        let tiff = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        raw[..tiff.len()].copy_from_slice(tiff);
        raw.extend(jpeg(640, 480));
        fs::write(&path, &raw).unwrap();
        let pipeline = MediaPipeline::new(temp.path().join("cache"), 32 * 1024 * 1024);
        let preview = await_preview(&pipeline, &path, raw.len() as u64, PreviewKind::Fit);
        assert_eq!((preview.width, preview.height), (480, 640));
        assert!(preview.embedded_raw);
        let state = pipeline.shared.state.lock().unwrap();
        let source = state
            .sources
            .get(&SourceKey {
                remote: None,
                sensor_raw: false,
                path,
                modified: 1,
                size: raw.len() as u64,
            })
            .unwrap();
        assert!(source.bytes < 1024 * 1024);
        assert!(state.memory + state.inflight <= state.budget);
    }

    #[test]
    fn raw_prefix_at_entire_budget_rejects_compaction_without_self_deadlock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.nef");
        let mut file = File::create(&path).unwrap();
        file.write_all(&jpeg(320, 200)).unwrap();
        file.set_len(RAW_SCAN_LIMIT + 4096).unwrap();
        drop(file);
        let pipeline = MediaPipeline::new(temp.path().join("cache"), RAW_SCAN_LIMIT as usize);
        let key = make_key(&path, 1, RAW_SCAN_LIMIT + 4096, PreviewKind::Fit);
        let task = Task {
            key: key.clone(),
            generation: 0,
            sequence: 0,
        };
        let shared = pipeline.shared.clone();
        let (send, receive) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let result = source_bytes(&shared, &key.source, &task);
            let _ = send.send(result.err().map(|e| e.to_string()));
        });
        let error = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("RAW compaction waited on its own reservation")
            .expect("RAW growth should reject an impossible combined reservation");
        assert!(error.contains("memory"));
        let state = pipeline.shared.state.lock().unwrap();
        assert_eq!(state.inflight, 0);
        assert!(state.stats.peak_estimated_bytes <= state.budget);
    }

    #[test]
    fn two_held_prefixes_never_wait_on_each_others_growth() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 128 * 1024 * 1024);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (send, receive) = std::sync::mpsc::channel();
        for index in 0..2 {
            let shared = pipeline.shared.clone();
            let barrier = barrier.clone();
            let send = send.clone();
            thread::spawn(move || {
                let task = Task {
                    key: make_key(
                        Path::new(&format!("raw-{index}.nef")),
                        1,
                        0,
                        PreviewKind::Fit,
                    ),
                    generation: 0,
                    sequence: index,
                };
                let prefix = reserve(&shared, &task, 60 * 1024 * 1024).unwrap();
                barrier.wait();
                let growth = reserve_growth(&shared, &task, 10 * 1024 * 1024, prefix.bytes);
                let accepted = growth.is_ok();
                drop(growth);
                drop(prefix);
                send.send(accepted).unwrap();
            });
        }
        barrier.wait();
        for _ in 0..2 {
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("held reservations deadlocked during concurrent growth");
        }
        let state = pipeline.shared.state.lock().unwrap();
        assert_eq!(state.inflight, 0);
        assert!(state.stats.peak_estimated_bytes <= state.budget);
    }

    #[test]
    fn forged_small_cache_metadata_cannot_admit_a_large_png_decode() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 8_000_000);
        let source = temp.path().join("disconnected.png");
        let key = make_key(&source, 1, 123, PreviewKind::Fit);
        let preview = make_preview(
            numbered(),
            PreviewKind::Fit,
            3,
            2,
            ColorStatus::AssumedSrgb,
            false,
        );
        write_disk(&pipeline.shared, &key, &preview).unwrap();
        let path = cache_path(&pipeline.shared, &key);
        let mut cache = fs::read(&path).unwrap();
        let metadata_len = u32::from_le_bytes(cache[8..12].try_into().unwrap()) as usize;
        cache.truncate(12 + metadata_len);
        let large = numbered().resize_exact(1024, 1024, FilterType::Nearest);
        let mut png = Cursor::new(Vec::new());
        large.write_to(&mut png, image::ImageFormat::Png).unwrap();
        cache.extend(png.into_inner());
        fs::write(&path, &cache).unwrap();
        assert!(read_disk_with_limit(&pipeline.shared, &key, 512 * 1024).is_none());
        assert!(!path.exists());
        assert_eq!(pipeline.stats().decoded, 0);
        assert_eq!(pipeline.stats().disk_hits, 0);
    }

    #[test]
    fn corrupt_cache_headers_are_removed_and_replaced_from_the_original() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("original.jpg");
        fs::write(&source, jpeg(320, 200)).unwrap();
        let size = fs::metadata(&source).unwrap().len();
        let key = make_key(&source, 1, size, PreviewKind::Fit);
        let expected = decode_preview(&source, PreviewKind::Fit).unwrap();
        for corruption in ["magic", "json", "source dimensions", "oversized"] {
            let pipeline = MediaPipeline::new(temp.path().join(corruption), 8 * 1024 * 1024);
            write_disk(&pipeline.shared, &key, &expected).unwrap();
            let path = cache_path(&pipeline.shared, &key);
            match corruption {
                "magic" => fs::write(&path, b"BROKEN00").unwrap(),
                "json" => {
                    let mut bytes = CACHE_MAGIC.to_vec();
                    bytes.extend_from_slice(&1u32.to_le_bytes());
                    bytes.push(b'{');
                    fs::write(&path, bytes).unwrap();
                }
                "source dimensions" => {
                    let mut bytes = fs::read(&path).unwrap();
                    let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
                    let mut meta: DiskMeta =
                        serde_json::from_slice(&bytes[12..12 + length]).unwrap();
                    meta.source_width = 0;
                    let invalid = serde_json::to_vec(&meta).unwrap();
                    bytes.splice(12..12 + length, invalid.iter().copied());
                    bytes[8..12].copy_from_slice(&(invalid.len() as u32).to_le_bytes());
                    fs::write(&path, bytes).unwrap();
                }
                "oversized" => File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(CACHE_FILE_LIMIT as u64 + 1)
                    .unwrap(),
                _ => unreachable!(),
            }
            assert!(open_disk(&pipeline.shared, &key).is_none(), "{corruption}");
            assert!(!path.exists(), "Unrepairable {corruption} cache retained");
            // Exercise real source admission/publication after rejecting the cache.
            let task = Task {
                key: key.clone(),
                generation: 0,
                sequence: 0,
            };
            process_task(&pipeline.shared, &task).unwrap();
            let actual = pipeline
                .shared
                .state
                .lock()
                .unwrap()
                .previews
                .get(&key)
                .unwrap()
                .value
                .clone();
            assert_eq!(actual.rgba, expected.rgba, "{corruption}");
            write_disk(&pipeline.shared, &key, &actual).unwrap();
            assert_eq!(
                read_disk(&pipeline.shared, &key).unwrap().rgba,
                expected.rgba
            );
        }
    }

    #[test]
    fn external_display_reservations_share_budget_and_drop_without_locking() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<DisplayReservation>();
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 1_000_000);
        let display = loop {
            if let Some(token) = pipeline.reserve_display_bytes(600_000) {
                break token;
            }
            thread::yield_now();
        };
        assert!(pipeline.reserve_display_bytes(500_000).is_none());
        let state = pipeline.shared.state.lock().unwrap();
        assert_eq!(state.display, 600_000);
        drop(display);
        assert_eq!(
            pipeline.shared.released_display.load(Ordering::Acquire),
            600_000
        );
        drop(state);
        assert_eq!(pipeline.stats().display_bytes, 0);
        let replacement = loop {
            if let Some(token) = pipeline.reserve_display_bytes(900_000) {
                break token;
            }
            thread::yield_now();
        };
        assert_eq!(replacement.bytes(), 900_000);
        assert!(pipeline.stats().peak_estimated_bytes <= 1_000_000);
    }

    #[test]
    fn lowering_budget_tracks_live_display_until_real_release() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = MediaPipeline::new(temp.path().into(), 1_000_000);
        let display = loop {
            if let Some(token) = pipeline.reserve_display_bytes(600_000) {
                break token;
            }
            thread::yield_now();
        };
        let state = pipeline.shared.state.lock().unwrap();
        assert!(!pipeline.set_memory_budget(500_000));
        drop(state);
        while !pipeline.set_memory_budget(500_000) {
            thread::yield_now();
        }
        {
            let state = pipeline.shared.state.lock().unwrap();
            assert_eq!(state.budget, 500_000);
            assert_eq!(state.inflight, 600_000);
        }
        assert!(pipeline.reserve_display_bytes(1).is_none());
        drop(display);
        while !pipeline.set_memory_budget(500_000) {
            thread::yield_now();
        }
        let state = pipeline.shared.state.lock().unwrap();
        assert_eq!(state.inflight, 0);
    }
}
