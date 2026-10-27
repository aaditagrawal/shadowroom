use super::{
    store::{FileRecord, ImportRecord},
    *,
};
use crate::catalog::verify_file;
use anyhow::{bail, ensure};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    time::Instant,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FsCapacity {
    pub filesystem: String,
    pub available: u64,
}
pub trait FsStats: Send + Sync {
    fn capacity(&self, path: &Path) -> Result<FsCapacity>;
}
pub struct NativeFsStats;
impl FsStats for NativeFsStats {
    fn capacity(&self, path: &Path) -> Result<FsCapacity> {
        let ancestor = existing_ancestor(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
            let name = std::ffi::CString::new(ancestor.as_os_str().as_bytes())?;
            let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // SAFETY: CString is terminated and statvfs writes its initialized result.
            if unsafe { libc::statvfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error()).context("Read destination free space");
            }
            let stats = unsafe { stats.assume_init() };
            Ok(FsCapacity {
                filesystem: fs::metadata(ancestor)?.dev().to_string(),
                available: (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = ancestor;
            bail!("Free-space checks are unsupported on this platform")
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceProjection {
    pub root: PathBuf,
    pub filesystem: String,
    pub required: u64,
    pub filesystem_required: u64,
    pub available: u64,
    pub remaining: u64,
    pub sufficient: bool,
}
pub fn space(plan: &IngestPlan, stats: &dyn FsStats) -> Result<Vec<SpaceProjection>> {
    let roots = std::iter::once(&plan.options.library_root).chain(plan.options.backup_roots.iter());
    let mut projections = Vec::new();
    let mut totals = HashMap::<String, u64>::new();
    for root in roots {
        let capacity = stats.capacity(root)?;
        let total = totals.entry(capacity.filesystem.clone()).or_default();
        *total = total
            .checked_add(plan.bytes)
            .context("Space calculation overflow")?;
        projections.push(SpaceProjection {
            root: root.clone(),
            filesystem: capacity.filesystem,
            required: plan.bytes,
            filesystem_required: 0,
            available: capacity.available,
            remaining: 0,
            sufficient: false,
        });
    }
    for item in &mut projections {
        item.filesystem_required = totals[&item.filesystem];
        // Reserve a small manifest/SQLite margin as well as the media bytes.
        let needed = item.filesystem_required.saturating_add(1024 * 1024);
        item.remaining = item.available.saturating_sub(needed);
        item.sufficient = item.available >= needed;
    }
    Ok(projections)
}
pub fn preflight(plan: &IngestPlan, stats: &dyn FsStats) -> Result<Vec<SpaceProjection>> {
    validate_roots(plan)?;
    let space = space(plan, stats)?;
    for item in &space {
        ensure!(
            item.sufficient,
            "Insufficient free space at {}: need {} bytes on filesystem {}, available {}",
            item.root.display(),
            item.filesystem_required.saturating_add(1024 * 1024),
            item.filesystem,
            item.available
        );
    }
    Ok(space)
}
fn existing_ancestor(path: &Path) -> Result<&Path> {
    let mut ancestor = path;
    while !ancestor.try_exists()? {
        ancestor = ancestor
            .parent()
            .context("Destination has no existing parent")?;
    }
    Ok(ancestor)
}
fn resolve_future(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "Destination must be absolute");
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Destination cannot contain .."
    );
    let ancestor = existing_ancestor(path)?;
    Ok(ancestor.canonicalize()?.join(path.strip_prefix(ancestor)?))
}
fn validate_roots(plan: &IngestPlan) -> Result<Vec<PathBuf>> {
    let source = plan.source.canonicalize()?;
    let mut roots = Vec::new();
    for root in std::iter::once(&plan.options.library_root).chain(plan.options.backup_roots.iter())
    {
        let resolved = resolve_future(root)?;
        ensure!(
            !resolved.starts_with(&source) && !source.starts_with(&resolved),
            "Import destination and source must be separate directory trees"
        );
        ensure!(
            !roots
                .iter()
                .any(|previous: &PathBuf| resolved.starts_with(previous)
                    || previous.starts_with(&resolved)),
            "Primary and backup destinations must be separate directory trees"
        );
        roots.push(resolved);
    }
    for group in &plan.groups {
        for file in &group.files {
            ensure!(
                file.relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
                    && !file.relative.as_os_str().is_empty(),
                "Invalid planned relative filename"
            );
            ensure!(
                file.source.path.starts_with(&source),
                "Source outside scan root"
            );
        }
    }
    Ok(roots)
}
fn safe_parent(root: &Path, relative: &Path) -> Result<PathBuf> {
    let directory = root.join(relative.parent().unwrap_or(Path::new("")));
    // Do not follow a pre-existing symlink beneath an approved destination root.
    let mut current = root.to_path_buf();
    for part in relative.parent().unwrap_or(Path::new("")).components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Destination folder is not a regular directory: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                File::open(current.parent().context("Destination parent")?)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        directory.canonicalize()?.starts_with(root.canonicalize()?),
        "Destination folder escaped its root"
    );
    Ok(directory)
}

/// Injectable writer used to exercise corruption, disk-full and interruption paths.
/// The production writer performs one bounded write per destination per chunk.
pub trait CopyHooks: Send + Sync {
    fn write_chunk(&self, _path: &Path, output: &mut File, bytes: &[u8]) -> Result<()> {
        output.write_all(bytes)?;
        Ok(())
    }
    /// Called only after the group transaction committed, before staging cleanup.
    fn before_committed_cleanup(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    /// Called after exclusive manifest creation/sync, before reread verification.
    fn before_manifest_verify(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}
pub struct DefaultHooks;
impl CopyHooks for DefaultHooks {}

#[derive(Clone)]
struct OwnedPath {
    path: PathBuf,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}
impl OwnedPath {
    fn new(path: PathBuf) -> Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        Ok(Self::from_metadata(path, &metadata))
    }
    fn from_metadata(path: PathBuf, metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                path,
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Self { path }
        }
    }
    fn verify_identity(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Owned artifact is not a regular file: {}",
            self.path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.dev() == self.dev && metadata.ino() == self.ino,
                "Owned artifact was replaced: {}",
                self.path.display()
            );
        }
        Ok(())
    }
    fn remove(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.dev() == self.dev && metadata.ino() == self.ino,
                "Cleanup refused replaced path: {}",
                self.path.display()
            );
        }
        #[cfg(not(unix))]
        let _ = metadata;
        fs::remove_file(&self.path)?;
        File::open(self.path.parent().context("Cleanup directory")?)?.sync_all()?;
        Ok(())
    }
}
#[derive(Default)]
struct Rollback {
    paths: Vec<OwnedPath>,
    committed: bool,
}
impl Rollback {
    fn cleanup(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for path in self.paths.iter().rev() {
            if let Err(error) = path.remove() {
                errors.push(format!("{error:#}"));
            }
        }
        self.paths.clear();
        ensure!(
            errors.is_empty(),
            "Group cleanup failed: {}",
            errors.join("; ")
        );
        Ok(())
    }
}
impl Drop for Rollback {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.cleanup();
        }
    }
}
struct Staged {
    temps: Vec<PathBuf>,
    size: u64,
    hash: blake3::Hash,
    source_metadata: fs::Metadata,
}

