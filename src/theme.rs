//! Lossless Zed theme-family import and local theme discovery, independent of GPUI.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub appearance: String,
    /// All top-level string style entries, including unknown future Zed keys.
    pub colors: HashMap<String, String>,
    /// Complete style, including null, nested syntax, players, accents and numbers.
    #[serde(default)]
    pub style: Map<String, Value>,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    /// Theme-level metadata, retained for round trips and future extensions.
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// An immutable palette compiled for the caller's own dense role enum/indices.
/// Rebuild when the selected theme or fallback changes, including overrides.
/// Theme.colors remains public and mutable, so a hidden cache inside Theme
/// would be unsafe: this explicit snapshot cannot silently become stale.
#[derive(Clone, Debug)]
pub struct PreparedColors<const N: usize> {
    colors: [Rgba; N],
}
impl<const N: usize> PreparedColors<N> {
    /// Uses exactly Workspace::c's precedence: theme value, fallback-theme value,
    /// supplied literal; invalid selected values retry the fallback theme, then
    /// use opaque grey. Compile once after theme activation, outside render.
    pub fn new(theme: &Theme, fallback: &Theme, roles: [(&str, &str); N]) -> Self {
        Self {
            colors: roles.map(|(key, literal)| {
                let value = theme
                    .colors
                    .get(key)
                    .or_else(|| fallback.colors.get(key))
                    .map(String::as_str)
                    .unwrap_or(literal);
                Rgba::parse(value)
                    .or_else(|| {
                        fallback
                            .colors
                            .get(key)
                            .and_then(|value| Rgba::parse(value))
                    })
                    .unwrap_or(Rgba {
                        r: 128,
                        g: 128,
                        b: 128,
                        a: 255,
                    })
            }),
        }
    }
    /// Constant-index callers compile to a direct load: no hashing or parsing.
    pub fn get(&self, role: usize) -> Rgba {
        self.colors[role]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self::builtin(
            "Shadowroom Dark",
            "dark",
            &[
                ("background", "#0f0f0f"),
                ("surface.background", "#181818"),
                ("elevated_surface.background", "#202020"),
                ("panel.background", "#181818"),
                ("editor.background", "#0f0f0f"),
                ("title_bar.background", "#181818"),
                ("status_bar.background", "#181818"),
                ("toolbar.background", "#181818"),
                ("text", "#e6e6e6"),
                ("text.muted", "#8a8a8a"),
                ("text.disabled", "#666666"),
                ("text.accent", "#5b9bd5"),
                ("icon", "#b8b8b8"),
                ("icon.muted", "#8a8a8a"),
                ("border", "#292929"),
                ("border.variant", "#333333"),
                ("border.focused", "#5b9bd5"),
                ("element.background", "#202020"),
                ("element.hover", "#202020"),
                ("element.active", "#2a2a2a"),
                ("element.selected", "#232323"),
                ("ghost_element.hover", "#202020"),
                ("ghost_element.selected", "#232323"),
                ("success", "#7fbf7f"),
                ("error", "#d46a6a"),
                ("warning", "#e0b341"),
                ("info", "#5b9bd5"),
                ("scrollbar.thumb.background", "#8a8a8a40"),
                ("scrollbar.thumb.hover_background", "#8a8a8a80"),
                ("editor.selection.background", "#5b9bd533"),
                ("editor.foreground", "#e6e6e6"),
                ("shadowroom.canvas", "#0f0f0f"),
                ("shadowroom.pick", "#7fbf7f"),
                ("shadowroom.reject", "#d46a6a"),
                ("shadowroom.rating", "#e0b341"),
            ],
        )
    }

