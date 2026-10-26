//! Self-contained regressions: all media bytes, catalogs and destinations are test-owned.
use super::*;
use std::{
    fs::File,
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

struct Fixture {
    temp: tempfile::TempDir,
    source: PathBuf,
    catalog: Catalog,
}
impl Fixture {
    fn new() -> Self {
        // Ingest stores resolved paths; keep expected paths identical on
        // systems whose temporary directory includes a symlink (macOS /var).
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = temp.path().join("card");
        fs::create_dir(&source).unwrap();
        let catalog = Catalog::open(&temp.path().join("catalog.sqlite3")).unwrap();
        Self {
            temp,
            source,
            catalog,
        }
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.source.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
    fn scan(&self) -> Scan {
        scan(&self.source, &self.catalog, |_| {}, &Control::default()).unwrap()
    }
    fn plan(&self) -> IngestPlan {
        let scan = self.scan();
        plan(
            &scan,
            IngestOptions {
                library_root: self.temp.path().join("library"),
                folder_template: String::new(),
                file_template: "{original}".into(),
                include_videos: true,
                // Explicit selection also exercises retry after durable history is present.
                selected_ids: Some(scan.groups.iter().map(|g| g.id).collect()),
                ..Default::default()
            },
            self.catalog.ingest_sequences().unwrap(),
        )
        .unwrap()
    }
    fn run(&self, plan: &IngestPlan, hooks: &dyn CopyHooks) -> Report {
        execute_with(
            plan,
            &self.catalog,
            &Control::default(),
            &Capacity,
            hooks,
            |_| {},
        )
        .unwrap()
    }
    fn assert_rolled_back(&self, report: &Report) {
        assert_eq!(report.progress.phase, "failed", "{:?}", report.progress);
        assert_eq!(report.progress.completed, 0);
        assert_eq!(report.progress.failed, 1);
        assert_eq!(self.catalog.ingest_sequences().unwrap().stored, 0);
        assert_eq!(
            self.catalog.ingest_history(10).unwrap()["total_imported_groups"],
            0
        );
        for entry in walkdir::WalkDir::new(self.temp.path().join("library"))
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_file() {
                assert!(
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("shadowroom-ingest-"),
                    "uncommitted file left behind: {}",
                    entry.path().display()
                );
            }
        }
        assert_no_parts(self.temp.path());
    }
}
struct Capacity;
impl FsStats for Capacity {
    fn capacity(&self, _: &Path) -> Result<FsCapacity> {
        Ok(FsCapacity {
            filesystem: "test".into(),
            available: u64::MAX,
        })
    }
}
struct Hook<F>(F);
impl<F> CopyHooks for Hook<F>
where
    F: Fn(&Path, &mut File, &[u8]) -> Result<()> + Send + Sync,
{
    fn write_chunk(&self, path: &Path, output: &mut File, bytes: &[u8]) -> Result<()> {
        (self.0)(path, output, bytes)
    }
}
fn assert_no_parts(root: &Path) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        assert!(
            !entry
                .file_name()
                .to_string_lossy()
                .contains("shadowroom-part"),
            "staging file left behind: {}",
            entry.path().display()
        );
    }
}

#[test]
fn restart_does_not_skip_a_different_same_fingerprint_sidecar() {
    let f = Fixture::new();
    f.write("A.mov", b"primary video");
    let old = f.write("A.log", b"old log");
    let timestamp = fs::metadata(&old).unwrap().modified().unwrap();
    assert_eq!(f.run(&f.plan(), &DefaultHooks).progress.completed, 1);
    fs::remove_file(old).unwrap();
    let new = f.write("A.wav", b"new wav");
    File::options()
        .write(true)
        .open(new)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    let plan = f.plan();
    let report = f.run(&plan, &DefaultHooks);
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.progress.skipped, 0);
    assert_eq!(
        fs::read(plan.options.library_root.join("A.wav")).unwrap(),
        b"new wav"
    );
    assert_eq!(
        fs::read(plan.options.library_root.join("A.log")).unwrap(),
        b"old log"
    );
    assert_no_parts(f.temp.path());
}

