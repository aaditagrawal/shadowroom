//! RGB histogram element for the inspector.
//!
//! Renders `shadowroom::media::Histogram` as three filled, outlined channel
//! curves over a neutral plot plate (`#0f0f0f` in dark themes, white in light
//! ones, from `shadowroom.histogram.background`). Overlaps are drawn as exact additive
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
//! - The clipping readout and the end markers use the overlay's rule
//!   (`peaking::classify`: shadows only where every channel is near black) and
//!   its inks, so words, markers and the J overlay agree. The caller can give
//!   the readout a steadier preview tier than the plotted one, so it does not
//!   change while sharper previews load.

// Public options are API for the inspector; not every variant is used by every caller.
#![allow(dead_code)]

use gpui::{
    AnyElement, App, Bounds, IntoElement, ParentElement, PathBuilder, Pixels, Rgba, Styled, Window,
    canvas, div, fill, point, prelude::FluentBuilder, px, rgb, rgba, size,
};
use shadowroom::{
    media::Histogram,
    peaking::{ClipCounts, HIGHLIGHT_INK, SHADOW_INK},
};
use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
};

type PlotPaths = Vec<(gpui::Path<Pixels>, Rgba)>;

thread_local! {
    /// The last tessellated plot, keyed by its bounds and display data. GPUI
    /// rebuilds the scene every frame, and tessellating seven bands plus three
    /// or four outlines of 256 bins each on every frame made the histogram one
    /// of the costliest regions with the inspector open (docs/PERFORMANCE.md,
    /// "Human interface responsiveness"). The plot changes only with the
    /// photo, panel geometry or scale, so other frames reuse these paths.
    static PLOT_PATHS: RefCell<Option<(u64, PlotPaths)>> = const { RefCell::new(None) };
}

/// Vertical scaling of bin counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramScale {
    /// Heights proportional to pixel count. Matches Lightroom/Affinity defaults.
    #[default]
    Linear,
    /// `ln(1 + count)`; reveals thin shadow/highlight tails.
    Log,
}

/// What the clipping readout and end markers report.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ClipSource {
    /// The plotted histogram's own counts.
    #[default]
    Plotted,
    /// Counts from a steadier preview than the plotted one (the tier the
    /// overlay paints), so the figures don't change as previews sharpen.
    Stable(ClipCounts),
    /// That preview isn't decoded yet: the readout says "Measuring…".
    Measuring,
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
    /// Label text, drawn on the surrounding panel (not the plot), so callers
    /// pass the theme's muted text; the plate itself is `plot`.
    pub label: Rgba,
    /// Clip readout while a channel is clipped.
    pub label_strong: Rgba,
    /// A clipped end above [`CLIP_WARNING`] of the frame (the theme's warning).
    pub label_warning: Rgba,
    /// Plot plate, its outline and the quarter-tone guides. A light plate
    /// switches the curves to darker inks that hold up on white.
    pub plot: Rgba,
    pub plot_border: Rgba,
    pub plot_grid: Rgba,
    /// Counts behind the clipping readout and the end markers.
    pub clip: ClipSource,
}

impl Default for HistogramOptions {
    fn default() -> Self {
        Self {
            scale: HistogramScale::Linear,
            source: HistogramSource::Preview,
            smooth: true,
            luminance: false,
            labels: true,
            label: rgb(LABEL),
            label_strong: rgb(LABEL_STRONG),
            label_warning: rgb(LABEL_WARNING),
            plot: rgb(PLOT_BG),
            plot_border: rgb(PLOT_BORDER),
            plot_grid: rgb(GRID),
            clip: ClipSource::Plotted,
        }
    }
}

impl HistogramOptions {
    /// A light plate (relative luminance above one half) takes the light inks.
    fn light(&self) -> bool {
        let c = self.plot;
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b > 0.5
    }
}

