//! FreeDesktop Trash storage, without a host GLib/GIO installation.
//!
//! Only regular files selected by a frozen DeletionPlan are accepted. All
//! mutations use pinned directory descriptors; existing Trash links, unsafe
//! permissions, cross-device moves, and unsupported operations fail closed.
use super::{DeletionFile, Identity, open_parent, regular_metadata_at};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Component, Path, PathBuf},
};

struct Trash {
    files: File,
    info: File,
    // Volume Trash metadata uses paths relative to the mount point so it
    // remains restorable if a removable volume is mounted elsewhere later.
    top: Option<PathBuf>,
}

pub(super) fn trash(file: &DeletionFile, parent: &File) -> Result<()> {
    let data = data_home()?;
    let data_dir = directory_tree(&data)?;
    let trash = if data_dir.metadata()?.dev() == parent.metadata()?.dev() {
        open_trash(&data_dir, OsStr::new("Trash"), None)?
    } else {
        let top = mount_point(&file.path, &fs::read("/proc/self/mountinfo")?)?;
        volume_trash(&top)?
    };
    move_to_trash(file, parent, &trash)
        .context("Could not move file to Trash; no permanent-delete fallback was used")
}

fn data_home() -> Result<PathBuf> {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        let path = PathBuf::from(data);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?);
    ensure!(home.is_absolute(), "HOME must be absolute to locate Trash");
    Ok(home.join(".local/share"))
}

fn open_directory(parent: &File, name: &OsStr, create: bool) -> Result<File> {
    let name = CString::new(name.as_bytes())?;
    if create {
        // SAFETY: the borrowed directory and terminated name are live.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
    }
    // SAFETY: successful descriptor ownership is transferred to File below.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("Open real Trash directory");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

// HOME/data-home ancestors may legitimately be symlinks (e.g. /home on Fedora
// Atomic). Resolve the trusted base once, then pin real directory descriptors.
// The Trash, files and info storage itself must never be symbolic links.
fn directory_tree(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "Trash location must be absolute");
    let mut existing = path;
    let mut missing = Vec::new();
    let resolved = loop {
        match fs::canonicalize(existing) {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .context("Invalid Trash base")?
                        .to_os_string(),
                );
                existing = existing.parent().context("Invalid Trash base")?;
            }
            Err(error) => return Err(error).context("Resolve Trash base"),
        }
    };
    let mut dir = File::open("/")?;
    for component in resolved.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => dir = open_directory(&dir, name, false)?,
            _ => bail!("Invalid Trash location"),
        }
    }
    for name in missing.iter().rev() {
        dir = open_directory(&dir, name, true)?;
    }
    Ok(dir)
}

fn private_directory(parent: &File, name: &OsStr) -> Result<File> {
    let dir = open_directory(parent, name, true)?;
    let metadata = dir.metadata()?;
    // Do not chmod an existing directory: insecure or foreign-owned Trash
    // storage is an error, not something to silently adopt.
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o7777 == 0o700,
        "Trash directories must belong to this user and have mode 0700"
    );
    Ok(dir)
}

fn open_trash(parent: &File, name: &OsStr, top: Option<PathBuf>) -> Result<Trash> {
    let dir = private_directory(parent, name)?;
    Ok(Trash {
        files: private_directory(&dir, OsStr::new("files"))?,
        info: private_directory(&dir, OsStr::new("info"))?,
        top,
    })
}

fn volume_trash(top: &Path) -> Result<Trash> {
    // open_parent walks every existing component without following links.
    let directory = open_parent(&top.join(".shadowroom-directory-handle"))?;
    let uid = unsafe { libc::geteuid() }.to_string();
    if let Ok(shared) = open_directory(&directory, OsStr::new(".Trash"), false) {
        if shared.metadata()?.mode() & 0o1000 != 0 {
            if let Ok(trash) = open_trash(&shared, OsStr::new(&uid), Some(top.into())) {
                return Ok(trash);
            }
        }
    }
    open_trash(
        &directory,
        OsStr::new(&format!(".Trash-{uid}")),
        Some(top.into()),
    )
    .context("No safe writable Trash is available on this volume")
}

fn mount_point(path: &Path, mountinfo: &[u8]) -> Result<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for line in mountinfo.split(|&b| b == b'\n') {
        if let Some(field) = line.split(|&b| b == b' ').nth(4) {
            let decoded = unescape_mount(field)?;
            let candidate = PathBuf::from(OsString::from_vec(decoded));
            if candidate.is_absolute()
                && path.starts_with(&candidate)
                && best
                    .as_ref()
                    .is_none_or(|old| candidate.as_os_str().len() > old.as_os_str().len())
            {
                best = Some(candidate);
            }
        }
    }
    best.context("Cannot identify the photo volume's mount point")
}

fn unescape_mount(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            ensure!(
                i + 3 < bytes.len()
                    && bytes[i + 1..i + 4]
                        .iter()
                        .all(|b| (b'0'..=b'7').contains(b)),
                "Invalid mount-table escape"
            );
            let value = (bytes[i + 1] - b'0') as u16 * 64
                + (bytes[i + 2] - b'0') as u16 * 8
                + (bytes[i + 3] - b'0') as u16;
            ensure!(value <= 255, "Invalid mount-table byte");
            result.push(value as u8);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Ok(result)
}

