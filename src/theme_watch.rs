//! Metadata-first theme hot reload. Unchanged polls neither enumerate directories
//! nor read JSON. Changed files are parsed independently and results replace a
//! single pending update; the GPUI thread never walks the filesystem.
use crate::theme::{Theme, normalize_jsonc, read_theme_file, theme_search_paths};
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZedSelection {
    pub mode: String,
    pub light: Option<String>,
    pub dark: Option<String>,
    pub name: Option<String>,
}
impl ZedSelection {
    pub fn selected_name(&self, system_dark: bool) -> Option<&str> {
        if let Some(name) = &self.name {
            return Some(name);
        }
        match self.mode.as_str() {
            "light" => self.light.as_deref(),
            "dark" => self.dark.as_deref(),
            _ => {
                if system_dark {
                    self.dark.as_deref()
                } else {
                    self.light.as_deref()
                }
            }
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct ZedSettings {
    pub selection: Option<ZedSelection>,
    pub overrides: Map<String, Value>,
}
pub fn parse_zed_settings(json: &str) -> Result<ZedSettings> {
    let value: Value =
        serde_json::from_str(&normalize_jsonc(json)?).context("Parse Zed settings JSONC")?;
    if !value.is_object() {
        bail!("Zed settings must be an object");
    }
    let selection = match value.get("theme") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(ZedSelection {
            mode: "static".into(),
            name: Some(name.clone()),
            light: None,
            dark: None,
        }),
        Some(Value::Object(theme)) => {
            let mode = match theme.get("mode") {
                None | Some(Value::Null) => "system",
                Some(Value::String(mode)) => mode.as_str(),
                _ => bail!("Zed theme.mode must be a string"),
            };
            if !["system", "light", "dark"].contains(&mode) {
                bail!("Unsupported Zed theme mode {mode:?}");
            }
            let string = |key: &str| -> Result<Option<String>> {
                match theme.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(name)) => Ok(Some(name.clone())),
                    _ => bail!("Zed theme.{key} must be a string"),
                }
            };
            Some(ZedSelection {
                mode: mode.into(),
                name: None,
                light: string("light")?,
                dark: string("dark")?,
            })
        }
        _ => bail!("Zed theme must be a string or mode/light/dark object"),
    };
    let overrides = match value.get("theme_overrides") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(overrides)) => overrides.clone(),
        _ => bail!("Zed theme_overrides must be an object"),
    };
    Ok(ZedSettings {
        selection,
        overrides,
    })
}