    pub fn light() -> Self {
        Self::builtin(
            "Shadowroom Light",
            "light",
            &[
                ("background", "#f1f2f4"),
                ("surface.background", "#f8f9fb"),
                ("elevated_surface.background", "#ffffff"),
                ("panel.background", "#f8f9fb"),
                ("editor.background", "#e7e9ed"),
                ("title_bar.background", "#f8f9fb"),
                ("status_bar.background", "#f8f9fb"),
                ("toolbar.background", "#f8f9fb"),
                ("text", "#292e37"),
                ("text.muted", "#626c7c"),
                ("text.disabled", "#8992a0"),
                ("text.accent", "#245c91"),
                ("icon", "#434e5f"),
                ("icon.muted", "#626c7c"),
                ("border", "#cbd1da"),
                ("border.variant", "#dce0e7"),
                ("border.focused", "#3979b4"),
                ("element.background", "#e5e9ef"),
                ("element.hover", "#dce2ea"),
                ("element.active", "#ced8e5"),
                ("element.selected", "#3979b429"),
                ("ghost_element.hover", "#1728420a"),
                ("ghost_element.selected", "#3979b418"),
                ("success", "#39734f"),
                ("error", "#a53e42"),
                ("warning", "#8a661f"),
                ("info", "#245c91"),
                ("scrollbar.thumb.background", "#626c7c40"),
                ("scrollbar.thumb.hover_background", "#626c7c80"),
                ("editor.selection.background", "#3979b433"),
                ("editor.foreground", "#292e37"),
                ("shadowroom.canvas", "#dce0e6"),
                ("shadowroom.pick", "#39734f"),
                ("shadowroom.reject", "#a53e42"),
                ("shadowroom.rating", "#8a661f"),
            ],
        )
    }

    fn builtin(name: &str, appearance: &str, pairs: &[(&str, &str)]) -> Self {
        let mut colors: HashMap<_, _> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (key, source) in [
            ("text.placeholder", "text.muted"),
            ("icon.placeholder", "icon.muted"),
            ("icon.disabled", "text.disabled"),
            ("icon.accent", "text.accent"),
            ("title_bar.inactive_background", "panel.background"),
            ("tab_bar.background", "panel.background"),
            ("tab.inactive_background", "panel.background"),
            ("tab.active_background", "background"),
            ("border.selected", "border.focused"),
            ("border.disabled", "border.variant"),
            ("element.disabled", "element.background"),
            ("ghost_element.active", "element.active"),
            ("ghost_element.disabled", "element.disabled"),
            ("panel.focused_border", "border.focused"),
            ("pane.focused_border", "border.focused"),
            ("scrollbar.thumb.border", "border.variant"),
            ("scrollbar.track.border", "border.variant"),
            ("link_text.hover", "text.accent"),
            ("search.match_background", "editor.selection.background"),
            ("drop_target.background", "editor.selection.background"),
        ] {
            if let Some(value) = colors.get(source).cloned() {
                colors.insert(key.into(), value);
            }
        }
        for key in [
            "ghost_element.background",
            "border.transparent",
            "scrollbar.track.background",
        ] {
            colors.insert(key.into(), "#00000000".into());
        }
        for status in ["success", "error", "warning", "info"] {
            if let Some(mut color) = colors.get(status).and_then(|s| Rgba::parse(s)) {
                color.a = 26;
                colors.insert(format!("{status}.background"), color.to_hex());
                color.a = 128;
                colors.insert(format!("{status}.border"), color.to_hex());
            }
        }
        let style = colors
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        Self {
            name: name.into(),
            appearance: appearance.into(),
            colors,
            style,
            family: "Shadowroom".into(),
            author: "Shadowroom".into(),
            source: None,
            metadata: Map::new(),
        }
    }

    /// Accepts a Zed family, single theme, or array. JSONC comments and trailing
    /// commas are accepted; quoted URLs and comment-like string contents survive.
    pub fn from_zed_json(json: &str) -> Result<Vec<Self>> {
        let value: Value =
            serde_json::from_str(&normalize_jsonc(json)?).context("Invalid Zed theme JSON")?;
        let family = value.get("name").and_then(Value::as_str).unwrap_or("");
        let author = value.get("author").and_then(Value::as_str).unwrap_or("");
        let entries: Vec<&Value> = if let Some(themes) = value.get("themes") {
            themes
                .as_array()
                .context("Zed family 'themes' must be an array")?
                .iter()
                .collect()
        } else if let Some(array) = value.as_array() {
            array.iter().collect()
        } else {
            vec![&value]
        };
        if entries.is_empty() {
            bail!("Theme family contains no themes");
        }
        entries
            .into_iter()
            .map(|entry| {
                let object = entry.as_object().context("Theme must be an object")?;
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .context("Theme requires a name")?
                    .to_string();
                let appearance = entry
                    .get("appearance")
                    .and_then(Value::as_str)
                    .unwrap_or("dark")
                    .to_string();
                if appearance != "dark" && appearance != "light" {
                    bail!("Theme {name:?} has invalid appearance {appearance:?}");
                }
                let style = entry
                    .get("style")
                    .and_then(Value::as_object)
                    .context("Theme requires a style object")?
                    .clone();
                let colors = style
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect();
                let mut metadata = object.clone();
                for key in ["name", "appearance", "style"] {
                    metadata.remove(key);
                }
                Ok(Self {
                    name,
                    appearance,
                    colors,
                    style,
                    family: family.into(),
                    author: author.into(),
                    source: None,
                    metadata,
                })
            })
            .collect()
    }