#[test]
fn restart_does_not_skip_changed_source_bytes_with_preserved_size_and_mtime() {
    let f = Fixture::new();
    let source = f.write("A.mov", b"original video");
    let plan = f.plan();
    let timestamp = fs::metadata(&source).unwrap().modified().unwrap();
    assert_eq!(f.run(&plan, &DefaultHooks).progress.completed, 1);
    fs::write(&source, b"replaced video").unwrap();
    File::options()
        .write(true)
        .open(source)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    let report = f.run(&plan, &DefaultHooks);
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.progress.skipped, 0);
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    assert_eq!(
        fs::read(plan.options.library_root.join("A-1.mov")).unwrap(),
        b"replaced video"
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 2);
    assert_no_parts(f.temp.path());
}

#[test]
fn source_mutation_in_writer_after_reader_finishes_rolls_back() {
    let f = Fixture::new();
    let source = f.write("A.mov", b"original video");
    let plan = f.plan();
    let report = f.run(
        &plan,
        &Hook(|_: &Path, output: &mut File, bytes: &[u8]| {
            output.write_all(bytes)?;
            fs::write(&source, b"changed length during copy")?;
            Ok(())
        }),
    );
    f.assert_rolled_back(&report);
    assert!(report.progress.errors[0].contains("Source changed"));
}

#[cfg(unix)]
#[test]
fn source_inode_replacement_with_same_fingerprint_rolls_back() {
    let f = Fixture::new();
    let source = f.write("A.mov", b"original video");
    let timestamp = fs::metadata(&source).unwrap().modified().unwrap();
    let replacement = f.temp.path().join("replacement");
    fs::write(&replacement, b"replaced video").unwrap();
    File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    let plan = f.plan();
    let report = f.run(
        &plan,
        &Hook(|_: &Path, output: &mut File, bytes: &[u8]| {
            output.write_all(bytes)?;
            fs::rename(&replacement, &source)?;
            Ok(())
        }),
    );
    f.assert_rolled_back(&report);
    assert_eq!(fs::read(source).unwrap(), b"replaced video");
}

#[test]
fn earlier_member_changed_while_sidecar_copies_rolls_back_whole_group() {
    let f = Fixture::new();
    let source = f.write("A.mov", b"original video");
    f.write("A.xmp", b"metadata");
    let plan = f.plan();
    let report = f.run(
        &plan,
        &Hook(|path: &Path, output: &mut File, bytes: &[u8]| {
            output.write_all(bytes)?;
            if path.to_string_lossy().contains(".xmp.") {
                fs::write(&source, b"changed primary after it was staged")?;
            }
            Ok(())
        }),
    );
    f.assert_rolled_back(&report);
    assert!(report.progress.errors[0].contains("Source changed"));
}

#[cfg(unix)]
#[test]
fn source_replaced_by_symlink_after_scan_is_rejected() {
    let f = Fixture::new();
    let source = f.write("A.mov", b"original video");
    let plan = f.plan();
    let outside = f.temp.path().join("outside");
    fs::rename(&source, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &source).unwrap();
    let report = f.run(&plan, &DefaultHooks);
    f.assert_rolled_back(&report);
    assert_eq!(fs::read(outside).unwrap(), b"original video");
}

#[cfg(unix)]
#[test]
fn source_parent_replaced_by_symlink_after_scan_is_rejected() {
    let f = Fixture::new();
    let folder = f.source.join("DCIM");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("A.mov"), b"original video").unwrap();
    let plan = f.plan();
    let outside = f.temp.path().join("outside");
    fs::rename(&folder, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &folder).unwrap();
    f.assert_rolled_back(&f.run(&plan, &DefaultHooks));
    assert_eq!(fs::read(outside.join("A.mov")).unwrap(), b"original video");
}

