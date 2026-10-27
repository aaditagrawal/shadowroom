#![cfg(unix)]
use shadowroom::{
    media::PreviewKind,
    remote::{
        Agent, AgentOptions, PreviewCache, PreviewResult, RemoteHost, Request, Response,
        protocol::*,
    },
};
use std::{
    fs,
    os::unix::{fs::symlink, net::UnixStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use tempfile::TempDir;
struct Running {
    host: RemoteHost,
    socket: UnixStream,
    worker: Option<thread::JoinHandle<anyhow::Result<()>>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.host.disconnect();
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(w) = self.worker.take() {
            w.join().unwrap().unwrap();
        }
    }
}
fn start(state: &Path, root: &Path, cache: &Path) -> Running {
    let mut options = AgentOptions::new(state.into());
    options.discover_drives = false;
    options.roots.push(root.into());
    start_options(options, cache)
}
fn start_options(options: AgentOptions, cache: &Path) -> Running {
    let agent = Agent::new(options).unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let socket = client.try_clone().unwrap();
    let output = server.try_clone().unwrap();
    let worker = thread::spawn(move || agent.serve(server, output));
    let host = RemoteHost::from_io(
        client.try_clone().unwrap(),
        client,
        cache.into(),
        Duration::from_secs(30),
    )
    .unwrap();
    Running {
        host,
        socket,
        worker: Some(worker),
    }
}
fn fixture(root: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let dest = root.join(name);
    // Every host exercises identical generated inputs, including developer
    // machines with unrelated photo fixtures in their temporary directories.
    let seed = name.bytes().fold(0u8, u8::wrapping_add);
    let image = image::RgbImage::from_fn(600, 400, |x, y| {
        image::Rgb([
            (x % 256) as u8 ^ seed,
            (y % 256) as u8,
            ((x + y) % 256) as u8,
        ])
    });
    image.save(&dest).unwrap();
    dest
}
#[test]
fn index_sync_preview_cache_offline_and_incremental_revisions() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    let cache = temp.path().join("client-cache");
    let source = fixture(&root, "shoot.jpg");
    fixture(&root, "ui.jpg");
    let state = temp.path().join("agent-state");
    let running = start(&state, &root, &cache);
    let host = &running.host;
    let hello = host.hello();
    assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
    assert!(
        hello
            .capabilities
            .iter()
            .any(|capability| capability == "preview_jpeg_first_v4")
    );
    assert!(
        !hello
            .capabilities
            .iter()
            .any(|capability| capability == "preview_provenance_v2")
    );
    let roots = host.list_roots().unwrap();
    assert_eq!(roots.len(), 1);
    let id = &roots[0].id;
    let mut progress = vec![];
    let summary = host.index(id, |p| progress.push(p.clone())).unwrap();
    assert_eq!(summary.progress.indexed, 2);
    assert!(!progress.is_empty());
    let sync = host.photos(id, 0).unwrap();
    assert_eq!(sync.records.len(), 2);
    assert!(sync.revision > 0);
    assert!(
        sync.records
            .iter()
            .all(|p| p.key.relative_path.is_relative() && p.metadata.path.is_relative())
    );
    assert!(host.photos(id, sync.revision).unwrap().records.is_empty());
    let key = &sync.records[0].key;
    let mut forged = key.clone();
    forged.volume_id = "wrong-volume".into();
    assert!(
        host.preview(&forged, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .is_err()
    );
    forged = key.clone();
    forged.relative_path = "../secret.jpg".into();
    assert!(
        host.preview(&forged, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .is_err()
    );

    let first = host
        .preview(key, PreviewKind::Thumbnail, MAX_PAYLOAD)
        .unwrap();
    assert!(!first.cache_hit);
    assert_eq!(first.width.max(first.height), 384);
    assert_eq!(
        image::load_from_memory(&first.jpeg).unwrap().width(),
        first.width
    );
    let before = host.stats();
    let second = host
        .preview(key, PreviewKind::Thumbnail, MAX_PAYLOAD)
        .unwrap();
    let after = host.stats();
    assert!(second.cache_hit);
    assert_eq!(first.jpeg, second.jpeg);
    assert_eq!(
        (before.sent_bytes, before.received_bytes),
        (after.sent_bytes, after.received_bytes)
    );
    assert!(host.preview(key, PreviewKind::Thumbnail, 1).is_err());
    assert_eq!(
        host.read_range(id, Path::new("shoot.jpg"), 5, 100).unwrap(),
        fs::read(&source).unwrap()[5..105]
    );
    assert!(matches!(
        host.stat(id, Path::new("shoot.jpg")).unwrap(),
        Response::Stat { is_dir: false, .. }
    ));
    assert!(
        host.read_range(id, Path::new("shoot.jpg"), 0, MAX_RANGE + 1)
            .is_err()
    );
    let repeat = host.index(id, |_| {}).unwrap();
    assert_eq!(repeat.progress.unchanged, 2);
    // The preview pipeline publishes its computed dhash without a second decode.
    assert!(repeat.revision > sync.revision);
    let hashes = host.photos(id, sync.revision).unwrap();
    assert_eq!(hashes.records.len(), 1);
    assert!(hashes.records[0].metadata.dhash.is_some());
    assert_eq!(host.index(id, |_| {}).unwrap().revision, repeat.revision);
    fs::remove_file(&source).unwrap();
    let changed = host.index(id, |_| {}).unwrap();
    assert!(changed.revision > sync.revision);
    let diff = host.photos(id, repeat.revision).unwrap();
    assert_eq!(diff.deleted, vec![PathBuf::from("shoot.jpg")]);
    assert!(host.photos(id, changed.revision + 1).is_err());
    host.disconnect();
    assert!(
        host.preview(key, PreviewKind::Thumbnail, MAX_PAYLOAD)
            .unwrap()
            .cache_hit
    );
    assert!(
        RemoteHost::offline_preview(
            cache.clone(),
            &hello.device_id,
            key,
            PreviewKind::Thumbnail,
            MAX_PAYLOAD
        )
        .unwrap()
        .is_some()
    );
    assert!(
        RemoteHost::offline_preview(
            cache.clone(),
            "other-device",
            key,
            PreviewKind::Thumbnail,
            MAX_PAYLOAD
        )
        .unwrap()
        .is_none()
    );
    let mut changed_key = key.clone();
    changed_key.companion_modified_at += 1;
    assert!(
        RemoteHost::offline_preview(
            cache.clone(),
            &hello.device_id,
            &changed_key,
            PreviewKind::Thumbnail,
            MAX_PAYLOAD
        )
        .unwrap()
        .is_none()
    );
    drop(running);
    // Device identity and revisions survive server restart; a new client cache
    // exercises the agent JPEG cache rather than the already populated client.
    let restarted = start(&state, &root, &temp.path().join("new-cache"));
    assert_eq!(restarted.host.hello().device_id, hello.device_id);
    assert_eq!(
        restarted.host.photos(id, 0).unwrap().revision,
        changed.revision
    );
    let remaining = restarted.host.photos(id, 0).unwrap().records.remove(0);
    restarted
        .host
        .preview(&remaining.key, PreviewKind::Thumbnail, MAX_PAYLOAD)
        .unwrap();
}
#[test]
fn root_confinement_and_read_only_storage() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fs::create_dir(&root).unwrap();
    fs::write(temp.path().join("secret"), "outside").unwrap();
    symlink(temp.path().join("secret"), root.join("escape")).unwrap();
    symlink(temp.path(), root.join("escaped-dir")).unwrap();
    let running = start(
        &temp.path().join("state"),
        &root,
        &temp.path().join("cache"),
    );
    let id = running.host.list_roots().unwrap().remove(0).id;
    for path in [
        "../secret",
        "nested/../../secret",
        "/etc/passwd",
        "escape",
        "escaped-dir/secret",
    ] {
        assert!(
            running
                .host
                .read_range(&id, Path::new(path), 0, 32)
                .is_err(),
            "{path}"
        );
        assert!(running.host.stat(&id, Path::new(path)).is_err(), "{path}");
    }
    assert!(running.host.stat("unknown", Path::new("")).is_err());
    let mut options = AgentOptions::new(root.join("state"));
    options.discover_drives = false;
    options.roots.push(root.clone());
    assert!(Agent::new(options).is_err());
    assert!(!root.join("state").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("secret")).unwrap(),
        "outside"
    );
}
#[test]
fn version_negotiation_and_clean_eof() {
    let temp = TempDir::new().unwrap();
    let mut options = AgentOptions::new(temp.path().join("state"));
    options.discover_drives = false;
    let agent = Agent::new(options).unwrap();
    let (mut client, server) = UnixStream::pair().unwrap();
    let writer = server.try_clone().unwrap();
    let worker = thread::spawn(move || agent.serve(server, writer));
    write_frame(&mut client, &Frame::new(1, true, Request::Ping, vec![])).unwrap();
    assert!(
        matches!(read_frame::<Response>(&mut client).unwrap().unwrap().header.message, Response::Error { code, .. } if code == "hello_required")
    );
    write_frame(
        &mut client,
        &Frame::new(
            2,
            true,
            Request::Hello {
                protocol_version: 999,
                app_version: "test".into(),
            },
            vec![],
        ),
    )
    .unwrap();
    assert!(
        matches!(read_frame::<Response>(&mut client).unwrap().unwrap().header.message, Response::Error { code, .. } if code == "version_mismatch")
    );
    write_frame(
        &mut client,
        &Frame::new(
            3,
            true,
            Request::Hello {
                protocol_version: PROTOCOL_VERSION,
                app_version: "test".into(),
            },
            vec![],
        ),
    )
    .unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client)
            .unwrap()
            .unwrap()
            .header
            .message,
        Response::Hello(_)
    ));
    client.shutdown(std::net::Shutdown::Write).unwrap();
    worker.join().unwrap().unwrap();
}
#[test]
fn closed_peer_during_hello_ack_is_a_clean_session_end() {
    struct ClosedPeer;
    impl std::io::Write for ClosedPeer {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let temp = TempDir::new().unwrap();
    let mut options = AgentOptions::new(temp.path().join("state"));
    options.discover_drives = false;
    let agent = Agent::new(options).unwrap();
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::new(
            1,
            true,
            Request::Hello {
                protocol_version: PROTOCOL_VERSION,
                app_version: "closed-peer-test".into(),
            },
            vec![],
        ),
    )
    .unwrap();
    agent
        .serve(std::io::Cursor::new(wire.clone()), ClosedPeer)
        .unwrap();
    // Disconnect may truncate a newly queued cancellation frame after the
    // session was negotiated. Joining workers still precedes clean return.
    wire.extend_from_slice(&[1, 2]);
    agent.serve(std::io::Cursor::new(wire), Vec::new()).unwrap();
    // Malformed input still fails; closed-peer handling must not mask it.
    assert!(
        agent
            .serve(std::io::Cursor::new([1, 2]), Vec::new())
            .is_err()
    );
}

