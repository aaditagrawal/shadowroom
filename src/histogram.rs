//! RGB histogram element for the inspector.
//!
//! Renders `shadowroom::media::Histogram` as three filled, outlined channel
//! curves over a neutral `#0f0f0f` plot. Overlaps are drawn as exact additive
//! bands (white where all three channels overlap, cyan/magenta/yellow where two
//! do) instead of relying on alpha blending, so the image reads the same on
//! every renderer and the colours mean something.
//!
//! Truthfulness rules:
//! - Counts come from `media::histogram`, which samples the decoded 8-bit sRGB
//!   preview (≤512 px long edge). It is never a RAW sensor histogram; the footer
//!   says so.
//! - The vertical scale is normalised to the tallest *interior* bin (1..=254),
//!   so a clipped spike at 0 or 255 can't flatten the rest of the curve. The
//!   end bins are clamped and reported separately through the clip markers.
//! - Display smoothing is a 3-tap [1,2,1] kernel on interior bins only; the end
//!   bins (the clipping evidence) are never smeared.

// Public options are API for the inspector; not every variant is used by every caller.
#![allow(dead_code)]

use gpui::{
    AnyElement, App, Bounds, IntoElement, ParentElement, PathBuilder, Pixels, Rgba, Styled, Window,
    canvas, div, fill, point, px, rgb, rgba, size,
};
use shadowroom::media::Histogram;

/// Vertical scaling of bin counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramScale {
    /// Heights proportional to pixel count. Matches Lightroom/Affinity defaults.
    #[default]
    Linear,
    /// `ln(1 + count)`; reveals thin shadow/highlight tails.
    Log,
}

/// Where the histogram's pixels came from. Drives the footer note only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramSource {
    /// Decoded JPEG/PNG/TIFF/WebP preview.
    #[default]
    Preview,
    /// JPEG embedded in a RAW file; tone and white balance are the camera's.
    EmbeddedRaw,
    RenderedRaw,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistogramOptions {
    pub scale: HistogramScale,
    pub source: HistogramSource,
    /// 3-tap display smoothing on interior bins.
    pub smooth: bool,
    /// Faint luminance outline on top of the RGB curves.
    pub luminance: bool,
    /// Tone-axis tick labels and the source/clip footer.
    pub labels: bool,
}

impl Default for HistogramOptions {
    fn default() -> Self {
        Self {
            scale: HistogramScale::Linear,
            source: HistogramSource::Preview,
            smooth: true,
            luminance: false,
            labels: true,
        }
    }
}

const PLOT_BG: u32 = 0x0f0f0f;
const PLOT_BORDER: u32 = 0x262626;
const GRID: u32 = 0x1c1c1c;
const LABEL: u32 = 0x6e6e6e;
const LABEL_STRONG: u32 = 0xa0a0a0;
const AXIS_ROW: f32 = 13.;
const NOTE_ROW: f32 = 13.;
/// Share of samples in an end bin before that channel counts as clipped.
const CLIP_THRESHOLD: f64 = 0.001;

// Band fills (straight alpha over #0f0f0f). Bands never overlap, so alpha only
// mixes with the plot background.
const FILL_R: u32 = 0xe0_4a_4a_8c;
const FILL_G: u32 = 0x4c_c2_5a_8c;
const FILL_B: u32 = 0x4a_7c_ec_8c;
const FILL_Y: u32 = 0xd6_c2_4a_8c;
const FILL_M: u32 = 0xc2_5c_d6_8c;
const FILL_C: u32 = 0x4a_c6_cc_8c;
const FILL_W: u32 = 0xb4_b4_b4_80;
const LINE_R: u32 = 0xf0_6a_6a_e6;
const LINE_G: u32 = 0x6a_dc_78_e6;
const LINE_B: u32 = 0x6e_9a_ff_e6;
const LINE_L: u32 = 0xe8_e8_e8_99;

/// Default inspector histogram: linear scale, preview source note.
pub fn histogram(hist: &Histogram, width: f32, height: f32) -> AnyElement {
    histogram_with(hist, width, height, HistogramOptions::default())
}

