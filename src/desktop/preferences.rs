//! Preference edits stay private until Apply, including imported theme families.
use super::{Cmd, Settings, Theme, Workspace, persist_theme_source, read_theme_family};
use gpui::{Context, PathPromptOptions};
use serde_json::{Value, json};

pub(super) struct Draft {
    pub id: u64,
    baseline: Settings,
    pub settings: Settings,
    pub themes: Vec<Theme>,
    pub imports: Vec<String>,
    pub applying: bool,
}

impl Workspace {
    pub(super) fn preference_settings(&self) -> &Settings {
        self.preferences_draft
            .as_ref()
            .map_or(&self.settings, |draft| &draft.settings)
    }

    pub(super) fn preference_themes(&self) -> &[Theme] {
        self.preferences_draft
            .as_ref()
            .map_or(&self.themes, |draft| &draft.themes)
    }

    pub(super) fn open_preferences(&mut self) {
        if self.preferences_draft.is_none() {
            self.preferences_draft = Some(Draft::new(&self.settings, &self.themes));
        }
        self.preferences = true;
    }

    pub(super) fn close_preferences(&mut self) {
        self.preferences = false;
        self.preferences_draft = None;
    }

    fn finish_preferences(&mut self, patch: &Value) {
        match self.update_settings(patch) {
            Ok(_) => self.close_preferences(),
            Err(error) => {
                if let Some(draft) = &mut self.preferences_draft {
                    draft.applying = false;
                }
                self.error = Some(error);
            }
        }
    }

