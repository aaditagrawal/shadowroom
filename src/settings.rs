//! Persistent preferences. A malformed settings file is reported, never overwritten
//! by the loader; callers can present the error and continue with defaults.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const DEFAULT_INTERFACE_FONT: &str = "Geist Mono";
pub const INTERFACE_FONTS: &[&str] = &["Geist Mono", "DM Mono", "DM Sans"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub interface_font: String,
    pub theme: String,
    /// `off`, `time`, `visual`, or `burst`.
    pub grouping_mode: String,
    pub burst_gap_ms: u64,
    pub visual_threshold: u32,
    pub auto_advance: bool,
    pub auto_discover_drives: bool,
    pub cache_budget_mb: usize,
    pub memory_budget_mb: usize,
    pub show_getting_started: bool,
    /// `system` moves originals to OS trash; `in_app` only hides catalog groups.
    pub trash_mode: String,
    pub last_photo_id: Option<i64>,
    pub view_mode: String,
    pub show_inspector: bool,
    pub show_sidebar: bool,
    /// Preferred dock widths in logical pixels; a narrow window can fit them smaller.
    pub sidebar_width: u32,
    pub inspector_width: u32,
    pub min_rating: u8,
    pub filter: String,
    pub ingest_on_insert: String,
    pub ingest_library_root: PathBuf,
    pub ingest_folder_template: String,
    pub ingest_file_template: String,
    pub ingest_backup_roots: Vec<PathBuf>,
    pub ingest_job_codes: Vec<String>,
    pub ingest_day_start_hour: u8,
    pub ingest_collision: String,
    pub ingest_time_group_minutes: u32,
    pub eject_after_ingest: bool,
    pub ingest_include_videos: bool,
    /// Volume identity → allow / deny. No mount paths are used as identity.
    pub ingest_volumes: BTreeMap<String, String>,
    pub ingest_presets: BTreeMap<String, (String, String)>,
    /// Preserve settings from newer versions when writing known preferences.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interface_font: DEFAULT_INTERFACE_FONT.into(),
            theme: "Shadowroom Dark".into(),
            grouping_mode: "burst".into(),
            burst_gap_ms: 2000,
            visual_threshold: 8,
            auto_advance: true,
            auto_discover_drives: true,
            cache_budget_mb: 8192,
            memory_budget_mb: 0,
            show_getting_started: true,
            trash_mode: "system".into(),
            last_photo_id: None,
            view_mode: "loupe".into(),
            show_inspector: true,
            show_sidebar: true,
            sidebar_width: 224,
            inspector_width: 288,
            min_rating: 0,
            filter: "all".into(),
            ingest_on_insert: "ask".into(),
            ingest_library_root: directories::UserDirs::new()
                .map(|dirs| {
                    dirs.picture_dir()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| dirs.home_dir().join("Pictures"))
                })
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from(std::path::MAIN_SEPARATOR_STR))
                        .join("Pictures")
                })
                .join("Shadowroom"),
            ingest_folder_template: "{YYYY}/{YYYY}-{MM}-{DD}{_job}".into(),
            ingest_file_template: "{original}".into(),
            ingest_backup_roots: Vec::new(),
            ingest_job_codes: Vec::new(),
            ingest_day_start_hour: 3,
            ingest_collision: "suffix".into(),
            ingest_time_group_minutes: 60,
            eject_after_ingest: false,
            ingest_include_videos: true,
            ingest_volumes: BTreeMap::new(),
            ingest_presets: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let mut settings: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("Parse preferences at {}", path.display()))?;
                settings.normalize();
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(error).with_context(|| format!("Read preferences at {}", path.display()))
            }
        }
    }

    /// Save a complete JSON snapshot by same-directory atomic rename. A failed
    /// serialization/write leaves the previous file intact. Parent directories
    /// are created, and the file is synced before becoming visible.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut normalized = self.clone();
        normalized.normalize();
        let mut bytes = serde_json::to_vec_pretty(&normalized)?;
        bytes.push(b'\n');
        atomic_write(path, &bytes)
    }

    pub fn load_default() -> Result<Self> {
        Self::load(&settings_path()?)
    }
    pub fn save_default(&self) -> Result<()> {
        self.save(&settings_path()?)
    }

    /// Clamp resource budgets and grouping parameters at the persistence boundary.
    /// User-selected theme names and installed font names remain unrestricted.
    pub fn normalize(&mut self) {
        if self.interface_font.trim().is_empty() {
            self.interface_font = DEFAULT_INTERFACE_FONT.into();
        }
        if self.theme.trim().is_empty() {
            self.theme = "Shadowroom Dark".into();
        }
        self.grouping_mode = self.grouping_mode.to_ascii_lowercase();
        if !["off", "time", "visual", "burst"].contains(&self.grouping_mode.as_str()) {
            self.grouping_mode = "off".into();
        }
        self.burst_gap_ms = self.burst_gap_ms.clamp(1, 3_600_000);
        self.visual_threshold = self.visual_threshold.min(64);
        if self.memory_budget_mb != 0 {
            self.memory_budget_mb = self.memory_budget_mb.clamp(64, 65_536);
        }
        self.cache_budget_mb = self.cache_budget_mb.clamp(128, 1_048_576);
        self.min_rating = self.min_rating.min(5);
        if !["system", "in_app"].contains(&self.trash_mode.as_str()) {
            self.trash_mode = "system".into();
        }
        self.sidebar_width = self.sidebar_width.clamp(180, 480);
        self.inspector_width = self.inspector_width.clamp(240, 480);
        self.ingest_day_start_hour = self.ingest_day_start_hour.min(23);
        self.ingest_time_group_minutes = self.ingest_time_group_minutes.clamp(1, 1440);
        if !["ask", "auto", "index_in_place", "ignore"].contains(&self.ingest_on_insert.as_str()) {
            self.ingest_on_insert = "ask".into();
        }
        if !["suffix", "skip"].contains(&self.ingest_collision.as_str()) {
            self.ingest_collision = "suffix".into();
        }
        self.ingest_job_codes.truncate(20);
    }

    pub fn remote_cache_bytes(&self) -> u64 {
        self.extra
            .get("remote_cache_budget_mb")
            .and_then(Value::as_u64)
            .unwrap_or(2048)
            .min(1_048_576)
            .saturating_mul(1024 * 1024)
    }
    pub fn memory_budget_bytes(&self) -> usize {
        if self.memory_budget_mb == 0 {
            automatic_memory_budget()
        } else {
            self.memory_budget_mb.saturating_mul(1024 * 1024)
        }
    }
    pub fn cache_budget_bytes(&self) -> usize {
        self.cache_budget_mb.saturating_mul(1024 * 1024)
    }
}

