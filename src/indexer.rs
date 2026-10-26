//! Read-only, cancellable scanning. EXIF reads headers; image pixels are decoded lazily by media workers.
use crate::{
    catalog::{Catalog, IndexedFile},
    model::{Photo, parse_capture_offset},
};
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, UNIX_EPOCH},
};

const METADATA_VERSION: u32 = 3;

#[derive(Clone, Debug)]
pub struct IndexOptions {
    pub pair_raw_jpeg: bool,
    pub read_exif: bool,
    pub batch_size: usize,
}
impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            pair_raw_jpeg: true,
            read_exif: true,
            batch_size: 512,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct IndexProgress {
    pub scanned: u64,
    pub indexed: u64,
    pub unchanged: u64,
    pub errors: u64,
    pub current_path: PathBuf,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug)]
pub enum IndexEvent {
    Started(PathBuf),
    Progress(IndexProgress),
    Error { path: PathBuf, message: String },
    Finished(IndexProgress),
    Cancelled(IndexProgress),
}
pub struct IndexJob {
    pub events: Receiver<IndexEvent>,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl IndexJob {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn try_recv(&self) -> std::result::Result<IndexEvent, crossbeam_channel::TryRecvError> {
        self.events.try_recv()
    }
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<IndexEvent, crossbeam_channel::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }
    pub fn join(mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Indexer thread panicked"))?;
        }
        Ok(())
    }
}
impl Drop for IndexJob {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub struct Indexer;
impl Indexer {
    pub fn start(catalog: Catalog, root: PathBuf, options: IndexOptions) -> IndexJob {
        let (sender, events) = crossbeam_channel::unbounded();
        let cancelled = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let done = finished.clone();
        let thread = thread::Builder::new()
            .name("shadowroom-index".into())
            .spawn(move || {
                let _ = sender.send(IndexEvent::Started(root.clone()));
                if let Err(error) = scan(&catalog, &root, &options, &stop, &sender) {
                    let _ = sender.send(IndexEvent::Error {
                        path: root,
                        message: format!("{error:#}"),
                    });
                }
                done.store(true, Ordering::Release);
            })
            .expect("Create indexer thread");
        IndexJob {
            events,
            cancelled,
            finished,
            thread: Some(thread),
        }
    }
}
fn scan(
    catalog: &Catalog,
    root: &Path,
    options: &IndexOptions,
    stop: &AtomicBool,
    sender: &Sender<IndexEvent>,
) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Open source {}", root.display()))?;
    if !root.is_dir() {
        bail!("Index root must be a directory");
    }
    if catalog.path().canonicalize()?.starts_with(&root) {
        bail!("Catalog must be on internal application storage, outside the source being indexed");
    }
    catalog.register_source(&root)?;
    let existing = catalog.fingerprints(&root)?;
    let mut progress = IndexProgress::default();
    let start = Instant::now();
    let mut last = Instant::now();
    let mut batch = IndexBatch {
        photos: Vec::with_capacity(options.batch_size.clamp(1, 1024)),
        last_flush: start,
    };
    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut paths = Vec::new();
        let mut children = Vec::new();
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for file in entries {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match file {
                        Ok(file) => {
                            if !allowed_name(&file.file_name()) {
                                continue;
                            }
                            match file.file_type() {
                                Ok(kind) if kind.is_dir() => children.push(file.path()),
                                Ok(kind) if kind.is_file() && is_photo(&file.path()) => {
                                    paths.push(file)
                                }
                                Ok(kind)
                                    if kind.is_file()
                                        && matches!(extension(&file.path()).as_str(), "avif") =>
                                {
                                    report_error(
                                        sender,
                                        &mut progress,
                                        &file.path(),
                                        "Unsupported preview format; original left untouched"
                                            .into(),
                                    )
                                }
                                Ok(_) => {}
                                Err(error) => report_error(
                                    sender,
                                    &mut progress,
                                    &file.path(),
                                    error.to_string(),
                                ),
                            }
                        }
                        Err(error) => {
                            report_error(sender, &mut progress, &directory, error.to_string())
                        }
                    }
                }
            }
            Err(error) => report_error(sender, &mut progress, &directory, error.to_string()),
        }
        children.sort_by(|a, b| b.cmp(a));
        directories.extend(children);
        process_folder(
            catalog,
            paths,
            options,
            &existing,
            stop,
            sender,
            &mut progress,
            &mut batch,
            start,
            &mut last,
        )?;
        send_progress(sender, &mut progress, start, &mut last);
    }
    if !batch.photos.is_empty() {
        progress.indexed += catalog.upsert_photos(&batch.photos)? as u64;
    }
    progress.elapsed_ms = start.elapsed().as_millis() as u64;
    let event = if stop.load(Ordering::Relaxed) {
        IndexEvent::Cancelled(progress)
    } else {
        IndexEvent::Finished(progress)
    };
    let _ = sender.send(event);
    Ok(())
}
fn allowed_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    !name.starts_with('.')
        && ![
            "$recycle.bin",
            "system volume information",
            "lost+found",
            "windows",
            "program files",
            "node_modules",
            "target",
        ]
        .iter()
        .any(|skip| name.eq_ignore_ascii_case(skip))
}
struct IndexBatch {
    photos: Vec<Photo>,
    last_flush: Instant,
}
struct FolderEntry {
    path: PathBuf,
    metadata: std::io::Result<fs::Metadata>,
    raw: bool,
    jpeg: bool,
    processed: bool,
    stem: String,
}
fn process_folder(
    catalog: &Catalog,
    entries: Vec<fs::DirEntry>,
    options: &IndexOptions,
    existing: &HashMap<PathBuf, IndexedFile>,
    stop: &AtomicBool,
    sender: &Sender<IndexEvent>,
    progress: &mut IndexProgress,
    batch: &mut IndexBatch,
    start: Instant,
    last: &mut Instant,
) -> Result<()> {
    let mut paths: Vec<_> = entries
        .into_iter()
        .take_while(|_| !stop.load(Ordering::Relaxed))
        .map(|entry| {
            let path = entry.path();
            let format = extension(&path);
            let raw = is_raw_extension(&format);
            let jpeg = matches!(format.as_str(), "jpg" | "jpeg");
            let processed = jpeg || matches!(format.as_str(), "heic" | "heif" | "hif");
            let stem = if options.pair_raw_jpeg && (raw || processed) {
                stem(&path)
            } else {
                String::new()
            };
            FolderEntry {
                path,
                metadata: entry.metadata(),
                raw,
                jpeg,
                processed,
                stem,
            }
        })
        .collect();
    paths.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    // Folder-local indices also give O(1) access to cached companion metadata.
    let mut jpegs: HashMap<&str, usize> = HashMap::new();
    if options.pair_raw_jpeg {
        // Insert JPEGs first, then HEIFs, in native path order. Stems and
        // classifications are computed once, rather than in sort comparators.
        for jpeg in [true, false] {
            for (index, entry) in paths.iter().enumerate() {
                if entry.processed && entry.jpeg == jpeg {
                    jpegs.entry(&entry.stem).or_insert(index);
                }
            }
        }
    }
    let mut pairs = HashMap::new();
    if options.pair_raw_jpeg {
        // Preserve independently cataloged IDs and established companion owners.
        let independent: HashSet<_> = paths
            .iter()
            .filter(|entry| entry.raw && existing.contains_key(&entry.path))
            .filter_map(|entry| jpegs.get(entry.stem.as_str()).copied())
            .filter(|&jpeg| existing.contains_key(&paths[jpeg].path))
            .collect();
        let mut raws: Vec<_> = paths
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.raw)
            .map(|(index, entry)| {
                let companion = jpegs.get(entry.stem.as_str()).copied();
                let owns = companion.is_some_and(|jpeg| {
                    existing
                        .get(&entry.path)
                        .is_some_and(|old| old.companion_path.as_ref() == Some(&paths[jpeg].path))
                });
                (index, companion, owns)
            })
            .collect();
        raws.sort_unstable_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| paths[a.0].path.cmp(&paths[b.0].path))
        });
        let mut claimed = HashSet::new();
        for (raw, companion, _) in raws {
            if let Some(jpeg) = companion {
                if !independent.contains(&jpeg) && claimed.insert(jpeg) {
                    pairs.insert(raw, jpeg);
                }
            }
        }
    }
    let companions: HashSet<_> = pairs.values().copied().collect();
    for (index, entry) in paths.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let path = &entry.path;
        progress.scanned += 1;
        progress.current_path = path.clone();
        if companions.contains(&index) {
            continue;
        }
        let companion_index = pairs.get(&index).copied();
        let companion = companion_index.map(|index| paths[index].path.clone());
        let metadata = match &entry.metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                report_error(sender, progress, path, error.to_string());
                continue;
            }
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let companion_metadata =
            companion_index.and_then(|index| paths[index].metadata.as_ref().ok());
        let companion_size_bytes = companion_metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let companion_modified_at = companion_metadata
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Some(old) = existing.get(path) {
            if old.size_bytes == metadata.len()
                && old.modified_at == modified
                && old.companion_path == companion
                && old.companion_size_bytes == companion_size_bytes
                && old.companion_modified_at == companion_modified_at
                && (!options.read_exif || old.metadata_version == METADATA_VERSION)
            {
                progress.unchanged += 1;
                continue;
            }
        }
        let mut photo = Photo {
            filename: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            folder: path
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned(),
            path: path.clone(),
            modified_at: modified,
            size_bytes: metadata.len(),
            orientation: 1,
            metadata_version: if options.read_exif {
                METADATA_VERSION
            } else {
                0
            },
            companion_path: companion,
            companion_size_bytes,
            companion_modified_at,
            ..Photo::default()
        };
        if options.read_exif {
            if let Err(error) = read_metadata(&mut photo) {
                report_error(sender, progress, path, format!("Read metadata: {error:#}"));
            }
        }
        batch.photos.push(photo);
        // Amortize FULL-synchronized commits on fast media. Slow sources keep
        // the earlier 64-photo latency rather than waiting for a larger batch.
        if batch.photos.len() >= options.batch_size.clamp(1, 1024)
            || (batch.photos.len() >= 64
                && batch.last_flush.elapsed() >= Duration::from_millis(150))
        {
            progress.indexed += catalog.upsert_photos(&batch.photos)? as u64;
            batch.photos.clear();
            batch.last_flush = Instant::now();
            send_progress(sender, progress, start, last);
        }
    }
    Ok(())
}
fn send_progress(
    sender: &Sender<IndexEvent>,
    progress: &mut IndexProgress,
    start: Instant,
    last: &mut Instant,
) {
    if last.elapsed() >= Duration::from_millis(150) {
        progress.elapsed_ms = start.elapsed().as_millis() as u64;
        let _ = sender.send(IndexEvent::Progress(progress.clone()));
        *last = Instant::now();
    }
}
fn report_error(
    sender: &Sender<IndexEvent>,
    progress: &mut IndexProgress,
    path: &Path,
    message: String,
) {
    progress.errors += 1;
    if progress.errors <= 100 {
        let _ = sender.send(IndexEvent::Error {
            path: path.to_path_buf(),
            message,
        });
    }
}
fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}
fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}
pub fn is_raw(path: &Path) -> bool {
    is_raw_extension(&extension(path))
}
fn is_raw_extension(extension: &str) -> bool {
    matches!(
        extension,
        "raw"
            | "arw"
            | "cr2"
            | "cr3"
            | "nef"
            | "nrw"
            | "dng"
            | "raf"
            | "orf"
            | "rw2"
            | "pef"
            | "srw"
            | "rwl"
            | "3fr"
            | "fff"
            | "iiq"
            | "mos"
            | "sr2"
            | "srf"
            | "x3f"
            | "kdc"
            | "dcr"
            | "mrw"
            | "erf"
            | "bay"
            | "mef"
            | "mdc"
            | "cap"
            | "obm"
            | "ptx"
            | "pxn"
            | "qtk"
            | "rdc"
            | "sti"
    )
}
pub fn is_photo(path: &Path) -> bool {
    let extension = extension(path);
    is_raw_extension(&extension)
        || matches!(
            extension.as_str(),
            "jpg"
                | "jpeg"
                | "png"
                | "tif"
                | "tiff"
                | "btf"
                | "btiff"
                | "tf8"
                | "webp"
                | "bmp"
                | "heic"
                | "heif"
                | "hif"
                | "jxl"
        )
}
fn image_dimensions_from(path: &Path, file: &mut fs::File) -> Result<(u32, u32)> {
    file.seek(SeekFrom::Start(0))?;
    Ok(
        image::ImageReader::with_format(BufReader::new(file), image::ImageFormat::from_path(path)?)
            .into_dimensions()?,
    )
}
fn read_metadata(photo: &mut Photo) -> Result<()> {
    let mut failure = None;
    photo.orientation = 0;
    let mut source = fs::File::open(&photo.path);
    match source
        .as_mut()
        .map_err(|error| anyhow::anyhow!("{error}"))
        .and_then(|file| read_exif_from(&photo.path, file))
    {
        Ok(Some(exif)) => {
            apply_exif(photo, &exif);
            if is_raw(&photo.path) {
                if let Some((width, height)) = raw_sensor_dimensions(exif.buf()) {
                    photo.width = width;
                    photo.height = height;
                }
            }
        }
        Ok(None) => {}
        Err(error) => failure = Some(error),
    }
    if crate::heif::is_heif(&photo.path) {
        match crate::heif::inspect_file(&photo.path) {
            Ok(info) => {
                photo.width = info.width;
                photo.height = info.height;
                photo.orientation = 1;
            }
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    } else if crate::jxl::is_jxl(&photo.path) {
        match crate::jxl::inspect_file(&photo.path) {
            Ok(info) => {
                photo.width = info.width;
                photo.height = info.height;
                photo.orientation = 1;
            }
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    } else if crate::tiff_media::is_tiff(&photo.path) {
        match crate::tiff_media::inspect_file(&photo.path) {
            Ok(info) => {
                photo.width = info.width;
                photo.height = info.height;
                photo.orientation = u32::from(info.orientation);
            }
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    } else if !is_raw(&photo.path) && (photo.width == 0 || photo.height == 0) {
        if let Ok((width, height)) = source
            .as_mut()
            .map_err(|error| anyhow::anyhow!("{error}"))
            .and_then(|file| image_dimensions_from(&photo.path, file))
        {
            if photo.width == 0 {
                photo.width = width;
            }
            if photo.height == 0 {
                photo.height = height;
            }
        }
    }
    // Preserve original RAW dimensions/exposure/time. A processed or reduced
    // paired JPEG/HEIF can only supply fields that the bounded RAW header lacks.
    if let Some(companion) = photo
        .companion_path
        .as_ref()
        .filter(|_| metadata_incomplete(photo))
    {
        let mut fallback = Photo::default();
        let mut source = fs::File::open(companion);
        match source
            .as_mut()
            .map_err(|error| anyhow::anyhow!("{error}"))
            .and_then(|file| read_exif_from(companion, file))
        {
            Ok(Some(exif)) => apply_exif(&mut fallback, &exif),
            Ok(None) => {}
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if crate::heif::is_heif(companion) {
            match crate::heif::inspect_file(companion) {
                Ok(info) => {
                    fallback.width = info.width;
                    fallback.height = info.height;
                    fallback.orientation = 1;
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        } else if fallback.width == 0 || fallback.height == 0 {
            if let Ok((width, height)) = source
                .as_mut()
                .map_err(|error| anyhow::anyhow!("{error}"))
                .and_then(|file| image_dimensions_from(companion, file))
            {
                if fallback.width == 0 {
                    fallback.width = width;
                }
                if fallback.height == 0 {
                    fallback.height = height;
                }
            }
        }
        fill_missing_metadata(photo, &fallback);
    }
    photo.orientation = photo.orientation.max(1);
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}
pub(crate) fn metadata_incomplete(photo: &Photo) -> bool {
    photo.width == 0
        || photo.height == 0
        || photo.orientation == 0
        || photo.camera.is_empty()
        || photo.lens.is_empty()
        || photo.iso.is_none()
        || photo.aperture.is_none()
        || photo.shutter.is_empty()
        || photo.focal_length.is_none()
        || photo.captured_at.is_none()
        || photo.captured_offset.is_none()
}
pub(crate) fn fill_missing_metadata(photo: &mut Photo, fallback: &Photo) {
    if photo.width == 0 {
        photo.width = fallback.width;
    }
    if photo.height == 0 {
        photo.height = fallback.height;
    }
    if photo.orientation == 0 {
        photo.orientation = fallback.orientation;
    }
    if photo.camera.is_empty() {
        photo.camera = fallback.camera.clone();
    }
    if photo.lens.is_empty() {
        photo.lens = fallback.lens.clone();
    }
    if photo.shutter.is_empty() {
        photo.shutter = fallback.shutter.clone();
    }
    photo.iso = photo.iso.or(fallback.iso);
    photo.aperture = photo.aperture.or(fallback.aperture);
    photo.focal_length = photo.focal_length.or(fallback.focal_length);
    if photo.captured_at.is_none() {
        photo.captured_at = fallback.captured_at;
        photo.captured_offset = fallback.captured_offset.clone();
        photo.capture_time_kind = fallback.capture_time_kind.clone();
    } else if photo.capture_time_kind == "camera_local" && photo.captured_offset.is_none() {
        // Offset may be present only in the JPEG. Apply it to the authoritative
        // RAW clock/subseconds, rather than replacing them with the JPEG date.
        if let Some(offset) = fallback
            .captured_offset
            .as_deref()
            .and_then(parse_capture_offset)
        {
            photo.captured_at = photo
                .captured_at
                .and_then(|time| time.checked_sub(i64::from(offset.local_minus_utc()) * 1000));
            photo.captured_offset = fallback.captured_offset.clone();
            photo.capture_time_kind = "offset".into();
        }
    }
}
// DNG/NEF TIFF containers can put a reduced preview in IFD0 and source pixels
// in SubIFDs. Inspect only the already-read prefix; never seek into pixel data.
fn raw_sensor_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let little = match data.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |offset: usize| {
        let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let u32_at = |offset: usize| {
        let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let mut pending = vec![u32_at(4)? as usize];
    let mut visited = HashSet::new();
    let mut best = None;
    while let Some(offset) = pending.pop() {
        if visited.len() >= 64 {
            break;
        }
        if offset == 0 || !visited.insert(offset) {
            continue;
        }
        let Some(count) = u16_at(offset) else {
            continue;
        };
        let mut width = 0;
        let mut height = 0;
        let mut photometric = 0;
        let mut reduced = false;
        for index in 0..usize::from(count).min(512) {
            let position = offset + 2 + index * 12;
            let (Some(tag), Some(kind), Some(count)) =
                (u16_at(position), u16_at(position + 2), u32_at(position + 4))
            else {
                break;
            };
            let scalar = if count == 1 {
                match kind {
                    3 => u16_at(position + 8).map(u32::from),
                    4 | 13 => u32_at(position + 8),
                    _ => None,
                }
            } else {
                None
            };
            match tag {
                254 => reduced = scalar.is_some_and(|value| value & 1 != 0),
                256 => width = scalar.unwrap_or(0),
                257 => height = scalar.unwrap_or(0),
                262 => photometric = scalar.unwrap_or(0),
                330 if matches!(kind, 4 | 13) && count <= 32 => {
                    if let Some(start) = if count == 1 {
                        Some(position + 8)
                    } else {
                        u32_at(position + 8).map(|offset| offset as usize)
                    } {
                        for child in 0..count as usize {
                            if let Some(child) = u32_at(start + child * 4) {
                                pending.push(child as usize);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !reduced && matches!(photometric, 32803 | 34892) && width > 0 && height > 0 {
            if best.is_none_or(|(w, h)| {
                u64::from(width) * u64::from(height) > u64::from(w) * u64::from(h)
            }) {
                best = Some((width, height));
            }
        }
        if let Some(next) = u32_at(offset + 2 + usize::from(count) * 12) {
            if next != 0 {
                pending.push(next as usize);
            }
        }
    }
    best
}
pub(crate) fn read_exif_bounded(path: &Path) -> Result<Option<exif::Exif>> {
    read_exif_from(path, &mut fs::File::open(path)?)
}
fn read_exif_from(path: &Path, file: &mut fs::File) -> Result<Option<exif::Exif>> {
    if extension(path) == "bmp" || crate::jxl::is_jxl(path) {
        return Ok(None);
    }
    let mut magic = [0; 4];
    let count = file.read(&mut magic)?;
    if count < 4 {
        return Ok(None);
    }
    // The EXIF reader supports Classic TIFF only. BigTIFF dimensions and
    // orientation are read by the bounded first-IFD inspector instead.
    if magic == [b'I', b'I', 43, 0] || magic == [b'M', b'M', 0, 43] {
        return Ok(None);
    }
    let reader = exif::Reader::new();
    let parsed = if magic == [b'I', b'I', 42, 0] || magic == [b'M', b'M', 0, 42] {
        // Start with one page and extend sequentially only to referenced IFDs
        // and tag values. Keep the old 256 KiB ceiling and partial EXIF behavior.
        let prefix = read_tiff_prefix(file, magic)?;
        let mut reader = reader;
        reader.continue_on_error(true);
        reader
            .read_raw(prefix)
            .or_else(|e| e.distill_partial_result(|_| {}))
    } else {
        file.seek(SeekFrom::Start(0))?;
        reader.read_from_container(&mut BufReader::new(MetadataReader {
            file,
            remaining: 2 * 1024 * 1024,
        }))
    };
    match parsed {
        Ok(exif) => Ok(Some(exif)),
        Err(exif::Error::NotFound(_)) => Ok(None),
        Err(exif::Error::InvalidFormat(_)) if is_raw(path) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
const TIFF_PREFIX_LIMIT: usize = 256 * 1024;
fn extend_tiff_prefix(
    file: &mut impl Read,
    bytes: &mut Vec<u8>,
    end: usize,
) -> std::io::Result<()> {
    let end = end.saturating_add(4095).min(TIFF_PREFIX_LIMIT) & !4095;
    if end > bytes.len() {
        file.take((end - bytes.len()) as u64).read_to_end(bytes)?;
    }
    Ok(())
}
fn read_tiff_prefix(file: &mut impl Read, magic: [u8; 4]) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(4096);
    bytes.extend_from_slice(&magic);
    extend_tiff_prefix(file, &mut bytes, 4096)?;
    let little = magic[0] == b'I';
    let word = |data: &[u8], at: usize| -> Option<usize> {
        let value = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(value)
        } else {
            u16::from_be_bytes(value)
        } as usize)
    };
    let long = |data: &[u8], at: usize| -> Option<usize> {
        let value = data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(value)
        } else {
            u32::from_be_bytes(value)
        } as usize)
    };
    let mut pending = vec![long(&bytes, 4).unwrap_or(0)];
    let mut visited = HashSet::new();
    while let Some(offset) = pending.pop() {
        if offset == 0 || offset >= TIFF_PREFIX_LIMIT || !visited.insert(offset) {
            continue;
        }
        // Unusual/malicious chains retain the old bounded-prefix fallback.
        if visited.len() > 64 {
            extend_tiff_prefix(file, &mut bytes, TIFF_PREFIX_LIMIT)?;
            break;
        }
        extend_tiff_prefix(file, &mut bytes, offset + 2)?;
        let Some(count) = word(&bytes, offset) else {
            continue;
        };
        let end = offset + 2 + count * 12;
        extend_tiff_prefix(file, &mut bytes, end + 4)?;
        if let Some(next) = long(&bytes, end) {
            pending.push(next);
        }
        for index in 0..count {
            let at = offset + 2 + index * 12;
            if at + 12 > bytes.len() {
                break;
            }
            let tag = word(&bytes, at).unwrap();
            let kind = word(&bytes, at + 2).unwrap();
            let count = long(&bytes, at + 4).unwrap();
            let unit = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => 0,
            };
            // TIFF counts are u32; use u64 for 32-bit targets and bad offsets.
            let size = count as u64 * unit as u64;
            let value_at = if size > 4 {
                long(&bytes, at + 8).unwrap()
            } else {
                at + 8
            };
            if size > 4 {
                let required = (value_at as u64 + size).min(TIFF_PREFIX_LIMIT as u64) as usize;
                extend_tiff_prefix(file, &mut bytes, required)?;
            }
            if matches!(tag, 34665 | 34853 | 40965) && count > 0 {
                let child = match kind {
                    1 => bytes.get(value_at).map(|v| usize::from(*v)),
                    3 => word(&bytes, value_at),
                    4 => long(&bytes, value_at),
                    _ => None,
                };
                if let Some(child) = child {
                    pending.push(child);
                }
            } else if tag == 330 && matches!(kind, 4 | 13) && count <= 32 {
                for child in 0..count {
                    if let Some(child) = value_at
                        .checked_add(child * 4)
                        .and_then(|at| long(&bytes, at))
                    {
                        pending.push(child);
                    }
                }
            }
        }
    }
    Ok(bytes)
}
struct MetadataReader<'a> {
    file: &'a mut fs::File,
    remaining: usize,
}
impl Read for MetadataReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            return Err(std::io::Error::other(
                "EXIF metadata read exceeded 2 MiB safety limit",
            ));
        }
        let length = buffer.len().min(self.remaining);
        let n = self.file.read(&mut buffer[..length])?;
        self.remaining -= n;
        Ok(n)
    }
}
impl Seek for MetadataReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}
pub(crate) fn apply_exif(photo: &mut Photo, exif: &exif::Exif) {
    use exif::{In, Tag, Value};
    let field = |tag| exif.get_field(tag, In::PRIMARY);
    let number = |tag| field(tag).and_then(|f| f.value.get_uint(0));
    let rational = |tag| {
        field(tag).and_then(|f| match &f.value {
            Value::Rational(v) => v.first().map(|r| r.to_f64()),
            _ => None,
        })
    };
    let text = |tag| {
        field(tag)
            .map(|f| match &f.value {
                Value::Ascii(v) => v
                    .first()
                    .map(|v| {
                        String::from_utf8_lossy(v)
                            .trim_matches('\0')
                            .trim()
                            .to_owned()
                    })
                    .unwrap_or_default(),
                _ => f.display_value().with_unit(exif).to_string(),
            })
            .unwrap_or_default()
    };
    photo.width = number(Tag::PixelXDimension)
        .or_else(|| number(Tag::ImageWidth))
        .unwrap_or(photo.width);
    photo.height = number(Tag::PixelYDimension)
        .or_else(|| number(Tag::ImageLength))
        .unwrap_or(photo.height);
    photo.orientation = number(Tag::Orientation)
        .filter(|n| (1..=8).contains(n))
        .unwrap_or(photo.orientation);
    let model = text(Tag::Model);
    if !model.is_empty() {
        photo.camera = model;
    }
    let lens = text(Tag::LensModel);
    if !lens.is_empty() {
        photo.lens = lens;
    }
    photo.iso = number(Tag::PhotographicSensitivity).or(photo.iso);
    photo.aperture = rational(Tag::FNumber).or(photo.aperture);
    photo.focal_length = rational(Tag::FocalLength).or(photo.focal_length);
    let shutter = text(Tag::ExposureTime);
    if !shutter.is_empty() {
        photo.shutter = shutter;
    }
    let date = text(Tag::DateTimeOriginal);
    let subsec = text(Tag::SubSecTimeOriginal);
    let offset = text(Tag::OffsetTimeOriginal);
    if let Some(time) = parse_capture_time(&date, &subsec, &offset) {
        photo.captured_at = Some(time);
        photo.captured_offset = parse_capture_offset(&offset).map(|_| offset);
        photo.capture_time_kind = if photo.captured_offset.is_some() {
            "offset"
        } else {
            "camera_local"
        }
        .into();
    }
}
fn parse_capture_time(date: &str, subsec: &str, offset: &str) -> Option<i64> {
    let base = chrono::NaiveDateTime::parse_from_str(date, "%Y:%m:%d %H:%M:%S").ok()?;
    let fraction: String = subsec
        .chars()
        .take_while(char::is_ascii_digit)
        .take(3)
        .collect();
    let ms = fraction.parse::<i64>().unwrap_or(0)
        * 10i64.pow(3u32.saturating_sub(fraction.len() as u32));
    let offset_seconds = parse_capture_offset(offset)
        .map(|offset| i64::from(offset.local_minus_utc()))
        .unwrap_or(0);
    Some(base.and_utc().timestamp_millis() + ms - offset_seconds * 1000)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Drive {
    pub name: String,
    pub path: PathBuf,
    pub removable: bool,
    pub photo_folders: Vec<PathBuf>,
}
/// Mounted source candidates only. Indexing a Drive.path scans the whole selected drive.
pub fn discover_drives() -> Vec<Drive> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();
    #[cfg(target_os = "linux")]
    if let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") {
        use std::sync::{Mutex, OnceLock};
        static REMOVABLE: OnceLock<Mutex<HashMap<(String, String), bool>>> = OnceLock::new();
        let mut cached = REMOVABLE
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut live = HashSet::new();
        for line in mounts.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let source = right.split_whitespace().nth(1).unwrap_or("");
            // Pseudo mounts can never be source drives. Avoid their sysfs probes.
            if !source.starts_with("/dev/") {
                continue;
            }
            let mut fields = left.split_whitespace();
            let Some(id) = fields.next() else {
                continue;
            };
            let Some(device) = fields.nth(1) else {
                continue;
            };
            let Some(mount) = fields.nth(1) else {
                continue;
            };
            let path = PathBuf::from(unescape_mount(mount));
            let external = path.starts_with("/media")
                || path.starts_with("/run/media")
                || path.starts_with("/mnt");
            let key = (id.to_owned(), device.to_owned());
            let removable = external
                || *cached
                    .entry(key.clone())
                    .or_insert_with(|| removable_device(device));
            live.insert(key);
            if removable {
                paths.push((path, true));
            }
        }
        cached.retain(|key, _| live.contains(key));
    }
    #[cfg(target_os = "macos")]
    if let Ok(entries) = fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            // APFS system, recovery and update volumes are never photo sources.
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || matches!(
                    name.as_str(),
                    "Recovery"
                        | "Preboot"
                        | "VM"
                        | "Update"
                        | "Data"
                        | "xarts"
                        | "iSCPreboot"
                        | "Hardware"
                )
            {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .path()
                    .canonicalize()
                    .is_ok_and(|p| p != Path::new("/"))
            {
                paths.push((entry.path(), true));
            }
        }
    }
    let mut drives = Vec::new();
    let mut seen = HashSet::new();
    for (path, removable) in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let photo_folders = [
            "DCIM", "dcim", "Pictures", "Photos", "photos", "PRIVATE", "private", "MP_ROOT",
            "mp_root",
        ]
        .iter()
        .map(|p| path.join(p))
        .filter(|p| p.is_dir())
        .collect();
        drives.push(Drive {
            name,
            path,
            removable,
            photo_folders,
        });
    }
    drives.sort_by(|a, b| a.path.cmp(&b.path));
    drives
}
#[cfg(target_os = "linux")]
fn removable_device(device: &str) -> bool {
    let Ok(sys_path) = PathBuf::from("/sys/dev/block").join(device).canonicalize() else {
        return false;
    };
    if sys_path.to_string_lossy().contains("/usb") {
        return true;
    }
    for ancestor in sys_path.ancestors().take(4) {
        if fs::read_to_string(ancestor.join("removable")).is_ok_and(|s| s.trim() == "1") {
            return true;
        }
    }
    false
}
fn unescape_mount(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            out.push((bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + bytes[i + 3] - b'0');
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Run serially so /proc/self/io counts only this workload's metadata reads.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn index_performance_harness() {
        let dir = tempfile::tempdir_in(
            std::env::var_os("SHADOWROOM_INDEX_BENCH_DIR").unwrap_or_else(|| "/tmp".into()),
        )
        .unwrap();
        let raw = dir.path().join("template.nef");
        fs::write(
            &raw,
            tiff_with_metadata(
                6000,
                4000,
                "Bench camera",
                "Bench lens",
                "2024:01:01 06:30:00",
                "+05:30",
                100,
            ),
        )
        .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&raw)
            .unwrap()
            .set_len(60 * 1024 * 1024)
            .unwrap();
        let rchar = || -> u64 {
            fs::read_to_string("/proc/self/io")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("rchar: ").and_then(|n| n.parse().ok()))
                .unwrap()
        };
        let before = rchar();
        let started = Instant::now();
        for _ in 0..2000 {
            assert!(read_exif_bounded(&raw).unwrap().is_some());
        }
        eprintln!(
            "BENCH metadata 2000x60MiB TIFF: {:?}, rchar={}",
            started.elapsed(),
            rchar() - before
        );
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        let jpeg = dir.path().join("template.jpg");
        image::RgbImage::new(32, 24).save(&jpeg).unwrap();
        for folder in 0..200 {
            let path = root.join(format!("folder{folder:03}"));
            fs::create_dir(&path).unwrap();
            for image in 0..40 {
                fs::hard_link(&raw, path.join(format!("frame{image:03}.nef"))).unwrap();
                fs::hard_link(&jpeg, path.join(format!("frame{image:03}.jpg"))).unwrap();
            }
            for item in 0..20 {
                fs::write(path.join(format!("note{item}.txt")), b"ignored").unwrap();
            }
        }
        let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        for pass in 0..3 {
            let before = rchar();
            let started = Instant::now();
            let job = Indexer::start(catalog.clone(), root.clone(), IndexOptions::default());
            let receiver = job.events.clone();
            job.join().unwrap();
            let mut done = None;
            let mut events = 0;
            for event in receiver.try_iter() {
                events += 1;
                if let IndexEvent::Finished(progress) = event {
                    done = Some(progress);
                }
            }
            let done = done.expect("successful scan");
            assert_eq!(done.errors, 0);
            assert_eq!(done.scanned, 16000);
            assert_eq!(done.indexed + done.unchanged, 8000);
            eprintln!(
                "BENCH tree pass={pass}: {:?}, rchar={}, indexed={}, unchanged={}, events={events}",
                started.elapsed(),
                rchar() - before,
                done.indexed,
                done.unchanged
            );
        }
        let started = Instant::now();
        for _ in 0..100 {
            std::hint::black_box(discover_drives());
        }
        eprintln!("BENCH discovery 100 ticks: {:?}", started.elapsed());
    }
    #[test]
    fn heif_is_indexed_without_a_codec_and_raw_pairing_prefers_jpeg() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("card");
        fs::create_dir(&root).unwrap();
        for name in [
            "paired.arw",
            "paired.HEIC",
            "both.nef",
            "both.jpg",
            "both.heif",
            "alone.heic",
        ] {
            fs::write(root.join(name), b"synthetic no pixels").unwrap();
        }
        let catalog = Catalog::open(&directory.path().join("db")).unwrap();
        let options = IndexOptions {
            read_exif: false,
            ..Default::default()
        };
        Indexer::start(catalog.clone(), root.clone(), options.clone())
            .join()
            .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 4);
        let paired = photos.iter().find(|p| p.filename == "paired.arw").unwrap();
        assert_eq!(
            paired.companion_path.as_ref().unwrap().file_name().unwrap(),
            "paired.HEIC"
        );
        let both = photos.iter().find(|p| p.filename == "both.nef").unwrap();
        assert_eq!(
            both.companion_path.as_ref().unwrap().file_name().unwrap(),
            "both.jpg"
        );
        assert!(photos.iter().any(|p| p.filename == "both.heif"));
        assert!(photos.iter().any(|p| p.filename == "alone.heic"));
        let id = paired.id;
        fs::write(root.join("paired.HEIC"), b"changed HEIF companion length").unwrap();
        Indexer::start(catalog.clone(), root.clone(), options.clone())
            .join()
            .unwrap();
        let paired = catalog
            .photos()
            .unwrap()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        assert_eq!(paired.companion_size_bytes, 29);
        let job = Indexer::start(catalog, root, options);
        let events = job.events.clone();
        job.join().unwrap();
        assert!(events.try_iter().any(
            |event| matches!(event,IndexEvent::Finished(p) if p.unchanged==4 && p.indexed==0)
        ));
    }
    #[test]
    fn separately_cataloged_raw_heif_ids_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("card");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("frame.dng"), b"raw").unwrap();
        fs::write(root.join("frame.heif"), b"heif").unwrap();
        let catalog = Catalog::open(&directory.path().join("db")).unwrap();
        Indexer::start(
            catalog.clone(),
            root.clone(),
            IndexOptions {
                read_exif: false,
                pair_raw_jpeg: false,
                ..Default::default()
            },
        )
        .join()
        .unwrap();
        let before: HashMap<_, _> = catalog
            .photos()
            .unwrap()
            .into_iter()
            .map(|p| (p.path, p.id))
            .collect();
        Indexer::start(
            catalog.clone(),
            root,
            IndexOptions {
                read_exif: false,
                ..Default::default()
            },
        )
        .join()
        .unwrap();
        let after = catalog.photos().unwrap();
        assert_eq!(after.len(), 2);
        for photo in after {
            assert_eq!(before.get(&photo.path), Some(&photo.id));
            assert!(photo.companion_path.is_none());
        }
    }
    #[test]
    fn incremental_pairing_and_hidden_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("A.NEF"), b"raw").unwrap();
        fs::write(root.join("A.JPG"), b"jpeg").unwrap();
        fs::write(root.join(".hidden/secret.jpg"), b"hidden").unwrap();
        let c = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        let options = IndexOptions {
            read_exif: false,
            ..Default::default()
        };
        Indexer::start(c.clone(), root.clone(), options.clone())
            .join()
            .unwrap();
        let photos = c.photos().unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(
            photos[0].companion_path,
            Some(fs::canonicalize(root.join("A.JPG")).unwrap())
        );
        let job = Indexer::start(c.clone(), root, options);
        let receiver = job.events.clone();
        job.join().unwrap();
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e,IndexEvent::Finished(p) if p.unchanged==1 && p.indexed==0))
        );
    }
    #[test]
    fn existing_independent_pair_keeps_both_ids_and_refreshes_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        let raw = root.join("frame.nef");
        let jpeg = root.join("frame.jpg");
        fs::write(&raw, b"raw").unwrap();
        fs::write(&jpeg, b"jpeg").unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        Indexer::start(
            catalog.clone(),
            root.clone(),
            IndexOptions {
                read_exif: false,
                pair_raw_jpeg: false,
                ..Default::default()
            },
        )
        .join()
        .unwrap();
        let before: HashMap<_, _> = catalog
            .photos()
            .unwrap()
            .into_iter()
            .map(|p| (p.path, p.id))
            .collect();
        fs::write(&jpeg, b"updated jpeg bytes").unwrap();
        Indexer::start(
            catalog.clone(),
            root,
            IndexOptions {
                read_exif: false,
                ..Default::default()
            },
        )
        .join()
        .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 2);
        for photo in photos {
            assert_eq!(before.get(&photo.path), Some(&photo.id));
            assert!(photo.companion_path.is_none());
            if photo.path == jpeg {
                assert_eq!(photo.size_bytes, 18);
            }
        }
    }
    #[test]
    fn multiple_raw_variants_share_no_companion_and_reindex_stays_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        for name in ["frame.nef", "frame.arw", "frame.jpg"] {
            fs::write(root.join(name), b"photo").unwrap();
        }
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        let options = IndexOptions {
            read_exif: false,
            ..Default::default()
        };
        Indexer::start(catalog.clone(), root.clone(), options.clone())
            .join()
            .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 2);
        let owners: Vec<_> = photos
            .iter()
            .filter(|p| p.companion_path.is_some())
            .collect();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].filename, "frame.arw");
        let job = Indexer::start(catalog, root, options);
        let events = job.events.clone();
        job.join().unwrap();
        assert!(events.try_iter().any(
            |event| matches!(event,IndexEvent::Finished(p) if p.unchanged==2 && p.indexed==0)
        ));
    }
    #[test]
    fn adaptive_tiff_prefix_matches_full_bounded_parser() {
        fn check(bytes: &[u8]) -> usize {
            use std::io::Cursor;
            let mut input = Cursor::new(&bytes[4..]);
            let prefix = read_tiff_prefix(&mut input, bytes[..4].try_into().unwrap()).unwrap();
            assert!(prefix.len() <= TIFF_PREFIX_LIMIT);
            let parse = |bytes: Vec<u8>| {
                let mut reader = exif::Reader::new();
                reader.continue_on_error(true);
                reader
                    .read_raw(bytes)
                    .or_else(|error| error.distill_partial_result(|_| {}))
                    .map(|exif| {
                        let mut photo = Photo::default();
                        apply_exif(&mut photo, &exif);
                        (
                            serde_json::to_value(photo).unwrap(),
                            raw_sensor_dimensions(exif.buf()),
                        )
                    })
                    .ok()
            };
            assert_eq!(
                parse(prefix.clone()),
                parse(bytes[..bytes.len().min(TIFF_PREFIX_LIMIT)].to_vec())
            );
            prefix.len()
        }
        let mut bytes = tiff_with_metadata(
            6000,
            4000,
            "Camera",
            "Lens",
            "2024:01:01 06:30:00",
            "+05:30",
            100,
        );
        bytes.resize(1024 * 1024, 0);
        assert_eq!(check(&bytes), 4096);
        // Relocate the complete root IFD while keeping its tag-value offsets
        // and EXIF child pointer unchanged. This requires multiple prefix pages.
        let mut distant = bytes.clone();
        distant[4..8].copy_from_slice(&120_000u32.to_le_bytes());
        distant[120_000..120_066].copy_from_slice(&bytes[8..74]);
        assert!(check(&distant) >= 120_066);
        // Big-endian TIFF, source dimensions and an out-of-line ASCII model.
        let mut big = vec![b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 3];
        for (tag, kind, count, value) in [
            (256u16, 4u16, 1u32, 6000u32),
            (257, 4, 1, 4000),
            (272, 2, 7, 20_000),
        ] {
            big.extend(tag.to_be_bytes());
            big.extend(kind.to_be_bytes());
            big.extend(count.to_be_bytes());
            big.extend(value.to_be_bytes());
        }
        big.extend(0u32.to_be_bytes());
        big.resize(512 * 1024, 0);
        big[20_000..20_007].copy_from_slice(b"Camera\0");
        check(&big);
        // Corrupt counts, offsets, truncation and cycles retain partial tags
        // without unbounded reads, arithmetic overflow or infinite traversal.
        for pointer in [0u32, 1, 4095, 262_140, 262_144, u32::MAX] {
            let mut bad = bytes.clone();
            bad[4..8].copy_from_slice(&pointer.to_le_bytes());
            check(&bad);
        }
        let mut bad = bytes.clone();
        bad[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        check(&bad);
        let mut cycle = bytes.clone();
        cycle[70..74].copy_from_slice(&8u32.to_le_bytes());
        check(&cycle);
        for length in [4, 7, 9, 31, 73, 100] {
            check(&bytes[..length]);
        }
    }
    #[test]
    fn raw_exif_reads_only_bounded_header() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.nef");
        let mut bytes = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 3, 0];
        for (tag, kind, value) in [(256u16, 4u16, 6000u32), (257, 4, 4000), (274, 3, 6)] {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&bytes).unwrap();
        file.set_len(256 * 1024 * 1024).unwrap();
        drop(file);
        let exif = read_exif_bounded(&path).unwrap().unwrap();
        assert!(exif.buf().len() <= 256 * 1024);
        let mut photo = Photo::default();
        apply_exif(&mut photo, &exif);
        assert_eq!(
            (photo.width, photo.height, photo.orientation),
            (6000, 4000, 6)
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), 256 * 1024 * 1024);
    }
    fn tiff_with_metadata(
        width: u32,
        height: u32,
        model: &str,
        lens: &str,
        date: &str,
        offset: &str,
        iso: u16,
    ) -> Vec<u8> {
        fn ascii(text: &str) -> Vec<u8> {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            bytes
        }
        fn append_ifd(bytes: &mut Vec<u8>, entries: &[(u16, u16, Vec<u8>)]) -> usize {
            let start = bytes.len();
            bytes.extend((entries.len() as u16).to_le_bytes());
            bytes.resize(start + 2 + entries.len() * 12 + 4, 0);
            for (index, (tag, kind, data)) in entries.iter().enumerate() {
                let pos = start + 2 + index * 12;
                bytes[pos..pos + 2].copy_from_slice(&tag.to_le_bytes());
                bytes[pos + 2..pos + 4].copy_from_slice(&kind.to_le_bytes());
                let width = match kind {
                    3 => 2,
                    4 => 4,
                    _ => 1,
                };
                let count = data.len() / width;
                bytes[pos + 4..pos + 8].copy_from_slice(&(count as u32).to_le_bytes());
                if data.len() <= 4 {
                    bytes[pos + 8..pos + 8 + data.len()].copy_from_slice(data);
                } else {
                    let offset = bytes.len() as u32;
                    bytes[pos + 8..pos + 12].copy_from_slice(&offset.to_le_bytes());
                    bytes.extend(data);
                }
            }
            start
        }
        let mut bytes = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        let root = append_ifd(
            &mut bytes,
            &[
                (256, 4, width.to_le_bytes().to_vec()),
                (257, 4, height.to_le_bytes().to_vec()),
                (274, 3, 6u16.to_le_bytes().to_vec()),
                (272, 2, ascii(model)),
                (34665, 4, 0u32.to_le_bytes().to_vec()),
            ],
        );
        let mut tags = vec![
            (36867, 2, ascii(date)),
            (37521, 2, ascii("125")),
            (34855, 3, iso.to_le_bytes().to_vec()),
        ];
        if !lens.is_empty() {
            tags.push((42036, 2, ascii(lens)));
        }
        if !offset.is_empty() {
            tags.push((36881, 2, ascii(offset)));
        }
        let exif = append_ifd(&mut bytes, &tags) as u32;
        let pointer = root + 2 + 4 * 12 + 8;
        bytes[pointer..pointer + 4].copy_from_slice(&exif.to_le_bytes());
        bytes
    }
    #[test]
    fn raw_pair_keeps_source_dimensions_camera_and_capture_time() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        let raw = root.join("paired.nef");
        let jpeg = root.join("paired.jpg");
        let mut file = fs::File::create(&raw).unwrap();
        file.write_all(&tiff_with_metadata(
            6000,
            4000,
            "RAW camera",
            "",
            "2024:01:01 06:30:00",
            "+05:30",
            100,
        ))
        .unwrap();
        file.set_len(256 * 1024 * 1024).unwrap();
        drop(file);
        image::RgbImage::new(1200, 800).save(&jpeg).unwrap();
        let original = fs::read(&jpeg).unwrap();
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend(tiff_with_metadata(
            1200,
            800,
            "JPEG camera",
            "JPEG lens",
            "1999:01:01 00:00:00",
            "-08:00",
            800,
        ));
        let mut paired = vec![0xff, 0xd8, 0xff, 0xe1];
        paired.extend(((payload.len() + 2) as u16).to_be_bytes());
        paired.extend(payload);
        paired.extend_from_slice(&original[2..]);
        fs::write(&jpeg, paired).unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        Indexer::start(catalog.clone(), root.clone(), IndexOptions::default())
            .join()
            .unwrap();
        let mut photo = catalog.photos().unwrap().remove(0);
        assert_eq!((photo.width, photo.height), (6000, 4000));
        assert_eq!(photo.camera, "RAW camera");
        assert_eq!(photo.lens, "JPEG lens");
        assert_eq!(photo.iso, Some(100));
        assert_eq!(photo.captured_offset.as_deref(), Some("+05:30"));
        assert_eq!(photo.capture_time_kind, "offset");
        assert_eq!(photo.capture_display(), "2024-01-01 06:30:00.125 +05:30");
        assert_eq!(photo.orientation, 6);
        assert_eq!(photo.metadata_version, METADATA_VERSION);
        // Old extraction revisions are refreshed once even when files are unchanged.
        catalog
            .set_flag(photo.id, crate::model::Flag::Pick)
            .unwrap();
        photo.width = 1200;
        photo.metadata_version = 0;
        catalog.upsert_photos(&[photo]).unwrap();
        Indexer::start(catalog.clone(), root, IndexOptions::default())
            .join()
            .unwrap();
        let refreshed = catalog.photos().unwrap().remove(0);
        assert_eq!(refreshed.width, 6000);
        assert_eq!(refreshed.flag, crate::model::Flag::Pick);
        assert_eq!(refreshed.metadata_version, METADATA_VERSION);
    }
    #[test]
    fn raw_subifd_dimensions_are_read_from_existing_prefix() {
        let mut bytes = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 3, 0];
        for (tag, value) in [(256u16, 640u32), (257, 480), (330, 50)] {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(4u16.to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
        assert_eq!(bytes.len(), 50);
        bytes.extend(3u16.to_le_bytes());
        for (tag, value) in [(256u16, 6048u32), (257, 4024), (262, 32803)] {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(4u16.to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
        assert_eq!(raw_sensor_dimensions(&bytes), Some((6048, 4024)));
        bytes.truncate(52);
        assert_eq!(raw_sensor_dimensions(&bytes), None);
    }
    #[test]
    fn missing_raw_offset_uses_jpeg_offset_without_replacing_raw_clock() {
        let mut raw = Photo {
            captured_at: parse_capture_time("2024:01:01 06:30:00", "125", ""),
            capture_time_kind: "camera_local".into(),
            ..Default::default()
        };
        let jpeg = Photo {
            captured_at: Some(0),
            captured_offset: Some("+05:30".into()),
            capture_time_kind: "offset".into(),
            ..Default::default()
        };
        fill_missing_metadata(&mut raw, &jpeg);
        assert_eq!(raw.capture_display(), "2024-01-01 06:30:00.125 +05:30");
        let base = parse_capture_time("2024:01:01 00:00:00", "", "bad");
        assert_eq!(base, Some(1704067200000));
    }
    #[test]
    fn cancellation_emits_terminal_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        let c = Catalog::open(&dir.path().join("db")).unwrap();
        let (sender, receiver) = crossbeam_channel::unbounded();
        let cancelled = AtomicBool::new(true);
        scan(&c, &root, &IndexOptions::default(), &cancelled, &sender).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            IndexEvent::Cancelled(_)
        ));
    }
    #[test]
    fn capture_fraction_and_offset() {
        assert_eq!(
            parse_capture_time("2024:01:01 01:00:00", "5", "+01:00"),
            Some(1704067200500)
        );
        assert_eq!(unescape_mount("/media/My\\040Card"), "/media/My Card");
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(dir.path().join("outside.jpg"), b"outside").unwrap();
        symlink(dir.path().join("outside.jpg"), root.join("linked.jpg")).unwrap();
        let c = Catalog::open(&dir.path().join("c.db")).unwrap();
        Indexer::start(
            c.clone(),
            root,
            IndexOptions {
                read_exif: false,
                ..Default::default()
            },
        )
        .join()
        .unwrap();
        assert!(c.photos().unwrap().is_empty());
    }
}
