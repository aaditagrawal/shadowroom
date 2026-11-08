//! Persisted host configuration and one lifecycle owner per remote connection.
use crate::{
    catalog::Catalog,
    media::RemotePreviewProvider,
    remote::{
        ConnectionSpec, RemoteHost,
        protocol::{Hello, PhotoRecord, Progress, RemoteRoot, Request, Response},
    },
    service::IoEvent,
};
use anyhow::{Context, Result, bail, ensure};
use crossbeam_channel::{Receiver, Sender, bounded};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn default_command() -> String {
    "shadowroom-remote serve --stdio".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostConfig {
    pub id: String,
    pub display_name: String,
    pub ssh_alias: String,
    #[serde(default = "default_command")]
    pub remote_command: String,
    #[serde(default)]
    pub extra_ssh_args: Vec<String>,
    /// Explicit local child transport for real-protocol integration tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_program: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_args: Vec<String>,
}
impl HostConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.trim().is_empty() && self.id.len() <= 128,
            "Host id must contain 1–128 characters"
        );
        ensure!(
            !self.display_name.trim().is_empty(),
            "Host display name is required"
        );
        if self.local_program.is_none() {
            ensure!(
                !self.ssh_alias.is_empty()
                    && !self.ssh_alias.starts_with('-')
                    && !self.ssh_alias.chars().any(char::is_whitespace),
                "Enter an SSH alias or user@host, without spaces"
            );
            ensure!(
                !self.remote_command.trim().is_empty(),
                "Remote command is required"
            );
        }
        Ok(())
    }
    fn spec(&self) -> ConnectionSpec {
        match &self.local_program {
            Some(program) => ConnectionSpec::Local {
                program: program.clone(),
                args: self.local_args.clone(),
            },
            None => ConnectionSpec::Ssh {
                host: self.ssh_alias.clone(),
                remote_command: self.remote_command.clone(),
                extra_args: self.extra_ssh_args.clone(),
            },
        }
    }
}
/// Wire/persisted/MCP spelling of a host's lifecycle state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostState {
    #[default]
    Offline,
    Connecting,
    Connected,
    Indexing,
    Error,
}
impl HostState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Indexing => "indexing",
            Self::Error => "error",
        }
    }
    /// Work in progress: connecting or indexing.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Connecting | Self::Indexing)
    }
}
impl std::fmt::Display for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
/// Host lifecycle. A failure message exists only in the states that can carry
/// one, so a snapshot can never claim "connected" or "indexing" with an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStatus {
    /// Not connected; `error` explains why when the host went offline on failure.
    Offline {
        error: Option<String>,
    },
    /// Connecting; `retrying` is the failure from the previous attempt, if any.
    Connecting {
        retrying: Option<String>,
    },
    Connected,
    Indexing,
    Error(String),
}
impl Default for HostStatus {
    fn default() -> Self {
        Self::Offline { error: None }
    }
}
impl HostStatus {
    pub fn state(&self) -> HostState {
        match self {
            Self::Offline { .. } => HostState::Offline,
            Self::Connecting { .. } => HostState::Connecting,
            Self::Connected => HostState::Connected,
            Self::Indexing => HostState::Indexing,
            Self::Error(_) => HostState::Error,
        }
    }
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Offline { error } | Self::Connecting { retrying: error } => error.as_deref(),
            Self::Error(error) => Some(error),
            Self::Connected | Self::Indexing => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(into = "HostSnapshotWire", from = "HostSnapshotWire")]
