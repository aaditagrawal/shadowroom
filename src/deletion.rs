//! Explicit, frozen plans for removing local photo groups from disk.
//!
//! OS Trash uses FreeDesktop storage on Linux and native services on macOS,
//! and never falls back to permanent deletion. Plans capture identities before confirmation
//! and revalidate them before making changes; directories are never removed.
use crate::model::Photo;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

#[cfg(target_os = "linux")]
mod linux_trash;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionMode {
    SystemTrash,
    Permanent,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletionFile {
    pub path: PathBuf,
    pub kind: String,
    pub photo_ids: Vec<i64>,
    #[serde(skip)]
    identity: Identity,
    #[serde(skip)]
    parent_identity: Identity,
    #[serde(skip)]
    catalog_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletionPlan {
    pub mode: DeletionMode,
    pub photo_ids: Vec<i64>,
    pub files: Vec<DeletionFile>,
    pub source_directories: Vec<PathBuf>,
    /// Ambiguous stem sidecars remain with unselected sibling media.
    pub preserved_shared_sidecars: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionStatus {
    Completed,
    Failed,
    NotAttempted,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletionFileResult {
    pub path: PathBuf,
    pub photo_ids: Vec<i64>,
    pub status: DeletionStatus,
    pub error: Option<String>,
    #[serde(skip)]
    catalog_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletionReport {
    pub mode: DeletionMode,
    pub photo_ids: Vec<i64>,
    pub files: Vec<DeletionFileResult>,
    pub error: Option<String>,
    pub catalog_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Identity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: (i64, i64),
}
impl Identity {
    fn from(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    fn same_directory(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            self == other
        }
    }
}

impl DeletionPlan {
    /// Only explicit local photo records are accepted. Related XMP/AAE and
    /// camera sidecars follow the ingest grouping rules; unrelated files stay.
    pub fn prepare(photos: &[Photo], mode: DeletionMode) -> Result<Self> {
        ensure!(!photos.is_empty(), "Select photos before deleting");
        let mut files = BTreeMap::<PathBuf, DeletionFile>::new();
        let mut photo_ids = BTreeSet::new();
        let mut directories = BTreeSet::new();
        let mut sidecars = SidecarCache::default();
        let mut media_members = Vec::new();
        let mut included_media = BTreeSet::new();
        for photo in photos {
            photo.require_local("Deleting files")?;
            ensure!(photo.id > 0, "A deletion plan requires catalog photo IDs");
            ensure!(
                photo_ids.insert(photo.id),
                "Duplicate photo ID {}",
                photo.id
            );
            for (index, path) in std::iter::once(&photo.path)
                .chain(photo.companion_path.iter())
                .enumerate()
            {
                let canonical = canonical_file_path(path)?;
                let parent = canonical
                    .parent()
                    .context("Photo has no parent directory")?;
                directories.insert(parent.to_path_buf());
                included_media.insert(canonical.clone());
                media_members.push((
                    photo.id,
                    path.clone(),
                    canonical,
                    if index == 0 { "original" } else { "companion" },
                ));
            }
        }
        for (id, catalog_path, path, kind) in media_members {
            add_file(&mut files, &path, kind, id)?;
            let file = files.get_mut(&path).expect("Just inserted photo file");
            if !file.catalog_paths.contains(&catalog_path) {
                file.catalog_paths.push(catalog_path);
            }
            for related in sidecars.related(&path, &included_media)? {
                add_file(&mut files, &related, "sidecar", id)?;
            }
        }
        // Protect the viewable original if an earlier member fails. Every file
        // is revalidated before the first move, then again at its own operation.
        let mut files: Vec<_> = files.into_values().collect();
        files.sort_by(|a, b| {
            let priority = |file: &DeletionFile| match file.kind.as_str() {
                "sidecar" => 0,
                "companion" => 1,
                _ => 2,
            };
            priority(a)
                .cmp(&priority(b))
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(Self {
            mode,
            photo_ids: photo_ids.into_iter().collect(),
            files,
            source_directories: directories.into_iter().collect(),
            preserved_shared_sidecars: sidecars.preserved.into_iter().collect(),
        })
    }
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
    pub fn photo_count(&self) -> usize {
        self.photo_ids.len()
    }
    pub fn destination_description(&self) -> &'static str {
        match self.mode {
            DeletionMode::SystemTrash => "Operating system Trash (recover using your file manager)",
            DeletionMode::Permanent => "Permanently deleted; bypasses Trash and cannot be undone",
        }
    }
    pub fn failure_report(&self, error: String) -> DeletionReport {
        DeletionReport {
            mode: self.mode,
            photo_ids: self.photo_ids.clone(),
            files: self
                .files
                .iter()
                .map(|file| DeletionFileResult {
                    path: file.path.clone(),
                    photo_ids: file.photo_ids.clone(),
                    status: DeletionStatus::NotAttempted,
                    error: None,
                    catalog_paths: file.catalog_paths.clone(),
                })
                .collect(),
            error: Some(error),
            catalog_error: None,
        }
    }
    pub fn execute(&self) -> DeletionReport {
        self.execute_with(|file, parent| match self.mode {
            DeletionMode::SystemTrash => {
                #[cfg(target_os = "linux")]
                {
                    linux_trash::trash(file, parent)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    system_trash(&file.path)
                }
            }
            DeletionMode::Permanent => remove_regular_at(parent, &file.path),
        })
    }
    fn execute_with(
        &self,
        mut remove: impl FnMut(&DeletionFile, &File) -> Result<()>,
    ) -> DeletionReport {
        let mut report = self.failure_report(String::new());
        report.error = None;
        for (index, file) in self.files.iter().enumerate() {
            if let Err(error) = validate(file) {
                let error = format!("Deletion cancelled before changing files: {error:#}");
                report.files[index].status = DeletionStatus::Failed;
                report.files[index].error = Some(error.clone());
                report.error = Some(error);
                return report;
            }
        }
        for (index, file) in self.files.iter().enumerate() {
            let outcome = validate(file).and_then(|parent| remove(file, &parent));
            match outcome {
                Ok(()) => report.files[index].status = DeletionStatus::Completed,
                Err(error) => {
                    let error = format!("Could not remove {}: {error:#}", file.path.display());
                    report.files[index].status = DeletionStatus::Failed;
                    report.files[index].error = Some(error.clone());
                    report.error = Some(error);
                    break;
                }
            }
        }
        report
    }
}
impl DeletionReport {
    pub fn removed_paths(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|file| file.status == DeletionStatus::Completed)
            .map(|file| file.path.clone())
            .collect()
    }
    pub(crate) fn removed_catalog_paths(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|file| file.status == DeletionStatus::Completed)
            .flat_map(|file| std::iter::once(&file.path).chain(file.catalog_paths.iter()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn completed_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.status == DeletionStatus::Completed)
            .count()
    }
    pub fn failed_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.status == DeletionStatus::Failed)
            .count()
    }
    pub fn is_success(&self) -> bool {
        self.error.is_none()
            && self.catalog_error.is_none()
            && self.completed_file_count() == self.files.len()
    }
}

fn canonical_file_path(path: &Path) -> Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "Photo path is empty");
    let filename = path.file_name().context("Photo path has no filename")?;
    // Resolve legitimate ancestor aliases once, before showing confirmation.
    // The final component must remain a regular file, never a followed symlink.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .with_context(|| format!("Find photo folder {}", parent.display()))?;
    let path = parent.join(filename);
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "Not a regular photo file: {}",
        path.display()
    );
    Ok(path)
}
fn add_file(
    files: &mut BTreeMap<PathBuf, DeletionFile>,
    path: &Path,
    kind: &str,
    id: i64,
) -> Result<()> {
    if let Some(file) = files.get_mut(path) {
        if !file.photo_ids.contains(&id) {
            file.photo_ids.push(id);
        }
        // A file explicitly catalogued as an original must be processed last.
        if kind == "original" {
            file.kind = kind.into();
        }
        return Ok(());
    }
    let parent = open_parent(path)?;
    let metadata = regular_metadata_at(&parent, path)?;
    let parent_identity = Identity::from(&parent.metadata()?);
    files.insert(
        path.into(),
        DeletionFile {
            path: path.into(),
            kind: kind.into(),
            photo_ids: vec![id],
            identity: Identity::from(&metadata),
            parent_identity,
            catalog_paths: Vec::new(),
        },
    );
    Ok(())
}
#[derive(Default)]
struct SidecarCache {
    directories: HashMap<PathBuf, DirectoryEntries>,
    preserved: BTreeSet<PathBuf>,
}
#[derive(Default)]
struct DirectoryEntries {
    sidecars: BTreeMap<Vec<u8>, Vec<PathBuf>>,
    media: BTreeMap<Vec<u8>, Vec<PathBuf>>,
}
impl SidecarCache {
    fn related(&mut self, media: &Path, included: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>> {
        let parent = media.parent().context("Photo has no parent directory")?;
        if !self.directories.contains_key(parent) {
            let mut entries = DirectoryEntries::default();
            for entry in fs::read_dir(parent)
                .with_context(|| format!("Read photo folder {}", parent.display()))?
            {
                let entry = entry?;
                if crate::indexer::is_photo(&entry.path()) {
                    if let Some(stem) = entry.path().file_stem() {
                        entries
                            .media
                            .entry(stem.as_encoded_bytes().to_ascii_lowercase())
                            .or_default()
                            .push(entry.path());
                    }
                }
                if entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        ["xmp", "aae", "thm", "wav", "mp3", "log", "lrv"]
                            .iter()
                            .any(|known| extension.eq_ignore_ascii_case(known))
                    })
                {
                    entries
                        .sidecars
                        .entry(entry.file_name().as_encoded_bytes().to_ascii_lowercase())
                        .or_default()
                        .push(entry.path());
                }
            }
            self.directories.insert(parent.into(), entries);
        }
        let mut names = BTreeSet::new();
        for base in [media.file_stem(), media.file_name()].into_iter().flatten() {
            let shared = Some(base) == media.file_stem()
                && self.directories[parent]
                    .media
                    .get(&base.as_encoded_bytes().to_ascii_lowercase())
                    .is_some_and(|paths| paths.iter().any(|path| !included.contains(path)));
            for extension in ["xmp", "aae", "thm", "wav", "mp3", "log", "lrv"] {
                let mut name = base.to_os_string();
                name.push(".");
                name.push(extension);
                let name = name.as_encoded_bytes().to_ascii_lowercase();
                if shared {
                    if let Some(paths) = self.directories[parent].sidecars.get(&name) {
                        self.preserved.extend(paths.iter().cloned());
                    }
                } else {
                    names.insert(name);
                }
            }
        }
        let mut related = Vec::new();
        for name in names {
            if let Some(paths) = self.directories[parent].sidecars.get(&name) {
                related.extend(paths.iter().cloned());
            }
        }
        related.sort();
        Ok(related)
    }
}
fn validate(file: &DeletionFile) -> Result<File> {
    let parent = open_parent(&file.path)?;
    ensure!(
        file.parent_identity
            .same_directory(&Identity::from(&parent.metadata()?)),
        "Photo folder changed since confirmation: {}",
        file.path.display()
    );
    let current = Identity::from(&regular_metadata_at(&parent, &file.path)?);
    ensure!(
        current == file.identity,
        "File changed since confirmation: {}",
        file.path.display()
    );
    Ok(parent)
}