#[test]
fn destination_writer_failure_preserves_injected_reason_and_cleans_up() {
    let f = Fixture::new();
    f.write("A.mov", &vec![7; 12 * 1024 * 1024]);
    let report = f.run(
        &f.plan(),
        &Hook(|_: &Path, _: &mut File, _: &[u8]| anyhow::bail!("injected destination disk full")),
    );
    f.assert_rolled_back(&report);
    assert!(
        report.progress.errors[0].contains("injected destination disk full"),
        "{:?}",
        report.progress.errors
    );
}

#[test]
fn restart_verifies_and_repairs_a_corrupted_backup_without_replacing_it() {
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let mut plan = f.plan();
    let backup = f.temp.path().join("backup");
    plan.options.backup_roots.push(backup.clone());
    assert_eq!(f.run(&plan, &DefaultHooks).progress.completed, 1);
    fs::write(backup.join("A.mov"), b"corrupted").unwrap();
    let report = f.run(&plan, &DefaultHooks);
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.progress.failed, 0);
    assert_eq!(fs::read(backup.join("A.mov")).unwrap(), b"corrupted");
    for root in [&backup, &plan.options.library_root] {
        assert_eq!(fs::read(root.join("A-1.mov")).unwrap(), b"original video");
    }
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 2);
    assert_no_parts(f.temp.path());
}

#[test]
fn cancel_while_paused_rolls_back_then_same_plan_restarts() {
    let f = Fixture::new();
    f.write("A.mov", &vec![9; 8 * 1024 * 1024]);
    let plan = f.plan();
    let control = Control::default();
    let (entered, received) = crossbeam_channel::bounded(1);
    let (done, result) = crossbeam_channel::bounded(1);
    let catalog = f.catalog.clone();
    let child_control = control.clone();
    let child_plan = plan.clone();
    let job = thread::spawn(move || {
        let first = AtomicBool::new(true);
        let hooks = Hook(|_: &Path, output: &mut File, bytes: &[u8]| {
            output.write_all(bytes)?;
            if first.swap(false, Ordering::SeqCst) {
                child_control.pause();
                entered.send(()).unwrap();
            }
            Ok(())
        });
        done.send(execute_with(
            &child_plan,
            &catalog,
            &child_control,
            &Capacity,
            &hooks,
            |_| {},
        ))
        .unwrap();
    });
    let paused = received.recv_timeout(Duration::from_secs(5));
    let before_cancel = result.recv_timeout(Duration::from_millis(50));
    control.cancel();
    assert!(paused.is_ok(), "writer never reached pause");
    assert!(matches!(
        before_cancel,
        Err(crossbeam_channel::RecvTimeoutError::Timeout)
    ));
    let report = result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    job.join().unwrap();
    assert_eq!(report.progress.phase, "cancelled");
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert_no_parts(f.temp.path());
    assert_eq!(f.run(&plan, &DefaultHooks).progress.completed, 1);
}

fn next_event(
    rx: &crossbeam_channel::Receiver<crate::service::IoEvent>,
    predicate: impl Fn(&worker::Event) -> bool,
) -> worker::Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining).expect("ingest event timeout") {
            crate::service::IoEvent::Ingest(event) if predicate(&event) => return event,
            crate::service::IoEvent::Ingest(worker::Event::Error(error)) => {
                panic!("unexpected ingest error: {error}")
            }
            _ => {}
        }
    }
}

#[test]
fn worker_failed_replan_invalidates_previous_startable_plan() {
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"video");
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    worker
        .send(Command::Scan {
            path: f.source.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    let mut options = f.plan().options;
    worker.send(Command::Plan(options.clone())).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(_)));
    options.file_template = "{unknown}".into();
    worker.send(Command::Plan(options)).unwrap();
    let error = next_event(&rx, |e| matches!(e, Event::Error(_)));
    assert!(matches!(error, Event::Error(e) if e.contains("Unknown naming token")));
    worker.send(Command::Start).unwrap();
    let error = next_event(&rx, |e| matches!(e, Event::Error(_) | Event::Progress(_)));
    worker.stop().unwrap();
    assert!(matches!(error, Event::Error(e) if e.contains("Compute an import plan first")));
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert!(!f.temp.path().join("library/A.mov").exists());
}

