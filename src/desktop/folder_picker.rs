//! Built-in Linux path picker. Directory and theme reads stay off the UI thread.
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

impl Workspace {
    pub(super) fn show_path_picker(
        &mut self,
        purpose: Purpose,
        initial: PathBuf,
        cx: &mut Context<Self>,
    ) {
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
        });
        self.picker_navigate(initial, cx);
        cx.notify();
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
        let photo_folders: Vec<PathBuf> = if matches!(
            purpose,
            Purpose::Workspace {
                export: Some(_),
                ..
            }
        ) {
            self.photos
                .iter()
                .filter(|photo| !photo.is_remote())
                .flat_map(|photo| std::iter::once(&photo.path).chain(photo.companion_path.iter()))
                .filter_map(|path| path.parent())
                .collect::<HashSet<_>>()
                .into_iter()
                .map(Path::to_path_buf)
                .collect()
        } else {
            Vec::new()
        };
        let request = rand::random();
        let state = self.folder_picker.as_mut().unwrap();
        state.request = request;
        state.loading = true;
        state.error = None;
        cx.spawn(async move |this, cx| {
            let files = purpose.files();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = std::fs::canonicalize(&path)
                        .with_context(|| format!("Cannot open {}", path.display()))?;
                    if files {
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
                        Ok::<_, anyhow::Error>((path, None))
                    }
                })
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
                        match purpose {
                            Purpose::Workspace {
                                export: Some(picks),
                                ids,
                            } => this.start_export(path, picks, ids),
                            Purpose::Workspace { export: None, .. } => {
                                this.status = format!("Opening {}", path.display());
                                this.open_ingest(path);
                            }
                            Purpose::Ingest(field) => this.apply_ingest_path(field, &path),
                            Purpose::Theme(id) => {
                                if let Some(draft) = this
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
                let files: usize = picks
                    .iter()
                    .map(|p| 1 + usize::from(p.companion_path.is_some()))
                    .sum();
                let bytes: u64 = picks
                    .iter()
                    .map(|p| p.size_bytes + p.companion_size_bytes)
                    .sum();
                let scope = if ids.is_empty() {
                    "from the whole catalog"
                } else {
                    "in this view"
                };
                (
                    format!("Export {} {scope}", plural(picks.len(), "pick", "picks")),
                    Some(format!(
                        "{}{} · {}, plus their sidecars. Originals are copied byte for byte and verified; nothing is modified.",
                        plural(files, "file", "files"),
                        if files > picks.len() {
                            " incl. RAW+JPEG pairs"
                        } else {
                            ""
                        },
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
            .child(self.modal_header(title, None, cx))
            .children(subtitle.map(|subtitle| {
                div()
                    .px(px(ui::DIALOG_INSET))
                    .pt(px(space::MD))
                    .text_size(px(ui::text::LABEL))
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
                    .child(self.picker_button(
                        "picker-home",
                        "Home",
                        Action::Home,
                        ButtonKind::Secondary,
                        false,
                        cx,
                    ))
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
                                self.border()
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
                .child(
                    div()
                        .text_color(self.muted())
                        .child("Ctrl+L Path · Enter Open · Ctrl+Enter Use · Esc Cancel"),
                )
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
