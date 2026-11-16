//! Bounded command dispatcher. Ingest work never holds up the catalog IO actor.
use super::*;
use crate::service::IoEvent;
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    process::Command as ProcessCommand,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
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
    /// Stop the running import only; the next queued card starts.
    Cancel,
    /// Drop one waiting import (by its source) without touching the others.
    Dequeue(PathBuf),
    /// Drop the waiting imports and let the running one finish ("Clear queue",
    /// app exit).
    ClearQueue,
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
    /// Acknowledges a pause or resume with the job's real progress: `Paused`
    /// while paused, otherwise the phase the job resumed into.
    Control(Progress),
    History(serde_json::Value),
    Marked(usize),
    /// Groups of the scan of `source` recorded as imported. A view showing
    /// another card (scanned while this job copies) must not apply them.
    Committed {
        source: PathBuf,
        groups: Vec<(usize, super::store::SeenRecord)>,
    },
    /// Imports waiting for the running job, in the order they will start.
    Queue(Vec<QueuedJob>),
    Ejected(PathBuf),
    Error(String),
}
/// An import accepted while another job copies; it starts when that job ends.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedJob {
    pub source: PathBuf,
    pub volume: String,
    pub photos: usize,
    pub videos: usize,
    pub bytes: u64,
}
impl QueuedJob {
    fn of(plan: &IngestPlan) -> Self {
        Self {
            source: plan.source.clone(),
            volume: plan
                .groups
                .first()
                .map(|g| g.group.volume.clone())
                .unwrap_or_default(),
            photos: plan.photos,
            videos: plan.videos,
            bytes: plan.bytes,
        }
    }
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
    /// Scans read only; they never share the copy job's pause/cancel state, so
    /// a second card can be scanned while a job copies.
    scan_control: Control,
    /// Bumped by every ClearQueue: queued imports from before it never start,
    /// even when the running job ends before the dispatcher reads it.
    cancel_epoch: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    activity: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(catalog: Catalog, events: Sender<IoEvent>, changed: Sender<()>) -> Self {
        Self::spawn(catalog, events, changed, Arc::new(DefaultHooks))
    }
    /// A worker whose copy jobs run through `hooks` (fault injection in tests).
    #[cfg(test)]
    pub(super) fn start_with_hooks(
        catalog: Catalog,
        events: Sender<IoEvent>,
        changed: Sender<()>,
        hooks: Arc<dyn CopyHooks>,
    ) -> Self {
        Self::spawn(catalog, events, changed, hooks)
    }
    fn spawn(
        catalog: Catalog,
        events: Sender<IoEvent>,
        changed: Sender<()>,
        hooks: Arc<dyn CopyHooks>,
    ) -> Self {
        let (tx, rx) = bounded(32);
        let control = Control::default();
        let worker_control = control.clone();
        let scan_control = Control::default();
        let worker_scan_control = scan_control.clone();
        let cancel_epoch = Arc::new(AtomicUsize::new(0));
        let worker_epoch = cancel_epoch.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop_worker = stopping.clone();
        let activity = Arc::new(AtomicUsize::new(0));
        let worker_activity = activity.clone();
        let thread = thread::Builder::new()
            .name("shadowroom-ingest".into())
            .spawn(move || {
                run(
                    catalog,
                    events,
                    changed,
                    rx,
                    Lanes {
                        control: worker_control,
                        scan: worker_scan_control,
                        cancel_epoch: worker_epoch,
                        hooks,
                    },
                    stop_worker,
                    worker_activity,
                )
            })
            .expect("Start ingest worker");
        Self {
            tx,
            control,
            scan_control,
            cancel_epoch,
            stopping,
            activity,
            thread: Some(thread),
        }
    }
    pub fn send(&self, command: Command) -> Result<()> {
        // Cancellation is immediate even while a scan reads bounded metadata,
        // and so are pause and resume: a second card's scan may be running on
        // the dispatcher, but the copy job must answer at once (the
        // acknowledgement follows from the dispatcher).
        match command {
            // Cancel stops the copying card, or the scan when nothing copies;
            // a second card being scanned (or queued) is left alone.
            Command::Cancel if self.control.snapshot().phase.is_active() => self.control.cancel(),
            Command::Cancel => self.scan_control.cancel(),
            Command::ClearQueue => {
                self.cancel_epoch.fetch_add(1, Ordering::AcqRel);
            }
            Command::Pause if self.control.snapshot().phase.is_active() => self.control.pause(),
            Command::Resume if self.control.snapshot().phase.is_active() => self.control.resume(),
            _ => {}
        }
        self.activity.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.tx.try_send(command) {
            self.activity.fetch_sub(1, Ordering::AcqRel);
            return Err(error).context("Import command queue is full or disconnected");
        }
        Ok(())
    }
    /// Includes accepted-but-not-started commands and paused copy jobs.
    pub fn is_busy(&self) -> bool {
        self.activity.load(Ordering::Acquire) != 0
    }
    pub fn stop(mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.control.cancel();
        self.scan_control.cancel();
        drop(self.tx);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("Ingest worker panicked"))?;
        }
        Ok(())
    }
}
struct Activity(Arc<AtomicUsize>);
impl Activity {
    fn start(activity: &Arc<AtomicUsize>) -> Self {
        activity.fetch_add(1, Ordering::AcqRel);
        Self(activity.clone())
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Signals the dispatcher when the job thread ends, however it ends.
struct Done(Option<Sender<()>>);
impl Drop for Done {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            let _ = done.try_send(());
        }
    }
}
fn emit(events: &Sender<IoEvent>, event: Event) {
    let _ = events.send(IoEvent::Ingest(event));
}
/// The dispatcher's controls: the copy job's, the scans', and the queue
/// generation that retires queued imports.
struct Lanes {
    control: Control,
    scan: Control,
    cancel_epoch: Arc<AtomicUsize>,
    hooks: Arc<dyn CopyHooks>,
}
/// The running copy job and the imports waiting for it.
struct Jobs {
    running: Option<JoinHandle<()>>,
    /// Source of the running job: a scan or import of it is refused.
    source: Option<PathBuf>,
    /// Volume of the running job: the same card mounted at another path is
    /// that job's card too (it resumes there), never a new card to scan.
    volume: Option<String>,
    /// Waiting imports, each with the cancel generation it was accepted in and
    /// an activity guard (app exit and deletion wait for queued work too).
    queue: std::collections::VecDeque<(IngestPlan, usize, Activity)>,
}
impl Jobs {
    fn summary(&self) -> Vec<QueuedJob> {
        self.queue
            .iter()
            .map(|(plan, _, _)| QueuedJob::of(plan))
            .collect()
    }
    fn busy_with(&self, path: &Path, hooks: &dyn CopyHooks) -> bool {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.source.as_ref() == Some(&path)
            || self
                .volume
                .as_deref()
                .filter(|volume| volume.starts_with("uuid:"))
                .is_some_and(|volume| hooks.volume_mounts(volume).contains(&path))
    }
    fn queued(&self, path: &Path) -> bool {
        self.queue.iter().any(|(plan, _, _)| plan.source == path)
    }
}
fn run(
    catalog: Catalog,
    events: Sender<IoEvent>,
    changed: Sender<()>,
    incoming: Receiver<Command>,
    lanes: Lanes,
    stopping: Arc<AtomicBool>,
    activity: Arc<AtomicUsize>,
) {
    let control = lanes.control;
    let scan_control = lanes.scan;
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
    let mut jobs = Jobs {
        running: None,
        source: None,
        volume: None,
        queue: std::collections::VecDeque::new(),
    };
    // A finished job wakes the dispatcher, which starts the next queued import.
    let (done_tx, done_rx) = bounded::<()>(4);
    let launch = |plan: IngestPlan, jobs: &mut Jobs, guard: Activity| {
        control.reset();
        // A pause/resume acknowledged before the job thread first
        // reports must read as active, never as `Idle`.
        control.report(&Progress {
            phase: Phase::Queued,
            source: Some(plan.source.clone()),
            ..Progress::default()
        });
        jobs.source = Some(plan.source.clone());
        jobs.volume = plan.groups.first().map(|g| g.group.volume.clone());
        jobs.running = Some(start_job_with_hooks_and_activity(
            plan,
            catalog.clone(),
            control.clone(),
            events.clone(),
            changed.clone(),
            lanes.hooks.clone(),
            Some(guard),
            Some(done_tx.clone()),
        ));
    };
    loop {
        let command = crossbeam_channel::select! {
            recv(incoming) -> command => match command {
                Ok(command) => Some(command),
                Err(_) => break,
            },
            recv(done_rx) -> _ => None,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if jobs
            .running
            .as_ref()
            .is_some_and(|j| j.is_finished() || control.is_finished())
        {
            let _ = jobs.running.take().unwrap().join();
            jobs.source = None;
            jobs.volume = None;
        }
        let Some(command) = command else {
            // The job ended (or was cancelled): start the oldest import still
            // waiting, unless Clear queue since it was queued retired it.
            if jobs.running.is_none() && !jobs.queue.is_empty() {
                let epoch = lanes.cancel_epoch.load(Ordering::Acquire);
                while let Some((plan, accepted, guard)) = jobs.queue.pop_front() {
                    if accepted != epoch {
                        continue;
                    }
                    match super::copy::preflight(&plan, &NativeFsStats) {
                        Ok(_) => {
                            launch(plan, &mut jobs, guard);
                            break;
                        }
                        Err(error) => emit(
                            &events,
                            Event::Error(format!(
                                "Queued import of {} cannot start: {error:#}",
                                plan.source.display()
                            )),
                        ),
                    }
                }
                emit(&events, Event::Queue(jobs.summary()));
            }
            continue;
        };
        let _command_activity = Activity(activity.clone());
        let card_scan = matches!(&command, Command::ScanCard { .. });
        let result = (|| -> Result<()> {
            match command {
                Command::Scan { path, auto } | Command::ScanCard { path, auto } => {
                    if jobs.busy_with(&path, lanes.hooks.as_ref())
                        || (auto.is_some() && jobs.queued(&path))
                    {
                        // The card being copied came back (reinserted after a
                        // pull): its job continues by itself; never rescan it
                        // or queue it twice.
                        return Ok(());
                    }
                    scan_control.reset();
                    scanned = None;
                    planned = None;
                    emit(&events, Event::Scanning(path.clone()));
                    let mut callback = |batch: &[IngestGroup]| {
                        if events.len() < 32 {
                            emit(&events, Event::Batch(batch.to_vec()));
                        }
                    };
                    let result = if card_scan {
                        scan_card(&path, &catalog, &mut callback, &scan_control)?
                    } else {
                        scan(&path, &catalog, &mut callback, &scan_control)?
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
                        let plan = planned.as_ref().unwrap();
                        ensure!(!plan.groups.is_empty(), "No new media to import");
                        super::copy::preflight(plan, &NativeFsStats)?;
                        if jobs.running.is_some() {
                            jobs.queue.push_back((
                                plan.clone(),
                                lanes.cancel_epoch.load(Ordering::Acquire),
                                Activity::start(&activity),
                            ));
                            emit(&events, Event::Queue(jobs.summary()));
                        } else {
                            launch(plan.clone(), &mut jobs, Activity::start(&activity));
                        }
                    }
                }
                Command::Plan(options) => {
                    // Planning is pure plus free-space reads, so a second card
                    // can be planned (and queued) while a job copies.
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
                    let plan = planned.as_ref().context("Compute an import plan first")?;
                    ensure!(!plan.groups.is_empty(), "No media selected for import");
                    super::copy::preflight(plan, &NativeFsStats)?;
                    if jobs.running.is_some() {
                        ensure!(
                            !jobs.busy_with(&plan.source, lanes.hooks.as_ref()),
                            "This source is already importing"
                        );
                        ensure!(!jobs.queued(&plan.source), "This source is already queued");
                        jobs.queue.push_back((
                            plan.clone(),
                            lanes.cancel_epoch.load(Ordering::Acquire),
                            Activity::start(&activity),
                        ));
                        emit(&events, Event::Queue(jobs.summary()));
                    } else {
                        launch(plan.clone(), &mut jobs, Activity::start(&activity));
                    }
                }
                Command::Pause => {
                    ensure!(jobs.running.is_some(), "No active import");
                    control.pause();
                    emit(&events, Event::Control(control.snapshot()));
                }
                Command::Resume => {
                    ensure!(jobs.running.is_some(), "No active import");
                    control.resume();
                    emit(&events, Event::Control(control.snapshot()));
                }
                Command::Cancel => {
                    // Cancel stops the running job only (already done in
                    // `Worker::send`); its end starts the next queued card.
                    if jobs.running.is_some() {
                        control.cancel();
                    }
                }
                Command::Dequeue(source) => {
                    let before = jobs.queue.len();
                    jobs.queue.retain(|(plan, _, _)| plan.source != source);
                    if jobs.queue.len() != before {
                        emit(&events, Event::Queue(jobs.summary()));
                    }
                }
                Command::ClearQueue => {
                    if !jobs.queue.is_empty() {
                        jobs.queue.clear();
                        emit(&events, Event::Queue(Vec::new()));
                    }
                }
                Command::History(limit) => {
                    emit(&events, Event::History(catalog.ingest_history(limit)?))
                }
                Command::MarkImported(ids) => {
                    ensure!(jobs.running.is_none(), "Cannot mark files during import");
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
                    let mut marked = Vec::new();
                    for group in &mut scan.groups {
                        if ids.contains(&group.id) {
                            group.seen = catalog.ingest_seen(group)?;
                            group.selected = false;
                            if let Some(seen) = &group.seen {
                                marked.push((group.id, seen.clone()));
                            }
                        }
                    }
                    planned = None;
                    // Update only the marked groups. Re-sending the whole scan would
                    // reset every other checkbox the user changed to scan defaults.
                    emit(
                        &events,
                        Event::Committed {
                            source: scan.source.clone(),
                            groups: marked,
                        },
                    );
                    emit(&events, Event::Marked(count));
                }
                Command::Eject(path) => {
                    // Another card than the one copying may go: it is not read.
                    let path_key = path.canonicalize().unwrap_or_else(|_| path.clone());
                    ensure!(
                        !jobs.busy_with(&path, lanes.hooks.as_ref()) && !jobs.queued(&path_key),
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
    jobs.queue.clear();
    if let Some(job) = jobs.running {
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
#[cfg(test)]
pub(super) fn start_job_with_hooks(
    plan: IngestPlan,
    catalog: Catalog,
    control: Control,
    events: Sender<IoEvent>,
    changed: Sender<()>,
    hooks: Arc<dyn CopyHooks>,
) -> JoinHandle<()> {
    start_job_with_hooks_and_activity(plan, catalog, control, events, changed, hooks, None, None)
}
#[allow(clippy::too_many_arguments)]
fn start_job_with_hooks_and_activity(
    plan: IngestPlan,
    catalog: Catalog,
    control: Control,
    events: Sender<IoEvent>,
    changed: Sender<()>,
    hooks: Arc<dyn CopyHooks>,
    activity: Option<Activity>,
    done: Option<Sender<()>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("shadowroom-ingest-copy".into())
        .spawn(move || {
            let _activity = activity;
            // Wakes the dispatcher once every event of this job is sent.
            let _done = Done(done);
            let mut latest = Progress {
                total: plan.groups.len(),
                total_bytes: plan.bytes,
                library_root: Some(plan.options.library_root.clone()),
                source: Some(plan.source.clone()),
                phase: Phase::Queued,
                ..Default::default()
            };
            control.report(&latest);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_with(
                    &plan,
                    &catalog,
                    &control,
                    &NativeFsStats,
                    hooks.as_ref(),
                    |progress| {
                        latest = progress.clone();
                        control.report(progress);
                        if events.len() < 64 && !progress.phase.is_terminal() {
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
                    emit(
                        &events,
                        Event::Committed {
                            source: plan.source.clone(),
                            groups: committed,
                        },
                    );
                    let mut indexing = report.progress.clone();
                    indexing.phase = Phase::Indexing;
                    control.report(&indexing);
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
                    latest.phase = Phase::Failed;
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

#[cfg(test)]
mod busy_tests {
    use super::*;

    struct Mounted(Vec<PathBuf>);
    impl CopyHooks for Mounted {
        fn volume_mounts(&self, volume: &str) -> Vec<PathBuf> {
            if volume == "uuid:7E57-CA4D" {
                self.0.clone()
            } else {
                Vec::new()
            }
        }
    }

    fn jobs(volume: &str) -> Jobs {
        Jobs {
            running: None,
            source: Some(PathBuf::from("/run/media/u/EOS_DIGITAL")),
            volume: Some(volume.into()),
            queue: std::collections::VecDeque::new(),
        }
    }

    #[test]
    fn the_copying_card_at_a_new_mount_point_is_busy_not_a_new_card() {
        let moved = PathBuf::from("/run/media/u/EOS_DIGITAL1");
        let hooks = Mounted(vec![moved.clone()]);
        let copying = jobs("uuid:7E57-CA4D");
        assert!(copying.busy_with(Path::new("/run/media/u/EOS_DIGITAL"), &hooks));
        assert!(copying.busy_with(&moved, &hooks));
        assert!(!copying.busy_with(Path::new("/run/media/u/CARD_B"), &hooks));
        // A label is not an identity: the same name may be another card.
        let label = jobs("label:EOS_DIGITAL");
        assert!(!label.busy_with(&moved, &Mounted(vec![moved.clone()])));
    }
}
