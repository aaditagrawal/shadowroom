use super::protocol::*;
use crate::{
    catalog::Catalog,
    indexer::{self, IndexEvent, IndexOptions, Indexer},
    media::MediaPipeline,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub state_dir: PathBuf,
    pub roots: Vec<PathBuf>,
    pub discover_drives: bool,
    pub workers: usize,
    pub jpeg_cache_bytes: u64,
    /// Shared media working-set admission budget, including cached pixels and
    /// in-flight decodes. Native RAW estimates for 24 MP exceed 512 MiB.
    pub decode_budget_bytes: usize,
}
impl AgentOptions {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            roots: Vec::new(),
            discover_drives: true,
            workers: 4,
            jpeg_cache_bytes: 4 * 1024 * 1024 * 1024,
            decode_budget_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    revision: u64,
    records: BTreeMap<PathBuf, PhotoRecord>,
    deleted: BTreeMap<PathBuf, u64>,
}
struct Root {
    info: RemoteRoot,
    catalog: Catalog,
    manifest_path: PathBuf,
    manifest: Mutex<Manifest>,
    indexing: Mutex<()>,
}
struct State {
    _state_lock: File,
    hello: Hello,
    roots: Vec<Root>,
    media: MediaPipeline,
    jpeg_cache: super::client::PreviewCache,
}
pub struct Agent {
    state: Arc<State>,
    workers: usize,
}
type Stops = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;
struct Work {
    id: u64,
    request: Request,
    stop: Arc<AtomicBool>,
}

// Keep the existing single-frame JPEG transport bounded while encoding, too.
// Checking only after encoding allowed an oversized representation to grow
// until the encoder finished, even though it could never be transmitted.
#[derive(Default)]
struct JpegPayload(Vec<u8>);
impl Write for JpegPayload {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_PAYLOAD.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Preview exceeds protocol payload limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// IndexJob's public Drop cancels but does not join. Keep the source worker
// inside the agent lifecycle even when a protocol write fails mid-index.
struct JoinedIndex(Option<crate::indexer::IndexJob>);
impl std::ops::Deref for JoinedIndex {
    type Target = crate::indexer::IndexJob;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl JoinedIndex {
    fn join(mut self) -> Result<()> {
        self.0.take().unwrap().join()
    }
}
impl Drop for JoinedIndex {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job.cancel();
            let _ = job.join();
        }
    }
}