const PLOT_BG: u32 = 0x0f0f0f;
const PLOT_BORDER: u32 = 0x262626;
const GRID: u32 = 0x1c1c1c;
/// Standalone defaults (dark panel); the inspector passes theme colours.
const LABEL: u32 = 0x9a9a9a;
const LABEL_STRONG: u32 = 0xe6e6e6;
const LABEL_WARNING: u32 = 0xe8893a;
/// "No histogram data" sits on the dark plot plate in every theme.
const EMPTY_LABEL: u32 = 0x9a9a9a;
const AXIS_ROW: f32 = 14.;
const NOTE_ROW: f32 = 14.;
/// Share of the frame before an end counts as clipped at all.
const CLIP_THRESHOLD: f64 = 0.001;
/// Share of the frame above which crushed shadows or a full white-out turn
/// the readout to the warning colour.
pub const CLIP_WARNING: f64 = 0.005;
/// Colour clipping (one or two channels at 255) is common on saturated
/// subjects and warns only past this share.
pub const CHANNEL_WARNING: f64 = 0.02;
/// Side of the readout's ink swatches.
const SWATCH: f32 = 6.;

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
// Light plate: the same additive bands (they mix only with the plate), with
// outlines darkened to keep the curves' edges at ~3:1 on white.
const LIGHT_LINE_R: u32 = 0xc4_3c_3c_e6;
const LIGHT_LINE_G: u32 = 0x2e_8a_40_e6;
const LIGHT_LINE_B: u32 = 0x2f_5f_d0_e6;
const LIGHT_LINE_L: u32 = 0x29_29_29_99;

/// Default inspector histogram: linear scale, preview source note.
pub fn histogram(hist: &Histogram, width: f32, height: f32) -> AnyElement {
    histogram_with(hist, width, height, HistogramOptions::default())
}

/// `width`/`height` are the whole element, including the label rows when
/// `options.labels` is set (28 px). Below 72 px tall the labels are dropped.
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
    let footer_right = data.clip_parts();
    let empty = data.is_empty();

    let painted = data.clone();
    let empty_label = if options.light() {
        options.label
    } else {
        rgb(EMPTY_LABEL)
    };
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
                .text_size(px(11.))
                .text_color(empty_label)
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
            .text_size(px(11.))
            .text_color(options.label);
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
                .text_size(px(11.))
                .text_color(options.label)
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
                        .flex()
                        .text_color(if data.any_clipped() {
                            options.label_strong
                        } else {
                            options.label
                        })
                        .items_center()
                        .gap(px(8.))
                        .children(footer_right.into_iter().map(|part| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .when(part.warn, |s| s.text_color(options.label_warning))
                                // The overlay's ink, as the end markers draw it.
                                .when_some(part.ink, |s, ink| {
                                    s.child(div().flex_none().size(px(SWATCH)).bg(ink))
                                })
                                .child(part.text)
                        })),
                ),
        );
    }
    root.into_any_element()
}

/// One clipping readout entry: "Shadows 0.8%" with the overlay's ink.
#[derive(Clone, Debug, PartialEq)]
struct ClipPart {
    text: String,
    warn: bool,
    ink: Option<Rgba>,
}

fn ink(rgb: [u8; 3]) -> Rgba {
    gpui::rgb(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]))
}

