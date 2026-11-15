//! Restrained entrance motion for transient surfaces.
//!
//! Menus, dialogs and the palette fade in over 120 ms; status notices, the
//! error toast and the trailing decision note over 150 ms; the on-photo
//! decision chip settles over 120 ms when the shown photo's decision changes.
//! Photo changes, navigation and anything else on the culling path are never
//! animated, and every entrance starts visible enough to read on its first
//! frame, so a fade never delays a keypress's visible result. Exits are
//! instant. GPUI 0.2.2 has no layout-independent transform for elements, so
//! there is no scale; a rise would move the modal scrim with its surface.
//!
//! Each animation frame is a full workspace frame: GPUI's `AnimationElement`
//! requests the next frame until the animation ends, then the window is idle
//! again. Reduced motion (the Preferences toggle, or the platform setting)
//! removes the wrappers entirely. `SHADOWROOM_REDUCE_MOTION=1|0` overrides
//! both for measurements.
use super::Workspace;
use gpui::{
    Animation, AnimationExt, AnyElement, ElementId, IntoElement, ParentElement, Styled, div,
};
use serde_json::Value;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

/// Menus, dialogs and the command palette.
pub(super) const OVERLAY: Duration = Duration::from_millis(120);
/// Status notices, the error toast and the trailing decision note.
pub(super) const NOTICE: Duration = Duration::from_millis(150);
/// The on-photo decision chip when the decision changes in place.
pub(super) const CHIP: Duration = Duration::from_millis(120);

/// Platform preference: 0 unknown, 1 motion allowed, 2 reduce motion.
static SYSTEM: AtomicU8 = AtomicU8::new(0);

/// `SHADOWROOM_REDUCE_MOTION`: `Some(true)` forces reduced motion, `Some(false)`
/// forces motion on regardless of the platform setting.
fn override_from_env() -> Option<bool> {
    static OVERRIDE: OnceLock<Option<bool>> = OnceLock::new();
    *OVERRIDE.get_or_init(
        || match std::env::var("SHADOWROOM_REDUCE_MOTION").as_deref() {
            Ok("1" | "true" | "on") => Some(true),
            Ok("0" | "false" | "off") => Some(false),
            _ => None,
        },
    )
}

/// Whether the platform asks applications to reduce motion, as last read.
pub(super) fn system_reduces_motion() -> bool {
    SYSTEM.load(Ordering::Relaxed) == 2
}

/// Reads the platform setting off the UI thread (startup and when Preferences
/// opens). Linux asks the desktop portal (`org.freedesktop.appearance
/// reduced-motion`, then GNOME's `enable-animations` through the portal) and
/// falls back to `gsettings`; macOS reads NSWorkspace's
/// `accessibilityDisplayShouldReduceMotion`.
pub(super) fn refresh_system_preference() {
    #[cfg(target_os = "macos")]
    {
        SYSTEM.store(if macos_reduce_motion() { 2 } else { 1 }, Ordering::Relaxed);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = std::thread::Builder::new()
            .name("reduce-motion".into())
            .spawn(|| {
                if let Some(reduce) = linux_reduce_motion() {
                    SYSTEM.store(if reduce { 2 } else { 1 }, Ordering::Relaxed);
                }
            });
    }
}

#[cfg(not(target_os = "macos"))]
fn linux_reduce_motion() -> Option<bool> {
    use std::process::{Command, Stdio};
    let run = |program: &str, args: &[&str]| -> Option<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let portal = |namespace: &str, key: &str| {
        run(
            "gdbus",
            &[
                "call",
                "--session",
                "--timeout",
                "2",
                "--dest",
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
                "--method",
                "org.freedesktop.portal.Settings.ReadOne",
                namespace,
                key,
            ],
        )
    };
    // "(<uint32 1>,)": 1 asks for reduced motion (xdg-desktop-portal 1.20+).
    let reduced = portal("org.freedesktop.appearance", "reduced-motion")
        .map(|reply| reply.contains("uint32 1"));
    // "(<false>,)": GNOME animations are off.
    let animations = portal("org.gnome.desktop.interface", "enable-animations")
        .map(|reply| !reply.contains("false"))
        .or_else(|| {
            run(
                "gsettings",
                &["get", "org.gnome.desktop.interface", "enable-animations"],
            )
            .map(|value| value != "false")
        });
    match (reduced, animations) {
        (None, None) => None,
        (reduced, animations) => Some(reduced == Some(true) || animations == Some(false)),
    }
}

#[cfg(target_os = "macos")]
fn macos_reduce_motion() -> bool {
    use objc2::{
        msg_send,
        runtime::{AnyClass, AnyObject, Bool},
    };
    let Some(class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: NSWorkspace's shared instance and this BOOL property exist on
    // every supported macOS release (10.12+); the instance is not retained.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: Bool = msg_send![&*workspace, accessibilityDisplayShouldReduceMotion];
        reduce.as_bool()
    }
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
        override_from_env().unwrap_or_else(|| {
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
