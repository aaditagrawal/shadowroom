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
            Self::Workspace { .. } => "Choose export destination",
            Self::Ingest(0) => "Choose photo library",
            Self::Ingest(_) => "Choose backup destination",
            Self::Theme(_) => "Choose theme JSON",
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
}
impl State {
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
            address_focused: true,
            loading: false,
            truncated: false,
            unreadable: 0,
            error: None,
            request: 0,
        });
        self.text_edit.focus(&initial.to_string_lossy());
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
                        this.text_edit.focus(&state.address);
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
                            } => {
                                this.status = "Exporting in background…".into();
                                if let Err(error) = this.queue_export(path, picks, ids) {
                                    this.error = Some(error);
                                }
                            }
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
            self.text_edit.focus(&state.address);
        } else if primary && key == "enter" {
            self.picker_action(Action::Choose, cx);
        } else if key == "tab" {
            state.address_focused = !state.address_focused;
            if state.address_focused {
                self.text_edit.focus(&state.address);
            } else if !state.entries.is_empty() {
                state.selected = Some(0);
            }
        } else if event.keystroke.modifiers.alt && key == "up" {
            self.picker_action(Action::Up, cx);
        } else if state.address_focused {
            if key == "enter" {
                self.picker_action(Action::Go, cx);
            } else {
                self.text_edit.key(&mut state.address, event, cx);
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
        label: &'static str,
        action: Action,
        cx: &Context<Self>,
    ) -> AnyElement {
        let hover = self.hover_bg();
        div()
            .id(id)
            .px(px(space::MD))
            .py(px(space::SM))
            .border_1()
            .border_color(self.border())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                window.focus(&this.focus);
                this.picker_action(action, cx);
            }))
            .into_any_element()
    }

    pub(super) fn picker_view(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.folder_picker.as_ref().unwrap();
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
                    .child(format!(
                        "{}  {}",
                        if entry.directory { "▸" } else { "·" },
                        label
                    ))
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
            rows = rows.child(div().p(px(space::LG)).text_color(self.muted()).child(
                if state.purpose.files() {
                    "No folders or theme JSON files"
                } else {
                    "No subfolders. You can choose this folder."
                },
            ));
        }
        let mut body = div()
            .id("path-picker")
            .w(px(720.))
            .max_w_full()
            .h(px((self.workspace_size.1 - 48.).clamp(180., 600.)))
            .flex()
            .flex_col()
            .border_1()
            .border_color(self.border())
            .bg(self.bg())
            .child(
                div()
                    .p(px(space::LG))
                    .font_weight(FontWeight::MEDIUM)
                    .child(state.purpose.title()),
            )
            .child(
                div()
                    .px(px(space::LG))
                    .pb(px(space::MD))
                    .flex()
                    .gap(px(space::SM))
                    .child(self.picker_button("picker-home", "Home", Action::Home, cx))
                    .child(self.picker_button("picker-up", "Up", Action::Up, cx))
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
                            .child(self.input_text(
                                &state.address,
                                "Folder path",
                                state.address_focused,
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                window.focus(&this.focus);
                                if let Some(state) = this.folder_picker.as_mut() {
                                    state.address_focused = true;
                                    this.text_edit.focus(&state.address);
                                }
                                cx.notify();
                            })),
                    )
                    .child(self.picker_button("picker-go", "Go", Action::Go, cx)),
            )
            .child(rows);
        if state.loading {
            body = body.child(
                div()
                    .px(px(space::LG))
                    .text_color(self.muted())
                    .child("Reading folder…"),
            );
        }
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
            div()
                .p(px(space::LG))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_color(self.muted())
                        .child("Ctrl+L Path · Enter Open · Ctrl+Enter Use · Esc Cancel"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(space::SM))
                        .child(self.picker_button("picker-cancel", "Cancel", Action::Cancel, cx))
                        .child(self.picker_button(
                            "picker-choose",
                            if state.purpose.files() {
                                "Use theme"
                            } else {
                                "Use folder"
                            },
                            Action::Choose,
                            cx,
                        )),
                ),
        );
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000088))
            .flex()
            .items_center()
            .justify_center()
            .p(px(space::LG))
            .child(body)
            .into_any_element()
    }
}
