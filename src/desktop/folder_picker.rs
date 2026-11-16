//! Path choosing: the platform file chooser first (the XDG file-chooser
//! portal on Linux, called directly so it carries a real title, accept label
//! and start folder; NSOpenPanel on macOS), the built-in picker as fallback.
//! Directory and theme reads and the portal call stay off the UI thread.
use super::*;
use anyhow::Context as _;
use shadowroom::path_picker::{Entry, address, list};
use std::path::Path;

#[derive(Clone)]
pub(super) enum Purpose {
    Workspace { export: Option<bool>, ids: Vec<i64> },
    Ingest(usize),
    Theme(u64),
}
impl Purpose {
    fn title(&self) -> &'static str {
        match self {
            Self::Workspace { export: None, .. } => "Open photo folder",
            Self::Workspace {
                export: Some(true), ..
            } => "Export picks",
            Self::Workspace { .. } => "Export XMP sidecars",
            Self::Ingest(0) => "Choose photo library",
            Self::Ingest(_) => "Choose backup destination",
            Self::Theme(_) => "Import Zed Theme",
        }
    }
    fn files(&self) -> bool {
        matches!(self, Self::Theme(_))
    }
}

pub(super) struct State {
    purpose: Purpose,
    directory: PathBuf,
    address: String,
    entries: Vec<Entry>,
    selected: Option<usize>,
    address_focused: bool,
    loading: bool,
    truncated: bool,
    unreadable: usize,
    error: Option<String>,
    request: u64,
    /// The address field's own caret, independent of any field beneath.
    edit: text_input::Edit,
    /// Sidecars a picks export would copy beside the originals, once counted
    /// (off the UI thread), so the header's file count matches the manifest.
    sidecars: Option<usize>,
}
impl State {
    pub(super) fn focused_edit(&mut self) -> Option<&mut text_input::Edit> {
        self.address_focused.then_some(&mut self.edit)
    }
    pub(super) fn snapshot(&self) -> Value {
        json!({"purpose":self.purpose.title(),"directory":self.directory,
            "address":self.address,"loading":self.loading,"error":self.error,
            "selected":self.selected,"truncated":self.truncated,
            "entries":self.entries.iter().map(|entry| json!({"path":entry.path,"directory":entry.directory})).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Copy)]
enum Action {
    Cancel,
    Home,
    Up,
    Go,
    Choose,
    Enter(usize),
}

/// A theme file's source label and the themes read from it.
type ThemeFamily = (String, Vec<Theme>);

/// Which chooser `show_path_picker` opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chooser {
    /// The platform dialog (XDG portal on Linux, NSOpenPanel on macOS).
    Native,
    /// The built-in picker.
    Builtin,
}

/// `SHADOWROOM_FILE_CHOOSER=native|builtin` forces one chooser.
fn chooser_override() -> Option<Chooser> {
    match std::env::var("SHADOWROOM_FILE_CHOOSER").as_deref() {
        Ok("native" | "portal") => Some(Chooser::Native),
        Ok("builtin" | "custom") => Some(Chooser::Builtin),
        _ => None,
    }
}

/// Which chooser asks: a forced one, else the built-in picker when the
/// native one is unavailable (no portal, automation), else the platform's.
/// Exports go native too: the portal request carries their title (count),
/// accept label and start folder.
fn chooser_for(forced: Option<Chooser>, native_unavailable: bool) -> Chooser {
    forced.unwrap_or(if native_unavailable {
        Chooser::Builtin
    } else {
        Chooser::Native
    })
}

impl Purpose {
    /// The native dialog's accept button.
    fn accept_label(&self) -> &'static str {
        match self {
            Self::Workspace { export: None, .. } => "Open",
            Self::Workspace {
                export: Some(true), ..
            } => "Export Here",
            Self::Workspace { .. } => "Write XMP Here",
            Self::Ingest(_) => "Choose",
            Self::Theme(_) => "Import",
        }
    }
}

/// What the platform chooser is asked: built (and tested) without a session
/// bus, sent by `portal::choose` on Linux. (macOS asks NSOpenPanel through
/// GPUI, which takes only the accept label.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChooserRequest {
    pub title: String,
    pub accept_label: String,
    pub directory: bool,
    /// Where the chooser opens (only an existing folder is sent).
    pub current_folder: Option<PathBuf>,
    /// File filters for a file choice: label and glob patterns.
    pub filters: Vec<(String, Vec<String>)>,
}

/// The platform chooser's answer.
#[derive(Debug)]
enum NativeAnswer {
    Chosen(PathBuf),
    Cancelled,
    /// No portal, no session bus, or the request failed: the built-in picker
    /// takes over.
    Unavailable(String),
}