#[test]
fn worker_failed_new_scan_discards_previous_source() {
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"video");
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    worker
        .send(Command::Scan {
            path: f.source.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    worker
        .send(Command::Scan {
            path: f.temp.path().join("missing-card"),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Error(_)));
    worker.send(Command::Plan(f.plan().options)).unwrap();
    let event = next_event(&rx, |e| matches!(e, Event::Error(_) | Event::Plan(_)));
    worker.stop().unwrap();
    assert!(matches!(event, Event::Error(e) if e.contains("Scan a source folder first")));
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
}

#[test]
fn worker_final_history_update_failure_is_terminal_and_publishes_committed_photos() {
    use crate::model::{ColorLabel, PhotoDecision};
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    let source = f.source.join("A.jpg");
    image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]))
        .save(&source)
        .unwrap();
    let source_photo = f.scan().groups.remove(0).photo;
    f.catalog.upsert_photos(&[source_photo]).unwrap();
    let id = f.catalog.photos().unwrap()[0].id;
    f.catalog
        .set_decisions(&[PhotoDecision {
            id,
            rating: 5,
            flag: Flag::Pick,
            label: ColorLabel::Green,
            trashed: false,
        }])
        .unwrap();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER test_fail_ingest_finish BEFORE UPDATE ON ingest_jobs
        BEGIN SELECT RAISE(ABORT, 'injected final history failure'); END;",
    )
    .unwrap();
    drop(db);
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, published) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    worker
        .send(Command::Scan {
            path: f.source.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    let options = f.plan().options;
    let destination = options.library_root.join("A.jpg");
    worker.send(Command::Plan(options)).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(_)));
    worker.send(Command::Start).unwrap();
    let committed = next_event(&rx, |e| matches!(e, Event::Committed(_)));
    assert!(matches!(committed, Event::Committed(records) if records.len() == 1));
    let terminal = next_event(&rx, |e| {
        matches!(e, Event::Progress(p)
        if ["completed", "failed", "cancelled"].contains(&p.phase.as_str()))
    });
    worker.stop().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, "failed");
    assert!(!progress.job_id.is_empty());
    assert_eq!(progress.completed, 1);
    assert_eq!(progress.total, 1);
    assert_eq!(progress.files, 1);
    assert_eq!(progress.photos, 1);
    assert!(
        progress
            .errors
            .iter()
            .any(|e| e.contains("injected final history failure"))
    );
    let manifest: Report = serde_json::from_slice(
        &fs::read(
            progress
                .manifest
                .as_ref()
                .expect("missing failure manifest"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.progress.phase, progress.phase);
    assert_eq!(manifest.progress.job_id, progress.job_id);
    assert_eq!(manifest.progress.completed, progress.completed);
    assert_eq!(manifest.progress.errors, progress.errors);
    assert_eq!(manifest.records.len(), 1);
    published
        .recv_timeout(Duration::from_secs(5))
        .expect("committed catalog changes were not published");
    assert_eq!(fs::read(&destination).unwrap(), fs::read(source).unwrap());
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 1);
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["total_imported_groups"],
        1
    );
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    let photos = reopened.photos().unwrap();
    let imported = photos.iter().find(|p| p.path == destination).unwrap();
    assert_eq!(imported.rating, 5);
    assert_eq!(imported.flag, Flag::Pick);
    assert_eq!(imported.label, ColorLabel::Green);
    assert_no_parts(f.temp.path());
}

#[test]
fn worker_job_begin_failure_emits_terminal_progress_with_job_identity() {
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"video");
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER test_fail_ingest_begin BEFORE INSERT ON ingest_jobs
        BEGIN SELECT RAISE(ABORT, 'injected job begin failure'); END;",
    )
    .unwrap();
    drop(db);
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    worker
        .send(Command::Scan {
            path: f.source.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    worker.send(Command::Plan(f.plan().options)).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(_)));
    worker.send(Command::Start).unwrap();
    let terminal = next_event(&rx, |e| {
        matches!(e, Event::Progress(p)
        if ["completed", "failed", "cancelled"].contains(&p.phase.as_str()))
    });
    worker.stop().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, "failed");
    assert!(!progress.job_id.is_empty());
    assert_eq!(progress.total, 1);
    assert_eq!(progress.completed, 0);
    assert!(
        progress
            .errors
            .iter()
            .any(|e| e.contains("injected job begin failure"))
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert!(!f.temp.path().join("library/A.mov").exists());
}

