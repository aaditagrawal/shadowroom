//! MCP stdio facade over a bounded, authenticated loopback bridge to the GPUI app.
//! This module never launches processes, evaluates commands, or changes photo files.
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const INPUT_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 8 * 1024 * 1024;
const WORKERS: usize = 4;
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
type AppReply = std::result::Result<Value, String>;

struct AcceptedConnection {
    stream: TcpStream,
    accepted_at: Instant,
}

#[derive(Default)]
struct DiagnosticLimiter {
    last: Option<Instant>,
    suppressed: u64,
}

// Log only fixed classifications and OS error kinds: never wire payloads, tokens,
// or descriptor contents. Logging contention must not hold the admission path.
fn bridge_diagnostic(
    limiter: &Mutex<DiagnosticLimiter>,
    message: &'static str,
    kind: Option<io::ErrorKind>,
) {
    let Ok(mut state) = limiter.try_lock() else {
        return;
    };
    if state
        .last
        .is_some_and(|last| last.elapsed() < Duration::from_secs(1))
    {
        state.suppressed = state.suppressed.saturating_add(1);
        return;
    }
    let suppressed = std::mem::take(&mut state.suppressed);
    state.last = Some(Instant::now());
    drop(state);
    eprintln!("Shadowroom IPC: {message}; io_kind={kind:?}; suppressed={suppressed}");
}

#[derive(Debug)]
pub struct AutomationRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<AppReply>,
    /// The UI must reject a request before dispatch if this instant has passed.
    pub deadline: Instant,
    pub request_id: String,
}
impl AutomationRequest {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

pub struct AutomationServer {
    pub requests: Receiver<AutomationRequest>,
    stop: Arc<AtomicBool>,
    manifest_path: PathBuf,
    token: String,
    listener: Option<JoinHandle<()>>,
    address: SocketAddr,
    workers: Vec<JoinHandle<()>>,
    _instance_lock: fs::File,
}
impl Drop for AutomationServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Never unlink a manifest belonging to a replacement process.
        if read_manifest(&self.manifest_path).is_ok_and(|m| constant_time_eq(&m.token, &self.token))
        {
            let _ = fs::remove_file(&self.manifest_path);
        }
        if let Some(listener) = self.listener.take() {
            // Wake blocking accept immediately, without an idle polling thread.
            let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
            let _ = listener.join();
        }
        // Workers have socket/deadline limits; do not block the rendering thread
        // while a local client finishes an incomplete line.
        self.workers.clear();
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    address: SocketAddr,
    token: String,
    pid: u32,
}
#[derive(Serialize, Deserialize)]
struct WireRequest {
    token: String,
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
    #[serde(default)]
    request_id: Option<String>,
}
fn empty_object() -> Value {
    json!({})
}

pub fn manifest_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("Locate user configuration directory")?
        .config_dir()
        .join("shadowroom/automation.json"))
}

pub fn start_server() -> Result<AutomationServer> {
    start_server_at(manifest_path()?)
}
fn start_server_at(path: PathBuf) -> Result<AutomationServer> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    if lock_path.exists() {
        check_private_file(&lock_path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let instance_lock = options
        .open(&lock_path)
        .context("Open automation instance lock")?;
    instance_lock
        .try_lock()
        .context("Another Shadowroom automation endpoint is already running")?;
    if path.exists() {
        check_private_file(&path)?;
        match read_manifest(&path) {
            Ok(old) => match TcpStream::connect_timeout(&old.address, Duration::from_millis(300)) {
                Ok(_) => bail!("Another Shadowroom automation endpoint is already running"),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::AddrNotAvailable
                    ) => {}
                Err(error) => {
                    return Err(error)
                        .context("Existing endpoint could not be verified; manifest preserved");
                }
            },
            Err(_) => {
                let quarantine =
                    path.with_file_name(format!("automation.invalid-{}.json", request_id()));
                fs::rename(&path, &quarantine)
                    .context("Preserve incomplete automation manifest")?;
            }
        }
    }
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("Bind local automation endpoint")?;
    let mut random = [0u8; 32];
    rand::rng().fill_bytes(&mut random);
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let manifest = Manifest {
        version: 1,
        address: listener.local_addr()?,
        token: token.clone(),
        pid: std::process::id(),
    };
    // Publish complete JSON atomically; a crash while writing never exposes a
    // half-written descriptor to readers or subsequent launches.
    let temporary = path.with_file_name(format!(".automation-{}.tmp", request_id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let published = (|| -> Result<()> {
        serde_json::to_writer(&mut file, &manifest)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = published {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (request_tx, requests) = bounded(64);
    let (connection_tx, connections) = bounded::<AcceptedConnection>(16);
    let handler_diagnostics = Arc::new(Mutex::new(DiagnosticLimiter::default()));
    let mut server = AutomationServer {
        requests,
        stop: stop.clone(),
        manifest_path: path,
        token: token.clone(),
        listener: None,
        address: manifest.address,
        workers: Vec::new(),
        _instance_lock: instance_lock,
    };
    for index in 0..WORKERS {
        let connections = connections.clone();
        let requests = request_tx.clone();
        let stop = stop.clone();
        let token = token.clone();
        let diagnostics = handler_diagnostics.clone();
        server.workers.push(
            thread::Builder::new()
                .name(format!("shadowroom-ipc-{index}"))
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        match connections.recv_timeout(Duration::from_millis(100)) {
                            Ok(connection) => {
                                if let Err(error) =
                                    handle_connection(connection, &token, &requests, &stop)
                                {
                                    let kind =
                                        error.downcast_ref::<io::Error>().map(io::Error::kind);
                                    let class = if error
                                        .to_string()
                                        .starts_with("JSON response exceeds ")
                                    {
                                        "response frame too large"
                                    } else {
                                        "connection handler failed"
                                    };
                                    bridge_diagnostic(&diagnostics, class, kind);
                                }
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })?,
        );
    }
    server.listener = Some(
        thread::Builder::new()
            .name("shadowroom-ipc-listen".into())
            .spawn(move || {
                let diagnostics = Mutex::new(DiagnosticLimiter::default());
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            let connection = AcceptedConnection {
                                stream,
                                accepted_at: Instant::now(),
                            };
                            if let Err(error) = connection_tx.try_send(connection) {
                                let message = match error {
                                    crossbeam_channel::TrySendError::Full(_) => {
                                        "connection admission rejected: queue full"
                                    }
                                    crossbeam_channel::TrySendError::Disconnected(_) => {
                                        "connection admission rejected: workers stopped"
                                    }
                                };
                                bridge_diagnostic(&diagnostics, message, None);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            bridge_diagnostic(
                                &diagnostics,
                                "listener accept failed",
                                Some(error.kind()),
                            );
                            break;
                        }
                    }
                }
            })?,
    );
    Ok(server)
}

fn check_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .context("Shadowroom is not running; open the desktop app first")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Automation descriptor must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Automation manifest permissions must be owner-only (0600)");
        }
        if let Some(parent) = path.parent() {
            if metadata.uid() != fs::metadata(parent)?.uid() {
                bail!(
                    "Automation descriptor must belong to its user configuration directory owner"
                );
            }
        }
    }
    Ok(())
}
fn read_manifest(path: &Path) -> Result<Manifest> {
    check_private_file(path)?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        bail!("Automation manifest exceeds size limit");
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("Invalid automation manifest")?;
    if manifest.version != 1
        || manifest.address.ip() != Ipv4Addr::LOCALHOST
        || manifest.token.len() != 64
        || !manifest.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("Invalid local automation endpoint; restart Shadowroom");
    }
    Ok(manifest)
}
fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn handle_connection(
    connection: AcceptedConnection,
    token: &str,
    requests: &Sender<AutomationRequest>,
    stop: &AtomicBool,
) -> Result<()> {
    handle_connection_at(
        connection.stream,
        token,
        requests,
        stop,
        connection.accepted_at,
        REQUEST_TIMEOUT,
    )
}
#[cfg(test)]
fn handle_connection_with_timeout(
    stream: TcpStream,
    token: &str,
    requests: &Sender<AutomationRequest>,
    stop: &AtomicBool,
    request_timeout: Duration,
) -> Result<()> {
    handle_connection_at(
        stream,
        token,
        requests,
        stop,
        Instant::now(),
        request_timeout,
    )
}

