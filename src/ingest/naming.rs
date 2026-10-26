//! Pure template expansion. Dates are camera wall time, never the host timezone.
use super::{IngestGroup, IngestOptions, SequenceState};
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use std::path::{Component, Path, PathBuf};

pub const PRESETS: &[(&str, &str, &str)] = &[
    ("Original names", "", "{original}"),
    (
        "Date folders",
        "{YYYY}/{YYYY}-{MM}-{DD}{_job}",
        "{original}",
    ),
    ("Date+job", "{date}{_job}", "{original}"),
    (
        "Date_time_sequence",
        "{date}{_job}",
        "{date}_{time}_{seq_day:3}",
    ),
];

pub fn wall_time(group: &IngestGroup) -> NaiveDateTime {
    let time =
        DateTime::from_timestamp_millis(group.capture_ms).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let offset = group
        .photo
        .captured_offset
        .as_deref()
        .and_then(crate::model::parse_capture_offset)
        .map_or(0, |o| i64::from(o.local_minus_utc()));
    time.naive_utc() + Duration::seconds(offset)
}
pub fn day_key(group: &IngestGroup, hour: u8) -> String {
    (wall_time(group) - Duration::hours(i64::from(hour)))
        .format("%Y-%m-%d")
        .to_string()
}
pub fn letter(mut index: u64) -> String {
    let mut bytes = Vec::new();
    loop {
        bytes.push(b'a' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    bytes.reverse();
    String::from_utf8(bytes).expect("ascii letters")
}
/// Conservative cross-platform component policy, UTF-8 boundary safe.
pub fn sanitize(text: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for ch in text.chars() {
        if ch.is_control()
            || "<>:\"/\\|?*".contains(ch)
            || ch.is_whitespace()
            || ch == '_'
            || ch == '-'
        {
            if !separator && !result.is_empty() {
                result.push(if ch == '_' { '_' } else { '-' });
            }
            separator = true;
        } else {
            result.push(ch);
            separator = false;
        }
    }
    result = result.trim_matches(['-', '_', '.', ' ']).to_string();
    while result.len() > 180 {
        result.pop();
    }
    result = result.trim_end_matches(['-', '_', '.', ' ']).to_string();
    let base = result.split('.').next().unwrap_or("").to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&base.as_str())
    {
        result.insert(0, '_');
    }
    result
}

pub struct Context<'a> {
    pub group: &'a IngestGroup,
    pub options: &'a IngestOptions,
    pub sequences: &'a SequenceState,
    pub session: u64,
    pub letter: u64,
}
impl Context<'_> {
    fn token(&self, token: &str) -> Result<String> {
        let time = wall_time(self.group);
        let format = match token {
            "YYYY" => Some("%Y"),
            "YY" => Some("%y"),
            "MM" => Some("%m"),
            "MMM" => Some("%b"),
            "DD" => Some("%d"),
            "hh" => Some("%H"),
            "mm" => Some("%M"),
            "ss" => Some("%S"),
            "subsec" => Some("%3f"),
            "date" => Some("%Y-%m-%d"),
            "time" => Some("%H%M%S"),
            _ => None,
        };
        if let Some(format) = format {
            return Ok(time.format(format).to_string());
        }
        if let Some(format) = token.strip_prefix("import:") {
            ensure!(!format.is_empty(), "Import date format is empty");
            let converted = format
                .replace("YYYY", "%Y")
                .replace("YY", "%y")
                .replace("MM", "%m")
                .replace("DD", "%d");
            ensure!(
                !converted.contains(['{', '}']) && !format.contains('%'),
                "Invalid import date format"
            );
            let time = DateTime::from_timestamp_millis(self.options.imported_at_ms)
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
            return Ok(time.format(&converted).to_string());
        }
        for (prefix, value) in [
            ("seq_session", self.session + 1),
            ("seq_stored", self.sequences.stored + 1),
            (
                "seq_day",
                self.sequences
                    .days
                    .get(&day_key(self.group, self.options.day_start_hour))
                    .copied()
                    .unwrap_or(0)
                    + 1,
            ),
        ] {
            if token == prefix {
                return Ok(value.to_string());
            }
            if let Some(width) = token.strip_prefix(&format!("{prefix}:")) {
                let width: usize = width.parse()?;
                ensure!((1..=18).contains(&width), "Sequence width must be 1–18");
                return Ok(format!("{value:0width$}"));
            }
        }
        let path = &self.group.primary.path;
        let value = match token {
            "original" => path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            "original_ext" => path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            "ext" => path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase(),
            "camera" => self.group.photo.camera.clone(),
            "make" => self.group.make.clone(),
            "serial" => self.group.serial.clone(),
            "lens" => self.group.photo.lens.clone(),
            "iso" => self
                .group
                .photo
                .iso
                .map_or(String::new(), |iso| iso.to_string()),
            "job" => self.options.job_code.clone(),
            "_job" => {
                if self.options.job_code.trim().is_empty() {
                    String::new()
                } else {
                    format!("_{}", sanitize(&self.options.job_code))
                }
            }
            "letter" => letter(self.letter),
            _ => bail!("Unknown naming token {{{token}}}"),
        };
        // Token values cannot inject a path separator into a folder template.
        Ok(if token == "_job" {
            value
        } else {
            sanitize(&value)
        })
    }
    pub fn expand(&self, template: &str, folder: bool) -> Result<PathBuf> {
        ensure!(template.len() <= 4096, "Template is too long");
        ensure!(
            !template.starts_with(['/', '\\']) && !template.contains('\\'),
            "Template must be relative; use / between folders"
        );
        let mut result = String::new();
        let mut remaining = template;
        while let Some(open) = remaining.find('{') {
            ensure!(!remaining[..open].contains('}'), "Unmatched naming brace");
            result.push_str(&remaining[..open]);
            let close = remaining[open + 1..]
                .find('}')
                .ok_or_else(|| anyhow::anyhow!("Unclosed naming token"))?
                + open
                + 1;
            result.push_str(&self.token(&remaining[open + 1..close])?);
            remaining = &remaining[close + 1..];
        }
        ensure!(!remaining.contains(['{', '}']), "Unmatched naming brace");
        result.push_str(remaining);
        ensure!(
            folder || !result.contains('/'),
            "Filename template cannot contain folders"
        );
        let mut path = PathBuf::new();
        for part in result.split('/') {
            ensure!(
                part != "." && part != "..",
                "Template cannot traverse directories"
            );
            let part = sanitize(part);
            if !part.is_empty() {
                path.push(part);
            }
        }
        ensure!(
            folder || !path.as_os_str().is_empty(),
            "Filename template expands to an empty name"
        );
        ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid destination name"
        );
        Ok(path)
    }
}

/// Suffix is shared by every media/sidecar member, including extension-style XMP.
pub fn member_suffix(source: &Path, group: &IngestGroup) -> String {
    let filename = source.file_name().unwrap_or_default().to_string_lossy();
    let stem = group
        .primary
        .path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    filename
        .get(stem.len()..)
        .filter(|suffix| suffix.starts_with('.'))
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                ".{}",
                source.extension().unwrap_or_default().to_string_lossy()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_component_policy_and_excel_style_letters() {
        assert_eq!(sanitize("  a::b / c__d.. "), "a-b-c_d");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(letter(26), "aa");
        assert_eq!(letter(18277), "zzz");
    }
}