struct FailCommittedCleanup {
    first: AtomicBool,
    catalog: Catalog,
}
impl CopyHooks for FailCommittedCleanup {
    fn before_committed_cleanup(&self, _: &Path) -> Result<()> {
        if self.first.swap(false, Ordering::SeqCst) {
            assert_eq!(self.catalog.ingest_sequences()?.stored, 1);
            anyhow::bail!("injected postcommit cleanup failure");
        }
        Ok(())
    }
}

#[test]
fn postcommit_cleanup_failure_keeps_publication_counts_decisions_and_later_sequences() {
    use crate::model::ColorLabel;
    use worker::Event;
    let f = Fixture::new();
    for name in ["A.jpg", "B.jpg"] {
        image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]))
            .save(f.source.join(name))
            .unwrap();
    }
    let mut scan = f.scan();
    scan.groups
        .sort_by(|a, b| a.primary.path.cmp(&b.primary.path));
    for group in &mut scan.groups {
        group.capture_ms = 1_700_000_000_000;
        group.capture_kind = "camera_local".into();
        group.photo.rating = 5;
        group.photo.flag = Flag::Pick;
        group.photo.label = ColorLabel::Green;
    }
    let mut options = f.plan().options;
    options.file_template = "{seq_stored:3}_{seq_session:3}_{seq_day:3}".into();
    let plan = plan(&scan, options, SequenceState::default()).unwrap();
    let library = plan.options.library_root.clone();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, published) = crossbeam_channel::bounded(1);
    let job = worker::start_job_with_hooks(
        plan,
        f.catalog.clone(),
        Control::default(),
        events,
        changed,
        Arc::new(FailCommittedCleanup {
            first: AtomicBool::new(true),
            catalog: f.catalog.clone(),
        }),
    );
    let committed = next_event(&rx, |e| matches!(e, Event::Committed(_)));
    assert!(matches!(committed, Event::Committed(records) if records.len() == 2));
    let terminal = next_event(&rx, |e| {
        matches!(e, Event::Progress(p)
        if ["completed", "failed", "cancelled"].contains(&p.phase.as_str()))
    });
    job.join().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, "failed");
    assert_eq!(progress.completed, 2);
    assert_eq!(
        progress.failed, 0,
        "cleanup is a job error, not an uncommitted group"
    );
    assert_eq!(progress.files, 2);
    assert_eq!(progress.photos, 2);
    assert!(
        progress
            .errors
            .iter()
            .any(|e| e.contains("injected postcommit cleanup failure"))
    );
    published.recv_timeout(Duration::from_secs(5)).unwrap();
    let report: Report =
        serde_json::from_slice(&fs::read(progress.manifest.as_ref().unwrap()).unwrap()).unwrap();
    assert_eq!(report.records.len(), 2);
    assert_eq!(report.progress.phase, progress.phase);
    assert_eq!(report.progress.completed, progress.completed);
    assert_eq!(report.progress.errors, progress.errors);
    for (source, destination) in [("A.jpg", "001_001_001.jpg"), ("B.jpg", "002_002_002.jpg")] {
        assert_eq!(
            fs::read(f.source.join(source)).unwrap(),
            fs::read(library.join(destination)).unwrap()
        );
    }
    let state = f.catalog.ingest_sequences().unwrap();
    assert_eq!(state.stored, 2);
    assert_eq!(state.days.values().copied().sum::<u64>(), 2);
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    let photos = reopened.photos().unwrap();
    assert_eq!(photos.len(), 2);
    for photo in photos {
        assert_eq!(photo.rating, 5);
        assert_eq!(photo.flag, Flag::Pick);
        assert_eq!(photo.label, ColorLabel::Green);
    }
    let history = reopened.ingest_history(10).unwrap();
    assert_eq!(history["jobs"][0]["verification"], "failed");
    assert_eq!(history["jobs"][0]["counts"]["completed"], 2);
    assert_eq!(history["total_imported_groups"], 2);
    assert_no_parts(f.temp.path());
}

