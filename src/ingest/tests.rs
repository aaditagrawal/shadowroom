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
        assert_eq!(
            report.progress.phase,
            Phase::Failed,
            "{:?}",
            report.progress
        );
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
    assert_eq!(report.progress.phase, Phase::Cancelled);
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
    let committed = next_event(&rx, |e| matches!(e, Event::Committed { .. }));
    assert!(matches!(committed, Event::Committed { groups: records, .. } if records.len() == 1));
    let terminal = next_event(&rx, |e| {
        matches!(e, Event::Progress(p)
        if p.phase.is_terminal())
    });
    worker.stop().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, Phase::Failed);
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
        if p.phase.is_terminal())
    });
    worker.stop().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, Phase::Failed);
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
            // Cleanup runs only after the group's (batched) commit.
            assert!(self.catalog.ingest_sequences()?.stored >= 1);
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
    let committed = next_event(&rx, |e| matches!(e, Event::Committed { .. }));
    assert!(matches!(committed, Event::Committed { groups: records, .. } if records.len() == 2));
    let terminal = next_event(&rx, |e| {
        matches!(e, Event::Progress(p)
        if p.phase.is_terminal())
    });
    job.join().unwrap();
    let Event::Progress(progress) = terminal else {
        unreachable!()
    };
    assert_eq!(progress.phase, Phase::Failed);
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
    assert_eq!(report.progress.phase, Phase::Failed);
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
    assert_eq!(report.progress.phase, Phase::Failed);
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
    assert_eq!(failed.progress.phase, Phase::Failed);
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
            assert_eq!(candidate.progress.phase, Phase::Completed);
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
    assert_eq!(report.progress.phase, Phase::Failed);
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
    assert_eq!(disk.progress.phase, Phase::Failed);
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
    assert_eq!(report.progress.phase, Phase::Failed);
    assert_eq!(report.progress.completed, 1);
    let manifest = report.progress.manifest.as_ref().unwrap();
    let disk: Report = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(disk.progress.phase, Phase::Failed);
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
    assert_eq!(after_restart.progress.phase, Phase::Failed);
}

