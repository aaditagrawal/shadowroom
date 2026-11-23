//! Restrained entrance motion for transient surfaces.
//!
//! Menus, dialogs and the palette fade in over 120 ms; status notices, the
//! error toast and the trailing decision note over 150 ms; the on-photo
//! decision chip settles over 120 ms when it appears for a photo's first
//! decision. A notice that replaces a visible one, a changed decision and
//! anything else on the culling path (navigation, photo changes, zoom, pan)
//! never animates, so rapid culling neither flickers nor draws extra frames,
//! and every entrance is readable on its first frame. Exits are instant. GPUI 0.2.2 has no layout-independent transform for elements, so
//! there is no scale; a rise would move the modal scrim with its surface.
//!
//! Each animation frame is a full workspace frame: GPUI's `AnimationElement`
//! requests the next frame until the animation ends, then the window is idle
//! again. Reduced motion (the Preferences toggle, or the platform setting)
//! removes the wrappers entirely. `SHADOWROOM_REDUCE_MOTION=1|0` overrides
//! both for measurements.
use super::{Workspace, platform_prefs};
use gpui::{
    Animation, AnimationExt, AnyElement, ElementId, IntoElement, ParentElement, Styled, div,
};
use serde_json::Value;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    time::{Duration, Instant},
};

/// Menus, dialogs and the command palette.
pub(super) const OVERLAY: Duration = Duration::from_millis(120);
/// Status notices, the error toast and the trailing decision note.
pub(super) const NOTICE: Duration = Duration::from_millis(150);
/// The on-photo decision chip when it appears in place.
pub(super) const CHIP: Duration = Duration::from_millis(120);

/// Whether the platform asks applications to reduce motion, as last read.
/// The value is live: `platform_prefs` follows system changes.
pub(super) fn system_reduces_motion() -> bool {
    platform_prefs::reduces_motion()
}

/// Re-reads the platform settings off the UI thread (startup and when
/// Preferences opens); see `platform_prefs`.
pub(super) fn refresh_system_preference() {
    platform_prefs::refresh();
}

/// A stable element id for one occurrence of a transient surface, so a new
/// notice restarts its entrance while an unchanged one keeps its finished state.
pub(super) fn occurrence(name: &'static str, value: impl Hash) -> ElementId {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    ElementId::from((name, hasher.finish() as usize))
}

/// Ease-out cubic: fast at first, settling at the end.
fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

impl Workspace {
    /// The Preferences toggle, the platform setting, or the environment override.
    pub(super) fn reduce_motion(&self) -> bool {
        platform_prefs::reduce_motion_override().unwrap_or_else(|| {
            self.settings
                .extra
                .get("reduce_motion")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || system_reduces_motion()
        })
    }

    /// Fades `element` in from `from` opacity over `duration`; unchanged when
    /// motion is reduced. The animation styles the element itself, so its
    /// layout is untouched.
    pub(super) fn fade_in<E: IntoElement + Styled + 'static>(
        &self,
        id: impl Into<ElementId>,
        duration: Duration,
        from: f32,
        element: E,
    ) -> AnyElement {
        if self.reduce_motion() {
            return element.into_any_element();
        }
        element
            .with_animation(
                id,
                Animation::new(duration).with_easing(ease_out),
                move |this, t| this.opacity(from + (1. - from) * t),
            )
            .into_any_element()
    }

    /// Opacity of an entrance that began at `started`, for elements whose
    /// callers keep styling them (the on-photo chips). While it runs, the
    /// frame asks GPUI for the next one, as `AnimationElement` does.
    pub(super) fn entrance_opacity(&self, started: Instant, duration: Duration, from: f32) -> f32 {
        if self.reduce_motion() {
            return 1.;
        }
        let t = started.elapsed().as_secs_f32() / duration.as_secs_f32();
        if t >= 1. {
            return 1.;
        }
        self.feedback.animating.set(true);
        from + (1. - from) * ease_out(t)
    }

    /// [`Self::fade_in`] for a full-window overlay (scrim and surface). It
    /// starts at 30% so the overlay is legible on the frame that answers the
    /// keypress.
    pub(super) fn overlay_in(&self, id: &'static str, element: AnyElement) -> AnyElement {
        if self.reduce_motion() {
            return element;
        }
        self.fade_in(id, OVERLAY, 0.3, div().absolute().inset_0().child(element))
    }
}

#[cfg(test)]
mod tests {
    use super::{ease_out, occurrence};

    #[test]
    fn easing_starts_fast_and_ends_settled() {
        assert_eq!(ease_out(0.), 0.);
        assert_eq!(ease_out(1.), 1.);
        assert!(ease_out(0.25) > 0.5);
    }

    #[test]
    fn occurrences_differ_by_value() {
        assert_eq!(occurrence("notice", 1u64), occurrence("notice", 1u64));
        assert_ne!(occurrence("notice", 1u64), occurrence("notice", 2u64));
    }
}