#[test]
fn multiplexing_cancellation_and_deadlines() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fs::create_dir(&root).unwrap();
    // Hold the first catalog commit so Started cannot race a completed index.
    // Invalid JPEGs also enqueue warning frames; counting a fixed number of
    // frames used to fail before the cancellation response reached the reader.
    let seed = temp.path().join("seed.jpg");
    image::RgbImage::new(8, 8).save(&seed).unwrap();
    for i in 0..2000 {
        fs::hard_link(&seed, root.join(format!("photo-{i}.jpg"))).unwrap();
    }
    let running = start(
        &temp.path().join("state"),
        &root,
        &temp.path().join("cache"),
    );
    let host = &running.host;
    let root_id = host.list_roots().unwrap().remove(0).id;
    let writer =
        rusqlite::Connection::open(temp.path().join("state").join(format!("{root_id}.sqlite")))
            .unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut index = host
        .request(Request::Index {
            root: root_id.clone(),
        })
        .unwrap();
    assert!(matches!(
        index.recv().unwrap().header.message,
        Response::Progress(_)
    ));
    let mut second = host.request(Request::Index { root: root_id }).unwrap();
    second.cancel();
    index.cancel();
    // The ordered reader handles Cancel immediately. A following Ping response
    // proves those frames were consumed before releasing the catalog writer.
    host.ping().unwrap();
    writer.execute_batch("ROLLBACK").unwrap();
    let cancelled = loop {
        match index.recv() {
            Ok(frame) if frame.header.done => {
                break matches!(frame.header.message, Response::Indexed(s) if s.cancelled);
            }
            Err(error) => {
                break error.to_string().contains("cancelled");
            }
            _ => (),
        }
    };
    assert!(cancelled);
    match second.recv() {
        Err(_) => (),
        Ok(f) => assert!(matches!(f.header.message, Response::Indexed(s) if s.cancelled)),
    }
    // Responses route by id with many requests outstanding.
    let mut pings: Vec<_> = (0..8)
        .map(|_| host.request(Request::Ping).unwrap())
        .collect();
    for request in &mut pings {
        assert!(matches!(
            request.recv().unwrap().header.message,
            Response::Pong
        ));
    }
    let mut expired = host
        .request_with_timeout(Request::Ping, Duration::ZERO)
        .unwrap();
    // If the response already arrived, zero timeout may still receive it; an idle
    // custom peer below exercises a guaranteed timeout instead.
    let _ = expired.recv();
    let (client, mut peer) = UnixStream::pair().unwrap();
    let control = client.try_clone().unwrap();
    let peer_worker = thread::spawn(move || {
        let hello = read_frame::<Request>(&mut peer).unwrap().unwrap();
        write_frame(
            &mut peer,
            &Frame::new(
                hello.header.id,
                true,
                Response::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    app_version: "test".into(),
                    device_id: "test".into(),
                    hostname: "test".into(),
                    os: "test".into(),
                    arch: "test".into(),
                    capabilities: vec![],
                }),
                vec![],
            ),
        )
        .unwrap();
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut peer, &mut sink).unwrap();
    });
    let silent = RemoteHost::from_io(
        client.try_clone().unwrap(),
        client,
        temp.path().join("silent-cache"),
        Duration::from_millis(30),
    )
    .unwrap();
    assert!(silent.ping().unwrap_err().to_string().contains("timed out"));
    silent.disconnect();
    control.shutdown(std::net::Shutdown::Both).unwrap();
    peer_worker.join().unwrap();
}
#[test]
fn local_child_and_reconnect() {
    use shadowroom::remote::ConnectionSpec;
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fixture(&root, "photo.jpg");
    let spec = ConnectionSpec::Local {
        program: PathBuf::from(env!("CARGO_BIN_EXE_shadowroom-remote")),
        args: vec![
            "serve".into(),
            "--stdio".into(),
            "--no-discover".into(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--state-dir".into(),
            temp.path().join("state").to_string_lossy().into_owned(),
        ],
    };
    let host =
        RemoteHost::connect(spec, temp.path().join("cache"), Duration::from_secs(30)).unwrap();
    let device = host.hello().device_id;
    host.ping().unwrap();
    host.disconnect();
    assert!(!host.is_connected());
    host.reconnect(2).unwrap();
    assert_eq!(host.hello().device_id, device);
    host.ping().unwrap();
    host.disconnect();
}

#[test]
fn bounded_streams_deliver_more_than_mailbox_capacity_and_kind_sizes() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fs::create_dir(&root).unwrap();
    let image = image::RgbImage::from_pixel(16, 12, image::Rgb([120u8, 60, 20]));
    for i in 0..40 {
        image.save(root.join(format!("tiny-{i:03}.jpg"))).unwrap();
    }
    fixture(&root, "large.jpg");
    let state = temp.path().join("state");
    let running = start(&state, &root, &temp.path().join("cache"));
    let id = running.host.list_roots().unwrap().remove(0).id;
    running.host.index(&id, |_| {}).unwrap();
    let records = running.host.photos(&id, 0).unwrap().records;
    assert_eq!(records.len(), 41);
    thread::scope(|scope| {
        let jobs: Vec<_> = records
            .iter()
            .filter(|r| r.key.relative_path != Path::new("large.jpg"))
            .take(4)
            .map(|r| {
                let host = &running.host;
                scope.spawn(move || {
                    host.preview(&r.key, PreviewKind::Thumbnail, MAX_PAYLOAD)
                        .unwrap()
                })
            })
            .collect();
        for job in jobs {
            assert!(!job.join().unwrap().jpeg.is_empty());
        }
    });

    let key = &records
        .iter()
        .find(|p| p.key.relative_path == Path::new("large.jpg"))
        .unwrap()
        .key;
    let fit = running
        .host
        .preview(key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap();
    assert!(fit.width.max(fit.height) <= 2560);
    let thumb = running
        .host
        .preview(key, PreviewKind::Thumbnail, MAX_PAYLOAD)
        .unwrap();
    assert!(thumb.width.max(thumb.height) <= 384);
    let mut other = AgentOptions::new(state);
    other.discover_drives = false;
    assert!(Agent::new(other).is_err());
}

#[test]
fn scan_errors_preserve_unseen_records_until_deletions_are_verified() {
    use std::os::unix::fs::PermissionsExt;
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    image::RgbImage::from_pixel(16, 12, image::Rgb([120u8, 60, 20]))
        .save(nested.join("photo.jpg"))
        .unwrap();
    let running = start(
        &temp.path().join("state"),
        &root,
        &temp.path().join("cache"),
    );
    let id = running.host.list_roots().unwrap().remove(0).id;
    let initial = running.host.index(&id, |_| {}).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read_dir(&nested).is_ok() {
        // root can bypass permissions on some CI hosts.
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let partial = running.host.index(&id, |_| {});
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
    let partial = partial.unwrap();
    assert!(partial.progress.errors > 0);
    assert_eq!(partial.revision, initial.revision);
    assert_eq!(running.host.photos(&id, 0).unwrap().records.len(), 1);
    assert!(
        running
            .host
            .photos(&id, initial.revision)
            .unwrap()
            .deleted
            .is_empty()
    );
}

#[test]
fn analyze_fills_perceptual_hashes_without_preview_requests() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    let cache = temp.path().join("client-cache");
    fixture(&root, "a.jpg");
    fixture(&root, "b.jpg");
    let running = start(&temp.path().join("agent-state"), &root, &cache);
    let host = &running.host;
    assert!(host.hello().capabilities.iter().any(|c| c == "analyze"));
    let id = host.list_roots().unwrap().remove(0).id;
    host.index(&id, |_| ()).unwrap();
    let before = host.photos(&id, 0).unwrap();
    assert!(before.records.iter().all(|p| p.metadata.dhash.is_none()));
    let mut pending = host.request(Request::Analyze { root: id.clone() }).unwrap();
    let summary = loop {
        let frame = pending.recv().unwrap();
        if frame.header.done {
            match frame.header.message {
                Response::Indexed(summary) => break summary,
                other => panic!("unexpected analyze completion: {other:?}"),
            }
        }
    };
    assert_eq!(summary.progress.indexed, 2);
    assert_eq!(summary.progress.errors, 0);
    assert!(summary.revision > before.revision);
    // Incremental sync delivers the hashes; a full listing shows every record hashed.
    assert!(
        !host
            .photos(&id, before.revision)
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        host.photos(&id, 0)
            .unwrap()
            .records
            .iter()
            .all(|p| p.metadata.dhash.is_some())
    );
}

fn cache_test_key() -> PhotoKey {
    PhotoKey {
        root_id: "root".into(),
        volume_id: "volume".into(),
        relative_path: "sensor.dng".into(),
        size_bytes: 100,
        modified_at: 42,
        companion_path: Some("sensor.jpg".into()),
        companion_size_bytes: 50,
        companion_modified_at: 40,
    }
}

fn cache_test_jpeg() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode_image(&image::RgbImage::from_pixel(
            64,
            48,
            image::Rgb([20, 30, 40]),
        ))
        .unwrap();
    bytes
}

