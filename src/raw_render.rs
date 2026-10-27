//! Full sensor RAW rendering through the public LibRaw C API.
//!
//! `libraw_data_t` stays opaque. Inspection identifies headers but never unpacks
//! pixels; callers must reserve `RawInfo::working_bytes` before decoding. This
//! is a conservative working-set estimate, not a native allocator sandbox.
//! LibRaw's memory writer applies orientation: NEVER apply EXIF orientation
//! again to the returned image. Thumbnail/fit tiers should use camera previews.
use anyhow::{Context as _, Result, bail};
use image::DynamicImage;
#[cfg(not(feature = "native-codecs"))]
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_SOURCE: usize = 512 * 1024 * 1024;
const FIXED_WORKING: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RawInfo {
    /// Expected output dimensions, including rotation and non-square pixels.
    pub width: u32,
    pub height: u32,
    /// Sensor allocation dimensions, including masked margins.
    pub raw_width: u32,
    pub raw_height: u32,
    /// Additional working set; excludes the caller-owned compressed source.
    /// Includes native buffers, output copies and downstream RGBA conversion.
    pub working_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawWhiteBalance {
    Camera,
    /// Missing/invalid as-shot multipliers: LibRaw's camera daylight defaults.
    DaylightFallback,
}

#[derive(Clone, Debug)]
pub struct RawProvenance {
    pub libraw_version: String,
    pub decoder: String,
    pub white_balance: RawWhiteBalance,
    pub camera_multipliers: Option<[f32; 4]>,
    pub converted_to_srgb: bool,
    pub orientation_applied: bool,
}

pub struct DecodedRaw {
    /// Eight-bit sRGB in display orientation; this is rendered sensor data.
    pub image: DynamicImage,
    pub source_width: u32,
    pub source_height: u32,
    pub provenance: RawProvenance,
}

#[cfg(feature = "native-codecs")]
mod native;
#[cfg(feature = "native-codecs")]
pub use native::{decode, decode_with_cancel, inspect};

/// Reserve this additional metadata allowance before calling `inspect`.
/// Identification may copy profiles/maker notes, although it unpacks no pixels.
/// The compressed source itself must already be accounted for by the caller.
pub fn inspection_working_bytes(source_len: usize) -> Result<usize> {
    validate_source_len(source_len)?;
    source_len
        .checked_mul(2)
        .and_then(|n| n.checked_add(FIXED_WORKING as usize))
        .context("RAW metadata estimate overflow")
}

fn validate_source_len(len: usize) -> Result<()> {
    if len < 16 {
        bail!("Truncated RAW source");
    }
    if len > MAX_SOURCE {
        bail!("RAW source exceeds the 512 MiB safety limit");
    }
    Ok(())
}

#[cfg(not(feature = "native-codecs"))]
pub fn inspect(bytes: &[u8]) -> Result<RawInfo> {
    validate_source_len(bytes.len())?;
    bail!(
        "Full sensor RAW rendering is unavailable in this build: enable the native-codecs feature for LibRaw; embedded camera previews remain supported"
    )
}

#[cfg(not(feature = "native-codecs"))]
pub fn decode(bytes: &[u8], _admitted_working_bytes: usize) -> Result<DecodedRaw> {
    validate_source_len(bytes.len())?;
    bail!(
        "Full sensor RAW rendering is unavailable in this build: enable the native-codecs feature for LibRaw; embedded camera previews remain supported"
    )
}

#[cfg(not(feature = "native-codecs"))]
pub fn decode_with_cancel(
    bytes: &[u8],
    admitted_working_bytes: usize,
    cancel: &AtomicBool,
) -> Result<DecodedRaw> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Sensor RAW rendering cancelled");
    }
    decode(bytes, admitted_working_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_admission_rejects_source_bombs_before_backend_selection() {
        assert!(inspection_working_bytes(0).is_err());
        assert!(inspection_working_bytes(MAX_SOURCE + 1).is_err());
        assert_eq!(
            inspection_working_bytes(1024).unwrap(),
            FIXED_WORKING as usize + 2048
        );
    }

    #[cfg(not(feature = "native-codecs"))]
    #[test]
    fn disabled_sensor_backend_reports_build_capability_and_preserves_cancellation() {
        let bytes = [0; 16];
        for error in [
            inspect(&bytes).err().unwrap(),
            decode(&bytes, usize::MAX).err().unwrap(),
        ] {
            let message = error.to_string();
            assert!(message.contains("unavailable in this build"), "{message}");
            assert!(message.contains("native-codecs"), "{message}");
        }
        assert!(inspect(&[]).unwrap_err().to_string().contains("Truncated"));
        let error = decode_with_cancel(&bytes, 0, &AtomicBool::new(true))
            .err()
            .unwrap();
        assert!(error.to_string().contains("cancelled"));
        let error = decode_with_cancel(&bytes, usize::MAX, &AtomicBool::new(false))
            .err()
            .unwrap();
        assert!(error.to_string().contains("native-codecs"));
    }
}
