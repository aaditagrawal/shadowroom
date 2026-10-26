use shadowroom::{
    catalog::Catalog,
    ingest::{
        self, Control, CopyHooks, FsCapacity, FsStats, IngestOptions, SequenceState, execute,
        execute_with, naming,
    },
    model::{ColorLabel, Flag, Photo, PhotoDecision},
    settings::Settings,
};
use std::{
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    catalog: Catalog,
}
impl Fixture {
    fn new() -> Self {
        // Ingest resolves destination roots. Use the same identity on macOS,
        // where the system temp directory can be /var -> /private/var.
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
    fn jpeg(&self, name: &str) -> PathBuf {
        let path = self.source.join(name);
        let seed = name.bytes().fold(0u8, u8::wrapping_add);
        let pixels = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([
                (x * 3 + y * 5) as u8 ^ seed,
                (x * 7 + y) as u8,
                (x + y * 9) as u8,
            ])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
            .encode_image(&pixels)
            .unwrap();
        // Valid, generated JPEG pixels and EXIF exercise metadata preservation
        // without a developer's private photos or an optional native codec.
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(fixture_metadata_tiff());
        let mut jpeg = encoded[..2].to_vec();
        jpeg.extend_from_slice(&[0xff, 0xe1]);
        jpeg.extend_from_slice(&u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
        jpeg.extend(app1);
        jpeg.extend_from_slice(&encoded[2..]);
        fs::write(&path, jpeg).unwrap();
        path
    }
    fn scan(&self) -> ingest::Scan {
        ingest::scan(&self.source, &self.catalog, |_| {}, &Control::default()).unwrap()
    }
    fn options(&self) -> IngestOptions {
        IngestOptions {
            library_root: self.temp.path().join("library"),
            folder_template: "".into(),
            file_template: "{original}".into(),
            ..Default::default()
        }
    }
    fn plan(&self) -> ingest::IngestPlan {
        ingest::plan(
            &self.scan(),
            self.options(),
            self.catalog.ingest_sequences().unwrap(),
        )
        .unwrap()
    }
    fn all_files(&self, root: &str) -> Vec<PathBuf> {
        walkdir::WalkDir::new(self.temp.path().join(root))
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect()
    }
}
fn fixture_metadata_tiff() -> Vec<u8> {
    // Let the EXIF writer put camera fields in IFD0 and capture/serial/lens
    // fields in the proper Exif sub-IFD, just as real camera metadata does.
    use exif::{Field, In, Tag, Value, experimental::Writer};
    let ascii = |text: &str| Value::Ascii(vec![text.as_bytes().to_vec()]);
    let fields = [
        (Tag::ImageWidth, Value::Long(vec![64])),
        (Tag::ImageLength, Value::Long(vec![48])),
        (Tag::Orientation, Value::Short(vec![6])),
        (Tag::Make, ascii("Test Make")),
        (Tag::Model, ascii("Test Camera")),
        (Tag::LensModel, ascii("Test 85mm")),
        (Tag::BodySerialNumber, ascii("SER123")),
        (Tag::DateTimeOriginal, ascii("2026:10:12 02:30:04")),
        (Tag::SubSecTimeOriginal, ascii("123")),
        (Tag::OffsetTimeOriginal, ascii("+05:30")),
        (Tag::PhotographicSensitivity, Value::Short(vec![800])),
        (
            Tag::ExposureTime,
            Value::Rational(vec![exif::Rational { num: 1, denom: 125 }]),
        ),
    ]
    .map(|(tag, value)| Field {
        tag,
        ifd_num: In::PRIMARY,
        value,
    });
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    let mut metadata = Cursor::new(Vec::new());
    writer.write(&mut metadata, true).unwrap();
    metadata.into_inner()
}
fn fake_raw(path: &Path) {
    // Metadata-bearing TIFF/RAW fixture. Ingest verifies unchanged bytes; it
    // does not develop sensor pixels or depend on a platform RAW decoder.
    let mut bytes = fixture_metadata_tiff();
    bytes.resize(1024 * 64, 0);
    fs::write(path, bytes).unwrap();
}
fn no_parts(root: &Path) {
    assert!(
        walkdir::WalkDir::new(root)
            .into_iter()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains("shadowroom-part"))
    );
}

#[test]
fn naming_all_tokens_sanitization_wall_clock_and_letter() {
    let f = Fixture::new();
    f.jpeg("IMG_001.JPG");
    let mut scan = f.scan();
    let group = &mut scan.groups[0];
    group.capture_ms = chrono::DateTime::parse_from_rfc3339("2026-10-12T02:30:04.123+05:30")
        .unwrap()
        .timestamp_millis();
    group.photo.captured_offset = Some("+05:30".into());
    group.photo.camera = "Sony: A/7".into();
    group.make = "Sony".into();
    group.serial = "12345".into();
    group.photo.lens = "FE 85mm".into();
    group.photo.iso = Some(800);
    let mut options = f.options();
    options.job_code = "Wedding / ?party".into();
    options.imported_at_ms = chrono::DateTime::parse_from_rfc3339("2026-10-19T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    let mut state = SequenceState {
        stored: 41,
        ..Default::default()
    };
    state.days.insert("2026-10-11".into(), 6);
    let ctx = naming::Context {
        group,
        options: &options,
        sequences: &state,
        session: 2,
        letter: 26,
    };
    let tokens = "{YYYY}/{YY}-{MM}-{MMM}-{DD}-{hh}-{mm}-{ss}-{subsec}/{date}_{time}/{import:YYYY-MM-DD}/{original}.{original_ext}.{ext}/{camera}_{make}_{serial}_{lens}_{iso}/{job}{_job}/{seq_session:4}_{seq_day:3}_{seq_stored:5}_{letter}";
    let expanded = ctx
        .expand(tokens, true)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        expanded.contains(
            "2026/26-10-Oct-12-02-30-04-123/2026-10-12_023004/2026-10-19/IMG_001.JPG.jpg"
        ),
        "{expanded}"
    );
    assert!(
        expanded.contains("Sony-A-7_Sony_12345_FE-85mm_800"),
        "{expanded}"
    );
    assert!(expanded.ends_with("0003_007_00042_aa"), "{expanded}");
    assert_eq!(naming::letter(0), "a");
    assert_eq!(naming::letter(25), "z");
    assert_eq!(naming::letter(701), "zz");
    assert_eq!(naming::letter(702), "aaa");
    assert_eq!(naming::sanitize(" .<>:/\\bad  ---name... "), "bad-name");
    assert_eq!(naming::sanitize("CON"), "_CON");
    assert!(naming::sanitize(&"é".repeat(200)).len() <= 180);
    assert!(ctx.expand("../escape", true).is_err());
    assert!(ctx.expand("/{original}", true).is_err());
    assert!(ctx.expand("{unknown}", false).is_err());
    assert!(ctx.expand("{seq_stored:99}", false).is_err());
}
#[test]
fn pure_plan_live_example_shared_stem_and_sidecars() {
    let f = Fixture::new();
    f.jpeg("SHOT.JPG");
    fake_raw(&f.source.join("SHOT.ARW"));
    fs::write(f.source.join("SHOT.xmp"), b"xmp").unwrap();
    fs::write(f.source.join("SHOT.ARW.xmp"), b"raw xmp").unwrap();
    let scan = f.scan();
    assert_eq!(scan.groups.len(), 1);
    assert!(scan.groups[0].companion.is_some());
    assert_eq!(scan.groups[0].sidecars.len(), 2);
    assert_eq!(scan.groups[0].photo.camera, "Test Camera");
    assert_eq!(scan.groups[0].make, "Test Make");
    assert_eq!(scan.groups[0].serial, "SER123");
    let mut options = f.options();
    options.folder_template = "{YYYY}/{date}{_job}".into();
    options.file_template = "{date}_{time}_{seq_session:4}_{seq_day:3}_{seq_stored:5}".into();
    options.job_code = "shoot".into();
    let plan = ingest::plan(&scan, options.clone(), SequenceState::default()).unwrap();
    assert_eq!(plan.photos, 1);
    assert_eq!(plan.files, 4);
    assert_eq!(plan.bytes, scan.groups[0].bytes());
    let first = &plan.groups[0].files[0].relative;
    assert_eq!(
        plan.example.as_ref().unwrap(),
        &options.library_root.join(first)
    );
    let stem = first.file_stem().unwrap().to_string_lossy();
    for file in &plan.groups[0].files {
        assert!(
            file.relative
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(stem.as_ref())
        );
    }
    assert!(
        plan.groups[0]
            .files
            .iter()
            .any(|f| f.relative.to_string_lossy().ends_with(".ARW.xmp"))
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert!(!options.library_root.exists());
}
#[test]
fn day_start_and_same_second_letters_and_in_plan_collisions() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    f.jpeg("B.jpg");
    let mut scan = f.scan();
    scan.groups[1].capture_ms = scan.groups[0].capture_ms;
    let mut options = f.options();
    options.file_template = "{letter}_{seq_session:2}".into();
    let plan = ingest::plan(&scan, options.clone(), SequenceState::default()).unwrap();
    assert_eq!(plan.groups[0].files[0].relative, Path::new("a_01.jpg"));
    assert_eq!(plan.groups[1].files[0].relative, Path::new("b_02.jpg"));
    options.file_template = "same".into();
    let plan = ingest::plan(&scan, options, SequenceState::default()).unwrap();
    assert_eq!(plan.groups[1].files[0].relative, Path::new("same-1.jpg"));
    let mut group = scan.groups[0].clone();
    group.photo.captured_offset = None;
    group.capture_ms = chrono::NaiveDate::from_ymd_opt(2026, 10, 12)
        .unwrap()
        .and_hms_opt(2, 59, 59)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    assert_eq!(naming::day_key(&group, 3), "2026-10-11");
    group.capture_ms += 1000;
    assert_eq!(naming::day_key(&group, 3), "2026-10-12");
}
#[test]
fn collision_identical_skip_and_record_different_suffix_no_overwrite() {
    let f = Fixture::new();
    let source = f.jpeg("A.jpg");
    let plan = f.plan();
    fs::create_dir(&plan.options.library_root).unwrap();
    let target = plan.options.library_root.join("A.jpg");
    fs::copy(&source, &target).unwrap();
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.skipped, 1);
    assert_eq!(report.progress.completed, 0);
    assert!(report.records[0].files[0].existed[0]);
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["total_imported_groups"],
        1
    );
    fs::write(&target, b"existing unrelated bytes").unwrap();
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.completed, 1);
    assert_eq!(fs::read(&target).unwrap(), b"existing unrelated bytes");
    let copied = plan.options.library_root.join("A-1.jpg");
    assert_eq!(fs::read(copied).unwrap(), fs::read(source).unwrap());
    no_parts(f.temp.path());
}
#[test]
fn collision_sidecar_suffixes_entire_group_and_skip_policy() {
    let f = Fixture::new();
    f.jpeg("S.jpg");
    fake_raw(&f.source.join("S.arw"));
    fs::write(f.source.join("S.arw.xmp"), b"source").unwrap();
    let plan = f.plan();
    fs::create_dir(&plan.options.library_root).unwrap();
    fs::write(plan.options.library_root.join("S.arw.xmp"), b"keep").unwrap();
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.completed, 1);
    for name in ["S-1.arw", "S-1.jpg", "S-1.arw.xmp"] {
        assert!(plan.options.library_root.join(name).is_file());
    }
    assert_eq!(
        fs::read(plan.options.library_root.join("S.arw.xmp")).unwrap(),
        b"keep"
    );
    let g = Fixture::new();
    g.jpeg("S.jpg");
    let mut plan = g.plan();
    plan.options.collision = "skip".into();
    fs::create_dir(&plan.options.library_root).unwrap();
    fs::write(plan.options.library_root.join("S.jpg"), b"keep").unwrap();
    let report = execute(&plan, &g.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.skipped, 1);
    assert_eq!(g.catalog.ingest_sequences().unwrap().stored, 0);
    assert_eq!(
        g.catalog.ingest_history(10).unwrap()["total_imported_groups"],
        0
    );
}
struct Capacity(u64);
impl FsStats for Capacity {
    fn capacity(&self, _: &Path) -> anyhow::Result<FsCapacity> {
        Ok(FsCapacity {
            filesystem: "same-disk".into(),
            available: self.0,
        })
    }
}
#[test]
fn free_space_same_filesystem_counts_both_destinations_and_refuses_before_writes() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let mut plan = f.plan();
    plan.options.backup_roots = vec![f.temp.path().join("backup")];
    let space = ingest::copy_space(&plan, &Capacity(u64::MAX)).unwrap();
    assert_eq!(space[0].filesystem_required, plan.bytes * 2);
    assert_eq!(space[1].filesystem_required, plan.bytes * 2);
    assert!(
        execute_with(
            &plan,
            &f.catalog,
            &Control::default(),
            &Capacity(1),
            &ingest::DefaultHooks,
            |_| {}
        )
        .is_err()
    );
    assert!(!plan.options.library_root.exists());
    assert!(!plan.options.backup_roots[0].exists());
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
struct Corrupt;
impl CopyHooks for Corrupt {
    fn write_chunk(&self, path: &Path, output: &mut File, bytes: &[u8]) -> anyhow::Result<()> {
        let mut bytes = bytes.to_vec();
        if path.to_string_lossy().contains("backup") {
            bytes[0] ^= 0xff;
        }
        output.write_all(&bytes)?;
        Ok(())
    }
}
#[test]
fn corrupt_backup_rolls_back_raw_jpeg_sidecar_and_counters() {
    let f = Fixture::new();
    let source = f.jpeg("A.jpg");
    fake_raw(&f.source.join("A.arw"));
    fs::write(f.source.join("A.xmp"), b"xmp").unwrap();
    let original = fs::read(source).unwrap();
    let mut plan = f.plan();
    plan.options.backup_roots = vec![f.temp.path().join("backup")];
    let report = execute_with(
        &plan,
        &f.catalog,
        &Control::default(),
        &Capacity(u64::MAX),
        &Corrupt,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.progress.phase, "failed");
    assert_eq!(report.progress.failed, 1);
    assert!(report.progress.errors[0].contains("verification"));
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert!(f.all_files("backup").is_empty());
    assert!(
        f.all_files("library")
            .iter()
            .all(|p| p.extension().is_some_and(|e| e == "json"))
    );
    no_parts(f.temp.path());
    assert_eq!(
        f.catalog.ingest_history(10).unwrap()["total_imported_groups"],
        0
    );
    assert_eq!(fs::read(f.source.join("A.jpg")).unwrap(), original);
}
#[test]
fn backup_fanout_both_verified_mtime_preserved_manifest_and_persistent_sequence() {
    let f = Fixture::new();
    let source = f.jpeg("A.jpg");
    let source_photo = &f.scan().groups[0].photo;
    assert_eq!((source_photo.width, source_photo.height), (64, 48));
    assert_eq!(source_photo.orientation, 6);
    assert_eq!(source_photo.camera, "Test Camera");
    assert_eq!(source_photo.lens, "Test 85mm");
    assert_eq!(source_photo.iso, Some(800));
    assert_eq!(source_photo.captured_offset.as_deref(), Some("+05:30"));
    assert_eq!(
        source_photo.captured_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-10-12T02:30:04.123+05:30")
                .unwrap()
                .timestamp_millis()
        )
    );
    let before = fs::metadata(&source).unwrap().modified().unwrap();
    let mut plan = f.plan();
    plan.options.backup_roots = vec![f.temp.path().join("backup")];
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.phase, "completed");
    assert_eq!(report.records[0].files[0].destinations.len(), 2);
    let expected = blake3::hash(&fs::read(source).unwrap())
        .to_hex()
        .to_string();
    for path in &report.records[0].files[0].destinations {
        assert_eq!(
            blake3::hash(&fs::read(path).unwrap()).to_hex().to_string(),
            expected
        );
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), before);
        let decoded = image::open(path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(report.progress.manifest.unwrap()).unwrap()).unwrap();
    assert_eq!(json["progress"]["phase"], "completed");
    let reopened = Catalog::open(f.catalog.path()).unwrap();
    assert_eq!(reopened.ingest_sequences().unwrap().stored, 1);
    assert_eq!(
        reopened.ingest_history(10).unwrap()["jobs"][0]["verification"],
        "completed"
    );
    let again = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(again.progress.skipped, 1);
    assert_eq!(again.progress.completed, 0);
    assert_eq!(reopened.ingest_sequences().unwrap().stored, 1);
    no_parts(f.temp.path());
}
struct Interrupt {
    control: Control,
    fired: AtomicBool,
    pause: bool,
    signal: crossbeam_channel::Sender<()>,
}
impl CopyHooks for Interrupt {
    fn write_chunk(&self, _: &Path, output: &mut File, bytes: &[u8]) -> anyhow::Result<()> {
        output.write_all(bytes)?;
        if !self.fired.swap(true, Ordering::SeqCst) {
            if self.pause {
                self.control.pause();
            } else {
                self.control.cancel();
            }
            let _ = self.signal.send(());
        }
        Ok(())
    }
}
#[test]
fn cancellation_cleans_temps_then_same_plan_resumes() {
    let f = Fixture::new();
    fs::write(f.source.join("big.mov"), vec![42; 12 * 1024 * 1024]).unwrap();
    let plan = f.plan();
    let control = Control::default();
    let (signal, _) = crossbeam_channel::unbounded();
    let hooks = Interrupt {
        control: control.clone(),
        fired: AtomicBool::new(false),
        pause: false,
        signal,
    };
    let report = execute_with(
        &plan,
        &f.catalog,
        &control,
        &Capacity(u64::MAX),
        &hooks,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.progress.phase, "cancelled");
    assert_eq!(report.progress.completed, 0);
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    no_parts(f.temp.path());
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.progress.videos, 1);
}
#[test]
fn pause_at_chunk_boundary_resume_and_cancel_while_paused() {
    let f = Fixture::new();
    fs::write(f.source.join("big.mov"), vec![9; 12 * 1024 * 1024]).unwrap();
    let plan = f.plan();
    let control = Control::default();
    let (signal, rx) = crossbeam_channel::bounded(1);
    let catalog = f.catalog.clone();
    let worker_control = control.clone();
    let handle = std::thread::spawn(move || {
        let hooks = Interrupt {
            control: worker_control.clone(),
            fired: AtomicBool::new(false),
            pause: true,
            signal,
        };
        execute_with(
            &plan,
            &catalog,
            &worker_control,
            &Capacity(u64::MAX),
            &hooks,
            |_| {},
        )
        .unwrap()
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(!handle.is_finished());
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    control.resume();
    assert_eq!(handle.join().unwrap().progress.completed, 1);
    let control = Arc::new(Control::default());
    control.pause();
    let clone = control.clone();
    let handle = std::thread::spawn(move || clone.checkpoint());
    std::thread::sleep(Duration::from_millis(10));
    control.cancel();
    assert!(handle.join().unwrap().is_err());
}
#[test]
fn previously_imported_lookup_hash_capture_timezone_all_candidates_and_mount_independence() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let mut group = f.scan().groups.remove(0);
    group.capture_kind = "camera_local".into();
    group.capture_ms = 1234000;
    let mut record = ingest::store::ImportRecord::marked(&group);
    record.content_hash = Some(blake3::hash(b"known").to_hex().to_string());
    // Public mark action records its known content hash.
    group.content_hash = record.content_hash.clone();
    f.catalog.ingest_mark_imported(&[group.clone()]).unwrap();
    group.primary.path = PathBuf::from("/a/completely/different/mount/A.jpg");
    group.volume = "different-volume".into();
    group.relative_path = PathBuf::from("other/A.jpg");
    let seen = f.catalog.ingest_seen(&group).unwrap().unwrap();
    assert_eq!(seen.matched_by, "hash");
    group.content_hash = None;
    assert_eq!(
        f.catalog.ingest_seen(&group).unwrap().unwrap().matched_by,
        "capture"
    );
    group.capture_ms += 111;
    group.primary.mtime_ns += 5 * 900_000_000_000;
    assert_eq!(
        f.catalog.ingest_seen(&group).unwrap().unwrap().matched_by,
        "mtime_timezone"
    );
    group.primary.mtime_ns += 1;
    assert!(f.catalog.ingest_seen(&group).unwrap().is_none());
    group.primary.mtime_ns -= 1;
    group.primary.mtime_ns += 100 * 900_000_000_000;
    assert!(f.catalog.ingest_seen(&group).unwrap().is_none());
    group.content_hash = Some(blake3::hash(b"different").to_hex().to_string());
    group.capture_ms = 1234000;
    assert!(f.catalog.ingest_seen(&group).unwrap().is_none());
    // An off-grid candidate inserted first must not hide a later valid candidate.
    let base = f.scan().groups.remove(0);
    let mut invalid = base.clone();
    invalid.primary.mtime_ns += 101;
    f.catalog
        .ingest_mark_imported(&[invalid, base.clone()])
        .unwrap();
    let mut shifted = base;
    shifted.capture_kind = "mtime".into();
    shifted.primary.mtime_ns -= 96 * 900_000_000_000;
    assert!(f.catalog.ingest_seen(&shifted).unwrap().is_some());
}
#[test]
fn old_catalog_migration_and_legacy_settings_keep_decisions_and_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("old.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE photos(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,modified_at INTEGER NOT NULL,size_bytes INTEGER NOT NULL,rating INTEGER NOT NULL DEFAULT 0,flag TEXT NOT NULL DEFAULT 'none',label TEXT NOT NULL DEFAULT '',data TEXT NOT NULL);").unwrap();
    let photo = Photo {
        path: PathBuf::from("/legacy/A.jpg"),
        filename: "A.jpg".into(),
        ..Default::default()
    };
    conn.execute("INSERT INTO photos(id,path,modified_at,size_bytes,rating,flag,label,data) VALUES(9,?1,0,1,4,'pick','Red',?2)",rusqlite::params![photo.path.to_string_lossy(),serde_json::to_string(&photo).unwrap()]).unwrap();
    drop(conn);
    let catalog = Catalog::open(&path).unwrap();
    assert_eq!(catalog.photos().unwrap()[0].id, 9);
    assert_eq!(catalog.photos().unwrap()[0].rating, 4);
    assert_eq!(catalog.ingest_sequences().unwrap().stored, 0);
    assert_eq!(
        catalog.ingest_history(10).unwrap()["total_imported_groups"],
        0
    );
    let settings: Settings = serde_json::from_str(
        r#"{"theme":"Shadowroom Dark","auto_advance":false,"future_setting":123}"#,
    )
    .unwrap();
    assert_eq!(settings.ingest_on_insert, "ask");
    assert_eq!(settings.ingest_day_start_hour, 3);
    assert_eq!(settings.extra["future_setting"], 123);
    Catalog::open(&path).unwrap();
}
#[test]
fn cull_before_copy_filter_and_decisions_carry_to_destination() {
    let f = Fixture::new();
    let source = f.jpeg("A.jpg");
    f.jpeg("B.jpg");
    let index = shadowroom::indexer::Indexer::start(
        f.catalog.clone(),
        f.source.clone(),
        Default::default(),
    );
    while !index.is_finished() {
        let _ = index.recv_timeout(Duration::from_millis(10));
    }
    index.join().unwrap();
    let photos = f.catalog.photos().unwrap();
    let picked = photos.iter().find(|p| p.path == source).unwrap();
    f.catalog
        .set_decisions(&[PhotoDecision {
            id: picked.id,
            rating: 5,
            flag: Flag::Pick,
            label: ColorLabel::Green,
            trashed: false,
        }])
        .unwrap();
    let scan = f.scan();
    let mut options = f.options();
    options.selection = "picks".into();
    let plan = ingest::plan(&scan, options, SequenceState::default()).unwrap();
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].group.photo.id, picked.id);
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.completed, 1);
    let photos = f.catalog.photos().unwrap();
    let dest = photos
        .iter()
        .find(|p| p.path.starts_with(&plan.options.library_root))
        .unwrap();
    assert_eq!(dest.rating, 5);
    assert_eq!(dest.flag, Flag::Pick);
    assert_eq!(dest.label, ColorLabel::Green);
    assert_ne!(dest.id, picked.id);
}
#[test]
fn videos_and_thm_wav_xmp_lrv_carry_but_catalog_remains_stills_only() {
    let f = Fixture::new();
    for (name, bytes) in [
        ("CLIP.MOV", b"fake video".as_slice()),
        ("CLIP.THM", b"thumbnail"),
        ("CLIP.WAV", b"audio"),
        ("CLIP.XMP", b"xmp"),
        ("CLIP.LRV", b"proxy"),
        ("SOLO.LRV", b"standalone video"),
    ] {
        fs::write(f.source.join(name), bytes).unwrap();
    }
    let scan = f.scan();
    assert_eq!(scan.groups.len(), 2);
    let clip = scan
        .groups
        .iter()
        .find(|g| g.photo.filename == "CLIP.MOV")
        .unwrap();
    assert_eq!(clip.sidecars.len(), 4);
    let plan = ingest::plan(&scan, f.options(), SequenceState::default()).unwrap();
    assert_eq!(plan.videos, 2);
    assert_eq!(plan.files, 6);
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.completed, 2);
    assert!(f.catalog.photos().unwrap().is_empty());
    for file in report.records.iter().flat_map(|r| &r.files) {
        assert_eq!(
            fs::read(&file.source).unwrap(),
            fs::read(&file.destinations[0]).unwrap()
        );
    }
    let mut options = f.options();
    options.include_videos = false;
    assert!(
        ingest::plan(&scan, options, SequenceState::default())
            .unwrap()
            .groups
            .is_empty()
    );
}
#[test]
fn device_clock_utc_local_unknown_and_two_minute_tolerance() {
    use ingest::{DeviceClock::*, infer_device_clock as clock};
    assert_eq!(clock(1_000_000, 1_050_000, 19800), Utc);
    assert_eq!(clock(1_000_000 + 19_800_000, 1_000_000, 19800), Local);
    assert_eq!(clock(2_000_000, 1_000_000, 19800), Unknown);
    assert_eq!(clock(1_120_000, 1_000_000, 0), Utc);
}
#[test]
fn stat_scan_skips_ignored_symlinks_and_reports_zero_length() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    fs::write(f.source.join("zero.mov"), []).unwrap();
    fs::create_dir(f.source.join("Screenshots")).unwrap();
    fs::write(f.source.join("Screenshots/ignored.jpg"), b"no").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(f.source.join("A.jpg"), f.source.join("link.jpg")).unwrap();
    let mut batches = 0;
    let scan = ingest::scan(
        &f.source,
        &f.catalog,
        |batch| batches += batch.len(),
        &Control::default(),
    )
    .unwrap();
    assert_eq!(scan.groups.len(), 1);
    assert_eq!(batches, 1);
    assert!(scan.warnings.iter().any(|w| w.contains("Zero-length")));
}
#[test]
fn destination_source_overlap_and_eject_test_folder_are_rejected() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let mut plan = f.plan();
    plan.options.library_root = f.source.join("inside-card");
    assert!(execute(&plan, &f.catalog, &Control::default(), |_| {}).is_err());
    assert!(!plan.options.library_root.exists());
    assert!(
        ingest::worker::eject(&f.source)
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
}

