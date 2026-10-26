//! Catalog actor and bounded background export/grouping work, independent of GPUI.
use crate::{
    catalog::{Catalog, ExportPlan},
    duplicates::{DuplicateJob, DuplicateScanState},
    grouping::{GroupingCache, GroupingMode, GroupingOptions},
    indexer::{Drive, IndexEvent, IndexJob, IndexOptions, Indexer, discover_drives},
    model::{Photo, PhotoDecision},
    settings::Settings,
};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub type PhotoSlot = Arc<Mutex<Option<Vec<Photo>>>>;
pub type GroupSlot = Arc<Mutex<Option<Vec<Vec<i64>>>>>;

#[derive(Debug)]
pub enum IoCommand {
    Remote(crate::network::RemoteCommand),
    ConnectRemote(String),
    DisconnectRemote(String),
    IndexRemote {
        host: String,
        root: String,
    },
    SyncRemote {
        host: String,
        root: String,
    },
    CancelRemote(String),
    IngestScan(PathBuf),
    IngestScanCard(PathBuf),
    IngestPlan(crate::ingest::IngestOptions),
    IngestStart,
    IngestPause,
    IngestResume,
    IngestCancel,
    IngestHistory(usize),
    IngestMarkImported(Vec<usize>),
    Eject(PathBuf),
    Import(PathBuf),
    CancelImport,
    /// Explicit opt-in; an empty ID list scans all nontrashed catalog assets.
    ScanDuplicates {
        id: u64,
        ids: Vec<i64>,
    },
    CancelDuplicates,
    Decisions(Vec<PhotoDecision>),
    Hash(i64, u64),
    Settings(Settings),
    Export(PathBuf, bool, Vec<i64>),
    ExportTracked {
        id: u64,
        destination: PathBuf,
        picks: bool,
        ids: Vec<i64>,
    },
    Refresh,
    Regroup,
    /// FIFO durability barrier without stopping indexing, exports or the actor.
    Flush(Sender<()>),
    /// FIFO barrier: ack only after earlier decisions/settings, all accepted
    /// exports finish, the export worker joins, and the final catalog flush.
    /// An error drops the ack sender and emits Error; caller must report that.
    Shutdown(Sender<()>),
}

#[derive(Debug)]
pub enum IoEvent {
    Remote(crate::network::HostSnapshot),
    RemoteRemoved(String),
    RemoteProvider(Arc<crate::media::RemotePreviewProvider>),
    Ingest(crate::ingest::worker::Event),
    IngestDetected(PathBuf),
    Drives(Vec<Drive>),
    Status(String),
    Error(String),
    Busy(bool),
    /// One successful durable Decisions command (not one photo).
    Saved,
    /// One failed Decisions command; clears one pending-save indication too.
    SaveFailed(String),
    /// Exact normalized snapshot successfully persisted by the preferences writer.
    PreferencesSaved(Settings),
    /// Pending preferences remain queued for retry; this never acknowledges durability.
    PreferencesFailed(String),
    DuplicateScan(DuplicateScanState),
    ExportJob {
        id: u64,
        state: String,
        completed: usize,
        error: Option<String>,
    },
}

const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);
const SETTINGS_DEBOUNCE: Duration = Duration::from_millis(750);
const HASH_DEBOUNCE: Duration = Duration::from_millis(300);
const HASH_BATCH: usize = 32;
const GROUP_DEBOUNCE: Duration = Duration::from_millis(350);
const TELEMETRY_LIMIT: usize = 128;
const COMMAND_CAPACITY: usize = 1024;

type Exporter = Arc<dyn Fn(ExportPlan) -> Result<usize> + Send + Sync>;
type PreferenceWriter = Arc<dyn Fn(&Settings) -> Result<()> + Send + Sync>;

/// A single replaceable photo snapshot and grouping result are shared with the
/// UI. Consume each with `try_lock().ok().and_then(|mut slot| slot.take())` once
/// per poll. Drain save acknowledgements first; defer photos while UI decisions
/// remain pending so an older snapshot cannot undo an optimistic local decision.
pub fn start_io(
    data: PathBuf,
    initial: Settings,
    import: Option<PathBuf>,
) -> (Sender<IoCommand>, Receiver<IoEvent>, PhotoSlot, GroupSlot) {
    start_io_with_exporter(
        data,
        initial,
        import,
        Arc::new(ExportPlan::execute),
        Arc::new(Settings::save_default),
    )
}

fn start_io_with_exporter(
    data: PathBuf,
    initial: Settings,
    import: Option<PathBuf>,
    exporter: Exporter,
    preferences: PreferenceWriter,
) -> (Sender<IoCommand>, Receiver<IoEvent>, PhotoSlot, GroupSlot) {
    let (commands, incoming) = bounded(COMMAND_CAPACITY);
    // Only reliable lightweight acknowledgements/errors can exceed the telemetry
    // cap. Never block the decision writer on a paused/destroyed UI receiver.
    let (events, outgoing) = unbounded();
    let photos: PhotoSlot = Arc::new(Mutex::new(None));
    let groups: GroupSlot = Arc::new(Mutex::new(None));
    let photo_output = photos.clone();
    let group_output = groups.clone();
    thread::Builder::new()
        .name("shadowroom-catalog".into())
        .spawn(move || {
            run_catalog(
                data,
                initial,
                import,
                incoming,
                events,
                photo_output,
                group_output,
                exporter,
                preferences,
            )
        })
        .expect("Start catalog worker");
    (commands, outgoing, photos, groups)
}

struct ExportRequest {
    id: Option<u64>,
    plan: ExportPlan,
    destination: PathBuf,
    picks: bool,
}

