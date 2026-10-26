//! Explicit, read-only exact-duplicate scans. One cooperative worker, bounded
//! sample/stream buffers, and a durable local fingerprint cache; never auto-trash.
use crate::model::Photo;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, UNIX_EPOCH},
};

const SAMPLE: usize = 16 * 1024;
const CHUNK: usize = 1024 * 1024;
const MAX_ERRORS: usize = 64;
const MAX_GROUPS: usize = 1000;
const MAX_RESULT_IDS: usize = 10_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DuplicateScanState {
    pub id: u64,
    pub phase: String,
    pub stage: String,
    pub total: usize,
    pub candidates: usize,
    pub processed: usize,
    pub bytes_read: u64,
    pub cache_hits: usize,
    pub groups: Vec<Vec<i64>>,
    pub errors: Vec<String>,
    pub errors_omitted: usize,
    pub truncated: bool,
}
impl DuplicateScanState {
    fn error(&mut self, error: impl std::fmt::Display) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(error.to_string());
        } else {
            self.errors_omitted += 1;
        }
    }
}

pub struct DuplicateJob {
    cancelled: Arc<AtomicBool>,
    updates: Arc<Mutex<Option<DuplicateScanState>>>,
    worker: Option<JoinHandle<()>>,
}
impl DuplicateJob {
    pub fn start(id: u64, photos: Vec<Photo>, catalog_path: PathBuf) -> Result<Self> {
        for photo in photos.iter().filter(|p| !p.trashed) {
            photo.require_local("Duplicate scanning")?;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let updates = Arc::new(Mutex::new(None));
        let stop = cancelled.clone();
        let output = updates.clone();
        let worker = thread::Builder::new()
            .name("shadowroom-duplicates".into())
            .spawn(move || {
                let mut state = DuplicateScanState {
                    id,
                    phase: "running".into(),
                    stage: "sizing".into(),
                    ..Default::default()
                };
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scan(photos, &catalog_path, &stop, &mut state, |state| {
                        *output.lock().unwrap_or_else(|p| p.into_inner()) = Some(state.clone());
                    })
                }));
                match outcome {
                    Ok(Ok(())) => {
                        state.phase = if stop.load(Ordering::Acquire) {
                            "cancelled"
                        } else {
                            "completed"
                        }
                        .into();
                    }
                    Ok(Err(error)) => {
                        state.error(format!("Duplicate scan: {error:#}"));
                        state.phase = "failed".into();
                    }
                    Err(_) => {
                        state.error("Duplicate scan worker panicked");
                        state.phase = "failed".into();
                    }
                }
                *output.lock().unwrap_or_else(|p| p.into_inner()) = Some(state);
            })
            .context("Start duplicate scan worker")?;
        Ok(Self {
            cancelled,
            updates,
            worker: Some(worker),
        })
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn take_update(&self) -> Option<DuplicateScanState> {
        self.updates
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn join(mut self) -> Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Duplicate worker did not stop cleanly"))?;
        }
        Ok(())
    }
}
impl Drop for DuplicateJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
struct Source {
    path: PathBuf,
    size: u64,
    fingerprint: String,
}
impl Source {
    fn read(path: &Path) -> Result<Self> {
        let metadata =
            fs::symlink_metadata(path).with_context(|| format!("Inspect {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("Not a regular source file: {}", path.display());
        }
        Ok(Self {
            path: path.into(),
            size: metadata.len(),
            fingerprint: fingerprint(&metadata)?,
        })
    }
    fn validate(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)
            .with_context(|| format!("Inspect {}", self.path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("Not a regular source file: {}", self.path.display());
        }
        if fingerprint(&metadata)? != self.fingerprint {
            bail!(
                "Source changed during duplicate scan: {}",
                self.path.display()
            );
        }
        Ok(())
    }
    fn open(&self) -> Result<File> {
        self.validate()?;
        let file = crate::fsutil::open_regular(&self.path)?;
        if fingerprint(&file.metadata()?)? != self.fingerprint {
            bail!(
                "Source replaced during duplicate scan: {}",
                self.path.display()
            );
        }
        Ok(file)
    }
}
fn fingerprint(metadata: &Metadata) -> Result<String> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH).map_or_else(
        |e| format!("-{}", e.duration().as_nanos()),
        |d| d.as_nanos().to_string(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "v1:{}:{modified}:{}:{}:{}:{}",
            metadata.len(),
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        let created = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        Ok(format!("v1:{}:{modified}:{created:?}", metadata.len()))
    }
}
struct Candidate {
    id: i64,
    sources: Vec<Source>,
}
#[derive(Clone)]
struct Record {
    path: PathBuf,
    fingerprint: String,
    sample: String,
    full: Option<String>,
}
struct Cache {
    connection: Connection,
    pending: HashMap<PathBuf, Record>,
}
impl Cache {
    fn is_current_asset(&self, candidate: &Candidate) -> Result<bool> {
        let current: Option<(String, Option<String>, bool)> = self
            .connection
            .prepare_cached(
                "SELECT path,json_extract(data,'$.companion_path'),trashed FROM photos WHERE id=?1",
            )?
            .query_row([candidate.id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let Some((path, companion, trashed)) = current else {
            return Ok(false);
        };
        Ok(!trashed
            && Path::new(&path) == candidate.sources[0].path
            && companion.as_deref().map(Path::new)
                == candidate.sources.get(1).map(|source| source.path.as_path()))
    }
    fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        // A rebuildable scan may fail/retry rather than make decision writes wait
        // behind a long cache transaction. No transaction spans any source I/O.
        connection.busy_timeout(Duration::from_millis(25))?;
        connection.execute_batch("PRAGMA synchronous=FULL;")?;
        Ok(Self {
            connection,
            pending: HashMap::new(),
        })
    }
    fn get(&self, source: &Source) -> Result<Option<Record>> {
        if let Some(record) = self
            .pending
            .get(&source.path)
            .filter(|r| r.fingerprint == source.fingerprint)
        {
            return Ok(Some(record.clone()));
        }
        self.connection
            .prepare_cached(
                "SELECT sample,full_hash FROM exact_hashes WHERE path=?1 AND fingerprint=?2",
            )?
            .query_row(
                params![source.path.to_string_lossy(), source.fingerprint],
                |row| {
                    Ok(Record {
                        path: source.path.clone(),
                        fingerprint: source.fingerprint.clone(),
                        sample: row.get(0)?,
                        full: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
    fn put(&mut self, record: Record) -> Result<()> {
        // Preserve the batch bound even if a caller handles a failed flush and
        // tries another record while a different SQLite writer holds the lock.
        if self.pending.len() >= 32 {
            self.flush()?;
        }
        self.pending.insert(record.path.clone(), record);
        if self.pending.len() >= 32 {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached("INSERT INTO exact_hashes(path,fingerprint,sample,full_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(path) DO UPDATE SET fingerprint=excluded.fingerprint,sample=excluded.sample,full_hash=excluded.full_hash")?;
            for record in self.pending.values() {
                statement.execute(params![
                    record.path.to_string_lossy(),
                    record.fingerprint,
                    record.sample,
                    record.full
                ])?;
            }
        }
        transaction.commit()?;
        self.pending.clear();
        Ok(())
    }
}
fn stopped(cancelled: &AtomicBool) -> bool {
    cancelled.load(Ordering::Acquire)
}
fn sample(
    source: &Source,
    state: &mut DuplicateScanState,
    cancelled: &AtomicBool,
) -> Result<(String, Option<String>)> {
    let mut file = source.open()?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"shadowroom-sample-v1");
    hash.update(&source.size.to_le_bytes());
    // Already ordered; fixed storage avoids a heap allocation per source.
    let offsets = [
        0,
        source.size.saturating_sub(SAMPLE as u64) / 2,
        source.size.saturating_sub(SAMPLE as u64),
    ];
    let mut buffer = [0u8; SAMPLE * 3];
    let small = source.size <= buffer.len() as u64;
    let full = if small {
        if stopped(cancelled) {
            bail!("Scan cancelled");
        }
        let length = source.size as usize;
        file.read_exact(&mut buffer[..length])?;
        state.bytes_read += length as u64;
        // The overlapping sample windows cover this entire source. Keep its
        // full digest, so verification never reopens/rereads these small files.
        Some(blake3::hash(&buffer[..length]).to_hex().to_string())
    } else {
        None
    };
    for (i, offset) in offsets.into_iter().enumerate() {
        if i > 0 && offset == offsets[i - 1] {
            continue;
        }
        if stopped(cancelled) {
            bail!("Scan cancelled");
        }
        let length = source.size.saturating_sub(offset).min(SAMPLE as u64) as usize;
        let bytes = if small {
            &buffer[offset as usize..offset as usize + length]
        } else {
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buffer[..length])?;
            state.bytes_read += length as u64;
            &buffer[..length]
        };
        hash.update(&offset.to_le_bytes());
        hash.update(bytes);
    }
    if fingerprint(&file.metadata()?)? != source.fingerprint {
        bail!("Source changed while sampling: {}", source.path.display());
    }
    source.validate()?;
    Ok((hash.finalize().to_hex().to_string(), full))
}

fn full_hash(
    source: &Source,
    state: &mut DuplicateScanState,
    cancelled: &AtomicBool,
    buffer: &mut [u8],
    progress: &mut impl FnMut(&DuplicateScanState),
    update_at: &mut Instant,
) -> Result<String> {
    let mut file = source.open()?;
    let mut hash = blake3::Hasher::new();
    let mut remaining = source.size;
    while remaining > 0 {
        if stopped(cancelled) {
            bail!("Scan cancelled");
        }
        let length = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..length])?;
        hash.update(&buffer[..length]);
        state.bytes_read += length as u64;
        remaining -= length as u64;
        // Cooperative background priority, bounded to one reader. Never hold a
        // catalog mutex/SQLite transaction while yielding or touching originals.
        if update_at.elapsed() >= Duration::from_millis(100) {
            progress(state);
            *update_at = Instant::now();
        }
        thread::yield_now();
    }
    if fingerprint(&file.metadata()?)? != source.fingerprint {
        bail!("Source changed while hashing: {}", source.path.display());
    }
    source.validate()?;
    Ok(hash.finalize().to_hex().to_string())
}

fn scan(
    mut photos: Vec<Photo>,
    path: &Path,
    cancelled: &AtomicBool,
    state: &mut DuplicateScanState,
    mut progress: impl FnMut(&DuplicateScanState),
) -> Result<()> {
    photos.retain(|photo| !photo.trashed);
    photos.sort_unstable_by_key(|photo| photo.id);
    photos.dedup_by_key(|photo| photo.id);
    state.total = photos.len();
    progress(state);
    if stopped(cancelled) {
        return Ok(());
    }
    let mut cache = Cache::open(path)?;
    let mut sizes = HashMap::<(u64, Option<u64>), Vec<Candidate>>::new();
    let mut update_at = Instant::now();
    for photo in photos {
        if stopped(cancelled) {
            return Ok(());
        }
        let candidate = (|| -> Result<Candidate> {
            photo.require_local("Duplicate scanning")?;
            let mut sources = vec![Source::read(&photo.path)?];
            if let Some(companion) = &photo.companion_path {
                sources.push(Source::read(companion)?);
            }
            Ok(Candidate {
                id: photo.id,
                sources,
            })
        })();
        match candidate {
            Ok(candidate) => {
                sizes
                    .entry((
                        candidate.sources[0].size,
                        candidate.sources.get(1).map(|source| source.size),
                    ))
                    .or_default()
                    .push(candidate);
            }
            Err(error) => state.error(format!("{}: {error:#}", photo.path.display())),
        }
        if update_at.elapsed() >= Duration::from_millis(100) {
            progress(state);
            update_at = Instant::now();
        }
        thread::yield_now();
    }
    // Size collection is expected O(n). Sort only non-singleton buckets to
    // retain the prior deterministic scan/error order; unique sizes need no
    // source reads and no O(n log n) tree insertions.
    let mut buckets: Vec<_> = sizes
        .into_iter()
        .filter(|(_, values)| values.len() > 1)
        .collect();
    buckets.sort_unstable_by_key(|(key, _)| *key);
    let candidates: Vec<_> = buckets.into_iter().flat_map(|(_, values)| values).collect();
    state.candidates = candidates.len();
    state.stage = "sampling".into();
    progress(state);
    let mut samples = BTreeMap::<Vec<String>, Vec<Candidate>>::new();
    for candidate in candidates {
        if stopped(cancelled) {
            cache.flush()?;
            return Ok(());
        }
        let result = (|| -> Result<Vec<String>> {
            candidate
                .sources
                .iter()
                .map(|source| {
                    if let Some(record) = cache.get(source)? {
                        source.validate()?;
                        return Ok(record.sample);
                    }
                    let (digest, full) = sample(source, state, cancelled)?;
                    cache.put(Record {
                        path: source.path.clone(),
                        fingerprint: source.fingerprint.clone(),
                        sample: digest.clone(),
                        full,
                    })?;
                    Ok(digest)
                })
                .collect()
        })();
        match result {
            Ok(key) => {
                samples.entry(key).or_default().push(candidate);
            }
            Err(error) => {
                if error.downcast_ref::<rusqlite::Error>().is_some() {
                    return Err(error.context("Duplicate hash cache"));
                }
                if !stopped(cancelled) {
                    state.error(format!("Photo {}: {error:#}", candidate.id));
                }
                state.processed += 1;
            }
        }
        if update_at.elapsed() >= Duration::from_millis(100) {
            progress(state);
            update_at = Instant::now();
        }
        thread::yield_now();
    }
    state.stage = "verifying".into();
    progress(state);
    let mut verified = BTreeMap::<Vec<String>, Vec<Candidate>>::new();
    let mut buffer = vec![0u8; CHUNK];
    for bucket in samples.into_values() {
        if bucket.len() < 2 {
            state.processed += bucket.len();
            continue;
        }
        for candidate in bucket {
            if stopped(cancelled) {
                cache.flush()?;
                return Ok(());
            }
            let result = (|| -> Result<Vec<String>> {
                candidate
                    .sources
                    .iter()
                    .map(|source| {
                        let mut record = cache.get(source)?.context("Sample cache missing")?;
                        if let Some(full) = record.full {
                            source.validate()?;
                            state.cache_hits += 1;
                            return Ok(full);
                        }
                        let full = full_hash(
                            source,
                            state,
                            cancelled,
                            &mut buffer,
                            &mut progress,
                            &mut update_at,
                        )?;
                        record.full = Some(full.clone());
                        cache.put(record)?;
                        Ok(full)
                    })
                    .collect()
            })();
            match result {
                Ok(key) => {
                    verified.entry(key).or_default().push(candidate);
                }
                Err(error) => {
                    if error.downcast_ref::<rusqlite::Error>().is_some() {
                        return Err(error.context("Duplicate hash cache"));
                    }
                    if !stopped(cancelled) {
                        state.error(format!("Photo {}: {error:#}", candidate.id));
                    }
                }
            }
            state.processed += 1;
            if update_at.elapsed() >= Duration::from_millis(100) {
                progress(state);
                update_at = Instant::now();
            }
        }
    }
    cache.flush()?;
    let mut result_ids = 0;
    for bucket in verified.into_values().filter(|values| values.len() > 1) {
        if stopped(cancelled) {
            return Ok(());
        }
        let mut group = Vec::new();
        for candidate in bucket {
            match candidate.sources.iter().try_for_each(Source::validate) {
                Ok(()) => {
                    // A stable ID may have been promoted from standalone JPEG
                    // to RAW+companion while this snapshot was being scanned.
                    // SQL errors fail the scan; stale identities are omitted.
                    if cache.is_current_asset(&candidate)? {
                        group.push(candidate.id);
                    } else {
                        state.error(format!("Photo {} changed catalog identity or moved to Trash during scan; omitted",candidate.id));
                    }
                }
                Err(error) => state.error(error),
            }
        }
        if group.len() < 2 {
            continue;
        }
        if state.groups.len() >= MAX_GROUPS || result_ids + group.len() > MAX_RESULT_IDS {
            state.truncated = true;
            continue;
        }
        result_ids += group.len();
        state.groups.push(group);
    }
    state.groups.sort_unstable_by_key(|group| group[0]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    fn photo(id: i64, path: PathBuf) -> Photo {
        Photo {
            id,
            path,
            ..Default::default()
        }
    }
    fn run(photos: Vec<Photo>, database: &Path) -> DuplicateScanState {
        Catalog::open(database)
            .unwrap()
            .upsert_photos(&photos)
            .unwrap();
        let mut state = DuplicateScanState::default();
        scan(
            photos,
            database,
            &AtomicBool::new(false),
            &mut state,
            |_| {},
        )
        .unwrap();
        state
    }
    #[test]
    fn overlapping_samples_preserve_v1_digest_and_read_small_files_once() {
        let directory = tempfile::tempdir().unwrap();
        for size in [
            0,
            1,
            SAMPLE - 1,
            SAMPLE,
            SAMPLE + 1,
            2 * SAMPLE,
            3 * SAMPLE,
            3 * SAMPLE + 1,
            8 * SAMPLE,
        ] {
            let path = directory.path().join(format!("{size}.raw"));
            let bytes: Vec<_> = (0..size)
                .map(|i| (i.wrapping_mul(31) % 251) as u8)
                .collect();
            fs::write(&path, &bytes).unwrap();
            let source = Source::read(&path).unwrap();
            let mut expected = blake3::Hasher::new();
            expected.update(b"shadowroom-sample-v1");
            expected.update(&(size as u64).to_le_bytes());
            let mut offsets = vec![
                0,
                size.saturating_sub(SAMPLE) / 2,
                size.saturating_sub(SAMPLE),
            ];
            offsets.sort_unstable();
            offsets.dedup();
            let mut old_bytes = 0;
            for offset in offsets {
                let end = (offset + SAMPLE).min(size);
                expected.update(&(offset as u64).to_le_bytes());
                expected.update(&bytes[offset..end]);
                old_bytes += end - offset;
            }
            let mut state = DuplicateScanState::default();
            let (digest, full) = sample(&source, &mut state, &AtomicBool::new(false)).unwrap();
            assert_eq!(digest, expected.finalize().to_hex().to_string());
            if size <= 3 * SAMPLE {
                assert_eq!(state.bytes_read, size as u64);
                assert_eq!(full, Some(blake3::hash(&bytes).to_hex().to_string()));
            } else {
                assert_eq!(state.bytes_read, old_bytes as u64);
                assert!(full.is_none());
            }
        }
    }
    #[test]
    fn singleton_sizes_read_nothing_and_small_duplicate_verification_reuses_samples() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let mut photos = Vec::new();
        for (name, bytes) in [
            ("a", vec![7; 20_000]),
            ("b", vec![7; 20_000]),
            ("c", vec![0; 13]),
            ("d", vec![0; 14]),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, bytes).unwrap();
            photos.push(photo(0, path));
        }
        let catalog = Catalog::open(&database).unwrap();
        catalog.upsert_photos(&photos).unwrap();
        let state = run(catalog.photos().unwrap(), &database);
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.candidates, 2);
        assert_eq!(state.bytes_read, 40_000);
        assert!(state.errors.is_empty());
    }
    fn reference_throttled_full_hash(
        source: &Source,
        state: &mut DuplicateScanState,
        cancelled: &AtomicBool,
        buffer: &mut [u8],
        progress: &mut impl FnMut(&DuplicateScanState),
        update_at: &mut Instant,
    ) -> Result<String> {
        let mut file = source.open()?;
        let mut hash = blake3::Hasher::new();
        let mut remaining = source.size;
        while remaining > 0 {
            if stopped(cancelled) {
                bail!("Scan cancelled");
            }
            let length = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..length])?;
            hash.update(&buffer[..length]);
            state.bytes_read += length as u64;
            remaining -= length as u64;
            // Cooperative background priority, bounded to one reader. Never hold a
            // catalog mutex/SQLite transaction while yielding or touching originals.
            if update_at.elapsed() >= Duration::from_millis(100) {
                progress(state);
                *update_at = Instant::now();
            }
            thread::sleep(Duration::from_millis(1));
        }
        if fingerprint(&file.metadata()?)? != source.fingerprint {
            bail!("Source changed while hashing: {}", source.path.display());
        }
        source.validate()?;
        Ok(hash.finalize().to_hex().to_string())
    }

    #[test]
    #[ignore]
    fn bench_full_hash_throttle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.raw");
        fs::write(&path, vec![37u8; 32 * 1024 * 1024]).unwrap();
        let source = Source::read(&path).unwrap();
        let mut buffer = vec![0u8; CHUNK];
        let cancelled = AtomicBool::new(false);
        for legacy in [true, false] {
            let mut state = DuplicateScanState::default();
            let began = Instant::now();
            let mut update_at = Instant::now();
            for _ in 0..3 {
                let digest = if legacy {
                    reference_throttled_full_hash(
                        &source,
                        &mut state,
                        &cancelled,
                        &mut buffer,
                        &mut |_| {},
                        &mut update_at,
                    )
                } else {
                    full_hash(
                        &source,
                        &mut state,
                        &cancelled,
                        &mut buffer,
                        &mut |_| {},
                        &mut update_at,
                    )
                }
                .unwrap();
                std::hint::black_box(digest);
            }
            eprintln!(
                "full hash legacy={legacy} 32MiB mean {:?}, bytes {}",
                began.elapsed() / 3,
                state.bytes_read / 3
            );
        }
    }
    #[test]
    fn duplicate_groups_match_exact_bytes_for_randomized_trees() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xd00f);
        for _ in 0..5 {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("catalog.db");
            let catalog = Catalog::open(&database).unwrap();
            let mut photos = Vec::new();
            for id in 0..48 {
                let identity = rng.random_range(0..12);
                let size = [0, 11, 20_000, 128 * 1024][identity % 4];
                let mut bytes = vec![identity as u8; size];
                if size > 0 && rng.random_bool(0.15) {
                    bytes[size / 4] = 99;
                }
                let path = directory.path().join(format!("{id:04}.raw"));
                fs::write(&path, bytes).unwrap();
                let mut p = photo(0, path);
                p.trashed = rng.random_bool(0.05);
                if rng.random_bool(0.4) {
                    let path = directory.path().join(format!("{id:04}.jpg"));
                    fs::write(&path, vec![rng.random_range(0..3); 30]).unwrap();
                    p.companion_path = Some(path);
                }
                photos.push(p);
            }
            catalog.upsert_photos(&photos).unwrap();
            let photos = catalog.photos().unwrap();
            let mut expected = BTreeMap::<Vec<Vec<u8>>, Vec<i64>>::new();
            for photo in &photos {
                if !photo.trashed {
                    let mut key = vec![fs::read(&photo.path).unwrap()];
                    if let Some(path) = &photo.companion_path {
                        key.push(fs::read(path).unwrap());
                    }
                    expected.entry(key).or_default().push(photo.id);
                }
            }
            let mut expected: Vec<_> = expected.into_values().filter(|g| g.len() > 1).collect();
            expected.sort_unstable_by_key(|g| g[0]);
            let actual = run(photos.clone(), &database);
            assert!(actual.errors.is_empty());
            assert_eq!(actual.groups, expected);
            let warm = run(photos, &database);
            assert_eq!(warm.groups, expected);
            assert_eq!(warm.bytes_read, 0);
        }
    }
    #[test]
    #[ignore]
    fn bench_duplicate_reads() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let mut photos = Vec::new();
        for id in 0..128 {
            let path = directory.path().join(format!("{id:04}.raw"));
            let mut bytes = vec![0u8; 256 * 1024];
            bytes[0] = (id / 2) as u8;
            fs::write(&path, bytes).unwrap();
            photos.push(photo(id, path));
        }
        for id in 128..192 {
            let path = directory.path().join(format!("{id:04}.raw"));
            fs::write(&path, vec![0u8; id as usize]).unwrap();
            photos.push(photo(id, path));
        }
        let catalog = Catalog::open(&database).unwrap();
        catalog.upsert_photos(&photos).unwrap();
        let photos = catalog.photos().unwrap();
        let began = Instant::now();
        let cold = run(photos.clone(), &database);
        assert_eq!(cold.groups.len(), 64);
        eprintln!(
            "duplicates cold {:?}, bytes {}, groups {}",
            began.elapsed(),
            cold.bytes_read,
            cold.groups.len()
        );
        let began = Instant::now();
        let warm = run(photos, &database);
        eprintln!(
            "duplicates warm {:?}, bytes {}",
            began.elapsed(),
            warm.bytes_read
        );
    }
    #[test]
    fn full_verification_rejects_sample_collision_and_warm_cache_skips_source_reads() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let _catalog = Catalog::open(&database).unwrap();
        let content = vec![23u8; 128 * 1024];
        let a = directory.path().join("a.raw");
        let b = directory.path().join("b.raw");
        let c = directory.path().join("c.raw");
        let unique = directory.path().join("unique.raw");
        fs::write(&a, &content).unwrap();
        fs::write(&b, &content).unwrap();
        let mut different = content.clone();
        different[30_000] = 42;
        fs::write(&c, different).unwrap();
        fs::write(&unique, b"different size").unwrap();
        let photos = vec![
            photo(3, c),
            photo(1, a.clone()),
            photo(4, unique),
            photo(2, b.clone()),
        ];
        let cold = run(photos.clone(), &database);
        assert_eq!(cold.groups, vec![vec![1, 2]]);
        assert_eq!(cold.candidates, 3);
        assert_eq!(cold.processed, 3);
        assert!(cold.bytes_read > 3 * content.len() as u64);
        let warm = run(photos.clone(), &database);
        assert_eq!(warm.groups, cold.groups);
        assert_eq!(warm.bytes_read, 0);
        assert_eq!(warm.cache_hits, 3);
        // Same-size source edits must invalidate a persisted full hash. Photo's
        // old indexed mtime is deliberately unchanged; use current filesystem stat.
        let mut changed = content.clone();
        changed[30_001] = 99;
        fs::write(&b, changed).unwrap();
        File::options()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000))
            .unwrap();
        let changed = run(photos, &database);
        assert!(changed.groups.is_empty());
        assert!(changed.bytes_read > 0);
        assert_eq!(changed.cache_hits, 2);
        assert_eq!(fs::read(a).unwrap(), content);
    }
    #[test]
    fn paired_variants_require_both_files_to_match_and_trash_is_excluded() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let _catalog = Catalog::open(&database).unwrap();
        let mut photos = Vec::new();
        for (id, companion) in [(1, b"variant1"), (2, b"variant2"), (3, b"variant1")] {
            let raw = directory.path().join(format!("{id}.nef"));
            let preview = directory.path().join(format!("{id}.heic"));
            fs::write(&raw, b"same raw").unwrap();
            fs::write(&preview, companion).unwrap();
            photos.push(Photo {
                id,
                path: raw,
                companion_path: Some(preview),
                ..Default::default()
            });
        }
        let standalone = directory.path().join("standalone.heic");
        fs::write(&standalone, b"variant1").unwrap();
        photos.push(photo(4, standalone));
        photos.push(Photo {
            id: 5,
            path: directory.path().join("missing-trashed.nef"),
            trashed: true,
            ..Default::default()
        });
        let state = run(photos, &database);
        assert_eq!(state.total, 4);
        assert_eq!(state.groups, vec![vec![1, 3]]);
        assert!(state.errors.is_empty());
    }
    #[test]
    fn cancellation_before_work_reads_nothing_and_error_history_is_bounded() {
        let mut state = DuplicateScanState::default();
        scan(
            vec![photo(1, PathBuf::from("missing.raw"))],
            Path::new("missing-catalog.db"),
            &AtomicBool::new(true),
            &mut state,
            |_| {},
        )
        .unwrap();
        assert_eq!(state.bytes_read, 0);
        assert!(state.errors.is_empty());
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let _catalog = Catalog::open(&database).unwrap();
        let state = run(
            (0..70)
                .map(|id| photo(id, directory.path().join(format!("missing{id}.raw"))))
                .collect(),
            &database,
        );
        assert_eq!(state.errors.len(), 64);
        assert_eq!(state.errors_omitted, 6);
        assert!(state.groups.is_empty());
    }
    #[test]
    fn changed_source_cannot_reuse_a_prepared_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.raw");
        fs::write(&path, b"before").unwrap();
        let source = Source::read(&path).unwrap();
        fs::write(&path, b"after longer").unwrap();
        assert!(
            sample(
                &source,
                &mut DuplicateScanState::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    #[test]
    fn sql_cache_failure_fails_job_and_a_blocked_batch_never_grows_past_32() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let catalog = Catalog::open(&database).unwrap();
        let mut cache = Cache::open(&database).unwrap();
        let writer = Connection::open(&database).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        for id in 0..31 {
            cache
                .put(Record {
                    path: PathBuf::from(format!("fake-{id}")),
                    fingerprint: "fixture".into(),
                    sample: "fixture".into(),
                    full: None,
                })
                .unwrap();
        }
        for id in 31..33 {
            assert!(
                cache
                    .put(Record {
                        path: PathBuf::from(format!("fake-{id}")),
                        fingerprint: "fixture".into(),
                        sample: "fixture".into(),
                        full: None
                    })
                    .is_err()
            );
            assert_eq!(cache.pending.len(), 32);
        }
        writer.execute_batch("ROLLBACK").unwrap();
        let a = directory.path().join("a.raw");
        let b = directory.path().join("b.raw");
        fs::write(&a, b"identical").unwrap();
        fs::write(&b, b"identical").unwrap();
        catalog.upsert_photos(&[photo(0, a), photo(0, b)]).unwrap();
        writer.execute_batch("DROP TABLE exact_hashes").unwrap();
        let job = DuplicateJob::start(91, catalog.photos().unwrap(), database).unwrap();
        let output = job.updates.clone();
        job.join().unwrap();
        let result = output.lock().unwrap().take().unwrap();
        assert_eq!(result.phase, "failed");
        assert!(result.groups.is_empty());
        assert_eq!(result.bytes_read, 0);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("Duplicate hash cache"))
        );
    }
    #[test]
    fn in_place_jpeg_to_raw_promotion_cannot_return_stale_duplicate_ids() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let catalog = Catalog::open(&database).unwrap();
        let a = directory.path().join("a.jpg");
        let b = directory.path().join("b.jpg");
        let raw = directory.path().join("a.nef");
        fs::write(&a, b"identical JPEG bytes").unwrap();
        fs::write(&b, b"identical JPEG bytes").unwrap();
        fs::write(&raw, b"a distinct RAW asset").unwrap();
        catalog
            .upsert_photos(&[photo(0, a.clone()), photo(0, b)])
            .unwrap();
        let photos = catalog.photos().unwrap();
        let original_id = photos[0].id;
        let mut promoted = false;
        let mut state = DuplicateScanState::default();
        scan(
            photos,
            &database,
            &AtomicBool::new(false),
            &mut state,
            |state| {
                if state.stage == "verifying" && !promoted {
                    catalog
                        .upsert_photos(&[Photo {
                            path: raw.clone(),
                            companion_path: Some(a.clone()),
                            ..Default::default()
                        }])
                        .unwrap();
                    promoted = true;
                }
            },
        )
        .unwrap();
        assert!(promoted);
        assert!(state.groups.is_empty());
        assert!(
            state
                .errors
                .iter()
                .any(|error| error.contains("changed catalog identity"))
        );
        assert_eq!(
            catalog
                .photos()
                .unwrap()
                .iter()
                .find(|photo| photo.path == raw)
                .unwrap()
                .id,
            original_id
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlink_sources_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.raw");
        fs::write(&original, b"read only").unwrap();
        let link = directory.path().join("linked.raw");
        std::os::unix::fs::symlink(original, &link).unwrap();
        assert!(Source::read(&link).is_err());
    }
}
