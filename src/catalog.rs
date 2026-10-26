use crate::model::{ColorLabel, Flag, Photo, PhotoDecision, PhotoSource, RemoteSource};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

fn validate_remote_path(path: &Path) -> Result<()> {
    anyhow::ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "Invalid remote relative path"
    );
    Ok(())
}

/// Minimal read-set for an incremental scan; avoids deserializing every Photo.
pub(crate) struct IndexedFile {
    pub modified_at: i64,
    pub size_bytes: u64,
    pub companion_path: Option<PathBuf>,
    pub companion_modified_at: i64,
    pub companion_size_bytes: u64,
    pub metadata_version: u32,
}
#[derive(Clone)]
pub struct Catalog {
    pub(crate) connection: Arc<Mutex<Connection>>,
    pub(crate) ingest_lock: Arc<Mutex<()>>,
    path: PathBuf,
}
impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path).context("Open local photo catalog")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA mmap_size=268435456;
          CREATE TABLE IF NOT EXISTS photos (
            id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, modified_at INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL, rating INTEGER NOT NULL DEFAULT 0 CHECK(rating BETWEEN 0 AND 5),
            flag TEXT NOT NULL DEFAULT 'none', label TEXT NOT NULL DEFAULT '', data TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS photos_flag ON photos(flag);
          CREATE INDEX IF NOT EXISTS photos_companion ON photos(json_extract(data,'$.companion_path')) WHERE json_extract(data,'$.companion_path') IS NOT NULL;
          CREATE TABLE IF NOT EXISTS sources(path TEXT PRIMARY KEY);")?;
        // An independent decision column preserves Trash through metadata refresh
        // and in-place JPEG→RAW promotion. Old catalogs migrate without rewriting
        // metadata blobs or retiring IDs.
        let has_trash = {
            let mut columns = connection.prepare("PRAGMA table_info(photos)")?;
            let names = columns.query_map([], |row| row.get::<_, String>(1))?;
            names
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .any(|name| name == "trashed")
        };
        if !has_trash {
            connection.execute_batch("ALTER TABLE photos ADD COLUMN trashed INTEGER NOT NULL DEFAULT 0 CHECK(trashed IN (0,1));")?;
        }
        // Add source columns independently of the metadata JSON. Existing IDs,
        // path uniqueness and decisions remain untouched during migration.
        let columns = connection
            .prepare("PRAGMA table_info(photos)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|c| c == "source_device") {
            connection.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE photos ADD COLUMN source_device TEXT;
                ALTER TABLE photos ADD COLUMN source_root TEXT;
                ALTER TABLE photos ADD COLUMN source_volume TEXT;
                ALTER TABLE photos ADD COLUMN source_relative TEXT;
                CREATE UNIQUE INDEX photos_remote_identity ON photos(source_device,source_root,source_volume,source_relative) WHERE source_device IS NOT NULL;
                COMMIT;")?;
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS remote_cursors(device TEXT,root TEXT,revision INTEGER NOT NULL,PRIMARY KEY(device,root));
            CREATE TABLE IF NOT EXISTS remote_stage(token TEXT,path TEXT,data TEXT,PRIMARY KEY(token,path));")?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS exact_hashes(path TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,sample TEXT NOT NULL,full_hash TEXT);")?;
        crate::ingest::store::migrate(&connection)?;
        Ok(Self {
            ingest_lock: Arc::new(Mutex::new(())),
            connection: Arc::new(Mutex::new(connection)),
            path: path.to_path_buf(),
        })
    }
    /// Runs after queued decisions have committed, on a background actor.
    pub fn flush(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let (busy, _, _): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(FULL)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if busy != 0 {
            bail!(
                "Catalog checkpoint blocked by another reader; committed decisions remain durable in the WAL"
            );
        }
        Ok(())
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn register_source(&self, path: &Path) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        conn.execute(
            "INSERT OR IGNORE INTO sources(path) VALUES(?1)",
            [path.canonicalize()?.to_string_lossy()],
        )?;
        Ok(())
    }
    /// Capture decisions before dispatch; execution performs all filesystem work.
    pub fn prepare_export(
        &self,
        destination: &Path,
        picks: bool,
        ids: &[i64],
    ) -> Result<ExportPlan> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let mut photos = Vec::new();
        if picks {
            // The flag index limits snapshot work to the actual export set.
            let mut stmt = conn.prepare_cached("SELECT id,data,rating,flag,label,trashed FROM photos WHERE flag='pick' AND trashed=0 ORDER BY id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                photos.push(photo_from_row(row)?);
            }
        } else {
            let mut selected = ids.to_vec();
            selected.sort_unstable();
            selected.dedup();
            let mut stmt = conn.prepare_cached(
                "SELECT id,data,rating,flag,label,trashed FROM photos WHERE id=?1 AND trashed=0",
            )?;
            for id in selected {
                let mut rows = stmt.query([id])?;
                if let Some(row) = rows.next()? {
                    photos.push(photo_from_row(row)?);
                }
            }
        }
        let mut stmt = conn.prepare("SELECT path FROM sources")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut source_roots = Vec::new();
        for row in rows {
            source_roots.push(PathBuf::from(row?));
        }
        for photo in &photos {
            photo.require_local("Export and XMP writing")?;
        }
        Ok(ExportPlan {
            photos,
            destination: destination.to_path_buf(),
            picks,
            source_roots,
        })
    }
    pub(crate) fn fingerprints(
        &self,
        root: &Path,
    ) -> Result<std::collections::HashMap<PathBuf, IndexedFile>> {
        let root = root
            .to_str()
            .context("Source root is not valid UTF-8")?
            .trim_end_matches('/');
        // The path UNIQUE index supplies exactly this root's descendants. '/' sorts
        // immediately before '0', so this range also includes arbitrary UTF-8 names.
        let lower = format!("{root}/");
        let upper = format!("{root}0");
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        // Decode the small fingerprint read-set in one pass over borrowed JSON.
        // Multiple json_extract expressions repeatedly traverse the same blob.
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct MetadataFingerprint {
            companion_path: Option<PathBuf>,
            companion_modified_at: i64,
            companion_size_bytes: u64,
            metadata_version: u32,
        }
        let mut stmt = conn.prepare_cached(
            "SELECT path,modified_at,size_bytes,data FROM photos WHERE source_device IS NULL AND path>=?1 AND path<?2",
        )?;
        let mut rows = stmt.query(params![lower, upper])?;
        let mut entries = HashMap::new();
        while let Some(row) = rows.next()? {
            let metadata: MetadataFingerprint = serde_json::from_str(row.get_ref(3)?.as_str()?)?;
            entries.insert(
                PathBuf::from(row.get_ref(0)?.as_str()?),
                IndexedFile {
                    modified_at: row.get(1)?,
                    size_bytes: row.get::<_, i64>(2)? as u64,
                    companion_path: metadata.companion_path,
                    companion_modified_at: metadata.companion_modified_at,
                    companion_size_bytes: metadata.companion_size_bytes,
                    metadata_version: metadata.metadata_version,
                },
            );
        }
        Ok(entries)
    }
    pub fn photos(&self) -> Result<Vec<Photo>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let mut stmt = conn
            .prepare_cached("SELECT id,data,rating,flag,label,trashed FROM photos ORDER BY id")?;
        let mut rows = stmt.query([])?;
        let mut photos = Vec::new();
        while let Some(row) = rows.next()? {
            photos.push(photo_from_row(row)?);
        }
        Ok(photos)
    }
    pub fn set_rating(&self, id: i64, rating: u8) -> Result<()> {
        if rating > 5 {
            bail!("Rating must be between 0 and 5");
        }
        self.update(id, "rating", &rating.to_string())
    }
    pub fn set_flag(&self, id: i64, flag: Flag) -> Result<()> {
        self.update(id, "flag", flag.as_str())
    }
    pub fn set_label(&self, id: i64, label: ColorLabel) -> Result<()> {
        self.update(id, "label", label.as_str())
    }
    fn update(&self, id: i64, column: &str, value: &str) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        if conn.execute(
            &format!("UPDATE photos SET {column}=?1 WHERE id=?2"),
            params![value, id],
        )? == 0
        {
            bail!("Photo {id} does not exist");
        }
        Ok(())
    }
    /// Metadata batches preserve all user decisions when a source changes.
    pub fn upsert_photos(&self, photos: &[Photo]) -> Result<usize> {
        for photo in photos {
            photo.require_local("Local indexing")?;
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = conn.transaction()?;
        {
            let mut existing = tx.prepare_cached("SELECT id FROM photos WHERE path=?1")?;
            let mut owner=tx.prepare_cached("SELECT id FROM photos WHERE source_device IS NULL AND json_extract(data,'$.companion_path')=?1 AND path<>?2 LIMIT 1")?;
            let mut promote = tx.prepare_cached(
                "UPDATE photos SET path=?1,modified_at=?2,size_bytes=?3,data=?4 WHERE id=?5",
            )?;
            let mut upsert=tx.prepare_cached("INSERT INTO photos(path,modified_at,size_bytes,data) VALUES(?1,?2,?3,?4) ON CONFLICT(path) DO UPDATE SET modified_at=excluded.modified_at,size_bytes=excluded.size_bytes,data=excluded.data")?;
            // A shared companion gets at most one deterministic owner per batch.
            let mut ordered: Vec<_> = photos.iter().collect();
            ordered.sort_by(|a, b| a.path.cmp(&b.path));
            for photo in ordered {
                let path = photo.path.to_string_lossy();
                let source_id: Option<i64> = if photo.companion_path.is_some() {
                    existing
                        .query_row([path.as_ref()], |r| r.get(0))
                        .optional()?
                } else {
                    None
                };
                let mut metadata = std::borrow::Cow::Borrowed(photo);
                let mut promotion = None;
                if let Some(companion) = &photo.companion_path {
                    let companion_path = companion.to_string_lossy();
                    let companion_id: Option<i64> = existing
                        .query_row([companion_path.as_ref()], |r| r.get(0))
                        .optional()?;
                    let claimed: Option<i64> = owner
                        .query_row(params![companion_path.as_ref(), path.as_ref()], |r| {
                            r.get(0)
                        })
                        .optional()?;
                    if companion == &photo.path
                        || claimed.is_some()
                        || (source_id.is_some() && companion_id.is_some())
                    {
                        // Already-distinct identities may have history/outbox writes.
                        // Keep both records and their decisions; never retire an ID.
                        let independent = metadata.to_mut();
                        independent.companion_path = None;
                        independent.companion_size_bytes = 0;
                        independent.companion_modified_at = 0;
                    } else if source_id.is_none() {
                        promotion = companion_id;
                    }
                }
                let data = serde_json::to_string(metadata.as_ref())?;
                if let Some(id) = promotion {
                    // Promote the JPEG record in place: undo, selection and delayed
                    // decision writes continue to address the same logical photo.
                    promote.execute(params![
                        path.as_ref(),
                        photo.modified_at,
                        photo.size_bytes as i64,
                        data,
                        id
                    ])?;
                } else {
                    upsert.execute(params![
                        path.as_ref(),
                        photo.modified_at,
                        photo.size_bytes as i64,
                        data
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(photos.len())
    }
    pub fn remote_revision(&self, device: &str, root: &str) -> Result<u64> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        Ok(conn
            .query_row(
                "SELECT revision FROM remote_cursors WHERE device=?1 AND root=?2",
                params![device, root],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
    /// Staging is durable and bounded by batches, but invisible until terminal sync.
    pub fn stage_remote(
        &self,
        token: &str,
        device: &str,
        root: &str,
        records: &[crate::remote::protocol::PhotoRecord],
        deleted: &[PathBuf],
    ) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO remote_stage(token,path,data) VALUES(?1,?2,?3) ON CONFLICT(token,path) DO UPDATE SET data=excluded.data")?;
            for record in records {
                anyhow::ensure!(
                    record.key.root_id == root,
                    "Remote record belongs to a different root"
                );
                validate_remote_path(&record.key.relative_path)?;
                if let Some(path) = &record.key.companion_path {
                    validate_remote_path(path)?;
                }
                let k = &record.key;
                let mut photo = record.metadata.clone();
                photo.id = 0;
                photo.path = k.relative_path.clone();
                photo.folder = k
                    .relative_path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .into();
                photo.companion_path = k.companion_path.clone();
                photo.size_bytes = k.size_bytes;
                photo.modified_at = k.modified_at;
                photo.companion_size_bytes = k.companion_size_bytes;
                photo.companion_modified_at = k.companion_modified_at;
                photo.rating = 0;
                photo.flag = Flag::None;
                photo.label = ColorLabel::None;
                photo.trashed = false;
                photo.source = PhotoSource::Remote(RemoteSource {
                    device_id: device.into(),
                    root_id: root.into(),
                    volume_id: k.volume_id.clone(),
                    relative_path: k.relative_path.clone(),
                    size_bytes: k.size_bytes,
                    modified_at: k.modified_at,
                    companion_path: k.companion_path.clone(),
                    companion_size_bytes: k.companion_size_bytes,
                    companion_modified_at: k.companion_modified_at,
                    available: true,
                });
                insert.execute(params![
                    token,
                    k.relative_path.to_string_lossy(),
                    serde_json::to_string(&photo)?
                ])?;
            }
            for path in deleted {
                validate_remote_path(path)?;
                insert.execute(params![
                    token,
                    path.to_string_lossy(),
                    Option::<String>::None
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn discard_remote_stage(&self, token: &str) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        conn.execute("DELETE FROM remote_stage WHERE token=?1", [token])?;
        Ok(())
    }
    /// Metadata, tombstones and cursor publish atomically. SQL decision columns
    /// are deliberately absent from the conflict update.
    pub fn finish_remote_sync(
        &self,
        token: &str,
        device: &str,
        root: &str,
        revision: u64,
        full: bool,
    ) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = conn.transaction()?;
        if full {
            tx.execute("UPDATE photos SET data=json_set(data,'$.source.available',json('false')) WHERE source_device=?1 AND source_root=?2", params![device,root])?;
        }
        tx.execute("UPDATE photos SET data=json_set(data,'$.source.available',json('false')) WHERE source_device=?1 AND source_root=?2 AND source_relative IN (SELECT path FROM remote_stage WHERE token=?3 AND data IS NULL)", params![device,root,token])?;
        tx.execute("INSERT INTO photos(path,modified_at,size_bytes,data,source_device,source_root,source_volume,source_relative)
            SELECT 'remote:'||hex(?2)||':'||hex(?3)||':'||hex(json_extract(data,'$.source.volume_id'))||':'||hex(path),json_extract(data,'$.modified_at'),json_extract(data,'$.size_bytes'),data,?2,?3,json_extract(data,'$.source.volume_id'),path
            FROM remote_stage WHERE token=?1 AND data IS NOT NULL
            ON CONFLICT(path) DO UPDATE SET modified_at=excluded.modified_at,size_bytes=excluded.size_bytes,data=excluded.data", params![token,device,root])?;
        tx.execute("INSERT INTO remote_cursors(device,root,revision) VALUES(?1,?2,?3) ON CONFLICT(device,root) DO UPDATE SET revision=excluded.revision", params![device,root,revision])?;
        tx.execute("DELETE FROM remote_stage WHERE token=?1", [token])?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_decisions(&self, decisions: &[PhotoDecision]) -> Result<()> {
        if decisions.iter().any(|d| d.rating > 5) {
            bail!("Rating must be between 0 and 5");
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE photos SET rating=?1,flag=?2,label=?3,trashed=?4 WHERE id=?5",
            )?;
            for d in decisions {
                if stmt.execute(params![
                    d.rating,
                    d.flag.as_str(),
                    d.label.as_str(),
                    d.trashed,
                    d.id
                ])? == 0
                {
                    bail!("Photo {} does not exist", d.id);
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn set_dhash(&self, id: i64, hash: u64) -> Result<()> {
        self.set_dhashes(&[(id, hash)])
    }
    pub fn set_visual_hash(&self, id: i64, hash: u64) -> Result<()> {
        self.set_dhashes(&[(id, hash)])
    }
    /// Rebuildable metadata shares one commit per batch and never changes decision
    /// columns. Missing IDs from an obsolete snapshot are harmless stale results.
    pub fn set_dhashes(&self, hashes: &[(i64, u64)]) -> Result<()> {
        if hashes.is_empty() {
            return Ok(());
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = conn.transaction()?;
        {
            let mut select = tx.prepare_cached("SELECT data FROM photos WHERE id=?1")?;
            let mut update = tx.prepare_cached("UPDATE photos SET data=?1 WHERE id=?2")?;
            for &(id, hash) in hashes {
                let data: String = match select.query_row([id], |row| row.get(0)) {
                    Ok(data) => data,
                    Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                    Err(error) => return Err(error.into()),
                };
                // Patch the JSON object rather than reconstructing Photo: retain
                // metadata fields introduced by a newer application version.
                let mut metadata: serde_json::Value = serde_json::from_str(&data)?;
                metadata
                    .as_object_mut()
                    .context("Photo metadata is not an object")?
                    .insert("dhash".into(), serde_json::Value::from(hash));
                update.execute(params![serde_json::to_string(&metadata)?, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn export_xmp(&self, ids: &[i64], destination: &Path) -> Result<usize> {
        self.prepare_export(destination, false, ids)?.execute()
    }
    pub fn export_picks(&self, destination: &Path) -> Result<usize> {
        self.prepare_export(destination, true, &[])?.execute()
    }
}

// SQLite owns these bytes until rows.next(): deserialize in place instead of
// allocating a temporary JSON String and two decision Strings for every row.
fn photo_from_row(row: &rusqlite::Row<'_>) -> Result<Photo> {
    let mut photo: Photo = serde_json::from_str(row.get_ref(1)?.as_str()?)?;
    photo.id = row.get(0)?;
    photo.rating = row.get(2)?;
    photo.flag = Flag::from(row.get_ref(3)?.as_str()?);
    photo.label = ColorLabel::from(row.get_ref(4)?.as_str()?);
    photo.trashed = row.get(5)?;
    Ok(photo)
}

/// A decision snapshot that is safe to move to a dedicated export worker.
pub struct ExportPlan {
    photos: Vec<Photo>,
    destination: PathBuf,
    picks: bool,
    source_roots: Vec<PathBuf>,
}
/// Machine-readable partial completion details carried through anyhow errors.
#[derive(Debug)]
pub struct ExportFailure {
    pub completed: usize,
    pub manifest_path: PathBuf,
    pub message: String,
}
impl std::fmt::Display for ExportFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; manifest: {}",
            self.message,
            self.manifest_path.display()
        )
    }
}
impl std::error::Error for ExportFailure {}
impl ExportPlan {
    pub fn execute(self) -> Result<usize> {
        for photo in &self.photos {
            photo.require_local("Export and XMP writing")?;
        }
        let destination = &self.destination;
        validate_destination(&self.photos, destination)?;
        let resolved = resolved_path(destination)?;
        if self
            .source_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            bail!("Export destination must be outside imported source roots");
        }
        if self.picks {
            self.copy_picks()
        } else {
            self.write_xmp()
        }
    }
    fn write_xmp(&self) -> Result<usize> {
        let destination = &self.destination;
        fs::create_dir_all(destination)?;
        let manifest_path = reserve_manifest(destination, "shadowroom-xmp-manifest.json", "xmp")?;
        let mut count = 0;
        let mut manifest = Vec::new();
        let mut failures = 0;
        for p in &self.photos {
            let stem = p.path.file_stem().unwrap_or_default().to_string_lossy();
            let path = destination.join(format!("{stem}.xmp"));
            let rating = if p.flag == Flag::Reject {
                -1
            } else {
                i32::from(p.rating)
            };
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:shadowroom=\"https://shadowroom.app/ns/1.0/\" xmp:Rating=\"{rating}\" xmp:Label=\"{}\" shadowroom:Flag=\"{}\" shadowroom:PriorRating=\"{}\"/></rdf:RDF></x:xmpmeta>\n",
                p.label.as_str(),
                p.flag.as_str(),
                p.rating,
            );
            match write_unique(&path, xml.as_bytes()) {
                Ok(written) => {
                    manifest.push(serde_json::json!({"photo_id":p.id,"source":p.path,"sidecar":written.file_name().map(|n|n.to_string_lossy().into_owned()),"status":"written","bytes":xml.len(),"blake3":blake3::hash(xml.as_bytes()).to_hex().as_str()}));
                    count += 1;
                }
                Err(error) => {
                    failures += 1;
                    manifest.push(serde_json::json!({"photo_id":p.id,"source":p.path,"sidecar":null,"status":"failed","error":format!("{error:#}")}));
                }
            }
        }
        persist_manifest(&manifest_path, &manifest).with_context(|| ExportFailure {
            completed: count,
            manifest_path: manifest_path.clone(),
            message: format!("{count} sidecars completed; manifest could not be finalized"),
        })?;
        if failures > 0 {
            return Err(ExportFailure {
                completed: count,
                manifest_path,
                message: format!("XMP export completed {count} sidecars; {failures} failed"),
            }
            .into());
        }
        Ok(count)
    }
    fn copy_picks(&self) -> Result<usize> {
        let destination = &self.destination;
        fs::create_dir_all(destination)?;
        let manifest_path =
            reserve_manifest(destination, "shadowroom-picks-manifest.json", "picks")?;
        let mut report = PicksReport {
            schema_version: 1,
            state: "completed".into(),
            completed_files: 0,
            completed_groups: 0,
            failed_groups: 0,
            groups: Vec::new(),
        };
        let mut sidecars = SidecarCache::default();
        for p in &self.photos {
            let group = copy_group(p, destination, &mut sidecars);
            if group.status == "verified" {
                report.completed_groups += 1;
                report.completed_files += group.files.len();
            } else {
                report.failed_groups += 1;
            }
            report.groups.push(group);
        }
        if report.failed_groups > 0 {
            report.state = "partial_failure".into();
        }
        persist_manifest(&manifest_path, &report).with_context(|| ExportFailure {
            completed: report.completed_files,
            manifest_path: manifest_path.clone(),
            message: format!(
                "{} files completed; manifest could not be finalized",
                report.completed_files
            ),
        })?;
        if report.failed_groups > 0 {
            return Err(ExportFailure {
                completed: report.completed_files,
                manifest_path,
                message: format!(
                    "Picks export completed {} verified files in {} groups; {} groups failed",
                    report.completed_files, report.completed_groups, report.failed_groups
                ),
            }
            .into());
        }
        Ok(report.completed_files)
    }
}

#[derive(Serialize)]
struct PicksReport {
    schema_version: u32,
    state: String,
    completed_files: usize,
    completed_groups: usize,
    failed_groups: usize,
    groups: Vec<GroupReport>,
}
#[derive(Serialize)]
struct GroupReport {
    photo_id: i64,
    original: PathBuf,
    basename: Option<String>,
    status: String,
    error: Option<String>,
    files: Vec<FileReport>,
}
#[derive(Serialize)]
struct FileReport {
    source: PathBuf,
    destination: Option<PathBuf>,
    kind: String,
    status: String,
    bytes: u64,
    blake3: Option<String>,
    output_retained: bool,
    error: Option<String>,
}
struct GroupMember {
    source: PathBuf,
    suffix: OsString,
    kind: &'static str,
}
struct ReservedOutput {
    path: PathBuf,
    file: Option<fs::File>,
}

#[derive(Default)]
struct SidecarCache {
    directories: HashMap<PathBuf, HashMap<Vec<u8>, Vec<PathBuf>>>,
}
impl SidecarCache {
    fn related(&mut self, media: &Path) -> Result<Vec<(PathBuf, OsString)>> {
        let parent = media.parent().context("Source has no parent directory")?;
        if !self.directories.contains_key(parent) {
            let mut entries: HashMap<Vec<u8>, Vec<PathBuf>> = HashMap::new();
            for entry in fs::read_dir(parent)
                .with_context(|| format!("Discover sidecars in {}", parent.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("xmp") || ext.eq_ignore_ascii_case("aae")
                }) {
                    let kind = entry.file_type()?;
                    if kind.is_file() || kind.is_symlink() {
                        entries
                            .entry(
                                path.file_name()
                                    .unwrap()
                                    .as_encoded_bytes()
                                    .to_ascii_lowercase(),
                            )
                            .or_default()
                            .push(path);
                    }
                }
            }
            for paths in entries.values_mut() {
                paths.sort();
            }
            self.directories.insert(parent.into(), entries);
        }
        let entries = &self.directories[parent];
        let stem = media.file_stem().context("Source has no stem")?;
        let filename = media.file_name().context("Source has no filename")?;
        let mut result = Vec::new();
        for base in [stem, filename] {
            for ext in ["xmp", "aae"] {
                let mut candidate = base.to_os_string();
                candidate.push(".");
                candidate.push(ext);
                if let Some(matches) =
                    entries.get(&candidate.as_encoded_bytes().to_ascii_lowercase())
                {
                    for path in matches {
                        let mut suffix = OsString::new();
                        if base == filename && filename != stem {
                            suffix.push(".");
                            suffix.push(media.extension().unwrap_or_default());
                        }
                        suffix.push(".");
                        suffix.push(path.extension().unwrap_or_default());
                        result.push((path.clone(), suffix));
                    }
                }
            }
        }
        Ok(result)
    }
}

fn group_members(photo: &Photo, cache: &mut SidecarCache) -> Result<Vec<GroupMember>> {
    let mut members = Vec::new();
    let mut seen = HashSet::new();
    for (index, media) in std::iter::once(&photo.path)
        .chain(photo.companion_path.iter())
        .enumerate()
    {
        if seen.insert(media.clone()) {
            let mut suffix = OsString::new();
            if let Some(ext) = media.extension() {
                suffix.push(".");
                suffix.push(ext);
            }
            members.push(GroupMember {
                source: media.clone(),
                suffix,
                kind: if index == 0 { "original" } else { "companion" },
            });
        }
    }
    for media in std::iter::once(&photo.path).chain(photo.companion_path.iter()) {
        for (source, suffix) in cache.related(media)? {
            if seen.insert(source.clone()) {
                members.push(GroupMember {
                    source,
                    suffix,
                    kind: "sidecar",
                });
            }
        }
    }
    // Case-insensitive suffix uniqueness also works on case-folding destinations.
    let mut suffixes = HashSet::new();
    for member in &mut members {
        if !suffixes.insert(member.suffix.as_encoded_bytes().to_ascii_lowercase()) {
            let original = member.suffix.clone();
            for alternative in 1..100_000 {
                let mut suffix = OsString::from(format!(".alternate-{alternative}"));
                suffix.push(&original);
                if suffixes.insert(suffix.as_encoded_bytes().to_ascii_lowercase()) {
                    member.suffix = suffix;
                    break;
                }
            }
        }
    }
    Ok(members)
}

fn copy_group(photo: &Photo, destination: &Path, cache: &mut SidecarCache) -> GroupReport {
    let mut report = GroupReport {
        photo_id: photo.id,
        original: photo.path.clone(),
        basename: None,
        status: "failed".into(),
        error: None,
        files: Vec::new(),
    };
    let members = match group_members(photo, cache) {
        Ok(members) => members,
        Err(error) => {
            report.error = Some(format!("{error:#}"));
            report.files = std::iter::once(&photo.path)
                .chain(photo.companion_path.iter())
                .map(|source| FileReport {
                    source: source.clone(),
                    destination: None,
                    kind: "media".into(),
                    status: "not_attempted".into(),
                    bytes: 0,
                    blake3: None,
                    output_retained: false,
                    error: None,
                })
                .collect();
            return report;
        }
    };
    report.files = members
        .iter()
        .map(|m| FileReport {
            source: m.source.clone(),
            destination: None,
            kind: m.kind.into(),
            status: "not_attempted".into(),
            bytes: 0,
            blake3: None,
            output_retained: false,
            error: None,
        })
        .collect();
    let stem = photo
        .path
        .file_stem()
        .unwrap_or_else(|| OsStr::new("photo"));
    let (basename, mut outputs) = match reserve_group(destination, stem, &members) {
        Ok(reserved) => reserved,
        Err(error) => {
            report.error = Some(format!("{error:#}"));
            return report;
        }
    };
    report.basename = Some(basename.to_string_lossy().into_owned());
    for (file, output) in report.files.iter_mut().zip(&outputs) {
        file.destination = Some(output.path.clone());
    }
    let mut failure = None;
    for (index, (member, output)) in members.iter().zip(&mut outputs).enumerate() {
        match copy_verified(&member.source, &output.path, output.file.as_mut().unwrap()) {
            Ok((bytes, hash)) => {
                let file = &mut report.files[index];
                file.status = "verified".into();
                file.bytes = bytes;
                file.blake3 = Some(hash);
                file.output_retained = true;
            }
            Err(error) => {
                let message = format!("{}: {error:#}", member.source.display());
                report.files[index].status = "failed".into();
                report.files[index].error = Some(message.clone());
                failure = Some(message);
                break;
            }
        }
    }
    if let Some(error) = failure {
        let cleanup = rollback_outputs(&mut outputs);
        for file in &mut report.files {
            file.output_retained = false;
            if file.status == "verified" {
                file.status = "rolled_back".into();
            }
            if let Some(path) = &file.destination {
                if let Some(message) = cleanup
                    .iter()
                    .find(|(failed, _)| failed == path)
                    .map(|(_, message)| message)
                {
                    file.status = "cleanup_failed".into();
                    file.output_retained = true;
                    file.error = Some(message.clone());
                }
            }
        }
        report.error = Some(if cleanup.is_empty() {
            error
        } else {
            format!(
                "{error}; {} output cleanup operations failed",
                cleanup.len()
            )
        });
    } else {
        report.status = "verified".into();
    }
    report
}

fn reserve_group(
    destination: &Path,
    stem: &OsStr,
    members: &[GroupMember],
) -> Result<(OsString, Vec<ReservedOutput>)> {
    for number in 0..100_000 {
        let mut basename = stem.to_os_string();
        if number > 0 {
            basename.push(format!("-{number}"));
        }
        let mut outputs = Vec::new();
        let mut collision = false;
        for member in members {
            let mut filename = basename.clone();
            filename.push(&member.suffix);
            let path = destination.join(filename);
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => outputs.push(ReservedOutput {
                    path,
                    file: Some(file),
                }),
                Err(error) => {
                    let cleanup = rollback_outputs(&mut outputs);
                    if !cleanup.is_empty() {
                        bail!(
                            "Could not clean {} own reservations after collision/error",
                            cleanup.len()
                        );
                    }
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        collision = true;
                        break;
                    }
                    return Err(error).with_context(|| format!("Reserve {}", path.display()));
                }
            }
        }
        if !collision {
            return Ok((basename, outputs));
        }
    }
    bail!("Too many filename collisions for export group")
}

fn rollback_outputs(outputs: &mut [ReservedOutput]) -> Vec<(PathBuf, String)> {
    let mut failures = Vec::new();
    for output in outputs {
        let owned = output.file.take();
        let identity = owned.as_ref().and_then(|file| file.metadata().ok());
        let current = fs::symlink_metadata(&output.path);
        let matches = match (&identity, &current) {
            (Some(original), Ok(current)) => same_file(original, current),
            (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            _ => false,
        };
        drop(owned);
        if !matches {
            failures.push((
                output.path.clone(),
                "Reserved output changed; left untouched".into(),
            ));
            continue;
        }
        if let Err(error) = fs::remove_file(&output.path) {
            failures.push((output.path.clone(), error.to_string()));
        }
    }
    failures
}
#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}
#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.created()
            .ok()
            .is_some_and(|time| b.created().ok() == Some(time))
        && a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
}

fn copy_verified(source: &Path, target: &Path, output: &mut fs::File) -> Result<(u64, String)> {
    copy_verified_observed(source, target, output, || Ok(()))
}

fn same_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    let unchanged = same_file(before, after)
        && before.len() == after.len()
        && before.modified().ok() == after.modified().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unchanged && before.ctime() == after.ctime() && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        unchanged
    }
}

fn copy_verified_observed(
    source: &Path,
    target: &Path,
    output: &mut fs::File,
    mut after_chunk: impl FnMut() -> Result<()>,
) -> Result<(u64, String)> {
    let mut input = crate::fsutil::open_regular(source)?;
    let before = input.metadata()?;
    if !before.is_file() {
        bail!("Source is not a regular file");
    }
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let remaining = before.len().saturating_sub(bytes);
        let length = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = input.read(&mut buffer[..length])?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            bail!("Source grew while copying");
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes += read as u64;
        after_chunk()?;
    }
    let after = input.metadata()?;
    if bytes != before.len()
        || !same_snapshot(&before, &after)
        || !same_snapshot(&before, &fs::symlink_metadata(source)?)
    {
        bail!("Source changed while copying");
    }
    output.sync_all()?;
    let hash = hasher.finalize();
    verify_file(target, bytes, &hash)?;
    Ok((bytes, hash.to_hex().to_string()))
}
pub(crate) fn verify_file(
    path: &Path,
    expected_bytes: u64,
    expected_hash: &blake3::Hash,
) -> Result<()> {
    let file = crate::fsutil::open_regular(path)?;
    if file.metadata()?.len() != expected_bytes {
        bail!("Export verification size mismatch for {}", path.display());
    }
    let mut file = file.take(expected_bytes.saturating_add(1));
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        bytes += read as u64;
    }
    if bytes != expected_bytes || hash.finalize() != *expected_hash {
        bail!("Export verification failed for {}", path.display());
    }
    Ok(())
}

fn reserve_manifest(destination: &Path, name: &str, mode: &str) -> Result<PathBuf> {
    let (path, mut file) = unique_file(&destination.join(name))?;
    let initial = serde_json::to_vec_pretty(
        &serde_json::json!({"schema_version":1,"state":"running","mode":mode,"note":"Export in progress; final per-file results replace this record on completion."}),
    )?;
    if let Err(error) = file.write_all(&initial).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error).context("Initialize export manifest");
    }
    Ok(path)
}
fn persist_manifest(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    crate::settings::atomic_write(path, &bytes)
}

fn validate_destination(photos: &[Photo], destination: &Path) -> Result<()> {
    // Resolve existing ancestors so symlinks and not-yet-created subdirectories cannot bypass the check.
    let destination = resolved_path(destination)?;
    let folders: std::collections::HashSet<_> = photos
        .iter()
        .flat_map(|p| std::iter::once(&p.path).chain(p.companion_path.iter()))
        .filter_map(|path| path.parent())
        .collect();
    for parent in folders {
        let parent = resolved_path(parent)?;
        if destination.starts_with(parent) {
            bail!("Export destination must be outside original photo folders");
        }
    }
    Ok(())
}
fn resolved_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    // Resolve in component order: lexical normalization alone is incorrect for
    // `symlink/..`, but retaining missing `new/../source` permits guard bypasses.
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                result.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::Normal(part) => {
                result.push(part);
                match fs::symlink_metadata(&result) {
                    Ok(_) => result = result.canonicalize()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("Resolve {}", result.display()));
                    }
                }
            }
        }
    }
    Ok(result)
}
fn unique_file(path: &Path) -> Result<(PathBuf, fs::File)> {
    for n in 0..100_000 {
        let candidate = if n == 0 {
            path.to_path_buf()
        } else {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let ext = path
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            path.with_file_name(format!("{stem}-{n}{ext}"))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("Too many filename collisions")
}
fn write_unique(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let (path, mut file) = unique_file(path)?;
    if let Err(err) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(err.into());
    }
    if let Err(error) = verify_file(&path, bytes.len() as u64, &blake3::hash(bytes)) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn catalog_performance_harness() {
        use std::time::Instant;
        let dir = tempfile::tempdir_in(
            std::env::var_os("SHADOWROOM_INDEX_BENCH_DIR").unwrap_or_else(|| "/tmp".into()),
        )
        .unwrap();
        let path = dir.path().join("catalog.db");
        let mut catalog = Catalog::open(&path).unwrap();
        let mut start = 0;
        for count in [10_000, 100_000] {
            let photos: Vec<_> = (start..count)
                .map(|index| Photo {
                    path: format!("/bench/shoot/folder{:03}/frame{index:06}.nef", index / 100)
                        .into(),
                    filename: format!("frame{index:06}.nef"),
                    folder: format!("/bench/shoot/folder{:03}", index / 100),
                    camera: "Bench camera".into(),
                    lens: "Bench lens".into(),
                    width: 6000,
                    height: 4000,
                    modified_at: 1704067200000 + index,
                    captured_at: Some(1704067200000 + index),
                    size_bytes: 60 * 1024 * 1024,
                    metadata_version: 2,
                    ..Default::default()
                })
                .collect();
            let started = Instant::now();
            for batch in photos.chunks(1024) {
                catalog.upsert_photos(batch).unwrap();
            }
            eprintln!(
                "BENCH catalog insert {} rows: {:?}",
                count - start,
                started.elapsed()
            );
            drop(photos);
            drop(catalog);
            let started = Instant::now();
            catalog = Catalog::open(&path).unwrap();
            eprintln!("BENCH catalog open {count}: {:?}", started.elapsed());
            for pass in 0..3 {
                let started = Instant::now();
                let photos = catalog.photos().unwrap();
                eprintln!(
                    "BENCH catalog load {count} pass={pass}: {:?}",
                    started.elapsed()
                );
                assert_eq!(photos.len(), count as usize);
                std::hint::black_box(photos);
                let started = Instant::now();
                let prints = catalog.fingerprints(Path::new("/bench/shoot")).unwrap();
                eprintln!(
                    "BENCH fingerprints {count} pass={pass}: {:?}",
                    started.elapsed()
                );
                assert_eq!(prints.len(), count as usize);
            }
            if count == 100_000 {
                catalog.set_flag(1, Flag::Pick).unwrap();
                let started = Instant::now();
                for _ in 0..10 {
                    assert_eq!(
                        catalog
                            .prepare_export(Path::new("/unused"), false, &[1])
                            .unwrap()
                            .photos
                            .len(),
                        1
                    );
                }
                eprintln!(
                    "BENCH 100k catalog 10 single-ID export plans: {:?}",
                    started.elapsed()
                );
                let started = Instant::now();
                for _ in 0..10 {
                    assert_eq!(
                        catalog
                            .prepare_export(Path::new("/unused"), true, &[])
                            .unwrap()
                            .photos
                            .len(),
                        1
                    );
                }
                eprintln!(
                    "BENCH 100k catalog 10 one-pick export plans: {:?}",
                    started.elapsed()
                );
                for mmap in [0, 268_435_456, 0, 268_435_456] {
                    catalog
                        .connection
                        .lock()
                        .unwrap()
                        .pragma_update(None, "mmap_size", mmap)
                        .unwrap();
                    let started = Instant::now();
                    for _ in 0..3 {
                        std::hint::black_box(catalog.photos().unwrap());
                        std::hint::black_box(
                            catalog.fingerprints(Path::new("/bench/shoot")).unwrap(),
                        );
                    }
                    eprintln!(
                        "BENCH catalog mmap={mmap}, 3x(load+fingerprints): {:?}",
                        started.elapsed()
                    );
                }
            }
            start = count;
        }
    }
    use super::*;
    #[test]
    fn nonexistent_parent_traversal_cannot_bypass_source_destination_guard() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a.jpg"), b"original").unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: source.join("a.jpg"),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        catalog.set_flag(id, Flag::Pick).unwrap();
        let sneaky = dir.path().join("not-yet-created/../source/export");
        assert!(catalog.export_picks(&sneaky).is_err());
        assert!(!dir.path().join("not-yet-created").exists());
        assert!(!source.join("export").exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_then_parent_uses_actual_target_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("source/nested");
        fs::create_dir_all(&nested).unwrap();
        let links = dir.path().join("links");
        fs::create_dir(&links).unwrap();
        std::os::unix::fs::symlink(&nested, links.join("alias")).unwrap();
        assert_eq!(
            resolved_path(&links.join("alias/../new")).unwrap(),
            dir.path().canonicalize().unwrap().join("source/new")
        );
    }
    #[test]
    fn paired_export_reserves_one_collision_suffix_and_verifies_existing_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("out");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for (name, bytes) in [
            ("capture.nef", b"raw".as_slice()),
            ("capture.jpg", b"jpeg"),
            ("capture.XMP", b"adobe"),
            ("capture.nef.xmp", b"darktable raw"),
            ("capture.jpg.xmp", b"darktable jpeg"),
            ("capture.AAE", b"apple edits"),
        ] {
            fs::write(source.join(name), bytes).unwrap();
        }
        // Colliding with the second reserved member must also remove the first
        // tentative reservation and choose one suffix for every group member.
        fs::write(destination.join("capture.jpg"), b"existing destination").unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: source.join("capture.nef"),
                companion_path: Some(source.join("capture.jpg")),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        catalog.set_flag(id, Flag::Pick).unwrap();
        assert_eq!(catalog.export_picks(&destination).unwrap(), 6);
        assert!(!destination.join("capture.nef").exists());
        assert_eq!(
            fs::read(destination.join("capture.jpg")).unwrap(),
            b"existing destination"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(destination.join("shadowroom-picks-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["completed_files"], 6);
        assert_eq!(manifest["groups"][0]["basename"], "capture-1");
        for file in manifest["groups"][0]["files"].as_array().unwrap() {
            let original = Path::new(file["source"].as_str().unwrap());
            let output = Path::new(file["destination"].as_str().unwrap());
            assert!(
                output
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("capture-1.")
            );
            let bytes = fs::read(original).unwrap();
            assert_eq!(fs::read(output).unwrap(), bytes);
            assert_eq!(file["blake3"], blake3::hash(&bytes).to_hex().as_str());
            assert_eq!(file["status"], "verified");
        }
        assert_eq!(
            fs::read(destination.join("capture-1.nef.xmp")).unwrap(),
            b"darktable raw"
        );
        assert_eq!(fs::read(source.join("capture.nef")).unwrap(), b"raw");
        // A repeat exports a fresh coordinated group, preserving its old report.
        assert_eq!(catalog.export_picks(&destination).unwrap(), 6);
        assert!(destination.join("capture-2.nef").exists());
        assert!(
            destination
                .join("shadowroom-picks-manifest-1.json")
                .exists()
        );
    }
    #[test]
    fn failing_companion_rolls_back_only_its_group_and_reports_partial_success() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("out");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("good.jpg"), b"good").unwrap();
        fs::write(source.join("broken.nef"), b"raw").unwrap();
        fs::write(destination.join("untouched.txt"), b"keep").unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog")).unwrap();
        catalog
            .upsert_photos(&[
                Photo {
                    path: source.join("good.jpg"),
                    ..Photo::default()
                },
                Photo {
                    path: source.join("broken.nef"),
                    companion_path: Some(source.join("broken.jpg")),
                    ..Photo::default()
                },
            ])
            .unwrap();
        for photo in catalog.photos().unwrap() {
            catalog.set_flag(photo.id, Flag::Pick).unwrap();
        }
        let error = format!("{:#}", catalog.export_picks(&destination).unwrap_err());
        assert!(error.contains("1 verified files"));
        assert!(error.contains("manifest:"));
        assert_eq!(fs::read(destination.join("good.jpg")).unwrap(), b"good");
        assert_eq!(
            fs::read(destination.join("untouched.txt")).unwrap(),
            b"keep"
        );
        assert!(!destination.join("broken.nef").exists());
        assert!(!destination.join("broken.jpg").exists());
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(destination.join("shadowroom-picks-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["completed_groups"], 1);
        assert_eq!(manifest["failed_groups"], 1);
        let failed = manifest["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["status"] == "failed")
            .unwrap();
        assert_eq!(failed["files"][0]["status"], "rolled_back");
        assert_eq!(failed["files"][1]["status"], "failed");
        assert!(
            failed["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["output_retained"] == false)
        );
        assert_eq!(fs::read(source.join("broken.nef")).unwrap(), b"raw");
    }
    #[test]
    fn destination_verification_rejects_modified_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("copy.jpg");
        fs::write(&output, b"good").unwrap();
        let hash = blake3::hash(b"good");
        verify_file(&output, 4, &hash).unwrap();
        fs::write(&output, b"evil").unwrap();
        assert!(verify_file(&output, 4, &hash).is_err());
        fs::write(&output, b"good plus extra").unwrap();
        assert!(verify_file(&output, 4, &hash).is_err());
    }
    #[test]
    fn export_copy_stops_at_its_source_snapshot_when_the_source_keeps_growing() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("growing.jpg");
        let target = dir.path().join("copy.jpg");
        fs::write(&source, vec![7; 64 * 1024]).unwrap();
        let mut output = fs::File::create(&target).unwrap();
        let mut chunks = 0;
        let result = copy_verified_observed(&source, &target, &mut output, || {
            chunks += 1;
            OpenOptions::new()
                .append(true)
                .open(&source)?
                .write_all(b"more")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(chunks, 1);
        assert_eq!(output.metadata().unwrap().len(), 64 * 1024);
    }
    #[test]
    fn export_copy_rejects_path_replacement_with_the_same_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jpg");
        let target = dir.path().join("copy.jpg");
        fs::write(&source, b"old").unwrap();
        let old_time = fs::metadata(&source).unwrap().modified().unwrap();
        let mut output = fs::File::create(&target).unwrap();
        let result = copy_verified_observed(&source, &target, &mut output, || {
            fs::rename(&source, dir.path().join("previous"))?;
            fs::write(&source, b"new")?;
            fs::File::options()
                .write(true)
                .open(&source)?
                .set_times(fs::FileTimes::new().set_modified(old_time))?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(source).unwrap(), b"new");
    }
    #[test]
    fn rejected_xmp_preserves_prior_stars() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: source.join("a.nef"),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        catalog.set_rating(id, 4).unwrap();
        catalog.set_flag(id, Flag::Reject).unwrap();
        let destination = dir.path().join("out");
        catalog.export_xmp(&[id], &destination).unwrap();
        let xml = fs::read_to_string(destination.join("a.xmp")).unwrap();
        assert!(xml.contains("xmp:Rating=\"-1\""));
        assert!(xml.contains("shadowroom:PriorRating=\"4\""));
    }
    #[test]
    fn decisions_survive_reindex_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("catalog.db");
        let c = Catalog::open(&db).unwrap();
        let mut p = Photo {
            path: dir.path().join("image.jpg"),
            ..Photo::default()
        };
        c.upsert_photos(&[p.clone()]).unwrap();
        let id = c.photos().unwrap()[0].id;
        c.set_rating(id, 5).unwrap();
        c.set_flag(id, Flag::Pick).unwrap();
        p.width = 4000;
        c.upsert_photos(&[p]).unwrap();
        c.flush().unwrap();
        drop(c);
        let c = Catalog::open(&db).unwrap();
        let p = c.photos().unwrap().remove(0);
        assert_eq!(p.rating, 5);
        assert_eq!(p.flag, Flag::Pick);
        assert_eq!(p.width, 4000);
    }
    #[test]
    fn export_queries_preserve_selection_order_deduplication_and_trash_filter() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        catalog
            .upsert_photos(
                &(0..3)
                    .map(|index| Photo {
                        path: format!("/source/{index}.jpg").into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let mut decisions: Vec<_> = catalog
            .photos()
            .unwrap()
            .iter()
            .map(PhotoDecision::from)
            .collect();
        decisions[1].trashed = true;
        decisions[1].flag = Flag::Pick;
        decisions[2].flag = Flag::Pick;
        catalog.set_decisions(&decisions).unwrap();
        let ids: Vec<_> = decisions.iter().map(|d| d.id).collect();
        let plan = catalog
            .prepare_export(
                Path::new("/unused"),
                false,
                &[ids[2], ids[1], ids[0], ids[2], i64::MAX],
            )
            .unwrap();
        assert_eq!(
            plan.photos.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![ids[0], ids[2]]
        );
        assert_eq!(plan.photos[1].flag, Flag::Pick);
        let picks = catalog
            .prepare_export(Path::new("/unused"), true, &[ids[0]])
            .unwrap();
        assert_eq!(
            picks.photos.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![ids[2]]
        );
        assert!(
            catalog
                .prepare_export(Path::new("/unused"), false, &[])
                .unwrap()
                .photos
                .is_empty()
        );
    }
    #[test]
    fn export_snapshot_and_duplicate_sidecar_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let c = Catalog::open(&dir.path().join("db")).unwrap();
        for folder in ["first", "second"] {
            fs::create_dir(dir.path().join(folder)).unwrap();
            let path = dir.path().join(folder).join("same.jpg");
            fs::write(&path, b"original").unwrap();
            c.upsert_photos(&[Photo {
                path,
                ..Default::default()
            }])
            .unwrap();
        }
        let photos = c.photos().unwrap();
        let ids: Vec<_> = photos.iter().map(|p| p.id).collect();
        for id in &ids {
            c.set_flag(*id, Flag::Pick).unwrap();
        }
        let picks = c
            .prepare_export(&dir.path().join("picks"), true, &[])
            .unwrap();
        for id in &ids {
            c.set_flag(*id, Flag::Reject).unwrap();
        }
        assert_eq!(picks.execute().unwrap(), 2);
        let out = dir.path().join("xmp");
        assert_eq!(c.export_xmp(&ids, &out).unwrap(), 2);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("shadowroom-xmp-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.as_array().unwrap().len(), 2);
        assert_ne!(manifest[0]["sidecar"], manifest[1]["sidecar"]);
        assert_eq!(
            c.connection
                .lock()
                .unwrap()
                .query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        c.flush().unwrap();
    }
    #[test]
    fn incremental_readset_is_scoped_to_source() {
        let dir = tempfile::tempdir().unwrap();
        let c = Catalog::open(&dir.path().join("db")).unwrap();
        for path in ["/photos/a.jpg", "/photos/sub/β.jpg", "/photosevil/b.jpg"] {
            c.upsert_photos(&[Photo {
                path: PathBuf::from(path),
                modified_at: 123,
                companion_path: Some(PathBuf::from("/photos/a.jpeg")),
                ..Default::default()
            }])
            .unwrap();
        }
        let fingerprints = c.fingerprints(Path::new("/photos")).unwrap();
        assert_eq!(fingerprints.len(), 2);
        assert_eq!(
            fingerprints[Path::new("/photos/sub/β.jpg")].modified_at,
            123
        );
        assert_eq!(
            fingerprints[Path::new("/photos/a.jpg")].companion_path,
            Some(PathBuf::from("/photos/a.jpeg"))
        );
    }
    #[test]
    fn jpeg_to_raw_promotion_preserves_id_and_history_writes() {
        use crate::indexer::{IndexOptions, Indexer};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        fs::create_dir(&root).unwrap();
        let jpeg = root.join("frame.jpg");
        let raw = root.join("frame.nef");
        fs::write(&jpeg, b"jpeg").unwrap();
        let db = dir.path().join("catalog.db");
        let catalog = Catalog::open(&db).unwrap();
        let options = IndexOptions {
            read_exif: false,
            ..Default::default()
        };
        Indexer::start(catalog.clone(), root.clone(), options.clone())
            .join()
            .unwrap();
        let original = catalog.photos().unwrap().remove(0);
        let before = PhotoDecision::from(&original);
        let rated = PhotoDecision {
            rating: 4,
            flag: Flag::Pick,
            label: ColorLabel::Blue,
            trashed: true,
            ..before
        };
        catalog.set_decisions(&[rated]).unwrap();
        fs::write(&raw, b"raw").unwrap();
        Indexer::start(catalog.clone(), root, options)
            .join()
            .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 1);
        let paired = &photos[0];
        assert_eq!(paired.id, original.id);
        assert_eq!(paired.path, fs::canonicalize(&raw).unwrap());
        assert_eq!(
            paired.companion_path,
            Some(fs::canonicalize(&jpeg).unwrap())
        );
        assert_eq!(PhotoDecision::from(paired), rated);
        // Undo and a delayed outbox write captured before import both still work.
        catalog.set_decisions(&[before]).unwrap();
        assert_eq!(PhotoDecision::from(&catalog.photos().unwrap()[0]), before);
        catalog.set_decisions(&[rated]).unwrap();
        drop(catalog);
        let reopened = Catalog::open(&db).unwrap();
        assert_eq!(PhotoDecision::from(&reopened.photos().unwrap()[0]), rated);
    }
    #[test]
    fn old_catalog_migrates_trash_and_reindex_exports_preserve_the_decision() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("old.sqlite3");
        let source = directory.path().join("original.jpg");
        fs::write(&source, b"unchanged original").unwrap();
        let old = Connection::open(&database).unwrap();
        old.execute_batch("CREATE TABLE photos(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,modified_at INTEGER NOT NULL,size_bytes INTEGER NOT NULL,rating INTEGER NOT NULL DEFAULT 0,flag TEXT NOT NULL DEFAULT 'none',label TEXT NOT NULL DEFAULT '',data TEXT NOT NULL);").unwrap();
        let photo = Photo {
            path: source.clone(),
            ..Default::default()
        };
        old.execute(
            "INSERT INTO photos(id,path,modified_at,size_bytes,data) VALUES(77,?1,0,18,?2)",
            params![
                source.to_string_lossy(),
                serde_json::to_string(&photo).unwrap()
            ],
        )
        .unwrap();
        drop(old);
        let catalog = Catalog::open(&database).unwrap();
        let before = PhotoDecision::from(&catalog.photos().unwrap()[0]);
        assert_eq!(before.id, 77);
        assert!(!before.trashed);
        let trashed = PhotoDecision {
            flag: Flag::Pick,
            rating: 5,
            trashed: true,
            ..before
        };
        catalog.set_decisions(&[trashed]).unwrap();
        catalog.upsert_photos(&[photo]).unwrap();
        assert_eq!(PhotoDecision::from(&catalog.photos().unwrap()[0]), trashed);
        assert_eq!(
            catalog
                .export_picks(&directory.path().join("copies"))
                .unwrap(),
            0
        );
        assert_eq!(
            catalog
                .export_xmp(&[77], &directory.path().join("xmp"))
                .unwrap(),
            0
        );
        drop(catalog);
        let reopened = Catalog::open(&database).unwrap();
        assert_eq!(PhotoDecision::from(&reopened.photos().unwrap()[0]), trashed);
        reopened.set_decisions(&[before]).unwrap();
        assert!(!reopened.photos().unwrap()[0].trashed);
        assert_eq!(fs::read(source).unwrap(), b"unchanged original");
    }
    #[test]
    fn distinct_raw_and_jpeg_ids_are_never_deleted_or_decision_merged() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        let raw = dir.path().join("frame.nef");
        let jpeg = dir.path().join("frame.jpg");
        catalog
            .upsert_photos(&[
                Photo {
                    path: raw.clone(),
                    ..Default::default()
                },
                Photo {
                    path: jpeg.clone(),
                    ..Default::default()
                },
            ])
            .unwrap();
        let photos = catalog.photos().unwrap();
        let raw_id = photos.iter().find(|p| p.path == raw).unwrap().id;
        let jpeg_id = photos.iter().find(|p| p.path == jpeg).unwrap().id;
        let decisions = [
            PhotoDecision {
                id: raw_id,
                rating: 5,
                flag: Flag::Pick,
                label: ColorLabel::Blue,
                trashed: false,
            },
            PhotoDecision {
                id: jpeg_id,
                rating: 1,
                flag: Flag::Reject,
                label: ColorLabel::Green,
                trashed: false,
            },
        ];
        catalog.set_decisions(&decisions).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: raw.clone(),
                companion_path: Some(jpeg.clone()),
                ..Default::default()
            }])
            .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 2);
        for decision in decisions {
            assert_eq!(
                PhotoDecision::from(photos.iter().find(|p| p.id == decision.id).unwrap()),
                decision
            );
        }
        assert!(
            photos
                .iter()
                .find(|p| p.id == raw_id)
                .unwrap()
                .companion_path
                .is_none()
        );
        catalog.set_decisions(&decisions).unwrap();
    }
    #[test]
    fn one_jpeg_has_only_one_deterministic_raw_owner() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        let jpeg = dir.path().join("frame.jpg");
        let first = dir.path().join("frame.arw");
        let second = dir.path().join("frame.nef");
        catalog
            .upsert_photos(&[
                Photo {
                    path: second,
                    companion_path: Some(jpeg.clone()),
                    ..Default::default()
                },
                Photo {
                    path: first.clone(),
                    companion_path: Some(jpeg),
                    ..Default::default()
                },
            ])
            .unwrap();
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 2);
        let owners: Vec<_> = photos
            .iter()
            .filter(|p| p.companion_path.is_some())
            .collect();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].path, first);
    }
    #[test]
    fn decisions_batch_is_atomic_and_pair_coalesces() {
        let dir = tempfile::tempdir().unwrap();
        let c = Catalog::open(&dir.path().join("db")).unwrap();
        let jpg = dir.path().join("a.jpg");
        c.upsert_photos(&[Photo {
            path: jpg.clone(),
            ..Default::default()
        }])
        .unwrap();
        let id = c.photos().unwrap()[0].id;
        c.set_rating(id, 4).unwrap();
        c.set_flag(id, Flag::Pick).unwrap();
        let result = c.set_decisions(&[
            PhotoDecision {
                id,
                rating: 1,
                flag: Flag::Reject,
                label: ColorLabel::Blue,
                trashed: false,
            },
            PhotoDecision {
                id: 9999,
                rating: 0,
                flag: Flag::None,
                label: ColorLabel::None,
                trashed: false,
            },
        ]);
        assert!(result.is_err());
        assert_eq!(c.photos().unwrap()[0].rating, 4);
        c.upsert_photos(&[Photo {
            path: dir.path().join("a.nef"),
            companion_path: Some(jpg),
            ..Default::default()
        }])
        .unwrap();
        let photos = c.photos().unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].rating, 4);
        assert_eq!(photos[0].flag, Flag::Pick);
    }
    #[test]
    fn exports_never_overwrite_and_sources_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        let original = source.join("a.jpg");
        fs::write(&original, b"original").unwrap();
        let c = Catalog::open(&dir.path().join("c.db")).unwrap();
        c.upsert_photos(&[Photo {
            path: original.clone(),
            ..Photo::default()
        }])
        .unwrap();
        let id = c.photos().unwrap()[0].id;
        c.set_flag(id, Flag::Pick).unwrap();
        assert!(c.export_picks(&source).is_err());
        let out = dir.path().join("out");
        c.export_picks(&out).unwrap();
        c.export_picks(&out).unwrap();
        assert_eq!(fs::read(out.join("a-1.jpg")).unwrap(), b"original");
        assert_eq!(fs::read(original).unwrap(), b"original");
        c.export_xmp(&[id], &out).unwrap();
        let xml = fs::read_to_string(out.join("a.xmp")).unwrap();
        assert!(xml.contains("shadowroom:Flag=\"pick\""));
    }
}