fn run_catalog(
    data: PathBuf,
    mut settings: Settings,
    import: Option<PathBuf>,
    commands: Receiver<IoCommand>,
    events: Sender<IoEvent>,
    photos: PhotoSlot,
    groups: GroupSlot,
    exporter: Exporter,
    preferences_writer: PreferenceWriter,
) {
    let catalog = match Catalog::open(&data.join("catalog.sqlite3")) {
        Ok(catalog) => catalog,
        Err(error) => {
            reliable(
                &events,
                IoEvent::Error(format!("Catalog could not open: {error:#}")),
            );
            return;
        }
    };
    let (remote_changed, remote_changes) = bounded(1);
    let mut network = match crate::network::RemoteService::new(
        data.clone(),
        catalog.clone(),
        events.clone(),
        remote_changed.clone(),
        settings.remote_cache_bytes(),
    ) {
        Ok(service) => Some(service),
        Err(error) => {
            reliable(
                &events,
                IoEvent::Error(format!("Network service: {error:#}")),
            );
            None
        }
    };
    let ingest =
        crate::ingest::worker::Worker::start(catalog.clone(), events.clone(), remote_changed);
    settings.normalize();
    let (exports, export_work) = bounded::<ExportRequest>(1);
    let (export_finished, export_completions) = unbounded::<()>();
    let export_events = events.clone();
    // Accepted exports drain gracefully. Their immutable input snapshots never
    // hold the catalog connection, including while Shutdown waits for completion.
    let export_worker = thread::Builder::new()
        .name("shadowroom-export".into())
        .spawn(move || {
            while let Ok(request) = export_work.recv() {
                export_job(&export_events, request.id, "running", 0, None);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    exporter(request.plan)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Export worker panicked")));
                reliable(
                    &export_events,
                    match result {
                        Ok(count) => {
                            export_job(&export_events, request.id, "completed", count, None);
                            IoEvent::Status(format!(
                                "Exported {count} {} to {}",
                                if request.picks { "files" } else { "sidecars" },
                                request.destination.display()
                            ))
                        }
                        Err(error) => {
                            let completed = error
                                .downcast_ref::<crate::catalog::ExportFailure>()
                                .map_or(0, |failure| failure.completed);
                            export_job(
                                &export_events,
                                request.id,
                                "failed",
                                completed,
                                Some(format!("{error:#}")),
                            );
                            IoEvent::Error(format!("Export: {error:#}"))
                        }
                    },
                );
                // Terminal events precede this private completion, so Shutdown
                // cannot acknowledge a job whose outcome has not been reported.
                let _ = export_finished.send(());
            }
        })
        .expect("Start export worker");

    let grouping = GroupWorker::start(groups, options(&settings));
    let (drive_stop, drive_cancelled) = bounded::<()>(1);
    let drive_slot = Arc::new(Mutex::new(None));
    let drive_output = drive_slot.clone();
    thread::Builder::new()
        .name("shadowroom-drives".into())
        .spawn(move || {
            loop {
                replace(&drive_output, discover_drives());
                match drive_cancelled.recv_timeout(Duration::from_secs(5)) {
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        })
        .expect("Start drive discovery worker");

    publish(&catalog, &photos, &grouping, &events);
    let mut last_snapshot = Instant::now();
    let mut pending_settings = None;
    let mut settings_at = Instant::now();
    let mut pending_hashes = HashMap::<i64, u64>::new();
    let mut hashes_at = Instant::now();
    let mut hash_retry_at = Instant::now();
    let mut job: Option<IndexJob> = None;
    let mut roots = VecDeque::new();
    if let Some(path) = import {
        roots.push_back(path);
    }
    let mut seen_drives = HashSet::new();
    let mut index_dirty = false;
    let mut indexed_seen = 0;
    let mut terminal_progress = false;
    let mut refresh_needed = false;
    let mut status = None;
    let mut busy = None;
    let mut pending_drives = None;
    let mut shutdown = Vec::<Sender<()>>::new();
    let mut active_exports = 0usize;
    let mut duplicate_job: Option<DuplicateJob> = None;

    'actor: loop {
        if remote_changes.try_recv().is_ok() {
            refresh_needed = true;
        }
        let first = match commands.recv_timeout(Duration::from_millis(25)) {
            Ok(command) => Some(command),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
        };
        // Drain a finite batch so indexing and settings debouncing still progress.
        let batch = first.into_iter().chain(commands.try_iter()).take(128);
        for command in batch {
            let command = match command {
                IoCommand::ConnectRemote(host) => {
                    IoCommand::Remote(crate::network::RemoteCommand::Connect(host))
                }
                IoCommand::DisconnectRemote(host) => {
                    IoCommand::Remote(crate::network::RemoteCommand::Disconnect(host))
                }
                IoCommand::IndexRemote { host, root } => {
                    IoCommand::Remote(crate::network::RemoteCommand::Index { host, root })
                }
                IoCommand::SyncRemote { host, root } => {
                    IoCommand::Remote(crate::network::RemoteCommand::Sync { host, root })
                }
                IoCommand::CancelRemote(host) => {
                    IoCommand::Remote(crate::network::RemoteCommand::Cancel(host))
                }
                command => command,
            };
            let card_scan = matches!(&command, IoCommand::IngestScanCard(_));
            match command {
                IoCommand::ConnectRemote(_)
                | IoCommand::DisconnectRemote(_)
                | IoCommand::IndexRemote { .. }
                | IoCommand::SyncRemote { .. }
                | IoCommand::CancelRemote(_) => unreachable!("remote command normalized"),
                IoCommand::Remote(command) => {
                    let result = network
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("Network service unavailable"))
                        .and_then(|service| service.command(command));
                    if let Err(error) = result {
                        reliable(&events, IoEvent::Error(format!("Network: {error:#}")));
                    }
                }
                IoCommand::IngestScan(path) | IoCommand::IngestScanCard(path) => {
                    let command = if card_scan {
                        crate::ingest::worker::Command::ScanCard { path, auto: None }
                    } else {
                        crate::ingest::worker::Command::Scan { path, auto: None }
                    };
                    if let Err(error) = ingest.send(command) {
                        reliable(&events, IoEvent::Error(format!("Import: {error:#}")));
                    }
                }
                IoCommand::IngestPlan(options) => {
                    if let Err(error) = ingest.send(crate::ingest::worker::Command::Plan(options)) {
                        reliable(&events, IoEvent::Error(format!("Import: {error:#}")));
                    }
                }
                IoCommand::IngestStart
                | IoCommand::IngestPause
                | IoCommand::IngestResume
                | IoCommand::IngestCancel
                | IoCommand::IngestHistory(_)
                | IoCommand::IngestMarkImported(_)
                | IoCommand::Eject(_) => {
                    use crate::ingest::worker::Command as Ingest;
                    let request = match command {
                        IoCommand::IngestStart => Ingest::Start,
                        IoCommand::IngestPause => Ingest::Pause,
                        IoCommand::IngestResume => Ingest::Resume,
                        IoCommand::IngestCancel => Ingest::Cancel,
                        IoCommand::IngestHistory(limit) => Ingest::History(limit),
                        IoCommand::IngestMarkImported(ids) => Ingest::MarkImported(ids),
                        IoCommand::Eject(path) => Ingest::Eject(path),
                        _ => unreachable!(),
                    };
                    if let Err(error) = ingest.send(request) {
                        reliable(&events, IoEvent::Error(format!("Import: {error:#}")));
                    }
                }
                IoCommand::Import(path) => {
                    if !roots.contains(&path) {
                        roots.push_back(path);
                    }
                }
                IoCommand::CancelImport => {
                    roots.clear();
                    if let Some(index) = &job {
                        index.cancel();
                        status = Some("Stopping folder indexing…".into());
                    }
                }
                IoCommand::ScanDuplicates { id, ids } => {
                    let result = (|| -> Result<DuplicateJob> {
                        if !shutdown.is_empty() {
                            anyhow::bail!("The app is closing");
                        }
                        if duplicate_job.is_some() {
                            anyhow::bail!(
                                "A duplicate scan is already active; cancel it before starting another"
                            );
                        }
                        let mut snapshot = catalog.photos()?;
                        if !ids.is_empty() {
                            let requested: HashSet<_> = ids.into_iter().collect();
                            let known: HashSet<_> = snapshot.iter().map(|p| p.id).collect();
                            if requested.iter().any(|id| !known.contains(id)) {
                                anyhow::bail!("Duplicate scan contains an unknown photo ID");
                            }
                            snapshot.retain(|photo| requested.contains(&photo.id));
                        }
                        for photo in &snapshot {
                            photo.require_local("Duplicate scanning")?;
                        }
                        DuplicateJob::start(id, snapshot, catalog.path().into())
                    })();
                    match result {
                        Ok(job) => duplicate_job = Some(job),
                        Err(error) => reliable(
                            &events,
                            IoEvent::DuplicateScan(DuplicateScanState {
                                id,
                                phase: "failed".into(),
                                errors: vec![format!("{error:#}")],
                                ..Default::default()
                            }),
                        ),
                    }
                }
                IoCommand::CancelDuplicates => {
                    if let Some(job) = &duplicate_job {
                        job.cancel();
                    }
                }
                IoCommand::Decisions(decisions) => {
                    let result = catalog.set_decisions(&decisions);
                    // Invalidate before acknowledging: a queued pre-decision
                    // snapshot must never replace newly acknowledged UI state.
                    let _ = take(&photos);
                    refresh_needed = true;
                    match result {
                        Ok(()) => reliable(&events, IoEvent::Saved),
                        Err(error) => reliable(
                            &events,
                            IoEvent::SaveFailed(format!("Decision was not saved: {error:#}")),
                        ),
                    }
                }
                IoCommand::Hash(id, hash) => {
                    if pending_hashes.is_empty() {
                        hashes_at = Instant::now();
                    }
                    pending_hashes.insert(id, hash);
                    grouping.hash(id, hash);
                }
                IoCommand::Settings(mut next) => {
                    next.normalize();
                    if next.auto_discover_drives && !settings.auto_discover_drives {
                        seen_drives.clear();
                    }
                    if grouping_key(&next) != grouping_key(&settings) {
                        grouping.configure(options(&next));
                    }
                    if let Some(network) = &network {
                        if let Err(error) = network.set_cache_budget(next.remote_cache_bytes()) {
                            reliable(
                                &events,
                                IoEvent::Error(format!("Remote cache budget: {error:#}")),
                            );
                        }
                    }
                    settings = next.clone();
                    pending_settings = Some(next);
                    settings_at = Instant::now();
                }
                IoCommand::Export(destination, picks, ids) => {
                    if queue_export(
                        &catalog,
                        &exports,
                        &events,
                        None,
                        destination,
                        picks,
                        ids,
                        shutdown.is_empty(),
                    ) {
                        active_exports += 1;
                    }
                }
                IoCommand::ExportTracked {
                    id,
                    destination,
                    picks,
                    ids,
                } => {
                    if queue_export(
                        &catalog,
                        &exports,
                        &events,
                        Some(id),
                        destination,
                        picks,
                        ids,
                        shutdown.is_empty(),
                    ) {
                        active_exports += 1;
                    }
                }
                IoCommand::Refresh => {
                    refresh_needed = true;
                    last_snapshot = Instant::now() - SNAPSHOT_INTERVAL;
                }
                IoCommand::Regroup => grouping.configure(options(&settings)),
                IoCommand::Flush(ack) => {
                    let hash_result = flush_hashes(&catalog, &mut pending_hashes);
                    let settings_result =
                        flush_preferences(&preferences_writer, &mut pending_settings, &events);
                    let catalog_result = catalog.flush();
                    match hash_result.and(settings_result).and(catalog_result) {
                        Ok(()) => {
                            let _ = ack.send(());
                        }
                        Err(error) => {
                            reliable(&events, IoEvent::Error(format!("Flush failed: {error:#}")))
                        }
                    }
                }
                IoCommand::Shutdown(ack) => {
                    // A failed close must leave queued preferences/hashes and a
                    // live actor available for the user's retry.
                    let hash_result = flush_hashes(&catalog, &mut pending_hashes);
                    let settings_result =
                        flush_preferences(&preferences_writer, &mut pending_settings, &events);
                    let catalog_result = catalog.flush();
                    match hash_result.and(settings_result).and(catalog_result) {
                        Ok(()) => {
                            shutdown.push(ack);
                            roots.clear();
                            if let Some(job) = &duplicate_job {
                                job.cancel();
                            }
                            if let Some(index) = &job {
                                index.cancel();
                            }
                        }
                        Err(error) => reliable(
                            &events,
                            IoEvent::Error(format!("Shutdown flush failed: {error:#}")),
                        ),
                    }
                }
            }
        }

        if let Some(job) = &duplicate_job {
            let finished = job.is_finished();
            if let Some(state) = job.take_update() {
                if state.phase != "running" || events.len() < TELEMETRY_LIMIT {
                    reliable(&events, IoEvent::DuplicateScan(state));
                }
            }
            if finished {
                if let Err(error) = duplicate_job.take().unwrap().join() {
                    reliable(
                        &events,
                        IoEvent::Error(format!("Duplicate scan: {error:#}")),
                    );
                }
            }
        }
        for () in export_completions.try_iter() {
            active_exports = active_exports.saturating_sub(1);
        }
        if !shutdown.is_empty() && active_exports == 0 && duplicate_job.is_none() {
            // Decisions/settings received during the export wait also need a
            // retryable barrier before any worker channels are closed.
            let hash_result = flush_hashes(&catalog, &mut pending_hashes);
            let settings_result =
                flush_preferences(&preferences_writer, &mut pending_settings, &events);
            let catalog_result = catalog.flush();
            if let Err(error) = hash_result.and(settings_result).and(catalog_result) {
                reliable(
                    &events,
                    IoEvent::Error(format!("Shutdown flush failed: {error:#}")),
                );
                shutdown.clear();
            } else {
                break 'actor;
            }
        }

        // Metadata never inserts an fsync ahead of already queued decisions.
        if !pending_hashes.is_empty()
            && commands.is_empty()
            && Instant::now() >= hash_retry_at
            && (pending_hashes.len() >= HASH_BATCH || hashes_at.elapsed() >= HASH_DEBOUNCE)
        {
            if let Err(error) = flush_hashes(&catalog, &mut pending_hashes) {
                reliable(
                    &events,
                    IoEvent::Error(format!("Visual hash batch: {error:#}")),
                );
                hash_retry_at = Instant::now() + HASH_DEBOUNCE;
            }
        }
        if pending_settings.is_some() && settings_at.elapsed() >= SETTINGS_DEBOUNCE {
            if flush_preferences(&preferences_writer, &mut pending_settings, &events).is_err() {
                settings_at = Instant::now();
            }
        }

        if let Some(drives) = take(&drive_slot) {
            let mounted: HashSet<_> = drives.iter().map(|drive| &drive.path).collect();
            seen_drives.retain(|path| mounted.contains(path));
            for drive in &drives {
                if seen_drives.insert(drive.path.clone())
                    && settings.auto_discover_drives
                    && shutdown.is_empty()
                    && drive.removable
                    && drive.photo_folders.iter().any(|p| {
                        p.file_name().is_some_and(|n| {
                            ["dcim", "private", "mp_root"]
                                .contains(&n.to_string_lossy().to_ascii_lowercase().as_str())
                        })
                    })
                    && !roots.contains(&drive.path)
                {
                    let identity = crate::ingest::volume_identity(&drive.path);
                    if settings
                        .ingest_volumes
                        .get(&identity)
                        .is_some_and(|rule| rule == "deny")
                    {
                        continue;
                    }
                    match settings.ingest_on_insert.as_str() {
                        "ignore" => {}
                        "index_in_place" => roots.push_back(drive.path.clone()),
                        "auto" => {
                            if let Err(error) =
                                ingest.send(crate::ingest::worker::Command::ScanCard {
                                    path: drive.path.clone(),
                                    auto: Some(crate::ingest::IngestOptions::from_settings(
                                        &settings,
                                    )),
                                })
                            {
                                reliable(
                                    &events,
                                    IoEvent::Error(format!("Auto import: {error:#}")),
                                );
                            }
                            reliable(&events, IoEvent::IngestDetected(drive.path.clone()));
                        }
                        _ => reliable(&events, IoEvent::IngestDetected(drive.path.clone())),
                    }
                }
            }
            pending_drives = Some(drives);
        }

        let mut finished = false;
        if let Some(index) = &job {
            for _ in 0..128 {
                let Ok(event) = index.try_recv() else {
                    break;
                };
                match event {
                    IndexEvent::Started(path) => {
                        status = Some(format!("Indexing {}", path.display()));
                    }
                    IndexEvent::Progress(progress) => {
                        status = Some(if progress.errors > 0 {
                            format!(
                                "Indexing · {} found · {}",
                                progress.scanned,
                                count(progress.errors, "error", "errors")
                            )
                        } else {
                            format!("Indexing · {} found", progress.scanned)
                        });
                        index_dirty |= progress.indexed > indexed_seen;
                        indexed_seen = progress.indexed;
                    }
                    IndexEvent::Finished(progress) | IndexEvent::Cancelled(progress) => {
                        index_dirty |= progress.indexed > indexed_seen;
                        indexed_seen = progress.indexed;
                        terminal_progress = true;
                        status = Some(index_summary(&progress));
                        finished = true;
                    }
                    IndexEvent::Error { path, message } => {
                        telemetry(
                            &events,
                            IoEvent::Error(format!("{}: {message}", path.display())),
                        );
                    }
                }
            }
            if index.is_finished() {
                finished = true;
            }
        }
        if finished {
            if let Some(index) = job.take() {
                if let Err(error) = index.join() {
                    reliable(&events, IoEvent::Error(format!("Indexer: {error:#}")));
                }
            }
            // A known library can finish without changing a single row. Its
            // initial snapshot is already published; avoid another full load,
            // sort and regroup solely because a read-only scan completed.
            if !terminal_progress || index_dirty || refresh_needed {
                publish_with_hashes(&catalog, &photos, &grouping, &events, &pending_hashes);
            }
            last_snapshot = Instant::now();
            index_dirty = false;
            refresh_needed = false;
            busy = Some(false);
        } else if (index_dirty || refresh_needed) && last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
            publish_with_hashes(&catalog, &photos, &grouping, &events, &pending_hashes);
            last_snapshot = Instant::now();
            index_dirty = false;
            refresh_needed = false;
        }
        if job.is_none() && shutdown.is_empty() {
            if let Some(path) = roots.pop_front() {
                indexed_seen = 0;
                terminal_progress = false;
                job = Some(Indexer::start(
                    catalog.clone(),
                    path,
                    IndexOptions::default(),
                ));
                busy = Some(true);
            }
        }
        // Replace pending telemetry rather than accumulating progress messages.
        if events.len() < TELEMETRY_LIMIT {
            if let Some(value) = busy.take() {
                reliable(&events, IoEvent::Busy(value));
            }
        }
        if events.len() < TELEMETRY_LIMIT {
            if let Some(value) = pending_drives.take() {
                reliable(&events, IoEvent::Drives(value));
            }
        }
        if events.len() < TELEMETRY_LIMIT {
            if let Some(value) = status.take() {
                reliable(&events, IoEvent::Status(value));
            }
        }
    }

    if let Some(job) = duplicate_job {
        job.cancel();
        // This actor is a background thread. Join before the final flush/ack so
        // a scan cannot keep writing its local hash cache after app shutdown.
        if let Err(error) = job.join() {
            reliable(
                &events,
                IoEvent::Error(format!("Stop duplicate scan: {error:#}")),
            );
        }
    }
    if let Err(error) = ingest.stop() {
        reliable(&events, IoEvent::Error(format!("Stop ingest: {error:#}")));
    }
    drop(network);
    drop(exports);
    let export_result = export_worker
        .join()
        .map_err(|_| anyhow::anyhow!("Export worker did not stop cleanly"));
    let _ = drive_stop.try_send(());
    drop(grouping);
    if let Some(index) = job {
        index.cancel();
        if let Err(error) = index.join() {
            reliable(&events, IoEvent::Error(format!("Stop indexing: {error:#}")));
        }
    }
    let hash_result = flush_hashes(&catalog, &mut pending_hashes);
    let settings_result = flush_preferences(&preferences_writer, &mut pending_settings, &events);
    let flush_result = catalog.flush();
    match export_result
        .and(hash_result)
        .and(settings_result)
        .and(flush_result)
    {
        Ok(()) => {
            for ack in shutdown {
                let _ = ack.send(());
            }
        }
        Err(error) => reliable(
            &events,
            IoEvent::Error(format!("Shutdown flush failed: {error:#}")),
        ),
    }
}