struct CorruptRaw;
impl CopyHooks for CorruptRaw {
    fn write_chunk(&self, path: &Path, file: &mut File, bytes: &[u8]) -> anyhow::Result<()> {
        let mut bytes = bytes.to_vec();
        if path.to_string_lossy().contains(".arw.") {
            bytes[0] ^= 0xff;
        }
        file.write_all(&bytes)?;
        Ok(())
    }
}
#[test]
fn failed_group_does_not_consume_stored_or_session_sequence_for_later_success() {
    let f = Fixture::new();
    fake_raw(&f.source.join("A.arw"));
    f.jpeg("B.jpg");
    let mut scan = f.scan();
    scan.groups
        .sort_by(|a, b| a.primary.path.cmp(&b.primary.path));
    let mut options = f.options();
    options.file_template = "{seq_stored:3}_{seq_session:2}".into();
    let plan = ingest::plan(&scan, options, SequenceState::default()).unwrap();
    let report = execute_with(
        &plan,
        &f.catalog,
        &Control::default(),
        &Capacity(u64::MAX),
        &CorruptRaw,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.progress.failed, 1);
    assert_eq!(report.progress.completed, 1);
    assert!(plan.options.library_root.join("001_01.jpg").exists());
    assert!(!plan.options.library_root.join("001_01.arw").exists());
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 1);
    no_parts(f.temp.path());
}
struct CorruptXmp;
impl CopyHooks for CorruptXmp {
    fn write_chunk(&self, path: &Path, file: &mut File, bytes: &[u8]) -> anyhow::Result<()> {
        if path.to_string_lossy().contains(".xmp.") {
            file.write_all(b"bad")?;
        } else {
            file.write_all(bytes)?;
        }
        Ok(())
    }
}
#[test]
fn last_sidecar_verification_failure_rolls_back_earlier_verified_media_stages() {
    let f = Fixture::new();
    fake_raw(&f.source.join("A.arw"));
    f.jpeg("A.jpg");
    fs::write(f.source.join("A.xmp"), b"good sidecar").unwrap();
    let plan = f.plan();
    let report = execute_with(
        &plan,
        &f.catalog,
        &Control::default(),
        &Capacity(u64::MAX),
        &CorruptXmp,
        |_| {},
    )
    .unwrap();
    assert_eq!(report.progress.failed, 1);
    assert!(
        f.all_files("library")
            .iter()
            .all(|p| p.extension().is_some_and(|e| e == "json"))
    );
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    no_parts(f.temp.path());
}
#[test]
fn cancel_keeps_committed_groups_and_retry_skips_them_without_counter_gaps() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    f.jpeg("B.jpg");
    let plan = f.plan();
    let control = Control::default();
    let report = execute(&plan, &f.catalog, &control, |progress| {
        if progress.completed == 1 {
            control.cancel();
        }
    })
    .unwrap();
    assert_eq!(report.progress.completed, 1);
    assert_eq!(report.progress.phase, "cancelled");
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 1);
    let resumed = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(resumed.progress.skipped, 1);
    assert_eq!(resumed.progress.completed, 1);
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 2);
    no_parts(f.temp.path());
}
#[test]
fn worker_scan_plan_start_poll_history_and_unsupported_eject_without_gui() {
    use shadowroom::{
        ingest::worker::{Command, Event, Worker},
        service::IoEvent,
    };
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let (events, rx) = crossbeam_channel::unbounded();
    let (changed, _) = crossbeam_channel::bounded(1);
    let worker = Worker::start(f.catalog.clone(), events, changed);
    let wait = |predicate: &dyn Fn(&Event) -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(std::time::Instant::now() < deadline, "worker event timeout");
            if let Ok(IoEvent::Ingest(event)) = rx.recv_timeout(Duration::from_millis(50)) {
                if predicate(&event) {
                    break event;
                }
                if let Event::Error(error) = event {
                    panic!("worker error: {error}");
                }
            }
        }
    };
    worker
        .send(Command::Scan {
            path: f.source.clone(),
            auto: None,
        })
        .unwrap();
    let event = wait(&|e| matches!(e, Event::Scanned(_)));
    assert!(matches!(event,Event::Scanned(s) if s.groups.len()==1));
    let mut options = f.options();
    options.folder_template = "{YYYY}/{date}{_job}".into();
    options.file_template = "{date}_{time}_{seq_stored:4}".into();
    options.backup_roots = vec![f.temp.path().join("backup")];
    options.job_code = "worker".into();
    worker.send(Command::Plan(options)).unwrap();
    let event = wait(&|e| matches!(e, Event::Plan(_)));
    assert!(
        matches!(event,Event::Plan(p) if p.ready && p.space.len()==2 && p.plan.example.is_some())
    );
    worker.send(Command::Start).unwrap();
    let event = wait(&|e| matches!(e,Event::Progress(p) if p.phase=="completed"));
    assert!(matches!(event,Event::Progress(p) if p.completed==1));
    worker.send(Command::History(10)).unwrap();
    let event = wait(&|e| matches!(e, Event::History(_)));
    assert!(matches!(event,Event::History(h) if h["total_imported_groups"]==1));
    worker.send(Command::Eject(f.source.clone())).unwrap();
    let event = wait(&|e| matches!(e, Event::Error(_)));
    assert!(matches!(event,Event::Error(e) if e.contains("unsupported")));
    worker.stop().unwrap();
    assert_eq!(f.catalog.photos().unwrap().len(), 1);
}
#[test]
fn second_fixture_jpeg_and_readonly_source_dest_symlink_never_overwritten() {
    let f = Fixture::new();
    let source = f.jpeg("A.jpg");
    let original = fs::read(&source).unwrap();
    let mut permissions = fs::metadata(&source).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&source, permissions).unwrap();
    let plan = f.plan();
    fs::create_dir(&plan.options.library_root).unwrap();
    #[cfg(unix)]
    {
        let keep = f.temp.path().join("keep");
        fs::write(&keep, b"keep").unwrap();
        std::os::unix::fs::symlink(&keep, plan.options.library_root.join("A.jpg")).unwrap();
        let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
        assert_eq!(report.progress.completed, 1);
        assert_eq!(fs::read(&keep).unwrap(), b"keep");
        assert_eq!(
            fs::read(plan.options.library_root.join("A-1.jpg")).unwrap(),
            original
        );
        assert_eq!(fs::read(&source).unwrap(), original);
    }
}