#[cfg(unix)]
#[test]
fn replaced_failed_manifest_path_is_never_adopted_or_deleted() {
    struct ReplaceFailedManifest;
    impl CopyHooks for ReplaceFailedManifest {
        fn before_manifest_verify(&self, path: &Path) -> Result<()> {
            let candidate: Report = serde_json::from_slice(&fs::read(path)?)?;
            if candidate.progress.phase == Phase::Failed {
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
    assert_eq!(report.progress.phase, Phase::Failed);
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

#[test]
fn every_reference_token_expands() {
    let f = Fixture::new();
    f.write("A.mov", b"primary video");
    let scan = f.scan();
    for &(_, token, _) in naming::TOKENS {
        for folder in [true, false] {
            let options = IngestOptions {
                library_root: f.temp.path().join("library"),
                folder_template: if folder { token.into() } else { String::new() },
                file_template: if folder {
                    "{original}".into()
                } else {
                    format!("x{token}")
                },
                job_code: "job".into(),
                include_videos: true,
                selected_ids: Some(scan.groups.iter().map(|g| g.id).collect()),
                ..Default::default()
            };
            plan(&scan, options, f.catalog.ingest_sequences().unwrap())
                .unwrap_or_else(|e| panic!("{token} (folder: {folder}): {e:#}"));
        }
    }
}

#[test]
fn pause_and_resume_acknowledge_real_progress_and_the_phase_actually_resumed() {
    let control = Control::default();
    control.report(&Progress {
        job_id: "job".into(),
        phase: Phase::Verifying,
        completed: 3,
        total: 9,
        bytes: 42,
        ..Default::default()
    });
    control.pause();
    let paused = control.snapshot();
    assert_eq!(paused.phase, Phase::Paused);
    assert_eq!(
        (paused.job_id.as_str(), paused.completed, paused.total),
        ("job", 3, 9)
    );
    assert_eq!(paused.bytes, 42);
    control.resume();
    let resumed = control.snapshot();
    assert_eq!(resumed.phase, Phase::Verifying, "not hard-coded copying");
    assert_eq!(resumed.completed, 3);
    // A pause racing job completion reports the terminal outcome, never Paused.
    control.pause();
    control.report(&Progress {
        job_id: "job".into(),
        phase: Phase::Completed,
        ..Default::default()
    });
    assert_eq!(control.snapshot().phase, Phase::Completed);
    control.reset();
    assert_eq!(control.snapshot().phase, Phase::Idle);
}

#[test]
fn worker_pause_without_a_job_is_rejected_not_faked() {
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    worker.send(Command::Pause).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let event = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining).expect("ingest event timeout") {
            crate::service::IoEvent::Ingest(event @ (Event::Error(_) | Event::Control(_))) => {
                break event;
            }
            _ => {}
        }
    };
    worker.stop().unwrap();
    assert!(matches!(event, Event::Error(e) if e.contains("No active import")));
}

#[test]
#[ignore]
fn bench_history_lookup_per_scanned_group() {
    let fixture = Fixture::new();
    {
        let mut connection = fixture.catalog.connection.lock().unwrap();
        let tx = connection.transaction().unwrap();
        for i in 0..20_000i64 {
            tx.execute("INSERT INTO ingest_log(source_volume,source_relative,original_filename,size,mtime_ns,capture_ms,capture_kind,content_hash,destinations,backups,job_id,imported_at,data) VALUES('v',?1,?1,?2,?3,?3,'offset',NULL,'[]','[]','job',?3,'{}')", rusqlite::params![format!("IMG_{i:05}.CR3"), 25_000_000 + i, 1_700_000_000_000 + i]).unwrap();
        }
        tx.commit().unwrap();
    }
    let groups: Vec<_> = (0..5_000i64)
        .map(|i| IngestGroup {
            id: i as usize,
            primary: SourceFile {
                path: format!("/card/DCIM/NEW_{i:05}.CR3").into(),
                size: 30_000_000 + i as u64,
                mtime_ns: 1_800_000_000_000_000_000 + i,
            },
            companion: None,
            sidecars: Vec::new(),
            kind: "photo".into(),
            capture_ms: 1_800_000_000_000 + i,
            capture_kind: "offset".into(),
            photo: Photo {
                filename: format!("NEW_{i:05}.CR3"),
                ..Default::default()
            },
            make: String::new(),
            serial: String::new(),
            volume: "v".into(),
            relative_path: format!("DCIM/NEW_{i:05}.CR3").into(),
            content_hash: None,
            seen: None,
            selected: true,
            warning: None,
        })
        .collect();
    for pass in 0..3 {
        let started = std::time::Instant::now();
        for group in &groups {
            assert!(fixture.catalog.ingest_seen(group).unwrap().is_none());
        }
        eprintln!(
            "BENCH ingest_seen 5000 new groups pass={pass}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn throughput_ignores_paused_time_and_withholds_early_estimates() {
    let start = std::time::Instant::now();
    let mut rate = copy::Throughput::new(start);
    // 10 MB/s for 6 s of copying.
    for second in 1..=6u64 {
        rate.sample(
            second * 10_000_000,
            Duration::ZERO,
            start + Duration::from_secs(second),
        );
        if second < 5 {
            assert_eq!(rate.eta(100_000_000), None, "estimating at {second}s");
        }
    }
    let before = rate.eta(100_000_000).unwrap();
    assert!((9..=11).contains(&before), "{before}");
    // A 10-minute pause, then copying resumes at the same speed.
    let paused = Duration::from_secs(600);
    rate.sample(70_000_000, paused, start + paused + Duration::from_secs(7));
    let after = rate.eta(100_000_000).unwrap();
    assert!(
        (after as f64 - before as f64).abs() <= before as f64 * 0.25,
        "before {before} s, after {after} s"
    );
    assert!((rate.bytes_per_second() - 10_000_000.).abs() < 1_000_000.);
}

#[test]
fn control_reports_time_spent_paused() {
    let control = Control::default();
    control.reset();
    assert_eq!(control.paused_for(), Duration::ZERO);
    control.pause();
    thread::sleep(Duration::from_millis(20));
    assert!(control.paused_for() >= Duration::from_millis(20));
    control.resume();
    let total = control.paused_for();
    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        control.paused_for(),
        total,
        "resumed time is not paused time"
    );
    control.reset();
    assert_eq!(control.paused_for(), Duration::ZERO);
}

/// Runs `plan` on a thread, reporting every progress update on a channel.
fn spawn_job(
    f: &Fixture,
    plan: &IngestPlan,
    control: &Control,
    hooks: impl CopyHooks + 'static,
) -> (
    crossbeam_channel::Receiver<Progress>,
    thread::JoinHandle<Result<Report>>,
) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let catalog = f.catalog.clone();
    let control = control.clone();
    let plan = plan.clone();
    let job = thread::spawn(move || {
        execute_with(&plan, &catalog, &control, &Capacity, &hooks, |progress| {
            let _ = tx.send(progress.clone());
        })
    });
    (rx, job)
}
fn wait_for_progress(
    rx: &crossbeam_channel::Receiver<Progress>,
    predicate: impl Fn(&Progress) -> bool,
) -> Progress {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let progress = rx.recv_timeout(remaining).expect("progress timeout");
        if predicate(&progress) {
            return progress;
        }
    }
}
/// Pulls the "card" (renames its folder away, as an unmount removes the mount
/// point) once the `after`-th file write begins.
fn pull_card_after(source: PathBuf, after: usize) -> impl CopyHooks + 'static {
    let writes = std::sync::atomic::AtomicUsize::new(0);
    Hook(move |_: &Path, output: &mut File, bytes: &[u8]| {
        if writes.fetch_add(1, Ordering::SeqCst) + 1 == after {
            fs::rename(&source, source.with_extension("pulled"))?;
        }
        output.write_all(bytes)?;
        Ok(())
    })
}