pub struct HostSnapshot {
    pub config: HostConfig,
    pub hello: Option<Hello>,
    pub roots: Vec<RemoteRoot>,
    pub status: HostStatus,
    pub progress: Option<Progress>,
    pub revisions: BTreeMap<String, u64>,
}
/// Field order and spelling are the persisted hosts.json and MCP format.
#[derive(Serialize, Deserialize)]
struct HostSnapshotWire {
    config: HostConfig,
    hello: Option<Hello>,
    roots: Vec<RemoteRoot>,
    state: HostState,
    error: Option<String>,
    progress: Option<Progress>,
    #[serde(default)]
    revisions: BTreeMap<String, u64>,
}
impl From<HostSnapshot> for HostSnapshotWire {
    fn from(s: HostSnapshot) -> Self {
        Self {
            state: s.status.state(),
            error: s.status.error().map(str::to_owned),
            config: s.config,
            hello: s.hello,
            roots: s.roots,
            progress: s.progress,
            revisions: s.revisions,
        }
    }
}
impl From<HostSnapshotWire> for HostSnapshot {
    fn from(w: HostSnapshotWire) -> Self {
        let status = match w.state {
            HostState::Offline => HostStatus::Offline { error: w.error },
            HostState::Connecting => HostStatus::Connecting { retrying: w.error },
            HostState::Connected => HostStatus::Connected,
            HostState::Indexing => HostStatus::Indexing,
            HostState::Error => HostStatus::Error(
                w.error
                    .unwrap_or_else(|| "Unknown remote host error".into()),
            ),
        };
        Self {
            config: w.config,
            hello: w.hello,
            roots: w.roots,
            status,
            progress: w.progress,
            revisions: w.revisions,
        }
    }
}
impl HostSnapshot {
    fn new(config: HostConfig) -> Self {
        Self {
            config,
            hello: None,
            roots: vec![],
            status: HostStatus::default(),
            progress: None,
            revisions: BTreeMap::new(),
        }
    }
    pub fn state(&self) -> HostState {
        self.status.state()
    }
    pub fn error(&self) -> Option<&str> {
        self.status.error()
    }
    /// Offline, keeping the last failure (if any) for diagnostics.
    fn go_offline(&mut self) {
        let error = self.error().map(str::to_owned);
        self.status = HostStatus::Offline { error };
    }
    fn begin_connecting(&mut self) {
        let retrying = self.error().map(str::to_owned);
        self.status = HostStatus::Connecting { retrying };
    }
}
struct Store {
    path: PathBuf,
    hosts: Mutex<BTreeMap<String, HostSnapshot>>,
}
impl Store {
    fn open(path: PathBuf) -> Result<Self> {
        let mut hosts: BTreeMap<String, HostSnapshot> = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("Read hosts.json (file left unchanged)")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        for snapshot in hosts.values_mut() {
            snapshot.go_offline();
            snapshot.progress = None;
        }
        Ok(Self {
            path,
            hosts: Mutex::new(hosts),
        })
    }
    fn put(&self, snapshot: HostSnapshot) -> Result<()> {
        let mut hosts = self.hosts.lock().unwrap();
        let mut next = hosts.clone();
        next.insert(snapshot.config.id.clone(), snapshot);
        crate::settings::atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        *hosts = next;
        Ok(())
    }
    fn remove(&self, id: &str) -> Result<()> {
        let mut hosts = self.hosts.lock().unwrap();
        let mut next = hosts.clone();
        next.remove(id);
        crate::settings::atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        *hosts = next;
        Ok(())
    }
}
#[derive(Debug)]
pub enum RemoteCommand {
    Add(HostConfig),
    Remove(String),
    Connect(String),
    Disconnect(String),
    Index { host: String, root: String },
    Sync { host: String, root: String },
    SyncFull { host: String, root: String },
    Cancel(String),
}
enum Command {
    Connect,
    Disconnect,
    Index(String, u64),
    Sync(String, bool, u64),
    Stop,
}
struct Actor {
    commands: Sender<Command>,
    cancel: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Actor {
    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .context("Remote host command queue is full or stopped")
    }
    fn cancel(&self) {
        self.cancel.fetch_add(1, Ordering::AcqRel);
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel();
        let _ = self.commands.try_send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct RemoteService {
    actors: BTreeMap<String, Actor>,
    store: Arc<Store>,
    catalog: Catalog,
    provider: Arc<RemotePreviewProvider>,
    events: Sender<IoEvent>,
    changed: Sender<()>,
}
impl RemoteService {
    pub fn new(
        data: PathBuf,
        catalog: Catalog,
        events: Sender<IoEvent>,
        changed: Sender<()>,
        cache_bytes: u64,
    ) -> Result<Self> {
        let provider = Arc::new(RemotePreviewProvider::new(data.join("remote-previews"))?);
        provider.set_budget(cache_bytes)?;
        let store = Arc::new(Store::open(data.join("hosts.json"))?);
        let mut service = Self {
            actors: BTreeMap::new(),
            store,
            catalog,
            provider,
            events,
            changed,
        };
        let _ = service
            .events
            .send(IoEvent::RemoteProvider(service.provider.clone()));
        let hosts: Vec<_> = service
            .store
            .hosts
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for host in hosts {
            service.spawn(host);
        }
        Ok(service)
    }
    pub fn set_cache_budget(&self, bytes: u64) -> Result<()> {
        self.provider.set_budget(bytes)
    }
    pub fn command(&mut self, command: RemoteCommand) -> Result<()> {
        match command {
            RemoteCommand::Add(config) => {
                config.validate()?;
                ensure!(
                    !self.actors.contains_key(&config.id),
                    "Host id already exists; remove it before replacing configuration"
                );
                let snapshot = HostSnapshot::new(config);
                self.store.put(snapshot.clone())?;
                let id = snapshot.config.id.clone();
                self.spawn(snapshot);
                self.actors[&id].send(Command::Connect)?;
            }
            RemoteCommand::Remove(id) => {
                let mut actor = self.actors.remove(&id).context("Unknown remote host")?;
                actor.stop();
                self.store.remove(&id)?;
                let _ = self.events.send(IoEvent::RemoteRemoved(id));
            }
            RemoteCommand::Connect(id) => self.actor(&id)?.send(Command::Connect)?,
            RemoteCommand::Disconnect(id) => {
                let actor = self.actor(&id)?;
                actor.cancel();
                actor.send(Command::Disconnect)?;
            }
            RemoteCommand::Cancel(id) => self.actor(&id)?.cancel(),
            RemoteCommand::SyncFull { host, root } => {
                let actor = self.actor(&host)?;
                actor.send(Command::Sync(
                    root,
                    true,
                    actor.cancel.load(Ordering::Acquire),
                ))?
            }
            RemoteCommand::Index { host, root } => {
                let actor = self.actor(&host)?;
                actor.send(Command::Index(root, actor.cancel.load(Ordering::Acquire)))?;
            }
            RemoteCommand::Sync { host, root } => {
                let actor = self.actor(&host)?;
                actor.send(Command::Sync(
                    root,
                    false,
                    actor.cancel.load(Ordering::Acquire),
                ))?
            }
        }
        Ok(())
    }
    fn actor(&self, id: &str) -> Result<&Actor> {
        self.actors.get(id).context("Unknown remote host")
    }
    fn spawn(&mut self, snapshot: HostSnapshot) {
        let (tx, rx) = bounded(16);
        let cancel = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (catalog, store, provider, events, changed) = (
            self.catalog.clone(),
            self.store.clone(),
            self.provider.clone(),
            self.events.clone(),
            self.changed.clone(),
        );
        let (cancel_worker, stop_worker) = (cancel.clone(), stop.clone());
        let id = snapshot.config.id.clone();
        let worker = thread::Builder::new()
            .name(format!("shadowroom-host-{id}"))
            .spawn(move || {
                run_host(
                    snapshot,
                    rx,
                    cancel_worker,
                    stop_worker,
                    catalog,
                    store,
                    provider,
                    events,
                    changed,
                );
            })
            .expect("Start remote host actor");
        self.actors.insert(
            id,
            Actor {
                commands: tx,
                cancel,
                stop,
                worker: Some(worker),
            },
        );
    }
}
impl Drop for RemoteService {
    fn drop(&mut self) {
        // Signal every host before joining any slow connection attempt.
        for actor in self.actors.values() {
            actor.stop.store(true, Ordering::Release);
            actor.cancel();
        }
        for actor in self.actors.values_mut() {
            actor.stop();
        }
    }
}
fn emit(snapshot: &HostSnapshot, events: &Sender<IoEvent>) {
    let _ = events.send(IoEvent::Remote(snapshot.clone()));
}
/// Surfaces each failure once per transition into failure. Snapshots carry the
/// current error for status/automation, but re-sending them (progress, retries)
/// must not re-raise a message the user already dismissed. Automatic retries that
/// fail identically stay quiet; every user command re-arms it.
struct Alerts {
    name: String,
    last: Option<String>,
}
impl Alerts {
    fn raise(&mut self, message: &str, events: &Sender<IoEvent>) {
        if self.last.as_deref() != Some(message) {
            let _ = events.send(IoEvent::Error(format!("{}: {message}", self.name)));
            self.last = Some(message.to_owned());
        }
    }
    fn rearm(&mut self) {
        self.last = None;
    }
}
/// Non-fatal per-file warnings streamed during index/analyze, reported once.
#[derive(Default)]
struct Warnings {
    count: u64,
    first: Option<String>,
}
impl Warnings {
    fn push(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
    }
    fn summary(&self) -> Option<String> {
        let first = self.first.as_deref()?;
        Some(if self.count == 1 {
            format!("Remote indexing warning: {first}")
        } else {
            format!("{} remote indexing warnings; first: {first}", self.count)
        })
    }
}
/// Fallback wake-up while idle. Commands, session closure and retry deadlines
/// wake the actor directly; this only bounds any missed-signal edge case.
const IDLE_WAKE: Duration = Duration::from_secs(30);
fn connect(
    snapshot: &mut HostSnapshot,
    host: &mut Option<RemoteHost>,
    provider: &RemotePreviewProvider,
) -> Result<()> {
    let remote = match host {
        Some(remote) => {
            remote.reconnect(1)?;
            remote.clone()
        }
        None => RemoteHost::connect_cached(
            snapshot.config.spec(),
            provider.cache(),
            Duration::from_secs(15),
        )?,
    };
    let hello = remote.hello();
    if snapshot
        .hello
        .as_ref()
        .is_some_and(|old| old.device_id != hello.device_id)
    {
        remote.disconnect();
        *host = None;
        bail!(
            "Device identity changed. Remove and add this host explicitly to associate the new device; existing photos and decisions are retained."
        );
    }
    let roots = remote.list_roots()?;
    snapshot.hello = Some(hello.clone());
    snapshot.roots = roots;
    provider.connect(hello.device_id, remote.clone());
    *host = Some(remote);
    snapshot.status = HostStatus::Connected;
    Ok(())
}
/// Streams an index/analyze request to completion, publishing throttled progress.
#[allow(clippy::too_many_arguments)]
fn stream_index(
    remote: &RemoteHost,
    request: Request,
    require_complete: bool,
    snapshot: &mut HostSnapshot,
    warnings: &mut Warnings,
    events: &Sender<IoEvent>,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    let mut pending = remote.request_with_timeout(request, Duration::from_secs(24 * 3600))?;
    let mut last = Instant::now() - Duration::from_secs(1);
    loop {
        match pending.recv_cancellable(&cancelled)?.header.message {
            Response::Progress(p) => {
                snapshot.progress = Some(p);
                if last.elapsed() >= Duration::from_millis(150) && events.len() < 128 {
                    emit(snapshot, events);
                    last = Instant::now();
                }
            }
            Response::Indexed(summary) => {
                ensure!(
                    !(require_complete && summary.cancelled),
                    "Remote indexing cancelled"
                );
                snapshot.progress = Some(summary.progress);
                return Ok(());
            }
            Response::Error { message, .. } => warnings.push(message),
            _ => bail!("Unexpected index response"),
        }
    }
}
#[allow(clippy::too_many_arguments)]
/// A session that stays up this long resets the reconnect backoff.
const STABLE_SESSION: Duration = Duration::from_secs(5);

/// Exponential reconnect backoff: 250 ms doubling per failure, capped at 30 s.
fn reconnect_delay(failures: u32) -> Duration {
    Duration::from_millis((250u64 << failures.min(7)).min(30_000))
}

fn run_host(
    mut snapshot: HostSnapshot,
    commands: Receiver<Command>,
    cancel: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    catalog: Catalog,
    store: Arc<Store>,
    provider: Arc<RemotePreviewProvider>,
    events: Sender<IoEvent>,
    changed: Sender<()>,
) {
    let mut host: Option<RemoteHost> = None;
    let mut desired = false;
    let mut retry = Instant::now();
    let mut failures = 0u32;
    // When the current session connected, so a session that dies right after
    // connecting counts as a failure and backs off instead of looping.
    let mut connected_at: Option<Instant> = None;
    let mut alerts = Alerts {
        name: snapshot.config.display_name.clone(),
        last: None,
    };
    emit(&snapshot, &events);
    while !stop.load(Ordering::Acquire) {
        // Block until a command, the session closing, or the reconnect backoff
        // elapses; an idle host performs no periodic wake-ups.
        let connected = host.as_ref().is_some_and(RemoteHost::is_connected);
        let closed = match &host {
            Some(remote) if connected => remote.closed_signal(),
            _ => crossbeam_channel::never(),
        };
        let wait = if desired && !connected {
            retry.saturating_duration_since(Instant::now())
        } else {
            IDLE_WAKE
        };
        let command = crossbeam_channel::select! {
            recv(commands) -> command => match command {
                Ok(Command::Stop) | Err(_) => break,
                Ok(command) => Some(command),
            },
            recv(closed) -> _ => {
                let short = connected_at
                    .take()
                    .is_some_and(|at| at.elapsed() < STABLE_SESSION);
                failures = if short { failures.saturating_add(1) } else { 0 };
                retry = Instant::now() + reconnect_delay(failures);
                None
            }
            default(wait) => None,
        };
        if command.is_some() {
            alerts.rearm();
        }
        if let Some(Command::Disconnect) = command {
            desired = false;
            if let Some(remote) = &host {
                remote.disconnect();
            }
            if let Some(hello) = &snapshot.hello {
                provider.disconnect(&hello.device_id);
            }
            snapshot.status = HostStatus::Offline { error: None };
            snapshot.progress = None;
            emit(&snapshot, &events);
            let _ = store.put(snapshot.clone());
            continue;
        }
        if matches!(command, Some(Command::Connect)) {
            desired = true;
            retry = Instant::now();
            failures = 0;
        }
        if desired
            && (host.as_ref().is_none_or(|h| !h.is_connected())
                || matches!(command, Some(Command::Connect)))
            && retry <= Instant::now()
        {
            snapshot.begin_connecting();
            emit(&snapshot, &events);
            match connect(&mut snapshot, &mut host, &provider) {
                Ok(()) => {
                    // `failures` resets once the session proves stable (see the
                    // closed-signal arm), not merely on a successful hello.
                    connected_at = Some(Instant::now());
                    alerts.rearm();
                    let _ = changed.try_send(());
                }
                Err(error) => {
                    if let Some(remote) = host.take() {
                        remote.disconnect();
                    }
                    if let Some(hello) = &snapshot.hello {
                        provider.disconnect(&hello.device_id);
                    }
                    failures = failures.saturating_add(1);
                    let message = format!(
                        "Connection/hello failed: {error:#}. Check the SSH alias, remote command, agent version, and stdout banners."
                    );
                    if message.contains("Device identity changed") {
                        desired = false;
                    }
                    alerts.raise(&message, &events);
                    snapshot.status = HostStatus::Error(message);
                    retry = Instant::now() + reconnect_delay(failures);
                }
            }
            emit(&snapshot, &events);
            if let Err(error) = store.put(snapshot.clone()) {
                let _ = events.send(IoEvent::Error(format!("Save remote hosts: {error:#}")));
            }
        }
        if !desired {
            if matches!(command, Some(Command::Index(_, _) | Command::Sync(_, _, _))) {
                let message = "Host is offline; connect before indexing or refreshing";
                alerts.raise(message, &events);
                // Keep a non-offline failure state (e.g. identity mismatch) intact.
                if snapshot.state() == HostState::Offline {
                    snapshot.status = HostStatus::Offline {
                        error: Some(message.into()),
                    };
                }
                emit(&snapshot, &events);
            }
            continue;
        }
        if let Some(command @ (Command::Index(_, _) | Command::Sync(_, _, _))) = command {
            let mut warnings = Warnings::default();
            let result = (|| -> Result<()> {
                let remote = host
                    .as_ref()
                    .filter(|h| h.is_connected())
                    .context("Host is offline; connect before indexing or refreshing")?;
                let (root, index, full, epoch) = match command {
                    Command::Index(root, epoch) => (root, true, false, epoch),
                    Command::Sync(root, full, epoch) => (root, false, full, epoch),
                    _ => unreachable!(),
                };
                ensure!(
                    snapshot.roots.iter().any(|r| r.id == root),
                    "Unknown root id; reconnect to refresh reported roots"
                );
                ensure!(
                    cancel.load(Ordering::Acquire) == epoch,
                    "Remote operation cancelled before starting"
                );
                snapshot.status = HostStatus::Indexing;
                emit(&snapshot, &events);
                let cancelled =
                    || cancel.load(Ordering::Acquire) != epoch || stop.load(Ordering::Acquire);
                if index {
                    stream_index(
                        remote,
                        Request::Index { root: root.clone() },
                        true,
                        &mut snapshot,
                        &mut warnings,
                        &events,
                        cancelled,
                    )?;
                }
                let device = &remote.hello().device_id;
                let mut revision =
                    sync_root(&catalog, remote, device, &root, full, epoch, &cancel, &stop)?;
                snapshot.revisions.insert(root.clone(), revision);
                let _ = changed.try_send(());
                // Photos are visible now; fill in perceptual hashes on the agent so
                // burst grouping is complete without fetching every preview.
                if index && remote.hello().capabilities.iter().any(|c| c == "analyze") {
                    stream_index(
                        remote,
                        Request::Analyze { root: root.clone() },
                        false,
                        &mut snapshot,
                        &mut warnings,
                        &events,
                        cancelled,
                    )?;
                    revision = sync_root(
                        &catalog, remote, device, &root, false, epoch, &cancel, &stop,
                    )?;
                    snapshot.revisions.insert(root, revision);
                    let _ = changed.try_send(());
                }
                Ok(())
            })();
            if let Some(summary) = warnings.summary() {
                alerts.raise(&summary, &events);
            }
            match result {
                Ok(()) => snapshot.status = HostStatus::Connected,
                Err(e) => {
                    let message = format!("Remote operation: {e:#}");
                    alerts.raise(&message, &events);
                    snapshot.status = if host.as_ref().is_some_and(|h| h.is_connected()) {
                        HostStatus::Error(message)
                    } else {
                        HostStatus::Offline {
                            error: Some(message),
                        }
                    };
                }
            }
            emit(&snapshot, &events);
            let _ = store.put(snapshot.clone());
        }
        if desired
            && host.as_ref().is_some_and(|h| !h.is_connected())
            && !matches!(snapshot.state(), HostState::Offline | HostState::Connecting)
        {
            if let Some(hello) = &snapshot.hello {
                provider.disconnect(&hello.device_id);
            }
            snapshot.go_offline();
            emit(&snapshot, &events);
        }
    }
    if let Some(remote) = host {
        remote.disconnect();
    }
    if let Some(hello) = &snapshot.hello {
        provider.disconnect(&hello.device_id);
    }
    snapshot.go_offline();
    snapshot.progress = None;
    if let Err(e) = store.put(snapshot) {
        let _ = events.send(IoEvent::Error(format!("Save remote host state: {e:#}")));
    }
}
#[allow(clippy::too_many_arguments)]
fn sync_root(
    catalog: &Catalog,
    host: &RemoteHost,
    device: &str,
    root: &str,
    full: bool,
    epoch: u64,
    cancel: &AtomicU64,
    stop: &AtomicBool,
) -> Result<u64> {
    match sync_root_once(catalog, host, device, root, full, epoch, cancel, stop) {
        // The agent lost its manifest (state restored or reset) while keeping
        // its device id: our cursor is from its previous history. An
        // incremental sync can never succeed again, so resynchronize fully;
        // the full publish also retires records the agent no longer reports.
        Err(error) if !full && format!("{error:#}").contains("Revision ahead of agent") => {
            sync_root_once(catalog, host, device, root, true, epoch, cancel, stop)
        }
        result => result,
    }
}
#[allow(clippy::too_many_arguments)]
fn sync_root_once(
    catalog: &Catalog,
    host: &RemoteHost,
    device: &str,
    root: &str,
    full: bool,
    epoch: u64,
    cancel: &AtomicU64,
    stop: &AtomicBool,
) -> Result<u64> {
    let since = if full {
        0
    } else {
        catalog.remote_revision(device, root)?
    };
    let token = format!("{:032x}", rand::random::<u128>());
    let result = (|| -> Result<u64> {
        let mut pending = host.request_with_timeout(
            Request::Photos {
                root: root.into(),
                since_revision: since,
            },
            Duration::from_secs(3600),
        )?;
        let mut records: Vec<PhotoRecord> = Vec::with_capacity(128);
        let mut deleted = Vec::with_capacity(128);
        loop {
            match pending
                .recv_cancellable(|| {
                    cancel.load(Ordering::Acquire) != epoch || stop.load(Ordering::Acquire)
                })?
                .header
                .message
            {
                Response::Photo(p) => records.push(p),
                Response::Deleted { relative_path } => deleted.push(relative_path),
                Response::Synced { revision } => {
                    catalog.stage_remote(&token, device, root, &records, &deleted)?;
                    catalog.finish_remote_sync(&token, device, root, revision, since == 0)?;
                    return Ok(revision);
                }
                _ => bail!("Unexpected metadata sync response"),
            }
            if records.len() + deleted.len() >= 128 {
                catalog.stage_remote(&token, device, root, &records, &deleted)?;
                records.clear();
                deleted.clear();
            }
        }
    })();
    if result.is_err() {
        let _ = catalog.discard_remote_stage(&token);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::remote::protocol::{Frame, PROTOCOL_VERSION, PhotoKey, read_frame, write_frame};
    use std::os::unix::net::UnixStream;

    fn config() -> HostConfig {
        HostConfig {
            id: "h".into(),
            display_name: "Host".into(),
            ssh_alias: "alias".into(),
            remote_command: default_command(),
            extra_ssh_args: vec![],
            local_program: None,
            local_args: vec![],
        }
    }

    #[test]
    fn host_snapshot_wire_format_is_unchanged_and_round_trips() {
        let mut snapshot = HostSnapshot::new(config());
        snapshot.status = HostStatus::Error("boom".into());
        assert_eq!(
            serde_json::to_string(&snapshot).unwrap(),
            r#"{"config":{"id":"h","display_name":"Host","ssh_alias":"alias","remote_command":"shadowroom-remote serve --stdio","extra_ssh_args":[]},"hello":null,"roots":[],"state":"error","error":"boom","progress":null,"revisions":{}}"#
        );
        for (status, state, error) in [
            (HostStatus::Offline { error: None }, "offline", None),
            (
                HostStatus::Offline {
                    error: Some("gone".into()),
                },
                "offline",
                Some("gone"),
            ),
            (
                HostStatus::Connecting {
                    retrying: Some("again".into()),
                },
                "connecting",
                Some("again"),
            ),
            (HostStatus::Connected, "connected", None),
            (HostStatus::Indexing, "indexing", None),
        ] {
            snapshot.status = status.clone();
            let value = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(value["state"], state);
            assert_eq!(value["error"].as_str(), error);
            let back: HostSnapshot = serde_json::from_value(value).unwrap();
            assert_eq!(back.status, status);
        }
        // Contradictory legacy records normalize to a representable status.
        let mut value = serde_json::to_value(&snapshot).unwrap();
        value["state"] = "connected".into();
        value["error"] = "stale".into();
        let back: HostSnapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back.status, HostStatus::Connected);
        value["state"] = "error".into();
        value["error"] = serde_json::Value::Null;
        let back: HostSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(back.state(), HostState::Error);
        assert!(back.error().is_some());
    }

    #[test]
    fn alerts_raise_once_per_failure_until_rearmed() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut alerts = Alerts {
            name: "Host".into(),
            last: None,
        };
        alerts.raise("down", &tx);
        alerts.raise("down", &tx);
        alerts.raise("down", &tx);
        alerts.raise("other", &tx);
        alerts.rearm();
        alerts.raise("other", &tx);
        let raised: Vec<_> = rx
            .try_iter()
            .map(|event| match event {
                IoEvent::Error(message) => message,
                _ => panic!("unexpected event"),
            })
            .collect();
        assert_eq!(raised, ["Host: down", "Host: other", "Host: other"]);
    }

    #[test]
    fn failed_stream_after_staged_batch_preserves_catalog_and_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        let key = PhotoKey {
            root_id: "root".into(),
            volume_id: "volume".into(),
            relative_path: "original.jpg".into(),
            size_bytes: 1,
            modified_at: 1,
            companion_path: None,
            companion_size_bytes: 0,
            companion_modified_at: 0,
        };
        let record = PhotoRecord {
            key,
            metadata: crate::model::Photo::default(),
            revision: 7,
        };
        catalog
            .stage_remote("first", "device", "root", &[record.clone()], &[])
            .unwrap();
        catalog
            .finish_remote_sync("first", "device", "root", 7, true)
            .unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        let socket = client.try_clone().unwrap();
        let worker = thread::spawn(move || {
            let hello = read_frame::<Request>(&mut server).unwrap().unwrap();
            write_frame(
                &mut server,
                &Frame::new(
                    hello.header.id,
                    true,
                    Response::Hello(Hello {
                        protocol_version: PROTOCOL_VERSION,
                        app_version: "test".into(),
                        device_id: "device".into(),
                        hostname: "test".into(),
                        os: "test".into(),
                        arch: "test".into(),
                        capabilities: vec![],
                    }),
                    vec![],
                ),
            )
            .unwrap();
            let request = read_frame::<Request>(&mut server).unwrap().unwrap();
            assert!(matches!(
                request.header.message,
                Request::Photos {
                    since_revision: 7,
                    ..
                }
            ));
            for i in 0..130 {
                let mut changed = record.clone();
                changed.key.relative_path = format!("new-{i}.jpg").into();
                write_frame(
                    &mut server,
                    &Frame::new(request.header.id, false, Response::Photo(changed), vec![]),
                )
                .unwrap();
            }
            write_frame(
                &mut server,
                &Frame::new(
                    request.header.id,
                    true,
                    Response::Error {
                        code: "interrupted".into(),
                        message: "test interrupted after a complete staging batch".into(),
                    },
                    vec![],
                ),
            )
            .unwrap();
        });
        let host = RemoteHost::from_io(
            client.try_clone().unwrap(),
            client,
            dir.path().join("wire"),
            Duration::from_secs(10),
        )
        .unwrap();
        let result = sync_root(
            &catalog,
            &host,
            "device",
            "root",
            false,
            0,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        );
        assert!(result.unwrap_err().to_string().contains("interrupted"));
        assert_eq!(catalog.remote_revision("device", "root").unwrap(), 7);
        let photos = catalog.photos().unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].path, PathBuf::from("original.jpg"));
        let conn = rusqlite::Connection::open(catalog.path()).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM remote_stage", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        host.disconnect();
        let _ = socket.shutdown(std::net::Shutdown::Both);
        worker.join().unwrap();
    }

    #[test]
    fn cursor_ahead_of_a_reset_agent_falls_back_to_a_full_resync() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&dir.path().join("db")).unwrap();
        let record = |path: &str, revision| PhotoRecord {
            key: PhotoKey {
                root_id: "root".into(),
                volume_id: "volume".into(),
                relative_path: path.into(),
                size_bytes: 1,
                modified_at: 1,
                companion_path: None,
                companion_size_bytes: 0,
                companion_modified_at: 0,
            },
            metadata: crate::model::Photo::default(),
            revision,
        };
        catalog
            .stage_remote("old", "device", "root", &[record("gone.jpg", 40)], &[])
            .unwrap();
        catalog
            .finish_remote_sync("old", "device", "root", 40, true)
            .unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        let socket = client.try_clone().unwrap();
        let worker = thread::spawn(move || {
            let mut sinces = Vec::new();
            while let Some(frame) = read_frame::<Request>(&mut server).unwrap() {
                let id = frame.header.id;
                let response = match frame.header.message {
                    Request::Hello { .. } => Response::Hello(Hello {
                        protocol_version: PROTOCOL_VERSION,
                        app_version: "test".into(),
                        device_id: "device".into(),
                        hostname: "test".into(),
                        os: "test".into(),
                        arch: "test".into(),
                        capabilities: vec![],
                    }),
                    // The reset agent's history now ends at revision 2.
                    Request::Photos { since_revision, .. } if since_revision > 2 => {
                        sinces.push(since_revision);
                        Response::Error {
                            code: "request_failed".into(),
                            message: "Revision ahead of agent; reset since_revision to zero".into(),
                        }
                    }
                    Request::Photos { since_revision, .. } => {
                        sinces.push(since_revision);
                        write_frame(
                            &mut server,
                            &Frame::new(id, false, Response::Photo(record("kept.jpg", 2)), vec![]),
                        )
                        .unwrap();
                        Response::Synced { revision: 2 }
                    }
                    Request::Cancel { .. } => continue,
                    other => panic!("unexpected {other:?}"),
                };
                write_frame(&mut server, &Frame::new(id, true, response, vec![])).unwrap();
            }
            sinces
        });
        let host = RemoteHost::from_io(
            client.try_clone().unwrap(),
            client,
            dir.path().join("wire"),
            Duration::from_secs(10),
        )
        .unwrap();
        let revision = sync_root(
            &catalog,
            &host,
            "device",
            "root",
            false,
            0,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(revision, 2);
        assert_eq!(catalog.remote_revision("device", "root").unwrap(), 2);
        let available: Vec<_> = catalog
            .photos()
            .unwrap()
            .into_iter()
            .map(|p| (p.path.to_string_lossy().into_owned(), p.source))
            .collect();
        assert_eq!(available.len(), 2);
        for (path, source) in available {
            let crate::model::PhotoSource::Remote(source) = source else {
                panic!("remote photo expected");
            };
            assert_eq!(
                source.available,
                source.relative_path == PathBuf::from("kept.jpg"),
                "{path}"
            );
        }
        host.disconnect();
        let _ = socket.shutdown(std::net::Shutdown::Both);
        assert_eq!(worker.join().unwrap(), [40, 0]);
    }
}