fn encoded_path(path: &Path) -> String {
    let mut encoded = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("String write cannot fail");
        }
    }
    encoded
}

fn move_to_trash(file: &DeletionFile, parent: &File, trash: &Trash) -> Result<()> {
    let original = match &trash.top {
        Some(top) => file
            .path
            .strip_prefix(top)
            .context("File is outside its Trash volume")?,
        None => &file.path,
    };
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encoded_path(original),
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    );
    let source = CString::new(
        file.path
            .file_name()
            .context("Photo has no filename")?
            .as_bytes(),
    )?;
    // Keep candidate lengths below NAME_MAX even when the original filename
    // already fills it. Metadata retains the complete original bytes.
    let original_name = source.as_bytes();
    for attempt in 0..10_000 {
        let mut candidate = original_name[..original_name.len().min(180)].to_vec();
        if attempt > 0 {
            candidate.extend_from_slice(format!(".{attempt}").as_bytes());
        }
        let target = CString::new(candidate.clone())?;
        candidate.extend_from_slice(b".trashinfo");
        let info = CString::new(candidate)?;
        // Metadata must be reserved and durable before the photo moves. O_EXCL
        // also rejects a symlink and coordinates with other Trash writers.
        let fd = unsafe {
            libc::openat(
                trash.info.as_raw_fd(),
                info.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error).context("Reserve Trash restore metadata");
        }
        let mut metadata = unsafe { File::from_raw_fd(fd) };
        let outcome = (|| -> Result<()> {
            metadata.write_all(body.as_bytes())?;
            metadata.sync_all()?;
            trash.info.sync_all()?;
            ensure!(
                Identity::from(&regular_metadata_at(parent, &file.path)?) == file.identity,
                "File changed since confirmation: {}",
                file.path.display()
            );
            rename_noreplace(parent, &source, &trash.files, &target)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                // A successful move remains successful if a directory fsync is
                // unsupported. The complete restore metadata was synced first.
                let _ = trash.files.sync_all();
                let _ = parent.sync_all();
                return Ok(());
            }
            Err(error) => {
                // Only remove the metadata reservation created above. No file
                // in files/ is ever deleted as a fallback or failure cleanup.
                unsafe {
                    libc::unlinkat(trash.info.as_raw_fd(), info.as_ptr(), 0);
                }
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::AlreadyExists)
                {
                    continue;
                }
                return Err(error);
            }
        }
    }
    bail!("Could not reserve a unique Trash filename")
}