/// Normalised display data, computed once per render and moved into the canvas.
#[derive(Clone)]
struct Prepared {
    red: Vec<f32>,
    green: Vec<f32>,
    blue: Vec<f32>,
    luma: Option<Vec<f32>>,
    /// The readout's counts; `None` while they are being measured.
    clip: Option<ClipCounts>,
    plot: Rgba,
    plot_border: Rgba,
    plot_grid: Rgba,
    light: bool,
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
                clip: None,
                plot: options.plot,
                plot_border: options.plot_border,
                plot_grid: options.plot_grid,
                light: options.light(),
            };
        }
        let channels = [&hist.red[..bins], &hist.green[..bins], &hist.blue[..bins]];
        let clip = match options.clip {
            ClipSource::Plotted => Some(hist.clip),
            ClipSource::Stable(counts) => Some(counts),
            ClipSource::Measuring => None,
        };

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
            clip,
            plot: options.plot,
            plot_border: options.plot_border,
            plot_grid: options.plot_grid,
            light: options.light(),
        }
    }

    fn is_empty(&self) -> bool {
        self.red.is_empty()
    }

    /// Which ends are clipped: (shadows, highlights).
    fn clipped_ends(&self) -> (bool, bool) {
        self.clip.map_or((false, false), |c| {
            (
                c.shadow_share() >= CLIP_THRESHOLD,
                c.highlight_share() >= CLIP_THRESHOLD,
            )
        })
    }

    fn any_clipped(&self) -> bool {
        let (low, high) = self.clipped_ends();
        low || high
    }

    /// "Shadows 0.8%" / "Highlights 1.0%" for each clipped end, in plain
    /// words with the overlay's ink, each flagged past its warning share
    /// (colour clipping on its own needs [`CHANNEL_WARNING`]).
    fn clip_parts(&self) -> Vec<ClipPart> {
        if self.is_empty() {
            return Vec::new();
        }
        let Some(clip) = self.clip else {
            return vec![ClipPart {
                text: "Measuring…".into(),
                warn: false,
                ink: None,
            }];
        };
        let pct = |share: f64| {
            if share >= 0.1 {
                format!("{:.0}%", share * 100.)
            } else {
                format!("{:.1}%", share * 100.)
            }
        };
        let (low, high) = self.clipped_ends();
        let mut parts = Vec::new();
        if low {
            parts.push(ClipPart {
                text: format!("Shadows {}", pct(clip.shadow_share())),
                warn: clip.shadow_share() > CLIP_WARNING,
                ink: Some(ink(SHADOW_INK)),
            });
        }
        if high {
            parts.push(ClipPart {
                text: format!("Highlights {}", pct(clip.highlight_share())),
                warn: clip.blown_share() > CLIP_WARNING || clip.channel_share() > CHANNEL_WARNING,
                ink: Some(ink(HIGHLIGHT_INK)),
            });
        }
        if parts.is_empty() {
            parts.push(ClipPart {
                text: "No clipping".into(),
                warn: false,
                ink: None,
            });
        }
        parts
    }

    /// Identity of a tessellated plot: its geometry and display values.
    fn plot_key(&self, plot: &Plot) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for value in [plot.left, plot.bottom, plot.width, plot.height] {
            value.to_bits().hash(&mut hasher);
        }
        self.light.hash(&mut hasher);
        for channel in [&self.red, &self.green, &self.blue]
            .into_iter()
            .chain(self.luma.as_ref())
        {
            channel.len().hash(&mut hasher);
            for value in channel {
                value.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Band fills, then channel outlines, in paint order.
    fn plot_paths(&self, plot: &Plot) -> PlotPaths {
        let mut paths = Vec::new();
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
                paths.push((path, rgba(*color)));
            }
        }
        let lines = if self.light {
            [LIGHT_LINE_R, LIGHT_LINE_G, LIGHT_LINE_B, LIGHT_LINE_L]
        } else {
            [LINE_R, LINE_G, LINE_B, LINE_L]
        };
        for (channel, color) in [(r, lines[0]), (g, lines[1]), (b, lines[2])] {
            if let Some(path) = plot.outline(channel) {
                paths.push((path, rgba(color)));
            }
        }
        if let Some(luma) = &self.luma
            && let Some(path) = plot.outline(luma)
        {
            paths.push((path, rgba(lines[3])));
        }
        paths
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        window.paint_quad(fill(bounds, self.plot));
        let left = f32::from(bounds.origin.x);
        let top = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);

        // Quarter-tone guides at 64/128/192.
        for quarter in 1..4 {
            let x = (left + w * quarter as f32 / 4.).round();
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(top)), size(px(1.), px(h))),
                self.plot_grid,
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
            let key = self.plot_key(&plot);
            PLOT_PATHS.with_borrow_mut(|cache| {
                if cache.as_ref().is_none_or(|(cached, _)| *cached != key) {
                    *cache = Some((key, self.plot_paths(&plot)));
                }
                if let Some((_, paths)) = cache {
                    for (path, color) in paths.iter() {
                        window.paint_path(path.clone(), *color);
                    }
                }
            });
        }

        // Clip markers: boxy 7 px squares in the top corners, in the
        // overlay's ink for that end (the readout's swatches match).
        let marker = 7.;
        let (low, high) = self.clipped_ends();
        for (lit, x, color) in [
            (low, left + 3., ink(SHADOW_INK)),
            (high, left + w - 3. - marker, ink(HIGHLIGHT_INK)),
        ] {
            // Unlit markers read as unexplained squares; draw only when clipping.
            if !lit {
                continue;
            }
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(top + 3.)), size(px(marker), px(marker))),
                color,
            ));
        }

        window.paint_quad(gpui::outline(
            bounds,
            self.plot_border,
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
            clip: Default::default(),
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
        // The readout and markers come from the overlay's counts, not the bins.
        let blown = ClipCounts {
            samples: 1_000,
            blown: 300,
            ..ClipCounts::default()
        };
        let p = Prepared::new(
            &h,
            HistogramOptions {
                clip: ClipSource::Stable(blown),
                ..Default::default()
            },
        );
        assert_eq!(p.clipped_ends(), (false, true));
        let parts = p.clip_parts();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].text.starts_with("Highlights "), "{parts:?}");
        assert!(parts[0].warn, "a large share warns");
    }

    #[test]
    fn clipping_readout_names_both_ends_and_warns_past_half_a_percent() {
        let readout = |clip: ClipCounts| {
            let mut h = hist(|_| (390, 390, 390));
            h.clip = clip;
            Prepared::new(&h, HistogramOptions::default())
                .clip_parts()
                .into_iter()
                .map(|part| (part.text, part.warn, part.ink.is_some()))
                .collect::<Vec<_>>()
        };
        // 0.8% crushed shadows, 0.2% blown highlights.
        assert_eq!(
            readout(ClipCounts {
                samples: 10_000,
                shadows: 80,
                blown: 20,
                channel: 0,
            }),
            vec![
                ("Shadows 0.8%".to_string(), true, true),
                ("Highlights 0.2%".to_string(), false, true),
            ]
        );
        // Colour clipping alone warns only past 2%.
        assert_eq!(
            readout(ClipCounts {
                samples: 10_000,
                channel: 150,
                ..ClipCounts::default()
            }),
            vec![("Highlights 1.5%".to_string(), false, true)]
        );
        assert_eq!(
            readout(ClipCounts {
                samples: 10_000,
                channel: 250,
                ..ClipCounts::default()
            })[0]
                .1,
            true
        );
        assert_eq!(
            readout(ClipCounts {
                samples: 10_000,
                ..ClipCounts::default()
            }),
            vec![("No clipping".to_string(), false, false)]
        );
        // A steadier tier still being decoded: no figures, no markers.
        let measuring = Prepared::new(
            &hist(|_| (10, 10, 10)),
            HistogramOptions {
                clip: ClipSource::Measuring,
                ..Default::default()
            },
        );
        assert_eq!(measuring.clip_parts()[0].text, "Measuring…");
        assert_eq!(measuring.clipped_ends(), (false, false));
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
            clip: Default::default(),
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

    #[test]
    fn light_plates_take_the_light_inks() {
        assert!(!HistogramOptions::default().light());
        let light = HistogramOptions {
            plot: rgb(0xffffff),
            ..Default::default()
        };
        assert!(light.light());
        let h = hist(|_| (10, 10, 10));
        let dark = Prepared::new(&h, HistogramOptions::default());
        let pale = Prepared::new(&h, light);
        let plot = Plot {
            left: 0.,
            bottom: 60.,
            width: 200.,
            height: 58.,
            bins: 256,
        };
        // Cached paths never carry one plate's inks onto the other.
        assert_ne!(dark.plot_key(&plot), pale.plot_key(&plot));
    }
}