#[test]
fn preview_protocol_defaults_and_provenance_roundtrip() {
    let legacy = serde_json::json!({
        "type": "preview", "width": 64, "height": 48, "mime": "image/jpeg"
    });
    assert!(matches!(
        serde_json::from_value::<Response>(legacy).unwrap(),
        Response::Preview {
            rendered_raw: false,
            embedded_raw: false,
            rendering_note: None,
            ..
        }
    ));
    for (rendered_raw, embedded_raw) in [(true, false), (false, true)] {
        let note = "Full resolution · Remote JPEG (quality 85) · 64 × 48 pixels · LibRaw";
        let response = Response::Preview {
            width: 64,
            height: 48,
            mime: "image/jpeg".into(),
            rendered_raw,
            embedded_raw,
            rendering_note: Some(note.into()),
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &Frame::new(12, true, response, vec![0xff, 0xd8])).unwrap();
        let frame = read_frame::<Response>(&mut std::io::Cursor::new(wire))
            .unwrap()
            .unwrap();
        match frame.header.message {
            Response::Preview {
                rendered_raw: r,
                embedded_raw: e,
                rendering_note,
                ..
            } => {
                assert_eq!((r, e), (rendered_raw, embedded_raw));
                assert_eq!(rendering_note.as_deref(), Some(note));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

#[test]
fn cache_preserves_raw_provenance_and_rejects_legacy_full_keys() {
    let temp = TempDir::new().unwrap();
    let key = cache_test_key();
    let kind = PreviewKind::Full;
    // Neither unversioned nor sensor-only v3 representations may satisfy v4.
    let old_hash =
        blake3::hash(&serde_json::to_vec(&(PROTOCOL_VERSION, "device", &key, kind)).unwrap())
            .to_hex()
            .to_string();
    let sensor_hash =
        blake3::hash(&serde_json::to_vec(&(PROTOCOL_VERSION, 3u32, "device", &key, kind)).unwrap())
            .to_hex()
            .to_string();
    let bytes = cache_test_jpeg();
    for hash in [old_hash, sensor_hash] {
        let old_meta = serde_json::json!({
            "key": hash,
            "jpeg_hash": blake3::hash(&bytes).to_hex().to_string(),
            "width": 64, "height": 48,
            "rendered_raw": true,
            "rendering_note": "obsolete sensor-only Full"
        });
        fs::write(temp.path().join(format!("{hash}.jpg")), &bytes).unwrap();
        fs::write(
            temp.path().join(format!("{hash}.json")),
            serde_json::to_vec(&old_meta).unwrap(),
        )
        .unwrap();
    }
    let cache = PreviewCache::new(temp.path().into()).unwrap();
    assert!(
        cache
            .get("device", &key, kind, MAX_PAYLOAD)
            .unwrap()
            .is_none()
    );
    let preview = PreviewResult {
        jpeg: bytes,
        width: 64,
        height: 48,
        cache_hit: false,
        rendered_raw: true,
        embedded_raw: false,
        rendering_note: Some("Full resolution · Remote JPEG · 64 × 48 pixels · LibRaw".into()),
    };
    cache.put("device", &key, kind, &preview).unwrap();
    drop(cache);
    let cache = PreviewCache::new(temp.path().into()).unwrap();
    let hit = cache
        .get("device", &key, kind, MAX_PAYLOAD)
        .unwrap()
        .unwrap();
    assert!(hit.cache_hit && hit.rendered_raw && !hit.embedded_raw);
    assert_eq!(hit.rendering_note, preview.rendering_note);
    assert_eq!(hit.jpeg, preview.jpeg);
    assert!(cache.get("device", &key, kind, 1).is_err());
    assert!(
        cache
            .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
            .unwrap()
            .is_none()
    );
    // A truncated sidecar is a miss, never a provenance-free sensor result.
    for entry in fs::read_dir(temp.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            fs::write(path, b"{").unwrap();
        }
    }
    assert!(
        cache
            .get("device", &key, kind, MAX_PAYLOAD)
            .unwrap()
            .is_none()
    );
}

#[test]
fn paired_jpeg_full_survives_restart_and_is_available_offline() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    let state = temp.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("paired.nef"), b"invalid sensor data: not a JPEG").unwrap();
    image::RgbImage::from_pixel(120, 80, image::Rgb([20u8, 160, 40]))
        .save(root.join("paired.jpg"))
        .unwrap();
    let running = start(&state, &root, &temp.path().join("cache"));
    let root_id = running.host.list_roots().unwrap().remove(0).id;
    running.host.index(&root_id, |_| {}).unwrap();
    let records = running.host.photos(&root_id, 0).unwrap().records;
    assert_eq!(records.len(), 1);
    let key = records[0].key.clone();
    assert_eq!(key.companion_path.as_deref(), Some(Path::new("paired.jpg")));
    let fit = running
        .host
        .preview(&key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap();
    assert_eq!((fit.width, fit.height), (120, 80));
    assert!(!fit.rendered_raw && !fit.embedded_raw);
    assert!(
        fit.rendering_note
            .as_deref()
            .unwrap()
            .contains("paired companion")
    );
    assert!(
        fit.rendering_note
            .as_deref()
            .unwrap()
            .contains("Remote JPEG")
    );
    // Seed the exact old agent cache with the companion under the Full key.
    let old_dir = state.join("jpeg-v1");
    fs::create_dir_all(&old_dir).unwrap();
    let old_hash = blake3::hash(&serde_json::to_vec(&(&key, PreviewKind::Full)).unwrap())
        .to_hex()
        .to_string();
    fs::write(old_dir.join(format!("{old_hash}.jpg")), &fit.jpeg).unwrap();
    let device = running.host.hello().device_id;
    drop(running);
    let restarted = start(&state, &root, &temp.path().join("fresh-cache"));
    let server_hit = restarted
        .host
        .preview(&key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap();
    assert!(!server_hit.cache_hit); // new client; server reads JPEG + sidecar
    assert_eq!(server_hit.jpeg, fit.jpeg);
    assert_eq!(server_hit.rendering_note, fit.rendering_note);
    assert!(!server_hit.rendered_raw && !server_hit.embedded_raw);
    // Full inspection uses the companion even when the RAW cannot be decoded.
    let full = restarted
        .host
        .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap();
    assert_eq!((full.width, full.height), (120, 80));
    assert!(!full.rendered_raw && !full.embedded_raw);
    assert!(
        full.rendering_note
            .as_deref()
            .unwrap()
            .contains("paired companion")
    );
    let cached_full = RemoteHost::offline_preview(
        temp.path().join("fresh-cache"),
        &device,
        &key,
        PreviewKind::Full,
        MAX_PAYLOAD,
    )
    .unwrap()
    .unwrap();
    assert!(cached_full.cache_hit);
    assert_eq!(cached_full.jpeg, full.jpeg);
    restarted.host.disconnect();
    let offline = restarted
        .host
        .preview(&key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap();
    assert!(offline.cache_hit);
    assert_eq!(offline.rendering_note, fit.rendering_note);
    drop(restarted);

    // Server cache hits must still validate the paired source fingerprint.
    fs::write(root.join("paired.jpg"), b"changed companion").unwrap();
    let changed = start(&state, &root, &temp.path().join("uncached-client"));
    let error = changed
        .host
        .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap_err();
    assert!(
        error.to_string().contains("Preview source changed"),
        "{error:#}"
    );
}

#[test]
fn embedded_jpeg_full_retains_native_dimensions_and_provenance_across_restart() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    let state = temp.path().join("state");
    let cache = temp.path().join("cache");
    fs::create_dir(&root).unwrap();
    let mut raw = b"not sensor data; embedded camera preview follows".to_vec();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode_image(&image::RgbImage::from_pixel(
            320,
            200,
            image::Rgb([40u8, 80, 160]),
        ))
        .unwrap();
    raw.extend_from_slice(&jpeg);
    fs::write(root.join("embedded.nef"), raw).unwrap();
    let running = start(&state, &root, &cache);
    let root_id = running.host.list_roots().unwrap().remove(0).id;
    running.host.index(&root_id, |_| {}).unwrap();
    let records = running.host.photos(&root_id, 0).unwrap().records;
    assert_eq!(records.len(), 1);
    let key = &records[0].key;
    assert!(key.companion_path.is_none());
    let full = running
        .host
        .preview(key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap();
    assert_eq!((full.width, full.height), (320, 200));
    assert!(full.embedded_raw && !full.rendered_raw);
    assert!(
        full.rendering_note
            .as_deref()
            .unwrap()
            .contains("embedded RAW preview")
    );
    let device = running.host.hello().device_id;
    drop(running);
    let restarted = start(&state, &root, &temp.path().join("fresh-cache"));
    let server_hit = restarted
        .host
        .preview(key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap();
    assert_eq!(server_hit.jpeg, full.jpeg);
    assert_eq!(server_hit.rendering_note, full.rendering_note);
    assert!(server_hit.embedded_raw && !server_hit.rendered_raw);
    let offline = RemoteHost::offline_preview(cache, &device, key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap()
        .unwrap();
    assert!(offline.cache_hit && offline.embedded_raw && !offline.rendered_raw);
    assert_eq!(offline.jpeg, full.jpeg);
}

#[test]
fn decode_budget_is_explicit_and_small_budget_rejects_full_without_resizing() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fs::create_dir(&root).unwrap();
    image::RgbImage::from_pixel(512, 384, image::Rgb([80u8, 30, 170]))
        .save(root.join("photo.jpg"))
        .unwrap();
    let mut options = AgentOptions::new(temp.path().join("zero-state"));
    assert_eq!(options.decode_budget_bytes, 2 * 1024 * 1024 * 1024);
    options.discover_drives = false;
    options.decode_budget_bytes = 0;
    assert!(
        Agent::new(options)
            .err()
            .unwrap()
            .to_string()
            .contains("budget")
    );
    let mut options = AgentOptions::new(temp.path().join("bounded-state"));
    options.discover_drives = false;
    options.roots.push(root);
    options.decode_budget_bytes = 128 * 1024;
    let running = start_options(options, &temp.path().join("cache"));
    let root_id = running.host.list_roots().unwrap().remove(0).id;
    running.host.index(&root_id, |_| {}).unwrap();
    let key = running
        .host
        .photos(&root_id, 0)
        .unwrap()
        .records
        .remove(0)
        .key;
    assert!(
        running
            .host
            .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
            .is_err()
    );
    running.host.ping().unwrap();
}

#[test]
fn full_jpeg_has_exact_dimensions_visible_encoding_note_and_no_budget_resize() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("photos");
    fs::create_dir(&root).unwrap();
    image::RgbImage::from_fn(2700, 40, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    })
    .save(root.join("wide.jpg"))
    .unwrap();
    let running = start(
        &temp.path().join("state"),
        &root,
        &temp.path().join("cache"),
    );
    let root_id = running.host.list_roots().unwrap().remove(0).id;
    running.host.index(&root_id, |_| {}).unwrap();
    let key = running
        .host
        .photos(&root_id, 0)
        .unwrap()
        .records
        .remove(0)
        .key;
    // Reject, then fetch normally: the smaller budget cannot poison the
    // canonical cache with a resized or lower-quality representation.
    assert!(running.host.preview(&key, PreviewKind::Full, 1).is_err());
    let full = running
        .host
        .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
        .unwrap();
    assert_eq!((full.width, full.height), (2700, 40));
    let decoded = image::load_from_memory(&full.jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2700, 40));
    let note = full.rendering_note.as_deref().unwrap();
    assert!(
        note.contains("Full resolution")
            && note.contains("JPEG (quality 85)")
            && note.contains("2700 × 40 pixels")
    );
    assert!(!full.rendered_raw && !full.embedded_raw);
}

#[test]
#[ignore = "requires native LibRaw and generated CC0 sensor-bayer-orientation-{1,6}.dng fixtures"]
fn native_unpaired_sensor_fallback_provenance_survives_both_caches() {
    let fixture_dir = std::env::var_os("SHADOWROOM_REMOTE_SENSOR_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".local/wiker-extended-20261021/formats")
        });
    for (orientation, dimensions) in [(1, (64, 48)), (6, (48, 64))] {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("photos");
        let state = temp.path().join("state");
        let client_cache = temp.path().join("cache");
        fs::create_dir(&root).unwrap();
        fs::copy(
            fixture_dir.join(format!("sensor-bayer-orientation-{orientation}.dng")),
            root.join("sensor.dng"),
        )
        .expect("generate the CC0 sensor fixtures before this native test");
        let running = start(&state, &root, &client_cache);
        let root_id = running.host.list_roots().unwrap().remove(0).id;
        running.host.index(&root_id, |_| {}).unwrap();
        let key = running
            .host
            .photos(&root_id, 0)
            .unwrap()
            .records
            .remove(0)
            .key;
        let device = running.host.hello().device_id;
        let fit = running
            .host
            .preview(&key, PreviewKind::Fit, MAX_PAYLOAD)
            .unwrap();
        assert_eq!((fit.width, fit.height), dimensions);
        let full = running
            .host
            .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
            .unwrap();
        assert_eq!((full.width, full.height), dimensions);
        assert!(full.rendered_raw && !full.embedded_raw);
        let note = full.rendering_note.as_deref().unwrap();
        assert!(
            note.contains("LibRaw")
                && note.contains("Remote JPEG")
                && note.contains("Full resolution")
        );
        assert!(!note.contains("paired companion"));
        let jpeg = image::load_from_memory(&full.jpeg).unwrap();
        assert_eq!((jpeg.width(), jpeg.height()), dimensions);
        // The agent and offline caches retain the fallback's provenance.
        drop(running);
        let restarted = start(&state, &root, &temp.path().join("new-cache"));
        let server_hit = restarted
            .host
            .preview(&key, PreviewKind::Full, MAX_PAYLOAD)
            .unwrap();
        assert_eq!(server_hit.jpeg, full.jpeg);
        assert!(server_hit.rendered_raw && !server_hit.embedded_raw);
        assert_eq!(server_hit.rendering_note, full.rendering_note);
        let offline = RemoteHost::offline_preview(
            client_cache,
            &device,
            &key,
            PreviewKind::Full,
            MAX_PAYLOAD,
        )
        .unwrap()
        .unwrap();
        assert!(offline.cache_hit && offline.rendered_raw && !offline.embedded_raw);
        assert_eq!(offline.rendering_note, full.rendering_note);
    }
}

#[test]
fn cache_sidecars_without_provenance_fields_remain_readable_without_sensor_claims() {
    let temp = TempDir::new().unwrap();
    let key = cache_test_key();
    let hash = blake3::hash(
        &serde_json::to_vec(&(
            PROTOCOL_VERSION,
            PREVIEW_CACHE_VERSION,
            "device",
            &key,
            PreviewKind::Fit,
        ))
        .unwrap(),
    )
    .to_hex()
    .to_string();
    let jpeg = cache_test_jpeg();
    fs::write(temp.path().join(format!("{hash}.jpg")), &jpeg).unwrap();
    fs::write(
        temp.path().join(format!("{hash}.json")),
        serde_json::to_vec(&serde_json::json!({
            "key": hash,
            "jpeg_hash": blake3::hash(&jpeg).to_hex().to_string(),
            "width": 64, "height": 48
        }))
        .unwrap(),
    )
    .unwrap();
    let cache = PreviewCache::new(temp.path().into()).unwrap();
    let hit = cache
        .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap()
        .unwrap();
    assert!(!hit.rendered_raw && !hit.embedded_raw && hit.rendering_note.is_none());
    // Embedded RAW provenance must survive just like sensor-rendered provenance.
    let embedded = PreviewResult {
        jpeg,
        width: 64,
        height: 48,
        cache_hit: false,
        rendered_raw: false,
        embedded_raw: true,
        rendering_note: Some("Remote JPEG · embedded RAW preview".into()),
    };
    cache
        .put("device", &key, PreviewKind::Fit, &embedded)
        .unwrap();
    let hit = PreviewCache::new(temp.path().into())
        .unwrap()
        .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
        .unwrap()
        .unwrap();
    assert!(!hit.rendered_raw && hit.embedded_raw);
    assert_eq!(hit.rendering_note, embedded.rendering_note);
}

// A real framed peer exercises client validation and legacy default handling,
// including its persisted offline result, without any native format decoder.
fn preview_fixture_peer(
    cache: &Path,
    capabilities: Vec<String>,
    response: serde_json::Value,
    jpeg: Vec<u8>,
) -> anyhow::Result<PreviewResult> {
    let (client, mut peer) = UnixStream::pair().unwrap();
    let control = client.try_clone().unwrap();
    let worker = thread::spawn(move || {
        let hello = read_frame::<Request>(&mut peer).unwrap().unwrap();
        write_frame(
            &mut peer,
            &Frame::new(
                hello.header.id,
                true,
                Response::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    app_version: "fixture-peer".into(),
                    device_id: "fixture-peer".into(),
                    hostname: "fixture-peer".into(),
                    os: "test".into(),
                    arch: "test".into(),
                    capabilities,
                }),
                vec![],
            ),
        )
        .unwrap();
        let request = read_frame::<Request>(&mut peer).unwrap().unwrap();
        assert!(matches!(
            request.header.message,
            Request::Preview {
                kind: PreviewKind::Full,
                ..
            }
        ));
        write_frame(
            &mut peer,
            &Frame::new(request.header.id, true, response, jpeg),
        )
        .unwrap();
        let mut rest = Vec::new();
        let _ = std::io::Read::read_to_end(&mut peer, &mut rest);
    });
    let host = RemoteHost::from_io(
        client.try_clone().unwrap(),
        client,
        cache.into(),
        Duration::from_secs(5),
    )
    .unwrap();
    let result = host.preview(&cache_test_key(), PreviewKind::Full, MAX_PAYLOAD);
    host.disconnect();
    control.shutdown(std::net::Shutdown::Both).unwrap();
    worker.join().unwrap();
    result
}

