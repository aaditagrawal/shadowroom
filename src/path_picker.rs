//! Bounded filesystem listings for the built-in picker. Call from an I/O worker.
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 512;
const MAX_SCANNED: usize = 16_384;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}

#[derive(Debug)]
pub struct Listing {
    pub directory: PathBuf,
    pub entries: Vec<Entry>,
    pub truncated: bool,
    pub unreadable: usize,
}

/// Keep actual paths, including non-UTF8 names, separate from display labels.
/// Follow directory symlinks as a file chooser normally does; this never writes.
pub fn list(path: &Path, json_files: bool) -> Result<Listing> {
    let directory =
        fs::canonicalize(path).with_context(|| format!("Cannot open {}", path.display()))?;
    ensure!(directory.is_dir(), "Choose a directory to browse");
    let mut listing = Listing {
        directory: directory.clone(),
        entries: Vec::new(),
        truncated: false,
        unreadable: 0,
    };
    for (scanned, item) in fs::read_dir(&directory)?.enumerate() {
        if scanned >= MAX_SCANNED {
            listing.truncated = true;
            break;
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => {
                listing.unreadable += 1;
                continue;
            }
        };
        let path = entry.path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                listing.unreadable += 1;
                continue;
            }
        };
        if !metadata.is_dir()
            && !(json_files
                && metadata.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("json")))
        {
            continue;
        }
        if listing.entries.len() >= MAX_ENTRIES {
            listing.truncated = true;
            break;
        }
        listing.entries.push(Entry {
            path,
            directory: metadata.is_dir(),
        });
    }
    listing.entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(listing)
}

/// Resolve relative entries against the directory shown, never the process cwd.
pub fn address(text: &str, directory: &Path) -> Result<PathBuf> {
    ensure!(!text.is_empty(), "Enter a path");
    if text == "~" || text.starts_with("~/") {
        let home = directories::BaseDirs::new().context("Home directory is unavailable")?;
        return Ok(home.home_dir().join(text.strip_prefix("~/").unwrap_or("")));
    }
    let path = PathBuf::from(text);
    Ok(if path.is_absolute() {
        path
    } else {
        directory.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_and_theme_listings_keep_file_types_separate() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("shoot")).unwrap();
        fs::write(root.path().join("theme.JSON"), "{}").unwrap();
        fs::write(root.path().join("photo.jpg"), "pixels").unwrap();
        assert_eq!(list(root.path(), false).unwrap().entries.len(), 1);
        let all = list(root.path(), true).unwrap();
        assert_eq!(all.entries.len(), 2);
        assert!(all.entries[0].directory);
        assert!(!all.entries[1].directory);
    }
    #[test]
    fn relative_paths_use_the_displayed_directory() {
        assert_eq!(
            address("next", Path::new("/photos")).unwrap(),
            Path::new("/photos/next")
        );
        assert!(address("", Path::new("/")).is_err());
    }
    #[test]
    fn very_large_directories_are_explicitly_truncated() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..MAX_ENTRIES + 1 {
            fs::create_dir(root.path().join(i.to_string())).unwrap();
        }
        let listing = list(root.path(), false).unwrap();
        assert_eq!(listing.entries.len(), MAX_ENTRIES);
        assert!(listing.truncated);
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_directory_paths_are_preserved() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let child = root
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'p', 255]));
        fs::create_dir(&child).unwrap();
        assert_eq!(list(root.path(), false).unwrap().entries[0].path, child);
    }
}