impl Agent {
    /// Validate storage before creating it: the agent never stores state on a served root.
    /// `discover_drives=false` is useful for explicit allowlists and isolated tests.
    pub fn new(options: AgentOptions) -> Result<Self> {
        ensure!(
            options.decode_budget_bytes > 0,
            "Decode budget must be positive"
        );
        let mut paths = options.roots;
        if options.discover_drives {
            paths.extend(indexer::discover_drives().into_iter().map(|d| d.path));
        }
        let mut paths = paths
            .into_iter()
            .map(|p| p.canonicalize())
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        paths.dedup();
        let state_dir = resolve_future_dir(&options.state_dir)?;
        for root in &paths {
            ensure!(root.is_dir(), "Root must be a directory");
            ensure!(
                !state_dir.starts_with(root),
                "Agent state must be outside photo roots"
            );
            ensure!(root.to_str().is_some(), "Roots must be valid UTF-8");
        }
        fs::create_dir_all(&state_dir)?;
        let state_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state_dir.join("agent.lock"))?;
        state_lock.try_lock().context("Agent state directory is already in use; one stdio agent per state directory is supported")?;
        let device_id = device_id(&state_dir)?;
        let mut roots: Vec<Root> = Vec::new();
        for path in paths {
            let (volume_id, identity_kind, mount, filesystem_root) = volume_identity(&path);
            let suffix = filesystem_root.join(path.strip_prefix(&mount).unwrap_or(&path));
            let id = blake3::hash(&serde_json::to_vec(&(&volume_id, suffix))?)
                .to_hex()
                .to_string();
            ensure!(
                roots.iter().all(|r| r.info.id != id),
                "Ambiguous duplicate root identity; configure distinct roots or filesystem UUIDs"
            );
            let info = RemoteRoot {
                id: id.clone(),
                volume_id,
                identity_kind,
                label: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path,
            };
            let manifest_path = state_dir.join(format!("{id}.json"));
            let manifest = match fs::read(&manifest_path) {
                Ok(bytes) => {
                    serde_json::from_slice(&bytes).context("Read remote revision manifest")?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
                Err(e) => return Err(e.into()),
            };
            roots.push(Root {
                info,
                catalog: Catalog::open(&state_dir.join(format!("{id}.sqlite")))?,
                manifest_path,
                manifest: Mutex::new(manifest),
                indexing: Mutex::new(()),
            });
        }
        let jpeg_cache = super::client::PreviewCache::new(
            state_dir.join(format!("jpeg-v{PREVIEW_CACHE_VERSION}")),
        )?;
        jpeg_cache.set_budget(options.jpeg_cache_bytes)?;
        let hostname = std::env::var("HOSTNAME")
            .ok()
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            })
            .unwrap_or_else(|| "unknown".into());
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            device_id,
            hostname,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            capabilities: vec![
                "index".into(),
                "analyze".into(),
                "revision_sync".into(),
                "jpeg_preview".into(),
                "preview_provenance_v2".into(),
                "read_range".into(),
                "cancel".into(),
                "read_only".into(),
            ],
        };
        Ok(Self {
            workers: options.workers.clamp(1, 4),
            state: Arc::new(State {
                _state_lock: state_lock,
                hello,
                roots,
                media: MediaPipeline::new(state_dir.join("media"), options.decode_budget_bytes),
                jpeg_cache,
            }),
        })
    }
    /// Runs until EOF. stdout must contain protocol bytes only. Four bounded workers
    /// service pipelined requests while this reader handles cancellation immediately.
    pub fn serve<R: Read, W: Write + Send>(&self, mut reader: R, writer: W) -> Result<()> {
        let writer = Mutex::new(writer);
        let stops: Stops = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = crossbeam_channel::bounded::<Work>(MAX_IN_FLIGHT);
        thread::scope(|scope| -> Result<()> {
            let mut handles = Vec::new();
            for _ in 0..self.workers {
                let rx = rx.clone();
                let stops = stops.clone();
                let writer = &writer;
                let state = &self.state;
                handles.push(scope.spawn(move || {
                    while let Ok(work) = rx.recv() {
                        let result = state.handle(&work, writer);
                        if let Err(error) = result {
                            let code = if work.stop.load(Ordering::Relaxed) {
                                "cancelled"
                            } else {
                                "request_failed"
                            };
                            let _ = send(
                                writer,
                                work.id,
                                true,
                                Response::Error {
                                    code: code.into(),
                                    message: format!("{error:#}"),
                                },
                                Vec::new(),
                            );
                        }
                        stops.lock().unwrap().remove(&work.id);
                    }
                }));
            }
            let mut negotiated = false;
            let result = (|| -> Result<()> {
                while let Some(frame) = read_frame::<Request>(&mut reader)? {
                    let id = frame.header.id;
                    ensure!(
                        id != 0 && frame.header.done && frame.payload.is_empty(),
                        "Invalid request envelope"
                    );
                    if let Request::Hello {
                        protocol_version, ..
                    } = &frame.header.message
                    {
                        if *protocol_version != PROTOCOL_VERSION {
                            send(
                                &writer,
                                id,
                                true,
                                Response::Error {
                                    code: "version_mismatch".into(),
                                    message: format!("Agent requires protocol {PROTOCOL_VERSION}"),
                                },
                                vec![],
                            )?;
                            continue;
                        }
                        ensure!(!negotiated, "Hello already negotiated");
                        send(
                            &writer,
                            id,
                            true,
                            Response::Hello(self.state.hello.clone()),
                            vec![],
                        )?;
                        negotiated = true;
                        continue;
                    }
                    if !negotiated {
                        send(
                            &writer,
                            id,
                            true,
                            Response::Error {
                                code: "hello_required".into(),
                                message: "Negotiate hello first".into(),
                            },
                            vec![],
                        )?;
                        continue;
                    }
                    if let Request::Cancel { request_id } = frame.header.message {
                        if let Some(stop) = stops.lock().unwrap().get(&request_id) {
                            stop.store(true, Ordering::Relaxed);
                        }
                        send(
                            &writer,
                            id,
                            true,
                            Response::Cancelled { request_id },
                            vec![],
                        )?;
                        continue;
                    }
                    let stop = Arc::new(AtomicBool::new(false));
                    {
                        let mut active = stops.lock().unwrap();
                        ensure!(!active.contains_key(&id), "Duplicate active request id");
                        if active.len() >= MAX_IN_FLIGHT {
                            send(
                                &writer,
                                id,
                                true,
                                Response::Error {
                                    code: "busy".into(),
                                    message: "Too many requests".into(),
                                },
                                vec![],
                            )?;
                            continue;
                        }
                        active.insert(id, stop.clone());
                    }
                    if tx
                        .try_send(Work {
                            id,
                            request: frame.header.message,
                            stop,
                        })
                        .is_err()
                    {
                        stops.lock().unwrap().remove(&id);
                        send(
                            &writer,
                            id,
                            true,
                            Response::Error {
                                code: "busy".into(),
                                message: "Request queue full".into(),
                            },
                            vec![],
                        )?;
                    }
                }
                Ok(())
            })();
            for stop in stops.lock().unwrap().values() {
                stop.store(true, Ordering::Relaxed);
            }
            drop(tx);
            for handle in handles {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("Agent worker panicked"))?;
            }
            // A peer may close while a cancellation acknowledgement is being
            // sent. Workers have already been stopped and joined; a closed
            // transport is an ordinary end of session, not a protocol failure.
            match result {
                Err(error)
                    if error.chain().any(|cause| {
                        cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
                            matches!(
                                e.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ) || (negotiated && e.kind() == std::io::ErrorKind::UnexpectedEof)
                        })
                    }) =>
                {
                    Ok(())
                }
                result => result,
            }
        })
    }
}
fn send(
    writer: &Mutex<impl Write>,
    id: u64,
    done: bool,
    message: Response,
    payload: Vec<u8>,
) -> Result<()> {
    write_frame(
        &mut *writer
            .lock()
            .map_err(|_| anyhow::anyhow!("Writer poisoned"))?,
        &Frame::new(id, done, message, payload),
    )?;
    Ok(())
}
fn check(stop: &AtomicBool) -> Result<()> {
    ensure!(!stop.load(Ordering::Relaxed), "Request cancelled");
    Ok(())
}
impl State {
    fn root(&self, id: &str) -> Result<&Root> {
        self.roots
            .iter()
            .find(|r| r.info.id == id)
            .context("Unknown root id")
    }
    fn handle(&self, work: &Work, writer: &Mutex<impl Write>) -> Result<()> {
        let id = work.id;
        let stop = &*work.stop;
        check(stop)?;
        let response = match &work.request {
            Request::ListRoots => Response::Roots {
                roots: self.roots.iter().map(|r| r.info.clone()).collect(),
            },
            Request::Ping => Response::Pong,
            Request::Stat { root, path } => {
                let metadata = fs::metadata(confined(&self.root(root)?.info.path, path)?)?;
                Response::Stat {
                    size_bytes: metadata.len(),
                    modified_at: mtime(&metadata),
                    is_dir: metadata.is_dir(),
                }
            }
            Request::ReadRange {
                root,
                path,
                offset,
                len,
            } => {
                ensure!(*len <= MAX_RANGE, "Range exceeds 1 MiB limit");
                offset.checked_add(*len as u64).context("Range overflow")?;
                let mut file = File::open(confined(&self.root(root)?.info.path, path)?)?;
                ensure!(file.metadata()?.is_file(), "Range requires regular file");
                file.seek(SeekFrom::Start(*offset))?;
                let mut bytes = Vec::new();
                file.take(*len as u64).read_to_end(&mut bytes)?;
                check(stop)?;
                return send(writer, id, true, Response::Range, bytes);
            }
            Request::Photos {
                root,
                since_revision,
            } => {
                let root = self.root(root)?;
                // Snapshot under lock, release before network writes.
                let manifest = root.manifest.lock().unwrap();
                ensure!(
                    *since_revision <= manifest.revision,
                    "Revision ahead of agent; reset since_revision to zero"
                );
                let records: Vec<_> = manifest
                    .records
                    .values()
                    .filter(|r| r.revision > *since_revision)
                    .cloned()
                    .collect();
                let deleted: Vec<_> = manifest
                    .deleted
                    .iter()
                    .filter(|(_, r)| **r > *since_revision)
                    .map(|(p, _)| p.clone())
                    .collect();
                let revision = manifest.revision;
                drop(manifest);
                for photo in records {
                    check(stop)?;
                    send(writer, id, false, Response::Photo(photo), vec![])?;
                }
                for relative_path in deleted {
                    check(stop)?;
                    send(
                        writer,
                        id,
                        false,
                        Response::Deleted { relative_path },
                        vec![],
                    )?;
                }
                Response::Synced { revision }
            }
            Request::Index { root } => {
                let root = self.root(root)?;
                let _guard = loop {
                    check(stop)?;
                    if let Ok(guard) = root.indexing.try_lock() {
                        break guard;
                    }
                    thread::sleep(Duration::from_millis(10));
                };
                confined(&root.info.path, Path::new(""))?;
                let job = JoinedIndex(Some(Indexer::start(
                    root.catalog.clone(),
                    root.info.path.clone(),
                    IndexOptions::default(),
                )));
                let mut summary = IndexSummary::default();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        job.cancel();
                    }
                    match job.recv_timeout(Duration::from_millis(20)) {
                        Ok(event) => {
                            let (p, phase, done, cancelled) = match event {
                                IndexEvent::Started(_) => {
                                    send(
                                        writer,
                                        id,
                                        false,
                                        Response::Progress(Progress {
                                            phase: "started".into(),
                                            ..Progress::default()
                                        }),
                                        vec![],
                                    )?;
                                    continue;
                                }
                                IndexEvent::Progress(p) => (p, "indexing", false, false),
                                IndexEvent::Finished(p) => (p, "finished", true, false),
                                IndexEvent::Cancelled(p) => (p, "cancelled", true, true),
                                IndexEvent::Error { message, .. } => {
                                    send(
                                        writer,
                                        id,
                                        false,
                                        Response::Error {
                                            code: "index_warning".into(),
                                            message,
                                        },
                                        vec![],
                                    )?;
                                    continue;
                                }
                            };
                            summary.progress = Progress {
                                scanned: p.scanned,
                                indexed: p.indexed,
                                unchanged: p.unchanged,
                                errors: p.errors,
                                elapsed_ms: p.elapsed_ms,
                                phase: phase.into(),
                            };
                            send(
                                writer,
                                id,
                                false,
                                Response::Progress(summary.progress.clone()),
                                vec![],
                            )?;
                            if done {
                                summary.cancelled = cancelled;
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => (),
                        Err(_) => bail!("Indexer ended without completion"),
                    }
                }
                job.join()?;
                summary.cancelled |= stop.load(Ordering::Relaxed);
                if !summary.cancelled {
                    summary.revision = root.refresh_manifest(summary.progress.errors == 0)?;
                } else {
                    summary.revision = root.manifest.lock().unwrap().revision;
                }
                return send(writer, id, true, Response::Indexed(summary), vec![]);
            }
            Request::Analyze { root } => {
                let root = self.root(root)?;
                let _guard = loop {
                    check(stop)?;
                    if let Ok(guard) = root.indexing.try_lock() {
                        break guard;
                    }
                    thread::sleep(Duration::from_millis(10));
                };
                // Thumbnails decoded here also warm the agent's preview cache, so
                // later client thumbnail requests are served without decoding.
                let missing: Vec<_> = root
                    .catalog
                    .photos()?
                    .into_iter()
                    .filter(|p| {
                        p.dhash.is_none() && !p.trashed && p.path.starts_with(&root.info.path)
                    })
                    .collect();
                let started = Instant::now();
                let mut progress = Progress {
                    phase: "analyzing".into(),
                    scanned: missing.len() as u64,
                    ..Progress::default()
                };
                send(
                    writer,
                    id,
                    false,
                    Response::Progress(progress.clone()),
                    vec![],
                )?;
                let kind = crate::media::PreviewKind::Thumbnail;
                for batch in missing.chunks(8) {
                    let mut jobs = Vec::with_capacity(batch.len());
                    for p in batch {
                        let path = p.preview_path().to_path_buf();
                        match fs::metadata(&path) {
                            Ok(meta) if meta.is_file() => {
                                let (modified, size) = (mtime(&meta), meta.len());
                                self.media.request_with_size(&path, modified, size, kind);
                                jobs.push((p.id, path, modified, size));
                            }
                            _ => progress.errors += 1,
                        }
                    }
                    let mut hashes = Vec::with_capacity(jobs.len());
                    for (photo, path, modified, size) in jobs {
                        let deadline = Instant::now() + Duration::from_secs(120);
                        loop {
                            check(stop)?;
                            if let Some(preview) =
                                self.media.get_with_size(&path, modified, size, kind)
                            {
                                hashes.push((photo, preview.dhash));
                                break;
                            }
                            if self
                                .media
                                .error_with_size(&path, modified, size, kind)
                                .is_some()
                                || Instant::now() > deadline
                            {
                                progress.errors += 1;
                                break;
                            }
                            self.media.request_with_size(&path, modified, size, kind);
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                    root.catalog.set_dhashes(&hashes)?;
                    progress.indexed += hashes.len() as u64;
                    progress.elapsed_ms = started.elapsed().as_millis() as u64;
                    send(
                        writer,
                        id,
                        false,
                        Response::Progress(progress.clone()),
                        vec![],
                    )?;
                }
                progress.phase = "finished".into();
                let revision = root.refresh_manifest(false)?;
                return send(
                    writer,
                    id,
                    true,
                    Response::Indexed(IndexSummary {
                        progress,
                        revision,
                        cancelled: false,
                    }),
                    vec![],
                );
            }
            Request::Preview {
                key,
                kind,
                max_bytes,
            } => {
                ensure!(
                    *max_bytes > 0 && *max_bytes <= MAX_PAYLOAD,
                    "Preview byte budget outside bounds"
                );
                let root = self.root(&key.root_id)?;
                ensure!(
                    key.volume_id == root.info.volume_id,
                    "Volume identity mismatch"
                );
                let record = root
                    .manifest
                    .lock()
                    .unwrap()
                    .records
                    .get(&key.relative_path)
                    .cloned()
                    .context("Photo not indexed")?;
                ensure!(record.key == *key, "Stale photo key; sync metadata again");
                let path = confined(&root.info.path, &key.relative_path)?;
                let sensor =
                    *kind == crate::media::PreviewKind::Full && indexer::is_raw(&key.relative_path);
                let companion =
                    *kind != crate::media::PreviewKind::Full && key.companion_path.is_some();
                let preview_relative = if companion {
                    key.companion_path.as_deref().unwrap()
                } else {
                    &key.relative_path
                };
                let preview_path = confined(&root.info.path, preview_relative)?;
                let meta = fs::metadata(&path)?;
                ensure!(
                    meta.is_file()
                        && meta.len() == key.size_bytes
                        && mtime(&meta) == key.modified_at,
                    "Source changed; re-index first"
                );
                let (size, modified) = if companion {
                    (key.companion_size_bytes, key.companion_modified_at)
                } else {
                    (key.size_bytes, key.modified_at)
                };
                let pm = fs::metadata(&preview_path)?;
                ensure!(
                    pm.is_file() && pm.len() == size && mtime(&pm) == modified,
                    "Preview source changed; re-index first"
                );
                if let Some(cached) =
                    self.jpeg_cache
                        .get(&self.hello.device_id, key, *kind, *max_bytes)?
                {
                    // Even a well-formed sidecar must never substitute an embedded
                    // or legacy image for a Full sensor request.
                    if !sensor || (cached.rendered_raw && !cached.embedded_raw) {
                        check(stop)?;
                        return send(writer, id, true, cached.response(), cached.jpeg);
                    }
                }
                let start = Instant::now();
                let preview = loop {
                    check(stop)?;
                    ensure!(
                        start.elapsed() < Duration::from_secs(120),
                        "Preview decode timeout"
                    );
                    if let Some(preview) =
                        self.media
                            .get_with_size(&preview_path, modified, size, *kind)
                    {
                        break preview;
                    }
                    if let Some(error) =
                        self.media
                            .error_with_size(&preview_path, modified, size, *kind)
                    {
                        bail!("{}", error.message);
                    }
                    self.media
                        .request_with_size(&preview_path, modified, size, *kind);
                    thread::sleep(Duration::from_millis(5));
                };
                ensure!(
                    !sensor || (preview.rendered_raw && !preview.embedded_raw),
                    "Full RAW requires sensor rendering; embedded/paired fallback is unavailable"
                );
                ensure!(
                    *kind != crate::media::PreviewKind::Full
                        || (preview.width, preview.height)
                            == (preview.source_width, preview.source_height),
                    "Full preview is not at source resolution"
                );
                ensure!(
                    preview.rgba.len() as u64
                        == u64::from(preview.width) * u64::from(preview.height) * 4,
                    "Invalid preview pixels"
                );
                // Charge RGB/resize copies and bounded JPEG Vec growth to the
                // same budget as media decodes; no unaccounted sensor-sized
                // buffers are admitted just because decoding has completed.
                let encode_bytes = preview
                    .rgba
                    .len()
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(2 * MAX_PAYLOAD))
                    .context("JPEG working memory estimate overflow")?;
                let admission = Instant::now();
                let _encode_memory = loop {
                    check(stop)?;
                    if let Some(permit) = self.media.reserve_display_bytes(encode_bytes) {
                        break permit;
                    }
                    ensure!(
                        admission.elapsed() < Duration::from_millis(500),
                        "JPEG working memory requires approximately {} MiB; increase decode budget",
                        encode_bytes.div_ceil(1024 * 1024)
                    );
                    thread::sleep(Duration::from_millis(5));
                };
                // Convert borrowed RGBA directly: cloning the full RGBA buffer
                // before conversion added another large sensor-sized allocation.
                let mut rgb = image::RgbImage::from_fn(preview.width, preview.height, |x, y| {
                    let at = (y as usize * preview.width as usize + x as usize) * 4;
                    image::Rgb([preview.rgba[at], preview.rgba[at + 1], preview.rgba[at + 2]])
                });
                let edge = match kind {
                    crate::media::PreviewKind::Thumbnail => Some(384),
                    crate::media::PreviewKind::Fit => Some(2560),
                    crate::media::PreviewKind::Full => None,
                };
                if let Some(edge) = edge {
                    if rgb.width().max(rgb.height()) > edge {
                        rgb = image::DynamicImage::ImageRgb8(rgb)
                            .resize(edge, edge, image::imageops::FilterType::Triangle)
                            .to_rgb8();
                    }
                }
                // Publish the media pipeline hash through the existing catalog API.
                // The next index revision exports it; no extra source read is needed.
                root.catalog.set_dhash(record.metadata.id, preview.dhash)?;
                let mut payload = JpegPayload::default();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut payload, 85)
                    .encode_image(&rgb)?;
                let bytes = payload.0;
                let mut note = format!(
                    "{}Remote JPEG (quality 85) · {} × {} pixels",
                    if *kind == crate::media::PreviewKind::Full {
                        "Full resolution · "
                    } else {
                        ""
                    },
                    rgb.width(),
                    rgb.height(),
                );
                if companion {
                    note.push_str(" · paired companion");
                }
                if preview.embedded_raw {
                    note.push_str(" · embedded RAW preview");
                }
                if let Some(source_note) = &preview.rendering_note {
                    note.push_str(" · ");
                    note.push_str(source_note);
                }
                let result = super::client::PreviewResult {
                    jpeg: bytes,
                    width: rgb.width(),
                    height: rgb.height(),
                    cache_hit: false,
                    rendered_raw: preview.rendered_raw,
                    embedded_raw: preview.embedded_raw,
                    rendering_note: Some(note),
                };
                // Keep one canonical fixed-quality representation. A smaller
                // byte budget fails explicitly instead of resizing a Full image.
                self.jpeg_cache
                    .put(&self.hello.device_id, key, *kind, &result)?;
                ensure!(result.jpeg.len() <= *max_bytes, "Preview exceeds max_bytes");
                check(stop)?;
                return send(writer, id, true, result.response(), result.jpeg);
            }
            _ => bail!("Unexpected control request"),
        };
        check(stop)?;
        send(writer, id, true, response, vec![])
    }
}
impl Root {
    fn refresh_manifest(&self, allow_deletions: bool) -> Result<u64> {
        let photos = self.catalog.photos()?;
        let mut records = BTreeMap::new();
        for mut p in photos {
            let Ok(relative) = p.path.strip_prefix(&self.info.path) else {
                continue;
            };
            let relative = relative.to_path_buf();
            if confined(&self.info.path, &relative).is_err() || !p.path.is_file() {
                continue;
            }
            let companion = p
                .companion_path
                .as_deref()
                .and_then(|p| p.strip_prefix(&self.info.path).ok())
                .map(Path::to_path_buf);
            if let Some(path) = &companion {
                confined(&self.info.path, path)?;
            }
            let key = PhotoKey {
                root_id: self.info.id.clone(),
                volume_id: self.info.volume_id.clone(),
                relative_path: relative.clone(),
                size_bytes: p.size_bytes,
                modified_at: p.modified_at,
                companion_path: companion.clone(),
                companion_size_bytes: p.companion_size_bytes,
                companion_modified_at: p.companion_modified_at,
            };
            p.path = relative.clone();
            p.companion_path = companion;
            p.folder = relative
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned();
            records.insert(
                relative,
                PhotoRecord {
                    key,
                    metadata: p,
                    revision: 0,
                },
            );
        }
        let mut manifest = self.manifest.lock().unwrap();
        // An unreadable directory is not evidence that its files were deleted.
        // Publish valid additions/changes, but retain unseen records until a
        // complete error-free scan can confirm removals.
        if !allow_deletions {
            for (path, record) in &manifest.records {
                records
                    .entry(path.clone())
                    .or_insert_with(|| record.clone());
            }
        }
        let mut next = Manifest {
            revision: manifest.revision,
            records: BTreeMap::new(),
            deleted: manifest.deleted.clone(),
        };
        let revision = manifest
            .revision
            .checked_add(1)
            .context("Revision overflow")?;
        let mut changed = false;
        for (path, mut record) in records {
            let same = manifest.records.get(&path).filter(|old| {
                old.key == record.key
                    && serde_json::to_vec(&old.metadata).ok()
                        == serde_json::to_vec(&record.metadata).ok()
            });
            record.revision = same.map_or(revision, |old| old.revision);
            if same.is_none() {
                changed = true;
            }
            next.deleted.remove(&path);
            next.records.insert(path, record);
        }
        for path in manifest
            .records
            .keys()
            .filter(|p| !next.records.contains_key(*p))
        {
            changed = true;
            next.deleted.insert(path.clone(), revision);
        }
        if changed {
            next.revision = revision;
            atomic_write(&self.manifest_path, &serde_json::to_vec(&next)?)?;
            *manifest = next;
        }
        Ok(manifest.revision)
    }
}
/// Canonical prefix checking plus rejecting every symlink component prevents
/// static symlink escapes. Filesystem mutation by a hostile local user is outside
/// the SSH account trust boundary (see the race limitation in NETWORK.md).
fn confined(root: &Path, relative: &Path) -> Result<PathBuf> {
    ensure!(!relative.is_absolute(), "Absolute paths are forbidden");
    ensure!(
        root.canonicalize()? == root,
        "Root moved or became a symlink"
    );
    let mut path = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                path.push(part);
                ensure!(
                    !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                    "Symlinks are forbidden"
                );
            }
            Component::CurDir => (),
            _ => bail!("Parent traversal is forbidden"),
        }
    }
    let canonical = path.canonicalize()?;
    ensure!(canonical.starts_with(root), "Path escapes root");
    Ok(canonical)
}
fn resolve_future_dir(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        !absolute
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "State path may not contain .."
    );
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .context("Invalid state directory")?
                .to_os_string(),
        );
        ancestor = ancestor.parent().context("Invalid state parent")?;
    }
    let mut result = ancestor.canonicalize()?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}
