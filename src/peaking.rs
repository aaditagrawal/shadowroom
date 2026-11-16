//! Display-only inspection aids: focus peaking and the clipping overlay.
//! Decisions and source pixels remain untouched; both paint a display copy.

/// Peaking ink. Red is the default: it stays visible on sky, water and
/// foliage, where cyan vanished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PeakingColor {
    #[default]
    Red,
    Cyan,
    Yellow,
    White,
}

impl PeakingColor {
    pub const ALL: [Self; 4] = [Self::Red, Self::Cyan, Self::Yellow, Self::White];

    /// Settings value: "red", "cyan", "yellow" or "white".
    pub fn key(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Cyan => "cyan",
            Self::Yellow => "yellow",
            Self::White => "white",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Cyan => "Cyan",
            Self::Yellow => "Yellow",
            Self::White => "White",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.key() == key)
    }

    /// The painted pixel, as BGR.
    fn bgr(self) -> [u8; 3] {
        match self {
            Self::Red => [64, 64, 255],
            Self::Cyan => [255, 240, 64],
            Self::Yellow => [32, 230, 255],
            Self::White => [255, 255, 255],
        }
    }
}

/// How strong a local edge must be to count as in focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PeakingSensitivity {
    Low,
    #[default]
    Medium,
    High,
}

impl PeakingSensitivity {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// Settings value: "low", "medium" or "high".
    pub fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.key() == key)
    }

    /// Sobel response (on 8-bit luma) a pixel must reach. Higher sensitivity
    /// marks softer edges too.
    fn threshold(self) -> i32 {
        match self {
            Self::Low => 300,
            Self::Medium => 240,
            Self::High => 180,
        }
    }
}

/// Colour and sensitivity of the peaking overlay (Preferences → Culling).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PeakingStyle {
    pub color: PeakingColor,
    pub sensitivity: PeakingSensitivity,
}

/// Highlight strong, local luminance edges in a BGRA display copy of RGBA
/// pixels, in the default style.
pub fn highlight(rgba: &[u8], bgra: &mut [u8], width: u32, height: u32) {
    highlight_with(rgba, bgra, width, height, PeakingStyle::default());
}

/// Paint clipped pixels in a BGRA display copy of RGBA pixels: red where a
/// channel reaches 255 (blown highlights), blue where a channel is 0
/// (crushed shadows). These are the end bins the histogram's clipping
/// readout counts. Transparent pixels are left alone.
pub fn clipping(rgba: &[u8], bgra: &mut [u8]) {
    assert_eq!(bgra.len(), rgba.len());
    for (source, display) in rgba.chunks_exact(4).zip(bgra.chunks_exact_mut(4)) {
        if source[3] < 128 {
            continue;
        }
        let rgb = &source[..3];
        if rgb.contains(&255) {
            display[..3].copy_from_slice(&[0, 0, 255]);
        } else if rgb.contains(&0) {
            display[..3].copy_from_slice(&[255, 64, 0]);
        }
    }
}

/// Highlight strong, local luminance edges in a BGRA display copy of RGBA pixels.
/// A Sobel response favors fine, abrupt detail over gradual tonal transitions.
/// Transparent neighborhoods never contribute an edge. No scratch image is needed.
pub fn highlight_with(rgba: &[u8], bgra: &mut [u8], width: u32, height: u32, style: PeakingStyle) {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(rgba.len(), w * h * 4);
    assert_eq!(bgra.len(), rgba.len());
    if w < 3 || h < 3 {
        return;
    }
    let ink = style.color.bgr();
    let threshold = style.sensitivity.threshold();
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
                if dx.abs().max(dy.abs()) >= threshold {
                    bgra[offset..offset + 3].copy_from_slice(&ink);
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

    #[test]
    fn colour_and_sensitivity_change_the_overlay() {
        // A soft step (Sobel response 220): only High sensitivity marks it.
        let mut source = Vec::new();
        for _ in 0..8 {
            for x in 0..32u8 {
                let value = if x < 16 { 100 } else { 155 };
                source.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let count = |sensitivity| {
            let mut display = source.clone();
            let style = PeakingStyle {
                color: PeakingColor::Red,
                sensitivity,
            };
            highlight_with(&source, &mut display, 32, 8, style);
            display
                .chunks_exact(4)
                .filter(|p| p[..3] == [64, 64, 255])
                .count()
        };
        assert_eq!(count(PeakingSensitivity::Low), 0);
        assert_eq!(count(PeakingSensitivity::Medium), 0);
        assert!(count(PeakingSensitivity::High) > 0);
        for color in PeakingColor::ALL {
            assert_eq!(PeakingColor::from_key(color.key()), Some(color));
        }
        for level in PeakingSensitivity::ALL {
            assert_eq!(PeakingSensitivity::from_key(level.key()), Some(level));
        }
        assert_eq!(PeakingStyle::default().color, PeakingColor::Red);
    }

    #[test]
    fn clipping_marks_blown_and_crushed_pixels_only() {
        let source = [
            255, 250, 240, 255, // highlight: red channel blown
            0, 10, 20, 255, // shadow: red channel crushed
            128, 128, 128, 255, // midtone
            255, 255, 255, 0, // transparent
        ];
        let mut display = source;
        for pixel in display.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let before = display;
        clipping(&source, &mut display);
        assert_eq!(&display[0..3], &[0, 0, 255], "BGRA red");
        assert_eq!(&display[4..7], &[255, 64, 0], "BGRA blue");
        assert_eq!(&display[8..16], &before[8..16]);
    }
}
