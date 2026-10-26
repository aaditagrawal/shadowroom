//! Desktop integration without GPUI or SSH: real stdio child, catalog actors,
//! bounded media workers, persistence, offline culling and safe local boundaries.
use crossbeam_channel::Receiver;
use shadowroom::{
    catalog::Catalog,
    grouping::{GroupingMode, GroupingOptions, group_photos},
    media::{MediaPipeline, PreviewKind, RemotePreviewProvider},
    model::{ColorLabel, Flag, Photo, PhotoDecision, PhotoSource, RemoteSource},
    network::{HostConfig, HostSnapshot, RemoteCommand},
    remote::{
        ConnectionSpec, PreviewCache, PreviewResult, RemoteHost,
        protocol::{MAX_PAYLOAD, PhotoKey, PhotoRecord},
    },
    service::{IoCommand, IoEvent, PhotoSlot, start_io},
    settings::Settings,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn record(root: &str, volume: &str, name: &str) -> PhotoRecord {
    PhotoRecord {
        key: PhotoKey {
            root_id: root.into(),
            volume_id: volume.into(),
            relative_path: name.into(),
            size_bytes: 99,
            modified_at: 42,
            companion_path: None,
            companion_size_bytes: 0,
            companion_modified_at: 0,
        },
        metadata: Photo {
            id: 999,
            path: name.into(),
            filename: name.into(),
            folder: "same".into(),
            captured_at: Some(42),
            camera: "same camera".into(),
            rating: 5,
            flag: Flag::Pick,
            label: ColorLabel::Red,
            trashed: true,
            width: 600,
            height: 400,
            ..Default::default()
        },
        revision: 1,
    }
}
fn apply(
    catalog: &Catalog,
    token: &str,
    device: &str,
    records: &[PhotoRecord],
    deleted: &[PathBuf],
    revision: u64,
    full: bool,
) {
    catalog
        .stage_remote(token, device, "root", records, deleted)
        .unwrap();
    catalog
        .finish_remote_sync(token, device, "root", revision, full)
        .unwrap();
}
#[test]
fn old_schema_migrates_in_place_and_local_identity_remains_local() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE photos(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,modified_at INTEGER NOT NULL,size_bytes INTEGER NOT NULL,rating INTEGER NOT NULL DEFAULT 0,flag TEXT NOT NULL DEFAULT 'none',label TEXT NOT NULL DEFAULT '',data TEXT NOT NULL);").unwrap();
    let mut data = serde_json::to_value(Photo {
        path: "/original.jpg".into(),
        ..Default::default()
    })
    .unwrap();
    data.as_object_mut().unwrap().remove("source");
    conn.execute(
        "INSERT INTO photos VALUES(77,'/original.jpg',0,0,4,'pick','Purple',?1)",
        [data.to_string()],
    )
    .unwrap();
    drop(conn);
    let catalog = Catalog::open(&path).unwrap();
    let photos = catalog.photos().unwrap();
    assert_eq!(photos[0].id, 77);
    assert_eq!(photos[0].source, PhotoSource::Local);
    assert_eq!(photos[0].rating, 4);
    assert_eq!(photos[0].flag, Flag::Pick);
    assert_eq!(photos[0].label, ColorLabel::Purple);
    apply(
        &catalog,
        "sync",
        "device",
        &[record("root", "volume", "same/a.jpg")],
        &[],
        1,
        true,
    );
    assert_eq!(catalog.photos().unwrap().len(), 2);
    drop(catalog);
    assert_eq!(Catalog::open(&path).unwrap().photos().unwrap()[0].id, 77);
}
#[test]
fn sync_is_atomic_preserves_decisions_and_tombstones_keep_identity() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(&dir.path().join("db")).unwrap();
    let mut original = record("root", "volume", "same/a.jpg");
    apply(
        &catalog,
        "first",
        "device",
        &[original.clone()],
        &[],
        1,
        true,
    );
    let mut photo = catalog.photos().unwrap().remove(0);
    assert_ne!(photo.id, 999);
    assert_eq!(photo.rating, 0);
    assert_eq!(photo.flag, Flag::None);
    assert!(!photo.trashed);
    let decision = PhotoDecision {
        id: photo.id,
        rating: 3,
        flag: Flag::Reject,
        label: ColorLabel::Blue,
        trashed: true,
    };
    catalog.set_decisions(&[decision]).unwrap();
    original.key.modified_at += 1;
    original.key.companion_path = Some("same/a.jpeg".into());
    original.key.companion_size_bytes = 12;
    original.metadata.dhash = Some(0x1234);
    catalog
        .stage_remote("interrupted", "device", "root", &[original.clone()], &[])
        .unwrap();
    assert_eq!(catalog.remote_revision("device", "root").unwrap(), 1);
    photo = catalog.photos().unwrap().remove(0);
    assert_eq!(photo.modified_at, 42);
    catalog.discard_remote_stage("interrupted").unwrap();
    apply(&catalog, "second", "device", &[original], &[], 2, false);
    photo = catalog.photos().unwrap().remove(0);
    assert_eq!(PhotoDecision::from(&photo), decision);
    assert_eq!(photo.modified_at, 43);
    assert_eq!(photo.dhash, Some(0x1234));
    apply(
        &catalog,
        "third",
        "device",
        &[],
        &["same/a.jpg".into()],
        3,
        false,
    );
    photo = catalog.photos().unwrap().remove(0);
    assert!(!photo.remote_source().unwrap().available);
    assert_eq!(PhotoDecision::from(&photo), decision);
    // Different devices, roots and volumes never collide on a relative path.
    apply(
        &catalog,
        "other-device",
        "device-2",
        &[record("root", "volume", "same/a.jpg")],
        &[],
        1,
        true,
    );
    apply(
        &catalog,
        "other-volume",
        "device",
        &[record("root", "volume-2", "same/a.jpg")],
        &[],
        4,
        false,
    );
    catalog
        .stage_remote(
            "other-root",
            "device",
            "root-2",
            &[record("root-2", "volume", "same/a.jpg")],
            &[],
        )
        .unwrap();
    catalog
        .finish_remote_sync("other-root", "device", "root-2", 1, true)
        .unwrap();
    assert_eq!(catalog.photos().unwrap().len(), 4);
    assert_eq!(catalog.remote_revision("device", "root").unwrap(), 4);
    // Full snapshots reconcile rows omitted after agent state recovery.
    apply(&catalog, "empty-full", "device-2", &[], &[], 2, true);
    assert!(
        catalog
            .photos()
            .unwrap()
            .iter()
            .filter(|p| p.remote_source().unwrap().device_id == "device-2")
            .all(|p| !p.remote_source().unwrap().available)
    );
    assert!(
        catalog
            .stage_remote(
                "invalid",
                "device",
                "root",
                &[record("root", "volume", "../escape.jpg")],
                &[]
            )
            .is_err()
    );
    assert!(
        catalog
            .stage_remote(
                "wrong-root",
                "device",
                "root",
                &[record("another", "volume", "a.jpg")],
                &[]
            )
            .is_err()
    );
}
fn remote_photo(device: &str, root: &str, id: i64) -> Photo {
    let key = record(root, "volume", "same/a.jpg").key;
    Photo {
        id,
        path: "same/a.jpg".into(),
        folder: "same".into(),
        captured_at: Some(42),
        camera: "same".into(),
        width: 600,
        height: 400,
        source: PhotoSource::Remote(RemoteSource {
            device_id: device.into(),
            root_id: root.into(),
            volume_id: key.volume_id,
            relative_path: key.relative_path,
            size_bytes: key.size_bytes,
            modified_at: key.modified_at,
            companion_path: None,
            companion_size_bytes: 0,
            companion_modified_at: 0,
            available: true,
        }),
        ..Default::default()
    }
}
#[test]
fn grouping_and_folder_scopes_separate_hosts_and_roots() {
    let photos = vec![
        remote_photo("A", "root", 1),
        remote_photo("A", "root", 2),
        remote_photo("B", "root", 3),
        remote_photo("B", "root", 4),
        remote_photo("A", "other", 5),
        remote_photo("A", "other", 6),
    ];
    let groups = group_photos(
        &photos,
        GroupingOptions {
            mode: GroupingMode::Time,
            ..Default::default()
        },
    );
    assert_eq!(groups, vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
    assert_ne!(photos[0].folder_key(), photos[2].folder_key());
    assert_ne!(photos[0].folder_key(), photos[4].folder_key());
    assert!(photos[0].matches_scope(&photos[0].remote_source().unwrap().root_scope()));
    assert!(!photos[2].matches_scope(&photos[0].remote_source().unwrap().root_scope()));
}
fn wait_preview(
    pipeline: &MediaPipeline,
    photo: &Photo,
    kind: PreviewKind,
) -> Arc<shadowroom::media::Preview> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        pipeline.request_photo(photo, kind);
        if let Some(preview) = pipeline.get_photo(photo, kind) {
            return preview;
        }
        if let Some(error) = pipeline.photo_error(photo, kind) {
            panic!("{}", error.message);
        }
        assert!(Instant::now() < deadline, "preview timeout");
        thread::sleep(Duration::from_millis(10));
    }
}
fn wait_error(pipeline: &MediaPipeline, photo: &Photo, kind: PreviewKind) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pipeline.request_photo(photo, kind);
        if let Some(error) = pipeline.photo_error(photo, kind) {
            return error.message;
        }
        assert!(Instant::now() < deadline, "error timeout");
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn remote_sources_cannot_fall_through_to_local_reads_exports_or_xmp() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("tempting-local.jpg");
    image::RgbImage::new(600, 400).save(&local).unwrap();
    let mut photo = remote_photo("offline", "root", 1);
    photo.path = local.clone();
    photo.flag = Flag::Pick;
    let pipeline = MediaPipeline::new(dir.path().join("media"), 64 * 1024 * 1024);
    pipeline.set_remote_provider(Arc::new(
        RemotePreviewProvider::new(dir.path().join("wire")).unwrap(),
    ));
    assert!(wait_error(&pipeline, &photo, PreviewKind::Thumbnail).contains("Unavailable offline"));
    assert_eq!(pipeline.stats().source_reads, 0);
    assert!(
        photo
            .require_local("Reveal in Files")
            .unwrap_err()
            .to_string()
            .contains("remote")
    );
    assert!(
        shadowroom::duplicates::DuplicateJob::start(1, vec![photo.clone()], dir.path().join("db"))
            .err()
            .unwrap()
            .to_string()
            .contains("remote")
    );
    let catalog = Catalog::open(&dir.path().join("db")).unwrap();
    assert!(
        catalog
            .upsert_photos(&[photo])
            .unwrap_err()
            .to_string()
            .contains("remote")
    );
    apply(
        &catalog,
        "sync",
        "offline",
        &[record("root", "volume", "same/a.jpg")],
        &[],
        1,
        true,
    );
    let photo = catalog.photos().unwrap().remove(0);
    catalog.set_flag(photo.id, Flag::Pick).unwrap();
    assert!(
        catalog
            .prepare_export(&dir.path().join("out"), true, &[])
            .err()
            .unwrap()
            .to_string()
            .contains("remote")
    );
    assert!(
        catalog
            .export_xmp(&[photo.id], &dir.path().join("xmp"))
            .unwrap_err()
            .to_string()
            .contains("remote")
    );
    assert!(!dir.path().join("out").exists());
    assert!(!dir.path().join("xmp").exists());
    assert!(local.exists());
}
fn cache_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum()
}
#[test]
fn client_cache_enforces_byte_lru_and_keeps_recent_representation() {
    let dir = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(dir.path().into()).unwrap();
    cache.set_budget(6800).unwrap();
    let a = record("root", "volume", "a.jpg").key;
    let b = record("root", "volume", "b.jpg").key;
    let c = record("root", "volume", "c.jpg").key;
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode_image(&image::RgbImage::from_pixel(10, 10, image::Rgb([7; 3])))
        .unwrap();
    jpeg.resize(3000, 0); // Keep the byte-budget fixture, with a real JPEG header.
    let preview = PreviewResult {
        jpeg,
        width: 10,
        height: 10,
        cache_hit: false,
        rendered_raw: false,
        embedded_raw: false,
        rendering_note: None,
    };
    for key in [&a, &b] {
        cache
            .put("device", key, PreviewKind::Thumbnail, &preview)
            .unwrap();
    }
    assert!(
        cache
            .get("device", &a, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .unwrap()
            .is_some()
    );
    cache
        .put("device", &c, PreviewKind::Thumbnail, &preview)
        .unwrap();
    assert!(cache_bytes(dir.path()) <= 6800);
    assert!(
        cache
            .get("device", &a, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .get("device", &b, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .get("device", &c, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .unwrap()
            .is_some()
    );
    cache.set_budget(0).unwrap();
    assert_eq!(cache_bytes(dir.path()), 0);
}
fn fixture(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("photo.jpg");
    image::RgbImage::from_fn(600, 400, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    })
    .save(&path)
    .unwrap();
    path
}
fn local_spec(state: &Path, root: &Path) -> ConnectionSpec {
    ConnectionSpec::Local {
        program: env!("CARGO_BIN_EXE_shadowroom-remote").into(),
        args: local_args(state, root),
    }
}
fn local_args(state: &Path, root: &Path) -> Vec<String> {
    vec![
        "serve".into(),
        "--stdio".into(),
        "--no-discover".into(),
        "--state-dir".into(),
        state.to_string_lossy().into(),
        "--root".into(),
        root.to_string_lossy().into(),
        "--cache-budget-mb".into(),
        "0".into(),
    ]
}
#[test]
fn real_child_provider_deduplicates_caches_and_reports_uncached_offline_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    fixture(&root);
    let provider = Arc::new(RemotePreviewProvider::new(dir.path().join("wire")).unwrap());
    let host = RemoteHost::connect_cached(
        local_spec(&dir.path().join("agent"), &root),
        provider.cache(),
        Duration::from_secs(30),
    )
    .unwrap();
    let root_id = host.list_roots().unwrap()[0].id.clone();
    host.index(&root_id, |_| {}).unwrap();
    let sync = host.photos(&root_id, 0).unwrap();
    let catalog = Catalog::open(&dir.path().join("db")).unwrap();
    catalog
        .stage_remote(
            "sync",
            &host.hello().device_id,
            &root_id,
            &sync.records,
            &[],
        )
        .unwrap();
    catalog
        .finish_remote_sync(
            "sync",
            &host.hello().device_id,
            &root_id,
            sync.revision,
            true,
        )
        .unwrap();
    let photo = catalog.photos().unwrap().remove(0);
    provider.connect(host.hello().device_id.clone(), host.clone());
    let pipeline = MediaPipeline::new(dir.path().join("media"), 64 * 1024 * 1024);
    pipeline.set_remote_provider(provider.clone());
    let before = host.stats();
    for _ in 0..64 {
        pipeline.request_photo(&photo, PreviewKind::Thumbnail);
    }
    let preview = wait_preview(&pipeline, &photo, PreviewKind::Thumbnail);
    assert_eq!(preview.width, 384);
    assert_eq!(preview.source_width, 600);
    let after = host.stats();
    assert!(after.received_payload_bytes > before.received_payload_bytes);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pipeline.stats().decoded == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(pipeline.stats().decoded, 1);
    assert_eq!(pipeline.stats().source_reads, 0);
    let cached = provider
        .fetch(
            photo.remote_source().unwrap(),
            PreviewKind::Thumbnail,
            || false,
        )
        .unwrap();
    assert!(cached.cache_hit);
    assert_eq!(host.stats().sent_bytes, after.sent_bytes);
    assert_eq!(host.stats().received_bytes, after.received_bytes);
    // A zero agent cache budget removes wire JPEGs, without deleting originals.
    assert_eq!(
        cache_bytes(&dir.path().join(format!(
            "agent/jpeg-v{}",
            shadowroom::remote::protocol::PREVIEW_CACHE_VERSION
        ))),
        0
    );
    assert!(root.join("photo.jpg").exists());
    host.disconnect();
    provider.disconnect(&host.hello().device_id);
    let reopened = Arc::new(RemotePreviewProvider::new(dir.path().join("wire")).unwrap());
    let offline = MediaPipeline::new(dir.path().join("offline-media"), 64 * 1024 * 1024);
    offline.set_remote_provider(reopened);
    let preview = wait_preview(&offline, &photo, PreviewKind::Thumbnail);
    assert_eq!(preview.width, 384);
    assert!(wait_error(&offline, &photo, PreviewKind::Full).contains("Unavailable offline"));
    assert_eq!(offline.stats().source_reads, 0);
}
fn wait_host(
    events: &Receiver<IoEvent>,
    predicate: impl Fn(&HostSnapshot) -> bool,
) -> HostSnapshot {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("host event timeout")
        {
            IoEvent::Remote(host) if predicate(&host) => return host,
            IoEvent::Error(error) => panic!("{error}"),
            IoEvent::Remote(host) if host.state == "error" => panic!("{:?}", host.error),
            _ => (),
        }
    }
}
fn wait_photos(slot: &PhotoSlot, predicate: impl Fn(&[Photo]) -> bool) -> Vec<Photo> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(photos) = slot.lock().unwrap().take() {
            if predicate(&photos) {
                return photos;
            }
        }
        assert!(Instant::now() < deadline, "catalog snapshot timeout");
        thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn catalog_actor_local_child_host_lifecycle_sync_tombstones_and_offline_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let original = fixture(&root);
    let data = dir.path().join("desktop");
    let (commands, events, photos, _) = start_io(
        data.clone(),
        Settings {
            auto_discover_drives: false,
            ..Default::default()
        },
        None,
    );
    let config = HostConfig {
        id: "test-host".into(),
        display_name: "Test host".into(),
        ssh_alias: "unused-local".into(),
        remote_command: "unused-local".into(),
        extra_ssh_args: vec![],
        local_program: Some(env!("CARGO_BIN_EXE_shadowroom-remote").into()),
        local_args: local_args(&dir.path().join("agent"), &root),
    };
    commands
        .send(IoCommand::Remote(RemoteCommand::Add(config)))
        .unwrap();
    let connected = wait_host(&events, |h| h.state == "connected");
    let root_id = connected.roots[0].id.clone();
    commands
        .send(IoCommand::Remote(RemoteCommand::Index {
            host: "test-host".into(),
            root: root_id.clone(),
        }))
        .unwrap();
    let indexed = wait_host(&events, |h| {
        h.state == "connected" && h.revisions.contains_key(&root_id)
    });
    assert!(indexed.revisions[&root_id] > 0);
    let photo = wait_photos(&photos, |p| p.len() == 1).remove(0);
    assert!(photo.is_remote());
    let decision = PhotoDecision {
        id: photo.id,
        rating: 4,
        flag: Flag::Pick,
        label: ColorLabel::Green,
        trashed: false,
    };
    commands.send(IoCommand::Decisions(vec![decision])).unwrap();
    commands
        .send(IoCommand::Remote(RemoteCommand::SyncFull {
            host: "test-host".into(),
            root: root_id.clone(),
        }))
        .unwrap();
    wait_host(&events, |h| h.state == "indexing");
    wait_host(&events, |h| h.state == "connected");
    fs::remove_file(original).unwrap();
    commands
        .send(IoCommand::Remote(RemoteCommand::Index {
            host: "test-host".into(),
            root: root_id.clone(),
        }))
        .unwrap();
    wait_host(&events, |h| {
        h.state == "connected"
            && h.revisions
                .get(&root_id)
                .is_some_and(|r| *r > indexed.revisions[&root_id])
    });
    let photo = wait_photos(&photos, |p| {
        p.len() == 1 && !p[0].remote_source().unwrap().available
    })
    .remove(0);
    assert_eq!(PhotoDecision::from(&photo), decision);
    commands
        .send(IoCommand::Remote(RemoteCommand::Disconnect(
            "test-host".into(),
        )))
        .unwrap();
    wait_host(&events, |h| h.state == "offline");
    let offline_decision = PhotoDecision {
        rating: 5,
        ..decision
    };
    commands
        .send(IoCommand::Decisions(vec![offline_decision]))
        .unwrap();
    let (ack, done) = crossbeam_channel::bounded(1);
    commands.send(IoCommand::Shutdown(ack)).unwrap();
    done.recv_timeout(Duration::from_secs(15)).unwrap();
    let saved = Catalog::open(&data.join("catalog.sqlite3"))
        .unwrap()
        .photos()
        .unwrap()
        .remove(0);
    assert_eq!(PhotoDecision::from(&saved), offline_decision);
    let hosts: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(hosts["test-host"]["state"], "offline");
    assert_eq!(
        hosts["test-host"]["hello"]["device_id"],
        connected.hello.unwrap().device_id
    );
    // A new service reopens persisted host roots and offline metadata without a connection.
    let (commands, events, photos, _) = start_io(
        data,
        Settings {
            auto_discover_drives: false,
            ..Default::default()
        },
        None,
    );
    let persisted = wait_host(&events, |h| h.state == "offline");
    assert_eq!(persisted.roots[0].id, root_id);
    assert_eq!(wait_photos(&photos, |p| p.len() == 1)[0].rating, 5);
    commands
        .send(IoCommand::Remote(RemoteCommand::Remove("test-host".into())))
        .unwrap();
    let (ack, done) = crossbeam_channel::bounded(1);
    commands.send(IoCommand::Shutdown(ack)).unwrap();
    done.recv_timeout(Duration::from_secs(15)).unwrap();
}
