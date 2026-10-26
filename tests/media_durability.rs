use shadowroom::{
    media::PreviewKind,
    remote::{
        PreviewCache, PreviewResult,
        protocol::{MAX_PAYLOAD, PhotoKey},
    },
};
use std::{fs, path::Path};

fn key() -> PhotoKey {
    PhotoKey {
        root_id: "root".into(),
        volume_id: "volume".into(),
        relative_path: "frame.jpg".into(),
        size_bytes: 100,
        modified_at: 1,
        companion_path: None,
        companion_size_bytes: 0,
        companion_modified_at: 0,
    }
}
fn preview() -> PreviewResult {
    let image = image::RgbImage::from_pixel(16, 12, image::Rgb([20, 30, 40]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode_image(&image)
        .unwrap();
    PreviewResult {
        jpeg,
        width: 16,
        height: 12,
        cache_hit: false,
        rendered_raw: false,
        embedded_raw: false,
        rendering_note: None,
    }
}
fn sidecar(dir: &Path) -> std::path::PathBuf {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap()
}

#[test]
fn cache_checks_jpeg_dimensions_even_when_content_checksum_matches() {
    let dir = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(dir.path().into()).unwrap();
    cache
        .put("camera", &key(), PreviewKind::Fit, &preview())
        .unwrap();
    let path = sidecar(dir.path());
    let mut meta: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    meta["width"] = serde_json::json!(16000);
    fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(
        cache
            .get("camera", &key(), PreviewKind::Fit, MAX_PAYLOAD)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cache_cannot_publish_contradictory_raw_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let cache = PreviewCache::new(dir.path().into()).unwrap();
    let mut invalid = preview();
    invalid.rendered_raw = true;
    invalid.embedded_raw = true;
    assert!(
        cache
            .put("camera", &key(), PreviewKind::Fit, &invalid)
            .is_err()
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn cache_does_not_follow_a_replaced_payload_symlink() {
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        let cache = PreviewCache::new(dir.path().into()).unwrap();
        let original = preview();
        cache
            .put("camera", &key(), PreviewKind::Fit, &original)
            .unwrap();
        let payload = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "jpg"))
            .unwrap();
        let external = dir.path().join("external");
        fs::write(&external, &original.jpeg).unwrap();
        fs::remove_file(&payload).unwrap();
        std::os::unix::fs::symlink(&external, &payload).unwrap();
        assert!(
            cache
                .get("camera", &key(), PreviewKind::Fit, MAX_PAYLOAD)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(external).unwrap(), original.jpeg);
    }
}

#[test]
fn resized_png_preview_does_not_reveal_fully_transparent_red() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.png");
    image::RgbaImage::from_fn(768, 16, |x, _| {
        if x < 384 {
            image::Rgba([255, 0, 0, 0])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    })
    .save(&path)
    .unwrap();
    let preview = shadowroom::media::decode_preview(&path, PreviewKind::Thumbnail).unwrap();
    assert_eq!((preview.width, preview.height), (384, 8));
    assert!(preview.rgba.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
    for pixel in preview.rgba.chunks_exact(4).filter(|p| p[3] > 0) {
        assert_eq!(&pixel[..3], &[0, 0, 255]);
    }
}
