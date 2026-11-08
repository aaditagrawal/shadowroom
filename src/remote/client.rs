use super::{protocol::*, server::atomic_write};
use crate::media::PreviewKind;
use anyhow::{Context, Result, bail, ensure};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum ConnectionSpec {
    Ssh {
        host: String,
        remote_command: String,
        extra_args: Vec<String>,
    },
    Local {
        program: PathBuf,
        args: Vec<String>,
    },
}
impl ConnectionSpec {
    pub fn ssh(host: impl Into<String>, remote_command: impl Into<String>) -> Self {
        Self::Ssh {
            host: host.into(),
            remote_command: remote_command.into(),
            extra_args: vec![],
        }
    }
    fn spawn(&self) -> Result<Child> {
        let mut command = match self {
            Self::Ssh {
                host,
                remote_command,
                extra_args,
            } => {
                ensure!(
                    !host.is_empty()
                        && !host.starts_with('-')
                        && !host.chars().any(char::is_whitespace),
                    "Invalid SSH host"
                );
                let mut c = Command::new("ssh");
                c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-T"])
                    .args(extra_args)
                    .args(["--", host, remote_command]);
                c
            }
            Self::Local { program, args } => {
                let mut c = Command::new(program);
                c.args(args);
                c
            }
        };
        // stderr is deliberately separate: SSH diagnostics must never corrupt framing.
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Spawn remote transport")
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct TransferStats {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub received_payload_bytes: u64,
}
#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    payload: AtomicU64,
}
struct CountingReader<R> {
    reader: R,
    counters: Arc<Counters>,
}
impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.reader.read(buffer)?;
        self.counters
            .received
            .fetch_add(bytes as u64, Ordering::Relaxed);
        Ok(bytes)
    }
}
type Delivery = std::result::Result<Frame<Response>, String>;
type Pending = Arc<Mutex<HashMap<u64, Arc<Mailbox>>>>;
/// Bytes one request may have parked between the shared reader and its
/// consumer before the reader applies transport backpressure. Credit-windowed
/// metadata streams stay far below this (FLOW_WINDOW small frames); it bounds
/// memory for legacy agents that stream without flow control. A single frame
/// is always admitted to an empty mailbox, so a maximal preview still fits.
const SPILL_BYTES: usize = 32 * 1024 * 1024;
/// Per-request delivery queue between the session reader and one consumer.
/// A slow consumer (for example a catalog write during photo sync) never fails
/// its request and never blocks other requests while its parked frames fit in
/// [`SPILL_BYTES`]; beyond that the reader waits for it until the request's own
/// deadline rather than a short fixed timeout.
struct Mailbox {
    state: Mutex<MailboxState>,
    changed: Condvar,
    deadline: Instant,
}
#[derive(Default)]
struct MailboxState {
    queue: VecDeque<(Delivery, usize)>,
    bytes: usize,
    /// The consumer is gone; later frames are discarded.
    closed: bool,
    /// A terminal error is queued (session closed or failed); a reader
    /// blocked for space gives up instead of waiting for the deadline.
    failed: bool,
    peak_bytes: usize,
}
impl Mailbox {
    fn new(deadline: Instant) -> Self {
        Self {
            state: Mutex::new(MailboxState::default()),
            changed: Condvar::new(),
            deadline,
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, MailboxState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Reader side. False when the consumer is gone or stayed full past the
    /// request deadline; the frame is then dropped.
    fn push(&self, frame: Frame<Response>, bytes: usize) -> bool {
        let mut state = self.lock();
        loop {
            if state.closed || state.failed {
                return false;
            }
            if state.bytes == 0 || state.bytes.saturating_add(bytes) <= SPILL_BYTES {
                state.bytes += bytes;
                state.peak_bytes = state.peak_bytes.max(state.bytes);
                state.queue.push_back((Ok(frame), bytes));
                self.changed.notify_all();
                return true;
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    /// Terminal failure; bypasses the byte bound so it is never lost.
    fn fail(&self, error: String) {
        let mut state = self.lock();
        if !state.closed && !state.failed {
            state.failed = true;
            state.queue.push_back((Err(error), 0));
        }
        self.changed.notify_all();
    }
    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.queue.clear();
        state.bytes = 0;
        self.changed.notify_all();
    }
    fn recv_until(&self, until: Instant) -> Option<Delivery> {
        let mut state = self.lock();
        loop {
            if let Some((delivery, bytes)) = state.queue.pop_front() {
                state.bytes -= bytes;
                self.changed.notify_all();
                return Some(delivery);
            }
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
/// Session liveness: a flag for cheap checks plus a channel that disconnects
/// once the session closes, so owners can block on closure instead of polling.
struct Closed {
    flag: AtomicBool,
    signal: Mutex<Option<Sender<()>>>,
    receiver: Receiver<()>,
}
impl Closed {
    fn new() -> Self {
        let (signal, receiver) = crossbeam_channel::bounded(0);
        Self {
            flag: AtomicBool::new(false),
            signal: Mutex::new(Some(signal)),
            receiver,
        }
    }
    fn load(&self, ordering: Ordering) -> bool {
        self.flag.load(ordering)
    }
    fn mark(&self) {
        self.flag.store(true, Ordering::Release);
        self.signal.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}
struct Session {
    hello: OnceLock<Hello>,
    writer: Mutex<Option<Sender<(u64, Request)>>>,
    pending: Pending,
    closed: Arc<Closed>,
    next_id: AtomicU64,
    child: Mutex<Option<Child>>,
    /// Set once negotiation enabled credit-windowed metadata streams.
    flow: AtomicBool,
}
impl Session {
    fn start<R: Read + Send + 'static, W: Write + Send + 'static>(
        reader: R,
        mut writer: W,
        child: Option<Child>,
        counters: Arc<Counters>,
    ) -> Arc<Self> {
        let mut reader = CountingReader {
            reader,
            counters: counters.clone(),
        };
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(Closed::new());
        let (write_tx, write_rx) = crossbeam_channel::bounded::<(u64, Request)>(MAX_IN_FLIGHT);
        let session = Arc::new(Self {
            hello: OnceLock::new(),
            writer: Mutex::new(Some(write_tx)),
            pending: pending.clone(),
            closed: closed.clone(),
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            flow: AtomicBool::new(false),
        });
        let writer_closed = closed.clone();
        let writer_pending = pending.clone();
        let writer_counters = counters.clone();
        thread::Builder::new()
            .name("shadowroom-remote-writer".into())
            .spawn(move || {
                while let Ok((id, message)) = write_rx.recv() {
                    if writer_closed.load(Ordering::Acquire) {
                        break;
                    }
                    match write_frame(&mut writer, &Frame::new(id, true, message, vec![])) {
                        Ok(bytes) => {
                            writer_counters
                                .sent
                                .fetch_add(bytes as u64, Ordering::Relaxed);
                        }
                        Err(error) => {
                            writer_closed.mark();
                            for (_, mailbox) in writer_pending.lock().unwrap().drain() {
                                mailbox.fail(format!("Remote write failed: {error:#}"));
                            }
                            break;
                        }
                    }
                }
            })
            .expect("Start remote writer");
        thread::Builder::new()
            .name("shadowroom-remote-reader".into())
            .spawn(move || {
                let error = loop {
                    let before = counters.received.load(Ordering::Relaxed);
                    match read_frame::<Response>(&mut reader) {
                        Ok(Some(frame)) => {
                            counters
                                .payload
                                .fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
                            // Only this thread reads, so the counter delta is
                            // exactly this frame's wire size.
                            let bytes = counters.received.load(Ordering::Relaxed) - before;
                            let id = frame.header.id;
                            let done = frame.header.done;
                            let mailbox = {
                                let mut requests = pending.lock().unwrap();
                                if done {
                                    requests.remove(&id)
                                } else {
                                    requests.get(&id).cloned()
                                }
                            };
                            if let Some(mailbox) = mailbox {
                                // Parks frames for a slow consumer without
                                // stalling other requests (see Mailbox).
                                if !mailbox.push(frame, bytes as usize) {
                                    pending.lock().unwrap().remove(&id);
                                    mailbox.fail(
                                        "Remote request consumer stalled past its deadline".into(),
                                    );
                                }
                            }
                        }
                        Ok(None) => break "Remote transport closed".to_owned(),
                        Err(e) => break format!("Remote protocol error: {e:#}"),
                    }
                };
                closed.mark();
                for (_, mailbox) in pending.lock().unwrap().drain() {
                    mailbox.fail(error.clone());
                }
            })
            .expect("Start remote reader");
        session
    }
    fn start_request(
        self: &Arc<Self>,
        message: Request,
        timeout: Duration,
    ) -> Result<PendingRequest> {
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "Remote is disconnected"
        );
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        ensure!(id != 0, "Request id overflow");
        let deadline = Instant::now() + timeout;
        let mailbox = Arc::new(Mailbox::new(deadline));
        let photos = matches!(message, Request::Photos { .. });
        {
            let mut pending = self.pending.lock().unwrap();
            ensure!(
                pending.len() < MAX_IN_FLIGHT,
                "Client request limit reached"
            );
            pending.insert(id, mailbox.clone());
        }
        // The agent windows exactly the Photos streams queued after FlowControl.
        // Reading the flag and queueing the request under the writer mutex
        // (which negotiation holds while setting it and queueing FlowControl)
        // makes the client's view match the agent's frame order exactly.
        let windowed =
            match self.write_with(id, message, |_| photos && self.flow.load(Ordering::Acquire)) {
                Ok(windowed) => windowed,
                Err(error) => {
                    self.pending.lock().unwrap().remove(&id);
                    return Err(error);
                }
            };
        Ok(PendingRequest {
            id,
            mailbox,
            session: self.clone(),
            deadline,
            finished: false,
            windowed,
            consumed: 0,
        })
    }
    /// Control frames that must not be dropped (credits, cancellation of a
    /// windowed stream): wait for queue space, bounded so a wedged transport
    /// cannot hang the caller (the session is then failing anyway).
    fn write_blocking(&self, message: Request) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let writer = self
            .writer
            .lock()
            .unwrap()
            .clone()
            .context("Remote is disconnected")?;
        writer
            .send_timeout((id, message), CONTROL_SEND)
            .context("Remote writer stalled or disconnected")?;
        Ok(())
    }
    /// Queue a request; `decide` runs under the writer mutex, atomically with
    /// the enqueue, and its result is returned.
    fn write_with<T>(
        &self,
        id: u64,
        message: Request,
        decide: impl FnOnce(&Sender<(u64, Request)>) -> T,
    ) -> Result<T> {
        let writer = self.writer.lock().unwrap();
        let writer = writer.as_ref().context("Remote is disconnected")?;
        let decided = decide(writer);
        writer
            .try_send((id, message))
            .context("Remote writer queue full or disconnected")?;
        Ok(decided)
    }
    fn write(&self, id: u64, message: Request) -> Result<()> {
        self.write_with(id, message, |_| ())
    }
    /// Enable credit-windowed Photos streams: set the flag and queue
    /// FlowControl under the writer mutex, so every Photos request queued
    /// after it is windowed on both sides and none before it is.
    fn enable_flow(&self) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let writer = self.writer.lock().unwrap();
        let writer = writer.as_ref().context("Remote is disconnected")?;
        writer
            .send_timeout(
                (
                    id,
                    Request::FlowControl {
                        window: FLOW_WINDOW,
                    },
                ),
                CONTROL_SEND,
            )
            .context("Remote writer stalled or disconnected")?;
        self.flow.store(true, Ordering::Release);
        Ok(())
    }
    /// Best-effort for ordinary requests. A windowed stream's agent worker
    /// waits for credit that will never come, so its Cancel must not be
    /// dropped on a full writer queue.
    fn cancel(&self, request_id: u64, windowed: bool) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if windowed {
            let _ = self.write_blocking(Request::Cancel { request_id });
        } else {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = self.write(id, Request::Cancel { request_id });
        }
    }
    fn close(&self) {
        self.closed.mark();
        self.writer.lock().unwrap().take();
        if let Some(mut child) = self.child.lock().unwrap().take() {
            // Close stdin first for graceful EOF; kill is a bounded fallback for
            // SSH/decoders that cannot finish promptly after cancellation.
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut nap = Duration::from_millis(1);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        for (_, mailbox) in self.pending.lock().unwrap().drain() {
            mailbox.fail("Remote disconnected".into());
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
/// Caller cancellation is a predicate, so it is sampled while waiting. Frames
/// still wake the receiver immediately; this only bounds cancellation latency.
const CANCEL_POLL: Duration = Duration::from_millis(50);
/// Bound on waiting for writer-queue space for a control frame that must not
/// be dropped. The writer drains continuously unless the transport is wedged.
const CONTROL_SEND: Duration = Duration::from_secs(5);
/// An outstanding multiplexed request. Dropping it sends best-effort cancellation.
/// `recv` has an overall deadline (progress does not extend it).
pub struct PendingRequest {
    pub id: u64,
    mailbox: Arc<Mailbox>,
    session: Arc<Session>,
    deadline: Instant,
    finished: bool,
    /// The agent sends this stream under a credit window.
    windowed: bool,
    consumed: u32,
}
impl PendingRequest {
    pub fn cancel(&self) {
        self.session.cancel(self.id, self.windowed);
    }
    pub fn recv(&mut self) -> Result<Frame<Response>> {
        // Nothing to poll: block until a frame, disconnection or the deadline.
        self.recv_polling(|| false, None)
    }
    pub fn recv_cancellable(&mut self, cancelled: impl Fn() -> bool) -> Result<Frame<Response>> {
        self.recv_polling(cancelled, Some(CANCEL_POLL))
    }
    fn recv_polling(
        &mut self,
        cancelled: impl Fn() -> bool,
        poll: Option<Duration>,
    ) -> Result<Frame<Response>> {
        let frame = loop {
            ensure!(!cancelled(), "Remote request cancelled");
            let now = Instant::now();
            ensure!(now < self.deadline, "Remote request timed out");
            let until = poll.map_or(self.deadline, |poll| self.deadline.min(now + poll));
            if let Some(frame) = self.mailbox.recv_until(until) {
                break frame.map_err(anyhow::Error::msg)?;
            }
        };
        if frame.header.done {
            self.finished = true;
        } else if self.windowed
            && matches!(
                frame.header.message,
                Response::Photo(_) | Response::Deleted { .. }
            )
        {
            // Return credit once half the window is consumed, so the agent
            // keeps streaming while this consumer works through the rest.
            self.consumed += 1;
            if self.consumed >= FLOW_WINDOW / 2 {
                self.session.write_blocking(Request::Credit {
                    request_id: self.id,
                    frames: self.consumed,
                })?;
                self.consumed = 0;
            }
        }
        if let Response::Error { code, message } = &frame.header.message {
            if frame.header.done {
                bail!("{code}: {message}");
            }
        }
        Ok(frame)
    }
}
impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.mailbox.close();
        if !self.finished {
            self.session.pending.lock().unwrap().remove(&self.id);
            self.session.cancel(self.id, self.windowed);
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreviewCache {
    dir: PathBuf,
    lru: Arc<super::cache::DiskLru>,
    // Hash device IDs so renderer metadata and preview paths use the same safe
    // filenames. Loaded once; UI-facing namespace lookups perform no disk I/O.
    renderers: Arc<Mutex<Renderers>>,
}
/// Renderer namespaces by hashed device id (the on-disk identity), plus a
/// memo by plain device id so per-frame lookups skip hashing. Both maps are
/// written together under one mutex, so the memo can never go stale.
#[derive(Debug, Default)]
struct Renderers {
    by_hash: HashMap<String, Option<Arc<str>>>,
    by_device: HashMap<String, Option<Arc<str>>>,
}
#[derive(Serialize, Deserialize)]
struct DeviceRenderer {
    device_id: String,
    namespace: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct CacheEntry {
    key: String,
    jpeg_hash: String,
    width: u32,
    height: u32,
    #[serde(default)]
    rendered_raw: bool,
    #[serde(default)]
    embedded_raw: bool,
    #[serde(default)]
    rendering_note: Option<String>,
}
#[derive(Clone, Debug)]
pub struct PreviewResult {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub cache_hit: bool,
    pub rendered_raw: bool,
    pub embedded_raw: bool,
    pub rendering_note: Option<String>,
}

fn validate_preview(preview: &PreviewResult) -> Result<()> {
    ensure!(
        preview.jpeg.len() <= MAX_PAYLOAD,
        "Preview too large for cache"
    );
    ensure!(
        preview.jpeg.starts_with(&[0xff, 0xd8]),
        "Preview is not JPEG"
    );
    let dimensions = image::ImageReader::new(std::io::Cursor::new(&preview.jpeg))
        .with_guessed_format()?
        .into_dimensions()?;
    ensure!(
        dimensions == (preview.width, preview.height) && preview.width > 0 && preview.height > 0,
        "Preview dimensions do not match JPEG"
    );
    ensure!(
        u64::from(preview.width) * u64::from(preview.height) <= 160_000_000,
        "Preview dimensions exceed pixel limit"
    );
    ensure!(
        !(preview.rendered_raw && preview.embedded_raw),
        "Conflicting RAW preview provenance"
    );
    ensure!(
        preview
            .rendering_note
            .as_ref()
            .is_none_or(|note| note.len() <= 16 * 1024),
        "Preview rendering note exceeds limit"
    );
    Ok(())
}

impl PreviewResult {
    pub(super) fn response(&self) -> Response {
        Response::Preview {
            width: self.width,
            height: self.height,
            mime: "image/jpeg".into(),
            rendered_raw: self.rendered_raw,
            embedded_raw: self.embedded_raw,
            rendering_note: self.rendering_note.clone(),
        }
    }
}
impl PreviewCache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let lru = Arc::new(super::cache::DiskLru::new(
            dir.clone(),
            2 * 1024 * 1024 * 1024,
        ));
        let mut renderers = HashMap::new();
        let metadata_dir = dir.join("renderers");
        let metadata_exists = match fs::symlink_metadata(&metadata_dir) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir(),
                    "Renderer metadata is not a regular directory"
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if metadata_exists {
            for entry in fs::read_dir(&metadata_dir)? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "renderer")
                {
                    continue;
                }
                let Some(hash) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    continue;
                }
                let record = crate::fsutil::read_bounded(&path, 4096)
                    .and_then(|bytes| Ok(serde_json::from_slice::<DeviceRenderer>(&bytes)?));
                let namespace = match record {
                    Ok(record)
                        if blake3::hash(record.device_id.as_bytes()).to_hex().as_str() == hash
                            && record
                                .namespace
                                .as_ref()
                                .is_none_or(|name| !name.is_empty() && name.len() <= 512) =>
                    {
                        record.namespace
                    }
                    // Corrupt identity metadata must miss, never fall back to a
                    // potentially obsolete legacy rendering for this device.
                    _ => Some("invalid-renderer-metadata".into()),
                };
                renderers.insert(hash.to_owned(), namespace.map(Arc::from));
            }
        }
        Ok(Self {
            dir,
            lru,
            renderers: Arc::new(Mutex::new(Renderers {
                by_hash: renderers,
                by_device: HashMap::new(),
            })),
        })
    }
    pub fn set_budget(&self, bytes: u64) -> Result<()> {
        self.lru.set_budget(bytes)
    }
    /// A device's renderer namespace, shared rather than copied, with no
    /// hashing after the first lookup of a device (called per tile per frame).
    pub(crate) fn rendering_namespace(&self, device_id: &str) -> Option<Arc<str>> {
        let mut renderers = self.renderers.lock().unwrap();
        if let Some(namespace) = renderers.by_device.get(device_id) {
            return namespace.clone();
        }
        let namespace = renderers
            .by_hash
            .get(blake3::hash(device_id.as_bytes()).to_hex().as_str())
            .cloned()
            .flatten();
        renderers
            .by_device
            .insert(device_id.to_owned(), namespace.clone());
        namespace
    }
    pub(crate) fn remember_renderer(&self, device_id: &str, namespace: Option<&str>) {
        let namespace: Option<Arc<str>> = namespace.map(Arc::from);
        let mut renderers = self.renderers.lock().unwrap();
        renderers.by_hash.insert(
            blake3::hash(device_id.as_bytes()).to_hex().to_string(),
            namespace.clone(),
        );
        renderers.by_device.insert(device_id.to_owned(), namespace);
    }
    fn set_renderer(&self, device_id: &str, namespace: Option<&str>) -> Result<()> {
        ensure!(
            device_id.len() <= 2048
                && namespace.is_none_or(|name| !name.is_empty() && name.len() <= 512),
            "Remote rendering identifier exceeds cache metadata bounds"
        );
        let record = DeviceRenderer {
            device_id: device_id.into(),
            namespace: namespace.map(str::to_owned),
        };
        let metadata_dir = self.dir.join("renderers");
        fs::create_dir_all(&metadata_dir)?;
        ensure!(
            fs::symlink_metadata(&metadata_dir)?.is_dir(),
            "Renderer metadata is not a regular directory"
        );
        let bytes = serde_json::to_vec(&record)?;
        ensure!(
            bytes.len() <= 4096,
            "Remote rendering metadata exceeds cache bounds"
        );
        atomic_write(
            &metadata_dir.join(format!(
                "{}.renderer",
                blake3::hash(device_id.as_bytes()).to_hex()
            )),
            &bytes,
        )?;
        self.remember_renderer(device_id, namespace);
        Ok(())
    }
    fn key(
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        namespace: Option<&str>,
    ) -> Result<String> {
        let bytes = match namespace {
            Some(namespace) => serde_json::to_vec(&(
                PROTOCOL_VERSION,
                PREVIEW_CACHE_VERSION,
                device_id,
                key,
                kind,
                namespace,
            ))?,
            // Preserve v4 sidecars from legacy agents that cannot advertise a
            // renderer. New agents always use the namespaced form above.
            None => serde_json::to_vec(&(
                PROTOCOL_VERSION,
                PREVIEW_CACHE_VERSION,
                device_id,
                key,
                kind,
            ))?,
        };
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
    pub fn get(
        &self,
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
    ) -> Result<Option<PreviewResult>> {
        let namespace = self.rendering_namespace(device_id);
        self.get_for_renderer(device_id, key, kind, max_bytes, namespace.as_deref())
    }
    pub(crate) fn get_for_renderer(
        &self,
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
        namespace: Option<&str>,
    ) -> Result<Option<PreviewResult>> {
        let hash = Self::key(device_id, key, kind, namespace)?;
        let path = self.dir.join(format!("{hash}.jpg"));
        let meta_path = self.dir.join(format!("{hash}.json"));
        let read = || -> Result<PreviewResult> {
            let meta: CacheEntry =
                serde_json::from_slice(&crate::fsutil::read_bounded(&meta_path, MAX_HEADER)?)?;
            let jpeg = crate::fsutil::read_bounded(&path, MAX_PAYLOAD)?;
            ensure!(
                meta.key == hash && meta.jpeg_hash == blake3::hash(&jpeg).to_hex().as_str(),
                "Cache checksum mismatch"
            );
            let preview = PreviewResult {
                jpeg,
                width: meta.width,
                height: meta.height,
                cache_hit: true,
                rendered_raw: meta.rendered_raw,
                embedded_raw: meta.embedded_raw,
                rendering_note: meta.rendering_note,
            };
            validate_preview(&preview)?;
            Ok(preview)
        };
        match read() {
            Ok(preview) => {
                ensure!(
                    preview.jpeg.len() <= max_bytes,
                    "Cached preview exceeds max_bytes"
                );
                self.lru.update(&[&path, &meta_path])?;
                Ok(Some(preview))
            }
            Err(_) => Ok(None), // A partial or corrupt entry is a cache miss.
        }
    }
    pub fn put(
        &self,
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        preview: &PreviewResult,
    ) -> Result<()> {
        let namespace = self.rendering_namespace(device_id);
        self.put_for_renderer(device_id, key, kind, preview, namespace.as_deref())
    }
    pub(crate) fn put_for_renderer(
        &self,
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        preview: &PreviewResult,
        namespace: Option<&str>,
    ) -> Result<()> {
        validate_preview(preview)?;
        let hash = Self::key(device_id, key, kind, namespace)?;
        let meta = CacheEntry {
            key: hash.clone(),
            jpeg_hash: blake3::hash(&preview.jpeg).to_hex().to_string(),
            width: preview.width,
            height: preview.height,
            rendered_raw: preview.rendered_raw,
            embedded_raw: preview.embedded_raw,
            rendering_note: preview.rendering_note.clone(),
        };
        atomic_write(&self.dir.join(format!("{hash}.jpg")), &preview.jpeg)?;
        atomic_write(
            &self.dir.join(format!("{hash}.json")),
            &serde_json::to_vec(&meta)?,
        )?;
        self.lru.update(&[
            &self.dir.join(format!("{hash}.jpg")),
            &self.dir.join(format!("{hash}.json")),
        ])
    }
}
struct Inner {
    spec: Option<ConnectionSpec>,
    session: Mutex<Option<Arc<Session>>>,
    reconnecting: Mutex<()>,
    hello: Mutex<Option<Hello>>,
    counters: Arc<Counters>,
    cache: PreviewCache,
    timeout: Duration,
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(session) = self.session.get_mut().unwrap().take() {
            session.close();
        }
    }
}
/// Cloneable, threaded, blocking client. Clones share a connection and cache.
/// Up to 32 requests can be outstanding; use a few caller threads for previews.
#[derive(Clone)]
pub struct RemoteHost {
    inner: Arc<Inner>,
}
impl RemoteHost {
    pub fn connect(spec: ConnectionSpec, cache_dir: PathBuf, timeout: Duration) -> Result<Self> {
        Self::connect_cached(spec, PreviewCache::new(cache_dir)?, timeout)
    }
    pub fn connect_cached(
        spec: ConnectionSpec,
        cache: PreviewCache,
        timeout: Duration,
    ) -> Result<Self> {
        let host = Self {
            inner: Arc::new(Inner {
                spec: Some(spec),
                session: Mutex::new(None),
                reconnecting: Mutex::new(()),
                hello: Mutex::new(None),
                counters: Arc::new(Counters::default()),
                cache,
                timeout,
            }),
        };
        host.reconnect(1)?;
        Ok(host)
    }
    /// Test/custom transport. The caller owns shutdown of an underlying pipe peer.
    pub fn from_io<R: Read + Send + 'static, W: Write + Send + 'static>(
        reader: R,
        writer: W,
        cache_dir: PathBuf,
        timeout: Duration,
    ) -> Result<Self> {
        let counters = Arc::new(Counters::default());
        let session = Session::start(reader, writer, None, counters.clone());
        let host = Self {
            inner: Arc::new(Inner {
                spec: None,
                session: Mutex::new(Some(session)),
                reconnecting: Mutex::new(()),
                hello: Mutex::new(None),
                counters,
                cache: PreviewCache::new(cache_dir)?,
                timeout,
            }),
        };
        host.negotiate()?;
        Ok(host)
    }
    /// Reopen a disconnected child with bounded exponential backoff. Operations
    /// are never replayed implicitly; the caller decides when to retry an index.
    pub fn reconnect(&self, attempts: usize) -> Result<()> {
        let _reconnecting = self.inner.reconnecting.lock().unwrap();
        let spec = self
            .inner
            .spec
            .as_ref()
            .context("Custom transports cannot reconnect")?;
        self.disconnect();
        let mut last = anyhow::anyhow!("No connection attempts requested");
        for attempt in 0..attempts.min(6) {
            if attempt > 0 {
                thread::sleep(Duration::from_millis((100u64 << (attempt - 1)).min(2000)));
            }
            let result = (|| -> Result<()> {
                let mut child = spec.spawn()?;
                let reader = child.stdout.take().context("Missing child stdout")?;
                let writer = child.stdin.take().context("Missing child stdin")?;
                let session =
                    Session::start(reader, writer, Some(child), self.inner.counters.clone());
                *self.inner.session.lock().unwrap() = Some(session);
                self.negotiate()
            })();
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last = error;
                    self.disconnect();
                }
            }
        }
        Err(last)
    }
    fn negotiate(&self) -> Result<()> {
        let session = self
            .inner
            .session
            .lock()
            .unwrap()
            .clone()
            .context("Remote is disconnected")?;
        let mut request = session.start_request(
            Request::Hello {
                protocol_version: PROTOCOL_VERSION,
                app_version: env!("CARGO_PKG_VERSION").into(),
            },
            self.inner.timeout,
        )?;
        let response = request.recv()?;
        ensure!(response.header.done, "Unexpected streaming hello response");
        let Response::Hello(hello) = response.header.message else {
            bail!("Unexpected hello response");
        };
        ensure!(
            hello.protocol_version == PROTOCOL_VERSION,
            "Protocol version mismatch"
        );
        self.inner
            .cache
            .set_renderer(&hello.device_id, hello.rendering_namespace())?;
        if hello
            .capabilities
            .iter()
            .any(|c| c == FLOW_CONTROL_CAPABILITY)
        {
            session.enable_flow()?;
        }
        session
            .hello
            .set(hello.clone())
            .map_err(|_| anyhow::anyhow!("Hello already negotiated"))?;
        *self.inner.hello.lock().unwrap() = Some(hello);
        Ok(())
    }
    pub fn hello(&self) -> Hello {
        self.inner
            .hello
            .lock()
            .unwrap()
            .clone()
            .expect("RemoteHost negotiated")
    }
    pub fn is_connected(&self) -> bool {
        self.inner
            .session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !s.closed.load(Ordering::Acquire))
    }
    /// A receiver that becomes disconnected (ready) once the current session
    /// closes, for blocking in `select!` instead of polling `is_connected`.
    /// Without a session it is ready immediately. A later reconnect starts a new
    /// session; call this again after reconnecting.
    pub fn closed_signal(&self) -> Receiver<()> {
        match self.inner.session.lock().unwrap().as_ref() {
            Some(session) => session.closed.receiver.clone(),
            None => crossbeam_channel::bounded(0).1,
        }
    }
    pub fn disconnect(&self) {
        if let Some(session) = self.inner.session.lock().unwrap().take() {
            session.close();
        }
    }
    pub fn set_cache_budget(&self, bytes: u64) -> Result<()> {
        self.inner.cache.set_budget(bytes)
    }
    pub fn stats(&self) -> TransferStats {
        let c = &self.inner.counters;
        TransferStats {
            sent_bytes: c.sent.load(Ordering::Relaxed),
            received_bytes: c.received.load(Ordering::Relaxed),
            received_payload_bytes: c.payload.load(Ordering::Relaxed),
        }
    }
    pub fn request(&self, request: Request) -> Result<PendingRequest> {
        self.request_with_timeout(request, self.inner.timeout)
    }
    pub fn request_with_timeout(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<PendingRequest> {
        self.inner
            .session
            .lock()
            .unwrap()
            .as_ref()
            .context("Remote is disconnected")?
            .start_request(request, timeout)
    }
    fn one(&self, request: Request) -> Result<Frame<Response>> {
        let mut pending = self.request(request)?;
        let frame = pending.recv()?;
        ensure!(frame.header.done, "Unexpected streaming response");
        Ok(frame)
    }
    pub fn list_roots(&self) -> Result<Vec<RemoteRoot>> {
        match self.one(Request::ListRoots)?.header.message {
            Response::Roots { roots } => Ok(roots),
            _ => bail!("Unexpected roots response"),
        }
    }
    pub fn ping(&self) -> Result<()> {
        ensure!(
            matches!(self.one(Request::Ping)?.header.message, Response::Pong),
            "Unexpected ping response"
        );
        Ok(())
    }
    pub fn index(&self, root: &str, mut progress: impl FnMut(&Progress)) -> Result<IndexSummary> {
        let mut pending = self.request(Request::Index { root: root.into() })?;
        loop {
            match pending.recv()?.header.message {
                Response::Progress(p) => progress(&p),
                Response::Indexed(summary) => return Ok(summary),
                Response::Error { code, message } if code == "index_warning" => {
                    let _ = message;
                }
                _ => bail!("Unexpected index response"),
            }
        }
    }
    pub fn photos(&self, root: &str, since_revision: u64) -> Result<PhotoSync> {
        let mut sync = PhotoSync::default();
        self.photos_stream(
            root,
            since_revision,
            |record| sync.records.push(record),
            |path| sync.deleted.push(path),
        )
        .map(|revision| {
            sync.revision = revision;
            sync
        })
    }
    pub fn photos_stream(
        &self,
        root: &str,
        since_revision: u64,
        mut record: impl FnMut(PhotoRecord),
        mut deleted: impl FnMut(PathBuf),
    ) -> Result<u64> {
        let mut pending = self.request(Request::Photos {
            root: root.into(),
            since_revision,
        })?;
        loop {
            match pending.recv()?.header.message {
                Response::Photo(p) => record(p),
                Response::Deleted { relative_path } => deleted(relative_path),
                Response::Synced { revision, .. } => return Ok(revision),
                _ => bail!("Unexpected photos response"),
            }
        }
    }
    pub fn preview(
        &self,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
    ) -> Result<PreviewResult> {
        self.preview_cancellable(key, kind, max_bytes, || false)
    }
    pub fn preview_cancellable(
        &self,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreviewResult> {
        let namespace = self.hello().rendering_namespace().map(str::to_owned);
        self.preview_for_renderer(key, kind, max_bytes, namespace.as_deref(), cancelled)
    }
    pub(crate) fn preview_for_renderer(
        &self,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
        namespace: Option<&str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreviewResult> {
        ensure!(
            max_bytes > 0 && max_bytes <= MAX_PAYLOAD,
            "Preview byte budget outside bounds"
        );
        let session = self.inner.session.lock().unwrap().clone();
        let hello = match &session {
            Some(session) => session
                .hello
                .get()
                .cloned()
                .context("Remote negotiation is incomplete")?,
            None => self.hello(),
        };
        ensure!(
            hello.rendering_namespace() == namespace,
            "Remote rendering configuration changed; request the preview again"
        );
        let device = hello.device_id;
        if let Some(preview) = self
            .inner
            .cache
            .get_for_renderer(&device, key, kind, max_bytes, namespace)?
        {
            return Ok(preview);
        }
        let mut pending = session.context("Remote is disconnected")?.start_request(
            Request::Preview {
                key: key.clone(),
                kind,
                max_bytes,
            },
            self.inner.timeout,
        )?;
        let frame = pending.recv_cancellable(cancelled)?;
        ensure!(frame.header.done, "Unexpected streaming preview response");
        let Response::Preview {
            width,
            height,
            mime,
            rendered_raw,
            embedded_raw,
            rendering_note,
        } = frame.header.message
        else {
            bail!("Unexpected preview response");
        };
        ensure!(
            mime == "image/jpeg" && frame.payload.len() <= max_bytes,
            "Invalid preview response"
        );
        ensure!(
            frame.payload.starts_with(&[0xff, 0xd8]),
            "Preview is not JPEG"
        );
        // Older agents explicitly promised sensor-only Full. Keep validating
        // that contract; v4 agents advertise JPEG-first inspection instead.
        if kind == PreviewKind::Full
            && crate::indexer::is_raw(&key.relative_path)
            && hello
                .capabilities
                .iter()
                .any(|c| c == "preview_provenance_v2")
        {
            ensure!(
                rendered_raw && !embedded_raw,
                "Full RAW response is not sensor rendered"
            );
        }
        let preview = PreviewResult {
            jpeg: frame.payload,
            width,
            height,
            cache_hit: false,
            rendered_raw,
            embedded_raw,
            rendering_note: rendering_note.or_else(|| {
                Some(format!(
                    "Remote JPEG · {width} × {height} pixels{}",
                    if kind == PreviewKind::Full {
                        " · legacy agent: Full source provenance unverified"
                    } else {
                        ""
                    }
                ))
            }),
        };
        // put validates JPEG dimensions, pixel count and provenance at the
        // shared network/cache boundary before publishing either file.
        // Pin publication to the session that produced these pixels, even if a
        // reconnect negotiated another renderer while this request completed.
        self.inner
            .cache
            .put_for_renderer(&device, key, kind, &preview, namespace)?;
        Ok(preview)
    }
    pub fn read_range(&self, root: &str, path: &Path, offset: u64, len: u32) -> Result<Vec<u8>> {
        ensure!(len <= MAX_RANGE, "Range exceeds limit");
        let frame = self.one(Request::ReadRange {
            root: root.into(),
            path: path.into(),
            offset,
            len,
        })?;
        ensure!(
            matches!(frame.header.message, Response::Range) && frame.payload.len() <= len as usize,
            "Invalid range response"
        );
        Ok(frame.payload)
    }
    pub fn stat(&self, root: &str, path: &Path) -> Result<Response> {
        let response = self
            .one(Request::Stat {
                root: root.into(),
                path: path.into(),
            })?
            .header
            .message;
        ensure!(
            matches!(response, Response::Stat { .. }),
            "Unexpected stat response"
        );
        Ok(response)
    }
    /// Reopen saved previews in a new offline session; saved records/roots are
    /// deliberately owned by the GUI catalog, rather than hidden in this cache.
    pub fn offline_preview(
        cache_dir: PathBuf,
        device_id: &str,
        key: &PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
    ) -> Result<Option<PreviewResult>> {
        PreviewCache::new(cache_dir)?.get(device_id, key, kind, max_bytes)
    }
}

#[cfg(test)]
mod mailbox_tests {
    use super::*;

    fn frame() -> Frame<Response> {
        Frame::new(1, false, Response::Pong, vec![])
    }

    #[test]
    fn spill_is_bounded_backpressures_until_drained_and_fails_only_at_deadline() {
        let mailbox = Arc::new(Mailbox::new(Instant::now() + Duration::from_secs(30)));
        // A single oversized frame is always admitted to an empty mailbox.
        assert!(mailbox.push(frame(), SPILL_BYTES + 1));
        let reader = {
            let mailbox = mailbox.clone();
            thread::spawn(move || mailbox.push(frame(), 10))
        };
        thread::sleep(Duration::from_millis(100));
        assert!(!reader.is_finished(), "reader must wait for space");
        assert!(mailbox.recv_until(Instant::now()).unwrap().is_ok());
        assert!(reader.join().unwrap());
        assert!(mailbox.lock().peak_bytes <= SPILL_BYTES + 1);
        // A consumer that stays full past the request deadline fails it.
        let expired = Mailbox::new(Instant::now() + Duration::from_millis(20));
        assert!(expired.push(frame(), SPILL_BYTES));
        assert!(!expired.push(frame(), 1));
        // Terminal errors bypass the bound; a closed mailbox drops frames.
        expired.fail("gone".into());
        assert!(expired.recv_until(Instant::now()).unwrap().is_ok());
        assert!(expired.recv_until(Instant::now()).unwrap().is_err());
        expired.close();
        assert!(!expired.push(frame(), 1));
        assert!(expired.recv_until(Instant::now()).is_none());
    }

    #[test]
    fn session_failure_wakes_a_reader_blocked_on_a_full_mailbox() {
        let mailbox = Arc::new(Mailbox::new(Instant::now() + Duration::from_secs(3600)));
        assert!(mailbox.push(frame(), SPILL_BYTES));
        let reader = {
            let mailbox = mailbox.clone();
            thread::spawn(move || mailbox.push(frame(), 10))
        };
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        mailbox.fail("Remote disconnected".into());
        assert!(!reader.join().unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
        // The consumer still sees the parked frame, then the terminal error.
        assert!(mailbox.recv_until(Instant::now()).unwrap().is_ok());
        assert!(mailbox.recv_until(Instant::now()).unwrap().is_err());
    }

    /// Records written frames; writes block while the gate is closed.
    #[derive(Clone)]
    struct GatedWriter {
        open: Arc<(Mutex<bool>, Condvar)>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }
    impl GatedWriter {
        fn new(open: bool) -> Self {
            Self {
                open: Arc::new((Mutex::new(open), Condvar::new())),
                bytes: Arc::default(),
            }
        }
        fn open(&self) {
            *self.open.0.lock().unwrap() = true;
            self.open.1.notify_all();
        }
        fn frames(&self) -> Vec<Frame<Request>> {
            let bytes = self.bytes.lock().unwrap().clone();
            let mut reader = &bytes[..];
            let mut frames = Vec::new();
            // Only complete frames: the writer may be mid-frame.
            while reader.len() >= 4
                && reader.len() >= 4 + u32::from_le_bytes(reader[..4].try_into().unwrap()) as usize
            {
                frames.push(read_frame::<Request>(&mut reader).unwrap().unwrap());
            }
            frames
        }
    }
    impl Write for GatedWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let mut open = self.open.0.lock().unwrap();
            while !*open {
                open = self.open.1.wait(open).unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    /// A reader that never yields a frame until the test drops its peer.
    fn idle_reader() -> (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ) {
        std::os::unix::net::UnixStream::pair().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn windowed_cancel_waits_for_queue_space_instead_of_being_dropped() {
        let writer = GatedWriter::new(false);
        let (reader, peer) = idle_reader();
        let session = Session::start(reader, writer.clone(), None, Arc::default());
        // The writer thread is parked on the gate; fill the queue behind it.
        let mut queued = 0;
        for _ in 0..2 {
            while session.write(1000 + queued, Request::Ping).is_ok() {
                queued += 1;
            }
            // Let the writer thread take its first frame, then refill the slot.
            thread::sleep(Duration::from_millis(20));
        }
        while session.write(1000 + queued, Request::Ping).is_ok() {
            queued += 1;
        }
        assert!(queued >= MAX_IN_FLIGHT as u64);
        let cancel = {
            let session = session.clone();
            thread::spawn(move || session.cancel(7, true))
        };
        thread::sleep(Duration::from_millis(50));
        assert!(!cancel.is_finished(), "windowed Cancel waits for space");
        writer.open();
        cancel.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !writer
            .frames()
            .iter()
            .any(|f| matches!(f.header.message, Request::Cancel { request_id: 7 }))
        {
            assert!(Instant::now() < deadline, "Cancel was dropped");
            thread::sleep(Duration::from_millis(5));
        }
        session.close();
        drop(peer);
    }

    #[cfg(unix)]
    #[test]
    fn photos_requests_racing_flow_negotiation_are_windowed_exactly_after_it() {
        for _ in 0..20 {
            let writer = GatedWriter::new(true);
            let (reader, peer) = idle_reader();
            let session = Session::start(reader, writer.clone(), None, Arc::default());
            let requests = thread::scope(|scope| {
                let workers: Vec<_> = (0..4)
                    .map(|_| {
                        let session = &session;
                        scope.spawn(move || {
                            (0..6)
                                .map(|_| {
                                    session
                                        .start_request(
                                            Request::Photos {
                                                root: "r".into(),
                                                since_revision: 0,
                                            },
                                            Duration::from_secs(30),
                                        )
                                        .unwrap()
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                session.enable_flow().unwrap();
                workers
                    .into_iter()
                    .flat_map(|w| w.join().unwrap())
                    .collect::<Vec<_>>()
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            let frames = loop {
                let frames = writer.frames();
                if frames.len() > requests.len() {
                    break frames;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            };
            let flow_at = frames
                .iter()
                .position(|f| matches!(f.header.message, Request::FlowControl { .. }))
                .unwrap();
            for request in &requests {
                let at = frames
                    .iter()
                    .position(|f| f.header.id == request.id)
                    .unwrap();
                assert_eq!(request.windowed, at > flow_at, "request {}", request.id);
            }
            drop(requests);
            session.close();
            drop(peer);
        }
    }
}

#[cfg(test)]
mod rendering_tests {
    use super::*;

    fn key() -> PhotoKey {
        PhotoKey {
            root_id: "root".into(),
            volume_id: "volume".into(),
            relative_path: "photo.jpg".into(),
            size_bytes: 123,
            modified_at: 456,
            companion_path: None,
            companion_size_bytes: 0,
            companion_modified_at: 0,
        }
    }
    fn preview(value: u8) -> PreviewResult {
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&image::RgbImage::from_pixel(2, 2, image::Rgb([value; 3])))
            .unwrap();
        PreviewResult {
            jpeg,
            width: 2,
            height: 2,
            cache_hit: false,
            rendered_raw: false,
            embedded_raw: false,
            rendering_note: Some(format!("renderer {value}")),
        }
    }

    #[test]
    fn renderer_switch_invalidates_cache_and_offline_restart_keeps_latest_identity() {
        let temp = tempfile::tempdir().unwrap();
        let cache = PreviewCache::new(temp.path().into()).unwrap();
        let key = key();
        let first = preview(25);
        let second = preview(225);
        cache.set_renderer("device", Some("renderer-a")).unwrap();
        cache.put("device", &key, PreviewKind::Fit, &first).unwrap();
        cache.set_renderer("device", Some("renderer-b")).unwrap();
        assert!(
            cache
                .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        cache
            .put("device", &key, PreviewKind::Fit, &second)
            .unwrap();
        // A late completion from the previous session cannot change either the
        // active namespace or the bytes now selected for this same photo.
        cache
            .put_for_renderer("device", &key, PreviewKind::Fit, &first, Some("renderer-a"))
            .unwrap();
        assert_eq!(
            cache
                .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .unwrap()
                .jpeg,
            second.jpeg
        );
        drop(cache);
        let offline = RemoteHost::offline_preview(
            temp.path().into(),
            "device",
            &key,
            PreviewKind::Fit,
            MAX_PAYLOAD,
        )
        .unwrap()
        .unwrap();
        assert_eq!(offline.jpeg, second.jpeg);
        let reopened = PreviewCache::new(temp.path().into()).unwrap();
        reopened.set_budget(0).unwrap();
        drop(reopened);
        let reopened = PreviewCache::new(temp.path().into()).unwrap();
        assert_eq!(
            reopened.rendering_namespace("device").as_deref(),
            Some("renderer-b")
        );
        // LRU eviction removes representations, never the identity needed to
        // select later offline entries after another app restart.
        assert!(
            reopened
                .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        reopened
            .put_for_renderer("device", &key, PreviewKind::Fit, &first, Some("renderer-a"))
            .unwrap();
        assert!(
            reopened
                .get("device", &key, PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        reopened.set_renderer("device", None).unwrap();
        reopened
            .put("device", &key, PreviewKind::Fit, &first)
            .unwrap();
        drop(reopened);
        assert_eq!(
            RemoteHost::offline_preview(
                temp.path().into(),
                "device",
                &key,
                PreviewKind::Fit,
                MAX_PAYLOAD
            )
            .unwrap()
            .unwrap()
            .jpeg,
            first.jpeg
        );
    }

    #[test]
    fn memoized_renderer_lookup_follows_changes_made_through_other_handles() {
        let temp = tempfile::tempdir().unwrap();
        let cache = PreviewCache::new(temp.path().into()).unwrap();
        let other = cache.clone();
        // Memoize both a miss and a hit before changing them elsewhere.
        assert_eq!(cache.rendering_namespace("device"), None);
        other.remember_renderer("device", Some("renderer-a"));
        assert_eq!(
            cache.rendering_namespace("device").as_deref(),
            Some("renderer-a")
        );
        other.set_renderer("device", Some("renderer-b")).unwrap();
        assert_eq!(
            cache.rendering_namespace("device").as_deref(),
            Some("renderer-b")
        );
        other.remember_renderer("device", None);
        assert_eq!(cache.rendering_namespace("device"), None);
        // A restart reads the hashed on-disk identity without the memo.
        assert_eq!(
            PreviewCache::new(temp.path().into())
                .unwrap()
                .rendering_namespace("device")
                .as_deref(),
            Some("renderer-b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn renderer_metadata_rejects_symlinks_and_oversized_records_without_legacy_fallback() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cache");
        let cache = PreviewCache::new(root.clone()).unwrap();
        cache
            .put("device", &key(), PreviewKind::Fit, &preview(25))
            .unwrap();
        cache.set_renderer("device", Some("renderer-a")).unwrap();
        let path = root
            .join("renderers")
            .join(format!("{}.renderer", blake3::hash(b"device").to_hex()));
        fs::write(&path, vec![b'x'; 4097]).unwrap();
        let reopened = PreviewCache::new(root.clone()).unwrap();
        assert!(
            reopened
                .get("device", &key(), PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        let external = temp.path().join("external");
        let contents = serde_json::to_vec(&DeviceRenderer {
            device_id: "device".into(),
            namespace: Some("outside".into()),
        })
        .unwrap();
        fs::write(&external, &contents).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&external, &path).unwrap();
        let reopened = PreviewCache::new(root.clone()).unwrap();
        assert_eq!(
            reopened.rendering_namespace("device").as_deref(),
            Some("invalid-renderer-metadata")
        );
        reopened.set_renderer("device", Some("renderer-b")).unwrap();
        assert_eq!(fs::read(&external).unwrap(), contents);
        let other_root = temp.path().join("other-cache");
        let other = PreviewCache::new(other_root.clone()).unwrap();
        symlink(root.join("renderers"), other_root.join("renderers")).unwrap();
        assert!(PreviewCache::new(other_root).is_err());
        assert!(other.set_renderer("device", Some("renderer-c")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn renegotiating_same_device_renderer_fetches_new_pixels_instead_of_cached_ones() {
        use std::os::unix::net::UnixStream;
        fn peer(namespace: Option<&str>, value: u8) -> (UnixStream, thread::JoinHandle<usize>) {
            let (client, mut server) = UnixStream::pair().unwrap();
            let capability = namespace.map(|name| format!("{RENDERING_CAPABILITY_PREFIX}{name}"));
            let worker = thread::spawn(move || {
                let mut previews = 0;
                while let Some(frame) = read_frame::<Request>(&mut server).unwrap() {
                    let (response, payload) = match frame.header.message {
                        Request::Hello { .. } => (
                            Response::Hello(Hello {
                                protocol_version: PROTOCOL_VERSION,
                                app_version: "test".into(),
                                device_id: "same-device".into(),
                                hostname: "test".into(),
                                os: "linux".into(),
                                arch: "x86_64".into(),
                                capabilities: capability.clone().into_iter().collect(),
                            }),
                            vec![],
                        ),
                        Request::Preview { .. } => {
                            previews += 1;
                            let image = preview(value);
                            (image.response(), image.jpeg)
                        }
                        Request::Cancel { .. } => continue,
                        other => panic!("unexpected request {other:?}"),
                    };
                    write_frame(
                        &mut server,
                        &Frame::new(frame.header.id, true, response, payload),
                    )
                    .unwrap();
                }
                previews
            });
            (client, worker)
        }
        let temp = tempfile::tempdir().unwrap();
        let (client, first_worker) = peer(Some("renderer-a"), 25);
        let first_control = client.try_clone().unwrap();
        let host = RemoteHost::from_io(
            client.try_clone().unwrap(),
            client,
            temp.path().into(),
            Duration::from_secs(5),
        )
        .unwrap();
        let first = host.preview(&key(), PreviewKind::Fit, MAX_PAYLOAD).unwrap();
        assert!(!first.cache_hit);
        host.disconnect();
        first_control.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(first_worker.join().unwrap(), 1);
        let (client, second_worker) = peer(Some("renderer-b"), 225);
        let second_control = client.try_clone().unwrap();
        let session = Session::start(
            client.try_clone().unwrap(),
            client,
            None,
            host.inner.counters.clone(),
        );
        *host.inner.session.lock().unwrap() = Some(session);
        host.negotiate().unwrap();
        let second = host.preview(&key(), PreviewKind::Fit, MAX_PAYLOAD).unwrap();
        assert!(!second.cache_hit);
        assert_ne!(first.jpeg, second.jpeg);
        assert!(
            host.preview(&key(), PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .cache_hit
        );
        host.disconnect();
        second_control.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(second_worker.join().unwrap(), 1);
        assert_eq!(
            RemoteHost::offline_preview(
                temp.path().into(),
                "same-device",
                &key(),
                PreviewKind::Fit,
                MAX_PAYLOAD
            )
            .unwrap()
            .unwrap()
            .jpeg,
            second.jpeg
        );
    }
}
