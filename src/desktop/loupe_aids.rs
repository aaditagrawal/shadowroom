//! Inspection aids around the loupe: Focus mode without chrome and its
//! lights-out level, the typed decision stamp, focus-peaking colour and
//! sensitivity, the clipping overlay and zoom presets.
//!
//! Focus mode has two levels. The first (Shift+Tab) hides the title bar,
//! toolbar, status bar and side panels; notices dock in the decision bar,
//! beside a compact culling line, so nothing covers the photo. A second press
//! is lights-out: the decision bar and filmstrip fold into an 8 px cull
//! ledger along the bottom edge, and each decision is confirmed by a typed
//! stamp ("PICK", "REJECT", "★★★★") at bottom centre. Escape steps back one
//! level; Ctrl+Q quits from either, and while a quit waits for writes,
//! exports or an import, both levels say so and Escape keeps the app open.
use super::*;
use shadowroom::peaking::{PeakingColor, PeakingSensitivity, PeakingStyle};

/// The decision stamp stays at full strength this long, then fades.
const STAMP_HOLD: Duration = Duration::from_millis(320);
const STAMP_FADE: Duration = Duration::from_millis(180);
/// With reduced motion the stamp holds a little longer and leaves at once.
const STAMP_HOLD_STILL: Duration = Duration::from_millis(700);
/// The stamp's plate opacity (the scrim colour, nearly opaque).
const STAMP_PLATE_ALPHA: f32 = 0.94;

/// The typed confirmation of a decision made in lights-out.
pub(super) struct Stamp {
    pub text: String,
    pub color: Hsla,
    pub at: Instant,
}

/// The stamp's words for a decision: the flag, else the rating, else the
/// label (a reset names itself).
pub(super) fn stamp_text(
    rating: Option<u8>,
    flag: Option<Flag>,
    label: Option<ColorLabel>,
) -> Option<&'static str> {
    Some(match (flag, rating, label) {
        (Some(Flag::None), Some(0), _) => "RESET",
        (Some(Flag::Pick), ..) => "PICK",
        (Some(Flag::Reject), ..) => "REJECT",
        (Some(Flag::None), ..) => "UNFLAGGED",
        (None, Some(0), _) => "NO RATING",
        (None, Some(r), _) => stars(r),
        (None, None, Some(ColorLabel::None)) => "NO LABEL",
        (None, None, Some(ColorLabel::Red)) => "RED",
        (None, None, Some(ColorLabel::Yellow)) => "YELLOW",
        (None, None, Some(ColorLabel::Green)) => "GREEN",
        (None, None, Some(ColorLabel::Blue)) => "BLUE",
        (None, None, Some(ColorLabel::Purple)) => "PURPLE",
        (None, None, None) => return None,
    })
}

/// Texture-key effect bits above the tier (0-2): +4 focus peaking, +8 the
/// clipping overlay, and the peaking style from bit 4, so a colour or
/// sensitivity change never reuses a stale peaked copy (`key.1 % 4` stays
/// the tier).
const PEAKING_BIT: u8 = 4;
const CLIPPING_BIT: u8 = 8;
const STYLE_SHIFT: u8 = 4;

/// Loupe zoom presets on Alt/Option+digit (REV-22): 0 is Fit.
pub(super) const ZOOM_PRESETS: [(&str, f32); 4] = [("0", 0.), ("1", 1.), ("2", 2.), ("4", 4.)];

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

#[cfg(test)]
mod tests {
    use super::stamp_text;
    use shadowroom::model::{ColorLabel, Flag};

