//! Catalog-backed history and counters; one shared SQLite connection, no per-file opens.
use super::{IngestGroup, IngestPlan, SequenceState};
use crate::{
    catalog::Catalog,
    model::{PhotoDecision, PhotoSource},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS ingest_log(
          id INTEGER PRIMARY KEY, source_volume TEXT NOT NULL, source_relative TEXT NOT NULL,
          original_filename TEXT NOT NULL, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
          capture_ms INTEGER, capture_kind TEXT NOT NULL, content_hash TEXT,
          destinations TEXT NOT NULL, backups TEXT NOT NULL, job_id TEXT NOT NULL,
          imported_at INTEGER NOT NULL, data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS ingest_hash ON ingest_log(content_hash) WHERE content_hash IS NOT NULL;
        CREATE INDEX IF NOT EXISTS ingest_capture ON ingest_log(original_filename,size,capture_ms);
        CREATE INDEX IF NOT EXISTS ingest_mtime ON ingest_log(original_filename,size,mtime_ns);
        CREATE TABLE IF NOT EXISTS ingest_jobs(
          id TEXT PRIMARY KEY, started INTEGER NOT NULL, finished INTEGER, source TEXT NOT NULL,
          options TEXT NOT NULL, counts TEXT NOT NULL, bytes INTEGER NOT NULL DEFAULT 0,
          errors TEXT NOT NULL, verification TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS ingest_sequences(key TEXT PRIMARY KEY,value INTEGER NOT NULL CHECK(value>=0));
        COMMIT;")?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeenRecord {
    pub record_id: i64,
    pub imported_at: i64,
    pub destination: Option<PathBuf>,
    pub job_id: String,
    pub matched_by: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub source: PathBuf,
    pub relative: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub blake3: String,
    pub destinations: Vec<PathBuf>,
    pub existed: Vec<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportRecord {
    pub source_volume: String,
    pub source_relative: PathBuf,
    pub original_filename: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub capture_ms: i64,
    pub capture_kind: String,
    pub content_hash: Option<String>,
    pub destinations: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    pub job_id: String,
    pub imported_at: i64,
    pub files: Vec<FileRecord>,
    pub marked: bool,
}
impl ImportRecord {
    pub fn marked(group: &IngestGroup) -> Self {
        Self {
            source_volume: group.volume.clone(),
            source_relative: group.relative_path.clone(),
            original_filename: group.photo.filename.clone(),
            size: group.primary.size,
            mtime_ns: group.primary.mtime_ns,
            capture_ms: group.capture_ms,
            capture_kind: group.capture_kind.clone(),
            content_hash: group.content_hash.clone(),
            destinations: Vec::new(),
            backups: Vec::new(),
            job_id: "marked".into(),
            imported_at: chrono::Utc::now().timestamp_millis(),
            files: Vec::new(),
            marked: true,
        }
    }
}
impl Catalog {
    pub fn ingest_sequences(&self) -> Result<SequenceState> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let mut state = SequenceState::default();
        let mut query = connection.prepare_cached("SELECT key,value FROM ingest_sequences")?;
        for row in query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })? {
            let (key, value) = row?;
            if key == "stored" {
                state.stored = value;
            } else if let Some(day) = key.strip_prefix("day:") {
                state.days.insert(day.into(), value);
            }
        }
        Ok(state)
    }
    /// Strongest available match wins. Hash mismatch never downgrades to metadata.
    /// Timezone matching checks every candidate; it does not use LIMIT 1 before the predicate.
    pub fn ingest_seen(&self, group: &IngestGroup) -> Result<Option<SeenRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<SeenRecord> {
            let destinations: String = row.get(1)?;
            let paths: Vec<PathBuf> = serde_json::from_str(&destinations).unwrap_or_default();
            Ok(SeenRecord {
                record_id: row.get(3)?,
                imported_at: row.get(0)?,
                destination: paths.into_iter().next(),
                job_id: row.get(2)?,
                matched_by: String::new(),
            })
        };
        if let Some(hash) = &group.content_hash {
            let mut found = connection.query_row("SELECT imported_at,destinations,job_id,id FROM ingest_log WHERE content_hash=?1 ORDER BY imported_at DESC LIMIT 1", [hash], read).optional()?;
            if let Some(found) = &mut found {
                found.matched_by = "hash".into();
            }
            return Ok(found);
        }
        if group.capture_kind != "mtime" {
            let mut found = connection.query_row("SELECT imported_at,destinations,job_id,id FROM ingest_log WHERE original_filename=?1 AND size=?2 AND capture_ms=?3 AND capture_kind<>'mtime' ORDER BY imported_at DESC LIMIT 1", params![group.photo.filename, group.primary.size, group.capture_ms], read).optional()?;
            if let Some(found) = &mut found {
                found.matched_by = "capture".into();
            }
            if found.is_some() {
                return Ok(found);
            }
        }
        const DAY: i64 = 86_400_000_000_000;
        const QUARTER: i64 = 900_000_000_000;
        let mut found = connection.query_row("SELECT imported_at,destinations,job_id,id FROM ingest_log WHERE original_filename=?1 AND size=?2 AND mtime_ns BETWEEN ?3 AND ?4 AND ((mtime_ns-?5) % ?6)=0 ORDER BY imported_at DESC LIMIT 1", params![group.photo.filename, group.primary.size, group.primary.mtime_ns.saturating_sub(DAY), group.primary.mtime_ns.saturating_add(DAY), group.primary.mtime_ns, QUARTER], read).optional()?;
        if let Some(found) = &mut found {
            found.matched_by = "mtime_timezone".into();
        }
        Ok(found)
    }
    pub fn ingest_previous(&self, group: &IngestGroup) -> Result<Option<ImportRecord>> {
        let Some(seen) = self.ingest_seen(group)? else {
            return Ok(None);
        };
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let data: String = connection.query_row(
            "SELECT data FROM ingest_log WHERE id=?1",
            [seen.record_id],
            |row| row.get(0),
        )?;
        Ok(Some(serde_json::from_str(&data)?))
    }
    pub fn ingest_source_decisions(
        &self,
        source: &Path,
    ) -> Result<HashMap<PathBuf, PhotoDecision>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let root = source.to_string_lossy();
        let root = root.trim_end_matches('/');
        let mut query = connection.prepare_cached("SELECT path,id,rating,flag,label,trashed FROM photos WHERE source_device IS NULL AND path>=?1 AND path<?2")?;
        let mut result = HashMap::new();
        for row in query.query_map(params![format!("{root}/"), format!("{root}0")], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                PhotoDecision {
                    id: r.get(1)?,
                    rating: r.get(2)?,
                    flag: crate::model::Flag::from(r.get::<_, String>(3)?.as_str()),
                    label: crate::model::ColorLabel::from(r.get::<_, String>(4)?.as_str()),
                    trashed: r.get(5)?,
                },
            ))
        })? {
            let (path, decision) = row?;
            result.insert(path, decision);
        }
        Ok(result)
    }
    pub fn ingest_begin(&self, id: &str, plan: &IngestPlan) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let progress = super::Progress {
            job_id: id.into(),
            phase: "copying".into(),
            total: plan.groups.len(),
            total_bytes: plan.bytes,
            library_root: Some(plan.options.library_root.clone()),
            ..Default::default()
        };
        connection.execute("INSERT INTO ingest_jobs(id,started,source,options,counts,errors,verification) VALUES(?1,?2,?3,?4,?5,'[]','running')", params![id, chrono::Utc::now().timestamp_millis(), plan.source.to_string_lossy(), serde_json::to_string(&plan.options)?, serde_json::to_string(&progress)?])?;
        Ok(())
    }
    /// Called before the worker accepts commands. The shared job lease prevents
    /// reconciliation from treating another job on this Catalog as abandoned.
    pub(crate) fn ingest_reconcile_abandoned(&self) -> Result<usize> {
        let _lease = self
            .ingest_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Ingest job lock poisoned"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = connection.transaction()?;
        let jobs = tx.prepare("SELECT id,counts FROM ingest_jobs WHERE verification IN ('running','finalizing') AND finished IS NULL")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, counts) in &jobs {
            let mut progress: super::Progress = serde_json::from_str(counts).unwrap_or_default();
            progress.job_id = id.clone();
            progress.phase = "failed".into();
            progress.completed = 0;
            progress.skipped = 0;
            progress.failed = 0;
            progress.files = 0;
            progress.photos = 0;
            progress.videos = 0;
            progress.bytes = 0;
            let data = tx
                .prepare("SELECT data FROM ingest_log WHERE job_id=?1 ORDER BY id")?
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for data in &data {
                let record: ImportRecord = serde_json::from_str(data)?;
                if record
                    .files
                    .iter()
                    .any(|file| file.existed.iter().any(|exists| !exists))
                {
                    progress.completed += 1;
                } else {
                    progress.skipped += 1;
                }
                progress.files += record.files.len();
                for file in &record.files {
                    progress.bytes = progress
                        .bytes
                        .checked_add(file.size)
                        .ok_or_else(|| anyhow::anyhow!("Recovered ingest size overflow"))?;
                }
                if record
                    .files
                    .first()
                    .is_some_and(|f| super::is_video(&f.source))
                {
                    progress.videos += 1;
                } else if !record.files.is_empty() {
                    progress.photos += 1;
                }
            }
            progress.total = progress.total.max(data.len());
            progress.errors.push("Import job was interrupted or its final history update failed; committed groups were retained; unfinished work was not inferred".into());
            tx.execute("UPDATE ingest_jobs SET finished=?2,counts=?3,bytes=?4,errors=?5,verification='failed' WHERE id=?1 AND verification IN ('running','finalizing') AND finished IS NULL", params![id, chrono::Utc::now().timestamp_millis(), serde_json::to_string(&progress)?, progress.bytes, serde_json::to_string(&progress.errors)?])?;
        }
        tx.commit()?;
        Ok(jobs.len())
    }
    pub fn ingest_finish(&self, progress: &super::Progress) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let finished =
            (progress.phase != "finalizing").then(|| chrono::Utc::now().timestamp_millis());
        connection.execute("UPDATE ingest_jobs SET finished=?2,counts=?3,bytes=?4,errors=?5,verification=?6 WHERE id=?1", params![progress.job_id, finished, serde_json::to_string(progress)?, progress.bytes, serde_json::to_string(&progress.errors)?, progress.phase])?;
        Ok(())
    }
    /// Files are verified/published before this transaction. A failed commit causes
    /// the caller to roll back its new filesystem entries. Counters and photo
    /// decisions are committed with the import record, never during planning.
    pub(crate) fn ingest_commit(
        &self,
        record: &ImportRecord,
        group: Option<&IngestGroup>,
        day: Option<&str>,
    ) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO ingest_log(source_volume,source_relative,original_filename,size,mtime_ns,capture_ms,capture_kind,content_hash,destinations,backups,job_id,imported_at,data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![record.source_volume, record.source_relative.to_string_lossy(), record.original_filename, record.size, record.mtime_ns, record.capture_ms, record.capture_kind, record.content_hash, serde_json::to_string(&record.destinations)?, serde_json::to_string(&record.backups)?, record.job_id, record.imported_at, serde_json::to_string(record)?])?;
        if let Some(day) = day {
            for key in ["stored".to_string(), format!("day:{day}")] {
                tx.execute("INSERT INTO ingest_sequences(key,value) VALUES(?1,1) ON CONFLICT(key) DO UPDATE SET value=value+1", [key])?;
            }
        }
        if let Some(group) = group.filter(|g| g.kind == "photo") {
            if let Some(file) = record.files.first() {
                let mut photo = group.photo.clone();
                photo.source = PhotoSource::Local;
                photo.id = 0;
                photo.path = file.destinations[0].clone();
                photo.filename = photo
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                photo.folder = photo
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .into_owned();
                photo.companion_path = group
                    .companion
                    .as_ref()
                    .and_then(|companion| record.files.iter().find(|f| f.source == companion.path))
                    .and_then(|f| f.destinations.first())
                    .cloned();
                tx.execute("INSERT INTO photos(path,modified_at,size_bytes,data,rating,flag,label,trashed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(path) DO UPDATE SET data=excluded.data,modified_at=excluded.modified_at,size_bytes=excluded.size_bytes", params![photo.path.to_string_lossy(), photo.modified_at, photo.size_bytes, serde_json::to_string(&photo)?, photo.rating, photo.flag.as_str(), photo.label.as_str(), photo.trashed])?;
                if group.photo.id != 0 {
                    tx.execute(
                        "UPDATE photos SET rating=?2,flag=?3,label=?4,trashed=?5 WHERE path=?1",
                        params![
                            photo.path.to_string_lossy(),
                            photo.rating,
                            photo.flag.as_str(),
                            photo.label.as_str(),
                            photo.trashed
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn ingest_mark_imported(&self, groups: &[IngestGroup]) -> Result<usize> {
        for group in groups {
            self.ingest_commit(&ImportRecord::marked(group), None, None)?;
        }
        Ok(groups.len())
    }
    pub fn ingest_history(&self, limit: usize) -> Result<serde_json::Value> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Catalog lock poisoned"))?;
        let mut query = connection.prepare_cached("SELECT id,started,finished,source,options,counts,bytes,errors,verification FROM ingest_jobs ORDER BY started DESC LIMIT ?1")?;
        let jobs = query.query_map([limit.min(1000)], |row| {
            let parse = |n| -> rusqlite::Result<serde_json::Value> { let value: String = row.get(n)?; Ok(serde_json::from_str(&value).unwrap_or(serde_json::Value::Null)) };
            Ok(serde_json::json!({"id":row.get::<_, String>(0)?,"started":row.get::<_, i64>(1)?,"finished":row.get::<_, Option<i64>>(2)?,"source":row.get::<_, String>(3)?,"options":parse(4)?,"counts":parse(5)?,"bytes":row.get::<_, u64>(6)?,"errors":parse(7)?,"verification":row.get::<_, String>(8)?}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut query = connection.prepare_cached(
            "SELECT data FROM ingest_log ORDER BY imported_at DESC,id DESC LIMIT ?1",
        )?;
        let records = query
            .query_map([limit.min(1000)], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .map(|s| serde_json::from_str::<serde_json::Value>(s))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let count: u64 =
            connection.query_row("SELECT count(*) FROM ingest_log", [], |row| row.get(0))?;
        Ok(serde_json::json!({"jobs":jobs,"records":records,"total_imported_groups":count}))
    }
}