/// `width`/`height` are the whole element, including the label rows when
/// `options.labels` is set (26 px). Below 72 px tall the labels are dropped.
pub fn histogram_with(
    hist: &Histogram,
    width: f32,
    height: f32,
    options: HistogramOptions,
) -> AnyElement {
    let width = width.max(32.);
    let height = height.max(16.);
    let labels = options.labels && height >= 72.;
    let plot_height = if labels {
        height - AXIS_ROW - NOTE_ROW
    } else {
        height
    };
    let data = Prepared::new(hist, options);

    // Only the caveat that changes interpretation is shown; scale is shown by
    // the caller's LIN/LOG control rather than inside the plot.
    let footer_left = match options.source {
        HistogramSource::Preview => "",
        HistogramSource::EmbeddedRaw => "Embedded JPEG, not RAW data",
        HistogramSource::RenderedRaw => "RAW render · 8-bit sRGB",
    };
    let footer_right = data.clip_note();
    let empty = data.is_empty();

    let painted = data.clone();
    let mut plot = div().relative().w(px(width)).h(px(plot_height)).child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| painted.paint(bounds, window, cx),
        )
        .size_full(),
    );
    if empty {
        plot = plot.child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.))
                .text_color(rgb(LABEL))
                .child("No histogram data"),
        );
    }

    let mut root = div()
        .flex()
        .flex_col()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .child(plot);
    if labels {
        let ticks = [
            (0usize, "0"),
            (64, "64"),
            (128, "128"),
            (192, "192"),
            (255, "255"),
        ];
        let mut axis = div()
            .relative()
            .w(px(width))
            .h(px(AXIS_ROW))
            .text_size(px(9.))
            .text_color(rgb(LABEL));
        for (value, text) in ticks {
            let x = value as f32 / 255. * width;
            let tick = div().absolute().top(px(1.)).w(px(28.)).child(text);
            axis = axis.child(match value {
                0 => tick.left(px(0.)),
                255 => tick.right(px(0.)).text_right(),
                _ => tick.left(px(x - 14.)).text_center(),
            });
        }
        root = root.child(axis).child(
            div()
                .w(px(width))
                .h(px(NOTE_ROW))
                .flex()
                .justify_between()
                .gap(px(6.))
                .text_size(px(9.))
                .text_color(rgb(LABEL))
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(footer_left),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(if data.any_clipped() {
                            LABEL_STRONG
                        } else {
                            LABEL
                        }))
                        .child(footer_right),
                ),
        );
    }
    root.into_any_element()
}

/// Normalised display data, computed once per render and moved into the canvas.
#[derive(Clone)]
struct Prepared {
    red: Vec<f32>,
    green: Vec<f32>,
    blue: Vec<f32>,
    luma: Option<Vec<f32>>,
    /// Channel mask (r=1, g=2, b=4) clipped at the low/high end.
    clip_low: u8,
    clip_high: u8,
    low_share: f64,
    high_share: f64,
}

impl Prepared {
    fn new(hist: &Histogram, options: HistogramOptions) -> Self {
        let bins = hist.red.len().min(hist.green.len()).min(hist.blue.len());
        if bins < 3 || hist.samples == 0 {
            return Self {
                red: Vec::new(),
                green: Vec::new(),
                blue: Vec::new(),
                luma: None,
                clip_low: 0,
                clip_high: 0,
                low_share: 0.,
                high_share: 0.,
            };
        }
        let channels = [&hist.red[..bins], &hist.green[..bins], &hist.blue[..bins]];
        let samples = hist.samples as f64;
        let mut clip_low = 0u8;
        let mut clip_high = 0u8;
        let mut low_share = 0f64;
        let mut high_share = 0f64;
        for (bit, channel) in channels.iter().enumerate() {
            let low = channel[0] as f64 / samples;
            let high = channel[bins - 1] as f64 / samples;
            if low >= CLIP_THRESHOLD {
                clip_low |= 1 << bit;
            }
            if high >= CLIP_THRESHOLD {
                clip_high |= 1 << bit;
            }
            low_share = low_share.max(low);
            high_share = high_share.max(high);
        }

        let smoothed: Vec<Vec<f64>> = channels
            .iter()
            .map(|c| display_counts(c, options.smooth))
            .collect();
        let luma = (options.luminance && hist.luminance.len() >= bins)
            .then(|| display_counts(&hist.luminance[..bins], options.smooth));
        // Reference = tallest interior bin across RGB, so end-bin clipping spikes
        // don't compress everything else into a flat line.
        let reference = smoothed
            .iter()
            .flat_map(|c| c[1..bins - 1].iter().copied())
            .fold(0f64, f64::max)
            .max(1.);
        let normalise = |counts: &[f64]| -> Vec<f32> {
            counts
                .iter()
                .map(|&c| {
                    let h = match options.scale {
                        HistogramScale::Linear => c / reference,
                        HistogramScale::Log => (1. + c).ln() / (1. + reference).ln(),
                    };
                    h.clamp(0., 1.) as f32
                })
                .collect()
        };
        Self {
            red: normalise(&smoothed[0]),
            green: normalise(&smoothed[1]),
            blue: normalise(&smoothed[2]),
            luma: luma.map(|l| normalise(&l)),
            clip_low,
            clip_high,
            low_share,
            high_share,
        }
    }

