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
    /// Open staging outputs, written and verified but not yet synced: the
    /// commit batch syncs them together before its catalog transaction.
    outputs: Vec<File>,
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
    mut update: impl FnMut(u64, Phase),
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
    let written = std::thread::scope(|scope| -> Result<Vec<File>> {
        let mut senders = Vec::new();
        let mut writers = Vec::new();
        for (mut output, path) in outputs.into_iter().zip(temps.iter().cloned()) {
            let (tx, rx) = crossbeam_channel::bounded::<Arc<Vec<u8>>>(4);
            senders.push(tx);
            let source_time = source_time(&file.source);
            writers.push(scope.spawn(move || -> Result<File> {
                while let Ok(bytes) = rx.recv() {
                    control.checkpoint()?;
                    hooks.write_chunk(&path, &mut output, &bytes)?;
                }
                output.set_times(fs::FileTimes::new().set_modified(source_time))?;
                Ok(output)
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
                update(read as u64, Phase::Copying);
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
        let mut finished = Vec::new();
        for writer in writers {
            match writer
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Ingest writer panicked")))
            {
                Ok(output) => finished.push(output),
                Err(error) => writer_error = Some(error),
            }
        }
        if let Some(error) = writer_error {
            return Err(error);
        }
        read_result?;
        Ok(finished)
    })?;
    // Writers can outlive the reader. Recheck the path after joining them, so a
    // source replaced or edited by then cannot be committed as the scanned file.
    ensure!(
        same_source(&before, &checked_source(&file.source)?),
        "Source changed while copying: {}",
        file.source.path.display()
    );
    let hash = hasher.finalize();
    update(0, Phase::Verifying);
    // Destinations are independent (often separate disks): reread them concurrently.
    std::thread::scope(|scope| -> Result<()> {
        let checks: Vec<_> = temps
            .iter()
            .map(|temp| scope.spawn(|| verify_interruptible(temp, size, &hash, control)))
            .collect();
        for check in checks {
            check
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Ingest verifier panicked")))?;
        }
        Ok(())
    })?;
    Ok(Staged {
        outputs: written,
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
    group: PlannedGroup,
    creates_files: bool,
    cleanup_errors: Vec<String>,
}

/// A group whose every member is staged, verified and published at every
/// destination, waiting for its commit batch. Dropping it uncommitted removes
/// the entries it created, exactly like a failed group.
struct PendingGroup {
    record: ImportRecord,
    group: PlannedGroup,
    creates_files: bool,
    rollback: Rollback,
    outputs: Vec<File>,
    temps: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    source_metadata: Vec<fs::Metadata>,
}

/// Commit batches amortize durability barriers over many small groups without
/// widening the window in which verified, published files lack a history row.
const BATCH_GROUPS: usize = 64;
const BATCH_FILES: usize = 256;
const BATCH_AGE: Duration = Duration::from_millis(500);

/// Same identity, regular file and expected size: a published name that is the
/// staged inode verified above. Rename and link change no file contents.
fn published_identity(target: &Path, staged: &OwnedPath, size: u64) -> Result<()> {
    let metadata = fs::symlink_metadata(target)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == size,
        "Published import is not the verified file: {}",
        target.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.dev() == staged.dev && metadata.ino() == staged.ino,
            "Published import is not the verified file: {}",
            target.display()
        );
    }
    #[cfg(not(unix))]
    let _ = staged;
    Ok(())
}

fn group_copy(
    group: &PlannedGroup,
    options: &IngestOptions,
    roots: &[PathBuf],
    job: &str,
    control: &Control,
    hooks: &dyn CopyHooks,
    mut update: impl FnMut(u64, Phase),
) -> Result<Option<PendingGroup>> {
    let mut rollback = Rollback::default();
    let outcome = (|| -> Result<Option<(ImportRecord, Vec<Staged>, Vec<PathBuf>)>> {
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
        let mut directories = Vec::new();
        for (file, staged) in files.iter().zip(&staged) {
            let mut destinations = Vec::new();
            let mut existed = Vec::new();
            for (root, temp) in roots.iter().zip(&staged.temps) {
                control.checkpoint()?;
                let target = root.join(&file.relative);
                let existing = match publish_noreplace(temp, &target) {
                    Ok(()) => {
                        // The verified staging inode now carries the final name.
                        let mut owned = rollback
                            .paths
                            .iter()
                            .find(|owned| owned.path == *temp)
                            .context("Missing staging ownership record")?
                            .clone();
                        // Keep the original identity for cleanup, including if the
                        // final name is replaced before the identity check.
                        owned.path = target.clone();
                        rollback.paths.push(owned.clone());
                        published_identity(&target, &owned, staged.size)?;
                        false
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // An identical file appeared after the collision check:
                        // verify its content in full; publication never replaces
                        // any directory entry.
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
                let directory = target.parent().context("Destination parent")?.to_path_buf();
                if !directories.contains(&directory) {
                    directories.push(directory);
                }
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
        // again once the whole group is published, including earlier files.
        for (file, staged) in files.iter().zip(&staged) {
            ensure!(
                same_source(&staged.source_metadata, &checked_source(&file.source)?),
                "Source changed before group commit: {}",
                file.source.path.display()
            );
        }
        Ok(Some((record, staged, directories)))
    })();
    match outcome {
        Ok(Some((record, staged, directories))) => {
            let creates_files = record
                .files
                .iter()
                .any(|f| f.existed.iter().any(|exists| !exists));
            let mut outputs = Vec::new();
            let mut temps = Vec::new();
            let mut source_metadata = Vec::new();
            for staged in staged {
                outputs.extend(staged.outputs);
                temps.extend(staged.temps);
                source_metadata.push(staged.source_metadata);
            }
            Ok(Some(PendingGroup {
                record,
                group: group.clone(),
                creates_files,
                rollback,
                outputs,
                temps,
                directories,
                source_metadata,
            }))
        }
        outcome => {
            let outcome = outcome.map(|_| None);
            if let Err(cleanup) = rollback.cleanup() {
                return Err(anyhow::anyhow!(
                    "{}; {cleanup:#}",
                    outcome
                        .as_ref()
                        .err()
                        .map_or_else(|| "Group skipped".into(), |e| format!("{e:#}"))
                ));
            }
            outcome
        }
    }
}

/// Fsync every item on a few threads. Concurrent fsyncs share journal commits
/// on common filesystems, so a batch costs a fraction of serial barriers.
fn sync_all_concurrently<T: Sync>(
    items: &[T],
    sync: impl Fn(&T) -> Result<()> + Sync,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let threads = items.len().min(16);
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| -> Result<()> {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            return Ok(());
                        };
                        sync(item)?;
                    }
                })
            })
            .collect();
        let mut first_error = None;
        for worker in workers {
            if let Err(error) = worker
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Ingest sync worker panicked")))
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    })
}

/// The commit point of a batch: file data, then directory entries, are made
/// durable, then one catalog transaction records every group. Nothing in the
/// batch counts as imported before that transaction commits; any failure
/// removes the entries the batch created, as for a failed single group.
fn commit_batch(
    batch: Vec<PendingGroup>,
    catalog: &Catalog,
    hooks: &dyn CopyHooks,
) -> std::result::Result<Vec<CommittedGroup>, (Vec<PendingGroup>, anyhow::Error)> {
    let durable = (|| -> Result<()> {
        let outputs: Vec<&File> = batch.iter().flat_map(|g| &g.outputs).collect();
        sync_all_concurrently(&outputs, |file| Ok(file.sync_all()?))
            .context("Sync imported files")?;
        let mut directories: Vec<&PathBuf> = batch.iter().flat_map(|g| &g.directories).collect();
        directories.sort();
        directories.dedup();
        sync_all_concurrently(&directories, |directory| {
            File::open(directory)?.sync_all()?;
            Ok(())
        })
        .context("Sync import directories")?;
        // The copier may have moved on to another (large or paused) group.
        // Revalidate at the commit boundary, after durability barriers, rather
        // than relying on checks made when this group entered the queue.
        for pending in &batch {
            for (file, metadata) in pending.group.files.iter().zip(&pending.source_metadata) {
                let current = checked_source(&file.source).with_context(|| {
                    format!(
                        "Source changed before import commit: {}",
                        file.source.path.display()
                    )
                })?;
                ensure!(
                    same_source(metadata, &current),
                    "Source changed before import commit: {}",
                    file.source.path.display()
                );
            }
            for file in &pending.record.files {
                for (path, existed) in file.destinations.iter().zip(&file.existed) {
                    if !existed {
                        let owned = pending
                            .rollback
                            .paths
                            .iter()
                            .find(|owned| &owned.path == path)
                            .context("Missing published ownership record")?;
                        published_identity(path, owned, file.size)?;
                    }
                }
            }
        }
        let items: Vec<_> = batch
            .iter()
            .map(|g| {
                (
                    &g.record,
                    Some(&g.group.group),
                    g.creates_files.then_some(g.group.day.as_str()),
                )
            })
            .collect();
        catalog.ingest_commit_batch(&items)
    })();
    if let Err(error) = durable {
        return Err((batch, error));
    }
    // Committed: cancellation or cleanup trouble from here on keeps the groups.
    Ok(batch
        .into_iter()
        .map(|mut pending| {
            pending.rollback.committed = true;
            drop(pending.outputs);
            let mut cleanup_errors = Vec::new();
            for temp in &pending.temps {
                // Retain the identity captured on exclusive creation. Re-snapshotting
                // here could adopt and remove a staging path replaced by someone else.
                let owned = pending
                    .rollback
                    .paths
                    .iter()
                    .find(|owned| owned.path == *temp);
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
            CommittedGroup {
                record: pending.record,
                group: pending.group,
                creates_files: pending.creates_files,
                cleanup_errors,
            }
        })
        .collect())
}

enum ToCommitter {
    Group(PendingGroup),
    /// After a failed batch: everything queued before this marker was failed.
    Resync,
}
enum FromCommitter {
    Settled(Vec<Settled>),
    Resynced,
}
enum Settled {
    Committed(CommittedGroup),
    Failed { path: PathBuf, error: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommitState {
    Accepting,
    /// Queued groups were named using counters from a failed batch. Fail them
    /// until the copier's marker confirms it will reload committed counters.
    AwaitingResync,
}

fn fail_pending(mut pending: PendingGroup, error: &str) -> Settled {
    let cleanup = pending.rollback.cleanup().err();
    Settled::Failed {
        path: pending.group.group.primary.path.clone(),
        error: format!(
            "{error}{}",
            cleanup.map_or_else(String::new, |c| format!("; {c:#}"))
        ),
    }
}

/// Commits groups in arrival order on its own thread, so a batch reaches its
/// commit point on time even while the copier is paused or busy with a long
/// file. A failed batch fails every group queued behind it until the copier
/// acknowledges with `Resync`: those were named assuming the batch committed,
/// and resynchronizing counters with the catalog keeps names free of gaps.
fn run_committer(
    incoming: crossbeam_channel::Receiver<ToCommitter>,
    outgoing: crossbeam_channel::Sender<FromCommitter>,
    catalog: &Catalog,
    hooks: &dyn CopyHooks,
    control: &Control,
) {
    let mut batch: Vec<PendingGroup> = Vec::new();
    let mut since = Instant::now();
    let mut state = CommitState::Accepting;
    let flush = |batch: &mut Vec<PendingGroup>, state: &mut CommitState| {
        if batch.is_empty() {
            return;
        }
        let settled = match commit_batch(std::mem::take(batch), catalog, hooks) {
            Ok(committed) => committed.into_iter().map(Settled::Committed).collect(),
            Err((failed, error)) => {
                *state = CommitState::AwaitingResync;
                let error = format!("{error:#}");
                failed
                    .into_iter()
                    .map(|pending| fail_pending(pending, &error))
                    .collect()
            }
        };
        let _ = outgoing.send(FromCommitter::Settled(settled));
    };
    loop {
        // With no pending durability work, a long copy or pause needs no
        // polling. Only a collecting batch checks pause/deadline periodically.
        let message = if batch.is_empty() {
            incoming
                .recv()
                .map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected)
        } else {
            incoming.recv_timeout(
                BATCH_AGE
                    .saturating_sub(since.elapsed())
                    .min(Duration::from_millis(20)),
            )
        };
        match message {
            Ok(ToCommitter::Group(group)) if state == CommitState::AwaitingResync => {
                let failed = fail_pending(group, "An earlier import commit batch failed");
                let _ = outgoing.send(FromCommitter::Settled(vec![failed]));
            }
            Ok(ToCommitter::Group(group)) => {
                if batch.is_empty() {
                    since = Instant::now();
                }
                batch.push(group);
            }
            Ok(ToCommitter::Resync) => {
                flush(&mut batch, &mut state);
                state = CommitState::Accepting;
                let _ = outgoing.send(FromCommitter::Resynced);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                // Finished or cancelled: verified groups still commit.
                flush(&mut batch, &mut state);
                return;
            }
        }
        if !batch.is_empty()
            && (batch.len() >= BATCH_GROUPS
                || batch.iter().map(|g| g.outputs.len()).sum::<usize>() >= BATCH_FILES
                || since.elapsed() >= BATCH_AGE
                || control.is_paused())
        {
            flush(&mut batch, &mut state);
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn pending_group() -> (tempfile::TempDir, Catalog, PendingGroup) {
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = directory.path().join("card");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("A.mov"), b"verified video").unwrap();
        let catalog = Catalog::open(&directory.path().join("catalog.sqlite3")).unwrap();
        let control = Control::default();
        let scan = crate::ingest::scan(&source, &catalog, |_| {}, &control).unwrap();
        let plan = crate::ingest::plan(
            &scan,
            IngestOptions {
                library_root: directory.path().join("library"),
                folder_template: String::new(),
                file_template: "{original}".into(),
                include_videos: true,
                ..Default::default()
            },
            SequenceState::default(),
        )
        .unwrap();
        let roots = validate_roots(&plan).unwrap();
        fs::create_dir_all(&roots[0]).unwrap();
        catalog.ingest_begin("batch-test", &plan).unwrap();
        let pending = group_copy(
            &plan.groups[0],
            &plan.options,
            &roots,
            "batch-test",
            &control,
            &DefaultHooks,
            |_, _| {},
        )
        .unwrap()
        .unwrap();
        (directory, catalog, pending)
    }

    #[test]
    fn source_changed_while_waiting_for_commit_rolls_back_publication() {
        let (_directory, catalog, pending) = pending_group();
        let target = pending.record.files[0].destinations[0].clone();
        fs::write(
            &pending.group.files[0].source.path,
            b"changed since staging",
        )
        .unwrap();
        let Err((failed, error)) = commit_batch(vec![pending], &catalog, &DefaultHooks) else {
            panic!("changed source committed");
        };
        assert!(format!("{error:#}").contains("Source changed before import commit"));
        drop(failed);
        assert!(!target.exists());
        assert_eq!(catalog.ingest_sequences().unwrap().stored, 0);
        assert_eq!(
            catalog.ingest_history(10).unwrap()["total_imported_groups"],
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaced_publication_is_never_committed_or_removed_by_rollback() {
        let (directory, catalog, pending) = pending_group();
        let target = pending.record.files[0].destinations[0].clone();
        let foreign = directory.path().join("foreign.mov");
        fs::write(&foreign, b"foreign output").unwrap();
        fs::rename(&foreign, &target).unwrap();
        let Err((failed, error)) = commit_batch(vec![pending], &catalog, &DefaultHooks) else {
            panic!("replaced publication committed");
        };
        assert!(format!("{error:#}").contains("Published import is not the verified file"));
        for mut pending in failed {
            let error = pending.rollback.cleanup().unwrap_err();
            assert!(format!("{error:#}").contains("Cleanup refused replaced path"));
        }
        assert_eq!(fs::read(&target).unwrap(), b"foreign output");
        assert_eq!(catalog.ingest_sequences().unwrap().stored, 0);
        assert_eq!(
            catalog.ingest_history(10).unwrap()["total_imported_groups"],
            0
        );
    }
}

/// Folds settled groups into the report; returns whether any failed.
fn fold(settled: Vec<Settled>, report: &mut Report) -> bool {
    let mut failed = false;
    for item in settled {
        match item {
            Settled::Committed(CommittedGroup {
                record,
                group,
                creates_files,
                cleanup_errors,
            }) => {
                report.progress.errors.extend(cleanup_errors);
                if creates_files {
                    report.progress.completed += 1;
                } else {
                    report.progress.skipped += 1;
                }
                report.progress.files += record.files.len();
                report.progress.bytes += group.group.bytes();
                if group.group.kind == "video" {
                    report.progress.videos += 1;
                } else {
                    report.progress.photos += 1;
                }
                report.records.push(record);
            }
            Settled::Failed { path, error } => {
                failed = true;
                report.progress.failed += 1;
                report
                    .progress
                    .errors
                    .push(format!("{}: {error}", path.display()));
            }
        }
    }
    failed
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
            phase: Phase::Copying,
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
    // Verified, published groups go to the committer; settled outcomes return.
    // The bound keeps the copier at most a few groups (and open files) ahead.
    let (to_committer, committer_incoming) = crossbeam_channel::bounded(8);
    let (committer_outgoing, settled) = crossbeam_channel::unbounded();
    std::thread::scope(|scope| {
        let committer = scope.spawn(move || {
            run_committer(
                committer_incoming,
                committer_outgoing,
                catalog,
                hooks,
                control,
            )
        });
        let mut state = SequenceState::default();
        let mut session = 0;
        let run = (|| -> Result<()> {
            for root in &roots {
                fs::create_dir_all(root)?;
                File::open(root)?.sync_all()?;
                if let Some(parent) = root.parent() {
                    File::open(parent)?.sync_all()?;
                }
            }
            let mut rate = Throughput::new(Instant::now());
            let mut last_update = Instant::now() - Duration::from_secs(1);
            // `moved` is bytes actually read (the rate); `handled` also counts
            // whole groups skipped or failed (the bar and time left).
            let mut moved = 0u64;
            let mut handled = 0u64;
            state = catalog.ingest_sequences()?;
            for planned in &plan.groups {
                if control.is_cancelled() {
                    break;
                }
                let mut batch_failed = false;
                for message in settled.try_iter() {
                    if let FromCommitter::Settled(list) = message {
                        batch_failed |= fold(list, &mut report);
                    }
                }
                if batch_failed {
                    // Wait until every group named after the failed batch is settled,
                    // then continue from the counters the catalog actually holds.
                    to_committer
                        .send(ToCommitter::Resync)
                        .map_err(|_| anyhow::anyhow!("Import committer stopped"))?;
                    for message in settled.iter() {
                        match message {
                            FromCommitter::Settled(list) => {
                                fold(list, &mut report);
                            }
                            FromCommitter::Resynced => break,
                        }
                    }
                    state = catalog.ingest_sequences()?;
                    session = report.progress.completed as u64;
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
                    let fingerprints_match =
                        planned.group.files().zip(&record.files).all(|(s, r)| {
                            s.path.file_name() == r.source.file_name()
                                && s.size == r.size
                                && s.mtime_ns == r.mtime_ns
                        });
                    let source_unchanged = fingerprints_match
                        && planned.group.files().all(|f| checked_source(f).is_ok());
                    if same_roots
                        && source_unchanged
                        && planned.group.files().zip(&record.files).all(|(source, f)| {
                            blake3::Hash::from_hex(&f.blake3).ok().is_some_and(|hash| {
                                // Metadata is only a history hint. A source may have been
                                // edited while preserving size and timestamp since import.
                                verify_interruptible(&source.path, f.size, &hash, control).is_ok()
                                    && f.destinations.iter().all(|path| {
                                        fs::symlink_metadata(path).is_ok_and(|m| {
                                            m.is_file() && !m.file_type().is_symlink()
                                        }) && verify_interruptible(path, f.size, &hash, control)
                                            .is_ok()
                                    })
                            })
                        })
                    {
                        report.progress.skipped += 1;
                        handled += planned.group.bytes();
                        report.progress.transferred = handled;
                        if last_update.elapsed() >= Duration::from_millis(100) {
                            update(&report.progress);
                            last_update = Instant::now();
                        }
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
                let group_start = handled;
                let result = group_copy(
                    &named,
                    &plan.options,
                    &roots,
                    &job,
                    control,
                    hooks,
                    |bytes, phase| {
                        moved += bytes;
                        handled += bytes;
                        // Phase changes are reported immediately; byte counts are throttled.
                        let phase_changed = report.progress.phase != phase;
                        report.progress.phase = phase;
                        report.progress.transferred = handled;
                        rate.sample(moved, control.paused_for(), Instant::now());
                        report.progress.mb_per_second = rate.bytes_per_second() / 1_000_000.;
                        report.progress.eta_seconds = rate.eta(plan.bytes.saturating_sub(handled));
                        if phase_changed || last_update.elapsed() >= Duration::from_millis(100) {
                            update(&report.progress);
                            last_update = Instant::now();
                        }
                    },
                );
                // A group that stopped early still counts as handled in full.
                handled = handled.max(group_start + named.group.bytes());
                report.progress.transferred = handled;
                match result {
                    Ok(Some(group)) => {
                        // Later names in this batch assume it commits; a failed batch
                        // resynchronizes these counters with the catalog.
                        if group.creates_files {
                            state.stored += 1;
                            *state.days.entry(group.group.day.clone()).or_default() += 1;
                            session += 1;
                        }
                        to_committer
                            .send(ToCommitter::Group(group))
                            .map_err(|_| anyhow::anyhow!("Import committer stopped"))?;
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
                // Byte progress is throttled above; per-group counters are too, so a
                // card of small files cannot flood the UI channel.
                if last_update.elapsed() >= Duration::from_millis(100) {
                    update(&report.progress);
                    last_update = Instant::now();
                }
            }
            Ok(())
        })();
        if let Err(error) = run {
            if !control.is_cancelled() {
                report.progress.errors.push(format!("{error:#}"));
            }
        }
        // Groups already verified and published commit even after cancellation,
        // exactly as when each group committed on its own.
        drop(to_committer);
        for message in settled.iter() {
            if let FromCommitter::Settled(list) = message {
                fold(list, &mut report);
            }
        }
        if let Err(panic) = committer.join() {
            std::panic::resume_unwind(panic);
        }
    });
    report.progress.phase = if control.is_cancelled() {
        Phase::Cancelled
    } else if report.progress.errors.is_empty() {
        Phase::Completed
    } else {
        Phase::Failed
    };
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

/// Copy throughput over active (unpaused) time: an exponentially weighted
/// average of one-second windows, so the rate follows the card reader and a
/// pause never drags it down or inflates the time left.
pub(crate) struct Throughput {
    started: Instant,
    window_at: f64,
    window_bytes: u64,
    average: Option<f64>,
    active: f64,
    moved: u64,
}
impl Throughput {
    /// Weight of the newest one-second window.
    const ALPHA: f64 = 0.2;
    /// Time left is withheld until this much copying has been measured.
    const SETTLE_SECONDS: f64 = 5.;
    pub(crate) fn new(started: Instant) -> Self {
        Self {
            started,
            window_at: 0.,
            window_bytes: 0,
            average: None,
            active: 0.,
            moved: 0,
        }
    }
    /// Records `moved` bytes read so far, `paused` total pause time, at `now`.
    pub(crate) fn sample(&mut self, moved: u64, paused: Duration, now: Instant) {
        self.active = (now
            .saturating_duration_since(self.started)
            .saturating_sub(paused))
        .as_secs_f64();
        let window = self.active - self.window_at;
        if window >= 1. {
            let sample = moved.saturating_sub(self.window_bytes) as f64 / window;
            self.average = Some(
                self.average
                    .map_or(sample, |a| Self::ALPHA * sample + (1. - Self::ALPHA) * a),
            );
            self.window_at = self.active;
            self.window_bytes = moved;
        }
        self.moved = moved;
    }
    pub(crate) fn bytes_per_second(&self) -> f64 {
        self.average
            .unwrap_or_else(|| self.moved as f64 / self.active.max(0.001))
    }
    /// Seconds left for `remaining` bytes, or None while still estimating.
    pub(crate) fn eta(&self, remaining: u64) -> Option<u64> {
        let rate = self.bytes_per_second();
        (self.active >= Self::SETTLE_SECONDS && rate > 0.).then(|| (remaining as f64 / rate) as u64)
    }
}

/// History stays non-successful until the proposed outcome artifact verifies.
/// Only this job's original artifact identity can be removed for a failed outcome;
/// every creation (including replacement) is exclusive and fully verified.
fn finalize_report(report: &mut Report, root: &Path, catalog: &Catalog, hooks: &dyn CopyHooks) {
    let manifest = root.join(format!("shadowroom-ingest-{}.json", report.progress.job_id));
    report.progress.manifest = Some(manifest.clone());
    let mut pending = report.progress.clone();
    pending.phase = Phase::Finalizing;
    if let Err(error) = catalog.ingest_finish(&pending) {
        report.progress.phase = Phase::Failed;
        report
            .progress
            .errors
            .push(format!("Prepare ingest finalization: {error:#}"));
    }
    let artifact = match write_manifest(report, &manifest, hooks) {
        Ok(owned) => Some(owned),
        Err(error) => {
            report.progress.phase = Phase::Failed;
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
        report.progress.phase = Phase::Failed;
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
