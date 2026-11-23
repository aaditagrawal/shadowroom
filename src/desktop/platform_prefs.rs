//! Platform accessibility preferences, read off the UI thread and kept live:
//! reduced motion, increased contrast and the desktop text-scaling factor.
//!
//! Linux asks the desktop portal (`org.freedesktop.appearance`
//! `reduced-motion` / `contrast`, GNOME's `enable-animations`, `high-contrast`
//! and `text-scaling-factor` through the portal) and falls back to
//! `gsettings`. A `gdbus monitor` child follows the portal's `SettingChanged`
//! signal, so a change in system settings applies within one poll (≤250 ms)
//! without reopening Preferences. macOS reads NSWorkspace's
//! `accessibilityDisplayShouldReduceMotion` / `…IncreaseContrast` and re-reads
//! them every two seconds from the UI poll (a BOOL read, no subprocess).
//!
//! `SHADOWROOM_REDUCE_MOTION`, `SHADOWROOM_HIGH_CONTRAST` (`1|0`) and
//! `SHADOWROOM_TEXT_SCALE` (a factor such as `1.25`) override the platform for
//! measurements.
use std::sync::{
    OnceLock,
    atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
};

/// 0 unknown, 1 off, 2 on.
static REDUCE_MOTION: AtomicU8 = AtomicU8::new(0);
static HIGH_CONTRAST: AtomicU8 = AtomicU8::new(0);
/// f32 bits of the desktop text-scaling factor (1.0 until read).
static TEXT_SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// Bumped whenever a value changes, so the UI re-applies theme and scale.
static REVISION: AtomicU64 = AtomicU64::new(0);

fn env_flag(name: &str) -> Option<bool> {
    match std::env::var(name).as_deref() {
        Ok("1" | "true" | "on") => Some(true),
        Ok("0" | "false" | "off") => Some(false),
        _ => None,
    }
}

fn overrides() -> &'static (Option<bool>, Option<bool>, Option<f32>) {
    static OVERRIDES: OnceLock<(Option<bool>, Option<bool>, Option<f32>)> = OnceLock::new();
    OVERRIDES.get_or_init(|| {
        (
            env_flag("SHADOWROOM_REDUCE_MOTION"),
            env_flag("SHADOWROOM_HIGH_CONTRAST"),
            std::env::var("SHADOWROOM_TEXT_SCALE")
                .ok()
                .and_then(|value| value.parse::<f32>().ok())
                .filter(|factor| factor.is_finite() && *factor > 0.),
        )
    })
}

/// `SHADOWROOM_REDUCE_MOTION`, if set.
pub(super) fn reduce_motion_override() -> Option<bool> {
    overrides().0
}

/// Whether the platform asks applications to reduce motion, as last read.
pub(super) fn reduces_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed) == 2
}

/// Whether the platform asks for increased contrast (or the override does).
pub(super) fn high_contrast() -> bool {
    overrides()
        .1
        .unwrap_or_else(|| HIGH_CONTRAST.load(Ordering::Relaxed) == 2)
}

/// The desktop's text-scaling factor (GNOME `text-scaling-factor`), 1.0
/// where the platform has none.
pub(super) fn text_scale() -> f32 {
    overrides()
        .2
        .unwrap_or_else(|| f32::from_bits(TEXT_SCALE.load(Ordering::Relaxed)))
}

/// Changes whenever a platform value changes.
pub(super) fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn store_flag(slot: &AtomicU8, on: bool) {
    let value = if on { 2 } else { 1 };
    if slot.swap(value, Ordering::Relaxed) != value {
        REVISION.fetch_add(1, Ordering::Relaxed);
    }
}

