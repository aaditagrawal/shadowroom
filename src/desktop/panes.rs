//! F6 / Shift+F6 move keyboard focus between the workspace panes (library,
//! photo area, inspector), as GTK, GNOME and macOS apps do. The focused pane
//! carries the focus ring while the keyboard leads; the pointer hands focus
//! back to the photo area.
//!
//! In the library, Up/Down/Home/End move a cursor over the collections, smart
//! filters, labels and folders, and Return or Space opens the row. In the
//! inspector, Left/Right switch between Review and Info. Escape returns to the
//! photo area. Decision keys keep working from any pane. `Workspace::key`
//! routes here only after the quit chord, so Ctrl+Q still quits first.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Pane {
    #[default]
    Canvas,
    Library,
    Inspector,
}
impl Pane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Library => "library",
            Self::Inspector => "inspector",
        }
    }
}

/// What Return does on a library row.
#[derive(Clone)]
pub(super) enum LibraryTarget {
    Command(Cmd),
    Folder(String),
}

impl Workspace {
    /// The pane that holds keyboard focus; a hidden dock gives it back.
    pub(super) fn focused_pane(&self) -> Pane {
        match self.pane {
            Pane::Library if self.docks.library <= 0. => Pane::Canvas,
            Pane::Inspector if self.docks.inspector <= 0. => Pane::Canvas,
            pane => pane,
        }
    }

    /// F6 (forward) / Shift+F6 (back) over the visible panes, in screen order.
    pub(super) fn cycle_pane(&mut self, back: bool, cx: &mut Context<Self>) {
        let mut order = Vec::with_capacity(3);
        if self.docks.library > 0. {
            order.push(Pane::Library);
        }
        order.push(Pane::Canvas);
        if self.docks.inspector > 0. {
            order.push(Pane::Inspector);
        }
        let current = order
            .iter()
            .position(|&pane| pane == self.focused_pane())
            .unwrap_or(0);
        // The first F6 shows the ring where focus already is: the photo area.
        let next = if !self.pane_ring {
            current
        } else if back {
            (current + order.len() - 1) % order.len()
        } else {
            (current + 1) % order.len()
        };
        self.pane = order[next];
        self.pane_ring = true;
        self.focus_visible = true;
        if self.pane == Pane::Library {
            let targets = self.library_targets();
            let active = targets.iter().position(|(_, _, active)| *active);
            self.library_cursor = active.unwrap_or(0).min(targets.len().saturating_sub(1));
        }
        self.announce(
            match self.pane {
                Pane::Library => "Library focused · ↑↓ to move, Return to open, Esc to return",
                Pane::Canvas => "Photos focused",
                Pane::Inspector => "Inspector focused · ←→ switch Review / Info, Esc to return",
            },
            Tone::Info,
        );
        cx.notify();
    }

    /// The pointer took over: the photo area owns keys again, ring hidden.
    pub(super) fn release_pane_focus(&mut self) -> bool {
        let changed = self.pane != Pane::Canvas || self.pane_ring;
        self.pane = Pane::Canvas;
        self.pane_ring = false;
        changed
    }