fn flush_hashes(catalog: &Catalog, pending: &mut HashMap<i64, u64>) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let hashes: Vec<_> = pending.iter().map(|(&id, &hash)| (id, hash)).collect();
    catalog.set_dhashes(&hashes)?;
    pending.clear();
    Ok(())
}

fn flush_preferences(
    writer: &PreferenceWriter,
    pending: &mut Option<Settings>,
    events: &Sender<IoEvent>,
) -> Result<()> {
    let Some(preferences) = pending.as_ref() else {
        return Ok(());
    };
    match writer(preferences) {
        Ok(()) => {
            reliable(events, IoEvent::PreferencesSaved(preferences.clone()));
            *pending = None;
            Ok(())
        }
        Err(error) => {
            reliable(
                events,
                IoEvent::PreferencesFailed(format!("Preferences were not saved: {error:#}")),
            );
            Err(error)
        }
    }
}

fn reliable(events: &Sender<IoEvent>, event: IoEvent) {
    let _ = events.send(event);
}
fn export_job(
    events: &Sender<IoEvent>,
    id: Option<u64>,
    state: &str,
    completed: usize,
    error: Option<String>,
) {
    if let Some(id) = id {
        reliable(
            events,
            IoEvent::ExportJob {
                id,
                state: state.into(),
                completed,
                error,
            },
        );
    }
}
fn queue_export(
    catalog: &Catalog,
    exports: &Sender<ExportRequest>,
    events: &Sender<IoEvent>,
    id: Option<u64>,
    destination: PathBuf,
    picks: bool,
    ids: Vec<i64>,
    accepting: bool,
) -> bool {
    let failure = if !accepting {
        Some("Shutdown is waiting for accepted exports; new exports are not accepted.".into())
    } else if exports.is_full() {
        Some("Export queue is full; wait for the running export before starting another.".into())
    } else {
        match catalog.prepare_export(&destination, picks, &ids) {
            Ok(plan) => {
                // Emit queued before dispatch so a fast worker cannot report
                // running/completed ahead of its queued transition.
                export_job(events, id, "queued", 0, None);
                match exports.try_send(ExportRequest {
                    id,
                    plan,
                    destination,
                    picks,
                }) {
                    Ok(()) => {
                        telemetry(
                            events,
                            IoEvent::Status("Export queued; decisions continue saving.".into()),
                        );
                        None
                    }
                    Err(TrySendError::Full(_)) => Some("Export queue is full.".into()),
                    Err(TrySendError::Disconnected(_)) => Some("Export worker stopped.".into()),
                }
            }
            Err(error) => Some(format!("Export: {error:#}")),
        }
    };
    if let Some(message) = failure {
        export_job(events, id, "failed", 0, Some(message.clone()));
        reliable(events, IoEvent::Error(message));
        false
    } else {
        true
    }
}
fn telemetry(events: &Sender<IoEvent>, event: IoEvent) {
    if events.len() < TELEMETRY_LIMIT {
        let _ = events.send(event);
    }
}