#[test]
fn card_scan_limits_camera_roots_while_folder_scan_includes_whole_source() {
    let f = Fixture::new();
    f.jpeg("outside.jpg");
    fs::create_dir(f.source.join("DCIM")).unwrap();
    fs::create_dir(f.source.join("PRIVATE")).unwrap();
    fs::create_dir(f.source.join("MP_ROOT")).unwrap();
    fs::copy(
        f.source.join("outside.jpg"),
        f.source.join("DCIM/inside.jpg"),
    )
    .unwrap();
    fs::write(f.source.join("PRIVATE/clip.mov"), b"video").unwrap();
    fs::write(f.source.join("MP_ROOT/clip2.mts"), b"video2").unwrap();
    let card = ingest::scan_card(&f.source, &f.catalog, |_| {}, &Control::default()).unwrap();
    assert_eq!(card.groups.len(), 3);
    assert!(
        card.groups
            .iter()
            .all(|g| g.relative_path.components().count() == 2)
    );
    assert_eq!(f.scan().groups.len(), 4);
}

#[cfg(unix)]
#[test]
fn unreadable_sidecar_is_reported_and_prevents_partial_group_success() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let sidecar = f.source.join("A.xmp");
    fs::write(&sidecar, b"keep editing metadata").unwrap();
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o000)).unwrap();
    let scan = f.scan();
    assert_eq!(scan.groups[0].sidecars.len(), 1);
    assert!(scan.warnings.iter().any(|w| w.contains("Unreadable")));
    let plan = ingest::plan(&scan, f.options(), SequenceState::default()).unwrap();
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.failed, 1);
    assert_eq!(report.progress.completed, 0);
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    no_parts(f.temp.path());
    fs::set_permissions(sidecar, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn database_commit_failure_rolls_back_outputs_and_never_advances_counters() {
    let f = Fixture::new();
    f.jpeg("A.jpg");
    let plan = f.plan();
    let db = rusqlite::Connection::open(f.catalog.path()).unwrap();
    db.execute_batch("CREATE TRIGGER reject_ingest BEFORE INSERT ON ingest_log BEGIN SELECT RAISE(ABORT,'injected commit failure'); END;").unwrap();
    drop(db);
    let report = execute(&plan, &f.catalog, &Control::default(), |_| {}).unwrap();
    assert_eq!(report.progress.failed, 1);
    assert!(report.progress.errors[0].contains("injected commit failure"));
    assert_eq!(f.catalog.ingest_sequences().unwrap().stored, 0);
    assert!(f.catalog.photos().unwrap().is_empty());
    assert!(!plan.options.library_root.join("A.jpg").exists());
    no_parts(f.temp.path());
}