fn store_scale(factor: f32) {
    if !factor.is_finite() || factor <= 0. {
        return;
    }
    if TEXT_SCALE.swap(factor.to_bits(), Ordering::Relaxed) != factor.to_bits() {
        REVISION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Reads every value once off the UI thread (startup and when Preferences
/// opens) and, on Linux, starts the change monitor the first time.
pub(super) fn refresh() {
    #[cfg(target_os = "macos")]
    {
        poll_macos();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = std::thread::Builder::new()
            .name("platform-prefs".into())
            .spawn(linux::read_all);
        static MONITOR: OnceLock<()> = OnceLock::new();
        MONITOR.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("platform-prefs-monitor".into())
                .spawn(linux::monitor);
        });
    }
}

/// macOS: re-read the NSWorkspace flags (called from the UI poll).
#[cfg(target_os = "macos")]
pub(super) fn poll_macos() {
    store_flag(&REDUCE_MOTION, macos::flag(macos::Flag::ReduceMotion));
    store_flag(&HIGH_CONTRAST, macos::flag(macos::Flag::IncreaseContrast));
}

/// One `SettingChanged` (or `ReadOne`) value, applied to the matching slot.
/// Returns whether the namespace/key pair is one this module follows.
fn apply(namespace: &str, key: &str, value: &str) -> bool {
    let value = value.trim();
    match (namespace, key) {
        ("org.freedesktop.appearance", "reduced-motion") => {
            store_flag(&REDUCE_MOTION, value.contains("uint32 1"))
        }
        ("org.gnome.desktop.interface", "enable-animations") => {
            store_flag(&REDUCE_MOTION, value.contains("false"))
        }
        ("org.freedesktop.appearance", "contrast") => {
            store_flag(&HIGH_CONTRAST, value.contains("uint32 1"))
        }
        ("org.gnome.desktop.a11y.interface", "high-contrast") => {
            store_flag(&HIGH_CONTRAST, value.contains("true"))
        }
        ("org.gnome.desktop.interface", "text-scaling-factor") => {
            if let Some(factor) = parse_double(value) {
                store_scale(factor);
            }
        }
        _ => return false,
    }
    true
}

/// The number inside a GVariant text such as `<1.25>`, `(<1.25>,)` or `1.25`.
fn parse_double(value: &str) -> Option<f32> {
    let digits: String = value
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

/// Parses one `gdbus monitor` line carrying a portal `SettingChanged` signal:
/// `…Settings.SettingChanged ('org.gnome.desktop.interface', 'enable-animations', <false>)`.
fn parse_setting_changed(line: &str) -> Option<(&str, &str, &str)> {
    let rest = &line[line.find("SettingChanged (")? + "SettingChanged (".len()..];
    let mut quoted = rest.split('\'');
    let namespace = quoted.nth(1)?;
    let key = quoted.nth(1)?;
    let value = rest[rest.find('<')?..].trim_end_matches(')');
    Some((namespace, key, value))
}

#[cfg(not(target_os = "macos"))]
mod linux {
    use super::{HIGH_CONTRAST, REDUCE_MOTION, apply, parse_setting_changed, store_flag};
    use std::process::{Command, Stdio};

    fn run(program: &str, args: &[&str]) -> Option<String> {
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
    }

    fn portal(namespace: &str, key: &str) -> Option<String> {
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
    }

    fn gsettings(schema: &str, key: &str) -> Option<String> {
        run("gsettings", &["get", schema, key])
    }

    pub(super) fn read_all() {
        // Reduced motion: the portal key (xdg-desktop-portal 1.20+) or
        // GNOME's animations switch; either one asking for less wins.
        let reduced = portal("org.freedesktop.appearance", "reduced-motion")
            .map(|reply| reply.contains("uint32 1"));
        let animations_off = portal("org.gnome.desktop.interface", "enable-animations")
            .or_else(|| gsettings("org.gnome.desktop.interface", "enable-animations"))
            .map(|reply| reply.contains("false"));
        if reduced.is_some() || animations_off.is_some() {
            store_flag(
                &REDUCE_MOTION,
                reduced == Some(true) || animations_off == Some(true),
            );
        }
        // Contrast: the portal's appearance key (GNOME 45+, KDE) or GNOME's
        // own high-contrast switch.
        let contrast = portal("org.freedesktop.appearance", "contrast")
            .map(|reply| reply.contains("uint32 1"));
        let gnome_contrast = portal("org.gnome.desktop.a11y.interface", "high-contrast")
            .or_else(|| gsettings("org.gnome.desktop.a11y.interface", "high-contrast"))
            .map(|reply| reply.contains("true"));
        if contrast.is_some() || gnome_contrast.is_some() {
            store_flag(
                &HIGH_CONTRAST,
                contrast == Some(true) || gnome_contrast == Some(true),
            );
        }
        if let Some(factor) = portal("org.gnome.desktop.interface", "text-scaling-factor")
            .or_else(|| gsettings("org.gnome.desktop.interface", "text-scaling-factor"))
        {
            apply(
                "org.gnome.desktop.interface",
                "text-scaling-factor",
                &factor,
            );
        }
    }

    /// Follows the portal's SettingChanged signal for the life of the app.
    /// Without gdbus or a portal it simply ends; values then refresh when
    /// Preferences opens.
    pub(super) fn monitor() {
        use std::io::{BufRead, BufReader};
        let Ok(mut child) = Command::new("gdbus")
            .args([
                "monitor",
                "--session",
                "--dest",
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
        else {
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some((namespace, key, value)) = parse_setting_changed(&line) {
                // Re-read every value: the portal's reduced-motion and GNOME's
                // enable-animations (and the two contrast keys) combine.
                if apply(namespace, key, value) {
                    read_all();
                }
            }
        }
        let _ = child.wait();
    }
}

#[cfg(target_os = "macos")]
mod macos {
    pub(super) enum Flag {
        ReduceMotion,
        IncreaseContrast,
    }

    pub(super) fn flag(flag: Flag) -> bool {
        use objc2::{
            msg_send,
            runtime::{AnyClass, AnyObject, Bool},
        };
        let Some(class) = AnyClass::get(c"NSWorkspace") else {
            return false;
        };
        // SAFETY: NSWorkspace's shared instance and these BOOL properties
        // exist on every supported macOS release (10.12+); the instance is
        // not retained.
        unsafe {
            let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
            if workspace.is_null() {
                return false;
            }
            let value: Bool = match flag {
                Flag::ReduceMotion => {
                    msg_send![&*workspace, accessibilityDisplayShouldReduceMotion]
                }
                Flag::IncreaseContrast => {
                    msg_send![&*workspace, accessibilityDisplayShouldIncreaseContrast]
                }
            };
            value.as_bool()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_double, parse_setting_changed};

    #[test]
    fn monitor_lines_parse_into_setting_changes() {
        let line = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged ('org.gnome.desktop.interface', 'enable-animations', <false>)";
        assert_eq!(
            parse_setting_changed(line),
            Some((
                "org.gnome.desktop.interface",
                "enable-animations",
                "<false>"
            ))
        );
        let line = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged ('org.freedesktop.appearance', 'contrast', <uint32 1>)";
        assert_eq!(
            parse_setting_changed(line),
            Some(("org.freedesktop.appearance", "contrast", "<uint32 1>"))
        );
        assert_eq!(parse_setting_changed("unrelated signal"), None);
    }

    #[test]
    fn text_scaling_values_parse() {
        assert_eq!(parse_double("<1.25>"), Some(1.25));
        assert_eq!(parse_double("(<1.0>,)"), Some(1.0));
        assert_eq!(parse_double("1.5"), Some(1.5));
        assert_eq!(parse_double("<true>"), None);
    }
}