/// The XDG desktop portal's FileChooser.OpenFile, called directly: GPUI's
/// prompt_for_paths titles every folder request "Open Folder" and sends no
/// start folder, which an export needs.
#[cfg(target_os = "linux")]
mod portal {
    use super::{ChooserRequest, NativeAnswer};
    use ashpd::desktop::file_chooser::{FileFilter, OpenFileRequest, SelectedFiles};

    /// The portal request for `request`, ready to send.
    pub(super) fn open_file(request: &ChooserRequest) -> ashpd::Result<OpenFileRequest> {
        let mut open = SelectedFiles::open_file()
            .title(request.title.as_str())
            .accept_label(request.accept_label.as_str())
            .modal(true)
            .multiple(false)
            .directory(request.directory);
        for (label, globs) in &request.filters {
            open = open.filter(
                globs
                    .iter()
                    .fold(FileFilter::new(label), |filter, glob| filter.glob(glob)),
            );
        }
        open.current_folder::<&std::path::Path>(request.current_folder.as_deref())
    }

    /// Asks the portal. The caller runs it on the background executor, never
    /// the UI thread; zbus answers through async-io's own reactor.
    pub(super) async fn choose(request: ChooserRequest) -> NativeAnswer {
        let open = match open_file(&request) {
            Ok(open) => open,
            Err(error) => return NativeAnswer::Unavailable(error.to_string()),
        };
        let response = match open.send().await {
            Ok(request) => request.response(),
            Err(error) => return NativeAnswer::Unavailable(error.to_string()),
        };
        match response {
            Ok(files) => files
                .uris()
                .iter()
                .find_map(|uri| uri.to_file_path().ok())
                .map_or(NativeAnswer::Cancelled, NativeAnswer::Chosen),
            // The user dismissed the dialog.
            Err(ashpd::Error::Response(_)) => NativeAnswer::Cancelled,
            Err(error) => NativeAnswer::Unavailable(error.to_string()),
        }
    }
}

