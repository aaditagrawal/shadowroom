//! Background-only disk LRU for rebuildable JPEG representations and sidecars.
use anyhow::Result;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

#[derive(Debug)]
pub struct DiskLru {
    dir: PathBuf,
    budget: AtomicU64,
    state: Mutex<Option<Entries>>,
}
#[derive(Default, Debug)]
struct Entries {
    files: HashMap<PathBuf, (SystemTime, u64)>,
    order: BTreeSet<(SystemTime, PathBuf)>,
    bytes: u64,
}
impl DiskLru {
    pub fn new(dir: PathBuf, bytes: u64) -> Self {
        Self {
            dir,
            budget: AtomicU64::new(bytes),
            state: Mutex::new(None),
        }
    }
    pub fn set_budget(&self, bytes: u64) -> Result<()> {
        self.budget.store(bytes, Ordering::Release);
        self.update(&[])
    }
    /// Called only by background cache readers/writers. One directory scan per
    /// cache instance; subsequent touches and eviction are O(log(cache entries)).
    pub fn update(&self, paths: &[&Path]) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let mut entries = Entries::default();
            for entry in fs::read_dir(&self.dir)? {
                let entry = entry?;
                let path = entry.path();
                if !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("jpg" | "json")
                ) {
                    continue;
                }
                let meta = entry.metadata()?;
                if !meta.is_file() {
                    continue;
                }
                let time = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                entries.bytes = entries.bytes.saturating_add(meta.len());
                entries.order.insert((time, path.clone()));
                entries.files.insert(path, (time, meta.len()));
            }
            *guard = Some(entries);
        }
        let entries = guard.as_mut().unwrap();
        for path in paths {
            if let Some((time, size)) = entries.files.remove(*path) {
                entries.order.remove(&(time, path.to_path_buf()));
                entries.bytes = entries.bytes.saturating_sub(size);
            }
            if let Ok(meta) = fs::metadata(path) {
                let time = SystemTime::now();
                // Persist recency across app/agent restarts without a database.
                if let Ok(file) = fs::File::open(path) {
                    let _ = file.set_modified(time);
                }
                entries.bytes = entries.bytes.saturating_add(meta.len());
                entries.order.insert((time, path.to_path_buf()));
                entries.files.insert(path.to_path_buf(), (time, meta.len()));
            }
        }
        while entries.bytes > self.budget.load(Ordering::Acquire) {
            let Some((time, path)) = entries.order.first().cloned() else {
                break;
            };
            // Keep failed removals accounted for and report an actionable error.
            match fs::remove_file(&path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
            entries.order.remove(&(time, path.clone()));
            if let Some((_, size)) = entries.files.remove(&path) {
                entries.bytes = entries.bytes.saturating_sub(size);
            }
        }
        Ok(())
    }
}