#[test]
fn pulled_card_pauses_without_failures_and_resumes_on_reinsert() {
    let f = Fixture::new();
    for n in 0..6 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let plan = f.plan();
    let control = Control::default();
    let (rx, job) = spawn_job(&f, &plan, &control, pull_card_after(f.source.clone(), 3));
    let paused = wait_for_progress(&rx, |p| p.source_missing);
    assert_eq!(paused.phase, Phase::Paused);
    assert_eq!(paused.failed, 0, "a pulled card is not a failure");
    assert!(paused.errors.is_empty(), "{:?}", paused.errors);
    assert!(control.is_source_missing());
    assert_eq!(control.snapshot().phase, Phase::Paused);
    assert!(control.snapshot().source_missing);
    // Still missing a moment later: the job waits rather than failing the rest.
    thread::sleep(Duration::from_millis(400));
    assert!(control.is_source_missing());
    // Reinsert: the same folder, the same files.
    fs::rename(f.source.with_extension("pulled"), &f.source).unwrap();
    let report = job.join().unwrap().unwrap();
    assert_eq!(
        report.progress.phase,
        Phase::Completed,
        "{:?}",
        report.progress
    );
    assert_eq!(report.progress.completed, 6);
    assert_eq!(report.progress.failed, 0);
    assert!(report.progress.errors.is_empty());
    assert!(!report.progress.source_missing);
    assert!(report.progress.transferred <= report.progress.total_bytes);
    assert!(!control.is_source_missing());
    // Every photo exactly once: nothing was copied twice under a suffix.
    let mut names: Vec<_> = fs::read_dir(f.temp.path().join("library"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("shadowroom-ingest-"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        (0..6)
            .map(|n| format!("IMG_{n:04}.mov"))
            .collect::<Vec<_>>()
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 6);
    assert_no_parts(f.temp.path());
    // The card was only ever read.
    for n in 0..6 {
        assert_eq!(
            fs::read(f.source.join(format!("IMG_{n:04}.mov"))).unwrap(),
            format!("clip {n}").as_bytes()
        );
    }
}

#[test]
fn cancel_while_waiting_for_a_pulled_card_keeps_verified_photos() {
    let f = Fixture::new();
    for n in 0..4 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let plan = f.plan();
    let control = Control::default();
    let (rx, job) = spawn_job(&f, &plan, &control, pull_card_after(f.source.clone(), 3));
    wait_for_progress(&rx, |p| p.source_missing);
    control.cancel();
    let report = job.join().unwrap().unwrap();
    assert_eq!(report.progress.phase, Phase::Cancelled);
    // The terminal report no longer waits for the card.
    assert!(!report.progress.source_missing);
    assert_eq!(report.progress.failed, 0);
    assert!(report.progress.completed <= 2);
    assert_eq!(
        f.catalog.ingest_sequences().unwrap().stored as usize,
        report.progress.completed,
        "{:?} {:?}",
        report.progress,
        f.catalog.ingest_history(10).unwrap()["jobs"]
    );
    assert_no_parts(f.temp.path());
    // Reinserted later, the same plan finishes without copying twice.
    fs::rename(f.source.with_extension("pulled"), &f.source).unwrap();
    let again = f.run(&plan, &DefaultHooks);
    assert_eq!(again.progress.phase, Phase::Completed);
    assert_eq!(
        again.progress.completed + again.progress.skipped,
        4,
        "{:?}",
        again.progress
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 4);
}

#[test]
fn one_missing_file_on_a_present_card_fails_alone_without_pausing() {
    let f = Fixture::new();
    for n in 0..4 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let plan = f.plan();
    let gone = plan.groups[2].group.clone();
    fs::remove_file(&gone.primary.path).unwrap();
    let control = Control::default();
    let (rx, job) = spawn_job(&f, &plan, &control, DefaultHooks);
    let report = job.join().unwrap().unwrap();
    assert!(rx.try_iter().all(|p| !p.source_missing));
    assert_eq!(report.progress.phase, Phase::Failed);
    assert_eq!(report.progress.completed, 3);
    assert_eq!(report.progress.failed, 1);
    assert_eq!(report.progress.failed_ids, vec![gone.id]);
    assert_eq!(report.progress.errors.len(), 1);
}

fn queue_options(f: &Fixture) -> IngestOptions {
    IngestOptions {
        library_root: f.temp.path().join("library"),
        folder_template: String::new(),
        file_template: "{original}".into(),
        include_videos: true,
        ..Default::default()
    }
}
fn job_progress(
    rx: &crossbeam_channel::Receiver<crate::service::IoEvent>,
    source: &Path,
    predicate: impl Fn(&Progress) -> bool,
) -> Progress {
    match next_event(rx, |e| {
        matches!(e, worker::Event::Progress(p)
            if p.source.as_deref() == Some(source) && predicate(p))
    }) {
        worker::Event::Progress(progress) => progress,
        _ => unreachable!(),
    }
}

#[test]
fn worker_scans_and_queues_a_second_card_while_the_first_waits_for_reinsertion() {
    use worker::{Command, Event, Worker};
    let f = Fixture::new();
    f.write("A_0001.mov", b"card a one");
    f.write("A_0002.mov", b"card a two");
    let card_b = f.temp.path().join("card-b");
    fs::create_dir(&card_b).unwrap();
    fs::write(card_b.join("B_0001.mov"), b"card b one").unwrap();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _changed) = crossbeam_channel::bounded(8);
    let card_a = f.source.clone();
    // Card A is pulled as its first photo is written: its job holds, paused.
    let worker = Worker::start_with_hooks(
        f.catalog.clone(),
        events,
        changed,
        Arc::new(pull_card_after(card_a.clone(), 1)),
    );
    worker
        .send(Command::Scan {
            path: card_a.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    worker.send(Command::Plan(queue_options(&f))).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(p) if p.ready));
    worker.send(Command::Start).unwrap();
    let held = job_progress(&rx, &card_a, |p| p.source_missing);
    assert_eq!(held.phase, Phase::Paused);
    assert!(worker.is_busy());

    // Card B scans read-only and plans while A's job owns the copier.
    worker
        .send(Command::Scan {
            path: card_b.clone(),
            auto: None,
        })
        .unwrap();
    let scanned = next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    assert!(matches!(&scanned, Event::Scanned(scan) if scan.source == card_b));
    worker.send(Command::Plan(queue_options(&f))).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(p) if p.ready));
    worker.send(Command::Start).unwrap();
    let queued = next_event(&rx, |e| matches!(e, Event::Queue(q) if !q.is_empty()));
    assert!(
        matches!(&queued, Event::Queue(q) if q.len() == 1 && q[0].source == card_b && q[0].photos + q[0].videos == 1)
    );
    // Starting B twice is refused, and nothing has been copied from B yet.
    worker.send(Command::Start).unwrap();
    let mut refused = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while refused.is_none() && std::time::Instant::now() < deadline {
        if let Ok(crate::service::IoEvent::Ingest(Event::Error(error))) =
            rx.recv_timeout(Duration::from_millis(100))
        {
            refused = Some(error);
        }
    }
    assert!(refused.is_some_and(|e| e.contains("already queued")));
    assert!(!f.temp.path().join("library/B_0001.mov").exists());

    // A is reinserted: it finishes, then B starts by itself.
    fs::rename(card_a.with_extension("pulled"), &card_a).unwrap();
    let a = job_progress(&rx, &card_a, |p| p.phase.is_terminal());
    assert_eq!(a.phase, Phase::Completed, "{a:?}");
    assert_eq!((a.completed, a.failed), (2, 0));
    let b = job_progress(&rx, &card_b, |p| p.phase.is_terminal());
    assert_eq!(b.phase, Phase::Completed, "{b:?}");
    assert_eq!(b.completed, 1);
    worker.stop().unwrap();
    for name in ["A_0001.mov", "A_0002.mov", "B_0001.mov"] {
        assert!(f.temp.path().join("library").join(name).is_file(), "{name}");
    }
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 3);
}