fn checked_source(source: &SourceFile) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(&source.path)?;
    ensure!(
        metadata.is_file()
            && metadata.len() == source.size
            && modified_ns(&metadata) == source.mtime_ns
            && source.path.canonicalize()? == source.path,
        "Source changed since scan: {}",
        source.path.display()
    );
    Ok(metadata)
}

fn same_source(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if !after.is_file()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn stage(
    file: &PlannedFile,
    roots: &[PathBuf],
    job: &str,
    index: usize,
    rollback: &mut Rollback,
    control: &Control,
    hooks: &dyn CopyHooks,
    mut update: impl FnMut(u64, &str),
) -> Result<Staged> {
    control.checkpoint()?;
    let scanned = checked_source(&file.source)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Reject a symlink swap; NONBLOCK also avoids hanging on a substituted FIFO.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = options
        .open(&file.source.path)
        .with_context(|| format!("Read {}", file.source.path.display()))?;
    let before = input.metadata()?;
    ensure!(
        same_source(&scanned, &before),
        "Source changed since scan: {}",
        file.source.path.display()
    );
    let mut outputs = Vec::new();
    let mut temps = Vec::new();
    for (n, root) in roots.iter().enumerate() {
        let directory = safe_parent(root, &file.relative)?;
        let name = file
            .relative
            .file_name()
            .context("Destination filename")?
            .to_string_lossy();
        let temp = directory.join(format!(".{name}.shadowroom-part-{job}-{index}-{n}"));
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("Create ingest staging file {}", temp.display()))?;
        rollback.paths.push(OwnedPath::new(temp.clone())?);
        outputs.push(output);
        temps.push(temp);
    }
    let mut hasher = blake3::Hasher::new();
    let mut size = 0;
    // One sequential source reader fans out Arc-backed 1 MiB chunks to independent
    // writers. Each channel holds at most four chunks, including large video files.
    std::thread::scope(|scope| -> Result<()> {
        let mut senders = Vec::new();
        let mut writers = Vec::new();
        for (mut output, path) in outputs.into_iter().zip(temps.iter().cloned()) {
            let (tx, rx) = crossbeam_channel::bounded::<Arc<Vec<u8>>>(4);
            senders.push(tx);
            let source_time = source_time(&file.source);
            writers.push(scope.spawn(move || -> Result<()> {
                while let Ok(bytes) = rx.recv() {
                    control.checkpoint()?;
                    hooks.write_chunk(&path, &mut output, &bytes)?;
                }
                output.set_times(fs::FileTimes::new().set_modified(source_time))?;
                output.sync_all()?;
                Ok(())
            }));
        }
        let read_result = (|| -> Result<()> {
            loop {
                control.checkpoint()?;
                let remaining = before.len().saturating_sub(size);
                let length = remaining.saturating_add(1).min(1024 * 1024) as usize;
                let mut bytes = vec![0; length];
                let read = input.read(&mut bytes)?;
                if read == 0 {
                    break;
                }
                ensure!(read as u64 <= remaining, "Source grew while copying");
                bytes.truncate(read);
                hasher.update(&bytes);
                size += read as u64;
                let bytes = Arc::new(bytes);
                for sender in &senders {
                    let mut pending = bytes.clone();
                    loop {
                        control.checkpoint()?;
                        match sender.send_timeout(pending, Duration::from_millis(50)) {
                            Ok(()) => break,
                            Err(crossbeam_channel::SendTimeoutError::Timeout(bytes)) => {
                                pending = bytes
                            }
                            Err(_) => bail!("Destination writer failed"),
                        }
                    }
                }
                update(read as u64, "copying");
            }
            let after = input.metadata()?;
            ensure!(
                size == before.len()
                    && before.len() == after.len()
                    && before.modified()? == after.modified()?,
                "Source changed while copying"
            );
            Ok(())
        })();
        drop(senders);
        // Always join writers, including on read failure; cleanup cannot race writes.
        let mut writer_error = None;
        for writer in writers {
            if let Err(error) = writer
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Ingest writer panicked")))
            {
                writer_error = Some(error);
            }
        }
        if let Some(error) = writer_error {
            return Err(error);
        }
        read_result?;
        Ok(())
    })?;
    // Writers can outlive the reader. Recheck the path after joining them, so a
    // source replaced or edited by then cannot be committed as the scanned file.
    ensure!(
        same_source(&before, &checked_source(&file.source)?),
        "Source changed while copying: {}",
        file.source.path.display()
    );
    let hash = hasher.finalize();
    update(0, "verifying");
    for temp in &temps {
        verify_interruptible(temp, size, &hash, control)?;
    }
    Ok(Staged {
        temps,
        size,
        hash,
        source_metadata: before,
    })
}
fn verify_interruptible(
    path: &Path,
    size: u64,
    expected: &blake3::Hash,
    control: &Control,
) -> Result<()> {
    let mut file = crate::fsutil::open_regular(path)?.take(size.saturating_add(1));
    let mut hash = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        control.checkpoint()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hash.update(&buffer[..n]);
    }
    ensure!(
        total == size && &hash.finalize() == expected,
        "Ingest verification failed: {}",
        path.display()
    );
    Ok(())
}
/// Atomic publication with a kernel-enforced no-replace operation. Unlike plain
/// rename, this works on library/backup filesystems without hard-link support.
fn publish_noreplace(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        #[cfg(target_os = "linux")]
        {
            // Call the kernel directly: glibc's renameat2 wrapper requires 2.28,
            // while the syscall works on older libc builds and Linux >= 3.15.
            // SAFETY: both paths are live, nul-terminated strings; flags prohibit replacement.
            if unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    libc::AT_FDCWD,
                    source_c.as_ptr(),
                    libc::AT_FDCWD,
                    target_c.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } == 0
            {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
            ) {
                return Err(error);
            }
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: valid C paths, RENAME_EXCL prevents replacing any existing entry.
            if unsafe { libc::renamex_np(source_c.as_ptr(), target_c.as_ptr(), libc::RENAME_EXCL) }
                == 0
            {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) {
                return Err(error);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (source_c, target_c);
        }
    }
    // Portable no-replace fallback. The caller owns and removes the staging link.
    fs::hard_link(source, target)
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn no_replace_publication_preserves_existing_files_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("staged-photo");
        let target = directory.path().join("existing-photo");
        fs::write(&source, b"imported photo").unwrap();
        fs::write(&target, b"existing photo").unwrap();
        assert_eq!(
            publish_noreplace(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&source).unwrap(), b"imported photo");
        assert_eq!(fs::read(&target).unwrap(), b"existing photo");
        #[cfg(unix)]
        {
            let link = directory.path().join("existing-link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(
                publish_noreplace(&source, &link).unwrap_err().kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(fs::read(&target).unwrap(), b"existing photo");
        }
        let fresh = directory.path().join("fresh-photo");
        publish_noreplace(&source, &fresh).unwrap();
        assert_eq!(fs::read(&fresh).unwrap(), b"imported photo");
    }
}

