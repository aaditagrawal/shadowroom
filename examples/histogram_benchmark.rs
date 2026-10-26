//! CPU-only comparison with the histogram implementation before sampled-pixel counting.
//! Run: cargo run --locked --release --no-default-features --example histogram_benchmark
//! Synthetic, memory-resident pixels; excludes decoding, storage, and GUI latency.
use image::{DynamicImage, ImageBuffer, Rgb};
use serde_json::json;
use shadowroom::media::{Histogram, histogram};
use std::{hint::black_box, time::Instant};

// Original algorithm, retained here as an experimental baseline. Fixtures are
// opaque RGB8, the common path; it counts an averaged 512-edge thumbnail.
fn averaged_histogram(image: &DynamicImage) -> Histogram {
    let thumbnail;
    let small = if image.width().max(image.height()) > 512 {
        thumbnail = image.thumbnail(512, 512);
        &thumbnail
    } else {
        image
    };
    let rgb = small.as_rgb8().unwrap();
    let mut h = Histogram {
        red: vec![0; 256],
        green: vec![0; 256],
        blue: vec![0; 256],
        luminance: vec![0; 256],
        samples: u64::from(small.width()) * u64::from(small.height()),
    };
    for p in rgb.as_raw().chunks_exact(3) {
        h.red[p[0] as usize] += 1;
        h.green[p[1] as usize] += 1;
        h.blue[p[2] as usize] += 1;
        let luma = (54 * u32::from(p[0]) + 183 * u32::from(p[1]) + 19 * u32::from(p[2])) >> 8;
        h.luminance[luma as usize] += 1;
    }
    h
}

fn clipped(h: &Histogram) -> f64 {
    (h.red[0] + h.red[255]) as f64 / h.samples as f64
}

fn timings(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    json!({"p50_ms": samples[samples.len() / 2], "p95_ms": samples[(samples.len() * 95 / 100).min(samples.len() - 1)]})
}

fn main() {
    let mut cases = Vec::new();
    for (name, width, height, sparse) in [
        ("thumbnail_checker", 384, 256, false),
        ("fit_checker", 2560, 1707, false),
        ("full_checker", 6000, 4000, false),
        ("full_sparse_clipping", 6000, 4000, true),
    ] {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
            let red = if sparse {
                if (x * 73 + y * 151) % 100 == 0 {
                    255
                } else {
                    96
                }
            } else if (x + y) % 2 == 0 {
                0
            } else {
                255
            };
            Rgb([red, ((x * 17 + y * 29) % 256) as u8, ((x ^ y) % 256) as u8])
        }));
        let exact_clipped = image
            .as_rgb8()
            .unwrap()
            .pixels()
            .filter(|p| p[0] == 0 || p[0] == 255)
            .count() as f64
            / (u64::from(width) * u64::from(height)) as f64;
        let before = averaged_histogram(&image);
        let after = histogram(&image);
        let mut before_ms = Vec::new();
        let mut after_ms = Vec::new();
        // Alternate measurement order. Warm both paths before the measured runs.
        for iteration in 0..23 {
            for new in if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = Instant::now();
                black_box(if new {
                    histogram(black_box(&image))
                } else {
                    averaged_histogram(black_box(&image))
                });
                if iteration >= 2 {
                    let samples = if new { &mut after_ms } else { &mut before_ms };
                    samples.push(start.elapsed().as_secs_f64() * 1000.);
                }
            }
        }
        cases.push(json!({
            "case": name, "width": width, "height": height,
            "exact_red_clipped_fraction": exact_clipped,
            "before": {"timing":timings(before_ms), "samples":before.samples,
                "red_clipped_fraction":clipped(&before), "clipping_error_percentage_points":(clipped(&before)-exact_clipped).abs()*100.},
            "after": {"timing":timings(after_ms), "samples":after.samples,
                "red_clipped_fraction":clipped(&after), "clipping_error_percentage_points":(clipped(&after)-exact_clipped).abs()*100.},
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "optimized": !cfg!(debug_assertions), "measured_iterations_per_method":21,
            "workload":"synthetic memory-resident RGB8; histogram only", "cases": cases
        }))
        .unwrap()
    );
}
