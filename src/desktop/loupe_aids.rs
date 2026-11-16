//! Inspection aids around the loupe: Focus mode without chrome, focus-peaking
//! colour and sensitivity, the clipping overlay and zoom presets.
use super::*;
use shadowroom::peaking::{PeakingColor, PeakingSensitivity, PeakingStyle};

/// Texture-key effect bits above the tier (0-2): +4 focus peaking, +8 the
/// clipping overlay, and the peaking style from bit 4, so a colour or
/// sensitivity change never reuses a stale peaked copy (`key.1 % 4` stays
/// the tier).
const PEAKING_BIT: u8 = 4;
const CLIPPING_BIT: u8 = 8;
const STYLE_SHIFT: u8 = 4;

/// The peaking style stored in settings (missing or unknown keys: default).
pub(super) fn peaking_style_of(settings: &Settings) -> PeakingStyle {
    let text = |key: &str| settings.extra.get(key).and_then(Value::as_str);
    PeakingStyle {
        color: text("peaking_color")
            .and_then(PeakingColor::from_key)
            .unwrap_or_default(),
        sensitivity: text("peaking_sensitivity")
            .and_then(PeakingSensitivity::from_key)
            .unwrap_or_default(),
    }
}

impl Workspace {
    pub(super) fn peaking_style(&self) -> PeakingStyle {
        peaking_style_of(&self.settings)
    }

    /// Effect bits of the display copy wanted for `kind`; thumbnails never
    /// carry an overlay.
    pub(super) fn display_effect(&self, kind: PreviewKind) -> u8 {
        if kind == PreviewKind::Thumbnail {
            return 0;
        }
        let mut effect = 0;
        if self.focus_peaking {
            let style = self.peaking_style();
            let color = PeakingColor::ALL
                .iter()
                .position(|&c| c == style.color)
                .unwrap_or(0);
            let level = PeakingSensitivity::ALL
                .iter()
                .position(|&l| l == style.sensitivity)
                .unwrap_or(0);
            effect |= PEAKING_BIT | (((color * 3 + level) as u8) << STYLE_SHIFT);
        }
        if self.clipping {
            effect |= CLIPPING_BIT;
        }
        effect
    }

    /// Paints the overlays `effect` asks for into a BGRA display copy of
    /// `rgba`. Runs on the background executor.
    pub(super) fn paint_effects(
        effect: u8,
        style: PeakingStyle,
        rgba: &[u8],
        bgra: &mut [u8],
        width: u32,
        height: u32,
    ) {
        if effect & PEAKING_BIT != 0 {
            shadowroom::peaking::highlight_with(rgba, bgra, width, height, style);
        }
        // Clipping paints last: an exposure warning outranks an edge.
        if effect & CLIPPING_BIT != 0 {
            shadowroom::peaking::clipping(rgba, bgra);
        }
    }

    /// Whether a display texture key carries focus peaking.
    pub(super) fn effect_peaks(effect: u8) -> bool {
        effect & PEAKING_BIT != 0
    }

    pub(super) fn toggle_clipping(&mut self) {
        self.clipping = !self.clipping;
        self.announce(
            if self.clipping {
                "Clipping shown · red marks blown highlights, blue crushed shadows · J toggles"
            } else {
                "Clipping hidden"
            },
            Tone::Info,
        );
    }

    /// Fit (0) or an exact magnification, from the presets or the palette.
    pub(super) fn zoom_preset(&mut self, factor: f32) {
        if factor <= 0. {
            self.zoom = false;
        } else {
            self.zoom = true;
            self.zoom_factor = factor.clamp(0.125, 8.);
        }
        self.read_ahead();
    }

    /// The title bar, view toolbar and status bar. Focus mode hides them so
    /// the photo gets the height (REV-25); Import always keeps them.
    pub(super) fn chrome_visible(&self) -> bool {
        !self.focus_mode || self.import_ui.open
    }

    /// Notices and save failures while Focus mode hides the status bar: a
    /// plate at the top of the canvas, so "Focus mode · Esc to exit", Undo
    /// and Retry stay reachable. Nothing shows when there is nothing to say.
    pub(super) fn focus_notices(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let notice = self.status_notice(cx);
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let save = failed.then(|| self.save_state_view(cx)).flatten();
        if notice.is_none() && save.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(px(space::SM))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("focus-notices")
                        .max_w(px(640.))
                        .min_w(px(0.))
                        .h(px(ui::COMPACT_CONTROL_HEIGHT + 2. * ui::STATUS_INSET + 2.))
                        .px(px(space::SM))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .bg(self.c("status_bar.background", "#141414"))
                        .border_1()
                        .border_color(self.border())
                        .text_size(px(ui::text::LABEL))
                        .text_color(self.muted())
                        .occlude()
                        .children(notice)
                        .children(save),
                )
                .into_any_element(),
        )
    }
}