/// Starts card A (pulled as its first photo is written, so its job holds),
/// then scans, plans and queues card B behind it.
fn a_held_and_b_queued(
    f: &Fixture,
) -> (
    worker::Worker,
    crossbeam_channel::Receiver<crate::service::IoEvent>,
    PathBuf,
    PathBuf,
) {
    a_held_and_b_queued_with(f, Arc::new(pull_card_after(f.source.clone(), 2)))
}

#[test]
fn cancel_skips_the_pulled_card_and_starts_the_queued_one() {
    use worker::{Command, Event};
    let f = Fixture::new();
    f.write("A_0001.mov", b"card a one");
    f.write("A_0002.mov", b"card a two");
    f.write("A_0003.mov", b"card a three");
    let (worker, rx, card_a, card_b) = a_held_and_b_queued(&f);
    // Drive discovery asks to scan (and auto-import) the copying card again:
    // ignored, neither a second scan nor a second queued job.
    worker
        .send(Command::ScanCard {
            path: card_a.clone(),
            auto: Some(queue_options(&f)),
        })
        .unwrap();
    // The app hears the refusal, so a view waiting for that scan returns.
    next_event(&rx, |e| matches!(e, Event::Busy(p) if *p == card_a));
    // "Skip CARD_A · start CARD_B": Cancel stops A only.
    worker.send(Command::Cancel).unwrap();
    let a = job_progress(&rx, &card_a, |p| p.phase.is_terminal());
    assert_eq!(a.phase, Phase::Cancelled);
    assert!(!a.source_missing, "{a:?}");
    assert_eq!(a.failed, 0, "{a:?}");
    // B starts by itself: no new Scan or Start.
    let copying = job_progress(&rx, &card_b, |p| p.phase == Phase::Copying);
    assert_eq!(copying.total, 1);
    let b = job_progress(&rx, &card_b, |p| p.phase.is_terminal());
    assert_eq!(b.phase, Phase::Completed, "{b:?}");
    worker.stop().unwrap();
    assert!(!matches!(
        rx.try_iter().last(),
        Some(crate::service::IoEvent::Ingest(Event::Queue(q))) if !q.is_empty()
    ));
    // A's verified photos stay imported; its History row reads Cancelled.
    let imported_a = (1..=3)
        .filter(|n| {
            f.temp
                .path()
                .join(format!("library/A_000{n}.mov"))
                .is_file()
        })
        .count();
    assert_eq!(imported_a, a.completed);
    assert!(f.temp.path().join("library/B_0001.mov").is_file());
    assert_eq!(
        f.catalog.ingest_sequences().unwrap().stored as usize,
        a.completed + 1
    );
    let history = f.catalog.ingest_history(10).unwrap();
    let rows = history["jobs"].as_array().unwrap();
    let row_a = rows
        .iter()
        .find(|row| row["source"].as_str() == Some(card_a.to_str().unwrap()))
        .unwrap();
    assert_eq!(row_a["verification"], "cancelled");
}