    fn is_empty(&self) -> bool {
        self.red.is_empty()
    }

    fn any_clipped(&self) -> bool {
        self.clip_low != 0 || self.clip_high != 0
    }

    fn clip_note(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let pct = |share: f64| {
            if share >= 0.1 {
                format!("{:.0}%", share * 100.)
            } else {
                format!("{:.1}%", share * 100.)
            }
        };
        match (self.clip_low != 0, self.clip_high != 0) {
            (false, false) => "No clipping".into(),
            (true, false) => format!("Clip ◀ {}", pct(self.low_share)),
            (false, true) => format!("Clip ▶ {}", pct(self.high_share)),
            (true, true) => format!("Clip ◀ {} ▶ {}", pct(self.low_share), pct(self.high_share)),
        }
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        window.paint_quad(fill(bounds, rgb(PLOT_BG)));
        let left = f32::from(bounds.origin.x);
        let top = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);

        // Quarter-tone guides at 64/128/192.
        for quarter in 1..4 {
            let x = (left + w * quarter as f32 / 4.).round();
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(top)), size(px(1.), px(h))),
                rgb(GRID),
            ));
        }

        if !self.is_empty() {
            // 1 px headroom so a full-height bin's outline isn't cut by the border.
            let plot = Plot {
                left,
                bottom: top + h,
                width: w,
                height: (h - 2.).max(1.),
                bins: self.red.len(),
            };
            let (r, g, b) = (&self.red, &self.green, &self.blue);
            // Additive bands: what the eye would see if each channel were light.
            // (lower, upper) per bin; white = all three, CMY = two, RGB = one.
            let band = |f: &dyn Fn(f32, f32, f32) -> (f32, f32)| -> (Vec<f32>, Vec<f32>) {
                (0..plot.bins).map(|i| f(r[i], g[i], b[i])).unzip()
            };
            let bands = [
                (band(&|r, g, b| (0., r.min(g).min(b))), FILL_W),
                (band(&|r, g, b| (b, r.min(g))), FILL_Y),
                (band(&|r, g, b| (g, r.min(b))), FILL_M),
                (band(&|r, g, b| (r, g.min(b))), FILL_C),
                (band(&|r, g, b| (g.max(b), r)), FILL_R),
                (band(&|r, g, b| (r.max(b), g)), FILL_G),
                (band(&|r, g, b| (r.max(g), b)), FILL_B),
            ];
            for ((lows, highs), color) in &bands {
                if let Some(path) = plot.band(lows, highs) {
                    window.paint_path(path, rgba(*color));
                }
            }
            for (channel, color) in [(r, LINE_R), (g, LINE_G), (b, LINE_B)] {
                if let Some(path) = plot.outline(channel) {
                    window.paint_path(path, rgba(color));
                }
            }
            if let Some(luma) = &self.luma {
                if let Some(path) = plot.outline(luma) {
                    window.paint_path(path, rgba(LINE_L));
                }
            }
        }

        // Clip markers: boxy 7 px squares in the top corners, lit in the additive
        // colour of the clipped channels (white = all three).
        let marker = 7.;
        for (mask, x) in [
            (self.clip_low, left + 3.),
            (self.clip_high, left + w - 3. - marker),
        ] {
            // Unlit markers read as unexplained squares; draw only when clipping.
            if mask == 0 {
                continue;
            }
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(top + 3.)), size(px(marker), px(marker))),
                mask_color(mask),
            ));
        }

        window.paint_quad(gpui::outline(
            bounds,
            rgb(PLOT_BORDER),
            gpui::BorderStyle::Solid,
        ));
    }
}

/// Integer counts → display counts. End bins are left untouched.
fn display_counts(counts: &[u64], smooth: bool) -> Vec<f64> {
    let n = counts.len();
    let raw: Vec<f64> = counts.iter().map(|&c| c as f64).collect();
    if !smooth || n < 5 {
        return raw;
    }
    let mut out = raw.clone();
    for i in 1..n - 1 {
        // Neighbours are clamped to the interior so clipping never bleeds inward.
        let prev = raw[(i - 1).max(1)];
        let next = raw[(i + 1).min(n - 2)];
        out[i] = (prev + 2. * raw[i] + next) / 4.;
    }
    out
}