fn mtime(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", rand::random::<u128>()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn device_id(dir: &Path) -> Result<String> {
    // Hard-link publication is atomic and never replaces another session's id.
    let path = dir.join("device-id");
    if !path.exists() {
        let temporary = dir.join(format!("device-id-{}", rand::random::<u128>()));
        let id = format!("{:032x}", rand::random::<u128>());
        fs::write(&temporary, id)?;
        let result = fs::hard_link(&temporary, &path);
        let _ = fs::remove_file(temporary);
        if let Err(e) = result {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
    }
    let id = fs::read_to_string(path)?;
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid persisted device id"
    );
    Ok(id)
}
fn volume_identity(path: &Path) -> (String, String, PathBuf, PathBuf) {
    #[cfg(target_os = "linux")]
    if let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") {
        let mut matched: Option<(PathBuf, PathBuf, PathBuf)> = None;
        for line in mounts.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let Some(mount) = left.split_whitespace().nth(4) else {
                continue;
            };
            let mount = PathBuf::from(unescape(mount));
            if path.starts_with(&mount)
                && matched
                    .as_ref()
                    .is_none_or(|(p, _, _)| mount.as_os_str().len() > p.as_os_str().len())
            {
                matched = Some((
                    mount,
                    PathBuf::from(unescape(right.split_whitespace().nth(1).unwrap_or(""))),
                    PathBuf::from(unescape(left.split_whitespace().nth(3).unwrap_or("/"))),
                ));
            }
        }
        if let Some((mount, source, filesystem_root)) = matched {
            for (directory, kind) in [
                ("/dev/disk/by-uuid", "uuid"),
                ("/dev/disk/by-label", "label"),
            ] {
                if let Ok(entries) = fs::read_dir(directory) {
                    for entry in entries.flatten() {
                        if source
                            .canonicalize()
                            .ok()
                            .is_some_and(|p| entry.path().canonicalize().ok().as_ref() == Some(&p))
                        {
                            return (
                                format!("{kind}:{}", entry.file_name().to_string_lossy()),
                                kind.into(),
                                mount,
                                filesystem_root,
                            );
                        }
                    }
                }
            }
            return (
                format!("mount:{}", mount.display()),
                "mount_path".into(),
                mount,
                filesystem_root,
            );
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("diskutil")
        .args(["info", "-plist"])
        .arg(path)
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let value = |key: &str| -> Option<String> {
            let rest = text.split_once(&format!("<key>{key}</key>"))?.1;
            Some(
                rest.split_once("<string>")?
                    .1
                    .split_once("</string>")?
                    .0
                    .to_owned(),
            )
        };
        if let (Some(uuid), Some(mount)) = (value("VolumeUUID"), value("MountPoint")) {
            return (
                format!("uuid:{uuid}"),
                "uuid".into(),
                PathBuf::from(mount),
                PathBuf::from("/"),
            );
        }
    }
    (
        format!("mount:{}", path.display()),
        "mount_path".into(),
        path.to_path_buf(),
        PathBuf::from("/"),
    )
}
#[cfg(target_os = "linux")]
fn unescape(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_payload_rejects_growth_before_crossing_frame_limit() {
        let mut payload = JpegPayload(vec![0; MAX_PAYLOAD - 1]);
        payload.write_all(&[1]).unwrap();
        assert_eq!(payload.0.len(), MAX_PAYLOAD);
        assert!(
            payload
                .write_all(&[2])
                .unwrap_err()
                .to_string()
                .contains("payload limit")
        );
        assert_eq!(payload.0.len(), MAX_PAYLOAD);
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Frame::new(
                1,
                true,
                Response::Preview {
                    width: 1,
                    height: 1,
                    mime: "image/jpeg".into(),
                    rendered_raw: true,
                    embedded_raw: false,
                    rendering_note: Some("Full resolution · Remote JPEG".into()),
                },
                payload.0,
            ),
        )
        .unwrap();
        let length = u32::from_le_bytes(wire[..4].try_into().unwrap()) as usize;
        assert!(length <= MAX_FRAME);
    }
}