#[test]
fn a_queued_card_can_be_removed_alone_and_clear_queue_drops_the_rest() {
    use worker::{Command, Event};
    let f = Fixture::new();
    f.write("A_0001.mov", b"card a one");
    f.write("A_0002.mov", b"card a two");
    let (worker, rx, card_a, card_b) = a_held_and_b_queued(&f);
    let card_c = f.temp.path().join("card-c");
    fs::create_dir(&card_c).unwrap();
    fs::write(card_c.join("C_0001.mov"), b"card c one").unwrap();
    worker
        .send(Command::Scan {
            path: card_c.clone(),
            auto: None,
        })
        .unwrap();
    next_event(&rx, |e| matches!(e, Event::Scanned(_)));
    worker.send(Command::Plan(queue_options(&f))).unwrap();
    next_event(&rx, |e| matches!(e, Event::Plan(p) if p.ready));
    worker.send(Command::Start).unwrap();
    next_event(&rx, |e| matches!(e, Event::Queue(q) if q.len() == 2));
    // The ✕ on B drops only B.
    worker.send(Command::Dequeue(card_b.clone())).unwrap();
    let queue = next_event(&rx, |e| matches!(e, Event::Queue(_)));
    assert!(
        matches!(&queue, Event::Queue(q) if q.len() == 1 && q[0].source == card_c),
        "{queue:?}"
    );
    // Clear queue drops C; A keeps waiting for its card.
    worker.send(Command::ClearQueue).unwrap();
    next_event(&rx, |e| matches!(e, Event::Queue(q) if q.is_empty()));
    fs::rename(card_a.with_extension("pulled"), &card_a).unwrap();
    let a = job_progress(&rx, &card_a, |p| p.phase.is_terminal());
    assert_eq!(a.phase, Phase::Completed, "{a:?}");
    // Neither removed card ever starts.
    let deadline = std::time::Instant::now() + Duration::from_millis(600);
    while let Ok(event) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        if let crate::service::IoEvent::Ingest(Event::Progress(p)) = event {
            assert_eq!(p.source.as_deref(), Some(card_a.as_path()), "{p:?}");
        }
    }
    worker.stop().unwrap();
    assert!(!f.temp.path().join("library/B_0001.mov").exists());
    assert!(!f.temp.path().join("library/C_0001.mov").exists());
}