struct CorruptManifest;
impl CopyHooks for CorruptManifest {
    fn before_manifest_verify(&self, path: &Path) -> Result<()> {
        fs::write(path, b"injected manifest corruption")?;
        Ok(())
    }
}

#[test]
fn manifest_verification_failure_removes_artifact_and_persists_failed_job() {
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let report = f.run(&plan, &CorruptManifest);
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.records.len(), 1);
    assert!(report.progress.manifest.is_none());
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("Manifest"))
    );
    let history = f.catalog.ingest_history(10).unwrap();
    assert_eq!(history["jobs"][0]["verification"], "failed");
    assert_eq!(history["jobs"][0]["counts"]["phase"], "failed");
    assert_eq!(history["jobs"][0]["counts"]["completed"], 1);
    assert_eq!(
        history["jobs"][0]["errors"],
        serde_json::to_value(&report.progress.errors).unwrap()
    );
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    assert!(
        !plan
            .options
            .library_root
            .join(format!("shadowroom-ingest-{}.json", report.progress.job_id))
            .exists()
    );
    assert_no_parts(f.temp.path());
}

#[test]
fn manifest_failure_and_rejected_failure_update_report_both_errors_without_success_artifact() {
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_failed_job BEFORE UPDATE ON ingest_jobs
        WHEN NEW.verification = 'failed'
        BEGIN SELECT RAISE(ABORT, 'injected failed history refusal'); END;",
    )
    .unwrap();
    drop(db);
    let report = f.run(&plan, &CorruptManifest);
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.records.len(), 1);
    assert!(report.progress.manifest.is_none());
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("Manifest"))
    );
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("injected failed history refusal"))
    );
    assert!(
        !plan
            .options
            .library_root
            .join(format!("shadowroom-ingest-{}.json", report.progress.job_id))
            .exists()
    );
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    // Artifact failure cannot certify success, even while failure UPDATEs refuse.
    let history = f.catalog.ingest_history(10).unwrap();
    assert_eq!(history["jobs"][0]["verification"], "finalizing");
    assert!(history["jobs"][0]["finished"].is_null());
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch("DROP TRIGGER reject_failed_job").unwrap();
    drop(db);
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = worker::Worker::start(reopened.clone(), events, changed);
    let event = next_event(&rx, |e| matches!(e, worker::Event::History(_)));
    worker.stop().unwrap();
    let worker::Event::History(history) = event else {
        unreachable!()
    };
    assert_eq!(history["jobs"][0]["verification"], "failed");
    assert_eq!(history["jobs"][0]["counts"]["completed"], 1);
    assert!(!history["jobs"][0]["finished"].is_null());
    assert_eq!(reopened.ingest_sequences().unwrap().stored, 1);
    assert_eq!(history["total_imported_groups"], 1);
}

#[test]
fn startup_reconciles_rejected_finalization_using_committed_log_counts() {
    use worker::{Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_finish BEFORE UPDATE ON ingest_jobs
        BEGIN SELECT RAISE(ABORT, 'injected history failure before restart'); END;",
    )
    .unwrap();
    let failed = f.run(&plan, &DefaultHooks);
    assert_eq!(failed.progress.phase, "failed");
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["jobs"][0]["verification"],
        "running"
    );
    db.execute_batch("DROP TRIGGER reject_finish").unwrap();
    drop(db);
    // A new catalog handle models reopening the previous process's durable state.
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(reopened.clone(), events, changed);
    let event = next_event(&rx, |e| matches!(e, Event::History(_)));
    worker.stop().unwrap();
    let Event::History(history) = event else {
        unreachable!()
    };
    let job = &history["jobs"][0];
    assert_eq!(job["verification"], "failed");
    assert!(!job["finished"].is_null());
    assert_eq!(job["counts"]["completed"], 1);
    assert_eq!(job["counts"]["total"], 1);
    assert_eq!(job["counts"]["files"], 1);
    assert_eq!(job["counts"]["videos"], 1);
    assert_eq!(job["counts"]["bytes"], failed.progress.bytes);
    assert!(
        job["errors"]
            .to_string()
            .contains("interrupted or its final history update failed")
    );
    assert_eq!(history["total_imported_groups"], 1);
    assert_eq!(reopened.ingest_sequences().unwrap().stored, 1);
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
}