#[cfg(unix)]
fn open_parent(path: &Path) -> Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    ensure!(path.is_absolute(), "Deletion paths must be absolute");
    let mut directory = File::open("/")?;
    for component in path.parent().context("File has no parent")?.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = std::ffi::CString::new(name.as_bytes())?;
                // SAFETY: directory and name stay alive; the returned descriptor
                // is immediately owned. O_NOFOLLOW rejects swapped path links.
                let descriptor = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("Open unchanged photo directory");
                }
                directory = unsafe { File::from_raw_fd(descriptor) };
            }
            _ => bail!("Deletion path contains an invalid component"),
        }
    }
    Ok(directory)
}
#[cfg(not(unix))]
fn open_parent(_path: &Path) -> Result<File> {
    bail!("File deletion is unsupported on this platform")
}

#[cfg(unix)]
fn regular_metadata_at(parent: &File, path: &Path) -> Result<fs::Metadata> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let filename =
        std::ffi::CString::new(path.file_name().context("File has no name")?.as_bytes())?;
    // SAFETY: the parent descriptor and filename are valid; opening a FIFO or
    // symlink cannot block or follow its target before checking the file type.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            filename.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Open unchanged file {}", path.display()));
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Not a regular file: {}", path.display());
    Ok(metadata)
}
#[cfg(not(unix))]
fn regular_metadata_at(_parent: &File, path: &Path) -> Result<fs::Metadata> {
    Ok(fs::symlink_metadata(path)?)
}

