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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub config: HostConfig,
    pub hello: Option<Hello>,
    pub roots: Vec<RemoteRoot>,
    pub state: String,
    pub error: Option<String>,
    pub progress: Option<Progress>,
    #[serde(default)]
    pub revisions: BTreeMap<String, u64>,
}
impl HostSnapshot {
    fn new(config: HostConfig) -> Self {
        Self {
            config,
            hello: None,
            roots: vec![],
            state: "offline".into(),
            error: None,
            progress: None,
            revisions: BTreeMap::new(),
        }
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
            snapshot.state = "offline".into();
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
fn connect(
    snapshot: &mut HostSnapshot,
    host: &mut Option<RemoteHost>,
    provider: &RemotePreviewProvider,
) -> Result<()> {
    snapshot.state = "connecting".into();
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
    snapshot.state = "connected".into();
    snapshot.error = None;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
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
    emit(&snapshot, &events);
    while !stop.load(Ordering::Acquire) {
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(Command::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Ok(command) => Some(command),
            Err(_) => None,
        };
        if let Some(Command::Disconnect) = command {
            desired = false;
            if let Some(remote) = &host {
                remote.disconnect();
            }
            if let Some(hello) = &snapshot.hello {
                provider.disconnect(&hello.device_id);
            }
            snapshot.state = "offline".into();
            snapshot.progress = None;
            snapshot.error = None;
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
            snapshot.state = "connecting".into();
            emit(&snapshot, &events);
            match connect(&mut snapshot, &mut host, &provider) {
                Ok(()) => {
                    failures = 0;
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
                    snapshot.state = "error".into();
                    snapshot.error = Some(format!(
                        "Connection/hello failed: {error:#}. Check the SSH alias, remote command, agent version, and stdout banners."
                    ));
                    if snapshot
                        .error
                        .as_deref()
                        .is_some_and(|e| e.contains("Device identity changed"))
                    {
                        desired = false;
                    }
                    retry = Instant::now()
                        + Duration::from_millis((250u64 << failures.min(7)).min(30_000));
                }
            }
            emit(&snapshot, &events);
            if let Err(error) = store.put(snapshot.clone()) {
                let _ = events.send(IoEvent::Error(format!("Save remote hosts: {error:#}")));
            }
        }
        if !desired {
            if matches!(command, Some(Command::Index(_, _) | Command::Sync(_, _, _))) {
                snapshot.error =
                    Some("Host is offline; connect before indexing or refreshing".into());
                emit(&snapshot, &events);
            }
            continue;
        }
        if let Some(command @ (Command::Index(_, _) | Command::Sync(_, _, _))) = command {
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
                snapshot.state = "indexing".into();
                snapshot.error = None;
                emit(&snapshot, &events);
                if index {
                    let mut pending = remote.request_with_timeout(
                        Request::Index { root: root.clone() },
                        Duration::from_secs(24 * 3600),
                    )?;
                    let mut last = Instant::now() - Duration::from_secs(1);
                    loop {
                        match pending
                            .recv_cancellable(|| {
                                cancel.load(Ordering::Acquire) != epoch
                                    || stop.load(Ordering::Acquire)
                            })?
                            .header
                            .message
                        {
                            Response::Progress(p) => {
                                snapshot.progress = Some(p);
                                if last.elapsed() >= Duration::from_millis(150)
                                    && events.len() < 128
                                {
                                    emit(&snapshot, &events);
                                    last = Instant::now();
                                }
                            }
                            Response::Indexed(summary) => {
                                ensure!(!summary.cancelled, "Remote indexing cancelled");
                                snapshot.progress = Some(summary.progress);
                                break;
                            }
                            Response::Error { message, .. } => {
                                snapshot.error = Some(message);
                            }
                            _ => bail!("Unexpected index response"),
                        }
                    }
                }
                let device = &remote.hello().device_id;
                let mut revision =
                    sync_root(&catalog, remote, device, &root, full, epoch, &cancel, &stop)?;
                snapshot.revisions.insert(root.clone(), revision);
                let _ = changed.try_send(());
                // Photos are visible now; fill in perceptual hashes on the agent so
                // burst grouping is complete without fetching every preview.
                if index && remote.hello().capabilities.iter().any(|c| c == "analyze") {
                    let mut pending = remote.request_with_timeout(
                        Request::Analyze { root: root.clone() },
                        Duration::from_secs(24 * 3600),
                    )?;
                    let mut last = Instant::now() - Duration::from_secs(1);
                    loop {
                        match pending
                            .recv_cancellable(|| {
                                cancel.load(Ordering::Acquire) != epoch
                                    || stop.load(Ordering::Acquire)
                            })?
                            .header
                            .message
                        {
                            Response::Progress(p) => {
                                snapshot.progress = Some(p);
                                if last.elapsed() >= Duration::from_millis(150)
                                    && events.len() < 128
                                {
                                    emit(&snapshot, &events);
                                    last = Instant::now();
                                }
                            }
                            Response::Indexed(summary) => {
                                snapshot.progress = Some(summary.progress);
                                break;
                            }
                            Response::Error { message, .. } => snapshot.error = Some(message),
                            _ => bail!("Unexpected analyze response"),
                        }
                    }
                    revision = sync_root(
                        &catalog, remote, device, &root, false, epoch, &cancel, &stop,
                    )?;
                    snapshot.revisions.insert(root, revision);
                    let _ = changed.try_send(());
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    snapshot.state = "connected".into();
                }
                Err(e) => {
                    snapshot.state = if host.as_ref().is_some_and(|h| h.is_connected()) {
                        "error"
                    } else {
                        "offline"
                    }
                    .into();
                    snapshot.error = Some(format!("Remote operation: {e:#}"));
                }
            }
            emit(&snapshot, &events);
            let _ = store.put(snapshot.clone());
        }
        if desired
            && host.as_ref().is_some_and(|h| !h.is_connected())
            && snapshot.state != "offline"
            && snapshot.state != "connecting"
        {
            if let Some(hello) = &snapshot.hello {
                provider.disconnect(&hello.device_id);
            }
            snapshot.state = "offline".into();
            emit(&snapshot, &events);
        }
    }
    if let Some(remote) = host {
        remote.disconnect();
    }
    if let Some(hello) = &snapshot.hello {
        provider.disconnect(&hello.device_id);
    }
    snapshot.state = "offline".into();
    snapshot.progress = None;
    if let Err(e) = store.put(snapshot) {
        let _ = events.send(IoEvent::Error(format!("Save remote host state: {e:#}")));
    }
}
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
}
