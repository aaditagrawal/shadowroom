//! Opt-in Rust HEVC path. Unsupported container features retain native coverage.
//! No native decoder is loaded when this path succeeds.
use super::{sdr_color::convert_nclx, *};
use heifer::heifer_isobmff::{HeifFile, boxes::Property};

pub(super) fn decode(bytes: &[u8], info: &HeifInfo) -> Result<Option<DecodedHeif>> {
    let file = HeifFile::parse(bytes).context("Rust HEIF container inspection")?;
    // Keep alpha, overlays and other derivations on the established backend
    // until fixture coverage establishes parity. In particular, grid-level
    // auxiliary alpha is not handled by heifer 0.2.0's grid decoder.
    if file
        .references
        .iter()
        .any(|reference| matches!(&reference.ref_type.0, b"auxl" | b"prem"))
        || file
            .items
            .iter()
            .any(|item| !matches!(&item.item_type.0, b"hvc1" | b"grid" | b"Exif" | b"mime"))
    {
        return Ok(None);
    }
    let primary = file.item(file.primary_id)?;
    if !matches!(&primary.item_type.0, b"hvc1" | b"grid") {
        return Ok(None);
    }
    // Permit one flat grid of coded tiles, never recursive derivations. This
    // also makes ICC inheritance bounded: heifer follows the first dimg item.
    for item in &file.items {
        if item.item_type.0 == *b"grid" && item.id != file.primary_id {
            return Ok(None);
        }
    }
    for reference in file.references.iter().filter(|r| r.ref_type.0 == *b"dimg") {
        if reference.from != file.primary_id || primary.item_type.0 != *b"grid" {
            return Ok(None);
        }
        for &tile in &reference.to {
            if file.item(tile)?.item_type.0 != *b"hvc1" {
                return Ok(None);
            }
        }
    }
    // Reject all unknown properties, even non-essential ones: some can alter
    // pixel interpretation. Native decoding retains its existing coverage.
    if file
        .properties
        .iter()
        .any(|p| matches!(p, Property::Unknown(..)))
    {
        return Ok(None);
    }
    let icc = file.icc_profile(file.primary_id)?;
    if icc.is_some_and(|profile| profile.len() > MAX_PROFILE) {
        bail!("HEIF ICC profile exceeds safety limit");
    }
    // One tile at a time bounds decoder scratch memory. The bitstream's SPS
    // dimensions are checked inside HEVC before allocating reconstruction
    // buffers; use the already admitted maximum surface, not the global cap.
    let decoded = match heifer::decode_with_options(
        bytes,
        &heifer::Options {
            max_threads: 1,
            max_pixels: info.allocation_pixels.max(u64::from(MIN_CODED_EDGE).pow(2)),
        },
    ) {
        Ok(decoded) => decoded,
        Err(error @ heifer::Error::Unsupported(_))
        | Err(error @ heifer::Error::Hevc(heifer::heifer_hevc_dec::Error::Unimplemented(_))) => {
            #[cfg(test)]
            eprintln!("Rust HEIF fallback: {error}");
            let _ = error;
            return Ok(None);
        }
        Err(error) => return Err(error).context("Rust HEIF decoding"),
    };
    if (decoded.width, decoded.height) != (info.width, info.height) {
        bail!("Rust HEIF dimensions differ from admitted header dimensions");
    }
    if !(8..=16).contains(&decoded.bit_depth) || decoded.bit_depth > info.bit_depth {
        bail!("Rust HEIF sample depth exceeds admitted header depth");
    }
    let maximum = (1u32 << decoded.bit_depth) - 1;
    if decoded
        .data
        .iter()
        .any(|&sample| u32::from(sample) > maximum)
    {
        bail!("Rust HEIF sample exceeds output depth");
    }
    let icc = icc.map(<[u8]>::to_vec);
    let (image, converted_to_srgb) = if decoded.bit_depth > 8 {
        // Scale the native 10/12-bit values to image's full u16 range before
        // color conversion. Eight-bit preview quantization happens downstream.
        let pixels = decoded
            .data
            .into_iter()
            .map(|value| ((u32::from(value) * 65535 + maximum / 2) / maximum) as u16)
            .collect();
        let mut image = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
            decoded.width,
            decoded.height,
            pixels,
        )
        .context("Invalid Rust HEIF pixel dimensions")?;
        let converted = if icc.is_none() {
            convert_nclx(&mut image, info)?
        } else {
            false
        };
        (DynamicImage::ImageRgba16(image), converted)
    } else {
        let pixels = decoded.data.into_iter().map(|value| value as u8).collect();
        let mut image = RgbaImage::from_raw(decoded.width, decoded.height, pixels)
            .context("Invalid Rust HEIF pixel dimensions")?;
        let converted = if icc.is_none() {
            convert_nclx(&mut image, info)?
        } else {
            false
        };
        (DynamicImage::ImageRgba8(image), converted)
    };
    Ok(Some(DecodedHeif {
        image,
        icc,
        converted_to_srgb,
        source_width: decoded.width,
        source_height: decoded.height,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Set SHADOWROOM_HEIF_FIXTURE to a supported SDR HEVC fixture"]
    fn rust_fixture_decodes_without_loading_native_codecs() {
        let path = std::env::var_os("SHADOWROOM_HEIF_FIXTURE").expect("fixture path");
        let bytes = std::fs::read(path).unwrap();
        let info = inspect(&bytes).unwrap();
        let admitted = working_bytes(&bytes, None).unwrap();
        assert!(decode(&bytes, &info).unwrap().is_some());
        let decoded = super::super::decode(&bytes, None, admitted).unwrap();
        assert_eq!(
            (decoded.source_width, decoded.source_height),
            (info.width, info.height)
        );
        assert_eq!(
            (decoded.image.width(), decoded.image.height()),
            (info.width, info.height)
        );
    }

    #[test]
    #[cfg(not(feature = "native-codecs"))]
    #[ignore = "Set SHADOWROOM_HEIF_FALLBACK_FIXTURE to an alpha or non-HEVC fixture"]
    fn unsupported_fixture_reports_disabled_native_backend() {
        let path = std::env::var_os("SHADOWROOM_HEIF_FALLBACK_FIXTURE").expect("fixture path");
        let bytes = std::fs::read(path).unwrap();
        let info = inspect(&bytes).unwrap();
        assert!(decode(&bytes, &info).unwrap().is_none());
        let admitted = working_bytes(&bytes, None).unwrap();
        let error = super::super::decode(&bytes, None, admitted).err().unwrap();
        assert!(error.to_string().contains("unavailable in this build"));
        assert!(error.to_string().contains("native-codecs"));
    }

    #[test]
    #[cfg(feature = "native-codecs")]
    #[ignore = "Set SHADOWROOM_HEIF_FIXTURE to a supported SDR HEVC fixture"]
    fn rust_fixture_matches_native_color_orientation_and_precision() {
        let path = std::env::var_os("SHADOWROOM_HEIF_FIXTURE").expect("fixture path");
        let bytes = std::fs::read(path).unwrap();
        let info = inspect(&bytes).unwrap();
        validate_sdr(&info).unwrap();
        let rust = decode(&bytes, &info)
            .unwrap()
            .expect("fixture must exercise Rust decoding");
        let native = platform::decode(&bytes, None, &info).unwrap();
        assert_eq!(rust.source_width, native.source_width);
        assert_eq!(rust.source_height, native.source_height);
        assert_eq!(rust.image.color(), native.image.color());
        assert_eq!(rust.icc, native.icc);
        assert_eq!(rust.converted_to_srgb, native.converted_to_srgb);
        let rust = rust.image.to_rgba8();
        let native = native.image.to_rgba8();
        let mut error = 0u64;
        for (a, b) in rust.pixels().zip(native.pixels()) {
            assert_eq!(a[3], b[3]);
            error += (0..3).map(|c| u64::from(a[c].abs_diff(b[c]))).sum::<u64>();
        }
        let mean = error as f64 / (f64::from(info.width) * f64::from(info.height) * 3.0);
        assert!(mean < 3.0, "Rust/native mean RGB error {mean:.3}");
        println!("Rust/native mean RGB error {mean:.3}");
    }

    #[test]
    #[cfg(feature = "native-codecs")]
    #[ignore = "Set SHADOWROOM_HEIF_FALLBACK_FIXTURE to an alpha or non-HEVC fixture"]
    fn unsupported_fixture_retains_native_fallback() {
        let path = std::env::var_os("SHADOWROOM_HEIF_FALLBACK_FIXTURE").expect("fixture path");
        let bytes = std::fs::read(path).unwrap();
        let info = inspect(&bytes).unwrap();
        assert!(decode(&bytes, &info).unwrap().is_none());
        let admitted = working_bytes(&bytes, None).unwrap();
        let output = super::super::decode(&bytes, None, admitted).unwrap();
        assert_eq!(
            (output.source_width, output.source_height),
            (info.width, info.height)
        );
    }
}