#[cfg(unix)]
fn remove_regular_at(parent: &File, path: &Path) -> Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let filename =
        std::ffi::CString::new(path.file_name().context("File has no name")?.as_bytes())?;
    // SAFETY: both inputs are valid. Flags=0 unlinks one file; it never traverses
    // or recursively removes a directory, even if a replacement races the UI.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Permanently delete file");
    }
    Ok(())
}
#[cfg(not(unix))]
fn remove_regular_at(_parent: &File, _path: &Path) -> Result<()> {
    bail!("Permanent deletion is unsupported on this platform")
}

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> Result<()> {
    system_trash_with_destination(path).map(|_| ())
}
#[cfg(target_os = "macos")]
fn system_trash_with_destination(path: &Path) -> Result<Option<PathBuf>> {
    use objc2_foundation::{NSFileManager, NSURL};
    // Foundation performs the move directly. Typed Objective-C ownership and
    // autoreleasing out-parameters avoid scripting-bridge lifetime problems;
    // no Finder automation permission or external interpreter is involved.
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::from_file_path(path).context("Invalid operating system Trash file URL")?;
        let mut destination = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut destination))
            .map_err(|error| {
                anyhow::anyhow!(
                    "Operating system Trash: {}; no permanent-delete fallback was used",
                    error.localizedDescription()
                )
            })?;
        // A successful move stays successful even if Foundation omits the
        // optional resulting URL; never report it as a failed/unmoved source.
        Ok(destination.and_then(|url| url.to_file_path()))
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn system_trash(_path: &Path) -> Result<()> {
    bail!("Operating system Trash is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn photo(path: &Path) -> Photo {
        Photo {
            id: 1,
            path: path.into(),
            ..Default::default()
        }
    }
    fn write(path: &Path) {
        fs::write(path, b"disposable fixture").unwrap();
    }
    #[test]
    fn permanent_deletion_includes_pair_and_case_insensitive_sidecars_only() {
        let dir = tempfile::tempdir().unwrap();
        // macOS exposes the temporary directory through /var -> /private/var.
        // Deletion plans deliberately report canonical physical paths.
        let root = dir.path().canonicalize().unwrap();
        let raw = root.join("IMG_0123.NEF");
        let jpeg = root.join("img_0123.JPG");
        let xmp = root.join("Img_0123.XMP");
        let extension_xmp = root.join("IMG_0123.NEF.xmp");
        let audio = root.join("IMG_0123.WAV");
        let unrelated = root.join("IMG_0124.xmp");
        for path in [&raw, &jpeg, &xmp, &extension_xmp, &audio, &unrelated] {
            write(path);
        }
        let mut photo = photo(&raw);
        photo.companion_path = Some(jpeg.clone());
        let plan = DeletionPlan::prepare(&[photo], DeletionMode::Permanent).unwrap();
        assert_eq!(plan.photo_count(), 1);
        assert_eq!(plan.file_count(), 5);
        assert_eq!(plan.files.last().unwrap().path, raw);
        let report = plan.execute();
        assert!(report.is_success(), "{report:?}");
        for path in [&raw, &jpeg, &xmp, &extension_xmp, &audio] {
            assert!(!path.exists());
        }
        assert_eq!(fs::read(unrelated).unwrap(), b"disposable fixture");
    }
    #[test]
    fn changed_or_missing_members_cancel_every_file_before_the_first_operation() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.nef");
        let jpeg = dir.path().join("a.jpg");
        write(&raw);
        write(&jpeg);
        let mut p = photo(&raw);
        p.companion_path = Some(jpeg.clone());
        let plan = DeletionPlan::prepare(&[p.clone()], DeletionMode::Permanent).unwrap();
        fs::rename(&jpeg, dir.path().join("previous.jpg")).unwrap();
        write(&jpeg);
        let report = plan.execute();
        assert!(!report.is_success());
        assert_eq!(report.completed_file_count(), 0);
        assert!(raw.exists());
        assert!(jpeg.exists());
        let plan = DeletionPlan::prepare(&[p], DeletionMode::Permanent).unwrap();
        fs::remove_file(&jpeg).unwrap();
        let report = plan.execute();
        assert_eq!(report.completed_file_count(), 0);
        assert!(raw.exists());
    }
    #[test]
    fn partial_failure_reports_precise_completed_paths_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let raw = root.join("a.nef");
        let jpeg = root.join("a.jpg");
        let sidecar = root.join("a.xmp");
        for path in [&raw, &jpeg, &sidecar] {
            write(path);
        }
        let mut p = photo(&raw);
        p.companion_path = Some(jpeg.clone());
        let plan = DeletionPlan::prepare(&[p], DeletionMode::SystemTrash).unwrap();
        let mut calls = 0;
        let report = plan.execute_with(|file, _| {
            calls += 1;
            if calls == 2 {
                bail!("fixture unavailable volume");
            }
            fs::remove_file(&file.path)?;
            Ok(())
        });
        assert_eq!(report.removed_paths(), vec![sidecar]);
        assert_eq!(report.failed_file_count(), 1);
        assert_eq!(report.files[2].status, DeletionStatus::NotAttempted);
        assert!(raw.exists());
        assert!(jpeg.exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_files_and_replaced_parent_directories_are_never_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let other = dir.path().join("other");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&other).unwrap();
        let a = source.join("a.jpg");
        let b = other.join("a.jpg");
        write(&a);
        write(&b);
        let plan = DeletionPlan::prepare(&[photo(&a)], DeletionMode::Permanent).unwrap();
        fs::rename(&source, dir.path().join("old-source")).unwrap();
        symlink(&other, &source).unwrap();
        let report = plan.execute();
        assert_eq!(report.completed_file_count(), 0);
        assert!(b.exists());
        assert!(
            DeletionPlan::prepare(
                &[photo(&dir.path().join("old-source/a.jpg"))],
                DeletionMode::Permanent
            )
            .is_ok()
        );
        assert!(dir.path().join("old-source/a.jpg").exists());
        assert!(b.exists());
    }
    #[cfg(unix)]
    #[test]
    fn final_symlink_and_non_regular_sidecar_are_rejected_at_planning() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.jpg");
        write(&target);
        let link = dir.path().join("link.jpg");
        symlink(&target, &link).unwrap();
        assert!(DeletionPlan::prepare(&[photo(&link)], DeletionMode::Permanent).is_err());
        fs::create_dir(dir.path().join("target.xmp")).unwrap();
        assert!(DeletionPlan::prepare(&[photo(&target)], DeletionMode::Permanent).is_err());
        assert!(target.exists());
    }
    #[test]
    fn shared_stem_sidecars_stay_with_unselected_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let raw = root.join("a.nef");
        let other = root.join("a.cr3");
        let shared = root.join("a.XMP");
        let specific = root.join("a.NEF.xmp");
        for path in [&raw, &other, &shared, &specific] {
            write(path);
        }
        let plan = DeletionPlan::prepare(&[photo(&raw)], DeletionMode::Permanent).unwrap();
        assert_eq!(plan.file_count(), 2);
        assert_eq!(plan.preserved_shared_sidecars, vec![shared.clone()]);
        assert!(plan.execute().is_success());
        assert!(other.exists());
        assert!(shared.exists());
        assert!(!specific.exists());
    }
    #[test]
    fn selecting_every_sibling_removes_one_deduplicated_shared_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let raw = root.join("a.nef");
        let other = root.join("a.cr3");
        let shared = root.join("a.xmp");
        for path in [&raw, &other, &shared] {
            write(path);
        }
        let a = photo(&raw);
        let mut b = photo(&other);
        b.id = 2;
        let plan = DeletionPlan::prepare(&[a, b], DeletionMode::Permanent).unwrap();
        assert!(plan.preserved_shared_sidecars.is_empty());
        assert_eq!(plan.file_count(), 3);
        assert_eq!(
            plan.files
                .iter()
                .find(|file| file.path == shared)
                .unwrap()
                .photo_ids,
            vec![1, 2]
        );
        assert!(plan.execute().is_success());
        assert!(!shared.exists());
    }
    #[cfg(unix)]
    #[test]
    fn legacy_catalog_paths_through_directory_aliases_reconcile_after_canonical_deletion() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let real = root.join("real");
        let alias = root.join("alias");
        fs::create_dir(&real).unwrap();
        symlink(&real, &alias).unwrap();
        let path = alias.join("a.jpg");
        write(&path);
        let catalog = crate::catalog::Catalog::open(&root.join("catalog.sqlite3")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: path.clone(),
                ..Default::default()
            }])
            .unwrap();
        let plan =
            DeletionPlan::prepare(&catalog.photos().unwrap(), DeletionMode::Permanent).unwrap();
        assert_eq!(plan.files[0].path, real.join("a.jpg"));
        let report = plan.execute();
        assert!(report.is_success());
        assert_eq!(report.removed_paths(), vec![real.join("a.jpg")]);
        catalog
            .reconcile_deleted_files(&report.removed_catalog_paths())
            .unwrap();
        assert!(catalog.photos().unwrap().is_empty());
    }
    #[test]
    fn remote_photos_and_duplicate_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        write(&path);
        let p = photo(&path);
        assert!(DeletionPlan::prepare(&[p.clone(), p], DeletionMode::Permanent).is_err());
        let mut p = photo(&path);
        p.source = crate::model::PhotoSource::Remote(crate::model::RemoteSource {
            device_id: "fixture".into(),
            root_id: "r".into(),
            volume_id: "v".into(),
            relative_path: "a.jpg".into(),
            size_bytes: 1,
            modified_at: 0,
            companion_path: None,
            companion_size_bytes: 0,
            companion_modified_at: 0,
            available: true,
        });
        assert!(DeletionPlan::prepare(&[p], DeletionMode::Permanent).is_err());
        assert!(path.exists());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn system_trash_preserves_two_files_and_restore_metadata_in_isolated_xdg_home() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "deletion::tests::isolated_system_trash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("SHADOWROOM_TEST_TRASH_ROOT", &root)
            .env("HOME", root.join("home"))
            .env("XDG_DATA_HOME", root.join("data"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native trash child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let trashed = root.join("data/Trash");
        assert_eq!(
            fs::read(trashed.join("files/a.nef")).unwrap(),
            b"disposable fixture"
        );
        assert_eq!(
            fs::read(trashed.join("files/a.jpg")).unwrap(),
            b"disposable fixture"
        );
        for name in ["a.nef", "a.jpg"] {
            let info = fs::read_to_string(trashed.join(format!("info/{name}.trashinfo"))).unwrap();
            assert!(info.starts_with("[Trash Info]\n"));
            assert!(
                info.contains(&format!("Path={}/{}", root.display(), name)),
                "{info}"
            );
            assert!(info.contains("DeletionDate="));
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Subprocess helper: parent test provides an isolated HOME and XDG_DATA_HOME"]
    fn isolated_system_trash_child() {
        let root = PathBuf::from(
            std::env::var_os("SHADOWROOM_TEST_TRASH_ROOT")
                .expect("Must be run by isolated trash parent test"),
        );
        assert_eq!(
            PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()),
            root.join("data")
        );
        let raw = root.join("a.nef");
        let jpeg = root.join("a.jpg");
        write(&raw);
        write(&jpeg);
        let mut p = photo(&raw);
        p.companion_path = Some(jpeg);
        let plan = DeletionPlan::prepare(&[p], DeletionMode::SystemTrash).unwrap();
        let report = plan.execute();
        assert!(report.is_success(), "{report:?}");
        assert_eq!(report.completed_file_count(), 2);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn system_trash_moves_two_files_and_restores_the_verified_native_destinations() {
        use objc2_foundation::{NSFileManager, NSURL};
        struct FixtureCleanup {
            source: PathBuf,
            destination: PathBuf,
            expected: Vec<u8>,
        }
        impl Drop for FixtureCleanup {
            fn drop(&mut self) {
                // Never leave a successful fixture move in the user's Trash
                // after a later assertion fails. Only restore our exact bytes,
                // and never overwrite another file at the source pathname.
                if fs::read(&self.destination).ok().as_deref() == Some(self.expected.as_slice())
                    && !self.source.exists()
                {
                    if fs::hard_link(&self.destination, &self.source).is_ok() {
                        let _ = fs::remove_file(&self.destination);
                    }
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let unique = root.file_name().unwrap().to_string_lossy();
        for index in 0..2 {
            let source = root.join(format!(
                "shadowroom-trash-fixture-{unique}-{index} space ' café.jpg"
            ));
            let payload =
                format!("Disposable Shadowroom native Trash fixture {index}\n").into_bytes();
            fs::write(&source, &payload).unwrap();
            let destination = system_trash_with_destination(&source)
                .expect("Native Foundation Trash must succeed")
                .expect("Foundation should return the resulting URL when requested");
            let _cleanup = FixtureCleanup {
                source: source.clone(),
                destination: destination.clone(),
                expected: payload.clone(),
            };
            assert!(!source.exists());
            assert!(destination.is_absolute());
            assert_eq!(fs::read(&destination).unwrap(), payload);
            objc2::rc::autoreleasepool(|_| {
                let origin = NSURL::from_file_path(&destination).unwrap();
                let target = NSURL::from_file_path(&source).unwrap();
                NSFileManager::defaultManager()
                    .moveItemAtURL_toURL_error(&origin, &target)
                    .unwrap_or_else(|error| {
                        panic!("Restore verified fixture: {}", error.localizedDescription())
                    });
            });
            assert!(!destination.exists());
            assert_eq!(fs::read(&source).unwrap(), payload);
        }
        let missing = root.join("missing-shadowroom-trash-fixture.jpg");
        assert!(system_trash_with_destination(&missing).is_err());
        assert!(!missing.exists());
    }
}