    #[test]
    fn stamps_name_the_decision_in_capitals() {
        assert_eq!(stamp_text(None, Some(Flag::Pick), None), Some("PICK"));
        assert_eq!(stamp_text(None, Some(Flag::Reject), None), Some("REJECT"));
        assert_eq!(stamp_text(Some(4), None, None), Some("★★★★"));
        assert_eq!(stamp_text(Some(0), Some(Flag::None), None), Some("RESET"));
        assert_eq!(stamp_text(None, None, Some(ColorLabel::Blue)), Some("BLUE"));
        assert_eq!(stamp_text(None, None, None), None);
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
            shadowroom::peaking::clipping(rgba, bgra, width);
        }
    }

    /// Whether a display texture key carries focus peaking.
    pub(super) fn effect_peaks(effect: u8) -> bool {
        effect & PEAKING_BIT != 0
    }

    pub(super) fn toggle_clipping(&mut self) {
        self.clipping = !self.clipping;
        self.announce_aid(
            CLIPPING_BIT,
            self.clipping,
            "clipping",
            "Clipping shown · magenta stripes mark blown highlights, blue crushed shadows · J toggles"
                .into(),
            "Clipping hidden",
        );
    }

    pub(super) fn toggle_peaking(&mut self) {
        self.focus_peaking = !self.focus_peaking;
        let shown = format!(
            "Focus peaking on · {} marks sharp edges · F toggles",
            self.peaking_style().color.name().to_lowercase()
        );
        self.announce_aid(
            PEAKING_BIT,
            self.focus_peaking,
            "focus peaking",
            shown,
            "Focus peaking off",
        );
    }

    /// Says an aid is on only once its overlay is on the photo (R4-review-04):
    /// until the background pass has painted the copy, the notice reads
    /// "Preparing …" and [`Self::settle_aid`] swaps in the real text.
    fn announce_aid(&mut self, bit: u8, on: bool, name: &str, shown: String, hidden: &str) {
        if !on {
            if self.pending_aid.as_ref().is_some_and(|(b, _)| *b == bit) {
                self.pending_aid = None;
            }
            self.announce(hidden, Tone::Info);
            return;
        }
        let photo_view = !self.import_ui.open
            && !self.filtered.is_empty()
            && matches!(self.mode, ViewMode::Loupe | ViewMode::Compare);
        let ready = !photo_view
            || self
                .review_photo()
                .is_some_and(|p| self.effect_ready(p, self.wanted_kind()));
        if ready {
            self.pending_aid = None;
            self.announce(shown, Tone::Info);
        } else {
            self.announce(format!("Preparing {name}…"), Tone::Info);
            self.pending_aid = Some((bit, shown));
        }
    }

    /// The loupe's preview tier at the current zoom.
    pub(super) fn wanted_kind(&self) -> PreviewKind {
        if self.zoom {
            PreviewKind::Full
        } else {
            PreviewKind::Fit
        }
    }

    /// Whether the overlaid display copy the loupe wants for `p` is cached.
    pub(super) fn effect_ready(&self, p: &Photo, kind: PreviewKind) -> bool {
        let effect = self.display_effect(kind);
        let (mtime, size) = (preview_mtime(p), preview_size(p));
        effect == 0
            || (1..=2u8).any(|tier| {
                self.textures
                    .contains_key(&(p.id, tier + effect, mtime, size))
            })
    }

    /// Called by the loupe each frame: once the pending aid's overlay is on
    /// the photo, the notice says so.
    pub(super) fn settle_aid(&mut self, p: &Photo, kind: PreviewKind) {
        let Some((bit, _)) = &self.pending_aid else {
            return;
        };
        if (self.display_effect(kind) & bit == 0 || self.effect_ready(p, kind))
            && let Some((_, shown)) = self.pending_aid.take()
        {
            self.announce(shown, Tone::Info);
        }
    }

    /// The effect bits last shown for `id`, so a new overlay copy can keep the
    /// old one on screen instead of dropping to the plain photo.
    pub(super) fn shown_effect(&self, id: i64) -> Option<u8> {
        self.shown_effects
            .iter()
            .find(|(shown, _)| *shown == id)
            .map(|&(_, effect)| effect)
    }

    pub(super) fn remember_effect(&mut self, id: i64, effect: u8) {
        if let Some(entry) = self
            .shown_effects
            .iter_mut()
            .find(|(shown, _)| *shown == id)
        {
            entry.1 = effect;
            return;
        }
        // Compare shows two photos; a few entries cover both and their neighbours.
        if self.shown_effects.len() >= 4 {
            self.shown_effects.remove(0);
        }
        self.shown_effects.push((id, effect));
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

    /// Lights-out, Focus mode's second level, applies where there is a photo
    /// to look at: Review, Compare and Survey with photos in the view.
    pub(super) fn lights_out(&self) -> bool {
        self.focus_mode && self.lights_out && self.lights_out_applies()
    }

    fn lights_out_applies(&self) -> bool {
        !self.import_ui.open && !self.filtered.is_empty() && self.mode != ViewMode::Grid
    }

    /// Shift+Tab (or Tab): off → Focus mode → lights-out → off. In the
    /// Catalog, where lights-out does not apply, it toggles Focus mode.
    pub(super) fn cycle_focus(&mut self) {
        if !self.focus_mode {
            self.focus_mode = true;
            self.lights_out = false;
            self.hint_once(
                "focus-mode",
                if self.lights_out_applies() {
                    "Focus mode · Shift+Tab again for lights out · Esc to exit"
                } else {
                    "Focus mode · Esc or Shift+Tab to exit"
                },
            );
        } else if !self.lights_out() && self.lights_out_applies() {
            self.lights_out = true;
            self.withdraw_notice("focus-mode");
            self.hint_once(
                "lights-out",
                "Lights out · Esc steps back · Shift+Tab exits",
            );
        } else {
            self.focus_mode = false;
            self.lights_out = false;
        }
    }

    /// Escape in Focus mode: lights-out steps back to Focus mode, which then
    /// ends. False when Focus mode is off.
    pub(super) fn step_back_focus(&mut self) -> bool {
        if self.lights_out() {
            self.lights_out = false;
            true
        } else if self.focus_mode {
            self.focus_mode = false;
            self.lights_out = false;
            true
        } else {
            false
        }
    }

    /// Escape while a quit waits for writes, exports or an import: Shadowroom
    /// stays open and the work carries on (R4-a11y-03). Cards queued behind
    /// the running import were already dropped by the quit request.
    pub(super) fn cancel_quit(&mut self) -> bool {
        if self.quit != QuitState::Draining || self.deletion.executing() {
            return false;
        }
        self.quit = QuitState::Open;
        let carrying_on = if self.import_ui.progress.phase.is_active() {
            " · the import continues"
        } else if self.export_jobs.iter().any(|job| job.state.is_active()) {
            " · exports continue"
        } else {
            ""
        };
        self.announce(format!("Staying open{carrying_on}"), Tone::Info);
        true
    }

    /// While a quit waits, the status it waits on and how to stay.
    fn closing_text(&self) -> Option<String> {
        match self.quit {
            QuitState::Open => None,
            QuitState::Draining if !self.deletion.executing() => {
                Some(format!("{} · Esc keeps Shadowroom open", self.status))
            }
            _ => Some(self.status.to_string()),
        }
    }

    /// Records a decision made in lights-out as a typed stamp at the bottom
    /// centre; a new decision replaces the stamp at once (no queue).
    pub(super) fn stamp_decision(
        &mut self,
        rating: Option<u8>,
        flag: Option<Flag>,
        label: Option<ColorLabel>,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = stamp_text(rating, flag, label) else {
            return;
        };
        let color = match (flag, rating, label) {
            (Some(Flag::Pick | Flag::Reject), ..) => self.scrim_flag(flag.unwrap_or_default()),
            (None, Some(r), _) if r > 0 => self.scrim_rating(),
            (None, None, Some(l)) => self
                .photo_label_color(l)
                .unwrap_or_else(|| self.scrim_text()),
            _ => self.scrim_text(),
        };
        self.stamp = Some(Stamp {
            text: text.into(),
            color,
            at: Instant::now(),
        });
        // Wake for the fade (or, with reduced motion, the exit); the fade's
        // own frames are requested while it runs.
        let hold = if self.reduce_motion() {
            STAMP_HOLD_STILL
        } else {
            STAMP_HOLD
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(hold).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// The stamp chip while it lasts: full strength, then a short fade
    /// (none with reduced motion). Expired stamps are dropped.
    fn stamp_view(&mut self) -> Option<AnyElement> {
        let stamp = self.stamp.as_ref()?;
        let elapsed = stamp.at.elapsed();
        let opacity = if self.reduce_motion() {
            (elapsed < STAMP_HOLD_STILL).then_some(1.)
        } else if elapsed < STAMP_HOLD {
            Some(1.)
        } else if elapsed < STAMP_HOLD + STAMP_FADE {
            self.feedback.animating.set(true);
            let t = (elapsed - STAMP_HOLD).as_secs_f32() / STAMP_FADE.as_secs_f32();
            Some(1. - t * t)
        } else {
            None
        };
        let Some(opacity) = opacity else {
            self.stamp = None;
            return None;
        };
        let stamp = self.stamp.as_ref()?;
        // A denser plate than the badges' scrim: 13 px words in the reject
        // ink need 4.5:1 over a white sky (5.8:1 here, 3.9:1 on the scrim).
        let plate = Hsla {
            a: STAMP_PLATE_ALPHA,
            ..self.scrim()
        };
        Some(
            div()
                .id("decision-stamp")
                .h(px(ui::CONTROL_HEIGHT))
                .px(px(space::MD))
                .flex()
                .items_center()
                .bg(plate)
                .opacity(opacity)
                .text_size(ui::font(ui::text::TITLE))
                .font_weight(FontWeight::MEDIUM)
                .text_color(stamp.color)
                .whitespace_nowrap()
                .child(stamp.text.clone())
                .into_any_element(),
        )
    }

    /// Focus mode's first level: what the status bar would say, docked in
    /// the decision bar's free middle so nothing covers the photo
    /// (R4-review-06): the closing status, then a notice and a failed save,
    /// else the compact culling line (exposure and capture time).
    pub(super) fn focus_slot(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let row = || {
            div()
                .id("focus-status")
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(space::SM))
                .overflow_hidden()
                .text_size(ui::font(ui::text::LABEL))
        };
        let ellipsis = |text: String| {
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text)
        };
        if let Some(closing) = self.closing_text() {
            return Some(
                row()
                    .text_color(self.text())
                    .child(ellipsis(closing))
                    .into_any_element(),
            );
        }
        let notice = self.status_notice(cx);
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let save = failed.then(|| self.save_state_view(cx)).flatten();
        if notice.is_some() || save.is_some() {
            return Some(row().children(notice).children(save).into_any_element());
        }
        let p = self.review_photo()?;
        let mut line = exposure_line(p).join("  ");
        if let Some(time) = p.capture_short() {
            if !line.is_empty() {
                line.push_str(" · ");
            }
            line.push_str(&time);
        }
        (!line.is_empty()).then(|| {
            row()
                .text_color(self.muted())
                .child(ellipsis(line))
                .into_any_element()
        })
    }

    /// Lights-out's bottom-centre column above the ledger: the decision
    /// stamp, and a plate for what must not wait (a closing status, a failed
    /// save, a warning or a hint); routine confirmations are the stamp's job.
    pub(super) fn lights_out_overlay(&mut self, cx: &Context<Self>) -> Option<AnyElement> {
        let stamp = self.stamp_view();
        let closing = self.closing_text();
        let notice = if closing.is_some() {
            None
        } else {
            self.feedback
                .flash
                .as_ref()
                .filter(|flash| flash.tone == Tone::Warning || flash.key.is_some())
                .and_then(|_| self.status_notice(cx))
        };
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let save = failed.then(|| self.save_state_view(cx)).flatten();
        let plate = (closing.is_some() || notice.is_some() || save.is_some()).then(|| {
            div()
                .id("lights-out-notices")
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
                .text_size(ui::font(ui::text::LABEL))
                .text_color(self.text())
                .occlude()
                .children(closing.map(|text| {
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text)
                }))
                .children(notice)
                .children(save)
        });
        if stamp.is_none() && plate.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(ledger::LIGHTS_OUT_BAND + space::LG))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::SM))
                .children(stamp)
                .children(plate)
                .into_any_element(),
        )
    }
}