impl Workspace {
    /// The native chooser's accept button: names the verb that opened it.
    fn accept_label(&self, purpose: &Purpose) -> &'static str {
        match (purpose, self.folder_intent) {
            (Purpose::Workspace { export: None, .. }, super::ingest_ui::FolderIntent::Cull) => {
                "Open to Cull"
            }
            (Purpose::Workspace { export: None, .. }, super::ingest_ui::FolderIntent::Import) => {
                "Import"
            }
            _ => purpose.accept_label(),
        }
    }
    /// Asks for a folder (or a theme file) for `purpose`. The platform's own
    /// chooser opens first, titled with what will happen ("Export 3 picks in
    /// this view"), with the verb on its accept button and opening at
    /// `initial`; the built-in picker is the fallback when the desktop has no
    /// file-chooser portal, when the app is driven by automation (whose state
    /// must stay inspectable), and when a native choice breaks an in-app rule
    /// (an export inside a photo folder, a file that is not a theme): it then
    /// opens at that choice with the reason.
    pub(super) fn show_path_picker(
        &mut self,
        purpose: Purpose,
        initial: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let chooser = chooser_for(
            chooser_override(),
            self.native_chooser_failed || self.automation_driven,
        );
        if chooser == Chooser::Native {
            self.prompt_native_path(purpose, initial, cx);
        } else {
            self.show_builtin_picker(purpose, initial, cx);
        }
    }

    /// The platform chooser's request for `purpose`, opening at `initial`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn chooser_request(&self, purpose: &Purpose, initial: &Path) -> ChooserRequest {
        ChooserRequest {
            title: self.picker_copy(purpose).0,
            accept_label: self.accept_label(purpose).into(),
            directory: !purpose.files(),
            current_folder: (!initial.as_os_str().is_empty() && initial.is_dir())
                .then(|| initial.to_path_buf()),
            filters: if purpose.files() {
                vec![("Zed theme (JSON)".into(), vec!["*.json".into()])]
            } else {
                Vec::new()
            },
        }
    }

    fn prompt_native_path(&mut self, purpose: Purpose, initial: PathBuf, cx: &mut Context<Self>) {
        self.palette = false;
        self.image_context = None;
        self.rating_filter_open = false;
        #[cfg(target_os = "linux")]
        let answer = cx
            .background_executor()
            .spawn(portal::choose(self.chooser_request(&purpose, &initial)));
        #[cfg(not(target_os = "linux"))]
        let answer = {
            let files = purpose.files();
            cx.prompt_for_paths(PathPromptOptions {
                files,
                directories: !files,
                multiple: false,
                prompt: Some(self.accept_label(&purpose).into()),
            })
        };
        let photo_folders = self.export_photo_folders(&purpose);
        cx.spawn(async move |this, cx| {
            #[cfg(target_os = "linux")]
            let answer = answer.await;
            #[cfg(not(target_os = "linux"))]
            let answer = match answer.await {
                Ok(Ok(Some(paths))) => paths
                    .into_iter()
                    .next()
                    .map_or(NativeAnswer::Cancelled, NativeAnswer::Chosen),
                Ok(Ok(None)) => NativeAnswer::Cancelled,
                Ok(Err(error)) => NativeAnswer::Unavailable(format!("{error:#}")),
                Err(error) => NativeAnswer::Unavailable(error.to_string()),
            };
            let path = match answer {
                NativeAnswer::Chosen(path) => path,
                NativeAnswer::Cancelled => {
                    let _ = this.update(cx, |this, cx| {
                        this.refocus_ingest_field();
                        cx.notify();
                    });
                    return;
                }
                // No portal (or no session bus): use the built-in picker
                // from now on, starting where the caller asked.
                NativeAnswer::Unavailable(error) => {
                    eprintln!(
                        "Native file chooser unavailable, using the built-in picker: {error}"
                    );
                    let _ = this.update(cx, |this, cx| {
                        this.native_chooser_failed = true;
                        this.show_builtin_picker(purpose, initial, cx);
                    });
                    return;
                }
            };
            let checked = path.clone();
            let check = purpose.clone();
            let result = cx
                .background_executor()
                .spawn(async move { validate_choice(&check, checked, &photo_folders) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((path, theme)) => this.apply_path_choice(purpose, path, theme),
                    Err(error) => {
                        // An in-app rule refused the native choice: the
                        // built-in picker shows why, at that location.
                        let start = if path.is_dir() {
                            path
                        } else {
                            path.parent().map(Path::to_path_buf).unwrap_or(initial)
                        };
                        this.show_builtin_picker(purpose, start, cx);
                        if let Some(state) = this.folder_picker.as_mut() {
                            state.error = Some(format!("{error:#}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Photo folders an export must stay outside of (empty for other purposes).
    fn export_photo_folders(&self, purpose: &Purpose) -> Vec<PathBuf> {
        if !matches!(
            purpose,
            Purpose::Workspace {
                export: Some(_),
                ..
            }
        ) {
            return Vec::new();
        }
        self.photos
            .iter()
            .filter(|photo| !photo.is_remote())
            .flat_map(|photo| std::iter::once(&photo.path).chain(photo.companion_path.iter()))
            .filter_map(|path| path.parent())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(Path::to_path_buf)
            .collect()
    }

    /// Hands a validated choice to whoever asked for it.
    fn apply_path_choice(&mut self, purpose: Purpose, path: PathBuf, theme: Option<ThemeFamily>) {
        match purpose {
            Purpose::Workspace {
                export: Some(picks),
                ids,
            } => self.start_export(path, picks, ids),
            Purpose::Workspace { export: None, .. } => {
                self.status = format!("Opening {}", path.display());
                self.open_ingest(path);
            }
            Purpose::Ingest(field) => self.apply_ingest_path(field, &path),
            Purpose::Theme(id) => {
                if let Some(draft) = self
                    .preferences_draft
                    .as_mut()
                    .filter(|draft| draft.id == id && !draft.applying)
                {
                    let (source, themes) = theme.expect("Validated theme file");
                    draft.import(source, themes);
                }
            }
        }
    }

    fn show_builtin_picker(&mut self, purpose: Purpose, initial: PathBuf, cx: &mut Context<Self>) {
        let home = directories::BaseDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let initial = if initial.as_os_str().is_empty() {
            home
        } else {
            initial
        };
        self.palette = false;
        self.image_context = None;
        self.rating_filter_open = false;
        self.folder_picker = Some(State {
            purpose,
            directory: initial.clone(),
            address: initial.to_string_lossy().into_owned(),
            entries: Vec::new(),
            selected: None,
            // The list takes focus: Down highlights the first folder at once,
            // and Ctrl+L (or a click) edits the path.
            address_focused: false,
            loading: false,
            truncated: false,
            unreadable: 0,
            error: None,
            request: 0,
            edit: text_input::Edit::default(),
            sidecars: None,
        });
        self.count_export_sidecars(cx);
        self.picker_navigate(initial, cx);
        cx.notify();
    }

    /// Counts, off the UI thread, the sidecars a picks export would copy.
    fn count_export_sidecars(&mut self, cx: &mut Context<Self>) {
        let Some(Purpose::Workspace {
            export: Some(true),
            ids,
        }) = self
            .folder_picker
            .as_ref()
            .map(|state| state.purpose.clone())
        else {
            return;
        };
        let media: Vec<PathBuf> = self
            .export_picks_in(&ids)
            .into_iter()
            .flat_map(|photo| {
                std::iter::once(photo.path.clone()).chain(photo.companion_path.clone())
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let count = cx
                .background_executor()
                .spawn(async move {
                    shadowroom::catalog::export_sidecar_count(media.iter().map(PathBuf::as_path))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.folder_picker.as_mut() {
                    state.sidecars = Some(count);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn picker_navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.folder_picker.as_mut() else {
            return;
        };
        if state.loading {
            return;
        }
        state.loading = true;
        state.error = None;
        state.request = rand::random();
        let request = state.request;
        let files = state.purpose.files();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { list(&path, files) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.folder_picker.as_mut().filter(|s| s.request == request)
                else {
                    return;
                };
                state.loading = false;
                match result {
                    Ok(listing) => {
                        state.directory = listing.directory;
                        state.address = state.directory.to_string_lossy().into_owned();
                        state.entries = listing.entries;
                        state.selected = None;
                        state.truncated = listing.truncated;
                        state.unreadable = listing.unreadable;
                        if state.address_focused {
                            state.edit.focus(&state.address);
                        }
                    }
                    Err(error) => state.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn picker_action(&mut self, action: Action, cx: &mut Context<Self>) {
        if matches!(action, Action::Cancel) {
            self.folder_picker = None;
            self.refocus_ingest_field();
            cx.notify();
            return;
        }
        let Some(state) = self.folder_picker.as_ref() else {
            return;
        };
        if state.loading {
            return;
        }
        let navigation = match action {
            Action::Home => directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()),
            Action::Up => state.directory.parent().map(Path::to_path_buf),
            Action::Go => match address(&state.address, &state.directory) {
                Ok(path) => Some(path),
                Err(error) => {
                    self.folder_picker.as_mut().unwrap().error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            },
            Action::Enter(index) if state.entries.get(index).is_some_and(|e| e.directory) => {
                Some(state.entries[index].path.clone())
            }
            _ => None,
        };
        if let Some(path) = navigation {
            self.picker_navigate(path, cx);
            cx.notify();
            return;
        }
        if !matches!(action, Action::Choose | Action::Enter(_)) {
            return;
        }
        let state = self.folder_picker.as_ref().unwrap();
        let path = if let Action::Enter(index) = action {
            state.entries.get(index).map(|entry| entry.path.clone())
        } else if !state.address_focused {
            state
                .selected
                .and_then(|index| state.entries.get(index))
                .map(|entry| entry.path.clone())
                .or_else(|| Some(state.directory.clone()))
        } else if state.address == state.directory.to_string_lossy() {
            Some(state.directory.clone())
        } else {
            address(&state.address, &state.directory).ok()
        };
        let Some(path) = path else {
            return;
        };
        if state.purpose.files()
            && state
                .entries
                .iter()
                .any(|entry| entry.path == path && entry.directory)
        {
            self.picker_navigate(path, cx);
            return;
        }
        let purpose = state.purpose.clone();
        // Exports check the destination rule here, so a bad choice keeps the
        // picker open with the reason instead of failing later in a toast.
        let photo_folders = self.export_photo_folders(&purpose);
        let request = rand::random();
        let state = self.folder_picker.as_mut().unwrap();
        state.request = request;
        state.loading = true;
        state.error = None;
        cx.spawn(async move |this, cx| {
            let check = purpose.clone();
            let result = cx
                .background_executor()
                .spawn(async move { validate_choice(&check, path, &photo_folders) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this
                    .folder_picker
                    .as_ref()
                    .is_some_and(|s| s.request == request)
                {
                    return;
                }
                match result {
                    Ok((path, theme)) => {
                        this.folder_picker = None;
                        this.apply_path_choice(purpose, path, theme);
                    }
                    Err(error) => {
                        let state = this.folder_picker.as_mut().unwrap();
                        state.loading = false;
                        state.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.folder_picker.as_mut() else {
            return false;
        };
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.picker_action(Action::Cancel, cx);
            return true;
        }
        if state.loading {
            return true;
        }
        let primary = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        if primary && key == "l" {
            state.address_focused = true;
            state.edit.focus(&state.address);
        } else if primary && key == "enter" {
            self.picker_action(Action::Choose, cx);
        } else if key == "tab" {
            state.address_focused = !state.address_focused;
            if state.address_focused {
                state.edit.focus(&state.address);
            } else if !state.entries.is_empty() {
                state.selected = Some(0);
            }
        } else if event.keystroke.modifiers.alt && key == "up" {
            self.picker_action(Action::Up, cx);
        } else if state.address_focused {
            if key == "enter" {
                self.picker_action(Action::Go, cx);
            } else {
                state.edit.key(&mut state.address, event, cx);
            }
        } else {
            match key {
                "up" => state.selected = state.selected.map(|index| index.saturating_sub(1)),
                "down" if !state.entries.is_empty() => {
                    state.selected = Some(
                        state
                            .selected
                            .map_or(0, |index| (index + 1).min(state.entries.len() - 1)),
                    )
                }
                "enter" => {
                    let action = state.selected.map_or(Action::Choose, Action::Enter);
                    self.picker_action(action, cx);
                }
                _ => {}
            }
        }
        cx.notify();
        true
    }

    fn picker_button(
        &self,
        id: &'static str,
        label: &str,
        action: Action,
        kind: ButtonKind,
        disabled: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.dialog_button(id, label, kind, disabled)
            .when(!disabled, |s| {
                s.on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    window.focus(&this.focus);
                    this.picker_action(action, cx);
                }))
            })
            .into_any_element()
    }

    /// The photos a picks export will copy: the scope's picks, or every
    /// pick in the catalog when the scope is empty.
    fn export_picks_in<'a>(&'a self, ids: &'a [i64]) -> Vec<&'a Photo> {
        if ids.is_empty() {
            self.photos
                .iter()
                .filter(|p| !p.trashed && p.flag == Flag::Pick)
                .collect()
        } else {
            ids.iter()
                .filter_map(|id| self.photo_index.get(id).map(|&i| &self.photos[i]))
                .filter(|p| !p.trashed && p.flag == Flag::Pick)
                .collect()
        }
    }
    /// Title, explanation and confirm label: exports say what they will
    /// write and for how many photos, so Picks and XMP never look alike.
    fn picker_copy(&self, purpose: &Purpose) -> (String, Option<String>, String) {
        let state = self.folder_picker.as_ref();
        // The folder the confirm button will use: the highlighted row, else the
        // folder being shown.
        let target = state
            .and_then(|state| {
                (!state.address_focused)
                    .then(|| state.selected.and_then(|i| state.entries.get(i)))
                    .flatten()
                    .filter(|entry| entry.directory)
                    .map(|entry| entry.path.clone())
                    .or_else(|| Some(state.directory.clone()))
            })
            .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "this folder".into());
        match purpose {
            Purpose::Workspace {
                export: Some(true),
                ids,
            } => {
                let picks = self.export_picks_in(ids);
                let pairs = picks.iter().filter(|p| p.companion_path.is_some()).count();
                let sidecars = state.and_then(|state| state.sidecars).unwrap_or(0);
                let files = picks.len() + pairs + sidecars;
                let bytes: u64 = picks
                    .iter()
                    .map(|p| p.size_bytes + p.companion_size_bytes)
                    .sum();
                let scope = if ids.is_empty() {
                    "from the whole catalog"
                } else {
                    "in this view"
                };
                // Only the clauses that apply: "3 files (1 RAW+JPEG pair,
                // 1 sidecar) · 4.1 MB"; no sidecars, no sidecar clause.
                let mut parts = Vec::new();
                if pairs > 0 {
                    parts.push(plural(pairs, "RAW+JPEG pair", "RAW+JPEG pairs"));
                }
                if sidecars > 0 {
                    parts.push(plural(sidecars, "sidecar", "sidecars"));
                }
                let detail = if parts.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", parts.join(", "))
                };
                (
                    format!("Export {} {scope}", plural(picks.len(), "pick", "picks")),
                    Some(format!(
                        "{}{detail} · {}. Originals are copied byte for byte and verified; nothing is modified.",
                        plural(files, "file", "files"),
                        copy::fmt_bytes(bytes)
                    )),
                    format!("Export to “{target}”"),
                )
            }
            Purpose::Workspace {
                export: Some(false),
                ids,
            } => {
                let photos = if self.multi.is_empty() {
                    format!("{} in this view", plural(ids.len(), "photo", "photos"))
                } else {
                    plural(ids.len(), "selected photo", "selected photos")
                };
                (
                    format!("Export decision XMP for {photos}"),
                    Some(
                        "Writes new .xmp files with rating, label and flag. Your originals and their existing sidecars are not modified."
                            .into(),
                    ),
                    format!("Write {}", plural(ids.len(), "sidecar", "sidecars")),
                )
            }
            Purpose::Theme(_) => (purpose.title().into(), None, "Use theme".into()),
            // The two entry verbs share this purpose; say which one is running.
            Purpose::Workspace { export: None, .. } => match self.folder_intent {
                super::ingest_ui::FolderIntent::Cull => (
                    "Open folder to cull in place".into(),
                    Some("Photos stay where they are; nothing is copied.".into()),
                    format!("Open “{target}”"),
                ),
                super::ingest_ui::FolderIntent::Import => (
                    "Import from card or folder".into(),
                    Some("Copies and verifies photos into your library; the source is never modified.".into()),
                    format!("Import “{target}”"),
                ),
            },
            _ => (purpose.title().into(), None, format!("Use “{target}”")),
        }
    }
    /// Places: home folders and mounted drives, one click away.
    fn picker_places(&self, cx: &Context<Self>) -> Stateful<Div> {
        let mut places: Vec<(String, &'static str, PathBuf)> = Vec::new();
        if let Some(dirs) = directories::UserDirs::new() {
            places.push(("Home".into(), "folder", dirs.home_dir().to_path_buf()));
            for (name, dir) in [
                ("Pictures", dirs.picture_dir()),
                ("Desktop", dirs.desktop_dir()),
                ("Downloads", dirs.download_dir()),
            ] {
                if let Some(dir) = dir {
                    places.push((name.into(), "folder", dir.to_path_buf()));
                }
            }
        }
        for drive in &self.drives {
            places.push((drive.name.clone(), "drive", drive.path.clone()));
        }
        let current = self.folder_picker.as_ref().map(|s| s.directory.clone());
        let mut column = div()
            .id("path-picker-places")
            .w(px(152.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .py(px(space::XS))
            .border_r_1()
            .border_color(self.border());
        for (index, (name, glyph, path)) in places.into_iter().enumerate() {
            let hover = self.hover_bg();
            let here = current.as_ref() == Some(&path);
            column = column.child(
                div()
                    .id(("path-picker-place", index))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::MD))
                    .py(px(space::XS + 1.))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .when(here, |s| s.bg(self.selected_bg()))
                    .hover(move |s| s.bg(hover))
                    .child(icon(glyph, self.icon_muted()))
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(name),
                    )
                    .tooltip(self.tip(&path.display().to_string(), ""))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus);
                        if let Some(state) = this.folder_picker.as_mut() {
                            state.address_focused = false;
                        }
                        this.picker_navigate(path.clone(), cx);
                        cx.notify();
                    })),
            );
        }
        column
    }
    pub(super) fn picker_view(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.folder_picker.as_ref().unwrap();
        let (title, subtitle, confirm) = self.picker_copy(&state.purpose);
        let mut rows = div()
            .id("path-picker-entries")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        for (index, entry) in state.entries.iter().enumerate() {
            let label = entry.path.file_name().unwrap_or_default().to_string_lossy();
            let hover = self.hover_bg();
            rows = rows.child(
                div()
                    .id(ElementId::Name(format!("path-picker-entry-{index}").into()))
                    .px(px(space::MD))
                    .py(px(space::SM))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .bg(if state.selected == Some(index) {
                        self.accent().opacity(0.18)
                    } else {
                        transparent_black()
                    })
                    .hover(move |style| style.bg(hover))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(if entry.directory {
                        icon("folder", self.icon_muted()).into_any_element()
                    } else {
                        icon("files", self.icon_muted()).into_any_element()
                    })
                    .child(label.into_owned())
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus);
                        if let Some(state) =
                            this.folder_picker.as_mut().filter(|state| !state.loading)
                        {
                            state.selected = Some(index);
                            state.address_focused = false;
                            if event.click_count() == 2 {
                                this.picker_action(Action::Enter(index), cx);
                            }
                        }
                        cx.notify();
                    })),
            );
        }
        if state.entries.is_empty() && !state.loading {
            rows = rows.child(
                div()
                    .p(px(space::LG))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .text_color(self.muted())
                    .when(state.purpose.files(), |s| {
                        s.child(div().text_color(self.text()).child("No theme files here"))
                            .child("Zed keeps installed themes in ~/.config/zed/themes")
                    })
                    .when(!state.purpose.files(), |s| {
                        s.child("No subfolders. You can choose this folder.")
                    }),
            );
        }
        // A theme import needs a .json file: a typed path or a selected file.
        let theme_ready = !state.purpose.files() || {
            let json = |path: &Path| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            };
            if state.address_focused {
                json(Path::new(state.address.trim()))
            } else {
                state
                    .selected
                    .and_then(|index| state.entries.get(index))
                    .is_some_and(|entry| !entry.directory && json(&entry.path))
            }
        };
        // Nested over Preferences, the theme picker is narrower than its parent.
        let width = if state.purpose.files() {
            dialog::DIALOG_LARGE - dialog::NESTED_INSET
        } else {
            dialog::DIALOG_LARGE
        };
        let mut body = self
            .modal_surface("path-picker", width)
            .h(px((self.workspace_size.1 - 48.).clamp(180., 600.)))
            // Closes like every other dialog.
            .child(self.modal_header(title, Some(("picker-close", Cmd::Close)), cx))
            .children(subtitle.map(|subtitle| {
                div()
                    .px(px(ui::DIALOG_INSET))
                    .pt(px(space::MD))
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(self.muted())
                    .whitespace_normal()
                    .child(subtitle)
            }))
            .child(
                div()
                    .px(px(space::LG))
                    .py(px(space::MD))
                    .flex()
                    .gap(px(space::SM))
                    // Home is the first place; the file picker has no
                    // places column, so it keeps a Home button.
                    .when(state.purpose.files(), |s| {
                        s.child(self.picker_button(
                            "picker-home",
                            "Home",
                            Action::Home,
                            ButtonKind::Secondary,
                            false,
                            cx,
                        ))
                    })
                    .child(self.picker_button(
                        "picker-up",
                        "Up",
                        Action::Up,
                        ButtonKind::Secondary,
                        false,
                        cx,
                    ))
                    .child(
                        div()
                            .id("picker-address")
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .p(px(space::SM))
                            .border_1()
                            .border_color(if state.address_focused {
                                self.accent()
                            } else {
                                self.control_border()
                            })
                            .child(self.input_field(
                                &state.edit,
                                // Unfocused, keep the root and the last folders visible.
                                &if state.address_focused {
                                    state.address.clone()
                                } else {
                                    copy::middle_path(&state.address, 58)
                                },
                                "Folder path",
                                state.address_focused,
                                state.address_focused,
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                window.focus(&this.focus);
                                if let Some(state) = this.folder_picker.as_mut() {
                                    state.address_focused = true;
                                    state.edit.focus(&state.address);
                                }
                                cx.notify();
                            })),
                    )
                    .child(self.picker_button(
                        "picker-go",
                        "Go",
                        Action::Go,
                        ButtonKind::Secondary,
                        false,
                        cx,
                    )),
            )
            .child(
                // A thin bar while a folder is read, where the list will change.
                div()
                    .h(px(2.))
                    .flex_shrink_0()
                    .mx(px(space::LG))
                    .when(state.loading, |s| s.bg(self.accent())),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .border_t_1()
                    .border_color(self.border())
                    .when(!state.purpose.files(), |s| s.child(self.picker_places(cx)))
                    .child(rows),
            );
        if state.truncated || state.unreadable > 0 {
            body = body.child(
                div()
                    .px(px(space::LG))
                    .text_color(self.muted())
                    .child(format!(
                        "{}{}",
                        if state.truncated {
                            "Listing limited; enter a path to reach another folder. "
                        } else {
                            ""
                        },
                        if state.unreadable > 0 {
                            format!("{} unreadable entries omitted.", state.unreadable)
                        } else {
                            String::new()
                        }
                    )),
            );
        }
        if let Some(error) = &state.error {
            body = body.child(
                div()
                    .p(px(space::MD))
                    .text_color(self.c("error", "#d46a6a"))
                    .child(error.clone()),
            );
        }
        body = body.child(
            self.modal_footer()
                // Enter goes into the highlighted folder; the primary button's
                // own verb names what Ctrl+Enter does (R4-overlays-05, R4-io-02).
                // The visible Cancel button already says Esc's job, and three
                // hints stay on one line beside a long "Export to “…”".
                .child(self.key_hints(&[
                    ("⌘L", "Path"),
                    ("Enter", "Go into"),
                    (
                        "⌘Enter",
                        confirm.split_whitespace().next().unwrap_or("Choose"),
                    ),
                ]))
                .child(
                    div()
                        .flex()
                        .gap(px(space::SM))
                        .child(self.picker_button(
                            "picker-cancel",
                            "Cancel",
                            Action::Cancel,
                            ButtonKind::Secondary,
                            false,
                            cx,
                        ))
                        .child(self.picker_button(
                            "picker-choose",
                            &confirm,
                            Action::Choose,
                            ButtonKind::Primary,
                            !theme_ready,
                            cx,
                        )),
                ),
        );
        self.modal_scrim(Overlay::FolderPicker)
            .p(px(space::LG))
            .child(body)
            .into_any_element()
    }
}

/// Where an export chooser opens: the last export folder while it exists,
/// else Pictures, else home, skipping the import library and anything in it
/// (the first export never starts there).
pub(super) fn export_start_folder(
    last: Option<PathBuf>,
    pictures: Option<PathBuf>,
    home: Option<PathBuf>,
    library: &Path,
) -> PathBuf {
    let outside_library =
        |path: &PathBuf| library.as_os_str().is_empty() || !path.starts_with(library);
    last.filter(|path| path.is_dir())
        .into_iter()
        .chain(pictures.filter(|path| path.is_dir() && outside_library(path)))
        .chain(home.filter(outside_library))
        .next()
        .unwrap_or_default()
}

/// The in-app rules for a chosen path, off the UI thread: themes must be JSON
/// theme files; folders must exist, and export folders must lie outside the
/// photo folders being exported. Returns the canonical path (and the theme
/// family read from a theme file).
fn validate_choice(
    purpose: &Purpose,
    path: PathBuf,
    photo_folders: &[PathBuf],
) -> anyhow::Result<(PathBuf, Option<ThemeFamily>)> {
    let path =
        std::fs::canonicalize(&path).with_context(|| format!("Cannot open {}", path.display()))?;
    if purpose.files() {
        anyhow::ensure!(
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("json")),
            "Choose a theme JSON file"
        );
        let theme = read_theme_family(&path)?;
        Ok((path, Some(theme)))
    } else {
        anyhow::ensure!(path.is_dir(), "Choose an existing folder");
        shadowroom::catalog::check_export_destination(
            photo_folders.iter().map(PathBuf::as_path),
            &path,
        )
        .context("Choose a folder outside your photo folders, such as a new Exports folder")?;
        Ok((path, None))
    }
}

#[cfg(test)]
mod chooser_tests {
    use super::{Chooser, ChooserRequest, Purpose, chooser_for};
    use std::path::PathBuf;

    #[test]
    fn every_purpose_goes_native_unless_unavailable_or_forced() {
        // Exports too: the portal request carries their context now.
        assert_eq!(chooser_for(None, false), Chooser::Native);
        // No portal, or automation: the built-in picker.
        assert_eq!(chooser_for(None, true), Chooser::Builtin);
        // SHADOWROOM_FILE_CHOOSER wins either way.
        assert_eq!(chooser_for(Some(Chooser::Native), true), Chooser::Native);
        assert_eq!(chooser_for(Some(Chooser::Builtin), false), Chooser::Builtin);
        let picks = Purpose::Workspace {
            export: Some(true),
            ids: vec![1, 2],
        };
        let xmp = Purpose::Workspace {
            export: Some(false),
            ids: vec![1],
        };
        assert_eq!(picks.accept_label(), "Export Here");
        assert_eq!(xmp.accept_label(), "Write XMP Here");
        assert_eq!(Purpose::Theme(1).accept_label(), "Import");
    }

    #[test]
    fn the_first_export_never_starts_in_the_import_library() {
        let root = tempfile::tempdir().unwrap();
        let pictures = root.path().join("Pictures");
        let library = pictures.join("Shadowroom");
        let exports = root.path().join("Exports");
        for dir in [&library, &exports] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let home = Some(root.path().to_path_buf());
        // No export yet: Pictures, not the library inside it.
        assert_eq!(
            super::export_start_folder(None, Some(pictures.clone()), home.clone(), &library),
            pictures
        );
        // The library is Pictures itself: home instead.
        assert_eq!(
            super::export_start_folder(None, Some(pictures.clone()), home.clone(), &pictures),
            root.path()
        );
        // The last export folder wins while it exists.
        assert_eq!(
            super::export_start_folder(
                Some(exports.clone()),
                Some(pictures.clone()),
                home.clone(),
                &library
            ),
            exports
        );
        let gone = root.path().join("gone");
        assert_eq!(
            super::export_start_folder(Some(gone), Some(pictures.clone()), home, &library),
            pictures
        );
    }

    /// The OpenFile call the portal receives, without a session bus: title,
    /// accept label, directory mode, modality and the start folder.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_request_carries_title_accept_label_and_start_folder() {
        let folder = tempfile::tempdir().unwrap();
        let request = ChooserRequest {
            title: "Export 3 picks in this view".into(),
            accept_label: "Export Here".into(),
            directory: true,
            current_folder: Some(folder.path().to_path_buf()),
            filters: Vec::new(),
        };
        let open = super::portal::open_file(&request).unwrap();
        let sent = format!("{open:?}");
        assert!(
            sent.contains("title: \"Export 3 picks in this view\""),
            "{sent}"
        );
        assert!(
            sent.contains("accept_label: Some(\"Export Here\")"),
            "{sent}"
        );
        assert!(sent.contains("directory: Some(true)"), "{sent}");
        assert!(sent.contains("modal: Some(true)"), "{sent}");
        assert!(sent.contains("multiple: Some(false)"), "{sent}");
        let folder_name = folder.path().file_name().unwrap().to_string_lossy();
        assert!(
            sent.contains("current_folder: Some(") && sent.contains(folder_name.as_ref()),
            "{sent}"
        );
        // A theme file: a JSON filter, not a folder, no start folder.
        let theme = ChooserRequest {
            title: "Import Zed Theme".into(),
            accept_label: "Import".into(),
            directory: false,
            current_folder: None::<PathBuf>,
            filters: vec![("Zed theme (JSON)".into(), vec!["*.json".into()])],
        };
        let sent = format!("{:?}", super::portal::open_file(&theme).unwrap());
        assert!(sent.contains("directory: Some(false)"), "{sent}");
        assert!(sent.contains("current_folder: None"), "{sent}");
        assert!(sent.contains("*.json"), "{sent}");
    }
}
