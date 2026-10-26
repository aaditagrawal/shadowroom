//! Bounded command dispatcher. Ingest work never holds up the catalog IO actor.
use super::*;
use crate::service::IoEvent;
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    process::Command as ProcessCommand,
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, JoinHandle},
};

#[derive(Clone, Debug)]
pub enum Command {
    Scan {
        path: PathBuf,
        auto: Option<IngestOptions>,
    },
    ScanCard {
        path: PathBuf,
        auto: Option<IngestOptions>,
    },
    Plan(IngestOptions),
    Start,
    Pause,
    Resume,
    Cancel,
    History(usize),
    MarkImported(Vec<usize>),
    Eject(PathBuf),
}
#[derive(Clone, Debug)]
pub enum Event {
    Scanning(PathBuf),
    Batch(Vec<IngestGroup>),
    Scanned(Arc<Scan>),
    Plan(Arc<PlanSummary>),
    Progress(Progress),
    History(serde_json::Value),
    Marked(usize),
    Committed(Vec<(usize, super::store::SeenRecord)>),
    Ejected(PathBuf),
    Error(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanSummary {
    pub plan: IngestPlan,
    pub space: Vec<super::copy::SpaceProjection>,
    pub ready: bool,
    pub error: Option<String>,
}
pub struct Worker {
    tx: Sender<Command>,
    control: Control,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(catalog: Catalog, events: Sender<IoEvent>, changed: Sender<()>) -> Self {
        let (tx, rx) = bounded(32);
        let control = Control::default();
        let worker_control = control.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop_worker = stopping.clone();
        let thread = thread::Builder::new()
            .name("shadowroom-ingest".into())
            .spawn(move || run(catalog, events, changed, rx, worker_control, stop_worker))
            .expect("Start ingest worker");
        Self {
            tx,
            control,
            stopping,
            thread: Some(thread),
        }
    }
    pub fn send(&self, command: Command) -> Result<()> {
        // Cancellation is immediate even while a scan reads bounded metadata.
        if matches!(command, Command::Cancel) {
            self.control.cancel();
        }
        self.tx
            .try_send(command)
            .context("Import command queue is full or disconnected")?;
        Ok(())
    }
    pub fn stop(mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.control.cancel();
        drop(self.tx);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Ingest worker panicked"))?;
        }
        Ok(())
    }
}
fn emit(events: &Sender<IoEvent>, event: Event) {
    let _ = events.send(IoEvent::Ingest(event));
}
fn run(
    catalog: Catalog,
    events: Sender<IoEvent>,
    changed: Sender<()>,
    incoming: Receiver<Command>,
    control: Control,
    stopping: Arc<AtomicBool>,
) {
    match catalog.ingest_reconcile_abandoned() {
        Ok(count) if count > 0 => {
            if let Ok(history) = catalog.ingest_history(100) {
                emit(&events, Event::History(history));
            }
        }
        Err(error) => emit(
            &events,
            Event::Error(format!("Recover abandoned ingest history: {error:#}")),
        ),
        _ => {}
    }
    let mut scanned: Option<Scan> = None;
    let mut planned: Option<IngestPlan> = None;
    let mut job: Option<JoinHandle<()>> = None;
    while let Ok(command) = incoming.recv() {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if job
            .as_ref()
            .is_some_and(|j| j.is_finished() || control.is_finished())
        {
            let _ = job.take().unwrap().join();
        }
        let card_scan = matches!(&command, Command::ScanCard { .. });
        let result = (|| -> Result<()> {
            match command {
                Command::Scan { path, auto } | Command::ScanCard { path, auto } => {
                    ensure!(
                        job.is_none(),
                        "An import is running; wait or cancel before scanning another source"
                    );
                    control.reset();
                    scanned = None;
                    planned = None;
                    emit(&events, Event::Scanning(path.clone()));
                    let mut callback = |batch: &[IngestGroup]| {
                        if events.len() < 32 {
                            emit(&events, Event::Batch(batch.to_vec()));
                        }
                    };
                    let result = if card_scan {
                        scan_card(&path, &catalog, &mut callback, &control)?
                    } else {
                        scan(&path, &catalog, &mut callback, &control)?
                    };
                    emit(&events, Event::Scanned(Arc::new(result.clone())));
                    scanned = Some(result);
                    if let Some(options) = auto {
                        planned = Some(make_plan(
                            scanned.as_ref().unwrap(),
                            options,
                            &catalog,
                            &events,
                        )?);
                        ensure!(
                            !planned.as_ref().unwrap().groups.is_empty(),
                            "No new media to import"
                        );
                        super::copy::preflight(planned.as_ref().unwrap(), &NativeFsStats)?;
                        job = Some(start_job(
                            planned.as_ref().unwrap().clone(),
                            catalog.clone(),
                            control.clone(),
                            events.clone(),
                            changed.clone(),
                        ));
                    }
                }
                Command::Plan(options) => {
                    ensure!(job.is_none(), "An import is already running");
                    // A failed replacement request must not leave the old plan startable.
                    planned = None;
                    let scan = scanned.as_mut().context("Scan a source folder first")?;
                    // Snapshot the latest cull decisions after all earlier IO commands.
                    let decisions = catalog.ingest_source_decisions(&scan.source)?;
                    for group in &mut scan.groups {
                        if let Some(decision) = decisions.get(&group.primary.path).or_else(|| {
                            group
                                .companion
                                .as_ref()
                                .and_then(|f| decisions.get(&f.path))
                        }) {
                            group.photo.id = decision.id;
                            group.photo.rating = decision.rating;
                            group.photo.flag = decision.flag;
                            group.photo.label = decision.label;
                            group.photo.trashed = decision.trashed;
                        }
                    }
                    planned = Some(make_plan(scan, options, &catalog, &events)?);
                }
                Command::Start => {
                    ensure!(job.is_none(), "An import is already running");
                    let plan = planned.as_ref().context("Compute an import plan first")?;
                    ensure!(!plan.groups.is_empty(), "No media selected for import");
                    super::copy::preflight(plan, &NativeFsStats)?;
                    control.reset();
                    job = Some(start_job(
                        plan.clone(),
                        catalog.clone(),
                        control.clone(),
                        events.clone(),
                        changed.clone(),
                    ));
                }
                Command::Pause => {
                    ensure!(job.is_some(), "No active import");
                    control.pause();
                    emit(
                        &events,
                        Event::Progress(Progress {
                            phase: "paused".into(),
                            ..Default::default()
                        }),
                    );
                }
                Command::Resume => {
                    ensure!(job.is_some(), "No active import");
                    control.resume();
                    emit(
                        &events,
                        Event::Progress(Progress {
                            phase: "copying".into(),
                            ..Default::default()
                        }),
                    );
                }
                Command::Cancel => control.cancel(),
                Command::History(limit) => {
                    emit(&events, Event::History(catalog.ingest_history(limit)?))
                }
                Command::MarkImported(ids) => {
                    ensure!(job.is_none(), "Cannot mark files during import");
                    let scan = scanned.as_mut().context("Scan a source folder first")?;
                    let ids: HashSet<_> = ids.into_iter().collect();
                    ensure!(
                        !ids.is_empty()
                            && ids.is_subset(&scan.groups.iter().map(|group| group.id).collect()),
                        "Choose valid scan group IDs to mark"
                    );
                    let selected: Vec<_> = scan
                        .groups
                        .iter()
                        .filter(|g| ids.contains(&g.id))
                        .cloned()
                        .collect();
                    let count = catalog.ingest_mark_imported(&selected)?;
                    for group in &mut scan.groups {
                        if ids.contains(&group.id) {
                            group.seen = catalog.ingest_seen(group)?;
                            group.selected = false;
                        }
                    }
                    planned = None;
                    emit(&events, Event::Scanned(Arc::new(scan.clone())));
                    emit(&events, Event::Marked(count));
                }
                Command::Eject(path) => {
                    ensure!(
                        job.is_none(),
                        "Import is still using the device; wait for completion before ejecting"
                    );
                    eject(&path)?;
                    emit(&events, Event::Ejected(path));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            emit(&events, Event::Error(format!("{error:#}")));
        }
    }
    control.cancel();
    if let Some(job) = job {
        let _ = job.join();
    }
}
fn make_plan(
    scan: &Scan,
    options: IngestOptions,
    catalog: &Catalog,
    events: &Sender<IoEvent>,
) -> Result<IngestPlan> {
    let plan = plan(scan, options, catalog.ingest_sequences()?)?;
    let space = super::copy::space(&plan, &NativeFsStats)?;
    let error = super::copy::preflight(&plan, &NativeFsStats)
        .err()
        .map(|e| format!("{e:#}"));
    emit(
        events,
        Event::Plan(Arc::new(PlanSummary {
            ready: error.is_none(),
            error,
            plan: plan.clone(),
            space,
        })),
    );
    Ok(plan)
}
fn start_job(
    plan: IngestPlan,
    catalog: Catalog,
    control: Control,
    events: Sender<IoEvent>,
    changed: Sender<()>,
) -> JoinHandle<()> {
    start_job_with_hooks(
        plan,
        catalog,
        control,
        events,
        changed,
        Arc::new(DefaultHooks),
    )
}
pub(super) fn start_job_with_hooks(
    plan: IngestPlan,
    catalog: Catalog,
    control: Control,
    events: Sender<IoEvent>,
    changed: Sender<()>,
    hooks: Arc<dyn CopyHooks>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("shadowroom-ingest-copy".into())
        .spawn(move || {
            let mut latest = Progress {
                total: plan.groups.len(),
                total_bytes: plan.bytes,
                library_root: Some(plan.options.library_root.clone()),
                ..Default::default()
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_with(
                    &plan,
                    &catalog,
                    &control,
                    &NativeFsStats,
                    hooks.as_ref(),
                    |progress| {
                        latest = progress.clone();
                        if events.len() < 64
                            && !["completed", "failed", "cancelled"]
                                .contains(&progress.phase.as_str())
                        {
                            emit(&events, Event::Progress(progress.clone()));
                        }
                    },
                )
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Import worker panicked")));
            match result {
                Ok(report) => {
                    let imported: HashSet<_> = report
                        .records
                        .iter()
                        .map(|record| (&record.source_volume, &record.source_relative))
                        .collect();
                    let committed = plan
                        .groups
                        .iter()
                        .filter(|g| imported.contains(&(&g.group.volume, &g.group.relative_path)))
                        .filter_map(|g| {
                            catalog
                                .ingest_seen(&g.group)
                                .ok()
                                .flatten()
                                .map(|seen| (g.group.id, seen))
                        })
                        .collect();
                    emit(&events, Event::Committed(committed));
                    let mut indexing = report.progress.clone();
                    indexing.phase = "indexing".into();
                    emit(&events, Event::Progress(indexing));
                    // Reuse the normal indexer for dimensions/format metadata. Decisions
                    // are already attached to destination rows at each group commit.
                    let mut folders: Vec<_> = report
                        .records
                        .iter()
                        .flat_map(|r| r.destinations.first())
                        .filter_map(|p| p.parent().map(Path::to_path_buf))
                        .collect();
                    folders.sort();
                    folders.dedup();
                    for folder in folders {
                        let index = indexer::Indexer::start(
                            catalog.clone(),
                            folder,
                            indexer::IndexOptions::default(),
                        );
                        while !index.is_finished() {
                            if control.is_cancelled() {
                                index.cancel();
                            }
                            let _ = index.recv_timeout(Duration::from_millis(50));
                        }
                        if let Err(error) = index.join() {
                            emit(
                                &events,
                                Event::Error(format!("Index imported files: {error:#}")),
                            );
                        }
                    }
                    let _ = changed.try_send(());
                    if let Ok(history) = catalog.ingest_history(100) {
                        emit(&events, Event::History(history));
                    }
                    control.finish();
                    emit(&events, Event::Progress(report.progress.clone()));
                }
                Err(error) => {
                    // Job failures are terminal; ancillary command errors remain
                    // ordinary Event::Error diagnostics in the dispatcher.
                    latest.phase = "failed".into();
                    latest.errors.push(format!("{error:#}"));
                    let _ = changed.try_send(());
                    control.finish();
                    emit(&events, Event::Progress(latest));
                    emit(&events, Event::Error(format!("{error:#}")));
                }
            }
        })
        .expect("Start import copy worker")
}
/// Only discovered removable mount roots are eligible. A temp directory is never
/// passed to a system eject command, including through automation.
pub fn eject(path: &Path) -> Result<()> {
    let path = path.canonicalize()?;
    let drive = indexer::discover_drives()
        .into_iter()
        .find(|d| d.removable && d.path.canonicalize().ok().as_ref() == Some(&path))
        .context("Eject unsupported: path is not a discovered removable volume mount root")?;
    let _ = drive;
    #[cfg(target_os = "linux")]
    {
        let output = ProcessCommand::new("gio")
            .args(["mount", "-e"])
            .arg(&path)
            .output()
            .context("Run gio mount -e (install a desktop volume manager)")?;
        ensure!(
            output.status.success(),
            "Eject failed (device may be busy): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let output = ProcessCommand::new("diskutil")
            .arg("eject")
            .arg(&path)
            .output()?;
        ensure!(
            output.status.success(),
            "Eject failed (device may be busy): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        anyhow::bail!("Eject is unsupported on this platform")
    }
}
