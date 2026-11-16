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

/// How one pixel clips, by the single rule the overlay and the histogram's
/// readout share. Saturated mid-tones routinely have one channel at 0 in
/// 8-bit sRGB (yellow-green grass, a fuchsia dress), so a shadow counts only
/// when every channel is near black; a highlight is a full white-out when
/// every channel is near white, and colour clipping when only one or two are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clip {
    None,
    /// Every channel at or below [`SHADOW_MAX`].
    Shadow,
    /// One or two channels at or above [`HIGHLIGHT_MIN`]: a blown colour.
    Channel,
    /// Every channel at or above [`HIGHLIGHT_MIN`]: detail is gone.
    Blown,
}

/// A channel at or below this is crushed (with the other two).
pub const SHADOW_MAX: u8 = 2;
/// A channel at or above this is clipped.
pub const HIGHLIGHT_MIN: u8 = 254;

/// The overlay's inks, as sRGB. The readout's swatches and the histogram's
/// end markers use the same colours. Highlights are magenta stripes, never
/// solid red: red is the default peaking ink and the reject colour.
pub const HIGHLIGHT_INK: [u8; 3] = [255, 51, 204];
/// Crushed shadows: solid blue.
pub const SHADOW_INK: [u8; 3] = [0, 72, 255];

/// Classifies one opaque RGB pixel.
pub fn classify(rgb: [u8; 3]) -> Clip {
    let high = rgb.iter().filter(|&&c| c >= HIGHLIGHT_MIN).count();
    if high == 3 {
        Clip::Blown
    } else if high > 0 {
        Clip::Channel
    } else if rgb.iter().all(|&c| c <= SHADOW_MAX) {
        Clip::Shadow
    } else {
        Clip::None
    }
}

/// Clipped samples of one preview, counted by [`classify`]. Pixels below half
/// opacity are skipped, as the overlay skips them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClipCounts {
    pub samples: u64,
    pub shadows: u64,
    pub channel: u64,
    pub blown: u64,
}

impl ClipCounts {
    pub fn add(&mut self, rgba: [u8; 4]) {
        if rgba[3] < 128 {
            return;
        }
        self.samples += 1;
        match classify([rgba[0], rgba[1], rgba[2]]) {
            Clip::Shadow => self.shadows += 1,
            Clip::Channel => self.channel += 1,
            Clip::Blown => self.blown += 1,
            Clip::None => {}
        }
    }
    /// Counts every pixel of an RGBA buffer.
    pub fn of(rgba: &[u8]) -> Self {
        let mut counts = Self::default();
        let (pixels, _) = rgba.as_chunks::<4>();
        for pixel in pixels {
            counts.add(*pixel);
        }
        counts
    }
    fn share(&self, count: u64) -> f64 {
        if self.samples == 0 {
            0.
        } else {
            count as f64 / self.samples as f64
        }
    }
    pub fn shadow_share(&self) -> f64 {
        self.share(self.shadows)
    }
    /// Full white-out plus colour clipping: every pixel the overlay stripes.
    pub fn highlight_share(&self) -> f64 {
        self.share(self.blown + self.channel)
    }
    pub fn blown_share(&self) -> f64 {
        self.share(self.blown)
    }
    pub fn channel_share(&self) -> f64 {
        self.share(self.channel)
    }
}

/// Stripe period, in image pixels, for an image whose long edge is `long`:
/// about 4 display pixels when a 2560 px preview is fitted to a window.
fn stripe_period(long: u32) -> u32 {
    (long / 240).max(6)
}

