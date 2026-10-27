//! A frame is u32 LE body length, u32 LE JSON length, JSON envelope, then bytes.
//! Lengths include neither their own prefix nor anything outside that frame.
use crate::{media::PreviewKind, model::Photo};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

pub const PROTOCOL_VERSION: u32 = 1;
// Independent of framing: v4 prefers paired/embedded JPEGs for full inspection.
// Invalidate previous sensor-only Full entries on both client and agent.
pub const PREVIEW_CACHE_VERSION: u32 = 4;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
pub const MAX_HEADER: usize = 256 * 1024;
pub const MAX_PAYLOAD: usize = MAX_FRAME - MAX_HEADER - 4;
pub const MAX_RANGE: u32 = 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub app_version: String,
    pub device_id: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
/// Optional Hello capability, ignored by older clients. Missing means legacy
/// rendering; preserve those agents' existing v4 cache keys and wire framing.
pub const RENDERING_CAPABILITY_PREFIX: &str = "preview_renderer:";
impl Hello {
    pub fn rendering_namespace(&self) -> Option<&str> {
        self.capabilities.iter().find_map(|capability| {
            capability
                .strip_prefix(RENDERING_CAPABILITY_PREFIX)
                .filter(|namespace| !namespace.is_empty())
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteRoot {
    pub id: String,
    pub volume_id: String,
    pub identity_kind: String,
    pub path: PathBuf,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhotoKey {
    pub root_id: String,
    pub volume_id: String,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub modified_at: i64,
    // Paired JPEG changes must invalidate previews even when the RAW is unchanged.
    pub companion_path: Option<PathBuf>,
    pub companion_size_bytes: u64,
    pub companion_modified_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhotoRecord {
    pub key: PhotoKey,
    /// Paths in this metadata are relative to the root, never local GUI paths.
    pub metadata: Photo,
    pub revision: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub scanned: u64,
    pub indexed: u64,
    pub unchanged: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub phase: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IndexSummary {
    pub progress: Progress,
    pub revision: u64,
    pub cancelled: bool,
}
#[derive(Clone, Debug, Default)]
pub struct PhotoSync {
    pub records: Vec<PhotoRecord>,
    pub deleted: Vec<PathBuf>,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol_version: u32,
        app_version: String,
    },
    ListRoots,
    Index {
        root: String,
    },
    /// Compute missing perceptual hashes next to the data so burst grouping is
    /// complete without transferring previews. Capability "analyze".
    Analyze {
        root: String,
    },
    Photos {
        root: String,
        since_revision: u64,
    },
    Preview {
        key: PhotoKey,
        kind: PreviewKind,
        max_bytes: usize,
    },
    ReadRange {
        root: String,
        path: PathBuf,
        offset: u64,
        len: u32,
    },
    Stat {
        root: String,
        path: PathBuf,
    },
    Cancel {
        request_id: u64,
    },
    Ping,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello(Hello),
    Roots {
        roots: Vec<RemoteRoot>,
    },
    Progress(Progress),
    Photo(PhotoRecord),
    Deleted {
        relative_path: PathBuf,
    },
    Synced {
        revision: u64,
    },
    Indexed(IndexSummary),
    Preview {
        width: u32,
        height: u32,
        mime: String,
        #[serde(default)]
        rendered_raw: bool,
        #[serde(default)]
        embedded_raw: bool,
        #[serde(default)]
        rendering_note: Option<String>,
    },
    Range,
    Stat {
        size_bytes: u64,
        modified_at: i64,
        is_dir: bool,
    },
    Cancelled {
        request_id: u64,
    },
    Pong,
    Error {
        code: String,
        message: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: u64,
    pub done: bool,
    pub payload_len: usize,
    pub message: T,
}
#[derive(Clone, Debug)]
pub struct Frame<T> {
    pub header: Envelope<T>,
    pub payload: Vec<u8>,
}
impl<T> Frame<T> {
    pub fn new(id: u64, done: bool, message: T, payload: Vec<u8>) -> Self {
        Self {
            header: Envelope {
                id,
                done,
                payload_len: payload.len(),
                message,
            },
            payload,
        }
    }
}
/// A clean EOF is accepted only between frames. Truncated frames are errors.
pub fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<Option<Frame<T>>> {
    let mut length = [0; 4];
    loop {
        match reader.read(&mut length[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    reader
        .read_exact(&mut length[1..])
        .context("Truncated frame length")?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(
        (4..=MAX_FRAME).contains(&length),
        "Frame length outside bounds"
    );
    let mut json_length = [0; 4];
    reader.read_exact(&mut json_length)?;
    let json_length = u32::from_le_bytes(json_length) as usize;
    ensure!(
        json_length > 0 && json_length <= MAX_HEADER && json_length <= length - 4,
        "Header length outside bounds"
    );
    let mut json = vec![0; json_length];
    reader.read_exact(&mut json)?;
    let header: Envelope<T> = serde_json::from_slice(&json).context("Invalid frame JSON")?;
    ensure!(
        header.payload_len == length - 4 - json_length && header.payload_len <= MAX_PAYLOAD,
        "Payload length mismatch or outside bounds"
    );
    let mut payload = vec![0; header.payload_len];
    reader
        .read_exact(&mut payload)
        .context("Truncated frame payload")?;
    Ok(Some(Frame { header, payload }))
}
pub fn write_frame<T: Serialize>(writer: &mut impl Write, frame: &Frame<T>) -> Result<usize> {
    ensure!(
        frame.header.payload_len == frame.payload.len(),
        "Payload length mismatch"
    );
    let json = serde_json::to_vec(&frame.header)?;
    let length = 4 + json.len() + frame.payload.len();
    if json.is_empty()
        || json.len() > MAX_HEADER
        || length > MAX_FRAME
        || frame.payload.len() > MAX_PAYLOAD
    {
        bail!("Frame exceeds bounds");
    }
    writer.write_all(&(length as u32).to_le_bytes())?;
    writer.write_all(&(json.len() as u32).to_le_bytes())?;
    writer.write_all(&json)?;
    writer.write_all(&frame.payload)?;
    writer.flush()?;
    Ok(length + 4)
}

#[cfg(test)]
mod rendering_tests {
    use super::*;

    #[test]
    fn legacy_hello_and_optional_renderer_capability_share_v1_framing() {
        let mut json = serde_json::json!({
            "protocol_version": 1, "app_version": "0.1.3", "device_id": "device",
            "hostname": "host", "os": "linux", "arch": "x86_64"
        });
        let legacy: Hello = serde_json::from_value(json.clone()).unwrap();
        assert!(legacy.capabilities.is_empty());
        assert_eq!(legacy.rendering_namespace(), None);
        json["capabilities"] = serde_json::json!(["jpeg_preview", "preview_renderer:renderer-a"]);
        let current: Hello = serde_json::from_value(json).unwrap();
        assert_eq!(current.rendering_namespace(), Some("renderer-a"));
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Frame::new(1, true, Response::Hello(current), vec![]),
        )
        .unwrap();
        let frame = read_frame::<Response>(&mut &wire[..]).unwrap().unwrap();
        let Response::Hello(hello) = frame.header.message else {
            panic!("expected hello")
        };
        assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
        assert_eq!(hello.rendering_namespace(), Some("renderer-a"));
    }
}