    /// Keys owned by the focused library or inspector pane. Returns false for
    /// keys the views beneath should still handle (decisions, chords).
    pub(super) fn pane_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let m = event.keystroke.modifiers;
        if m.control || m.platform || m.alt {
            return false;
        }
        let key = event.keystroke.key.as_str();
        match self.focused_pane() {
            Pane::Canvas => false,
            Pane::Library => {
                let targets = self.library_targets();
                let last = targets.len().saturating_sub(1);
                match key {
                    "up" => self.library_cursor = self.library_cursor.saturating_sub(1),
                    "down" => self.library_cursor = (self.library_cursor + 1).min(last),
                    "home" | "pageup" => self.library_cursor = 0,
                    "end" | "pagedown" => self.library_cursor = last,
                    "enter" | "space" => {
                        if let Some((_, target, _)) = targets.into_iter().nth(self.library_cursor) {
                            match target {
                                LibraryTarget::Command(cmd) => self.execute(cmd, window, cx),
                                LibraryTarget::Folder(path) => self.show_folder(path),
                            }
                        }
                    }
                    "escape" => {
                        self.pane = Pane::Canvas;
                    }
                    _ => return false,
                }
                self.pane_ring = true;
                cx.notify();
                true
            }
            Pane::Inspector => {
                match key {
                    "left" => self.execute(Cmd::InspectorTab(false), window, cx),
                    "right" => self.execute(Cmd::InspectorTab(true), window, cx),
                    "escape" => self.pane = Pane::Canvas,
                    _ => return false,
                }
                self.pane_ring = true;
                cx.notify();
                true
            }
        }
    }

    /// The library rows the keyboard cursor visits, in the order drawn:
    /// (row element id, what Return does, whether the row is current).
    pub(super) fn library_targets(&self) -> Vec<(ElementId, LibraryTarget, bool)> {
        let counts = self.scope_counts.as_ref().unwrap_or(&self.counts);
        let mut targets = Vec::new();
        for (n, filter) in [
            Filter::All,
            Filter::Picks,
            Filter::Unrated,
            Filter::Rejects,
            Filter::Trash,
        ]
        .into_iter()
        .enumerate()
        {
            let cmd = if filter == Filter::All {
                Cmd::ClearFilters
            } else {
                Cmd::Filter(filter)
            };
            let active = !self.duplicates_only && self.filter == filter;
            targets.push((
                ElementId::from(("collection", n)),
                LibraryTarget::Command(cmd),
                active,
            ));
        }
        targets.push((
            "collection-duplicates".into(),
            LibraryTarget::Command(Cmd::ShowDuplicates),
            self.duplicates_only,
        ));
        targets.push((
            "smart-rated".into(),
            LibraryTarget::Command(Cmd::RatedAtLeast(3)),
            false,
        ));
        if !self.groups.is_empty() {
            targets.push((
                "smart-bursts".into(),
                LibraryTarget::Command(Cmd::BurstsOnly),
                self.bursts_only,
            ));
        }
        for (n, label) in [
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ]
        .into_iter()
        .enumerate()
        {
            if counts.labels[label_index(label)] == 0 && self.label_filter != Some(label) {
                continue;
            }
            targets.push((
                ElementId::from(("smart-label", n)),
                LibraryTarget::Command(Cmd::LabelFilter(label)),
                false,
            ));
        }
        for (n, (path, _)) in self.folders.iter().enumerate() {
            if path.starts_with("remote-root:") {
                continue;
            }
            targets.push((
                ElementId::from(("folder", n)),
                LibraryTarget::Folder(path.clone()),
                self.scope.as_ref() == Some(path),
            ));
        }
        targets
    }

    /// The library row under the keyboard cursor, while the library has focus
    /// and the keyboard leads.
    pub(super) fn library_cursor_id(&self) -> Option<ElementId> {
        if !self.pane_ring || self.focused_pane() != Pane::Library {
            return None;
        }
        self.library_targets()
            .into_iter()
            .nth(self.library_cursor)
            .map(|(id, ..)| id)
    }

    /// A 2 px ring inside a library row under the keyboard cursor.
    pub(super) fn library_cursor_ring(&self) -> Div {
        div()
            .absolute()
            .inset_0()
            .border_2()
            .border_color(self.focus_ring())
    }

    /// The focused pane's ring, drawn over the workspace body (it takes no
    /// pointer input). Only while the keyboard leads.
    pub(super) fn pane_ring_view(&self) -> Option<AnyElement> {
        if !self.pane_ring || !self.focus_visible || self.top_overlay().is_some() {
            return None;
        }
        let ring = div()
            .absolute()
            .top_0()
            .bottom_0()
            .border_2()
            .border_color(self.focus_ring());
        Some(
            match self.focused_pane() {
                Pane::Library => ring.left_0().w(px(self.docks.library)),
                Pane::Inspector => ring.right_0().w(px(self.docks.inspector)),
                Pane::Canvas => ring
                    .left(px(self.docks.library))
                    .right(px(self.docks.inspector)),
            }
            .into_any_element(),
        )
    }

    /// Show one folder, clearing the other constraints (the folder row click).
    pub(super) fn show_folder(&mut self, path: String) {
        self.scope = Some(path);
        self.duplicates_only = false;
        self.label_filter = None;
        self.bursts_only = false;
        self.filter = Filter::All;
        self.settings.filter = "all".into();
        self.save_settings();
        self.rebuild(None);
    }
}