fn handle_connection_at(
    stream: TcpStream,
    token: &str,
    requests: &Sender<AutomationRequest>,
    stop: &AtomicBool,
    accepted_at: Instant,
    request_timeout: Duration,
) -> Result<()> {
    // Darwin inherits O_NONBLOCK from the listener. DeadlineStream relies on
    // blocking reads/writes with socket timeouts; otherwise partial requests or
    // a full send buffer can yield WouldBlock and truncate a large reply.
    stream
        .set_nonblocking(false)
        .context("Configure blocking automation connection")?;
    stream
        .set_nodelay(true)
        .context("Configure automation response socket")?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let request = read_line_limited(
        &mut BufReader::new(DeadlineStream {
            stream: stream.try_clone()?,
            deadline: accepted_at + FRAME_TIMEOUT,
        }),
        INPUT_LIMIT,
    )
    .and_then(|line| line.context("Empty IPC request"))
    .and_then(|line| serde_json::from_slice::<WireRequest>(&line).context("Invalid IPC JSON"));
    let result: AppReply = match request {
        Err(error) => Err(error.to_string()),
        // Parsing or scheduling may consume the remaining frame time even when
        // the socket supplied a complete frame in one read.
        Ok(_) if Instant::now() >= accepted_at + FRAME_TIMEOUT => {
            Err("IPC frame deadline exceeded".into())
        }
        Ok(request) if !constant_time_eq(&request.token, token) => {
            Err("Authentication failed".into())
        }
        Ok(request) if request.method == "__ping" => Ok(json!({"alive":true})),
        Ok(request) => match normalize_arguments(&request.method, request.params) {
            Err(error) => Err(error),
            Ok(params) => {
                let (reply_tx, reply_rx) = bounded(1);
                let deadline = Instant::now() + request_timeout;
                let request_id = request
                    .request_id
                    .filter(|id| {
                        !id.is_empty()
                            && id.len() <= 64
                            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    .unwrap_or_else(request_id);
                let request = AutomationRequest {
                    method: request.method,
                    params,
                    reply: reply_tx,
                    deadline,
                    request_id,
                };
                match requests.try_send(request) {
                    Err(crossbeam_channel::TrySendError::Full(_)) => Err(
                        "Desktop automation queue is busy; retry after current work finishes"
                            .into(),
                    ),
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        Err("Desktop app is closing".into())
                    }
                    Ok(()) => loop {
                        if stop.load(Ordering::Acquire) {
                            break Err("Desktop app is closing".into());
                        }
                        if Instant::now() >= deadline {
                            break Err("Desktop did not acknowledge within 20 seconds; inspect workspace state before retrying a mutation".into());
                        }
                        match reply_rx.recv_timeout(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(100)),
                        ) {
                            Ok(result) => break result,
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                break Err("Desktop dropped the automation request".into());
                            }
                        }
                    },
                }
            }
        },
    };
    // Move the potentially large result into its envelope instead of cloning
    // the complete Value tree through json! / Serialize for Value.
    let response = match result {
        Ok(value) => Value::Object(serde_json::Map::from_iter([("result".into(), value)])),
        Err(message) => json!({"error":message}),
    };
    write_json(
        &mut DeadlineStream {
            stream,
            deadline: Instant::now() + FRAME_TIMEOUT,
        },
        &response,
        RESPONSE_LIMIT,
    )
}
fn forward_at(path: &Path, method: &str, params: Value) -> AppReply {
    let result = (|| -> Result<Value> {
        let manifest = read_manifest(path)?;
        let mut stream = TcpStream::connect_timeout(&manifest.address, Duration::from_secs(1))
            .context(
                "Shadowroom desktop endpoint is unavailable; open or restart the desktop app",
            )?;
        stream
            .set_nodelay(true)
            .context("Configure desktop request socket")?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT + Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        write_json(
            &mut stream,
            &json!({"token":manifest.token,"method":method,"params":params,"request_id":request_id()}),
            INPUT_LIMIT,
        )?;
        let line = read_line_limited(&mut BufReader::new(stream), RESPONSE_LIMIT)?
            .context("Desktop closed the automation connection")?;
        let mut value: Value = serde_json::from_slice(&line).context("Invalid desktop response")?;
        if let Some(error) = value.get("error") {
            bail!("{}", error.as_str().unwrap_or("Desktop request failed"));
        }
        value
            .as_object_mut()
            .and_then(|object| object.remove("result"))
            .context("Desktop response has no result")
    })();
    result.map_err(|error| format!("{error:#}"))
}

