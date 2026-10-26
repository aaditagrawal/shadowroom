use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}
impl Flag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pick => "pick",
            Self::Reject => "reject",
        }
    }
}
impl From<&str> for Flag {
    fn from(s: &str) -> Self {
        match s {
            "pick" => Self::Pick,
            "reject" => Self::Reject,
            _ => Self::None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorLabel {
    #[default]
    None,
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}
impl ColorLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Red => "Red",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
        }
    }
}
impl From<&str> for ColorLabel {
    fn from(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "red" => Self::Red,
            "yellow" => Self::Yellow,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "purple" => Self::Purple,
            _ => Self::None,
        }
    }
}

/// Local paths remain on Photo for compatibility with existing catalogs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PhotoSource {
    #[default]
    Local,
    Remote(RemoteSource),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteSource {
    pub device_id: String,
    pub root_id: String,
    pub volume_id: String,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub modified_at: i64,
    pub companion_path: Option<PathBuf>,
    pub companion_size_bytes: u64,
    pub companion_modified_at: i64,
    #[serde(default = "source_available")]
    pub available: bool,
}
fn source_available() -> bool {
    true
}
impl RemoteSource {
    pub fn key(&self) -> crate::remote::protocol::PhotoKey {
        crate::remote::protocol::PhotoKey {
            root_id: self.root_id.clone(),
            volume_id: self.volume_id.clone(),
            relative_path: self.relative_path.clone(),
            size_bytes: self.size_bytes,
            modified_at: self.modified_at,
            companion_path: self.companion_path.clone(),
            companion_size_bytes: self.companion_size_bytes,
            companion_modified_at: self.companion_modified_at,
        }
    }
    pub fn root_scope(&self) -> String {
        remote_root_scope(&self.device_id, &self.root_id)
    }
}
pub fn remote_root_scope(device: &str, root: &str) -> String {
    format!(
        "remote-root:{}",
        blake3::hash(&serde_json::to_vec(&(device, root)).expect("identity serialization"))
            .to_hex()
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Photo {
    pub source: PhotoSource,
    pub id: i64,
    pub path: PathBuf,
    pub filename: String,
    pub folder: String,
    pub rating: u8,
    pub flag: Flag,
    pub label: ColorLabel,
    /// Restorable app-only Trash; originals remain at their source paths.
    #[serde(default)]
    pub trashed: bool,
    pub width: u32,
    pub height: u32,
    pub captured_at: Option<i64>,
    /// Original EXIF OffsetTimeOriginal, e.g. +05:30; never inferred from this computer.
    pub captured_offset: Option<String>,
    /// `offset` means captured_at is UTC; `camera_local` uses a neutral wall-time timeline.
    pub capture_time_kind: String,
    /// Rebuildable metadata extraction revision; decisions are stored independently.
    pub metadata_version: u32,
    pub camera: String,
    pub lens: String,
    pub iso: Option<u32>,
    pub aperture: Option<f64>,
    pub shutter: String,
    pub focal_length: Option<f64>,
    pub orientation: u32,
    pub size_bytes: u64,
    pub modified_at: i64,
    /// JPEG belonging to a RAW with the same case-insensitive stem in the same folder.
    pub companion_path: Option<PathBuf>,
    pub dhash: Option<u64>,
    pub companion_size_bytes: u64,
    pub companion_modified_at: i64,
}
impl Photo {
    pub fn is_remote(&self) -> bool {
        matches!(self.source, PhotoSource::Remote(_))
    }
    pub fn remote_source(&self) -> Option<&RemoteSource> {
        match &self.source {
            PhotoSource::Remote(source) => Some(source),
            PhotoSource::Local => None,
        }
    }
    pub fn require_local(&self, action: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.is_remote(),
            "{action} is unavailable for remote photos; originals remain on the remote host"
        );
        Ok(())
    }
    pub fn location_display(&self) -> String {
        match self.remote_source() {
            Some(source) => format!(
                "Remote · {} / {} / {}",
                source.device_id,
                source.root_id,
                source.relative_path.display()
            ),
            None => self.path.display().to_string(),
        }
    }
    pub fn folder_key(&self) -> String {
        match self.remote_source() {
            Some(source) => format!("{}/{}", source.root_scope(), self.folder),
            None => self.folder.clone(),
        }
    }
    pub fn matches_scope(&self, scope: &str) -> bool {
        self.remote_source()
            .is_some_and(|s| s.root_scope() == scope)
            || self.folder_key() == scope
    }
    pub fn grouping_key(&self) -> (&str, &str, &str, &str) {
        match self.remote_source() {
            Some(s) => (&s.device_id, &s.root_id, &self.folder, &self.camera),
            None => ("", "", &self.folder, &self.camera),
        }
    }

    pub fn capture_display(&self) -> String {
        let Some(timestamp) = self
            .captured_at
            .and_then(chrono::DateTime::from_timestamp_millis)
        else {
            return "—".into();
        };
        if self.capture_time_kind == "offset" {
            if let Some((text, offset)) = self
                .captured_offset
                .as_deref()
                .and_then(|text| parse_capture_offset(text).map(|offset| (text, offset)))
            {
                return format!(
                    "{} {text}",
                    timestamp
                        .with_timezone(&offset)
                        .format("%Y-%m-%d %H:%M:%S%.3f")
                );
            }
        }
        let qualifier = if self.capture_time_kind == "camera_local" {
            "camera local"
        } else {
            "offset unavailable"
        };
        format!(
            "{} ({qualifier})",
            timestamp.naive_utc().format("%Y-%m-%d %H:%M:%S%.3f")
        )
    }
    pub fn preview_path(&self) -> &std::path::Path {
        self.companion_path.as_deref().unwrap_or(&self.path)
    }
}

pub(crate) fn parse_capture_offset(text: &str) -> Option<chrono::FixedOffset> {
    if text.len() != 6 || !text.is_ascii() {
        return None;
    }
    let bytes = text.as_bytes();
    if !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return None;
    }
    let hour = text[1..3].parse::<i32>().ok()?;
    let minute = text[4..6].parse::<i32>().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    chrono::FixedOffset::east_opt(sign * (hour * 3600 + minute * 60))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoDecision {
    pub id: i64,
    pub rating: u8,
    pub flag: Flag,
    pub label: ColorLabel,
    /// Restorable app-only Trash; originals remain at their source paths.
    #[serde(default)]
    pub trashed: bool,
}
impl From<&Photo> for PhotoDecision {
    fn from(p: &Photo) -> Self {
        Self {
            id: p.id,
            rating: p.rating,
            flag: p.flag,
            label: p.label,
            trashed: p.trashed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_display_preserves_camera_clock_and_offset() {
        let mut photo = Photo {
            captured_at: Some(1704067200500),
            captured_offset: Some("+01:00".into()),
            capture_time_kind: "offset".into(),
            ..Default::default()
        };
        assert_eq!(photo.capture_display(), "2024-01-01 01:00:00.500 +01:00");
        photo.captured_offset = Some("-05:30".into());
        assert_eq!(photo.capture_display(), "2023-12-31 18:30:00.500 -05:30");
        photo.capture_time_kind = "camera_local".into();
        photo.captured_offset = None;
        assert_eq!(
            photo.capture_display(),
            "2024-01-01 00:00:00.500 (camera local)"
        );
        photo.capture_time_kind.clear();
        assert!(photo.capture_display().ends_with("(offset unavailable)"));
        photo.captured_at = None;
        assert_eq!(photo.capture_display(), "—");
    }
}