struct CommittedGroup {
    record: ImportRecord,
    cleanup_errors: Vec<String>,
}

fn group_copy(
    group: &PlannedGroup,
    options: &IngestOptions,
    roots: &[PathBuf],
    job: &str,
    catalog: &Catalog,
    control: &Control,
    hooks: &dyn CopyHooks,
    mut update: impl FnMut(u64, &str),
) -> Result<Option<CommittedGroup>> {
    let mut rollback = Rollback::default();
    let outcome = (|| -> Result<Option<CommittedGroup>> {
        let mut staged = Vec::new();
        for (n, file) in group.files.iter().enumerate() {
            staged.push(stage(
                file,
                roots,
                job,
                n,
                &mut rollback,
                control,
                hooks,
                &mut update,
            )?);
        }
        let mut files = group.files.clone();
        let mut suffix = 0;
        loop {
            control.checkpoint()?;
            let mut collision = false;
            for (file, staged) in files.iter().zip(&staged) {
                for root in roots {
                    let target = root.join(&file.relative);
                    match fs::symlink_metadata(&target) {
                        Ok(metadata) => {
                            if !metadata.is_file()
                                || metadata.file_type().is_symlink()
                                || verify_interruptible(&target, staged.size, &staged.hash, control)
                                    .is_err()
                            {
                                collision = true;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            if !collision {
                break;
            }
            if options.collision == "skip" {
                return Ok(None);
            }
            suffix += 1;
            ensure!(suffix <= 100_000, "Too many ingest name collisions");
            files = suffixed(&group.files, &group.group, suffix);
        }
        let mut records = Vec::new();
        for (file, staged) in files.iter().zip(&staged) {
            let mut destinations = Vec::new();
            let mut existed = Vec::new();
            for (root, temp) in roots.iter().zip(&staged.temps) {
                control.checkpoint()?;
                let target = root.join(&file.relative);
                let existing = match publish_noreplace(temp, &target) {
                    Ok(()) => {
                        rollback.paths.push(OwnedPath::new(target.clone())?);
                        false
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // This check is repeated after publication to close the exists/
                        // rename race. publication never replaces any directory entry.
                        ensure!(
                            !fs::symlink_metadata(&target)?.file_type().is_symlink(),
                            "Collision appeared while publishing import"
                        );
                        verify_interruptible(&target, staged.size, &staged.hash, control)
                            .context("Destination changed while publishing group")?;
                        true
                    }
                    Err(error) => {
                        return Err(error).context("Publish ingest file without replacement");
                    }
                };
                verify_interruptible(&target, staged.size, &staged.hash, control)?;
                File::open(target.parent().context("Destination parent")?)?.sync_all()?;
                destinations.push(target);
                existed.push(existing);
            }
            records.push(FileRecord {
                source: file.source.path.clone(),
                relative: file.relative.clone(),
                size: staged.size,
                mtime_ns: file.source.mtime_ns,
                blake3: staged.hash.to_hex().to_string(),
                destinations,
                existed,
            });
        }
        let mut record = ImportRecord::marked(&group.group);
        record.marked = false;
        record.job_id = job.into();
        record.imported_at = chrono::Utc::now().timestamp_millis();
        record.content_hash = records.first().map(|f| f.blake3.clone());
        record.destinations = records
            .iter()
            .filter_map(|f| f.destinations.first())
            .cloned()
            .collect();
        record.backups = records
            .iter()
            .flat_map(|f| f.destinations.iter().skip(1))
            .cloned()
            .collect();
        record.files = records;
        control.checkpoint()?;
        // A sidecar may take much longer than its primary. Check every member
        // again at the group commit point, including the earlier staged files.
        for (file, staged) in files.iter().zip(&staged) {
            ensure!(
                same_source(&staged.source_metadata, &checked_source(&file.source)?),
                "Source changed before group commit: {}",
                file.source.path.display()
            );
        }
        let creates_files = record
            .files
            .iter()
            .any(|f| f.existed.iter().any(|exists| !exists));
        catalog.ingest_commit(
            &record,
            Some(&group.group),
            creates_files.then_some(group.day.as_str()),
        )?;
        // The DB commit is the group commit point; cancellation after it keeps the group.
        rollback.committed = true;
        let mut cleanup_errors = Vec::new();
        for staged in &staged {
            for temp in &staged.temps {
                // Retain the identity captured on exclusive creation. Re-snapshotting
                // here could adopt and remove a staging path replaced by someone else.
                let owned = rollback.paths.iter().find(|owned| owned.path == *temp);
                let cleanup = hooks
                    .before_committed_cleanup(temp)
                    .and_then(|()| owned.context("Missing staging ownership record")?.remove());
                if let Err(error) = cleanup {
                    cleanup_errors.push(format!(
                        "Committed staging cleanup {}: {error:#}",
                        temp.display()
                    ));
                }
            }
        }
        Ok(Some(CommittedGroup {
            record,
            cleanup_errors,
        }))
    })();
    if !rollback.committed {
        if let Err(cleanup) = rollback.cleanup() {
            return Err(anyhow::anyhow!(
                "{}; {cleanup:#}",
                outcome
                    .as_ref()
                    .err()
                    .map_or_else(|| "Group skipped".into(), |e| format!("{e:#}"))
            ));
        }
    }
    outcome
}

pub fn execute(
    plan: &IngestPlan,
    catalog: &Catalog,
    control: &Control,
    progress: impl FnMut(&Progress),
) -> Result<Report> {
    execute_with(
        plan,
        catalog,
        control,
        &NativeFsStats,
        &DefaultHooks,
        progress,
    )
}
pub fn execute_with(
    plan: &IngestPlan,
    catalog: &Catalog,
    control: &Control,
    stats: &dyn FsStats,
    hooks: &dyn CopyHooks,
    mut update: impl FnMut(&Progress),
) -> Result<Report> {
    // Serialize jobs sharing a catalog without holding the SQLite connection lock.
    let _lease = catalog
        .ingest_lock
        .lock()
        .map_err(|_| anyhow::anyhow!("Ingest job lock poisoned"))?;
    preflight(plan, stats)?;
    let roots = validate_roots(plan)?;
    let job = format!(
        "{}-{:016x}",
        Utc::now().timestamp_millis(),
        rand::random::<u64>()
    );
    let mut report = Report {
        progress: Progress {
            job_id: job.clone(),
            phase: "copying".into(),
            total: plan.groups.len(),
            total_bytes: plan.bytes,
            library_root: Some(roots[0].clone()),
            destinations: roots
                .iter()
                .map(|p| format!("{}: pending verification", p.display()))
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    };
    update(&report.progress);
    catalog.ingest_begin(&job, plan)?;
    let run = (|| -> Result<()> {
        for root in &roots {
            fs::create_dir_all(root)?;
            File::open(root)?.sync_all()?;
            if let Some(parent) = root.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        let started = Instant::now();
        let mut last_update = Instant::now() - Duration::from_secs(1);
        let mut transferred = 0u64;
        let mut session = 0;
        let mut state = catalog.ingest_sequences()?;
        for planned in &plan.groups {
            if control.is_cancelled() {
                break;
            }
            control.checkpoint()?;
            if let Some(record) = catalog.ingest_previous(&planned.group)? {
                let same_roots = record.files.len() == planned.files.len()
                    && !record.files.is_empty()
                    && record.files.iter().all(|f| {
                        f.destinations.len() == roots.len()
                            && f.destinations
                                .iter()
                                .zip(&roots)
                                .all(|(path, root)| path.starts_with(root))
                    });
                let fingerprints_match = planned.group.files().zip(&record.files).all(|(s, r)| {
                    s.path.file_name() == r.source.file_name()
                        && s.size == r.size
                        && s.mtime_ns == r.mtime_ns
                });
                let source_unchanged =
                    fingerprints_match && planned.group.files().all(|f| checked_source(f).is_ok());
                if same_roots
                    && source_unchanged
                    && planned.group.files().zip(&record.files).all(|(source, f)| {
                        blake3::Hash::from_hex(&f.blake3).ok().is_some_and(|hash| {
                            // Metadata is only a history hint. A source may have been
                            // edited while preserving size and timestamp since import.
                            verify_interruptible(&source.path, f.size, &hash, control).is_ok()
                                && f.destinations.iter().all(|path| {
                                    fs::symlink_metadata(path)
                                        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
                                        && verify_interruptible(path, f.size, &hash, control)
                                            .is_ok()
                                })
                        })
                    })
                {
                    report.progress.skipped += 1;
                    update(&report.progress);
                    continue;
                }
            }
            // Recompute only if earlier failures left sequence values unconsumed or
            // the catalog changed after preview. Keep in-plan collision suffixes
            // when counters agree with the preview.
            let named = if state.stored == plan.sequences.stored + planned.session
                && session == planned.session
            {
                planned.clone()
            } else {
                name_group(
                    &planned.group,
                    &plan.options,
                    &state,
                    session,
                    planned.letter,
                )?
            };
            let result = group_copy(
                &named,
                &plan.options,
                &roots,
                &job,
                catalog,
                control,
                hooks,
                |bytes, phase| {
                    transferred += bytes;
                    report.progress.phase = phase.into();
                    let elapsed = started.elapsed().as_secs_f64().max(0.001);
                    report.progress.mb_per_second = transferred as f64 / elapsed / 1_000_000.;
                    report.progress.eta_seconds = if transferred > 0 {
                        Some(
                            ((plan.bytes.saturating_sub(transferred)) as f64
                                / (transferred as f64 / elapsed))
                                as u64,
                        )
                    } else {
                        None
                    };
                    if last_update.elapsed() >= Duration::from_millis(100) {
                        update(&report.progress);
                        last_update = Instant::now();
                    }
                },
            );
            match result {
                Ok(Some(committed)) => {
                    let CommittedGroup {
                        record,
                        cleanup_errors,
                    } = committed;
                    report.progress.errors.extend(cleanup_errors);
                    let creates_files = record
                        .files
                        .iter()
                        .any(|f| f.existed.iter().any(|exists| !exists));
                    if creates_files {
                        report.progress.completed += 1;
                    } else {
                        report.progress.skipped += 1;
                    }
                    report.progress.files += record.files.len();
                    report.progress.bytes += named.group.bytes();
                    if named.group.kind == "video" {
                        report.progress.videos += 1;
                    } else {
                        report.progress.photos += 1;
                    }
                    if creates_files {
                        state.stored += 1;
                        *state.days.entry(named.day).or_default() += 1;
                        session += 1;
                    }
                    report.records.push(record);
                }
                Ok(None) => report.progress.skipped += 1,
                Err(error) => {
                    if control.is_cancelled() {
                        break;
                    }
                    report.progress.failed += 1;
                    report
                        .progress
                        .errors
                        .push(format!("{}: {error:#}", named.group.primary.path.display()));
                }
            }
            update(&report.progress);
        }
        Ok(())
    })();
    if let Err(error) = run {
        if !control.is_cancelled() {
            report.progress.errors.push(format!("{error:#}"));
        }
    }
    report.progress.phase = if control.is_cancelled() {
        "cancelled"
    } else if report.progress.errors.is_empty() {
        "completed"
    } else {
        "failed"
    }
    .into();
    report.progress.destinations = roots
        .iter()
        .map(|p| {
            format!(
                "{}: {} verified groups",
                p.display(),
                report.progress.completed
            )
        })
        .collect();
    finalize_report(&mut report, &roots[0], catalog, hooks);
    update(&report.progress);
    Ok(report)
}

/// History stays non-successful until the proposed outcome artifact verifies.
/// Only this job's original artifact identity can be removed for a failed outcome;
/// every creation (including replacement) is exclusive and fully verified.
fn finalize_report(report: &mut Report, root: &Path, catalog: &Catalog, hooks: &dyn CopyHooks) {
    let manifest = root.join(format!("shadowroom-ingest-{}.json", report.progress.job_id));
    report.progress.manifest = Some(manifest.clone());
    let mut pending = report.progress.clone();
    pending.phase = "finalizing".into();
    if let Err(error) = catalog.ingest_finish(&pending) {
        report.progress.phase = "failed".into();
        report
            .progress
            .errors
            .push(format!("Prepare ingest finalization: {error:#}"));
    }
    let artifact = match write_manifest(report, &manifest, hooks) {
        Ok(owned) => Some(owned),
        Err(error) => {
            report.progress.phase = "failed".into();
            report
                .progress
                .errors
                .push(format!("Manifest {}: {error:#}", manifest.display()));
            None
        }
    };
    // A completed history write is possible only when the outcome artifact above
    // verified. Failed candidates instead persist a failed outcome, never success.
    let history_failed = if let Err(error) = catalog.ingest_finish(&report.progress) {
        report.progress.phase = "failed".into();
        report
            .progress
            .errors
            .push(format!("Finalize ingest job: {error:#}"));
        if let Err(history) = catalog.ingest_finish(&report.progress) {
            report
                .progress
                .errors
                .push(format!("Record failed ingest outcome: {history:#}"));
        }
        true
    } else {
        false
    };
    if artifact.is_some() && !history_failed {
        return;
    }
    // The first artifact is either absent or describes a history write that
    // subsequently failed. Publish the complete failed outcome once, with no
    // unbounded alternation between filesystem and SQLite retries.
    if let Some(owned) = artifact {
        if let Err(cleanup) = owned.remove() {
            report.progress.manifest = None;
            report.progress.errors.push(format!(
                "Manifest cleanup {}: {cleanup:#}",
                manifest.display()
            ));
            persist_artifact_failure(report, catalog);
            return;
        }
    }
    if let Err(error) = write_manifest(report, &manifest, hooks) {
        report.progress.manifest = None;
        report.progress.errors.push(format!(
            "Final failed manifest {}: {error:#}",
            manifest.display()
        ));
        persist_artifact_failure(report, catalog);
    }
}

fn persist_artifact_failure(report: &mut Report, catalog: &Catalog) {
    if let Err(history) = catalog.ingest_finish(&report.progress) {
        report.progress.errors.push(format!(
            "Record manifest failure in ingest history: {history:#}"
        ));
    }
}

fn write_manifest(report: &Report, path: &Path, hooks: &dyn CopyHooks) -> Result<OwnedPath> {
    let bytes = serde_json::to_vec_pretty(report)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    // Use the exclusively opened file's identity, never adopt a new path lookup.
    let owned = OwnedPath::from_metadata(path.to_path_buf(), &output.metadata()?);
    let written = (|| -> Result<()> {
        output.write_all(&bytes)?;
        output.sync_all()?;
        hooks.before_manifest_verify(path)?;
        Ok(())
    })();
    drop(output);
    let verified = written.and_then(|()| {
        owned.verify_identity()?;
        verify_file(path, bytes.len() as u64, &blake3::hash(&bytes))?;
        File::open(path.parent().context("Manifest directory")?)?.sync_all()?;
        owned.verify_identity()?;
        Ok(())
    });
    if let Err(error) = verified {
        if let Err(cleanup) = owned.remove() {
            return Err(anyhow::anyhow!(
                "{error:#}; Manifest cleanup {}: {cleanup:#}",
                path.display()
            ));
        }
        return Err(error);
    }
    Ok(owned)
}