#[test]
fn startup_reconciliation_refusal_is_explicit_and_does_not_claim_repaired_history() {
    use worker::{Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    f.catalog.ingest_begin("abandoned", &plan).unwrap();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_recovery BEFORE UPDATE ON ingest_jobs
        BEGIN SELECT RAISE(ABORT, 'injected recovery refusal'); END;",
    )
    .unwrap();
    drop(db);
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    let event = next_event(&rx, |e| matches!(e, Event::Error(_)));
    worker.stop().unwrap();
    assert!(
        matches!(event, Event::Error(error) if error.contains("Recover abandoned ingest history")
        && error.contains("injected recovery refusal"))
    );
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["jobs"][0]["verification"],
        "running"
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
}

#[test]
fn abandoned_history_reconciliation_waits_for_catalog_ingest_lease() {
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    f.catalog.ingest_begin("abandoned", &f.plan()).unwrap();
    let lease = f.catalog.ingest_lock.lock().unwrap();
    let catalog = f.catalog.clone();
    let (entered, started) = crossbeam_channel::bounded(1);
    let (done, result) = crossbeam_channel::bounded(1);
    let thread = thread::spawn(move || {
        entered.send(()).unwrap();
        done.send(catalog.ingest_reconcile_abandoned()).unwrap();
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let while_locked = result.recv_timeout(Duration::from_millis(50));
    drop(lease);
    let after_unlock = result.recv_timeout(Duration::from_secs(5));
    thread.join().unwrap();
    assert!(
        matches!(
            while_locked,
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        ),
        "reconciliation ignored the active job lease"
    );
    assert_eq!(after_unlock.unwrap().unwrap(), 1);
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["jobs"][0]["verification"],
        "failed"
    );
}

struct ObserveFinalizing {
    first: AtomicBool,
    catalog: Catalog,
}
impl CopyHooks for ObserveFinalizing {
    fn before_manifest_verify(&self, path: &Path) -> Result<()> {
        if self.first.swap(false, Ordering::SeqCst) {
            let history = self.catalog.ingest_history(10)?;
            assert_eq!(history["jobs"][0]["verification"], "finalizing");
            assert!(history["jobs"][0]["finished"].is_null());
            let candidate: Report = serde_json::from_slice(&fs::read(path)?)?;
            assert_eq!(candidate.progress.phase, "completed");
        }
        Ok(())
    }
}

#[test]
fn last_completed_history_write_failure_replaces_owned_manifest_with_failed_outcome() {
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_completed BEFORE UPDATE ON ingest_jobs
        WHEN NEW.verification = 'completed'
        BEGIN SELECT RAISE(ABORT, 'injected last completed write failure'); END;",
    )
    .unwrap();
    drop(db);
    let hooks = ObserveFinalizing {
        first: AtomicBool::new(true),
        catalog: f.catalog.clone(),
    };
    let report = f.run(&plan, &hooks);
    assert!(!hooks.first.load(Ordering::SeqCst));
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.records.len(), 1);
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("injected last completed write failure"))
    );
    let disk: Report =
        serde_json::from_slice(&fs::read(report.progress.manifest.as_ref().unwrap()).unwrap())
            .unwrap();
    assert_eq!(disk.progress.phase, "failed");
    assert_eq!(disk.progress.job_id, report.progress.job_id);
    assert_eq!(disk.progress.completed, report.progress.completed);
    assert_eq!(disk.progress.errors, report.progress.errors);
    assert_eq!(disk.records.len(), 1);
    let history = f.catalog.ingest_history(10).unwrap();
    assert_eq!(history["jobs"][0]["verification"], "failed");
    assert_eq!(
        history["jobs"][0]["errors"],
        serde_json::to_value(&report.progress.errors).unwrap()
    );
    assert_eq!(history["jobs"][0]["counts"]["completed"], 1);
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 1);
    assert_no_parts(f.temp.path());
}