#[test]
fn legacy_agent_full_is_marked_unverified_and_bad_dimensions_or_provenance_fail() {
    let temp = TempDir::new().unwrap();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode_image(&image::RgbImage::from_pixel(
            64,
            48,
            image::Rgb([40u8, 80, 160]),
        ))
        .unwrap();
    let legacy =
        serde_json::json!({ "type": "preview", "width": 64, "height": 48, "mime": "image/jpeg" });
    let cache = temp.path().join("legacy");
    let result = preview_fixture_peer(&cache, vec![], legacy.clone(), jpeg.clone()).unwrap();
    assert!(!result.rendered_raw && !result.embedded_raw);
    let note = result.rendering_note.as_deref().unwrap();
    assert!(
        note.contains("Remote JPEG") && note.contains("64 × 48") && note.contains("unverified")
    );
    let offline = RemoteHost::offline_preview(
        cache,
        "fixture-peer",
        &cache_test_key(),
        PreviewKind::Full,
        MAX_PAYLOAD,
    )
    .unwrap()
    .unwrap();
    assert_eq!(offline.rendering_note, result.rendering_note);
    assert!(!offline.rendered_raw && !offline.embedded_raw);
    for (name, capabilities, response, expected_error) in [
        (
            "bad-dimensions",
            vec![],
            serde_json::json!({
                "type": "preview", "width": 6000, "height": 4000, "mime": "image/jpeg"
            }),
            "dimensions",
        ),
        (
            "conflicting",
            vec![],
            serde_json::json!({
                "type": "preview", "width": 64, "height": 48, "mime": "image/jpeg",
                "rendered_raw": true, "embedded_raw": true
            }),
            "Conflicting",
        ),
        (
            "false-full",
            vec!["preview_provenance_v2".into()],
            legacy,
            "sensor rendered",
        ),
    ] {
        let cache = temp.path().join(name);
        let error = preview_fixture_peer(&cache, capabilities, response, jpeg.clone()).unwrap_err();
        assert!(
            error.to_string().contains(expected_error),
            "{name}: {error:#}"
        );
        assert!(
            RemoteHost::offline_preview(
                cache,
                "fixture-peer",
                &cache_test_key(),
                PreviewKind::Full,
                MAX_PAYLOAD
            )
            .unwrap()
            .is_none()
        );
    }
}
