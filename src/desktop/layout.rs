//! Logical-pixel layout rules shared by the workspace and its dialogs.
//!
//! Related items use 4 px, icon/label pairs 8 px, control groups 12 px,
//! panel content and section breaks 16 px, dialog content 24 px.
//! 2 px is reserved for compact badges and thumbnail group decoration;
//! 1 px borders and native platform chrome are optical/platform exceptions.
//! Edge controls use equal padding on every facing side, measured inside the
//! hairline. Bar height is control + twice its inset + the separator stroke;
//! text gutters are separate from the inset around a highlighted rectangle.

use std::sync::atomic::{AtomicU32, Ordering};

pub(super) mod space {
    pub const TINY: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const SM: f32 = 8.;
    pub const MD: f32 = 12.;
    pub const LG: f32 = 16.;
    pub const XL: f32 = 24.;
    pub const XXL: f32 = 32.;
}

/// Five-step mono type scale. Roles, not sizes: hierarchy also comes from
/// weight (MEDIUM never reflows in Geist Mono) and colour.
/// MICRO is for glyph-like text only (thumbnail badges, keycaps, axis ticks);
/// readable copy never goes below LABEL. Nothing is drawn below 11 px at
/// 100%; every size passes through [`font`], so the Text size preference
/// scales the whole scale.
pub(super) mod text {
    /// Thumbnail badges, keycaps, histogram axis ticks.
    pub const MICRO: f32 = 11.;
    /// Section headers, captions, secondary rows, status bar.
    pub const LABEL: f32 = 11.;
    /// UI default: rows, buttons, tabs.
    pub const BODY: f32 = 12.;
    /// Inspector filename, dialog subheads.
    pub const TITLE: f32 = 13.;
    /// Dialog and empty-state titles.
    pub const DISPLAY: f32 = 16.;
}

/// Text size, as a factor of the type scale (1.0 = 100%). One value for the
/// window, set from the Text size preference (or the desktop's text-scaling
/// factor) before each frame; see `Workspace::apply_ui_scale`.
static TEXT_SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Text size choices offered in Preferences, in percent.
pub(super) const TEXT_SCALES: [u32; 5] = [90, 100, 110, 125, 150];

pub(super) fn text_scale() -> f32 {
    f32::from_bits(TEXT_SCALE.load(Ordering::Relaxed))
}

/// Sets the factor (clamped to the offered 90-150% range); true if it changed.
pub(super) fn set_text_scale(factor: f32) -> bool {
    let factor = if factor.is_finite() {
        factor.clamp(0.9, 1.5)
    } else {
        1.
    };
    TEXT_SCALE.swap(factor.to_bits(), Ordering::Relaxed) != factor.to_bits()
}

/// A type-scale size (or any font size) at the current text size.
pub(super) fn font(size: f32) -> gpui::Pixels {
    gpui::px(size * text_scale())
}

/// A text-bearing length (line box, field or section height) at the current
/// text size, on whole pixels so 1 px rules stay crisp. Controls keep their
/// 24/28 px boxes: a 150% line (24 px) still fits inside them.
pub(super) fn scaled(length: f32) -> f32 {
    (length * text_scale()).round()
}

pub(super) const CONTROL_HEIGHT: f32 = 28.;
pub(super) const COMPACT_CONTROL_HEIGHT: f32 = 24.;
pub(super) const ROW_HEIGHT: f32 = 32.;
pub(super) const SECTION_HEIGHT: f32 = 24.;
pub(super) const PANEL_INSET: f32 = space::LG;
pub(super) const DIALOG_INSET: f32 = space::XL;
pub(super) const TEXT_LINE_HEIGHT: f32 = 16.;
pub(super) const HAIRLINE: f32 = 1.;
pub(super) const BAR_INSET: f32 = space::SM;
pub(super) const STATUS_INSET: f32 = space::TINY;
pub(super) const CONTROL_ROW_HEIGHT: f32 = CONTROL_HEIGHT + 2. * BAR_INSET;
pub(super) const FIELD_INSET: f32 = space::SM;
pub(super) const FIELD_HEIGHT: f32 = TEXT_LINE_HEIGHT + 2. * FIELD_INSET + 2. * HAIRLINE;
pub(super) const INSPECTOR_SECTION_HEIGHT: f32 = SECTION_HEIGHT + space::LG + space::SM + 1.;

