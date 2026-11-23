//! Text size and contrast: the Appearance preferences that follow the
//! platform (`platform_prefs`) unless the user chooses otherwise.
//!
//! Text size scales every font size and text line box (`layout::font`,
//! `layout::scaled`): 90/100/110/125/150%, or System, which follows the
//! desktop's text-scaling factor (GNOME `text-scaling-factor`; 100% where the
//! platform has none). Controls keep their 24/28 px boxes and spacing stays on
//! the 4 px grid, so layouts keep their shape while text grows.
//!
//! When the system asks for more contrast, Shadowroom Dark and Light are drawn
//! as Shadowroom High Contrast Dark and Light; both are also ordinary themes in
//! the Appearance list. Imported themes are used as chosen.
use super::*;

/// The settings value for a Text size choice: `"system"` or the percentage.
pub(super) fn text_size_value(percent: u32) -> Value {
    if percent == 0 {
        json!("system")
    } else {
        json!(percent)
    }
}

/// The chosen percentage, or `None` for System (the default).
fn chosen_text_size(settings: &Settings) -> Option<u32> {
    settings
        .extra
        .get("text_size")
        .and_then(Value::as_u64)
        .map(|percent| percent as u32)
        .filter(|percent| ui::TEXT_SCALES.contains(percent))
}

impl Workspace {
    /// Sets the window's text size from the preference or the platform;
    /// true when it changed (the next frame lays out at the new size).
    pub(super) fn apply_ui_scale(&mut self) -> bool {
        let factor = match chosen_text_size(&self.settings) {
            Some(percent) => percent as f32 / 100.,
            None => platform_prefs::text_scale(),
        };
        ui::set_text_scale(factor)
    }

    /// The theme drawn for `name`: its high-contrast variant while the system
    /// asks for more contrast, else `name` itself.
    pub(super) fn contrast_theme<'a>(&self, name: &'a str) -> &'a str {
        if platform_prefs::high_contrast() {
            Theme::high_contrast_variant(name).unwrap_or(name)
        } else {
            name
        }
    }

    /// Text size choices for Preferences → Appearance (System, then the
    /// offered percentages) and the description under them.
    pub(super) fn text_size_choices(
        &self,
        settings: &Settings,
    ) -> (Vec<(String, Cmd, bool)>, String) {
        let chosen = chosen_text_size(settings);
        let mut choices = vec![("System".to_string(), Cmd::TextSize(0), chosen.is_none())];
        choices.extend(
            ui::TEXT_SCALES
                .iter()
                .map(|&p| (format!("{p}%"), Cmd::TextSize(p), chosen == Some(p))),
        );
        let system = (platform_prefs::text_scale().clamp(0.9, 1.5) * 100.).round();
        let description = if chosen.is_none() {
            format!(
                "Size of all interface text. System follows the desktop's text scaling, now {system}%."
            )
        } else {
            "Size of all interface text. System follows the desktop's text scaling.".to_string()
        };
        (choices, description)
    }

    /// The line under the theme list while a high-contrast variant replaces
    /// the chosen theme.
    pub(super) fn contrast_note(&self, chosen: &str) -> Option<String> {
        let variant = Theme::high_contrast_variant(chosen)?;
        platform_prefs::high_contrast().then(|| {
            format!("The system asks for more contrast, so {chosen} is shown as {variant}.")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{chosen_text_size, text_size_value};
    use crate::desktop::layout as ui;
    use serde_json::json;
    use shadowroom::{settings::Settings, theme::Theme};

    #[test]
    fn text_size_values_round_trip() {
        let mut settings = Settings::default();
        assert_eq!(chosen_text_size(&settings), None);
        for percent in ui::TEXT_SCALES {
            settings
                .extra
                .insert("text_size".into(), text_size_value(percent));
            assert_eq!(chosen_text_size(&settings), Some(percent));
        }
        settings
            .extra
            .insert("text_size".into(), text_size_value(0));
        assert_eq!(chosen_text_size(&settings), None);
        // Values that were never offered fall back to System.
        settings.extra.insert("text_size".into(), json!(300));
        assert_eq!(chosen_text_size(&settings), None);
    }

    #[test]
    fn only_standard_palettes_have_high_contrast_variants() {
        assert_eq!(
            Theme::high_contrast_variant("Shadowroom Dark"),
            Some(shadowroom::theme::HIGH_CONTRAST_DARK)
        );
        assert_eq!(
            Theme::high_contrast_variant("Shadowroom Light"),
            Some(shadowroom::theme::HIGH_CONTRAST_LIGHT)
        );
        assert_eq!(Theme::high_contrast_variant("One Dark"), None);
        let names: Vec<String> = Theme::builtins().into_iter().map(|t| t.name).collect();
        assert!(
            names
                .iter()
                .any(|n| n == shadowroom::theme::HIGH_CONTRAST_DARK)
        );
        assert!(
            names
                .iter()
                .any(|n| n == shadowroom::theme::HIGH_CONTRAST_LIGHT)
        );
    }
}