/// Paint clipped pixels in a BGRA display copy of RGBA pixels, by [`classify`]:
/// crushed shadows solid blue; highlights as 45° magenta stripes, wide where
/// every channel is blown and thin where only one or two are, so the two read
/// apart and neither matches peaking or reject red. Transparent pixels are
/// left alone.
pub fn clipping(rgba: &[u8], bgra: &mut [u8], width: u32) {
    assert_eq!(bgra.len(), rgba.len());
    let width = width.max(1) as usize;
    let height = rgba.len() / 4 / width;
    let period = stripe_period(width.max(height) as u32) as usize;
    let thin = (period / 4).max(1);
    let wide = period / 2;
    let [hr, hg, hb] = HIGHLIGHT_INK;
    let [sr, sg, sb] = SHADOW_INK;
    let (sources, _) = rgba.as_chunks::<4>();
    let (displays, _) = bgra.as_chunks_mut::<4>();
    for (i, (source, display)) in sources.iter().zip(displays.iter_mut()).enumerate() {
        if source[3] < 128 {
            continue;
        }
        let stripe = || (i % width + i / width) % period;
        match classify([source[0], source[1], source[2]]) {
            Clip::None => {}
            Clip::Shadow => display[..3].copy_from_slice(&[sb, sg, sr]),
            Clip::Blown if stripe() < wide => display[..3].copy_from_slice(&[hb, hg, hr]),
            Clip::Channel if stripe() < thin => display[..3].copy_from_slice(&[hb, hg, hr]),
            Clip::Blown | Clip::Channel => {}
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
    fn clipping_rule_ignores_saturated_mid_tones() {
        // Yellow-green grass, a fuchsia dress: one channel at 0 is a gamut
        // edge, not a crushed shadow.
        assert_eq!(classify([150, 200, 0]), Clip::None);
        assert_eq!(classify([230, 0, 140]), Clip::None);
        assert_eq!(classify([0, 0, 0]), Clip::Shadow);
        assert_eq!(classify([2, 1, 2]), Clip::Shadow);
        assert_eq!(classify([3, 0, 0]), Clip::None);
        assert_eq!(classify([255, 255, 255]), Clip::Blown);
        assert_eq!(classify([254, 255, 254]), Clip::Blown);
        assert_eq!(classify([255, 120, 40]), Clip::Channel);
        assert_eq!(classify([255, 255, 80]), Clip::Channel);
    }

    #[test]
    fn overlay_and_readout_agree_on_a_mixed_fixture() {
        // 24 x 24: rows of black, white, a red dress, grass and a mid grey.
        let (w, h) = (24u32, 24u32);
        let mut source = Vec::new();
        for y in 0..h {
            for _ in 0..w {
                let rgb = match y % 6 {
                    0 => [0, 0, 0],
                    1 => [255, 255, 255],
                    2 => [255, 60, 40],
                    3 => [150, 200, 0],
                    4 => [128, 128, 128],
                    _ => [1, 2, 0],
                };
                source.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let mut display = source.clone();
        for pixel in display.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let before = display.clone();
        clipping(&source, &mut display, w);
        let counts = ClipCounts::of(&source);
        assert_eq!(counts.samples, u64::from(w * h));
        // Every pixel the overlay may paint is one the readout counts, by row.
        for (i, (pixel, plain)) in display
            .as_chunks::<4>()
            .0
            .iter()
            .zip(before.as_chunks::<4>().0)
            .enumerate()
        {
            let row = i / w as usize;
            let painted = pixel != plain;
            match row % 6 {
                0 | 5 => assert_eq!(&pixel[..3], &[255, 72, 0], "shadow rows are solid blue"),
                1 | 2 => {}
                _ => assert!(!painted, "grass and grey stay untouched (row {row})"),
            }
        }
        assert_eq!(counts.shadows, 2 * 4 * u64::from(w));
        assert_eq!(counts.blown, 4 * u64::from(w));
        assert_eq!(counts.channel, 4 * u64::from(w));
        // Blown rows carry wider stripes than colour-clipped ones.
        let stripes = |row: usize| {
            display[row * w as usize * 4..(row + 1) * w as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] == [204, 51, 255])
                .count()
        };
        assert!(stripes(1) > stripes(2) && stripes(2) > 0);
        // Highlights never paint the default peaking ink or a solid field.
        assert_ne!(HIGHLIGHT_INK, [255, 64, 64]);
        assert!(stripes(1) < w as usize);
    }

    #[test]
    fn transparent_pixels_are_neither_painted_nor_counted() {
        let source = [255, 255, 255, 0, 0, 0, 0, 100];
        let mut display = source;
        clipping(&source, &mut display, 2);
        assert_eq!(display, source);
        assert_eq!(ClipCounts::of(&source), ClipCounts::default());
    }
}