    /// Missing, null and unsupported color formats use the supplied fallback.
    /// Supported alpha values are preserved, never silently truncated.
    pub fn color(&self, key: &str, fallback: &str) -> String {
        self.colors
            .get(key)
            .filter(|s| Rgba::parse(s).is_some())
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    }

    pub fn color_rgba(&self, key: &str, fallback: Rgba) -> Rgba {
        self.colors
            .get(key)
            .and_then(|s| Rgba::parse(s))
            .unwrap_or(fallback)
    }

    /// Resolve a missing theme color against the same-appearance built-in palette.
    pub fn resolved_color(&self, key: &str) -> String {
        static LIGHT: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
        static DARK: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
        let base = if self.appearance == "light" {
            LIGHT.get_or_init(Self::light)
        } else {
            DARK.get_or_init(Self::dark)
        };
        self.color(
            key,
            base.colors
                .get(key)
                .map(String::as_str)
                .unwrap_or("#808080"),
        )
    }

    /// Recursively merge Zed overrides. Null and unknown style values survive;
    /// arrays replace arrays, while nested syntax attributes merge independently.
    pub fn apply_style_overrides(&mut self, overrides: &Map<String, Value>) {
        merge_style(&mut self.style, overrides);
        self.colors = self
            .style
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|value| (k.clone(), value.into())))
            .collect();
    }

    /// Contrast of explicitly supplied roles. Missing/invalid roles return None;
    /// translucent backgrounds composite over the theme's main background.
    pub fn contrast_ratio(&self, foreground: &str, background: &str) -> Option<f64> {
        let foreground = self.colors.get(foreground).and_then(|s| Rgba::parse(s))?;
        let background = self.colors.get(background).and_then(|s| Rgba::parse(s))?;
        let opaque = if self.appearance == "light" {
            Rgba::parse("#f1f2f4").unwrap()
        } else {
            Rgba::parse("#0f0f0f").unwrap()
        };
        let canvas = self
            .colors
            .get("background")
            .and_then(|s| Rgba::parse(s))
            .unwrap_or(opaque)
            .composite_over(opaque);
        Some(foreground.contrast_ratio(background.composite_over(canvas)))
    }

    /// Advisory diagnostics only: external theme colors are never rewritten.
    pub fn accessibility_diagnostics(&self) -> Vec<String> {
        let mut issues = Vec::new();
        for key in ["text", "background"] {
            if !self.colors.contains_key(key) {
                issues.push(format!(
                    "{}: sparse theme lacks {key}; appearance fallback is used",
                    self.name
                ));
            }
        }
        for key in [
            "text",
            "text.muted",
            "text.placeholder",
            "icon",
            "icon.muted",
            "background",
            "panel.background",
            "title_bar.background",
            "status_bar.background",
            "ghost_element.hover",
            "element.selected",
        ] {
            if let Some(value) = self.colors.get(key) {
                if Rgba::parse(value).is_none() {
                    issues.push(format!(
                        "{}: unsupported {key} color {value:?}; appearance fallback is used",
                        self.name
                    ));
                }
            }
        }
        for (foreground, background, minimum) in [
            ("text", "background", 4.5),
            ("text", "panel.background", 4.5),
            ("text", "title_bar.background", 4.5),
            ("text", "status_bar.background", 4.5),
            ("text.muted", "panel.background", 4.5),
            ("text.placeholder", "panel.background", 4.5),
            ("icon", "panel.background", 3.0),
        ] {
            if let Some(ratio) = self.contrast_ratio(foreground, background) {
                if ratio < minimum {
                    issues.push(format!("{}: {foreground} on {background} contrast {ratio:.2}:1 is below {minimum}:1 (advisory; colors preserved)",self.name));
                }
            }
        }
        issues
    }
}