fn replace<T>(slot: &Arc<Mutex<Option<T>>>, value: T) {
    // Drop obsolete snapshots outside the lock to keep UI try_lock inexpensive.
    let old = slot
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .replace(value);
    drop(old);
}
fn take<T>(slot: &Arc<Mutex<Option<T>>>) -> Option<T> {
    slot.lock().unwrap_or_else(|p| p.into_inner()).take()
}

fn publish(
    catalog: &Catalog,
    output: &PhotoSlot,
    grouping: &GroupWorker,
    events: &Sender<IoEvent>,
) {
    publish_with_hashes(catalog, output, grouping, events, &HashMap::new());
}

fn publish_with_hashes(
    catalog: &Catalog,
    output: &PhotoSlot,
    grouping: &GroupWorker,
    events: &Sender<IoEvent>,
    hashes: &HashMap<i64, u64>,
) {
    match catalog.photos() {
        Ok(mut photos) => {
            for photo in &mut photos {
                if let Some(&hash) = hashes.get(&photo.id) {
                    photo.dhash = Some(hash);
                }
            }
            photos.sort_unstable_by(|a, b| {
                a.captured_at
                    .unwrap_or(a.modified_at)
                    .cmp(&b.captured_at.unwrap_or(b.modified_at))
                    .then_with(|| a.path.cmp(&b.path))
                    .then_with(|| a.id.cmp(&b.id))
            });
            // Grouping receives only fields that its comparator/partitioner uses.
            let samples = photos
                .iter()
                .filter(|p| !p.trashed)
                .map(|p| Photo {
                    source: p.source.clone(),
                    id: p.id,
                    captured_at: p.captured_at,
                    capture_time_kind: p.capture_time_kind.clone(),
                    modified_at: p.modified_at,
                    folder: p.folder.clone(),
                    camera: p.camera.clone(),
                    dhash: p.dhash,
                    ..Photo::default()
                })
                .collect();
            grouping.photos(samples);
            replace(output, photos);
        }
        Err(error) => reliable(events, IoEvent::Error(format!("Read catalog: {error:#}"))),
    }
}