/// A read error while the card still answers: the job waits for the source
/// to settle before counting the photo as failed. Here the reader returns
/// EIO and the mount goes 500 ms later, as a real pull does.
#[test]
fn a_read_error_shortly_before_the_card_vanishes_is_deferred_not_failed() {
    let f = Fixture::new();
    for n in 0..6 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let plan = f.plan();
    let control = Control::default();
    let writes = std::sync::atomic::AtomicUsize::new(0);
    let source = f.source.clone();
    let hooks = Hook(move |_: &Path, output: &mut File, bytes: &[u8]| {
        if writes.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            let source = source.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                fs::rename(&source, source.with_extension("pulled")).unwrap();
            });
            return Err(std::io::Error::from_raw_os_error(libc::EIO).into());
        }
        output.write_all(bytes)?;
        Ok(())
    });
    let (rx, job) = spawn_job(&f, &plan, &control, hooks);
    let paused = wait_for_progress(&rx, |p| p.source_missing);
    assert_eq!(paused.failed, 0, "{paused:?}");
    assert!(paused.errors.is_empty(), "{:?}", paused.errors);
    fs::rename(f.source.with_extension("pulled"), &f.source).unwrap();
    let report = job.join().unwrap().unwrap();
    assert_eq!(
        report.progress.phase,
        Phase::Completed,
        "{:?}",
        report.progress
    );
    assert_eq!(report.progress.failed, 0);
    assert_eq!(report.progress.completed, 6);
    let mut names: Vec<_> = fs::read_dir(f.temp.path().join("library"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("shadowroom-ingest-"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 6, "{names:?}");
    assert_no_parts(f.temp.path());
}

/// A read error on a card that stays present is the photo's own failure,
/// decided after the grace period, and the next failure does not wait again.
#[test]
fn read_errors_on_a_present_card_fail_after_one_grace_period() {
    let f = Fixture::new();
    for n in 0..4 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let plan = f.plan();
    let control = Control::default();
    let hooks = Hook(|path: &Path, output: &mut File, bytes: &[u8]| {
        let name = path.file_name().unwrap().to_string_lossy();
        if name.contains("IMG_0001") || name.contains("IMG_0002") {
            return Err(std::io::Error::from_raw_os_error(libc::EIO).into());
        }
        output.write_all(bytes)?;
        Ok(())
    });
    let started = std::time::Instant::now();
    let (rx, job) = spawn_job(&f, &plan, &control, hooks);
    let report = job.join().unwrap().unwrap();
    let took = started.elapsed();
    assert!(rx.try_iter().all(|p| !p.source_missing));
    assert_eq!(report.progress.phase, Phase::Failed);
    assert_eq!(report.progress.completed, 2);
    assert_eq!(report.progress.failed, 2);
    assert_eq!(report.progress.failed_sources.len(), 2);
    assert!(
        report
            .progress
            .failed_sources
            .iter()
            .all(|path| path.starts_with(&f.source))
    );
    assert!(took >= Duration::from_millis(1400), "{took:?}");
    assert!(took < Duration::from_millis(4000), "{took:?}");
}

/// Test card whose volume identity follows where it is mounted, as a
/// filesystem UUID does.
struct Remount {
    mounted: Arc<std::sync::Mutex<Option<PathBuf>>>,
    writes: std::sync::atomic::AtomicUsize,
    pull_after: usize,
}
const TEST_CARD: &str = "uuid:7E57-CA4D";
impl CopyHooks for Remount {
    fn write_chunk(&self, _: &Path, output: &mut File, bytes: &[u8]) -> Result<()> {
        if self.writes.fetch_add(1, Ordering::SeqCst) + 1 == self.pull_after {
            let mut mounted = self.mounted.lock().unwrap();
            let card = mounted.take().unwrap();
            fs::rename(&card, card.with_extension("pulled"))?;
        }
        output.write_all(bytes)?;
        Ok(())
    }
    fn volume_identity(&self, path: &Path) -> String {
        match self.mounted.lock().unwrap().as_deref() {
            Some(mount) if path.starts_with(mount) => TEST_CARD.into(),
            _ => "uuid:0000-ROOT".into(),
        }
    }
    fn volume_mounts(&self, volume: &str) -> Vec<PathBuf> {
        if volume != TEST_CARD {
            return Vec::new();
        }
        self.mounted.lock().unwrap().iter().cloned().collect()
    }
}

#[test]
fn a_card_back_at_another_mount_point_resumes_there_without_duplicates() {
    let f = Fixture::new();
    for n in 0..6 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let mut plan = f.plan();
    for planned in &mut plan.groups {
        planned.group.volume = TEST_CARD.into();
    }
    let mounted = Arc::new(std::sync::Mutex::new(Some(f.source.clone())));
    let hooks = Remount {
        mounted: mounted.clone(),
        writes: std::sync::atomic::AtomicUsize::new(0),
        pull_after: 3,
    };
    let control = Control::default();
    let (rx, job) = spawn_job(&f, &plan, &control, hooks);
    wait_for_progress(&rx, |p| p.source_missing);
    // The old mount point stays behind, empty and no longer the card; the
    // card comes back as EOS_DIGITAL1.
    let moved = f.temp.path().join("card1");
    fs::rename(f.source.with_extension("pulled"), &moved).unwrap();
    fs::create_dir(&f.source).unwrap();
    *mounted.lock().unwrap() = Some(moved.clone());
    let report = job.join().unwrap().unwrap();
    assert_eq!(
        report.progress.phase,
        Phase::Completed,
        "{:?}",
        report.progress
    );
    assert_eq!(report.progress.completed, 6);
    assert_eq!(report.progress.failed, 0);
    // Progress keeps naming the scanned source, so the view still matches,
    // and says where the card is now and which card it is.
    assert_eq!(report.progress.source.as_deref(), Some(f.source.as_path()));
    assert_eq!(
        report.progress.current_source.as_deref(),
        Some(moved.as_path())
    );
    assert_eq!(report.progress.volume.as_deref(), Some(TEST_CARD));
    let mut names: Vec<_> = fs::read_dir(f.temp.path().join("library"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("shadowroom-ingest-"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        (0..6)
            .map(|n| format!("IMG_{n:04}.mov"))
            .collect::<Vec<_>>()
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 6);
    assert_no_parts(f.temp.path());
    for n in 0..6 {
        assert_eq!(
            fs::read(moved.join(format!("IMG_{n:04}.mov"))).unwrap(),
            format!("clip {n}").as_bytes()
        );
    }
}

/// A card with a label but no filesystem UUID is never followed to another
/// path: another card could carry the same name.
#[test]
fn a_label_only_card_is_not_followed_to_another_path() {
    let f = Fixture::new();
    for n in 0..3 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let mut plan = f.plan();
    for planned in &mut plan.groups {
        planned.group.volume = "label:EOS_DIGITAL".into();
    }
    struct LabelOnly(Arc<std::sync::Mutex<Option<PathBuf>>>);
    impl CopyHooks for LabelOnly {
        fn volume_identity(&self, _: &Path) -> String {
            "label:EOS_DIGITAL".into()
        }
        fn volume_mounts(&self, _: &str) -> Vec<PathBuf> {
            self.0.lock().unwrap().iter().cloned().collect()
        }
    }
    let watched = Arc::new(std::sync::Mutex::new(Some(f.source.clone())));
    let hooks = LabelOnly(watched.clone());
    let control = Control::default();
    let watch = super::copy::SourceWatch::new(&plan, &hooks, &control);
    let moved = f.temp.path().join("card1");
    fs::rename(&f.source, &moved).unwrap();
    *watched.lock().unwrap() = Some(moved);
    let probe = plan.groups[0].group.primary.path.clone();
    assert!(!watch.present(&probe));
    assert!(!watch.relocate(&probe));
}

/// The job tells the app where it reads after the card came back elsewhere,
/// and names failures under the scanned source (as the scan's tiles and
/// "Retry failed" do), not the new mount point.
#[test]
fn a_relocated_source_is_reported_and_failures_keep_the_scanned_names() {
    let f = Fixture::new();
    for n in 0..2 {
        f.write(&format!("IMG_{n:04}.mov"), format!("clip {n}").as_bytes());
    }
    let mut plan = f.plan();
    for planned in &mut plan.groups {
        planned.group.volume = TEST_CARD.into();
    }
    let mounted = Arc::new(std::sync::Mutex::new(Some(f.source.clone())));
    let hooks = Remount {
        mounted: mounted.clone(),
        writes: std::sync::atomic::AtomicUsize::new(0),
        pull_after: usize::MAX,
    };
    let control = Control::default();
    let watch = super::copy::SourceWatch::new(&plan, &hooks, &control);
    assert_eq!(watch.identity().as_deref(), Some(TEST_CARD));
    assert_eq!(watch.inside(), Some(PathBuf::new()));
    let probe = plan.groups[0].group.primary.path.clone();
    let moved = f.temp.path().join("card1");
    fs::rename(&f.source, &moved).unwrap();
    fs::create_dir(&f.source).unwrap();
    *mounted.lock().unwrap() = Some(moved.clone());
    assert!(watch.relocate(&probe));
    assert_eq!(watch.current(), moved);
    assert_eq!(
        watch.scanned(&moved.join("IMG_0001.mov")),
        probe.with_file_name("IMG_0001.mov")
    );
    // A path outside the source is left alone.
    let elsewhere = f.temp.path().join("other/x.mov");
    assert_eq!(watch.scanned(&elsewhere), elsewhere);
}

/// Reads like `pull_card_after`, and keeps the dispatcher busy scanning
/// `slow` for `delay` (a slow third card).
struct SlowScan<H> {
    inner: H,
    slow: PathBuf,
    delay: Duration,
}
impl<H: CopyHooks> CopyHooks for SlowScan<H> {
    fn write_chunk(&self, path: &Path, output: &mut File, bytes: &[u8]) -> Result<()> {
        self.inner.write_chunk(path, output, bytes)
    }
    fn before_scan(&self, path: &Path) {
        if path == self.slow {
            thread::sleep(self.delay);
        }
    }
}

/// Starts card A (pulled as its first photo is written, so its job holds),
/// then scans, plans and queues card B behind it, through `hooks`.
fn a_held_and_b_queued_with(
    f: &Fixture,
    hooks: Arc<dyn CopyHooks>,
) -> (
    worker::Worker,
    crossbeam_channel::Receiver<crate::service::IoEvent>,
    PathBuf,
    PathBuf,
) {
    use worker::{Command, Event, Worker};
    let card_b = f.temp.path().join("card-b");
    fs::create_dir(&card_b).unwrap();
    fs::write(card_b.join("B_0001.mov"), b"card b one").unwrap();
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _changed) = crossbeam_channel::bounded(8);
    let card_a = f.source.clone();
    let worker = Worker::start_with_hooks(f.catalog.clone(), events, changed, hooks);
    for card in [&card_a, &card_b] {
        worker
            .send(Command::Scan {
                path: card.clone(),
                auto: None,
            })
            .unwrap();
        next_event(&rx, |e| matches!(e, Event::Scanned(_)));
        worker.send(Command::Plan(queue_options(f))).unwrap();
        next_event(&rx, |e| matches!(e, Event::Plan(p) if p.ready));
        worker.send(Command::Start).unwrap();
        if card == &card_a {
            job_progress(&rx, &card_a, |p| p.source_missing);
        }
    }
    next_event(&rx, |e| matches!(e, Event::Queue(q) if q.len() == 1));
    (worker, rx, card_a, card_b)
}

/// R5-io-02: "Skip CARD_A · start CARD_B" while the dispatcher is busy
/// scanning a third card. A ends during that scan, so the Cancel command and
/// A's end are both waiting when the dispatcher returns; whichever it reads
/// first, the Cancel never reaches B (it names A's launch).
#[test]
fn a_cancel_read_after_the_next_card_started_never_cancels_it() {
    use worker::{Command, Event};
    for round in 0..8 {
        let f = Fixture::new();
        f.write("A_0001.mov", b"card a one");
        f.write("A_0002.mov", b"card a two");
        let card_c = f.temp.path().join("card-c");
        fs::create_dir(&card_c).unwrap();
        fs::write(card_c.join("C_0001.mov"), b"card c one").unwrap();
        let hooks = SlowScan {
            inner: pull_card_after(f.source.clone(), 2),
            slow: card_c.clone(),
            delay: Duration::from_millis(400),
        };
        let (worker, rx, card_a, card_b) = a_held_and_b_queued_with(&f, Arc::new(hooks));
        worker
            .send(Command::Scan {
                path: card_c.clone(),
                auto: None,
            })
            .unwrap();
        next_event(&rx, |e| matches!(e, Event::Scanning(p) if *p == card_c));
        worker.send(Command::Cancel).unwrap();
        let a = job_progress(&rx, &card_a, |p| p.phase.is_terminal());
        assert_eq!(a.phase, Phase::Cancelled, "round {round}: {a:?}");
        let b = job_progress(&rx, &card_b, |p| p.phase.is_terminal());
        assert_eq!(b.phase, Phase::Completed, "round {round}: {b:?}");
        worker.stop().unwrap();
        assert!(f.temp.path().join("library/B_0001.mov").is_file());
    }
}

/// A quit holds the queue: the running card ends (here: cancelled while
/// its card is out) and the queued card does not start; the queue is kept.
/// Releasing it (Escape: Shadowroom stays open) starts the queued card.
#[test]
fn a_held_queue_keeps_its_cards_until_released() {
    use worker::{Command, Event};
    let f = Fixture::new();
    f.write("A_0001.mov", b"card a one");
    f.write("A_0002.mov", b"card a two");
    let (worker, rx, card_a, card_b) = a_held_and_b_queued(&f);
    worker.send(Command::HoldQueue(true)).unwrap();
    worker.send(Command::Cancel).unwrap();
    let a = job_progress(&rx, &card_a, |p| p.phase.is_terminal());
    assert_eq!(a.phase, Phase::Cancelled);
    // B waits: no progress of B, and the queue is not emptied.
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    while let Ok(event) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        match event {
            crate::service::IoEvent::Ingest(Event::Progress(p)) => {
                assert_ne!(p.source.as_deref(), Some(card_b.as_path()), "{p:?}")
            }
            crate::service::IoEvent::Ingest(Event::Queue(q)) => {
                assert_eq!(q.len(), 1, "{q:?}")
            }
            _ => {}
        }
    }
    assert!(!f.temp.path().join("library/B_0001.mov").exists());
    worker.send(Command::HoldQueue(false)).unwrap();
    let b = job_progress(&rx, &card_b, |p| p.phase.is_terminal());
    assert_eq!(b.phase, Phase::Completed, "{b:?}");
    worker.stop().unwrap();
    assert!(f.temp.path().join("library/B_0001.mov").is_file());
}