    pub(super) fn apply_preferences(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.preferences_draft else {
            return;
        };
        if !self.settings_writable {
            self.error = Some("Repair the preferences file before applying changes".into());
            return;
        }
        if !draft.changed() {
            self.close_preferences();
            return;
        }
        let patch = draft.patch();
        if draft.imports.is_empty() {
            self.finish_preferences(&patch);
            return;
        }
        draft.applying = true;
        let id = draft.id;
        let imports = draft.imports.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut installed = Vec::new();
                    for source in imports {
                        installed.extend(persist_theme_source(&source, None)?);
                    }
                    anyhow::Ok(installed)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this
                    .preferences_draft
                    .as_ref()
                    .is_some_and(|draft| draft.id == id)
                {
                    return;
                }
                match result {
                    Ok(themes) => {
                        for theme in themes {
                            this.themes.retain(|t| t.name != theme.name);
                            this.themes.push(theme);
                        }
                        this.finish_preferences(&patch);
                    }
                    Err(error) => {
                        this.preferences_draft.as_mut().unwrap().applying = false;
                        this.error = Some(format!("Theme import: {error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn choose_preference_theme(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.preferences_draft.as_ref().map(|draft| draft.id) else {
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose Zed theme JSON".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await {
                if let Some(path) = paths.first() {
                    let path = path.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { read_theme_family(&path) })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(draft) = &mut this.preferences_draft {
                            if draft.id != id || draft.applying {
                                return;
                            }
                            match result {
                                Ok((source, themes)) => draft.import(source, themes),
                                Err(error) => this.error = Some(format!("Theme import: {error:#}")),
                            }
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }
}

impl Draft {
    pub fn new(settings: &Settings, themes: &[Theme]) -> Self {
        Self {
            id: rand::random(),
            baseline: settings.clone(),
            settings: settings.clone(),
            themes: themes.to_vec(),
            imports: Vec::new(),
            applying: false,
        }
    }

    /// Only edited fields are committed, preserving unrelated live workspace state.
    pub fn patch(&self) -> Value {
        let before = serde_json::to_value(&self.baseline).expect("serializable settings");
        let mut after = serde_json::to_value(&self.settings).expect("serializable settings");
        after
            .as_object_mut()
            .unwrap()
            .retain(|key, value| before.get(key) != Some(value));
        after
    }

    pub fn changed(&self) -> bool {
        self.settings != self.baseline || !self.imports.is_empty()
    }

    pub fn import(&mut self, source: String, themes: Vec<Theme>) {
        if let Some(theme) = themes.first() {
            self.settings.theme = theme.name.clone();
            self.settings
                .extra
                .insert("theme_mode".into(), json!("manual"));
        }
        for theme in themes {
            self.themes.retain(|t| t.name != theme.name);
            self.themes.push(theme);
        }
        if !self.imports.contains(&source) {
            self.imports.push(source);
        }
    }

    pub fn edit(&mut self, cmd: &Cmd, volume: Option<&str>) -> bool {
        let s = &mut self.settings;
        match cmd {
            Cmd::SetAutoAdvance(enabled) => s.auto_advance = *enabled,
            Cmd::SetAutoDrives(enabled) => s.auto_discover_drives = *enabled,
            Cmd::Font(font) => s.interface_font = font.clone(),
            Cmd::Group(mode) => s.grouping_mode = mode.clone(),
            Cmd::Gap(gap) => s.burst_gap_ms = *gap,
            Cmd::Threshold(threshold) => s.visual_threshold = *threshold,
            Cmd::Memory(mb) => s.memory_budget_mb = *mb,
            Cmd::Disk(mb) => s.cache_budget_mb = *mb,
            Cmd::Theme(name) if self.themes.iter().any(|t| t.name == *name) => {
                s.theme = name.clone();
                s.extra.insert("theme_mode".into(), json!("manual"));
            }
            Cmd::ThemeMode(mode) => {
                s.extra.insert("theme_mode".into(), json!(mode));
            }
            Cmd::SystemTheme(name) => {
                if let Some(theme) = self.themes.iter().find(|t| t.name == *name) {
                    let slot = if theme.appearance == "light" {
                        "theme_light"
                    } else {
                        "theme_dark"
                    };
                    s.extra.insert(slot.into(), json!(name));
                    s.extra.insert("theme_mode".into(), json!("system"));
                }
            }
            Cmd::Ingest(action) => match action.as_str() {
                "insert" => {
                    s.ingest_on_insert = match s.ingest_on_insert.as_str() {
                        "ask" => "auto",
                        "auto" => "index_in_place",
                        "index_in_place" => "ignore",
                        _ => "ask",
                    }
                    .into()
                }
                "day" => s.ingest_day_start_hour = (s.ingest_day_start_hour + 1) % 24,
                "auto_eject" => s.eject_after_ingest = !s.eject_after_ingest,
                "remember" => {
                    if let Some(volume) = volume {
                        let rule = if s.ingest_volumes.get(volume).is_some_and(|r| r == "deny") {
                            "allow"
                        } else {
                            "deny"
                        };
                        s.ingest_volumes.insert(volume.into(), rule.into());
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsing_preferences_never_changes_live_settings() {
        let active = Settings::default();
        let original = active.clone();
        let mut draft = Draft::new(&active, &[Theme::dark(), Theme::light()]);
        for cmd in [
            Cmd::SetAutoAdvance(false),
            Cmd::SetAutoDrives(false),
            Cmd::Font("DM Sans".into()),
            Cmd::Theme("Shadowroom Light".into()),
            Cmd::Group("off".into()),
            Cmd::Memory(128),
            Cmd::Disk(2048),
            Cmd::Gap(500),
            Cmd::Threshold(14),
            Cmd::Ingest("insert".into()),
            Cmd::Ingest("day".into()),
            Cmd::Ingest("auto_eject".into()),
            Cmd::Ingest("remember".into()),
        ] {
            assert!(draft.edit(&cmd, Some("test-volume")));
        }
        assert!(draft.changed());
        assert_eq!(active, original);
        assert_eq!(draft.settings.ingest_on_insert, "auto");
        assert!(draft.settings.eject_after_ingest);
        drop(draft); // Cancel/Escape drops the draft without any writer command.
        assert_eq!(active, original);
    }

    #[test]
    fn applying_only_commits_edited_fields() {
        let active = Settings::default();
        let mut draft = Draft::new(&active, &[]);
        assert!(!draft.changed());
        assert_eq!(draft.patch(), json!({}));
        draft.edit(&Cmd::Memory(2048), None);
        assert_eq!(draft.patch(), json!({"memory_budget_mb":2048}));
        let mut concurrent = active.clone();
        concurrent.last_photo_id = Some(42);
        concurrent.sidebar_width = 300;
        let mut merged = serde_json::to_value(&concurrent).unwrap();
        for (key, value) in draft.patch().as_object().unwrap() {
            merged[key] = value.clone();
        }
        let applied: Settings = serde_json::from_value(merged).unwrap();
        assert_eq!(applied.memory_budget_mb, 2048);
        assert_eq!(applied.last_photo_id, Some(42));
        assert_eq!(applied.sidebar_width, 300);
        draft.edit(&Cmd::Memory(active.memory_budget_mb), None);
        assert!(!draft.changed());
        assert_eq!(draft.patch(), json!({}));
    }

    #[test]
    fn imported_themes_remain_in_the_draft_until_apply() {
        let active = Settings::default();
        let mut draft = Draft::new(&active, &[Theme::dark()]);
        draft.import("demo source".into(), vec![Theme::light()]);
        assert_eq!(active.theme, "Shadowroom Dark");
        assert_eq!(draft.settings.theme, "Shadowroom Light");
        assert_eq!(draft.imports, vec!["demo source"]);
        assert_eq!(draft.patch()["theme"], "Shadowroom Light");
        assert!(draft.themes.iter().any(|t| t.name == "Shadowroom Light"));
    }
}