fn merge_style(target: &mut Map<String, Value>, overrides: &Map<String, Value>) {
    for (key, value) in overrides {
        if let (Some(Value::Object(existing)), Value::Object(replacement)) =
            (target.get_mut(key), value)
        {
            merge_style(existing, replacement);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// Zed colors use CSS hexadecimal order: #RGBA / #RRGGBBAA (alpha last).
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        if s.eq_ignore_ascii_case("transparent") {
            return Some(Self {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            });
        }
        let h = s.strip_prefix('#')?;
        if !h.is_ascii() {
            return None;
        }
        let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&h[range], 16).ok();
        match h.len() {
            3 | 4 => {
                let digit = |i| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
                Some(Self {
                    r: digit(0)?,
                    g: digit(1)?,
                    b: digit(2)?,
                    a: if h.len() == 4 { digit(3)? } else { 255 },
                })
            }
            6 | 8 => Some(Self {
                r: byte(0..2)?,
                g: byte(2..4)?,
                b: byte(4..6)?,
                a: if h.len() == 8 { byte(6..8)? } else { 255 },
            }),
            _ => None,
        }
    }
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
    pub fn composite_over(self, background: Self) -> Self {
        let a = self.a as f64 / 255.0;
        let ba = background.a as f64 / 255.0;
        let out = a + ba * (1.0 - a);
        if out == 0.0 {
            return Self {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            };
        }
        let blend = |f: u8, b: u8| ((f as f64 * a + b as f64 * ba * (1.0 - a)) / out).round() as u8;
        Self {
            r: blend(self.r, background.r),
            g: blend(self.g, background.g),
            b: blend(self.b, background.b),
            a: (out * 255.0).round() as u8,
        }
    }
    pub fn relative_luminance(self) -> f64 {
        let linear = |channel: u8| {
            let value = channel as f64 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }
    /// Foreground alpha composites over an opaque supplied background.
    pub fn contrast_ratio(self, background: Self) -> f64 {
        let a = self.composite_over(background).relative_luminance();
        let b = background.relative_luminance();
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ThemeDiscovery {
    pub themes: Vec<Theme>,
    pub diagnostics: Vec<String>,
}

/// Config themes override installed extensions of the same name; Shadowroom
/// themes override Zed themes. Files are sorted for deterministic selection.
pub fn discover_themes() -> ThemeDiscovery {
    discover_themes_in(&theme_search_paths())
}

pub fn theme_search_paths() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = directories::BaseDirs::new() {
        let h = home.home_dir();
        // Linux stable/preview/dev installs and XDG overrides.
        for app in ["zed", "zed-preview", "zed-dev"] {
            roots.push(home.data_dir().join(app).join("extensions/installed"));
            roots.push(
                h.join(".local/share")
                    .join(app)
                    .join("extensions/installed"),
            );
            roots.push(
                h.join("Library/Application Support")
                    .join(app)
                    .join("extensions/installed"),
            );
            roots.push(home.data_dir().join(app).join("extensions/work"));
        }
        // Windows Zed uses capitalized Roaming/Local directories.
        roots.push(home.data_local_dir().join("Zed/extensions/installed"));
        // Every local config theme has precedence over every extension edition.
        // Stable comes last so an installed preview does not replace its choices.
        for app in ["zed-dev", "zed-preview", "zed"] {
            roots.push(home.config_dir().join(app).join("themes"));
            roots.push(h.join(".config").join(app).join("themes"));
        }
        roots.push(home.config_dir().join("Zed/themes"));
        roots.push(home.config_dir().join("shadowroom/themes"));
        roots.push(h.join(".config/shadowroom/themes"));
    }
    let mut seen = HashSet::new();
    roots.retain(|p| seen.insert(p.clone()));
    roots
}

pub fn discover_themes_in(roots: &[PathBuf]) -> ThemeDiscovery {
    let mut result = ThemeDiscovery {
        themes: vec![Theme::dark(), Theme::light()],
        diagnostics: Vec::new(),
    };
    let mut seen = HashSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(root).max_depth(8).follow_links(false) {
            match entry {
                Ok(entry) if entry.file_type().is_file() => {
                    let path = entry.path();
                    let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("");
                    // Extension manifests, language files and package metadata are not themes.
                    if (extension.eq_ignore_ascii_case("json")
                        || extension.eq_ignore_ascii_case("jsonc"))
                        && (root.ends_with("themes")
                            || path.strip_prefix(root).is_ok_and(|relative| {
                                relative.ancestors().skip(1).any(|p| p.ends_with("themes"))
                            }))
                    {
                        files.push(path.to_path_buf());
                    }
                }
                Ok(_) => {}
                Err(error) => result.diagnostics.push(error.to_string()),
            }
        }
        files.sort();
        for path in files {
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical) {
                continue;
            }
            match read_theme_file(&path) {
                Ok(themes) => {
                    for mut theme in themes {
                        theme.source = Some(path.clone());
                        if let Some(index) = result.themes.iter().position(|t| t.name == theme.name)
                        {
                            result.themes[index] = theme;
                        } else {
                            result.themes.push(theme);
                        }
                    }
                }
                Err(error) => result
                    .diagnostics
                    .push(format!("{}: {error:#}", path.display())),
            }
        }
    }
    result
}

