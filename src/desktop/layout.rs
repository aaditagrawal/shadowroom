//! Logical-pixel layout rules shared by the workspace and its dialogs.
//!
//! Related items use 4 px, icon/label pairs 8 px, control groups 12 px,
//! panel content and section breaks 16 px, dialog content 24 px.
//! 2 px is reserved for compact badges and thumbnail group decoration;
//! 1 px borders and native platform chrome are optical/platform exceptions.
//! Edge controls use equal padding on every facing side, measured inside the
//! hairline. Bar height is control + twice its inset + the separator stroke;
//! text gutters are separate from the inset around a highlighted rectangle.

pub(super) mod space {
    pub const TINY: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const SM: f32 = 8.;
    pub const MD: f32 = 12.;
    pub const LG: f32 = 16.;
    pub const XL: f32 = 24.;
    pub const XXL: f32 = 32.;
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
// Keep the progress strip and secondary review actions only when the pane can
// fit them alongside long mono labels and batch counts, rather than wrapping.
pub(super) const REVIEW_EXTRAS_MIN_WIDTH: f32 = 896.;
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
pub(super) const FILM_GAP: f32 = space::XS;
// Image + selection border + inner ring, plus a conservative burst run allowance.
pub(super) const FILM_CELL_BUDGET: f32 = FILM_WIDTH + 6. + FILM_GAP + 2. * space::TINY;

pub(super) const GRID_GAP: f32 = space::MD;
pub(super) const GRID_CELL_MIN_WIDTH: f32 = 208.;
pub(super) const GRID_ROW_HEIGHT: f32 = 216.;
pub(super) const GRID_IMAGE_HEIGHT: f32 = 160.;

pub(super) const INGEST_TIMELINE_WIDTH: f32 = 160.;
pub(super) const INGEST_DESTINATION_WIDTH: f32 = 304.;
pub(super) const SURVEY_HEADER_HEIGHT: f32 = 32.;