/// Use a quarter of physical RAM, bounded to 512 MiB–4 GiB. Resolve once;
/// querying the OS never becomes part of the frame/render workload.
fn automatic_memory_budget() -> usize {
    static BUDGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| {
        #[cfg(unix)]
        {
            // SAFETY: sysconf has no pointer arguments or process side effects.
            let (pages, page_size) = unsafe {
                (
                    libc::sysconf(libc::_SC_PHYS_PAGES),
                    libc::sysconf(libc::_SC_PAGESIZE),
                )
            };
            if pages > 0 && page_size > 0 {
                return ((pages as u64).saturating_mul(page_size as u64) / 4)
                    .clamp(512 * 1024 * 1024, 4 * 1024 * 1024 * 1024)
                    as usize;
            }
        }
        1024 * 1024 * 1024
    })
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("Could not find user configuration directory")?
        .config_dir()
        .join("shadowroom/settings.json"))
}

/// Also used for exporting shortcut profiles. Never writes through a target
/// symlink: rename replaces the directory entry itself.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with_sync(path, bytes, |parent| {
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
        Ok(())
    })
}

fn atomic_write_with_sync(
    path: &Path,
    bytes: &[u8],
    mut sync_parent: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("Create {}", parent.display()))?;
    let filename = path
        .file_name()
        .context("Output path needs a filename")?
        .to_string_lossy();
    let mut created = None;
    for _ in 0..32 {
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".{filename}.{}.{}.tmp", std::process::id(), nonce));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => {
                created = Some((temporary, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Create temporary file beside {}", path.display()));
            }
        }
    }
    let Some((temporary, mut file)) = created else {
        bail!("Could not create a unique temporary settings file");
    };
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path).with_context(|| format!("Replace {}", path.display()))?;
        // Visibility after rename is not a durable acknowledgement. An error
        // here leaves the new value visible, but callers must keep it pending.
        // Retry the entire ancestor chain even when a previous write created
        // it successfully but failed its directory sync. Existence on retry
        // says nothing about whether those directory entries are durable.
        let mut directory = Some(parent);
        while let Some(current) = directory {
            sync_parent(current)
                .with_context(|| format!("Sync output directory {}", current.display()))?;
            directory = current
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .or_else(|| {
                    (!current.is_absolute() && current != Path::new(".")).then_some(Path::new("."))
                });
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_preferences_default_to_system_trash_and_preserve_explicit_in_app_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"auto_advance":false}"#).unwrap();
        let mut settings = Settings::load(&path).unwrap();
        assert_eq!(settings.trash_mode, "system");
        settings.trash_mode = "in_app".into();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().trash_mode, "in_app");
        settings.trash_mode = "permanent".into();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().trash_mode, "system");
    }
    #[test]
    fn failed_directory_sync_is_reported_and_retry_preserves_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"old").unwrap();
        let error = atomic_write_with_sync(&path, b"new", |_| {
            Err(std::io::Error::other("injected directory sync failure").into())
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected directory sync failure"));
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
    #[test]
    fn directory_sync_retry_revisits_previously_created_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/settings.json");
        let mut first = Vec::new();
        let error = atomic_write_with_sync(&path, b"new", |parent| {
            first.push(parent.to_path_buf());
            if parent == dir.path() {
                anyhow::bail!("injected ancestor failure");
            }
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected ancestor failure"));
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let mut retried = Vec::new();
        atomic_write_with_sync(&path, b"new", |parent| {
            retried.push(parent.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert!(
            retried.starts_with(&first),
            "retry skipped an unsynced ancestor"
        );
        assert!(retried.contains(&dir.path().to_path_buf()));
    }
    #[test]
    fn relative_nested_output_syncs_dot_and_retries_it_after_failure() {
        // A relative, test-owned directory exercises the namespace containing
        // its first component without changing process-global current_dir.
        let dir = tempfile::tempdir_in(".").unwrap();
        let relative = Path::new(dir.path().file_name().unwrap());
        let path = relative.join("a/b/settings.json");
        let mut synced = Vec::new();
        assert!(
            atomic_write_with_sync(&path, b"new", |parent| {
                synced.push(parent.to_path_buf());
                if parent == Path::new(".") {
                    anyhow::bail!("injected dot sync failure");
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(synced.last().unwrap(), Path::new("."));
        let mut retried = Vec::new();
        atomic_write_with_sync(&path, b"new", |parent| {
            retried.push(parent.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(retried, synced);
        assert_eq!(fs::read(path).unwrap(), b"new");
    }
    #[test]
    fn roundtrip_defaults_and_preserve_unknown_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let mut settings = Settings::default();
        settings.interface_font = "DM Mono".into();
        settings.auto_advance = false;
        settings.sidebar_width = 352;
        settings.inspector_width = 400;
        settings.show_sidebar = false;
        settings.extra.insert(
            "future_preference".into(),
            serde_json::json!({"nested":true}),
        );
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        settings.theme = "Shadowroom Light".into();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().theme, "Shadowroom Light");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn loads_partial_and_normalizes_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"memory_budget_mb":0,"visual_threshold":999,"grouping_mode":"BURST","sidebar_width":0,"inspector_width":9999}"#,
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.memory_budget_mb, 0);
        assert!(
            (512 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&settings.memory_budget_bytes())
        );
        assert!(settings.show_getting_started);
        assert_eq!(settings.visual_threshold, 64);
        assert_eq!(settings.grouping_mode, "burst");
        assert_eq!(settings.interface_font, "Geist Mono");
        assert_eq!(settings.sidebar_width, 180);
        assert_eq!(settings.inspector_width, 480);
        assert!(settings.show_sidebar);
    }
    #[test]
    fn malformed_file_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "broken JSON").unwrap();
        assert!(Settings::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken JSON");
    }
    #[test]
    fn failed_save_keeps_target_and_cleans_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a-directory");
        fs::create_dir(&path).unwrap();
        assert!(Settings::default().save(&path).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
