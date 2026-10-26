//! Image-analysis and capture-time grouping regressions.
use image::{DynamicImage, ImageBuffer, Rgb, Rgba};
use shadowroom::{
    grouping::{GroupingCache, GroupingMode, GroupingOptions, group_photos},
    media::histogram,
    model::Photo,
};

#[test]
fn histogram_samples_pixels_instead_of_averaging_away_clipping() {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(2048, 2048, |x, y| {
        Rgb([if (x + y) % 2 == 0 { 0 } else { 255 }; 3])
    }));
    let hist = histogram(&image);
    assert_eq!(hist.samples, 512 * 512);
    for channel in [&hist.red, &hist.green, &hist.blue, &hist.luminance] {
        assert_eq!(channel[0] + channel[255], hist.samples);
        // Fixed-stride sampling aliases this pattern to all black or all white.
        let black_share = channel[0] as f64 / hist.samples as f64;
        assert!((black_share - 0.5).abs() < 0.01, "{black_share}");
    }
    assert_eq!(hist.red, histogram(&image).red, "sampling is repeatable");
}

#[test]
fn histogram_excludes_hidden_rgb_and_counts_partial_alpha_once() {
    let image = DynamicImage::ImageRgba8(ImageBuffer::from_fn(4, 1, |x, _| {
        [
            Rgba([255, 0, 0, 0]),
            Rgba([0, 255, 0, 255]),
            Rgba([0, 0, 255, 128]),
            Rgba([0, 0, 0, 0]),
        ][x as usize]
    }));
    let hist = histogram(&image);
    assert_eq!(hist.samples, 2);
    assert_eq!(hist.red[255], 0);
    assert_eq!(hist.green[255], 1);
    assert_eq!(hist.blue[255], 1);
    for channel in [&hist.red, &hist.green, &hist.blue, &hist.luminance] {
        assert_eq!(channel.iter().sum::<u64>(), hist.samples);
    }
}

#[test]
fn histogram_handles_empty_transparent_thin_and_native_depth_inputs() {
    for (width, height) in [(0, 0), (0, 10), (10, 0), (1, 4096), (4096, 1)] {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(width, height, Rgb([19; 3])));
        let hist = histogram(&image);
        assert_eq!(
            hist.samples,
            u64::from(width.min(512)) * u64::from(height.min(512))
        );
        assert_eq!(hist.red[19], hist.samples);
    }
    let transparent = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
        1024,
        1024,
        Rgba([255, 255, 255, 0]),
    ));
    assert_eq!(histogram(&transparent).samples, 0);

    let native = DynamicImage::ImageRgba16(ImageBuffer::from_fn(2, 1, |x, _| {
        if x == 0 {
            Rgba([u16::MAX, 0, 0, 0])
        } else {
            Rgba([0, u16::MAX, 0, u16::MAX])
        }
    }));
    let hist = histogram(&native);
    assert_eq!(hist.samples, 1);
    assert_eq!(hist.red[255], 0);
    assert_eq!(hist.green[255], 1);
}

fn mixed_clocks() -> Vec<Photo> {
    ["offset", "camera_local", "", "mtime"]
        .into_iter()
        .chain(["offset", "camera_local", "", "mtime"])
        .enumerate()
        .map(|(i, clock)| Photo {
            id: i as i64 + 1,
            folder: "same folder".into(),
            camera: "same camera".into(),
            captured_at: (clock != "mtime").then_some(1_700_000_000_000),
            capture_time_kind: clock.into(),
            modified_at: 1_700_000_000_000,
            dhash: Some(0),
            ..Default::default()
        })
        .collect()
}

#[test]
fn matching_integer_timestamps_do_not_bridge_different_clocks() {
    let photos = mixed_clocks();
    for mode in [
        GroupingMode::Time,
        GroupingMode::Visual,
        GroupingMode::Burst,
    ] {
        let options = GroupingOptions {
            mode,
            ..Default::default()
        };
        assert_eq!(
            group_photos(&photos, options),
            vec![vec![1, 5], vec![2, 6], vec![3, 7], vec![4, 8]],
            "{mode:?}"
        );
        assert_eq!(
            GroupingCache::new(&photos, options).groups(),
            group_photos(&photos, options)
        );
    }
}

#[test]
fn progressive_hashes_preserve_clock_boundaries() {
    let mut photos = mixed_clocks();
    for photo in &mut photos {
        photo.dhash = None;
    }
    for mode in [GroupingMode::Visual, GroupingMode::Burst] {
        let options = GroupingOptions {
            mode,
            ..Default::default()
        };
        let mut cache = GroupingCache::new(&photos, options);
        for i in (0..photos.len()).rev() {
            photos[i].dhash = Some(0);
            cache.update_hashes([(photos[i].id, 0)]);
            assert_eq!(cache.groups(), group_photos(&photos, options));
        }
        assert_eq!(cache.groups().len(), 4);
        for photo in &mut photos {
            photo.dhash = None;
        }
    }
}

#[test]
fn normalized_utc_instants_group_across_original_exif_offsets() {
    let mut photos = mixed_clocks();
    photos.retain(|p| p.capture_time_kind == "offset");
    photos[0].captured_offset = Some("+05:30".into());
    photos[1].captured_offset = Some("-04:00".into());
    assert_eq!(
        group_photos(
            &photos,
            GroupingOptions {
                mode: GroupingMode::Time,
                ..Default::default()
            }
        ),
        vec![vec![1, 5]]
    );
}