fn request_id() -> String {
    format!(
        "{:016x}{:016x}",
        rand::rng().next_u64(),
        rand::rng().next_u64()
    )
}
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}
impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "IPC frame deadline exceeded")
            })?;
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IPC frame deadline exceeded",
            ));
        }
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "IPC write deadline exceeded")
            })?;
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IPC write deadline exceeded",
            ));
        }
        self.stream.set_write_timeout(Some(remaining))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
fn read_line_limited(reader: &mut impl BufRead, maximum: usize) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let size = reader
        .take(maximum as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    if size == 0 {
        return Ok(None);
    }
    if size > maximum {
        bail!("JSON message exceeds {maximum} bytes");
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(bytes))
}
fn write_json(writer: &mut impl Write, value: &Value, maximum: usize) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > maximum {
        bail!("JSON response exceeds {maximum} bytes; use smaller library pages");
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn run_mcp_stdio() -> Result<()> {
    let path = manifest_path()?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_stdio(stdin.lock(), stdout.lock(), |method, params| {
        forward_at(&path, method, params)
    })
}
fn run_stdio(
    mut input: impl BufRead,
    mut output: impl Write,
    mut dispatch: impl FnMut(&str, Value) -> AppReply,
) -> Result<()> {
    let mut session = McpSession::default();
    loop {
        let line = match read_line_limited(&mut input, INPUT_LIMIT) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_json(
                    &mut output,
                    &rpc_error(Value::Null, -32700, &error.to_string()),
                    RESPONSE_LIMIT,
                )?;
                return Ok(());
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = match serde_json::from_slice(&line) {
            Ok(value) => session.handle(value, &mut dispatch),
            Err(_) => Some(rpc_error(Value::Null, -32700, "Invalid JSON")),
        };
        if let Some(response) = response {
            write_json(&mut output, &response, RESPONSE_LIMIT * 2)?;
        }
    }
}
#[derive(Default)]
struct McpSession {
    initialized: bool,
}
impl McpSession {
    fn handle(
        &mut self,
        request: Value,
        dispatch: &mut impl FnMut(&str, Value) -> AppReply,
    ) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(rpc_error(
                Value::Null,
                -32600,
                "Expected a JSON-RPC request object",
            ));
        };
        if object.get("jsonrpc") != Some(&json!("2.0")) {
            return Some(rpc_error(Value::Null, -32600, "jsonrpc must be 2.0"));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // No server-initiated requests exist; unsolicited client responses are ignored.
            if object.contains_key("result") || object.contains_key("error") {
                return None;
            }
            return Some(rpc_error(Value::Null, -32600, "Request method is missing"));
        };
        let Some(id) = object.get("id") else {
            return None;
        };
        if !id.is_string() && id.as_i64().is_none() && id.as_u64().is_none() {
            return Some(rpc_error(
                Value::Null,
                -32600,
                "Request ID must be a string or integer",
            ));
        }
        let id = id.clone();
        let params = object.get("params").cloned().unwrap_or_else(empty_object);
        if !params.is_object() {
            return Some(rpc_error(id, -32602, "params must be an object"));
        }
        let result = match method {
            "initialize" => {
                if self.initialized {
                    return Some(rpc_error(id, -32600, "Session is already initialized"));
                }
                let Some(version) = params.get("protocolVersion").and_then(Value::as_str) else {
                    return Some(rpc_error(id, -32602, "initialize requires protocolVersion"));
                };
                if !params.get("capabilities").is_some_and(Value::is_object)
                    || !params.get("clientInfo").is_some_and(Value::is_object)
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "initialize requires capabilities and clientInfo objects",
                    ));
                }
                let version = if PROTOCOLS.contains(&version) {
                    version
                } else {
                    PROTOCOLS[0]
                };
                self.initialized = true;
                json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"shadowroom","version":env!("CARGO_PKG_VERSION")},"instructions":"Controls the running native Shadowroom desktop app. Open the app before invoking tools. Import/export calls enqueue background jobs; inspect workspace_state for progress and pending saves. Tools never move or delete originals; the desktop offers separately confirmed file deletion."})
            }
            "ping" => json!({}),
            _ if !self.initialized => {
                return Some(rpc_error(id, -32002, "Initialize the MCP session first"));
            }
            "tools/list" => {
                if params
                    .get("cursor")
                    .is_some_and(|cursor| cursor.as_str() != Some(""))
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Unknown tool-list cursor; all tools fit in one page",
                    ));
                }
                json!({"tools":tool_definitions()})
            }
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(rpc_error(id, -32602, "tools/call requires a tool name"));
                };
                if !cached_tool_definitions()
                    .iter()
                    .any(|tool| tool["name"] == name)
                {
                    return Some(rpc_error(id, -32602, &format!("Unknown tool: {name}")));
                }
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(empty_object);
                if !arguments.is_object() {
                    return Some(rpc_error(id, -32602, "Tool arguments must be an object"));
                }
                match normalize_arguments(name, arguments)
                    .and_then(|arguments| dispatch(name, arguments))
                {
                    Ok(value) => {
                        let text = serde_json::to_string(&value).unwrap_or_default();
                        let structured = if value.is_object() {
                            value
                        } else {
                            Value::Object(serde_json::Map::from_iter([("value".into(), value)]))
                        };
                        Value::Object(serde_json::Map::from_iter([
                            ("content".into(), json!([{"type":"text","text":text}])),
                            ("structuredContent".into(), structured),
                            ("isError".into(), Value::Bool(false)),
                        ]))
                    }
                    Err(message) => {
                        json!({"content":[{"type":"text","text":message}],"isError":true})
                    }
                }
            }
            _ => return Some(rpc_error(id, -32601, &format!("Unknown method: {method}"))),
        };
        Some(Value::Object(serde_json::Map::from_iter([
            ("jsonrpc".into(), Value::String("2.0".into())),
            ("id".into(), id),
            ("result".into(), result),
        ])))
    }
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// Public for CLI discovery and integration checks. Schemas are also enforced at IPC ingress.
pub fn tool_definitions() -> Vec<Value> {
    cached_tool_definitions().to_vec()
}
fn cached_tool_definitions() -> &'static [Value] {
    static TOOLS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(build_tool_definitions)
}
fn build_tool_definitions() -> Vec<Value> {
    let id = json!({"type":"integer","minimum":1});
    let ids = json!({"type":"array","items":id,"minItems":1,"maxItems":1000});
    let path = json!({"type":"string","minLength":1,"maxLength":4096});
    let modes = json!({"type":"string","enum":["off","time","visual","burst"]});
    let rating = json!({"type":"integer","minimum":0,"maximum":5});
    let mut settings = json!({"type":"object","additionalProperties":false,"properties":{
        "interface_font":{"type":"string","minLength":1,"maxLength":128},"theme":{"type":"string","minLength":1,"maxLength":256},
        "theme_mode":{"type":"string","enum":["manual","system","zed"]},"theme_light":{"type":"string","minLength":1,"maxLength":256},"theme_dark":{"type":"string","minLength":1,"maxLength":256},
        "grouping_mode":modes,"burst_gap_ms":{"type":"integer","minimum":1,"maximum":3600000},"visual_threshold":{"type":"integer","minimum":0,"maximum":64},
        "remote_cache_budget_mb":{"type":"integer","minimum":0,"maximum":1048576},"auto_advance":{"type":"boolean"},"auto_discover_drives":{"type":"boolean"},"cache_budget_mb":{"type":"integer","minimum":128,"maximum":1048576},"memory_budget_mb":{"anyOf":[{"type":"integer","enum":[0]},{"type":"integer","minimum":64,"maximum":65536}]},"show_getting_started":{"type":"boolean"},"trash_mode":{"type":"string","enum":["system","in_app"]},
        "last_photo_id":{"type":["integer","null"],"minimum":1},"view_mode":{"type":"string","enum":["grid","loupe","compare","survey"]},"show_inspector":{"type":"boolean"},"show_sidebar":{"type":"boolean"},"sidebar_width":{"type":"integer","minimum":180,"maximum":480},"inspector_width":{"type":"integer","minimum":240,"maximum":480},"min_rating":rating,"filter":{"type":"string","enum":["all","picks","unrated","rejects","trash"]}
    }});
    let ingest_options = json!({"type":"object","additionalProperties":false,"properties":{
        "library_root":path,"folder_template":{"type":"string","maxLength":4096},"file_template":path,
        "backup_roots":{"type":"array","items":path,"maxItems":16},"job_code":{"type":"string","maxLength":256},
        "day_start_hour":{"type":"integer","minimum":0,"maximum":23},"collision":{"type":"string","enum":["suffix","skip"]},
        "include_videos":{"type":"boolean"},"selection":{"type":"string","enum":["all","picks","except_rejects","rated"]},
        "min_rating":rating,"selected_ids":{"type":"array","items":{"type":"integer","minimum":0},"maxItems":100000}
    }});
    let import_settings = json!({
        "ingest_on_insert":{"type":"string","enum":["ask","auto","index_in_place","ignore"]},
        "ingest_library_root":path,"ingest_folder_template":{"type":"string","maxLength":4096},"ingest_file_template":path,
        "ingest_backup_roots":{"type":"array","items":path,"maxItems":16},"ingest_job_codes":{"type":"array","items":{"type":"string"},"maxItems":20},
        "ingest_day_start_hour":{"type":"integer","minimum":0,"maximum":23},"ingest_collision":{"type":"string","enum":["suffix","skip"]},
        "ingest_time_group_minutes":{"type":"integer","minimum":1,"maximum":1440},"eject_after_ingest":{"type":"boolean"},
        "ingest_include_videos":{"type":"boolean"},"ingest_volumes":{"type":"object"},"ingest_presets":{"type":"object"}
    });
    settings["properties"]
        .as_object_mut()
        .expect("settings schema")
        .extend(
            import_settings
                .as_object()
                .expect("import settings")
                .clone(),
        );
    let definitions = vec![
        tool(
            "ingest_scan",
            "Scan a folder/card read-only on a worker; poll ingest_status for batches and completion.",
            json!({"path":path}),
            &["path"],
            false,
        ),
        tool(
            "ingest_plan",
            "Compute names, selection, live example and filesystem capacity on a worker. Poll ingest_status for the resulting plan.",
            json!({"options":ingest_options}),
            &["options"],
            false,
        ),
        tool(
            "ingest_start",
            "Start the prepared verified import; poll ingest_status for progress.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "ingest_status",
            "Read cached scan, plan, progress and diagnostics without disk access. Scan items are paginated.",
            json!({"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000}}),
            &[],
            true,
        ),
        tool(
            "ingest_pause",
            "Pause copying at chunk boundaries.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "ingest_resume",
            "Resume a paused import.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "ingest_cancel",
            "Cancel the import/scan; keep committed groups and remove incomplete staging files.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "ingest_history",
            "Request durable import jobs and group history from the IO worker. Poll ingest_status.history.",
            json!({"limit":{"type":"integer","minimum":1,"maximum":1000}}),
            &[],
            true,
        ),
        tool(
            "ingest_mark_imported",
            "Mark explicit scan group IDs imported by another tool without copying or writing the source.",
            json!({"ids":{"type":"array","items":{"type":"integer","minimum":0},"minItems":1,"maxItems":100000}}),
            &["ids"],
            false,
        ),
        tool(
            "eject",
            "Explicitly eject a discovered removable mount root. Folder/test paths return unsupported without invoking system eject.",
            json!({"path":path}),
            &["path"],
            false,
        ),
        tool(
            "remote_host_add",
            "Persist an SSH host and check the agent protocol/hello on a background actor. Poll remote_status for completion. local_program/local_args explicitly select a local child for protocol tests.",
            json!({"id":path,"display_name":path,"ssh_alias":path,"remote_command":path,"extra_ssh_args":{"type":"array","items":{"type":"string"},"maxItems":64},"local_program":path,"local_args":{"type":"array","items":{"type":"string"},"maxItems":64}}),
            &["id", "ssh_alias"],
            false,
        ),
        tool(
            "remote_host_remove",
            "Remove host configuration and disconnect. Catalog metadata, decisions and cached previews remain.",
            json!({"host":path}),
            &["host"],
            false,
        ),
        tool(
            "remote_hosts",
            "List persisted hosts, hello identities, states, roots, progress and diagnostics.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "remote_status",
            "Report connection/index/sync state and diagnostic for a host or all hosts.",
            json!({"host":path}),
            &[],
            true,
        ),
        tool(
            "remote_connect",
            "Connect/reconnect using the configured transport and check protocol/hello. Does not scan originals.",
            json!({"host":path}),
            &["host"],
            false,
        ),
        tool(
            "remote_disconnect",
            "Cancel active host work and disconnect. Cached previews and culling decisions remain available.",
            json!({"host":path}),
            &["host"],
            false,
        ),
        tool(
            "remote_cancel",
            "Cancel the host's active index or metadata sync, preserving the previous revision cursor.",
            json!({"host":path}),
            &["host"],
            false,
        ),
        tool(
            "remote_roots",
            "List a host's last reported roots/volumes and their library filter scope strings. Reconnect to refresh mounts.",
            json!({"host":path}),
            &["host"],
            true,
        ),
        tool(
            "remote_index",
            "Index an allowlisted remote root, then sync metadata and tombstones into the local catalog. Poll remote_status; originals remain remote.",
            json!({"host":path,"root":path}),
            &["host", "root"],
            false,
        ),
        tool(
            "remote_sync",
            "Refresh indexed metadata without filesystem scanning. full=true requests an atomic full snapshot, including recovery from a revision-ahead error.",
            json!({"host":path,"root":path,"full":{"type":"boolean"}}),
            &["host", "root"],
            false,
        ),
        tool(
            "workspace_state",
            "Read current view, selection, filters, grouping, pending saves, import/export status and errors.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "library_list",
            "List catalog photographs with bounded pagination. Use scope=all for the whole catalog or filtered for current visible results.",
            json!({"offset":{"type":"integer","minimum":0,"default":0},"limit":{"type":"integer","minimum":1,"maximum":1000,"default":200},"scope":{"type":"string","enum":["all","filtered"],"default":"filtered"}}),
            &[],
            true,
        ),
        tool(
            "photo_get",
            "Read complete catalog metadata and current decisions for one photograph.",
            json!({"id":id}),
            &["id"],
            true,
        ),
        tool(
            "select",
            "Select a photograph by ID/index, or an explicit group of IDs. At least one of id, ids, index is required; index refers to filtered results.",
            json!({"id":id,"ids":ids,"index":{"type":"integer","minimum":0},"extend":{"type":"boolean"}}),
            &[],
            false,
        ),
        tool(
            "rate",
            "Set stars for explicit IDs or the current selection; zero clears stars.",
            json!({"rating":rating,"ids":ids}),
            &["rating"],
            false,
        ),
        tool(
            "flag",
            "Set a pick/reject flag for explicit IDs or the current selection. No original is deleted.",
            json!({"flag":{"type":"string","enum":["none","pick","reject"]},"ids":ids}),
            &["flag"],
            false,
        ),
        tool(
            "label",
            "Set a color label for explicit IDs or the current selection.",
            json!({"label":{"type":"string","enum":["none","red","yellow","green","blue","purple"]},"ids":ids}),
            &["label"],
            false,
        ),
        tool(
            "trash",
            "Set restorable in-app Trash for explicit IDs or the selection. trashed=false restores; original files are never moved or deleted. Inspect decision persistence.",
            json!({"ids":ids,"trashed":{"type":"boolean"}}),
            &["trashed"],
            false,
        ),
        tool(
            "duplicates",
            "Scan exact duplicate bytes for explicit IDs or the nontrashed catalog, cancel a scan, or read its status. Scanning is read-only and opt-in; no automatic trash.",
            json!({"action":{"type":"string","enum":["scan","cancel","status"]},"ids":ids}),
            &["action"],
            false,
        ),
        tool(
            "undo",
            "Undo the most recent decision change.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "redo",
            "Redo the most recently undone decision change.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "view",
            "Switch the native workspace view.",
            json!({"mode":{"type":"string","enum":["grid","loupe","compare","survey"]},"panel":{"type":"string","enum":["review","info"]}}),
            &["mode"],
            false,
        ),
        tool(
            "zoom",
            "Set Fit, 100%, custom magnification, or step in/out. Inspection prefers paired or embedded JPEGs; sources remain unchanged.",
            json!({"mode":{"type":"string","enum":["fit","full","toggle","in","out","custom"]},"percent":{"type":"number","minimum":12.5,"maximum":800}}),
            &["mode"],
            false,
        ),
        tool(
            "pin",
            "Pin a photograph as comparison reference; omit ID for the current photograph.",
            json!({"id":id}),
            &[],
            false,
        ),
        tool(
            "filter",
            "Update library filter, minimum stars or folder scope; clear_folder removes folder scope.",
            json!({"filter":{"type":"string","enum":["all","picks","unrated","rejects","trash"]},"duplicates_only":{"type":"boolean"},"min_rating":rating,"folder":path,"clear_folder":{"type":"boolean"}}),
            &[],
            false,
        ),
        tool(
            "group",
            "Choose temporal/visual grouping mode and optional thresholds.",
            json!({"mode":modes,"gap_ms":{"type":"integer","minimum":1,"maximum":3600000},"threshold":{"type":"integer","minimum":0,"maximum":64}}),
            &["mode"],
            false,
        ),
        tool(
            "settings_get",
            "Read current application preferences.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "settings_set",
            "Apply partial validated preferences to the running app and persist them.",
            json!({"settings":settings}),
            &["settings"],
            false,
        ),
        tool(
            "import",
            "Enqueue read-only indexing of a folder or whole selected drive; returns acceptance, not completion. Poll workspace_state.",
            json!({"path":path}),
            &["path"],
            false,
        ),
        tool(
            "export_xmp",
            "Enqueue sidecar export for IDs or current selection; originals remain untouched and destination files are never overwritten. Poll workspace_state.",
            json!({"destination":path,"ids":ids}),
            &["destination"],
            false,
        ),
        tool(
            "export_picks",
            "Enqueue copying current picked originals and RAW/JPEG companions without overwriting destination files. Poll workspace_state.",
            json!({"destination":path}),
            &["destination"],
            false,
        ),
        tool(
            "cancel_import",
            "Request cancellation of active and queued imports while preserving indexed data.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "theme_import",
            "Import compatible Zed theme JSON from a local path.",
            json!({"path":path}),
            &["path"],
            false,
        ),
        tool(
            "cache_stats",
            "Read preview queue, memory/cache usage and source/decode counters.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "command",
            "Execute an allowlisted workspace action; this is not a shell or arbitrary-command interface.",
            json!({"name":{"type":"string","enum":["next","previous","next_group","previous_group","toggle_histogram","toggle_inspector","toggle_sidebar","toggle_auto_advance","toggle_focus","select_all","refresh","retry_saves","retry_preview","toggle_histogram_log","toggle_compare_sync","next_unreviewed","previous_unreviewed","resolve_burst","show_duplicates","scan_duplicates_in_view","toggle_palette","toggle_preferences","open_folder_picker","open_theme_picker","show_appearance","show_rating_filter","reset_layout","resize_catalog","resize_inspector","close_overlay","getting_started","toggle_focus_peaking","show_full_folder","reset_decision"]}}),
            &["name"],
            false,
        ),
        tool(
            "quit",
            "Request graceful application quit after pending decisions are handled.",
            json!({}),
            &[],
            false,
        ),
    ];
    definitions
}
fn tool(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read_only,"destructiveHint":false,"openWorldHint":false}})
}
fn normalize_arguments(name: &str, mut params: Value) -> std::result::Result<Value, String> {
    let tools = cached_tool_definitions();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| format!("Unknown desktop method: {name}"))?;
    validate_schema(&params, &tool["inputSchema"], "arguments")?;
    if name == "duplicates" && params["action"] != "scan" && params.get("ids").is_some() {
        return Err("duplicates.ids is only valid for action=scan".into());
    }
    if name == "select"
        && !["id", "ids", "index"]
            .iter()
            .any(|key| params.get(key).is_some())
    {
        return Err("select requires id, ids or index".into());
    }
    if name == "library_list" {
        let map = params
            .as_object_mut()
            .ok_or("arguments must be an object")?;
        map.entry("offset").or_insert(json!(0));
        map.entry("limit").or_insert(json!(200));
        map.entry("scope").or_insert(json!("filtered"));
    }
    Ok(params)
}
fn validate_schema(
    value: &Value,
    schema: &Value,
    location: &str,
) -> std::result::Result<(), String> {
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        if !options
            .iter()
            .any(|option| validate_schema(value, option, location).is_ok())
        {
            return Err(format!("{location}: outside allowed alternatives"));
        }
    }
    let matches_type = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };
    let valid_type = match schema.get("type") {
        Some(Value::String(kind)) => matches_type(kind),
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).any(matches_type),
        _ => true,
    };
    if !valid_type {
        return Err(format!("{location}: incorrect value type"));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            return Err(format!("{location}: unsupported value {value}"));
        }
    }
    if let Some(number) = value.as_f64() {
        for (key, valid) in [("minimum", true), ("maximum", false)] {
            if let Some(bound) = schema.get(key).and_then(Value::as_f64) {
                if (valid && number < bound) || (!valid && number > bound) {
                    return Err(format!("{location}: outside allowed {key} {bound}"));
                }
            }
        }
    }
    if let Some(text) = value.as_str() {
        let len = text.chars().count();
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|n| len < n as usize)
            || schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|n| len > n as usize)
        {
            return Err(format!("{location}: text length outside limits"));
        }
    }
    if let Some(items) = value.as_array() {
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|n| items.len() < n as usize)
            || schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|n| items.len() > n as usize)
        {
            return Err(format!("{location}: item count outside limits"));
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate_schema(item, item_schema, &format!("{location}[{index}]"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{location}: missing {key}"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(child_schema) => {
                    validate_schema(child, child_schema, &format!("{location}.{key}"))?
                }
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{location}: unknown field {key}"));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn initialize(version: &str) -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
    }
    #[test]
    #[ignore]
    fn bench_automation_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automation.json");
        let server = start_server_at(path.clone()).unwrap();
        let requests = server.requests.clone();
        let handler = thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                let response = if request.method == "library_list" {
                    json!({"photos":(0..1000).map(|id|json!({"id":id,"path":format!("/photos/folder/DSC{id:05}.nef"),"rating":0,"flag":"None","metadata":{"camera":"Example camera","lens":"24-70mm"}})).collect::<Vec<_>>()})
                } else {
                    json!({"count":100_000,"selected":1})
                };
                let _ = request.reply.send(Ok(response));
            }
        });
        for (method, params) in [
            ("workspace_state", json!({})),
            ("library_list", json!({"limit":1000})),
        ] {
            let began = Instant::now();
            for _ in 0..30 {
                std::hint::black_box(forward_at(&path, method, params.clone()).unwrap());
            }
            eprintln!("bridge {method} mean {:?}", began.elapsed() / 30);
        }
        drop(server);
        handler.join().unwrap();
    }
    #[test]
    fn protocol_negotiation_notifications_and_full_results() {
        let mut session = McpSession::default();
        let mut calls = 0;
        let mut dispatch = |_: &str, params: Value| {
            calls += 1;
            Ok(json!({"nested":{"values":[1,2,3]},"params":params}))
        };
        assert_eq!(
            session
                .handle(initialize("2025-06-18"), &mut dispatch)
                .unwrap()["result"]["protocolVersion"],
            "2025-06-18"
        );
        assert!(
            session
                .handle(
                    json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                    &mut dispatch
                )
                .is_none()
        );
        let result=session.handle(json!({"jsonrpc":"2.0","id":"call","method":"tools/call","params":{"name":"library_list","arguments":{}}}),&mut dispatch).unwrap();
        assert_eq!(
            result["result"]["structuredContent"]["nested"]["values"],
            json!([1, 2, 3])
        );
        assert_eq!(
            result["result"]["structuredContent"]["params"]["limit"],
            200
        );
        let text = result["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            result["result"]["structuredContent"]
        );
        assert_eq!(calls, 1);
    }
    #[test]
    fn stdio_is_json_only_and_recovers_parse_error() {
        let input = format!(
            "bad json\n{}\n{}\n{}\n",
            initialize("unsupported"),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        );
        let mut output = Vec::new();
        run_stdio(io::Cursor::new(input), &mut output, |_, _| {
            panic!("Tool list must not access desktop")
        })
        .unwrap();
        let lines: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["error"]["code"], -32700);
        assert_eq!(lines[1]["result"]["protocolVersion"], PROTOCOLS[0]);
        assert_eq!(lines[2]["result"]["tools"].as_array().unwrap().len(), 46);
    }
    #[test]
    fn argument_limits_and_unknown_commands_are_enforced() {
        assert!(
            normalize_arguments(
                "settings_set",
                json!({"settings":{"memory_budget_mb":0,"show_getting_started":false}})
            )
            .is_ok()
        );
        assert!(normalize_arguments("zoom", json!({"mode":"custom","percent":175})).is_ok());
        assert!(normalize_arguments("zoom", json!({"mode":"custom","percent":900})).is_err());
        assert!(normalize_arguments("library_list", json!({"limit":1001})).is_err());
        assert!(normalize_arguments("rate", json!({"rating":6})).is_err());
        assert!(normalize_arguments("select", json!({})).is_err());
        assert!(normalize_arguments("trash", json!({"trashed":"true"})).is_err());
        assert!(normalize_arguments("trash", json!({"trashed":false,"ids":[1]})).is_ok());
        assert!(normalize_arguments("duplicates", json!({"action":"erase"})).is_err());
        assert!(normalize_arguments("duplicates", json!({"action":"status","ids":[1]})).is_err());
        assert!(normalize_arguments("command", json!({"name":"rm -rf /"})).is_err());
        assert!(
            normalize_arguments("settings_set", json!({"settings":{"memory_budget_mb":1}}))
                .is_err()
        );
        assert!(
            normalize_arguments("settings_set", json!({"settings":{"last_photo_id":null}})).is_ok()
        );
        let mut reader = io::Cursor::new(vec![b'a'; 33]);
        assert!(read_line_limited(&mut reader, 32).is_err());
    }
    #[test]
    fn recovery_histogram_compare_and_theme_modes_are_exposed() {
        for name in ["retry_saves", "toggle_histogram_log", "toggle_compare_sync"] {
            assert!(normalize_arguments("command", json!({"name":name})).is_ok());
        }
        for mode in ["manual", "system", "zed"] {
            assert!(
                normalize_arguments(
                    "settings_set",
                    json!({"settings":{"theme_mode":mode,"theme_light":"Day","theme_dark":"Night"}})
                )
                .is_ok()
            );
        }
        assert!(
            normalize_arguments("settings_set", json!({"settings":{"theme_mode":"unknown"}}))
                .is_err()
        );
        assert_eq!(tool_definitions().len(), 46);
    }
    #[test]
    fn inherited_nonblocking_socket_handles_partial_input_and_large_reply() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.set_nodelay(true).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let (stream, _) = listener.accept().unwrap();
        // Reproduce Darwin's inherited flag on every test platform.
        stream.set_nonblocking(true).unwrap();
        let (tx, rx) = bounded(1);
        let server = thread::spawn(move || {
            handle_connection_with_timeout(
                stream,
                "test-token",
                &tx,
                &AtomicBool::new(false),
                Duration::from_secs(2),
            )
        });
        let payload_size = 7 * 1024 * 1024;
        let app = thread::spawn(move || {
            let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            request
                .reply
                .send(Ok(json!({"payload":"x".repeat(payload_size)})))
                .unwrap();
        });
        let request = serde_json::to_vec(
            &json!({"token":"test-token","method":"workspace_state","params":{}}),
        )
        .unwrap();
        let middle = request.len() / 2;
        client.write_all(&request[..middle]).unwrap();
        thread::sleep(Duration::from_millis(30));
        client.write_all(&request[middle..]).unwrap();
        client.write_all(b"\n").unwrap();
        // Delay the reader so the response cannot rely on a perpetually empty
        // kernel send buffer. The result still fits the documented frame cap.
        thread::sleep(Duration::from_millis(100));
        let line = read_line_limited(&mut BufReader::new(client), RESPONSE_LIMIT)
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(
            value["result"]["payload"].as_str().unwrap().len(),
            payload_size
        );
        app.join().unwrap();
        server.join().unwrap().unwrap();
    }
    #[test]
    fn tcp_bridge_authenticates_preserves_results_and_cleans_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automation.json");
        let server = start_server_at(path.clone()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let manifest = read_manifest(&path).unwrap();
        assert_eq!(manifest.address.ip(), Ipv4Addr::LOCALHOST);
        let requests = server.requests.clone();
        let handler = thread::spawn(move || {
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(request.method, "photo_get");
            request
                .reply
                .send(Ok(
                    json!({"id":7,"metadata":{"camera":"Test"},"flags":["pick"]}),
                ))
                .unwrap();
        });
        let result = forward_at(&path, "photo_get", json!({"id":7})).unwrap();
        assert_eq!(result["metadata"]["camera"], "Test");
        handler.join().unwrap();
        let mut stream = TcpStream::connect(manifest.address).unwrap();
        write_json(
            &mut stream,
            &json!({"token":"invalid","method":"quit","params":{}}),
            INPUT_LIMIT,
        )
        .unwrap();
        let response = read_line_limited(&mut BufReader::new(stream), RESPONSE_LIMIT)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&response).unwrap()["error"],
            "Authentication failed"
        );
        assert!(server.requests.is_empty());
        drop(server);
        assert!(!path.exists());
        assert!(
            forward_at(&path, "workspace_state", json!({}))
                .unwrap_err()
                .contains("not running")
        );
    }
    #[test]
    fn partial_manifest_is_preserved_and_next_launch_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automation.json");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path).unwrap().write_all(b"{partial").unwrap();
        let server = start_server_at(path.clone()).unwrap();
        assert_eq!(read_manifest(&path).unwrap().version, 1);
        let quarantines: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("automation.invalid-")
            })
            .collect();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(fs::read(quarantines[0].path()).unwrap(), b"{partial");
        drop(server);
        assert!(!path.exists());
        let next = start_server_at(path.clone()).unwrap();
        drop(next);
        assert!(!path.exists());
    }
    #[test]
    fn absolute_frame_deadline_stops_slow_trickle() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let writer = thread::spawn(move || {
            for _ in 0..30 {
                if client.write_all(b"a").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let started = Instant::now();
        let mut input = BufReader::new(DeadlineStream {
            stream,
            deadline: started + Duration::from_millis(35),
        });
        assert!(read_line_limited(&mut input, INPUT_LIMIT).is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(input);
        writer.join().unwrap();
    }
    #[test]
    fn authenticated_queued_mutation_has_expiry_and_is_not_applied_late() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let (tx, rx) = bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            handle_connection_with_timeout(
                stream,
                "test-token",
                &tx,
                &stopped,
                Duration::from_millis(20),
            )
            .unwrap()
        });
        write_json(&mut client,&json!({"token":"test-token","method":"rate","params":{"rating":5},"request_id":"late-test"}),INPUT_LIMIT).unwrap();
        let request = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(request.request_id, "late-test");
        thread::sleep(Duration::from_millis(35));
        assert!(request.is_expired());
        let mut applied = false;
        if request.is_expired() {
            let _ = request
                .reply
                .send(Err("Expired request was not applied".into()));
        } else {
            applied = true;
            let _ = request.reply.send(Ok(json!({})));
        }
        let response = read_line_limited(&mut BufReader::new(client), RESPONSE_LIMIT)
            .unwrap()
            .unwrap();
        assert!(
            serde_json::from_slice::<Value>(&response)
                .unwrap()
                .get("error")
                .is_some()
        );
        assert!(!applied);
        worker.join().unwrap();
    }
    #[test]
    fn expired_accepted_connection_cannot_enqueue_a_buffered_mutation() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        // Even a complete authenticated frame already waiting in the socket may
        // not acquire a fresh deadline after spending too long in admission.
        write_json(&mut client,&json!({"token":"test-token","method":"rate","params":{"rating":5},"request_id":"expired-before-worker"}),INPUT_LIMIT).unwrap();
        let (requests, received) = bounded(1);
        let stop = AtomicBool::new(false);
        let accepted_at = Instant::now() - FRAME_TIMEOUT - Duration::from_millis(1);
        let started = Instant::now();
        // An unread expired socket may reset during its bounded error response;
        // the safety invariant is independent of whether that response arrives.
        let _ = handle_connection_at(
            stream,
            "test-token",
            &requests,
            &stop,
            accepted_at,
            Duration::from_millis(20),
        );
        assert!(
            received.is_empty(),
            "expired connection reached the UI mutation queue"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }
    #[test]
    fn live_second_instance_refused_and_replacement_manifest_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automation.json");
        let server = start_server_at(path.clone()).unwrap();
        assert!(start_server_at(path.clone()).is_err());
        let mut manifest = read_manifest(&path).unwrap();
        manifest.token = "a".repeat(64);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        drop(server);
        assert!(path.exists());
    }
}