fn grouping_key(settings: &Settings) -> (&str, u64, u32) {
    (
        &settings.grouping_mode,
        settings.burst_gap_ms,
        settings.visual_threshold,
    )
}
fn options(settings: &Settings) -> GroupingOptions {
    GroupingOptions {
        mode: match settings.grouping_mode.as_str() {
            "time" => GroupingMode::Time,
            "visual" => GroupingMode::Visual,
            "burst" => GroupingMode::Burst,
            _ => GroupingMode::Off,
        },
        gap_ms: settings.burst_gap_ms,
        hamming_distance: settings.visual_threshold,
        ..GroupingOptions::default()
    }
}

struct GroupInput {
    revision: u64,
    photos: Option<Vec<Photo>>,
    hashes: HashMap<i64, u64>,
    options: GroupingOptions,
}
struct GroupWorker {
    input: Arc<Mutex<GroupInput>>,
    wake: Sender<()>,
    output: GroupSlot,
}

/// Coalesce only until an absolute deadline: a steady stream of preview hashes
/// must not postpone provisional time-based groups until the whole scan ends.
fn wait_for_group_batch(
    mut remaining: impl FnMut() -> Option<Duration>,
    mut receive: impl FnMut(Duration) -> Result<(), crossbeam_channel::RecvTimeoutError>,
) -> bool {
    while let Some(wait) = remaining() {
        match receive(wait) {
            Ok(()) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return false,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => return true,
        }
    }
    true
}