fn rename_noreplace(from: &File, source: &CString, to: &File, target: &CString) -> io::Result<()> {
    // Use the kernel ABI rather than glibc's >= 2.28 renameat2 symbol.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            source.as_ptr(),
            to.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // Never emulate moving user files with linkat + unlinkat: another writer
    // could replace the source name after linking, and unlinkat would then
    // permanently remove the replacement. Kernels/filesystems without atomic
    // no-replace rename must report unsupported Trash instead.
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deletion::{DeletionMode, DeletionPlan},
        model::Photo,
    };
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn planned(path: &Path) -> DeletionPlan {
        DeletionPlan::prepare(
            &[Photo {
                id: 1,
                path: path.into(),
                ..Default::default()
            }],
            DeletionMode::SystemTrash,
        )
        .unwrap()
    }

    fn fixture_trash(root: &Path) -> Trash {
        open_trash(&File::open(root).unwrap(), OsStr::new("Trash"), None).unwrap()
    }

    #[test]
    fn collisions_preserve_existing_payloads_and_metadata_and_encoded_paths_restore() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join(OsString::from_vec(b"photo %\n\xff.jpg".to_vec()));
        fs::write(&path, b"first photo").unwrap();
        let trash = fixture_trash(&root);
        let plan = planned(&path);
        move_to_trash(&plan.files[0], &open_parent(&path).unwrap(), &trash).unwrap();
        fs::write(&path, b"second photo").unwrap();
        let second = planned(&path);
        move_to_trash(&second.files[0], &open_parent(&path).unwrap(), &trash).unwrap();
        let basename = path.file_name().unwrap();
        assert_eq!(
            fs::read(root.join("Trash/files").join(basename)).unwrap(),
            b"first photo"
        );
        let mut second_name = basename.to_os_string();
        second_name.push(".1");
        assert_eq!(
            fs::read(root.join("Trash/files").join(&second_name)).unwrap(),
            b"second photo"
        );
        let mut info_name = second_name.clone();
        info_name.push(".trashinfo");
        let info = fs::read_to_string(root.join("Trash/info").join(info_name)).unwrap();
        assert!(info.starts_with("[Trash Info]\n"));
        assert!(info.contains(&format!("Path={}\n", encoded_path(&path))));
        assert!(info.contains("photo%20%25%0A%FF.jpg"));
        let date = info
            .lines()
            .find_map(|line| line.strip_prefix("DeletionDate="))
            .unwrap();
        chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S").unwrap();
        fs::rename(root.join("Trash/files").join(second_name), &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second photo");
    }

    #[test]
    fn orphan_payload_collision_is_not_overwritten() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let trash = fixture_trash(&root);
        fs::write(root.join("Trash/files/a.jpg"), b"existing orphan").unwrap();
        fs::write(root.join("a.jpg"), b"new photo").unwrap();
        let plan = planned(&root.join("a.jpg"));
        move_to_trash(
            &plan.files[0],
            &open_parent(&root.join("a.jpg")).unwrap(),
            &trash,
        )
        .unwrap();
        assert_eq!(
            fs::read(root.join("Trash/files/a.jpg")).unwrap(),
            b"existing orphan"
        );
        assert_eq!(
            fs::read(root.join("Trash/files/a.jpg.1")).unwrap(),
            b"new photo"
        );
        assert!(!root.join("Trash/info/a.jpg.trashinfo").exists());
    }

    #[test]
    fn changed_source_fails_without_removing_it_or_leaving_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join("a.jpg");
        fs::write(&path, b"original").unwrap();
        let plan = planned(&path);
        fs::write(&path, b"changed source").unwrap();
        let trash = fixture_trash(&root);
        assert!(move_to_trash(&plan.files[0], &open_parent(&path).unwrap(), &trash).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"changed source");
        assert_eq!(fs::read_dir(root.join("Trash/info")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.join("Trash/files")).unwrap().count(), 0);
    }

    #[test]
    fn unsafe_and_symlink_trash_directories_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::create_dir(root.join("other")).unwrap();
        symlink(root.join("other"), root.join("Trash")).unwrap();
        assert!(open_trash(&File::open(&root).unwrap(), OsStr::new("Trash"), None).is_err());
        fs::remove_file(root.join("Trash")).unwrap();
        fs::create_dir(root.join("Trash")).unwrap();
        fs::set_permissions(root.join("Trash"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(open_trash(&File::open(&root).unwrap(), OsStr::new("Trash"), None).is_err());
        assert_eq!(fs::read_dir(root.join("other")).unwrap().count(), 0);
        fs::set_permissions(root.join("Trash"), fs::Permissions::from_mode(0o700)).unwrap();
        symlink(root.join("other"), root.join("Trash/files")).unwrap();
        assert!(open_trash(&File::open(&root).unwrap(), OsStr::new("Trash"), None).is_err());
    }

    #[test]
    fn trusted_data_home_symlinks_work_without_allowing_symlink_trash_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("actual-home");
        fs::create_dir(&real).unwrap();
        let alias = temporary.path().join("home-alias");
        symlink(&real, &alias).unwrap();
        let data = directory_tree(&alias.join(".local/share")).unwrap();
        let actual = fs::metadata(real.join(".local/share")).unwrap();
        assert_eq!(data.metadata().unwrap().ino(), actual.ino());
        open_trash(&data, OsStr::new("Trash"), None).unwrap();
        assert!(real.join(".local/share/Trash/info").is_dir());
        let actual_trash = real.join(".local/share/Trash");
        fs::rename(&actual_trash, real.join("saved-trash")).unwrap();
        symlink(real.join("saved-trash"), &actual_trash).unwrap();
        assert!(open_trash(&data, OsStr::new("Trash"), None).is_err());
    }

    #[test]
    fn sticky_shared_volume_trash_and_private_fallback_use_relative_restore_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let uid = unsafe { libc::geteuid() };
        fs::create_dir(root.join(".Trash")).unwrap();
        fs::set_permissions(root.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        let trash = volume_trash(&root).unwrap();
        let source = root.join("a.jpg");
        fs::write(&source, b"photo").unwrap();
        let plan = planned(&source);
        move_to_trash(&plan.files[0], &open_parent(&source).unwrap(), &trash).unwrap();
        let info =
            fs::read_to_string(root.join(format!(".Trash/{uid}/info/a.jpg.trashinfo"))).unwrap();
        assert!(info.contains("\nPath=a.jpg\n"));
        fs::set_permissions(root.join(".Trash"), fs::Permissions::from_mode(0o777)).unwrap();
        volume_trash(&root).unwrap();
        assert!(root.join(format!(".Trash-{uid}/files")).is_dir());
        fs::remove_dir_all(root.join(".Trash")).unwrap();
        symlink(root.join(format!(".Trash-{uid}")), root.join(".Trash")).unwrap();
        volume_trash(&root).unwrap();
    }

    #[test]
    fn nested_and_escaped_mount_points_choose_the_exact_volume() {
        let mounts = b"1 0 0:1 / / rw - ext4 /dev/root rw\n2 1 0:2 / /media/photo\\040drive rw - vfat /dev/sdb rw\n3 2 0:3 / /media/photo\\040drive/nested rw - ext4 /dev/sdc rw\n";
        assert_eq!(
            mount_point(Path::new("/media/photo drive/nested/a.jpg"), mounts).unwrap(),
            Path::new("/media/photo drive/nested")
        );
        assert_eq!(
            mount_point(Path::new("/media/photo driver/a.jpg"), mounts).unwrap(),
            Path::new("/")
        );
    }
}
