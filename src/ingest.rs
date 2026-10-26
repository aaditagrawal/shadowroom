//! Read-only source scanning, pure naming plans, durable verified ingest.
mod copy;
pub mod naming;
pub mod store;
#[cfg(test)]
mod tests;
pub mod worker;

use crate::{
    catalog::Catalog,
    indexer,
    model::{Flag, Photo},
    settings::Settings,
};
use anyhow::{Context, Result, ensure};
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub use copy::{
    CopyHooks, DefaultHooks, FsCapacity, FsStats, NativeFsStats, SpaceProjection, execute,
    execute_with, preflight, space as copy_space,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: PathBuf,
    pub size: u64,
    /// Nanoseconds retained for exact copy fingerprints and timezone-tolerant history.
    /// Photo metadata uses milliseconds, matching the existing indexer.
    pub mtime_ns: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestGroup {
    pub id: usize,
    pub primary: SourceFile,
    pub companion: Option<SourceFile>,
    pub sidecars: Vec<SourceFile>,
    pub kind: String,
    pub capture_ms: i64,
    pub capture_kind: String,
    pub photo: Photo,
    pub make: String,
    pub serial: String,
    pub volume: String,
    pub relative_path: PathBuf,
    pub content_hash: Option<String>,
    pub seen: Option<store::SeenRecord>,
    pub selected: bool,
    pub warning: Option<String>,
}
impl IngestGroup {
    pub fn files(&self) -> impl Iterator<Item = &SourceFile> {
        std::iter::once(&self.primary)
            .chain(self.companion.iter())
            .chain(self.sidecars.iter())
    }
    pub fn bytes(&self) -> u64 {
        self.files().map(|f| f.size).sum()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scan {
    pub source: PathBuf,
    pub volume: String,
    pub groups: Vec<IngestGroup>,
    pub warnings: Vec<String>,
    pub clock: DeviceClock,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SequenceState {
    pub stored: u64,
    pub days: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestOptions {
    pub library_root: PathBuf,
    pub folder_template: String,
    pub file_template: String,
    pub backup_roots: Vec<PathBuf>,
    pub job_code: String,
    pub day_start_hour: u8,
    pub collision: String,
    pub include_videos: bool,
    /// all | picks | except_rejects | rated. Catalog decisions are snapshotted by the worker.
    pub selection: String,
    pub min_rating: u8,
    /// None follows checkboxes/new-only; Some includes precisely these scan group IDs.
    pub selected_ids: Option<Vec<usize>>,
    pub imported_at_ms: i64,
}
impl Default for IngestOptions {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}
impl IngestOptions {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            library_root: settings.ingest_library_root.clone(),
            folder_template: settings.ingest_folder_template.clone(),
            file_template: settings.ingest_file_template.clone(),
            backup_roots: settings.ingest_backup_roots.clone(),
            job_code: String::new(),
            day_start_hour: settings.ingest_day_start_hour,
            collision: settings.ingest_collision.clone(),
            include_videos: settings.ingest_include_videos,
            selection: "all".into(),
            min_rating: 1,
            selected_ids: None,
            imported_at_ms: Utc::now().timestamp_millis(),
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.day_start_hour <= 23 && self.min_rating <= 5,
            "Invalid day start or rating"
        );
        ensure!(
            ["suffix", "skip"].contains(&self.collision.as_str()),
            "Collision must be suffix or skip"
        );
        ensure!(
            ["all", "picks", "except_rejects", "rated"].contains(&self.selection.as_str()),
            "Invalid import selection"
        );
        ensure!(
            self.library_root.is_absolute() && self.backup_roots.iter().all(|p| p.is_absolute()),
            "Destination roots must be absolute"
        );
        ensure!(
            self.backup_roots.len() <= 16,
            "At most 16 backup destinations"
        );
        ensure!(self.job_code.len() <= 256, "Job code is too long");
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedFile {
    pub source: SourceFile,
    pub relative: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedGroup {
    pub group: IngestGroup,
    pub files: Vec<PlannedFile>,
    pub day: String,
    pub session: u64,
    pub letter: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestPlan {
    pub source: PathBuf,
    pub options: IngestOptions,
    pub sequences: SequenceState,
    pub groups: Vec<PlannedGroup>,
    pub photos: usize,
    pub videos: usize,
    pub bytes: u64,
    pub files: usize,
    pub example: Option<PathBuf>,
    pub timeline: Vec<TimeGroup>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeGroup {
    pub month: String,
    pub day: String,
    pub label: String,
    pub ids: Vec<usize>,
}

pub fn timeline(scan: &Scan, gap_minutes: u32) -> Vec<TimeGroup> {
    let mut groups: Vec<TimeGroup> = Vec::new();
    let mut last = None;
    for item in &scan.groups {
        let time = naming::wall_time(item);
        let day = time.format("%Y-%m-%d").to_string();
        if last.is_none_or(|previous: chrono::NaiveDateTime| {
            time.signed_duration_since(previous).num_seconds() > i64::from(gap_minutes.max(1)) * 60
                || previous.date() != time.date()
        }) {
            groups.push(TimeGroup {
                month: time.format("%Y-%m").to_string(),
                day,
                label: time.format("%H:%M").to_string(),
                ids: Vec::new(),
            });
        }
        groups.last_mut().expect("time group").ids.push(item.id);
        last = Some(time);
    }
    groups
}
/// Pure: does not inspect destinations, read media, or mutate sequence state.
pub fn plan(
    scan: &Scan,
    options: IngestOptions,
    mut sequences: SequenceState,
) -> Result<IngestPlan> {
    options.validate()?;
    let initial = sequences.clone();
    let selected: Option<HashSet<_>> = options
        .selected_ids
        .as_ref()
        .map(|ids| ids.iter().copied().collect());
    if let Some(ids) = &selected {
        ensure!(
            ids.is_subset(&scan.groups.iter().map(|group| group.id).collect()),
            "Unknown ingest group ID"
        );
    }
    let mut groups = Vec::new();
    let mut seconds = HashMap::<i64, u64>::new();
    let mut paths = HashSet::new();
    let mut photos = 0;
    let mut videos = 0;
    let mut bytes = 0u64;
    let mut files = 0;
    for group in &scan.groups {
        if !selected
            .as_ref()
            .map_or(group.selected, |ids| ids.contains(&group.id))
            || (group.kind == "video" && !options.include_videos)
        {
            continue;
        }
        let decision = &group.photo;
        if decision.trashed
            || match options.selection.as_str() {
                "picks" => decision.flag != Flag::Pick,
                "except_rejects" => decision.flag == Flag::Reject,
                "rated" => decision.rating < options.min_rating,
                _ => false,
            }
        {
            continue;
        }
        let letter = seconds
            .entry(group.capture_ms.div_euclid(1000))
            .or_default();
        let mut planned = name_group(group, &options, &sequences, groups.len() as u64, *letter)?;
        // In-plan collisions are resolved without filesystem access. A common suffix
        // protects pairs and sidecars even when the chosen template omits sequences.
        let original = planned.files.clone();
        let mut suffix = 0;
        while planned
            .files
            .iter()
            .any(|f| paths.contains(&f.relative.to_string_lossy().to_lowercase()))
        {
            suffix += 1;
            planned.files = suffixed(&original, group, suffix);
        }
        for file in &planned.files {
            paths.insert(file.relative.to_string_lossy().to_lowercase());
        }
        sequences.stored += 1;
        *sequences.days.entry(planned.day.clone()).or_default() += 1;
        *letter += 1;
        if group.kind == "video" {
            videos += 1;
        } else {
            photos += 1;
        }
        bytes = bytes
            .checked_add(group.bytes())
            .context("Ingest size overflow")?;
        files += planned.files.len();
        groups.push(planned);
    }
    let example = groups
        .first()
        .and_then(|g| g.files.first())
        .map(|f| options.library_root.join(&f.relative));
    Ok(IngestPlan {
        source: scan.source.clone(),
        options,
        sequences: initial,
        groups,
        photos,
        videos,
        bytes,
        files,
        example,
        timeline: timeline(scan, 60),
    })
}
fn name_group(
    group: &IngestGroup,
    options: &IngestOptions,
    state: &SequenceState,
    session: u64,
    letter: u64,
) -> Result<PlannedGroup> {
    let context = naming::Context {
        group,
        options,
        sequences: state,
        session,
        letter,
    };
    let folder = context.expand(&options.folder_template, true)?;
    let stem = context.expand(&options.file_template, false)?;
    let mut used = HashSet::new();
    let mut files = Vec::new();
    for source in group.files() {
        let suffix = naming::member_suffix(&source.path, group);
        let mut name = format!("{}{suffix}", stem.display());
        let mut alternate = 0;
        while !used.insert(name.to_lowercase()) {
            alternate += 1;
            name = format!("{}.alternate-{alternate}{suffix}", stem.display());
        }
        files.push(PlannedFile {
            source: source.clone(),
            relative: folder.join(name),
        });
    }
    Ok(PlannedGroup {
        group: group.clone(),
        files,
        day: naming::day_key(group, options.day_start_hour),
        session,
        letter,
    })
}
fn suffixed(files: &[PlannedFile], group: &IngestGroup, number: usize) -> Vec<PlannedFile> {
    files
        .iter()
        .map(|file| {
            let suffix = naming::member_suffix(&file.source.path, group);
            let filename = file
                .relative
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let base = filename.strip_suffix(&suffix).unwrap_or(&filename);
            PlannedFile {
                source: file.source.clone(),
                relative: file
                    .relative
                    .with_file_name(format!("{base}-{number}{suffix}")),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClock {
    Utc,
    Local,
    #[default]
    Unknown,
}
pub fn infer_device_clock(wall_ms: i64, mtime_ms: i64, local_offset_seconds: i32) -> DeviceClock {
    let delta = i128::from(wall_ms) - i128::from(mtime_ms);
    if delta.abs() <= 120_000 {
        DeviceClock::Utc
    } else if (delta - i128::from(local_offset_seconds) * 1000).abs() <= 120_000 {
        DeviceClock::Local
    } else {
        DeviceClock::Unknown
    }
}
fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64)
}
fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}
pub fn is_video(path: &Path) -> bool {
    [
        "mp4", "mov", "m4v", "mts", "m2ts", "avi", "mxf", "mkv", "braw", "crm", "3gp", "insv",
        "lrv",
    ]
    .contains(&extension(path).as_str())
}
fn is_sidecar(path: &Path) -> bool {
    ["xmp", "aae", "thm", "wav", "mp3", "log", "lrv"].contains(&extension(path).as_str())
}
fn allowed(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    !name.starts_with('.') && !["thmbnl", "__macosx", "screenshots"].contains(&name.as_str())
}
/// Bounded EXIF headers only; source contents are not hashed until copy.
/// Batches contain complete groups, so pairing never changes an already-published item.
pub fn scan(
    path: &Path,
    catalog: &Catalog,
    batch: impl FnMut(&[IngestGroup]),
    control: &Control,
) -> Result<Scan> {
    scan_internal(path, catalog, batch, control, false)
}
pub fn scan_card(
    path: &Path,
    catalog: &Catalog,
    batch: impl FnMut(&[IngestGroup]),
    control: &Control,
) -> Result<Scan> {
    scan_internal(path, catalog, batch, control, true)
}
fn scan_internal(
    path: &Path,
    catalog: &Catalog,
    mut batch: impl FnMut(&[IngestGroup]),
    control: &Control,
    card: bool,
) -> Result<Scan> {
    let source = path.canonicalize().context("Open import source folder")?;
    ensure!(source.is_dir(), "Import source must be a directory");
    ensure!(
        !catalog.path().canonicalize()?.starts_with(&source),
        "The writable catalog must be outside the read-only import source"
    );
    let volume = volume_identity(&source);
    let mut result = Scan {
        source: source.clone(),
        volume: volume.clone(),
        ..Default::default()
    };
    let decisions = catalog.ingest_source_decisions(&source)?;
    let mut published = 0;
    // Process one directory at a time. Metadata and thumbnail batches can arrive
    // before a large card has finished enumeration; symlinks are never followed.
    for directory in walkdir::WalkDir::new(&source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            allowed(entry)
                && (!card
                    || entry.depth() != 1
                    || entry.file_type().is_dir()
                        && ["dcim", "private", "mp_root"].contains(
                            &entry
                                .file_name()
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .as_str(),
                        ))
        })
    {
        control.checkpoint()?;
        let directory = match directory {
            Ok(entry) => entry,
            Err(error) => {
                result.warnings.push(error.to_string());
                continue;
            }
        };
        if !directory.file_type().is_dir() || (card && directory.depth() == 0) {
            continue;
        }
        let mut entries = Vec::new();
        let children = match fs::read_dir(directory.path()) {
            Ok(children) => children,
            Err(error) => {
                result
                    .warnings
                    .push(format!("{}: {error}", directory.path().display()));
                continue;
            }
        };
        for entry in children {
            control.checkpoint()?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.warnings.push(error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            if !entry.file_type().is_ok_and(|t| t.is_file())
                || path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                || !(indexer::is_photo(&path) || is_video(&path) || is_sidecar(&path))
            {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) if metadata.len() > 0 => {
                    if let Err(error) = fs::File::open(&path) {
                        // Retain this member so copy fails/rolls back its entire
                        // RAW/JPEG/sidecar group instead of silently dropping it.
                        result
                            .warnings
                            .push(format!("Unreadable {}: {error}", path.display()));
                    }
                    entries.push(SourceFile {
                        path,
                        size: metadata.len(),
                        mtime_ns: modified_ns(&metadata),
                    });
                }
                Ok(_) => result
                    .warnings
                    .push(format!("Zero-length file: {}", path.display())),
                Err(error) => result.warnings.push(format!("{}: {error}", path.display())),
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut by_stem = BTreeMap::<String, Vec<SourceFile>>::new();
        let mut sidecars = HashMap::<String, Vec<SourceFile>>::new();
        // LRV is a sidecar when a same-stem main video exists, standalone video otherwise.
        let video_stems: HashSet<_> = entries
            .iter()
            .filter(|f| is_video(&f.path) && extension(&f.path) != "lrv")
            .map(|f| {
                f.path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
            })
            .collect();
        for entry in entries {
            let stem = entry
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if is_sidecar(&entry.path)
                && (extension(&entry.path) != "lrv" || video_stems.contains(&stem))
            {
                sidecars.entry(stem).or_default().push(entry);
            } else {
                by_stem.entry(stem).or_default().push(entry);
            }
        }
        let mut claimed = HashSet::new();
        for (stem, mut media) in by_stem {
            // RAW first, then JPEG, HEIF; videos never pair with stills.
            media.sort_by_key(|f| {
                (
                    if indexer::is_raw(&f.path) {
                        0
                    } else if matches!(extension(&f.path).as_str(), "jpg" | "jpeg") {
                        1
                    } else {
                        2
                    },
                    f.path.clone(),
                )
            });
            while !media.is_empty() {
                control.checkpoint()?;
                let primary = media.remove(0);
                let companion = if indexer::is_raw(&primary.path) {
                    media
                        .iter()
                        .position(|f| {
                            matches!(
                                extension(&f.path).as_str(),
                                "jpg" | "jpeg" | "heif" | "heic" | "hif"
                            )
                        })
                        .map(|n| media.remove(n))
                } else {
                    None
                };
                let names: Vec<_> = std::iter::once(&primary)
                    .chain(companion.iter())
                    .map(|f| {
                        f.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase()
                    })
                    .collect();
                let attached = std::iter::once(&stem)
                    .chain(names.iter())
                    .flat_map(|key| sidecars.get(key).into_iter().flatten())
                    .filter(|file| !claimed.contains(&file.path))
                    .cloned()
                    .collect::<Vec<_>>();
                for file in &attached {
                    claimed.insert(file.path.clone());
                }
                let video = is_video(&primary.path);
                let mut photo = Photo {
                    path: primary.path.clone(),
                    filename: primary
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    folder: primary
                        .path
                        .parent()
                        .unwrap_or(&source)
                        .to_string_lossy()
                        .into_owned(),
                    size_bytes: primary.size,
                    modified_at: primary.mtime_ns / 1_000_000,
                    companion_path: companion.as_ref().map(|f| f.path.clone()),
                    companion_size_bytes: companion.as_ref().map_or(0, |f| f.size),
                    companion_modified_at: companion.as_ref().map_or(0, |f| f.mtime_ns / 1_000_000),
                    ..Default::default()
                };
                let mut make = String::new();
                let mut serial = String::new();
                let mut warning = None;
                if !video {
                    match indexer::read_exif_bounded(&primary.path) {
                        Ok(Some(exif)) => {
                            indexer::apply_exif(&mut photo, &exif);
                            let text = |tag| {
                                exif.get_field(tag, exif::In::PRIMARY)
                                    .and_then(|f| match &f.value {
                                        exif::Value::Ascii(values) => values.first().map(|s| {
                                            String::from_utf8_lossy(s)
                                                .trim_matches('\0')
                                                .trim()
                                                .to_string()
                                        }),
                                        _ => None,
                                    })
                                    .unwrap_or_default()
                            };
                            make = text(exif::Tag::Make);
                            serial = text(exif::Tag::BodySerialNumber);
                        }
                        Ok(None) => {}
                        Err(error) => warning = Some(format!("Metadata: {error:#}")),
                    }
                    if indexer::metadata_incomplete(&photo) {
                        if let Some(companion) = &companion {
                            if let Ok(Some(exif)) = indexer::read_exif_bounded(&companion.path) {
                                let mut fallback = Photo::default();
                                indexer::apply_exif(&mut fallback, &exif);
                                indexer::fill_missing_metadata(&mut photo, &fallback);
                            }
                        }
                    }
                }
                if let Some(decision) = decisions
                    .get(&primary.path)
                    .or_else(|| companion.as_ref().and_then(|f| decisions.get(&f.path)))
                {
                    photo.id = decision.id;
                    photo.rating = decision.rating;
                    photo.flag = decision.flag;
                    photo.label = decision.label;
                    photo.trashed = decision.trashed;
                }
                let capture_ms = photo.captured_at.unwrap_or(primary.mtime_ns / 1_000_000);
                let capture_kind = if photo.captured_at.is_some() {
                    photo.capture_time_kind.clone()
                } else {
                    warning = Some("Capture time unavailable; using file modification time".into());
                    "mtime".into()
                };
                if capture_kind == "camera_local" && result.clock == DeviceClock::Unknown {
                    result.clock = infer_device_clock(
                        capture_ms,
                        primary.mtime_ns / 1_000_000,
                        chrono::DateTime::from_timestamp_millis(primary.mtime_ns / 1_000_000)
                            .map_or(0, |time| {
                                time.with_timezone(&Local).offset().local_minus_utc()
                            }),
                    );
                }
                let relative_path = primary.path.strip_prefix(&source)?.to_path_buf();
                let mut group = IngestGroup {
                    id: result.groups.len(),
                    primary,
                    companion,
                    sidecars: attached,
                    kind: if video { "video" } else { "photo" }.into(),
                    capture_ms,
                    capture_kind,
                    photo,
                    make,
                    serial,
                    volume: volume.clone(),
                    relative_path,
                    content_hash: None,
                    seen: None,
                    selected: true,
                    warning,
                };
                group.seen = catalog.ingest_seen(&group)?;
                group.selected = group.seen.is_none();
                result.groups.push(group);
                if result.groups.len() - published >= 128 {
                    batch(&result.groups[published..]);
                    published = result.groups.len();
                }
            }
        }
        if published < result.groups.len() {
            batch(&result.groups[published..]);
            published = result.groups.len();
        }
    }
    // File times are host-epoch; without evidence of a UTC camera clock, render
    // them as local wall time (camera cards store local time, as RPD assumes).
    if result.clock != DeviceClock::Utc {
        for group in &mut result.groups {
            if group.capture_kind == "mtime" {
                if let Some(time) =
                    chrono::DateTime::from_timestamp_millis(group.primary.mtime_ns / 1_000_000)
                {
                    group.capture_ms +=
                        i64::from(time.with_timezone(&Local).offset().local_minus_utc()) * 1000;
                }
            }
        }
    }
    if published < result.groups.len() {
        batch(&result.groups[published..]);
    }
    // IDs are stable scan identities; sort once off the UI thread for the timeline.
    result.groups.sort_by(|a, b| {
        a.capture_ms
            .cmp(&b.capture_ms)
            .then(a.primary.path.cmp(&b.primary.path))
    });
    Ok(result)
}

/// Use a block-volume UUID when available, label as fallback; folder identity is
/// intentionally independent of the current mount prefix. Seen lookup never needs it.
pub fn volume_identity(path: &Path) -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
            let mut best = (0, String::new());
            for row in mounts.lines() {
                let fields: Vec<_> = row.split_whitespace().collect();
                if fields.len() < 2 {
                    continue;
                }
                let mount = PathBuf::from(fields[1].replace("\\040", " ").replace("\\134", "\\"));
                if path.starts_with(&mount) && mount.as_os_str().len() >= best.0 {
                    best = (mount.as_os_str().len(), fields[0].to_string());
                }
            }
            let device = fs::canonicalize(&best.1).ok();
            for (directory, prefix) in [
                ("/dev/disk/by-uuid", "uuid"),
                ("/dev/disk/by-label", "label"),
            ] {
                if let Ok(entries) = fs::read_dir(directory) {
                    for entry in entries.flatten() {
                        if device.is_some() && fs::canonicalize(entry.path()).ok() == device {
                            return format!("{prefix}:{}", entry.file_name().to_string_lossy());
                        }
                    }
                }
            }
        }
    }
    format!(
        "label:{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

#[derive(Clone, Default)]
pub struct Control(Arc<(Mutex<ControlState>, Condvar)>);
#[derive(Default)]
struct ControlState {
    paused: bool,
    cancelled: bool,
    finished: bool,
}
impl Control {
    pub fn reset(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.cancelled = false;
            state.finished = false;
            state.paused = false;
            self.0.1.notify_all();
        }
    }
    pub(crate) fn finish(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.finished = true;
        }
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.0.0.lock().is_ok_and(|state| state.finished)
    }
    pub fn pause(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.paused = true;
        }
    }
    pub fn resume(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.paused = false;
            self.0.1.notify_all();
        }
    }
    pub fn cancel(&self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.cancelled = true;
            self.0.1.notify_all();
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.0.lock().map_or(true, |s| s.cancelled)
    }
    pub fn checkpoint(&self) -> Result<()> {
        let mut state = self
            .0
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Ingest control lock poisoned"))?;
        while state.paused && !state.cancelled {
            state = self
                .0
                .1
                .wait(state)
                .map_err(|_| anyhow::anyhow!("Ingest control lock poisoned"))?;
        }
        ensure!(!state.cancelled, "Import cancelled");
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub job_id: String,
    pub phase: String,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total: usize,
    pub files: usize,
    pub photos: usize,
    pub videos: usize,
    pub bytes: u64,
    pub total_bytes: u64,
    pub mb_per_second: f64,
    pub eta_seconds: Option<u64>,
    pub destinations: Vec<String>,
    pub errors: Vec<String>,
    pub manifest: Option<PathBuf>,
    pub library_root: Option<PathBuf>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub progress: Progress,
    pub records: Vec<store::ImportRecord>,
}

pub(crate) fn source_time(file: &SourceFile) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(file.mtime_ns.max(0) as u64)
}