impl GroupWorker {
    fn start(output: GroupSlot, options: GroupingOptions) -> Self {
        let input = Arc::new(Mutex::new(GroupInput {
            revision: 0,
            photos: None,
            hashes: HashMap::new(),
            options,
        }));
        let work = input.clone();
        let result = output.clone();
        let (wake, receive) = bounded(1);
        thread::Builder::new()
            .name("shadowroom-grouping".into())
            .spawn(move || {
                let mut photos = Vec::<Photo>::new();
                let mut positions = HashMap::new();
                // Incremental grouping: hash batches only recompute affected
                // components. Rebuilt on snapshot or option changes.
                let mut cache: Option<(GroupingCache, (GroupingMode, u64, u32, usize))> = None;
                while receive.recv().is_ok() {
                    let deadline = Instant::now() + GROUP_DEBOUNCE;
                    if !wait_for_group_batch(
                        || deadline.checked_duration_since(Instant::now()),
                        |remaining| receive.recv_timeout(remaining),
                    ) {
                        return;
                    }
                    let (revision, replacement, hashes, options) = {
                        let mut next = work.lock().unwrap_or_else(|p| p.into_inner());
                        (
                            next.revision,
                            next.photos.take(),
                            std::mem::take(&mut next.hashes),
                            next.options,
                        )
                    };
                    if let Some(replacement) = replacement {
                        photos = replacement;
                        positions = photos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
                        cache = None;
                    }
                    let mut updates = Vec::with_capacity(hashes.len());
                    for (id, hash) in hashes {
                        if let Some(&position) = positions.get(&id) {
                            photos[position].dhash = Some(hash);
                            updates.push((id, hash));
                        }
                    }
                    let key = (
                        options.mode,
                        options.gap_ms,
                        options.hamming_distance,
                        options.max_neighbors,
                    );
                    if cache.as_ref().is_some_and(|(_, k)| *k != key) {
                        cache = None;
                    }
                    let groups = if options.mode == GroupingMode::Off {
                        cache = None;
                        Vec::new()
                    } else if let Some((grouping, _)) = cache.as_mut() {
                        grouping.update_hashes(updates);
                        grouping.groups()
                    } else {
                        let grouping = GroupingCache::new(&photos, options);
                        let groups = grouping.groups();
                        cache = Some((grouping, key));
                        groups
                    };
                    let current = work.lock().unwrap_or_else(|p| p.into_inner());
                    if revision == current.revision {
                        replace(&result, groups);
                    }
                }
            })
            .expect("Start grouping worker");
        Self {
            input,
            wake,
            output,
        }
    }
    fn update(&self, update: impl FnOnce(&mut GroupInput)) {
        let mut state = self.input.lock().unwrap_or_else(|p| p.into_inner());
        state.revision = state.revision.wrapping_add(1);
        update(&mut state);
        let obsolete = take(&self.output);
        drop(state);
        drop(obsolete);
        let _ = self.wake.try_send(());
    }
    fn photos(&self, photos: Vec<Photo>) {
        self.update(|input| {
            input.photos = Some(photos);
            input.hashes.clear();
        });
    }
    fn hash(&self, id: i64, hash: u64) {
        self.update(|input| {
            input.hashes.insert(id, hash);
        });
    }
    fn configure(&self, options: GroupingOptions) {
        self.update(|input| input.options = options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColorLabel, Flag};

    fn decision(id: i64, rating: u8) -> PhotoDecision {
        PhotoDecision {
            id,
            rating,
            flag: Flag::Pick,
            label: ColorLabel::Green,
            trashed: false,
        }
    }
    fn shutdown(commands: &Sender<IoCommand>) {
        let (ack, done) = bounded(1);
        commands.send(IoCommand::Shutdown(ack)).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    fn flush(commands: &Sender<IoCommand>) {
        let (ack, done) = bounded(1);
        commands.send(IoCommand::Flush(ack)).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    fn wal_commits(catalog: &Catalog) -> usize {
        let mut path = catalog.path().as_os_str().to_os_string();
        path.push("-wal");
        let bytes = std::fs::read(PathBuf::from(path)).unwrap();
        let page = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let page = if page == 1 { 65536 } else { page };
        bytes[32..]
            .chunks_exact(page + 24)
            .filter(|frame| u32::from_be_bytes(frame[4..8].try_into().unwrap()) != 0)
            .count()
    }
    #[test]
    fn hash_burst_uses_few_commits_preserves_decisions_and_flushes_latest_values() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        let photos: Vec<_> = (0..100)
            .map(|i| Photo {
                path: dir.path().join(format!("photo-{i}.jpg")),
                ..Photo::default()
            })
            .collect();
        catalog.upsert_photos(&photos).unwrap();
        let photos = catalog.photos().unwrap();
        let target = photos[50].id;
        let connection = rusqlite::Connection::open(catalog.path()).unwrap();
        let raw: String = connection
            .query_row("SELECT data FROM photos WHERE id=?1", [target], |row| {
                row.get(0)
            })
            .unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&raw).unwrap();
        metadata["future_metadata"] = serde_json::json!({"keep":true});
        connection
            .execute(
                "UPDATE photos SET data=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&metadata).unwrap(), target],
            )
            .unwrap();
        let before = wal_commits(&catalog);
        let (commands, events, _, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
            Arc::new(ExportPlan::execute),
            Arc::new(|_| Ok(())),
        );
        for (index, photo) in photos.iter().enumerate() {
            commands
                .send(IoCommand::Hash(photo.id, u64::MAX - photo.id as u64))
                .unwrap();
            if index == 50 {
                commands
                    .send(IoCommand::Decisions(vec![decision(target, 5)]))
                    .unwrap();
            }
        }
        // The most recent sparse update wins, including values outside signed SQLite integers.
        commands.send(IoCommand::Hash(target, u64::MAX)).unwrap();
        flush(&commands);
        let after = wal_commits(&catalog);
        assert!(
            after.saturating_sub(before) <= 6,
            "hash burst made {} commits",
            after.saturating_sub(before)
        );
        let saved = catalog.photos().unwrap();
        for photo in saved {
            assert_eq!(
                photo.dhash,
                Some(if photo.id == target {
                    u64::MAX
                } else {
                    u64::MAX - photo.id as u64
                })
            );
            if photo.id == target {
                assert_eq!(photo.rating, 5);
                assert_eq!(photo.flag, Flag::Pick);
                assert_eq!(photo.label, ColorLabel::Green);
            }
        }
        let raw: String = connection
            .query_row("SELECT data FROM photos WHERE id=?1", [target], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()["future_metadata"]["keep"],
            true
        );
        assert_eq!(
            events
                .try_iter()
                .filter(|e| matches!(e, IoEvent::Saved))
                .count(),
            1
        );
        // Shutdown also drains a sub-threshold pending hash without waiting for debounce.
        commands.send(IoCommand::Hash(target, 7)).unwrap();
        shutdown(&commands);
        assert_eq!(
            Catalog::open(catalog.path())
                .unwrap()
                .photos()
                .unwrap()
                .into_iter()
                .find(|p| p.id == target)
                .unwrap()
                .dhash,
            Some(7)
        );
    }
    #[test]
    fn failed_shutdown_keeps_preferences_queued_and_acknowledges_actual_saved_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let failure = failed.clone();
        let written = Arc::new(Mutex::new(Vec::<Settings>::new()));
        let output = written.clone();
        let writer: PreferenceWriter = Arc::new(move |settings| {
            if failure.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("simulated preference failure");
            }
            output.lock().unwrap().push(settings.clone());
            Ok(())
        });
        let (commands, events, _, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
            Arc::new(ExportPlan::execute),
            writer,
        );
        let mut preferences = Settings {
            interface_font: "Geist Mono".into(),
            auto_discover_drives: false,
            ..Settings::default()
        };
        preferences.normalize();
        commands
            .send(IoCommand::Settings(preferences.clone()))
            .unwrap();
        let (ack, done) = bounded(1);
        commands.send(IoCommand::Shutdown(ack)).unwrap();
        assert!(matches!(
            done.recv_timeout(Duration::from_secs(3)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        ));
        let failures: Vec<_> = events.try_iter().collect();
        assert!(failures.iter().any(
            |e| matches!(e,IoEvent::PreferencesFailed(message) if message.contains("simulated"))
        ));
        assert!(
            !failures
                .iter()
                .any(|e| matches!(e, IoEvent::PreferencesSaved(_)))
        );
        failed.store(false, std::sync::atomic::Ordering::SeqCst);
        flush(&commands);
        let acknowledged: Vec<_> = events
            .try_iter()
            .filter_map(|e| match e {
                IoEvent::PreferencesSaved(value) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(acknowledged, vec![preferences.clone()]);
        assert_eq!(*written.lock().unwrap(), acknowledged);
        preferences.theme = "Shadowroom Light".into();
        commands
            .send(IoCommand::Settings(preferences.clone()))
            .unwrap();
        shutdown(&commands);
        assert!(
            events
                .try_iter()
                .any(|e| matches!(e,IoEvent::PreferencesSaved(saved) if saved==preferences))
        );
        assert_eq!(written.lock().unwrap().len(), 2);
    }
    #[test]
    fn unchanged_import_does_not_reload_but_explicit_refresh_does() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        std::fs::create_dir(&root).unwrap();
        image::RgbImage::new(4, 3)
            .save(root.join("frame.bmp"))
            .unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        Indexer::start(catalog.clone(), root.clone(), IndexOptions::default())
            .join()
            .unwrap();
        let (commands, events, photos, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Default::default()
            },
            None,
            Arc::new(ExportPlan::execute),
            Arc::new(|_| Ok(())),
        );
        flush(&commands); // initial publication precedes command processing
        assert_eq!(take(&photos).unwrap().len(), 1);
        commands.send(IoCommand::Import(root.clone())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(
                events
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap(),
                IoEvent::Busy(false)
            ) {
                break;
            }
        }
        assert!(take(&photos).is_none());
        commands.send(IoCommand::Refresh).unwrap();
        flush(&commands);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(snapshot) = take(&photos) {
                assert_eq!(snapshot.len(), 1);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        // Changed metadata still publishes on completion, even without an
        // intermediate progress event on a very short scan.
        image::RgbImage::new(5, 4)
            .save(root.join("frame.bmp"))
            .unwrap();
        commands.send(IoCommand::Import(root)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(
                events
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap(),
                IoEvent::Busy(false)
            ) {
                break;
            }
        }
        assert_eq!(take(&photos).unwrap()[0].width, 5);
        shutdown(&commands);
    }
    #[test]
    fn snapshot_slot_retains_only_the_latest_and_sort_is_worker_side() {
        let slot: PhotoSlot = Arc::new(Mutex::new(None));
        for revision in 0..1000 {
            replace(
                &slot,
                vec![Photo {
                    id: revision,
                    ..Photo::default()
                }],
            );
        }
        assert_eq!(take(&slot).unwrap()[0].id, 999);
        assert!(take(&slot).is_none());
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        for (name, time) in [("c.jpg", 2), ("b.jpg", 1), ("a.jpg", 1)] {
            catalog
                .upsert_photos(&[Photo {
                    path: dir.path().join(name),
                    modified_at: time,
                    ..Photo::default()
                }])
                .unwrap();
        }
        let output: GroupSlot = Arc::new(Mutex::new(None));
        let grouping = GroupWorker::start(output, GroupingOptions::default());
        let (events, _) = unbounded();
        publish(&catalog, &slot, &grouping, &events);
        let photos = take(&slot).unwrap();
        assert_eq!(
            photos
                .iter()
                .map(|p| p.path.file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a.jpg", "b.jpg", "c.jpg"]
        );
    }
    #[test]
    fn decisions_remain_responsive_while_shutdown_waits_for_all_exports() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: dir.path().join("source.jpg"),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        let (entered, started) = bounded(2);
        let (release, blocked) = bounded::<()>(1);
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct WorkerLifetime(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for WorkerLifetime {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let lifetime = WorkerLifetime(stopped.clone());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written = writes.clone();
        let exporter: Exporter = Arc::new(move |_| {
            let _worker_lifetime = &lifetime;
            entered.send(()).unwrap();
            let _ = blocked.recv();
            written.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(1)
        });
        let preference_path = dir.path().join("settings.json");
        let preference_output = preference_path.clone();
        let (commands, events, _, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
            exporter,
            Arc::new(move |settings| settings.save(&preference_output)),
        );
        commands
            .send(IoCommand::ExportTracked {
                id: 1,
                destination: dir.path().join("export"),
                picks: false,
                ids: vec![id],
            })
            .unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        commands
            .send(IoCommand::Decisions(vec![decision(id, 5)]))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                IoEvent::Saved => break,
                IoEvent::SaveFailed(message) => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(catalog.photos().unwrap()[0].rating, 5);
        commands
            .send(IoCommand::Settings(Settings {
                interface_font: "DM Mono".into(),
                auto_discover_drives: false,
                ..Settings::default()
            }))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !preference_path.exists() {
            assert!(
                Instant::now() < deadline,
                "preferences blocked behind export"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            Settings::load(&preference_path).unwrap().interface_font,
            "DM Mono"
        );
        // One queued export must drain too. Shutdown cannot detach either job,
        // and the catalog actor must keep saving decisions while it waits.
        commands
            .send(IoCommand::ExportTracked {
                id: 2,
                destination: dir.path().join("queued-export"),
                picks: false,
                ids: vec![id],
            })
            .unwrap();
        let (ack, done) = bounded(1);
        commands.send(IoCommand::Shutdown(ack)).unwrap();
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(100)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        ));
        commands
            .send(IoCommand::Decisions(vec![decision(id, 3)]))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                IoEvent::Saved => break,
                IoEvent::SaveFailed(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(catalog.photos().unwrap()[0].rating, 3);
        flush(&commands);
        release.send(()).unwrap();
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(100)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            stopped.load(std::sync::atomic::Ordering::SeqCst),
            "acknowledgement preceded export worker exit"
        );
        let mut completed: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                IoEvent::ExportJob { id, state, .. } if state == "completed" => Some(id),
                _ => None,
            })
            .collect();
        completed.sort_unstable();
        assert_eq!(completed, vec![1, 2]);
        assert_eq!(
            Catalog::open(catalog.path()).unwrap().photos().unwrap()[0].rating,
            3
        );
    }
    #[test]
    fn fifo_shutdown_flushes_all_earlier_decisions_without_consuming_ack_events() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: dir.path().join("a.jpg"),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        let (commands, events, _, _) = start_io(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
        );
        for i in 0..200 {
            commands
                .send(IoCommand::Decisions(vec![decision(id, (i % 6) as u8)]))
                .unwrap();
        }
        shutdown(&commands);
        assert_eq!(catalog.photos().unwrap()[0].rating, 1);
        assert_eq!(
            events
                .try_iter()
                .filter(|e| matches!(e, IoEvent::Saved))
                .count(),
            200
        );
    }
    #[test]
    fn flush_is_a_fifo_barrier_and_keeps_actor_alive() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        catalog
            .upsert_photos(&[Photo {
                path: dir.path().join("a.jpg"),
                ..Photo::default()
            }])
            .unwrap();
        let id = catalog.photos().unwrap()[0].id;
        let preference_path = dir.path().join("settings.json");
        let output = preference_path.clone();
        let (commands, events, _, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
            Arc::new(ExportPlan::execute),
            Arc::new(move |settings| settings.save(&output)),
        );
        commands
            .send(IoCommand::Settings(Settings {
                interface_font: "Geist Mono".into(),
                auto_discover_drives: false,
                ..Settings::default()
            }))
            .unwrap();
        commands
            .send(IoCommand::Decisions(vec![decision(id, 4)]))
            .unwrap();
        let (ack, done) = bounded(1);
        commands.send(IoCommand::Flush(ack)).unwrap();
        done.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            Settings::load(&preference_path).unwrap().interface_font,
            "Geist Mono"
        );
        assert_eq!(catalog.photos().unwrap()[0].rating, 4);
        assert!(events.try_iter().any(|e| matches!(e, IoEvent::Saved)));
        commands
            .send(IoCommand::Decisions(vec![decision(id, 2)]))
            .unwrap();
        shutdown(&commands);
        assert_eq!(catalog.photos().unwrap()[0].rating, 2);
    }
    #[test]
    fn decisions_stay_responsive_during_duplicate_reads_and_shutdown_joins_scan() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&directory.path().join("catalog.sqlite3")).unwrap();
        let mut source_photos = Vec::new();
        for name in ["a.raw", "b.raw"] {
            let path = directory.path().join(name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(1024 * 1024 * 1024).unwrap();
            source_photos.push(Photo {
                path,
                ..Default::default()
            });
        }
        catalog.upsert_photos(&source_photos).unwrap();
        let id = catalog.photos().unwrap()[0].id;
        let (commands, events, _, _) = start_io(
            directory.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Default::default()
            },
            None,
        );
        commands
            .send(IoCommand::ScanDuplicates {
                id: 42,
                ids: Vec::new(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if matches!(event, IoEvent::DuplicateScan(ref state) if state.stage == "verifying" && state.phase == "running")
            {
                break;
            }
        }
        let trashed = PhotoDecision {
            trashed: true,
            ..decision(id, 4)
        };
        commands.send(IoCommand::Decisions(vec![trashed])).unwrap();
        loop {
            match events.recv_timeout(Duration::from_secs(3)).unwrap() {
                IoEvent::Saved => break,
                IoEvent::DuplicateScan(state) => assert_ne!(
                    state.phase, "completed",
                    "decision writer waited for the entire source scan"
                ),
                _ => {}
            }
        }
        shutdown(&commands);
        assert_eq!(PhotoDecision::from(&catalog.photos().unwrap()[0]), trashed);
        assert!(events.try_iter().any(|event| matches!(event,IoEvent::DuplicateScan(state) if state.id==42 && state.phase=="cancelled")));
        for photo in source_photos {
            assert_eq!(
                std::fs::metadata(photo.path).unwrap().len(),
                1024 * 1024 * 1024
            );
        }
    }
    #[test]
    fn continuous_group_notifications_cannot_extend_the_batch_deadline() {
        let budget = Duration::from_millis(350);
        let mut elapsed = Duration::ZERO;
        let mut waits = Vec::new();
        // A wake is always ready. Advance an injected monotonic clock rather
        // than depending on scheduler speed or asserting wall-clock latency.
        assert!(wait_for_group_batch(
            || {
                let remaining = budget.checked_sub(elapsed);
                elapsed += Duration::from_millis(100);
                remaining.filter(|wait| !wait.is_zero())
            },
            |remaining| {
                waits.push(remaining);
                Ok(())
            },
        ));
        assert_eq!(waits, [350, 250, 150, 50].map(Duration::from_millis));
        assert!(!wait_for_group_batch(
            || Some(budget),
            |_| Err(crossbeam_channel::RecvTimeoutError::Disconnected),
        ));
    }
    #[test]
    fn grouping_replaces_obsolete_work_and_applies_hashes_off_ui_thread() {
        let output: GroupSlot = Arc::new(Mutex::new(None));
        let grouping = GroupWorker::start(
            output.clone(),
            GroupingOptions {
                mode: GroupingMode::Visual,
                gap_ms: 1000,
                hamming_distance: 0,
                ..GroupingOptions::default()
            },
        );
        grouping.photos(vec![Photo {
            id: 100,
            ..Photo::default()
        }]);
        grouping.photos(vec![
            Photo {
                id: 1,
                ..Photo::default()
            },
            Photo {
                id: 2,
                ..Photo::default()
            },
        ]);
        grouping.hash(1, 0);
        grouping.hash(2, 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(groups) = take(&output) {
                assert_eq!(groups, vec![vec![1, 2]]);
                break;
            }
            assert!(Instant::now() < deadline, "grouping did not complete");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn published_groups_preserve_clock_and_remote_boundaries() {
        use crate::remote::protocol::{PhotoKey, PhotoRecord};
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        let photos: Vec<_> = ["offset", "camera_local", "", "mtime"]
            .into_iter()
            .cycle()
            .take(8)
            .enumerate()
            .map(|(i, clock)| Photo {
                path: dir.path().join(format!("photo-{i}.jpg")),
                folder: "same folder".into(),
                camera: "same camera".into(),
                captured_at: (clock != "mtime").then_some(1_700_000_000_000),
                capture_time_kind: clock.into(),
                modified_at: 1_700_000_000_000,
                dhash: Some(0),
                ..Photo::default()
            })
            .collect();
        catalog.upsert_photos(&photos).unwrap();
        for (device, root) in [("a", "one"), ("b", "one"), ("a", "two")] {
            let records: Vec<_> = (0..2)
                .map(|i| PhotoRecord {
                    key: PhotoKey {
                        root_id: root.into(),
                        volume_id: "card".into(),
                        relative_path: format!("same folder/remote-{i}.jpg").into(),
                        size_bytes: 1,
                        modified_at: 1_700_000_000_000,
                        companion_path: None,
                        companion_size_bytes: 0,
                        companion_modified_at: 0,
                    },
                    metadata: photos[0].clone(),
                    revision: 1,
                })
                .collect();
            catalog
                .stage_remote("sync", device, root, &records, &[])
                .unwrap();
            catalog
                .finish_remote_sync("sync", device, root, 1, true)
                .unwrap();
        }
        for mode in [
            GroupingMode::Time,
            GroupingMode::Visual,
            GroupingMode::Burst,
        ] {
            let options = GroupingOptions {
                mode,
                ..GroupingOptions::default()
            };
            let output: GroupSlot = Arc::new(Mutex::new(None));
            let grouping = GroupWorker::start(output.clone(), options);
            let photos: PhotoSlot = Arc::new(Mutex::new(None));
            let (events, _) = unbounded();
            publish_with_hashes(&catalog, &photos, &grouping, &events, &HashMap::new());
            let expected = crate::grouping::group_photos(&take(&photos).unwrap(), options);
            assert_eq!(expected.len(), 7);
            assert!(expected.iter().all(|group| group.len() == 2));
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(actual) = take(&output) {
                    assert_eq!(actual, expected, "{mode:?} lost grouping provenance");
                    break;
                }
                assert!(Instant::now() < deadline, "grouping did not complete");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[test]
    fn tracked_export_reports_ordered_transitions_and_partial_file_count() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("good.jpg"), b"good").unwrap();
        std::fs::write(source.join("missing.nef"), b"raw").unwrap();
        let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
        catalog
            .upsert_photos(&[
                Photo {
                    path: source.join("good.jpg"),
                    ..Photo::default()
                },
                Photo {
                    path: source.join("missing.nef"),
                    companion_path: Some(source.join("missing.jpg")),
                    ..Photo::default()
                },
            ])
            .unwrap();
        for photo in catalog.photos().unwrap() {
            catalog.set_flag(photo.id, Flag::Pick).unwrap();
        }
        let (commands, events, _, _) = start_io(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
        );
        commands
            .send(IoCommand::ExportTracked {
                id: 7,
                destination: dir.path().join("out"),
                picks: true,
                ids: vec![],
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut states = Vec::new();
        loop {
            if let IoEvent::ExportJob {
                id,
                state,
                completed,
                error,
            } = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                assert_eq!(id, 7);
                states.push(state.clone());
                if state == "failed" {
                    assert_eq!(completed, 1);
                    assert!(error.unwrap().contains("manifest:"));
                    break;
                }
            }
        }
        assert_eq!(states, vec!["queued", "running", "failed"]);
        shutdown(&commands);
    }
    #[test]
    fn tracked_export_queue_is_bounded_and_failure_is_reliable() {
        let dir = tempfile::tempdir().unwrap();
        let (release, blocked) = bounded::<()>(2);
        let exporter: Exporter = Arc::new(move |_| {
            blocked.recv().unwrap();
            Ok(2)
        });
        let (commands, events, _, _) = start_io_with_exporter(
            dir.path().into(),
            Settings {
                auto_discover_drives: false,
                ..Settings::default()
            },
            None,
            exporter,
            Arc::new(|_| Ok(())),
        );
        let request = |id| IoCommand::ExportTracked {
            id,
            destination: dir.path().join(format!("out-{id}")),
            picks: false,
            ids: vec![],
        };
        commands.send(request(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut transitions = HashMap::<u64, Vec<String>>::new();
        loop {
            if let IoEvent::ExportJob { id, state, .. } = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                transitions.entry(id).or_default().push(state.clone());
                if id == 1 && state == "running" {
                    break;
                }
            }
        }
        commands.send(request(2)).unwrap();
        commands.send(request(3)).unwrap();
        loop {
            if let IoEvent::ExportJob {
                id,
                state,
                completed,
                error,
            } = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                transitions.entry(id).or_default().push(state.clone());
                if id == 3 {
                    assert_eq!(state, "failed");
                    assert_eq!(completed, 0);
                    assert!(error.unwrap().contains("queue is full"));
                    break;
                }
            }
        }
        release.send(()).unwrap();
        release.send(()).unwrap();
        while transitions
            .get(&2)
            .is_none_or(|states| states.last().map(String::as_str) != Some("completed"))
        {
            if let IoEvent::ExportJob {
                id,
                state,
                completed,
                error,
            } = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                if state == "completed" {
                    assert_eq!(completed, 2);
                    assert!(error.is_none());
                }
                transitions.entry(id).or_default().push(state);
            }
        }
        assert_eq!(transitions[&1], vec!["queued", "running", "completed"]);
        assert_eq!(transitions[&2], vec!["queued", "running", "completed"]);
        assert_eq!(transitions[&3], vec!["failed"]);
        shutdown(&commands);
    }
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Finished-index status: plural-correct, and silent about zero counts.
fn index_summary(progress: &crate::indexer::IndexProgress) -> String {
    let mut parts = Vec::new();
    if progress.indexed > 0 || progress.unchanged == 0 {
        parts.push(format!(
            "Indexed {}",
            count(progress.indexed, "photo", "photos")
        ));
    }
    if progress.unchanged > 0 {
        parts.push(format!(
            "{} up to date",
            count(progress.unchanged, "photo", "photos")
        ));
    }
    if progress.errors > 0 {
        parts.push(count(progress.errors, "error", "errors"));
    }
    parts.join(" · ")
}