#[derive(Clone, Debug)]
pub struct ThemeUpdate {
    pub themes: Vec<Theme>,
    pub diagnostics: Vec<String>,
    pub zed_theme: Option<ZedSelection>,
}
#[derive(Clone, Debug)]
pub struct ThemeWatchPaths {
    pub theme_roots: Vec<PathBuf>,
    pub zed_settings: Vec<PathBuf>,
}
impl Default for ThemeWatchPaths {
    fn default() -> Self {
        let mut settings = Vec::new();
        if let Some(base) = directories::BaseDirs::new() {
            settings.push(base.config_dir().join("zed/settings.json"));
            settings.push(base.home_dir().join(".config/zed/settings.json"));
            settings.push(base.config_dir().join("Zed/settings.json"));
        }
        let mut seen = HashSet::new();
        settings.retain(|p| seen.insert(p.clone()));
        Self {
            theme_roots: theme_search_paths(),
            zed_settings: settings,
        }
    }
}
pub struct ThemeWatcher {
    stop: Sender<()>,
}
impl ThemeWatcher {
    /// Default polls are two seconds; fixtures may request a shorter interval.
    pub fn start_with_paths(
        paths: ThemeWatchPaths,
        interval: Duration,
    ) -> (Self, Receiver<ThemeUpdate>) {
        let interval = interval.clamp(Duration::from_millis(20), Duration::from_secs(2));
        let (stop, stopped) = bounded(1);
        let (send, updates) = bounded(1);
        let obsolete = updates.clone();
        thread::Builder::new()
            .name("shadowroom-themes".into())
            .spawn(move || {
                let mut engine = ThemeEngine::new(paths);
                loop {
                    let began = Instant::now();
                    if let Some(update) = engine.poll() {
                        match send.try_send(update) {
                            Ok(()) => {}
                            Err(TrySendError::Full(update)) => {
                                let _ = obsolete.try_recv();
                                let _ = send.try_send(update);
                            }
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    match stopped.recv_timeout(
                        interval
                            .saturating_sub(began.elapsed())
                            .max(Duration::from_millis(20)),
                    ) {
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })
            .expect("Start theme watcher");
        (Self { stop }, updates)
    }
    pub fn stop(&self) {
        let _ = self.stop.try_send(());
    }
}
impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}
pub fn start_theme_watch() -> (ThemeWatcher, Receiver<ThemeUpdate>) {
    ThemeWatcher::start_with_paths(ThemeWatchPaths::default(), Duration::from_secs(2))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stamp {
    Missing,
    Error(std::io::ErrorKind),
    Present {
        length: u64,
        modified: Option<SystemTime>,
        directory: bool,
        #[cfg(unix)]
        identity: (u64, u64, i64, i64),
    },
}
fn stamp(path: &Path) -> Stamp {
    match fs::metadata(path) {
        Ok(metadata) => Stamp::Present {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            directory: metadata.is_dir(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stamp::Missing,
        Err(error) => Stamp::Error(error.kind()),
    }
}
#[derive(Default)]
struct DirectoryIndex {
    directories: HashMap<PathBuf, Directory>,
    #[cfg(test)]
    listings: usize,
}
struct Directory {
    stamp: Stamp,
    children: Vec<PathBuf>,
    files: Vec<PathBuf>,
    error: Option<String>,
}
impl DirectoryIndex {
    fn files(&mut self, roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
        let mut visited = HashSet::new();
        let mut files = Vec::new();
        let mut diagnostics = Vec::new();
        for root in roots {
            let mut local_files = Vec::new();
            self.visit(
                root,
                root,
                0,
                &mut visited,
                &mut local_files,
                &mut diagnostics,
            );
            local_files.sort();
            files.extend(local_files);
        }
        self.directories.retain(|path, _| visited.contains(path));
        (files, diagnostics)
    }
    fn visit(
        &mut self,
        root: &Path,
        path: &Path,
        depth: usize,
        visited: &mut HashSet<PathBuf>,
        files: &mut Vec<PathBuf>,
        diagnostics: &mut Vec<String>,
    ) {
        if depth >= 8 || !visited.insert(path.into()) {
            return;
        }
        let current = stamp(path);
        if self
            .directories
            .get(path)
            .is_none_or(|cached| cached.stamp != current)
        {
            #[cfg(test)]
            if matches!(
                &current,
                Stamp::Present {
                    directory: true,
                    ..
                }
            ) {
                self.listings += 1;
            }
            let mut record = Directory {
                stamp: current.clone(),
                children: Vec::new(),
                files: Vec::new(),
                error: None,
            };
            match current {
                Stamp::Missing => {}
                Stamp::Present {
                    directory: true, ..
                } => match fs::read_dir(path) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry.and_then(|entry| {
                                entry.file_type().map(|kind| (entry.path(), kind))
                            }) {
                                Ok((path, kind)) if kind.is_dir() => {
                                    let extension_root =
                                        root.ends_with("installed") || root.ends_with("work");
                                    let already_in_themes =
                                        path.strip_prefix(root).is_ok_and(|relative| {
                                            relative
                                                .ancestors()
                                                .any(|parent| parent.ends_with("themes"))
                                        });
                                    if !extension_root || depth < 2 || already_in_themes {
                                        record.children.push(path);
                                    }
                                }
                                Ok((path, kind)) if kind.is_file() => {
                                    let json = path
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .is_some_and(|e| {
                                            e.eq_ignore_ascii_case("json")
                                                || e.eq_ignore_ascii_case("jsonc")
                                        });
                                    let theme = root.ends_with("themes")
                                        || path.strip_prefix(root).is_ok_and(|relative| {
                                            relative
                                                .ancestors()
                                                .skip(1)
                                                .any(|parent| parent.ends_with("themes"))
                                        });
                                    if json && theme {
                                        record.files.push(path);
                                    }
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    record.error = Some(format!("{}: {error}", path.display()))
                                }
                            }
                        }
                    }
                    Err(error) => record.error = Some(format!("{}: {error}", path.display())),
                },
                _ => {
                    record.error =
                        Some(format!("Cannot inspect theme directory {}", path.display()))
                }
            }
            record.children.sort();
            record.files.sort();
            self.directories.insert(path.into(), record);
        }
        let record = &self.directories[path];
        if let Some(error) = &record.error {
            diagnostics.push(error.clone());
        }
        files.extend(record.files.iter().cloned());
        let children = record.children.clone();
        for child in children {
            self.visit(root, &child, depth + 1, visited, files, diagnostics);
        }
    }
}
struct CachedTheme {
    stamp: Stamp,
    themes: Vec<Theme>,
    canonical: PathBuf,
    error: Option<String>,
}
struct CachedSettings {
    stamp: Stamp,
    settings: Option<ZedSettings>,
    error: Option<String>,
}
struct ThemeEngine {
    paths: ThemeWatchPaths,
    directories: DirectoryIndex,
    themes: HashMap<PathBuf, CachedTheme>,
    settings: HashMap<PathBuf, CachedSettings>,
    last_files: Vec<PathBuf>,
    last_errors: Vec<String>,
    initial: bool,
    #[cfg(test)]
    parses: usize,
}
impl ThemeEngine {
    fn new(paths: ThemeWatchPaths) -> Self {
        Self {
            paths,
            directories: DirectoryIndex::default(),
            themes: HashMap::new(),
            settings: HashMap::new(),
            last_files: Vec::new(),
            last_errors: Vec::new(),
            initial: true,
            #[cfg(test)]
            parses: 0,
        }
    }
    fn poll(&mut self) -> Option<ThemeUpdate> {
        let (files, mut diagnostics) = self.directories.files(&self.paths.theme_roots);
        let mut changed =
            self.initial || files != self.last_files || diagnostics != self.last_errors;
        self.initial = false;
        if files != self.last_files {
            // Membership only changes with the directory inventory. Unchanged
            // polls need neither cloned paths nor a temporary membership table.
            let present: HashSet<_> = files.iter().collect();
            self.themes.retain(|path, _| present.contains(path));
            self.last_files.clone_from(&files);
        }
        if diagnostics != self.last_errors {
            self.last_errors.clone_from(&diagnostics);
        }
        for path in &files {
            let current = stamp(path);
            if self
                .themes
                .get(path)
                .is_none_or(|cached| cached.stamp != current)
            {
                changed = true;
                #[cfg(test)]
                {
                    self.parses += 1;
                }
                let previous = self.themes.remove(path);
                let (themes, error) = match read_theme_file(path) {
                    Ok(mut themes) => {
                        for theme in &mut themes {
                            theme.source = Some(path.clone());
                        }
                        (themes, None)
                    }
                    Err(error) => (
                        previous.map_or_else(Vec::new, |previous| previous.themes),
                        Some(format!(
                            "{}: {error:#}; retaining last valid theme if available",
                            path.display()
                        )),
                    ),
                };
                self.themes.insert(
                    path.clone(),
                    CachedTheme {
                        stamp: current,
                        themes,
                        canonical: fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                        error,
                    },
                );
            }
        }
        for path in &self.paths.zed_settings {
            let current = stamp(path);
            if self
                .settings
                .get(path)
                .is_none_or(|cached| cached.stamp != current)
            {
                changed = true;
                let previous = self.settings.remove(path);
                let (settings, error) = if current == Stamp::Missing {
                    (None, None)
                } else {
                    #[cfg(test)]
                    {
                        self.parses += 1;
                    }
                    match fs::metadata(path)
                        .and_then(|metadata| {
                            if metadata.len() <= 16 * 1024 * 1024 {
                                fs::read_to_string(path)
                            } else {
                                Err(std::io::Error::other("Zed settings exceeds 16 MiB"))
                            }
                        })
                        .map_err(anyhow::Error::from)
                        .and_then(|json| parse_zed_settings(&json))
                    {
                        Ok(settings) => (Some(settings), None),
                        Err(error) => (
                            previous.and_then(|previous| previous.settings),
                            Some(format!(
                                "{}: {error:#}; retaining last valid Zed settings if available",
                                path.display()
                            )),
                        ),
                    }
                };
                self.settings.insert(
                    path.clone(),
                    CachedSettings {
                        stamp: current,
                        settings,
                        error,
                    },
                );
            }
        }
        if !changed {
            return None;
        }
        let mut themes = vec![Theme::dark(), Theme::light()];
        let mut canonical = HashSet::new();
        for path in files {
            let cached = &self.themes[&path];
            if let Some(error) = &cached.error {
                diagnostics.push(error.clone());
            }
            if !canonical.insert(cached.canonical.clone()) {
                continue;
            }
            for theme in &cached.themes {
                if let Some(position) = themes
                    .iter()
                    .position(|existing| existing.name == theme.name)
                {
                    themes[position] = theme.clone();
                } else {
                    themes.push(theme.clone());
                }
            }
        }
        let mut zed = None;
        for path in &self.paths.zed_settings {
            let cached = &self.settings[path];
            if let Some(error) = &cached.error {
                diagnostics.push(error.clone());
            }
            if zed.is_none() {
                zed = cached.settings.as_ref();
            }
        }
        if let Some(zed) = zed {
            for theme in &mut themes {
                if let Some(overrides) = zed.overrides.get(&theme.name) {
                    match overrides.as_object() {
                        Some(overrides) => theme.apply_style_overrides(overrides),
                        None => diagnostics.push(format!(
                            "Zed overrides for {} must be an object",
                            theme.name
                        )),
                    }
                }
            }
            if let Some(selection) = &zed.selection {
                let mut missing = HashSet::new();
                for name in [
                    selection.name.as_ref(),
                    selection.light.as_ref(),
                    selection.dark.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    if missing.insert(name) && !themes.iter().any(|theme| &theme.name == name) {
                        diagnostics.push(format!("Zed selected theme {name:?} is unavailable locally; import/install its JSON family (embedded Zed defaults are not local files)"));
                    }
                }
            }
        }
        for theme in &themes {
            diagnostics.extend(theme.accessibility_diagnostics());
        }
        Some(ThemeUpdate {
            themes,
            diagnostics,
            zed_theme: zed.and_then(|zed| zed.selection.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(root: &Path) -> ThemeWatchPaths {
        ThemeWatchPaths {
            theme_roots: vec![root.join("themes")],
            zed_settings: vec![root.join("settings.json")],
        }
    }
    #[test]
    fn zed_selection_and_nested_overrides_preserve_all_styles() {
        let settings=parse_zed_settings(r##"{"theme":{"mode":"system","dark":"Example","light":"Light"},"theme_overrides":{"Example":{"text":"#ffffff80","syntax":{"comment":{"font_style":"italic"}},"future":[1,null]}}}"##).unwrap();
        assert_eq!(
            settings.selection.as_ref().unwrap().selected_name(true),
            Some("Example")
        );
        assert_eq!(
            settings.selection.unwrap().selected_name(false),
            Some("Light")
        );
        let mut theme=Theme::from_zed_json(r##"{"name":"Example","style":{"text":"#ddd","syntax":{"comment":{"color":"#123456","font_weight":500}}}}"##).unwrap().remove(0);
        theme.apply_style_overrides(settings.overrides["Example"].as_object().unwrap());
        assert_eq!(theme.colors["text"], "#ffffff80");
        assert_eq!(theme.style["syntax"]["comment"]["color"], "#123456");
        assert_eq!(theme.style["syntax"]["comment"]["font_style"], "italic");
        assert!(theme.style["future"][1].is_null());
        assert_eq!(
            parse_zed_settings(r#"{"theme":"Fixed"}"#)
                .unwrap()
                .selection
                .unwrap()
                .selected_name(false),
            Some("Fixed")
        );
    }
    #[test]
    fn unchanged_metadata_does_not_rescan_or_parse_and_bad_edits_keep_last_good() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("themes")).unwrap();
        let file = root.path().join("themes/example.json");
        fs::write(
            &file,
            r##"{"name":"Example","style":{"text":"#fff","background":"#000"}}"##,
        )
        .unwrap();
        let mut engine = ThemeEngine::new(paths(root.path()));
        assert!(engine.poll().is_some());
        let parses = engine.parses;
        let listings = engine.directories.listings;
        for _ in 0..100 {
            assert!(engine.poll().is_none());
        }
        assert_eq!(engine.parses, parses);
        assert_eq!(engine.directories.listings, listings);
        fs::write(&file, "{bad edit").unwrap();
        let update = engine.poll().unwrap();
        assert!(update.themes.iter().any(|t| t.name == "Example"));
        assert!(
            update
                .diagnostics
                .iter()
                .any(|d| d.contains("retaining last valid"))
        );
        fs::remove_file(&file).unwrap();
        assert!(
            !engine
                .poll()
                .unwrap()
                .themes
                .iter()
                .any(|t| t.name == "Example")
        );
    }
    #[test]
    fn watcher_reloads_file_and_zed_settings_and_drop_stops_worker() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("themes")).unwrap();
        let file = root.path().join("themes/example.json");
        fs::write(
            &file,
            r##"{"name":"Example","style":{"text":"#fff","background":"#000"}}"##,
        )
        .unwrap();
        let (watcher, updates) =
            ThemeWatcher::start_with_paths(paths(root.path()), Duration::from_millis(20));
        assert_eq!(updates.capacity(), Some(1));
        updates.recv_timeout(Duration::from_secs(3)).unwrap();
        fs::write(
            root.path().join("settings.json"),
            r##"{"theme":"Example","theme_overrides":{"Example":{"text":"#aabbcc80"}}}"##,
        )
        .unwrap();
        let update = updates.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(update.zed_theme.unwrap().name.as_deref(), Some("Example"));
        assert_eq!(
            update
                .themes
                .iter()
                .find(|t| t.name == "Example")
                .unwrap()
                .colors["text"],
            "#aabbcc80"
        );
        fs::write(
            &file,
            r##"{"name":"Example","style":{"text":"#fff","background":"#112233"}}"##,
        )
        .unwrap();
        let update = updates.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            update
                .themes
                .iter()
                .find(|t| t.name == "Example")
                .unwrap()
                .colors["background"],
            "#112233"
        );
        drop(watcher);
        assert!(matches!(
            updates.recv_timeout(Duration::from_secs(3)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        ));
    }
}