pub fn read_theme_file(path: &Path) -> Result<Vec<Theme>> {
    // Bound untrusted local files to avoid allocating without limit during discovery.
    if fs::metadata(path)?.len() > 16 * 1024 * 1024 {
        bail!("Theme exceeds 16 MiB");
    }
    Theme::from_zed_json(
        &fs::read_to_string(path).with_context(|| format!("Read {}", path.display()))?,
    )
}

pub(crate) fn normalize_jsonc(input: &str) -> Result<String> {
    let mut bytes = input.trim_start_matches('\u{feff}').as_bytes().to_vec();
    let mut i = 0;
    let mut quoted = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => {
                i += 2;
                continue;
            }
            b'"' => quoted = !quoted,
            b'/' if !quoted && bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    bytes[i] = b' ';
                    i += 1;
                }
                continue;
            }
            b'/' if !quoted && bytes.get(i + 1) == Some(&b'*') => {
                bytes[i] = b' ';
                bytes[i + 1] = b' ';
                i += 2;
                loop {
                    if i + 1 >= bytes.len() {
                        bail!("Unterminated JSON block comment");
                    }
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        bytes[i] = b' ';
                        bytes[i + 1] = b' ';
                        i += 2;
                        break;
                    }
                    if bytes[i] != b'\n' && bytes[i] != b'\r' {
                        bytes[i] = b' ';
                    }
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    quoted = false;
    i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => {
                i += 2;
                continue;
            }
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
                if matches!(next, Some(b'}' | b']')) {
                    bytes[i] = b' ';
                }
            }
            _ => {}
        }
        i += 1;
    }
    String::from_utf8(bytes).context("Invalid UTF-8 in JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contrast_diagnostics_preserve_theme_fidelity_and_alpha() {
        assert!(
            (Rgba::parse("#fff")
                .unwrap()
                .contrast_ratio(Rgba::parse("#000").unwrap())
                - 21.0)
                .abs()
                < 0.0001
        );
        let mut theme=Theme::from_zed_json(r##"{"name":"Low contrast","style":{"text":"#11111180","background":"#111111","panel.background":"#111111","icon":"#111"}}"##).unwrap().remove(0);
        assert_eq!(theme.contrast_ratio("text", "background"), Some(1.0));
        assert!(
            theme
                .accessibility_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.contains("below 4.5"))
        );
        assert_eq!(theme.color("text", "#fff"), "#11111180");
        theme.apply_style_overrides(
            serde_json::json!({"text":null,"future":{"a":[1,2]}})
                .as_object()
                .unwrap(),
        );
        assert!(theme.style["text"].is_null());
        assert_eq!(theme.color("text", "#fff"), "#fff");
        assert!(
            theme
                .accessibility_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.contains("sparse"))
        );
        for base in [Theme::dark(), Theme::light()] {
            for key in [
                "icon.accent",
                "icon.disabled",
                "text.placeholder",
                "title_bar.inactive_background",
                "ghost_element.active",
                "error.background",
                "scrollbar.track.background",
            ] {
                assert!(Rgba::parse(&base.resolved_color(key)).is_some());
            }
            assert!(base.contrast_ratio("text", "panel.background").unwrap() >= 4.5);
        }
    }
    #[test]
    fn imports_family_losslessly_with_optional_values() {
        let family = r##"{ // comment
            "name":"Example", "author":"A", "themes":[{
            "name":"Cool", "appearance":"dark", "style":{
                "background":"#1238", "text":null,
                "syntax":{"comment":{"color":"#ABCDEF80","font_style":"italic"}},
                "players":[{"cursor":"#fff"}], "future":42,
                "url":"https://example.org//theme",},},],}"##;
        let themes = Theme::from_zed_json(family).unwrap();
        let t = &themes[0];
        assert_eq!(t.color("background", "#000"), "#1238");
        assert_eq!(t.color("text", "#fff"), "#fff");
        assert!(t.style["text"].is_null());
        assert_eq!(t.style["syntax"]["comment"]["font_style"], "italic");
        assert_eq!(t.style["future"], 42);
        assert_eq!(t.style["url"], "https://example.org//theme");
        assert_eq!(t.author, "A");
    }
    #[test]
    fn prepared_colors_preserve_workspace_fallback_precedence_and_are_snapshots() {
        let mut theme = Theme::dark();
        let mut base = Theme::light();
        theme.colors.insert("text".into(), "unsupported".into());
        base.colors.insert("invalid".into(), "unsupported".into());
        let roles = [
            ("text", "#123"),
            ("missing", "#1234"),
            ("invalid", "#fff"),
            ("background", "#fff"),
        ];
        let prepared = PreparedColors::new(&theme, &base, roles);
        for (i, (key, fallback)) in roles.into_iter().enumerate() {
            let selected = theme
                .colors
                .get(key)
                .or_else(|| base.colors.get(key))
                .map(String::as_str)
                .unwrap_or(fallback);
            let expected = Rgba::parse(selected)
                .or_else(|| base.colors.get(key).and_then(|c| Rgba::parse(c)))
                .unwrap_or(Rgba {
                    r: 128,
                    g: 128,
                    b: 128,
                    a: 255,
                });
            assert_eq!(prepared.get(i), expected);
        }
        let old = prepared.get(3);
        theme.colors.insert("background".into(), "#123".into());
        assert_eq!(prepared.get(3), old);
        assert_ne!(PreparedColors::new(&theme, &base, roles).get(3), old);
    }
    #[test]
    #[ignore]
    fn bench_theme_colors() {
        let theme = Theme::dark();
        let began = std::time::Instant::now();
        for _ in 0..1_000_000 {
            std::hint::black_box(theme.color_rgba(
                std::hint::black_box("panel.background"),
                Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                },
            ));
        }
        eprintln!("theme lookup and parse 1m {:?}", began.elapsed());
        let palette = PreparedColors::new(&theme, &Theme::dark(), [("panel.background", "#000")]);
        let began = std::time::Instant::now();
        for _ in 0..1_000_000 {
            std::hint::black_box(std::hint::black_box(&palette).get(0));
        }
        eprintln!("prepared palette 1m {:?}", began.elapsed());
    }
    #[test]
    fn colors_preserve_alpha_and_reject_bad_input() {
        assert_eq!(Rgba::parse("#1234").unwrap().to_hex(), "#11223344");
        assert_eq!(Rgba::parse("#AABBCC80").unwrap().a, 128);
        assert_eq!(Rgba::parse("#fff").unwrap().a, 255);
        assert!(Rgba::parse("#☃fff").is_none());
        assert!(Rgba::parse("#12xz").is_none());
        assert_eq!(
            Rgba::parse("#ff000080")
                .unwrap()
                .composite_over(Rgba::parse("#000").unwrap()),
            Rgba {
                r: 128,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }
    #[test]
    fn discovery_is_deterministic_and_reports_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let extension = dir.path().join("installed/example/themes");
        let local = dir.path().join("themes");
        fs::create_dir_all(&extension).unwrap();
        fs::create_dir_all(&local).unwrap();
        fs::write(
            extension.join("one.json"),
            r##"{"name":"One","style":{"text":"#fff"}}"##,
        )
        .unwrap();
        fs::write(
            local.join("one.json"),
            r##"{"name":"One","style":{"text":"#000"}}"##,
        )
        .unwrap();
        fs::write(local.join("broken.json"), "bad").unwrap();
        let result = discover_themes_in(&[dir.path().join("installed"), local]);
        assert_eq!(result.themes.iter().filter(|t| t.name == "One").count(), 1);
        assert_eq!(
            result
                .themes
                .iter()
                .find(|t| t.name == "One")
                .unwrap()
                .color("text", ""),
            "#000"
        );
        assert_eq!(result.diagnostics.len(), 1);
    }
    #[test]
    fn malformed_families_return_errors() {
        for json in [
            "{}",
            "[]",
            "{\"themes\":{}}",
            "/* incomplete",
            "{\"themes\":[]}",
        ] {
            assert!(Theme::from_zed_json(json).is_err());
        }
    }
}