fn mask_color(mask: u8) -> Rgba {
    rgb(match mask {
        0b001 => 0xf06a6a,
        0b010 => 0x6adc78,
        0b100 => 0x6e9aff,
        0b011 => 0xe6d25a,
        0b101 => 0xd26ae6,
        0b110 => 0x5ad6dc,
        _ => 0xf0f0f0,
    })
}

struct Plot {
    left: f32,
    bottom: f32,
    width: f32,
    height: f32,
    bins: usize,
}

impl Plot {
    fn x(&self, i: usize) -> f32 {
        self.left + i as f32 / (self.bins - 1) as f32 * self.width
    }

    fn y(&self, v: f32) -> f32 {
        self.bottom - v * self.height
    }

    /// Closed polygon between `lows` and `max(lows, highs)`. Returns `None`
    /// when the band is empty everywhere.
    fn band(&self, lows: &[f32], highs: &[f32]) -> Option<gpui::Path<Pixels>> {
        let n = self.bins;
        let highs: Vec<f32> = highs.iter().zip(lows).map(|(h, l)| h.max(*l)).collect();
        if lows.iter().zip(&highs).all(|(l, h)| h - l < 1e-4) {
            return None;
        }
        let mut builder = PathBuilder::fill();
        builder.move_to(point(px(self.x(0)), px(self.y(highs[0]))));
        for i in 1..n {
            builder.line_to(point(px(self.x(i)), px(self.y(highs[i]))));
        }
        for i in (0..n).rev() {
            builder.line_to(point(px(self.x(i)), px(self.y(lows[i]))));
        }
        builder.close();
        builder.build().ok()
    }

    fn outline(&self, values: &[f32]) -> Option<gpui::Path<Pixels>> {
        if values.iter().all(|v| *v <= 0.) {
            return None;
        }
        let mut builder = PathBuilder::stroke(px(1.));
        builder.move_to(point(px(self.x(0)), px(self.y(values[0]))));
        for (i, v) in values.iter().enumerate().skip(1) {
            builder.line_to(point(px(self.x(i)), px(self.y(*v))));
        }
        builder.build().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(f: impl Fn(usize) -> (u64, u64, u64)) -> Histogram {
        let mut h = Histogram {
            red: vec![0; 256],
            green: vec![0; 256],
            blue: vec![0; 256],
            luminance: vec![0; 256],
            samples: 0,
        };
        for i in 0..256 {
            let (r, g, b) = f(i);
            h.red[i] = r;
            h.green[i] = g;
            h.blue[i] = b;
            h.samples += r;
        }
        h
    }

    #[test]
    fn end_spike_does_not_flatten_interior() {
        let h = hist(|i| {
            if i == 255 {
                (100_000, 100_000, 100_000)
            } else {
                (100, 100, 100)
            }
        });
        let p = Prepared::new(&h, HistogramOptions::default());
        assert!((p.red[128] - 1.).abs() < 1e-6);
        assert_eq!(p.red[255], 1.);
        assert_eq!(p.clip_high, 0b111);
        assert_eq!(p.clip_low, 0);
    }

    #[test]
    fn smoothing_keeps_end_bins() {
        let counts: Vec<u64> = (0..256).map(|i| if i == 0 { 500 } else { 10 }).collect();
        let d = display_counts(&counts, true);
        assert_eq!(d[0], 500.);
        assert_eq!(d[1], 10.);
    }

    #[test]
    fn empty_histogram_is_empty() {
        let h = Histogram {
            red: vec![],
            green: vec![],
            blue: vec![],
            luminance: vec![],
            samples: 0,
        };
        assert!(Prepared::new(&h, HistogramOptions::default()).is_empty());
    }

    #[test]
    fn log_scale_lifts_tails() {
        let h = hist(|i| {
            if i == 10 {
                (1, 1, 1)
            } else {
                (1000, 1000, 1000)
            }
        });
        let lin = Prepared::new(
            &h,
            HistogramOptions {
                smooth: false,
                ..Default::default()
            },
        );
        let log = Prepared::new(
            &h,
            HistogramOptions {
                smooth: false,
                scale: HistogramScale::Log,
                ..Default::default()
            },
        );
        assert!(log.red[10] > lin.red[10]);
    }
}
