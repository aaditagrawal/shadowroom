//! Display-only focus assistance. Decisions and source pixels remain untouched.

/// Highlight strong, local luminance edges in a BGRA display copy of RGBA pixels.
/// A Sobel response favors fine, abrupt detail over gradual tonal transitions.
/// Transparent neighborhoods never contribute an edge. No scratch image is needed.
pub fn highlight(rgba: &[u8], bgra: &mut [u8], width: u32, height: u32) {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(rgba.len(), w * h * 4);
    assert_eq!(bgra.len(), rgba.len());
    if w < 3 || h < 3 {
        return;
    }
    let luma = |x: usize, y: usize| -> i32 {
        let p = &rgba[(y * w + x) * 4..][..4];
        if p[3] < 128 {
            // Alpha edges and hidden RGB values are not photographic detail.
            -1
        } else {
            (54 * i32::from(p[0]) + 183 * i32::from(p[1]) + 19 * i32::from(p[2])) >> 8
        }
    };
    for y in 1..h - 1 {
        let column = |x| [luma(x, y - 1), luma(x, y), luma(x, y + 1)];
        let mut left = column(0);
        let mut center = column(1);
        for x in 1..w - 1 {
            // Reuse the preceding columns: only three new samples per pixel.
            let right = column(x + 1);
            let offset = (y * w + x) * 4;
            if left.iter().chain(&center).chain(&right).all(|&v| v >= 0) {
                let dx = right[0] + 2 * right[1] + right[2] - left[0] - 2 * left[1] - left[2];
                let dy = left[2] + 2 * center[2] + right[2] - left[0] - 2 * center[0] - right[0];
                if dx.abs().max(dy.abs()) >= 240 {
                    // Cyan is visible against skin tones and warm backgrounds.
                    bgra[offset..offset + 3].copy_from_slice(&[255, 240, 64]);
                }
            }
            left = center;
            center = right;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharp_edges_peak_but_flat_and_gradual_detail_do_not() {
        for sharp in [false, true] {
            let mut source = Vec::new();
            for _ in 0..8 {
                for x in 0..32 {
                    let value = if sharp {
                        if x < 16 { 0 } else { 255 }
                    } else {
                        x * 4
                    };
                    source.extend_from_slice(&[value, value, value, 255]);
                }
            }
            let original = source.clone();
            let mut display = source.clone();
            highlight(&source, &mut display, 32, 8);
            assert_eq!(source, original);
            assert_eq!(display != source, sharp);
            assert!(display.chunks_exact(4).all(|p| p[3] == 255));
        }
    }

    #[test]
    fn transparent_hidden_detail_is_not_highlighted() {
        let mut source = vec![0; 8 * 8 * 4];
        for (i, pixel) in source.chunks_exact_mut(4).enumerate() {
            pixel[..3].fill(if i % 2 == 0 { 0 } else { 255 });
        }
        let mut display = source.clone();
        highlight(&source, &mut display, 8, 8);
        assert_eq!(display, source);
    }

    #[test]
    fn opacity_edges_do_not_create_false_focus_detail() {
        for transparent_alpha in [0, 100, 180] {
            let mut source = vec![255; 8 * 8 * 4];
            for row in source.chunks_exact_mut(8 * 4) {
                for pixel in row.chunks_exact_mut(4).take(4) {
                    pixel[3] = transparent_alpha;
                }
            }
            let mut display = source.clone();
            highlight(&source, &mut display, 8, 8);
            assert_eq!(
                display, source,
                "alpha-only boundary at {transparent_alpha}"
            );
        }
    }

    #[test]
    fn tiny_images_are_unchanged() {
        for (width, height) in [(0, 0), (1, 1), (2, 8), (8, 2)] {
            let source = vec![255; width as usize * height as usize * 4];
            let mut display = source.clone();
            highlight(&source, &mut display, width, height);
            assert_eq!(display, source);
        }
    }
}
