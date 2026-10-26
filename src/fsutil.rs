//! Shared bounded reads for regular local files. Not a path-sandbox primitive.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
};

pub(crate) fn open_regular(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A final-component replacement must not follow a symlink or wait for
        // a FIFO writer before its type can be checked on the opened handle.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .with_context(|| format!("Open {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "Not a regular file: {}",
        path.display()
    );
    Ok(file)
}

pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = open_regular(path)?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "File exceeds byte limit: {}",
        path.display()
    );
    read_bounded_from(file, limit)
}

fn read_bounded_from(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let bound = limit.checked_add(1).context("Invalid byte limit")?;
    let mut bytes = Vec::new();
    reader.take(bound as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "File grew beyond byte limit");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn growing_reader_stops_after_limit_and_one_detection_byte() {
        let mut source = std::io::repeat(17);
        assert!(read_bounded_from(&mut source, 1024).is_err());
        assert_eq!(read_bounded_from(&b"abc"[..], 3).unwrap(), b"abc");
        assert!(read_bounded_from(&b"abcd"[..], 3).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn special_files_and_terminal_symlinks_are_rejected_without_waiting() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular");
        std::fs::write(&file, b"data").unwrap();
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(open_regular(&link).is_err());
        assert!(open_regular(dir.path()).is_err());
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live, NUL-terminated pathname owned by this fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(open_regular(&fifo).is_err());
        assert_eq!(read_bounded(&file, 4).unwrap(), b"data");
        assert!(read_bounded(&file, 3).is_err());
    }
}