#[test]
fn rejected_terminal_history_writes_leave_failed_manifest_and_recoverable_finalizing_row() {
    use worker::{Event, Worker};
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_terminal BEFORE UPDATE ON ingest_jobs
        WHEN NEW.verification IN ('completed', 'failed')
        BEGIN SELECT RAISE(ABORT, 'injected terminal outcome refusal'); END;",
    )
    .unwrap();
    let hooks = ObserveFinalizing {
        first: AtomicBool::new(true),
        catalog: f.catalog.clone(),
    };
    let report = f.run(&plan, &hooks);
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.completed, 1);
    let manifest = report.progress.manifest.as_ref().unwrap();
    let disk: Report = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(disk.progress.phase, "failed");
    assert_eq!(disk.progress.errors, report.progress.errors);
    assert_eq!(disk.progress.completed, 1);
    assert_eq!(disk.records.len(), 1);
    assert!(
        disk.progress
            .errors
            .iter()
            .any(|e| e.contains("Record failed ingest outcome"))
    );
    let history = f.catalog.ingest_history(10).unwrap();
    assert_eq!(history["jobs"][0]["verification"], "finalizing");
    assert!(history["jobs"][0]["finished"].is_null());
    db.execute_batch("DROP TRIGGER reject_terminal").unwrap();
    drop(db);
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(reopened.clone(), events, changed);
    let event = next_event(&rx, |e| matches!(e, Event::History(_)));
    worker.stop().unwrap();
    let Event::History(history) = event else {
        unreachable!()
    };
    assert_eq!(history["jobs"][0]["verification"], "failed");
    assert_eq!(history["jobs"][0]["counts"]["completed"], 1);
    assert_eq!(history["total_imported_groups"], 1);
    assert_eq!(reopened.ingest_sequences().unwrap().stored, 1);
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    let after_restart: Report = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(after_restart.progress.phase, "failed");
}

#[cfg(unix)]
#[test]
fn replaced_failed_manifest_path_is_never_adopted_or_deleted() {
    struct ReplaceFailedManifest;
    impl CopyHooks for ReplaceFailedManifest {
        fn before_manifest_verify(&self, path: &Path) -> Result<()> {
            let candidate: Report = serde_json::from_slice(&fs::read(path)?)?;
            if candidate.progress.phase == "failed" {
                fs::rename(path, path.with_extension("owned-moved"))?;
                fs::write(path, b"foreign entry must remain")?;
            }
            Ok(())
        }
    }
    let f = Fixture::new();
    f.write("A.mov", b"original video");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_completed BEFORE UPDATE ON ingest_jobs
        WHEN NEW.verification='completed'
        BEGIN SELECT RAISE(ABORT, 'injected last write refusal'); END;",
    )
    .unwrap();
    drop(db);
    let report = f.run(&plan, &ReplaceFailedManifest);
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.completed, 1);
    assert!(report.progress.manifest.is_none());
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("Owned artifact was replaced"))
    );
    assert!(
        report
            .progress
            .errors
            .iter()
            .any(|e| e.contains("Cleanup refused replaced path"))
    );
    let path = plan
        .options
        .library_root
        .join(format!("shadowroom-ingest-{}.json", report.progress.job_id));
    assert_eq!(fs::read(path).unwrap(), b"foreign entry must remain");
    assert_eq!(
        fs::read(plan.options.library_root.join("A.mov")).unwrap(),
        b"original video"
    );
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["jobs"][0]["verification"],
        "failed"
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 1);
}