pub(super) const TITLE_HEIGHT: f32 = CONTROL_ROW_HEIGHT + HAIRLINE;
pub(super) const TOOLBAR_HEIGHT: f32 = CONTROL_ROW_HEIGHT + HAIRLINE;
pub(super) const DECISION_HEIGHT: f32 = CONTROL_ROW_HEIGHT + HAIRLINE;
pub(super) const STATUS_HEIGHT: f32 = COMPACT_CONTROL_HEIGHT + 2. * STATUS_INSET + HAIRLINE;
pub(super) const FILMSTRIP_HEIGHT: f32 = 96.;
pub(super) const LIBRARY_WIDTH: f32 = 224.;
pub(super) const INFO_WIDTH: f32 = 288.;
pub(super) const LIBRARY_MIN_WIDTH: f32 = 180.;
pub(super) const INFO_MIN_WIDTH: f32 = 240.;
pub(super) const DOCK_MAX_WIDTH: f32 = 480.;
pub(super) const CENTER_MIN_WIDTH: f32 = 416.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockSide {
    Library,
    Inspector,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DockLayout {
    pub library: f32,
    pub inspector: f32,
    pub center: f32,
}

/// Fit preferred widths without overwriting them. The photo keeps a useful
/// minimum width; both docks shrink to their minimum before one is tucked away.
/// The most recently opened dock wins only when both minimums cannot fit.
pub(super) fn fit_docks(
    width: f32,
    library: Option<f32>,
    inspector: Option<f32>,
    priority: DockSide,
    minimum_center: f32,
) -> DockLayout {
    let width = width.max(0.);
    let available = (width - minimum_center.max(0.)).max(0.);
    let mut left = library
        .map(|w| w.clamp(LIBRARY_MIN_WIDTH, DOCK_MAX_WIDTH))
        .unwrap_or(0.);
    let mut right = inspector
        .map(|w| w.clamp(INFO_MIN_WIDTH, DOCK_MAX_WIDTH))
        .unwrap_or(0.);
    let mut left_min = if left > 0. { LIBRARY_MIN_WIDTH } else { 0. };
    let mut right_min = if right > 0. { INFO_MIN_WIDTH } else { 0. };
    if left_min + right_min > available {
        match priority {
            DockSide::Library => {
                right = 0.;
                right_min = 0.;
            }
            DockSide::Inspector => {
                left = 0.;
                left_min = 0.;
            }
        }
    }
    if left_min + right_min > available {
        left = 0.;
        right = 0.;
        left_min = 0.;
        right_min = 0.;
    }
    let excess = (left + right - available).max(0.);
    let slack = left - left_min + right - right_min;
    if excess > 0. && slack > 0. {
        let take_left = excess * (left - left_min) / slack;
        left -= take_left;
        right -= excess - take_left;
    }
    // Integral edges keep controls and 1 px dividers on the pixel grid.
    left = left.floor();
    right = right.floor();
    DockLayout {
        library: left,
        inspector: right,
        center: (width - left - right).max(0.),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_width_matrix_keeps_photo_space_and_restores_preferences() {
        for width in [640., 800., 960., 1024., 1280., 1600., 2400.] {
            for left in [None, Some(180.), Some(224.), Some(480.)] {
                for right in [None, Some(240.), Some(288.), Some(480.)] {
                    for priority in [DockSide::Library, DockSide::Inspector] {
                        let fit = fit_docks(width, left, right, priority, CENTER_MIN_WIDTH);
                        assert!(fit.center >= CENTER_MIN_WIDTH);
                        assert_eq!(fit.center + fit.library + fit.inspector, width);
                        assert!(fit.library == 0. || fit.library >= LIBRARY_MIN_WIDTH);
                        assert!(fit.inspector == 0. || fit.inspector >= INFO_MIN_WIDTH);
                        if width >= 1600. {
                            assert_eq!(fit.library, left.unwrap_or(0.));
                            assert_eq!(fit.inspector, right.unwrap_or(0.));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn decision_bar_drops_lowest_value_items_first() {
        assert_eq!(fit_prefix(100., 50., &[20., 20., 20.]), 2);
        assert_eq!(fit_prefix(40., 50., &[20.]), 0);
        assert_eq!(fit_prefix(1000., 50., &[20., 500., 20.]), 3);
        // A wide low-priority item never displaces a narrower, more valuable one.
        assert_eq!(fit_prefix(100., 50., &[20., 500., 20.]), 1);
    }

    #[test]
    fn small_windows_hide_the_catalog_before_the_inspector() {
        assert_eq!(culling_docks(true, false, 1024.), (false, true));
        assert_eq!(culling_docks(true, true, 1024.), (false, false));
        assert_eq!(culling_docks(true, true, 1200.), (false, true));
        assert_eq!(culling_docks(true, true, 1440.), (true, true));
        assert_eq!(culling_docks(false, false, 1024.), (true, true));
        // Compare at 1024 px with both docks requested: two usable panes.
        let (library, inspector) = culling_docks(true, true, 1024.);
        let fit = fit_docks(
            1024.,
            library.then_some(LIBRARY_WIDTH),
            inspector.then_some(INFO_WIDTH),
            DockSide::Library,
            560.,
        );
        assert!((fit.center - 2. * PANEL_INSET - space::SM) / 2. >= 440.);
    }

    #[test]
    fn recent_dock_has_priority_only_when_both_cannot_fit() {
        let fit = fit_docks(
            800.,
            Some(224.),
            Some(288.),
            DockSide::Inspector,
            CENTER_MIN_WIDTH,
        );
        assert_eq!(fit.library, 0.);
        assert_eq!(fit.inspector, 288.);
        let fit = fit_docks(
            960.,
            Some(480.),
            Some(480.),
            DockSide::Inspector,
            CENTER_MIN_WIDTH,
        );
        assert!(fit.library >= 180. && fit.inspector >= 240.);
    }
}

pub(super) const FILM_WIDTH: f32 = 96.;
pub(super) const FILM_HEIGHT: f32 = 64.;
// One constant gap, the same as the grid's inner padding rhythm; the selection
// ring is drawn inside the image, so every cell has the same outer box.
pub(super) const FILM_GAP: f32 = space::SM;
pub(super) const FILM_CELL_BUDGET: f32 = FILM_WIDTH + FILM_GAP;

pub(super) const GRID_GAP: f32 = space::MD;
/// Catalog thumbnail sizes S / M / L: (minimum cell width, image height).
/// M is the original grid; a row adds `GRID_ROW_CHROME` (padding, burst bar,
/// caption) to the image height.
pub(super) const GRID_SIZES: [(f32, f32); 3] = [(144., 104.), (208., 160.), (288., 224.)];
pub(super) const GRID_SIZE_LABELS: [&str; 3] = ["S", "M", "L"];
pub(super) const GRID_ROW_CHROME: f32 = 56.;
/// Below this centre width the Catalog uses the small size whatever was chosen,
/// so a laptop with both panels still shows three columns.
pub(super) const GRID_SMALL_CENTER: f32 = 640.;

/// How many optional controls fit, as a prefix of `optional` (most valuable
/// first): the bar always keeps `base` and drops the lowest-value items first.
pub(super) fn fit_prefix(available: f32, base: f32, optional: &[f32]) -> usize {
    let mut used = base;
    optional
        .iter()
        .take_while(|&&width| {
            used += width;
            used <= available
        })
        .count()
}

/// Small windows give the photo room: in Review, Compare and Survey the
/// catalog yields first below 1280 px, and in Compare/Survey the inspector
/// also yields below 1100 px. Settings are untouched; Catalog restores both.
pub(super) fn culling_docks(photo_view: bool, side_by_side: bool, width: f32) -> (bool, bool) {
    let library = !(photo_view && width < 1280.);
    let inspector = !(side_by_side && width < 1100.);
    (library, inspector)
}

pub(super) const INGEST_TIMELINE_WIDTH: f32 = 160.;
pub(super) const INGEST_DESTINATION_WIDTH: f32 = 304.;
pub(super) const SURVEY_HEADER_HEIGHT: f32 = 32.;
