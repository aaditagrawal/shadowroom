//! Repeatable pixel comparison for the optional, Rust-native HEIF backend.
use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageBuffer, Rgba};
use serde_json::json;
use std::{path::Path, time::Instant};

const MAX_SOURCE: u64 = 512 * 1024 * 1024;
const MAX_PIXELS: u64 = 160_000_000;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let source = args
        .next()
        .context("usage: codec-evaluation INPUT.heic OUTPUT.png [REFERENCE.png]")?;
    let output = args.next().context("output PNG path required")?;
    let reference = args.next();
    if args.next().is_some() {
        bail!("too many arguments");
    }
    if std::fs::metadata(&source)?.len() > MAX_SOURCE {
        bail!("source exceeds 512 MiB evaluation limit");
    }
    let bytes = std::fs::read(&source)?;
    let container = heifer::heifer_isobmff::HeifFile::parse(&bytes)?;
    let start = Instant::now();
    let decoded = heifer::decode_with_options(
        &bytes,
        &heifer::Options {
            max_threads: 1,
            max_pixels: MAX_PIXELS,
        },
    )?;
    let elapsed = start.elapsed();
    if !(8..=16).contains(&decoded.bit_depth) {
        bail!("unsupported sample depth");
    }
    let max = (1u32 << decoded.bit_depth) - 1;
    let pixels = decoded
        .data
        .into_iter()
        .map(|sample| {
            if u32::from(sample) > max {
                bail!("sample exceeds bit depth");
            }
            Ok(((u32::from(sample) * 65535 + max / 2) / max) as u16)
        })
        .collect::<Result<Vec<_>>>()?;
    let image = DynamicImage::ImageRgba16(
        ImageBuffer::<Rgba<u16>, _>::from_raw(decoded.width, decoded.height, pixels)
            .context("invalid output dimensions")?,
    );
    image.save_with_format(Path::new(&output), image::ImageFormat::Png)?;
    let comparison = reference
        .map(|path| -> Result<_> {
            let reference = image::open(path)?;
            compare(&reference, &image)
        })
        .transpose()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "backend": "heifer 0.2.0",
        "items": container.items.iter().map(|item| format!("{}:{}", item.id, item.item_type)).collect::<Vec<_>>(),
        "unknown_properties": container.properties.iter().filter_map(|property| {
            if let heifer::heifer_isobmff::boxes::Property::Unknown(kind, _) = property {
                Some(kind.to_string())
            } else { None }
        }).collect::<Vec<_>>(),
        "references": container.references.iter().map(|reference| format!("{}:{}->{:?}", reference.ref_type, reference.from, reference.to)).collect::<Vec<_>>(),
            "width": image.width(),
            "height": image.height(),
            "source_bit_depth": decoded.bit_depth,
            "has_alpha": decoded.has_alpha,
            "threads": 1,
            "decode_ms": elapsed.as_secs_f64() * 1000.0,
            "color_management": "source RGB; ICC/NCLX conversion not applied by this evaluation tool",
            "comparison": comparison,
        }))?
    );
    Ok(())
}

fn compare(expected: &DynamicImage, actual: &DynamicImage) -> Result<serde_json::Value> {
    if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        bail!("reference and decoded dimensions differ");
    }
    let expected = expected.to_rgba8();
    let actual = actual.to_rgba8();
    let mut squared = 0f64;
    let mut rgb_error = 0u64;
    let mut alpha_error = 0u64;
    let mut max_error = 0u8;
    for (expected, actual) in expected.pixels().zip(actual.pixels()) {
        for channel in 0..3 {
            let error = expected[channel].abs_diff(actual[channel]);
            rgb_error += u64::from(error);
            squared += f64::from(error).powi(2);
            max_error = max_error.max(error);
        }
        alpha_error += u64::from(expected[3].abs_diff(actual[3]));
    }
    let pixels = f64::from(expected.width()) * f64::from(expected.height());
    let mse = squared / (pixels * 3.0);
    Ok(json!({
        "mean_rgb_error_8bit": rgb_error as f64 / (pixels * 3.0),
        "max_rgb_error_8bit": max_error,
        "mean_alpha_error_8bit": alpha_error as f64 / pixels,
        "rgb_psnr_db": if mse == 0.0 { None } else { Some(10.0 * (255.0f64.powi(2) / mse).log10()) },
        "identical_rgb": mse == 0.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_rejects_orientation_and_records_alpha_differences() {
        assert!(
            compare(
                &DynamicImage::new_rgba8(2, 3),
                &DynamicImage::new_rgba8(3, 2)
            )
            .is_err()
        );
        let expected =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([10, 20, 30, 255])));
        let actual =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([10, 20, 30, 128])));
        let result = compare(&expected, &actual).unwrap();
        assert_eq!(result["identical_rgb"], true);
        assert_eq!(result["mean_alpha_error_8bit"], 127.0);
    }
}
