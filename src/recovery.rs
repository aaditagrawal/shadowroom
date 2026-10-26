//! Last-resort recovery for OS termination, after the native close gate can no
//! longer veto exit. Originals are never touched; replay uses the catalog actor.
use crate::{
    model::PhotoDecision,
    settings::{Settings, atomic_write},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path};

/// One writable native workspace owns this catalog and its recovery journal.
/// The OS releases the advisory lock on crash; the file itself is never removed.
pub struct CatalogLease {
    _file: fs::File,
}
impl CatalogLease {
    pub fn acquire(data: &Path) -> Result<Self> {
        fs::create_dir_all(data)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(data.join("workspace.lock"))?;
        file.try_lock().context("Another Shadowroom workspace already owns this catalog. Use the existing window or its MCP endpoint")?;
        Ok(Self { _file: file })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryJournal {
    version: u32,
    pub decisions: Vec<PhotoDecision>,
    pub settings: Option<Settings>,
}
impl RecoveryJournal {
    pub fn new(decisions: Vec<PhotoDecision>, settings: Option<Settings>) -> Self {
        Self {
            version: 1,
            decisions,
            settings,
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?)
            .context("Sync pending decision recovery journal")?;
        Self::sync_parent(path)
    }
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Open decision recovery journal"),
        };
        const LIMIT: u64 = 256 * 1024 * 1024;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            bail!("Decision recovery journal exceeds 256 MiB");
        }
        let journal: Self =
            serde_json::from_slice(&bytes).context("Parse decision recovery journal")?;
        if journal.version != 1 {
            bail!("Unsupported decision recovery journal version");
        }
        if journal.decisions.iter().any(|d| d.id <= 0 || d.rating > 5) {
            bail!("Invalid decision in recovery journal");
        }
        Ok(Some(journal))
    }
    /// Remove only after both decisions and preferences have durable ACKs.
    pub fn clear(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Clear replayed recovery journal"),
        }
        // Retry the directory sync even if a previous unlink succeeded but its
        // sync failed. A missing filename alone is not a confirmed durable clear.
        Self::sync_parent(path)
    }
    fn sync_parent(path: &Path) -> Result<()> {
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?
                .sync_all()
                .context("Sync recovery directory")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::Catalog,
        model::{ColorLabel, Flag, Photo},
    };
    #[test]
    fn catalog_lease_excludes_another_writer_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let lease = CatalogLease::acquire(dir.path()).unwrap();
        assert!(CatalogLease::acquire(dir.path()).is_err());
        drop(lease);
        assert!(CatalogLease::acquire(dir.path()).is_ok());
    }
    #[test]
    fn os_quit_journal_replays_latest_decision_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        let photo = Photo {
            path: "/shoot/a.jpg".into(),
            filename: "a.jpg".into(),
            ..Photo::default()
        };
        catalog.upsert_photos(&[photo]).unwrap();
        let id = catalog.photos().unwrap()[0].id;
        let latest = PhotoDecision {
            id,
            rating: 5,
            flag: Flag::Pick,
            label: ColorLabel::Blue,
            trashed: false,
        };
        let settings = Settings {
            auto_advance: false,
            ..Settings::default()
        };
        RecoveryJournal::new(vec![latest], Some(settings.clone()))
            .save(&path)
            .unwrap();
        let replay = RecoveryJournal::load(&path).unwrap().unwrap();
        assert_eq!(replay.decisions, vec![latest]);
        catalog.set_decisions(&replay.decisions).unwrap();
        replay
            .settings
            .unwrap()
            .save(&dir.path().join("settings.json"))
            .unwrap();
        catalog.flush().unwrap();
        RecoveryJournal::clear(&path).unwrap();
        assert!(RecoveryJournal::load(&path).unwrap().is_none());
        assert_eq!(catalog.photos().unwrap()[0].rating, 5);
        assert_eq!(
            Settings::load(&dir.path().join("settings.json")).unwrap(),
            settings
        );
    }
    #[test]
    fn malformed_journal_remains_available_for_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending.json");
        fs::write(&path, b"{broken").unwrap();
        assert!(RecoveryJournal::load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
    }
}
