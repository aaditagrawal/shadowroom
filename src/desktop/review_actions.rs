use super::*;

/// At 100% text; the menu widens with the Text size preference.
const MENU_WIDTH: f32 = 312.;
const MENU_HEADER: f32 = 30.;
const MENU_ROW: f32 = 28.;
const MENU_PADDING: f32 = 5.;
/// A hairline rule with 4 px above and below.
const MENU_SEPARATOR: f32 = 2. * space::XS + 1.;
/// Rating and label chips inside their menu rows: 24 px targets (WCAG 2.5.8).
const MENU_CHIP: f32 = 24.;

/// Menu width at the current text size.
fn menu_width() -> f32 {
    (MENU_WIDTH * ui::text_scale()).round()
}

pub(super) struct ImageContextMenu {
    photo_id: i64,
    position: Point<Pixels>,
    /// Index into [`Workspace::image_menu_actions`].
    selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageAction {
    Review,
    Survey,
    Compare,
    Pick,
    Reject,
    Reset,
    Reveal,
    Trash,
    PermanentDelete,
    /// Chip rows; reached with their digit keys, not Up/Down.
    Rating(u8),
    Label(ColorLabel),
}

/// One row of the image menu, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuRow {
    Action(ImageAction),
    Ratings,
    Labels,
    Separator,
}

impl MenuRow {
    fn height(self) -> f32 {
        match self {
            Self::Separator => MENU_SEPARATOR,
            _ => MENU_ROW,
        }
    }
}

/// Every image-menu label, for the Getting Started copy test.
#[cfg(test)]
pub(super) fn image_action_labels() -> Vec<&'static str> {
    use ImageAction as A;
    let actions = [
        A::Review,
        A::Survey,
        A::Compare,
        A::Pick,
        A::Reject,
        A::Reset,
        A::Reveal,
        A::Trash,
        A::PermanentDelete,
        A::Rating(0),
        A::Label(ColorLabel::None),
    ];
    let mut labels: Vec<_> = actions.iter().map(|a| a.label(false)).collect();
    labels.push(A::Survey.label(true));
    labels
}

impl ImageAction {
    fn label(self, in_burst: bool) -> &'static str {
        match self {
            Self::Review => copy::OPEN_IN_REVIEW,
            Self::Survey if in_burst => "Survey Burst",
            Self::Survey => "Survey",
            Self::Compare => "Compare",
            Self::Pick => "Pick",
            Self::Reject => "Reject",
            Self::Reset => copy::RESET_DECISION,
            Self::Reveal => reveal_label(),
            Self::Trash => copy::MOVE_TO_TRASH,
            Self::PermanentDelete => copy::DELETE_PERMANENTLY,
            Self::Rating(_) => "Rating",
            Self::Label(_) => "Label",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Reveal => "folder-opened",
            Self::Review => "loupe",
            Self::Survey => "group",
            Self::Compare => "compare",
            Self::Pick => "pick",
            Self::Reject => "reject",
            Self::Reset => "reset",
            Self::Trash => "trash",
            Self::PermanentDelete => "warning",
            Self::Rating(_) => "star",
            Self::Label(_) => "symbol-color",
        }
    }

    /// The key that runs it from the open menu and from the view beneath.
    fn keys(self) -> &'static str {
        match self {
            Self::Review => "Enter",
            Self::Survey => "S",
            Self::Compare => "C",
            Self::Pick => "P",
            Self::Reject => "X",
            Self::Reset => "R",
            Self::Reveal => "",
            Self::Trash => "Delete",
            Self::PermanentDelete => "⇧Delete",
            Self::Rating(_) => "0–5",
            Self::Label(_) => "6–9",
        }
    }

    /// Accelerators while the menu is open: the same keys as the view.
    fn for_key(key: &str, shift: bool) -> Option<Self> {
        Some(match key {
            "delete" | "backspace" if shift => Self::PermanentDelete,
            _ if shift => return None,
            "s" => Self::Survey,
            "c" => Self::Compare,
            "p" => Self::Pick,
            "x" => Self::Reject,
            "r" => Self::Reset,
            "delete" | "backspace" => Self::Trash,
            "0" | "1" | "2" | "3" | "4" | "5" => Self::Rating(key.parse().unwrap_or(0)),
            "6" => Self::Label(ColorLabel::Red),
            "7" => Self::Label(ColorLabel::Yellow),
            "8" => Self::Label(ColorLabel::Green),
            "9" => Self::Label(ColorLabel::Blue),
            _ => return None,
        })
    }
}

impl Workspace {
    pub(super) fn change_photo_view(
        &mut self,
        mode: ViewMode,
        selected_subset: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        self.rating_filter_open = false;
        self.catalog_hover = None;
        let keep = self.selected_id();
        if self.mode == ViewMode::Grid && mode != ViewMode::Grid {
            self.review_subset =
                (selected_subset && self.multi.len() > 1).then(|| self.multi.clone());
            self.review_selection = Some(std::mem::take(&mut self.multi));
            self.review_folder = self.scope.clone();
        } else if mode == ViewMode::Grid {
            if self.review_folder == self.scope {
                if let Some(ids) = self.review_selection.take() {
                    self.multi = ids;
                }
            } else {
                self.review_selection = None;
            }
            self.review_subset = None;
        }
        self.mode = mode;
        self.rebuild(keep);
        if self.review_subset.is_some()
            && self
                .reference
                .is_some_and(|id| self.visible_position(id).is_none())
        {
            self.reference = None;
        }
        if mode == ViewMode::Compare {
            if self.filtered.len() < 2 {
                self.reference = None;
                self.announce("Compare needs two photos in this view", Tone::Warning);
            } else {
                if self.reference.is_none() {
                    self.reference = self.selected_id();
                }
                if self.reference == self.selected_id() {
                    self.navigate(
                        if self.selected + 1 < self.filtered.len() {
                            1
                        } else {
                            -1
                        },
                        cx,
                    );
                }
            }
        }
        self.settings.view_mode = format!("{mode:?}").to_lowercase();
        self.save_settings();
        self.media.cancel_pending();
        self.read_ahead();
    }

    /// Enter/L open the outlined (selected) photo. The pointer can rest on a
    /// different cell, and arrow navigation recentres the grid under it, so
    /// following the hover made Enter land on a photo the user never chose.
    /// Double-click opens the photo under the pointer.
    pub(super) fn activate_review(&mut self, cx: &mut gpui::Context<Self>) {
        self.change_photo_view(ViewMode::Loupe, true, cx);
    }

    pub(super) fn hover_catalog_photo(
        &mut self,
        id: i64,
        hovered: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if hovered {
            self.catalog_hover = Some(id);
        } else if self.catalog_hover == Some(id) {
            self.catalog_hover = None;
        }
        cx.notify();
    }

    pub(super) fn open_image_context(
        &mut self,
        id: i64,
        position: Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.other_overlay_open() || self.quit.closing() || self.recovery_pending {
            return;
        }
        let Some(visible) = self.visible_position(id) else {
            return;
        };
        if self.mode == ViewMode::Compare && self.reference == Some(id) {
            self.multi.clear();
            self.reference_focus = true;
        } else if self.mode == ViewMode::Grid && self.multi.contains(&id) {
            // Keep a deliberate batch selection only when the clicked image
            // belongs to it. An unrelated old selection must never receive an
            // action requested on a different photograph.
            self.selected = visible;
            self.reference_focus = false;
        } else {
            self.select(visible, false, false, cx);
        }
        self.settings.last_photo_id = self.selected_id();
        self.save_navigation();
        self.read_ahead();
        self.rating_filter_open = false;
        self.image_context = Some(ImageContextMenu {
            photo_id: id,
            position,
            selected: 0,
        });
        cx.notify();
    }

    /// Shift+F10 or the Menu key: the same menu for the current photo, placed
    /// over the middle of the photo area (there is no pointer position).
    pub(super) fn open_image_context_from_keyboard(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(id) = self.review_photo().map(|photo| photo.id) else {
            return;
        };
        let left = self.docks.library;
        let width = menu_width();
        let center = self.docks.center.max(width);
        let x = left + ((center - width) / 2.).max(0.);
        let y = (self.workspace_size.1 / 3.).max(ui::TITLE_HEIGHT + ui::TOOLBAR_HEIGHT);
        self.open_image_context(id, point(px(x), px(y)), cx);
    }

    fn run_image_action(
        &mut self,
        action: ImageAction,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(menu) = self.image_context.take() else {
            return;
        };
        match action {
            ImageAction::Reveal => {
                if let Some(photo) = self
                    .photo_index
                    .get(&menu.photo_id)
                    .map(|&i| &self.photos[i])
                {
                    if let Err(error) = photo.require_local(reveal_label()) {
                        self.error = Some(error.to_string().into());
                    } else {
                        cx.reveal_path(&photo.path);
                    }
                }
            }
            ImageAction::Review | ImageAction::Survey => {
                // Explicit image actions always open the complete collection.
                // Enter/L on a multi-selection are the subset-review gesture.
                self.review_subset = None;
                if let Some(position) = self.visible_position(menu.photo_id) {
                    self.selected = position;
                    self.reference_focus = false;
                }
                self.change_photo_view(
                    if matches!(action, ImageAction::Review) {
                        ViewMode::Loupe
                    } else {
                        ViewMode::Survey
                    },
                    false,
                    cx,
                );
            }
            ImageAction::Compare => {
                if self.mode == ViewMode::Compare && self.reference == Some(menu.photo_id) {
                    self.reference_focus = false;
                }
                self.execute(Cmd::View(ViewMode::Compare), window, cx);
            }
            ImageAction::Pick
            | ImageAction::Reject
            | ImageAction::Reset
            | ImageAction::Trash
            | ImageAction::PermanentDelete
            | ImageAction::Rating(_)
            | ImageAction::Label(_)
                if !self.target_context_photo(menu.photo_id) =>
            {
                self.announce("That photo is no longer in this view", Tone::Warning);
            }
            ImageAction::Pick => self.execute(Cmd::Flag(Flag::Pick), window, cx),
            ImageAction::Reject => self.execute(Cmd::Flag(Flag::Reject), window, cx),
            ImageAction::Reset => self.execute(Cmd::ResetDecision, window, cx),
            ImageAction::Trash => self.execute(Cmd::Trash(true), window, cx),
            ImageAction::PermanentDelete => self.execute(Cmd::PermanentDelete, window, cx),
            ImageAction::Rating(stars) => self.execute(Cmd::Rating(stars), window, cx),
            ImageAction::Label(label) => self.execute(Cmd::Label(label), window, cx),
        }
        cx.notify();
    }

    /// Decision actions act on the review target or the batch selection.
    /// The catalog may rebuild while the menu is open (writes, imports,
    /// filters), so re-aim at the clicked photo first; false when it left
    /// the view.
    fn target_context_photo(&mut self, id: i64) -> bool {
        if self.mode == ViewMode::Grid && self.multi.contains(&id) {
            return true;
        }
        if self.review_photo().map(|p| p.id) == Some(id) {
            return true;
        }
        if self.mode == ViewMode::Compare && self.reference == Some(id) {
            self.multi.clear();
            self.reference_focus = true;
            return self.review_photo().is_some();
        }
        let Some(position) = self.visible_position(id) else {
            return false;
        };
        self.multi.clear();
        if self.mode == ViewMode::Grid {
            self.multi.insert(id);
        }
        self.selected = position;
        self.reference_focus = false;
        self.selection_anchor = Some(id);
        true
    }

    /// The menu's rows: open / decide / reveal / delete, separated by rules.
    /// Survey is named for the burst only when the photo is in one, and
    /// "Open in Review" is offered only outside Review (R4-catalog-06).
    fn image_menu_rows(&self) -> Vec<MenuRow> {
        use ImageAction as A;
        let open = (self.mode != ViewMode::Loupe).then_some(MenuRow::Action(A::Review));
        open.into_iter()
            .chain([
                MenuRow::Action(A::Survey),
                MenuRow::Action(A::Compare),
                MenuRow::Separator,
                MenuRow::Action(A::Pick),
                MenuRow::Action(A::Reject),
                MenuRow::Ratings,
                MenuRow::Labels,
                MenuRow::Action(A::Reset),
                MenuRow::Separator,
                MenuRow::Action(A::Reveal),
                MenuRow::Separator,
                MenuRow::Action(A::Trash),
                MenuRow::Action(A::PermanentDelete),
            ])
            .collect()
    }

    /// The rows Up/Down/Enter move through, in order.
    fn image_menu_actions(&self) -> Vec<ImageAction> {
        self.image_menu_rows()
            .into_iter()
            .filter_map(|row| match row {
                MenuRow::Action(action) => Some(action),
                _ => None,
            })
            .collect()
    }

    /// The photos a decision from the menu applies to: the batch when the
    /// clicked photo belongs to it, else the clicked photo.
    fn image_menu_targets(&self, id: i64) -> Vec<&Photo> {
        let ids: Vec<i64> = if self.mode == ViewMode::Grid && self.multi.contains(&id) {
            self.multi.iter().copied().collect()
        } else {
            vec![id]
        };
        ids.iter()
            .filter_map(|id| self.photo_index.get(id).map(|&i| &self.photos[i]))
            .collect()
    }

    pub(super) fn image_context_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let actions = self.image_menu_actions();
        let Some(menu) = self.image_context.as_mut() else {
            return;
        };
        let keystroke = &event.keystroke;
        let plain = !keystroke.modifiers.control
            && !keystroke.modifiers.platform
            && !keystroke.modifiers.alt;
        let last = actions.len().saturating_sub(1);
        match keystroke.key.as_str() {
            // Shift+F10 / Menu again closes it, as GTK and macOS menus do.
            "escape" | "menu" => self.image_context = None,
            "f10" if keystroke.modifiers.shift => self.image_context = None,
            "up" => menu.selected = menu.selected.checked_sub(1).unwrap_or(last),
            "down" => {
                menu.selected = if menu.selected >= last {
                    0
                } else {
                    menu.selected + 1
                }
            }
            "home" => menu.selected = 0,
            "end" => menu.selected = last,
            "enter" | "space" => {
                if let Some(&action) = actions.get(menu.selected) {
                    self.run_image_action(action, window, cx);
                }
            }
            key if plain => {
                if let Some(action) = ImageAction::for_key(key, keystroke.modifiers.shift) {
                    self.run_image_action(action, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn image_context_bounds(&self) -> (f32, f32, f32, f32) {
        let rows: f32 = self.image_menu_rows().iter().map(|row| row.height()).sum();
        let height = MENU_HEADER + MENU_PADDING * 2. + rows;
        let width = menu_width();
        let position = self.image_context.as_ref().map(|menu| menu.position);
        let x = position.map_or(0., |p| f32::from(p.x));
        let y = position.map_or(0., |p| f32::from(p.y));
        (
            x.clamp(0., (self.workspace_size.0 - width - 2.).max(0.)),
            y.clamp(0., (self.workspace_size.1 - height - 2.).max(0.)),
            width,
            height,
        )
    }

    pub(super) fn image_context_state(&self) -> Value {
        let Some(menu) = &self.image_context else {
            return Value::Null;
        };
        let (x, y, width, height) = self.image_context_bounds();
        let in_burst = self.burst_slot(menu.photo_id).is_some();
        let mut top = y + MENU_PADDING + MENU_HEADER;
        let mut actions = Vec::new();
        for row in self.image_menu_rows() {
            if let MenuRow::Action(action) = row {
                actions.push(
                    json!({"label":action.label(in_burst),"keys":shortcut(action.keys()),
                    "x":x+width/2.,"y":top+MENU_ROW/2.}),
                );
            }
            top += row.height();
        }
        json!({"photo_id":menu.photo_id,"selection_count":self.multi.len().max(1),
            "bounds":{"x":x,"y":y,"width":width,"height":height},"selected":menu.selected,
            "actions":actions})
    }

    /// One action row: icon, label, check for the current flag, keycap.
    fn image_menu_row(
        &self,
        action: ImageAction,
        index: usize,
        selected: bool,
        checked: bool,
        in_burst: bool,
        cx: &mut gpui::Context<Self>,
    ) -> Stateful<Div> {
        let destructive = action == ImageAction::PermanentDelete;
        let error = self.c("error", "#d46a6a");
        div()
            .id(("image-action", index))
            .h(px(MENU_ROW))
            .flex_shrink_0()
            .px(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .cursor_pointer()
            .when(selected, |row| row.bg(self.selected_bg()))
            .when(!selected, |row| row.hover(|row| row.bg(self.hover_bg())))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    if let Some(menu) = this.image_context.as_mut() {
                        menu.selected = index;
                    }
                    cx.notify();
                }
            }))
            .child(
                icon(
                    action.icon(),
                    if destructive {
                        error
                    } else {
                        self.icon_muted()
                    },
                )
                .size(px(14.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    // On the selection fill the error colour falls below 4.5:1
                    // in dark themes, so the highlighted row's label turns to
                    // text colour; the warning icon keeps the error colour.
                    .when(destructive && !selected, |s| s.text_color(error))
                    .child(action.label(in_burst)),
            )
            .child(div().w(px(16.)).flex_shrink_0().when(checked, |s| {
                s.child(icon("check", self.text()).size(px(14.)))
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .child(self.keycaps(action.keys())),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.run_image_action(action, window, cx);
            }))
    }

    /// The Rating or Label row: a caption, one chip per value (the current
    /// one outlined), and the digit keys that set it.
    fn image_menu_chips(
        &self,
        ratings: bool,
        targets: &[&Photo],
        cx: &mut gpui::Context<Self>,
    ) -> Div {
        let single = |f: &dyn Fn(&Photo) -> u8| {
            let mut values = targets.iter().map(|photo| f(photo));
            let first = values.next();
            first.filter(|&v| values.all(|other| other == v))
        };
        let current = if ratings {
            single(&|photo| photo.rating)
        } else {
            single(&|photo| photo.label as u8)
        };
        let action = if ratings {
            ImageAction::Rating(0)
        } else {
            ImageAction::Label(ColorLabel::None)
        };
        let mut chips = div().flex().items_center().gap(px(space::XS));
        let values: Vec<ImageAction> = if ratings {
            (0..=5).map(ImageAction::Rating).collect()
        } else {
            [
                ColorLabel::None,
                ColorLabel::Red,
                ColorLabel::Yellow,
                ColorLabel::Green,
                ColorLabel::Blue,
                ColorLabel::Purple,
            ]
            .map(ImageAction::Label)
            .to_vec()
        };
        for value in values {
            let (id, on, tip) = match value {
                ImageAction::Rating(stars) => (
                    stars as usize,
                    current == Some(stars),
                    if stars == 0 {
                        "No stars".to_string()
                    } else {
                        plural(stars as usize, "star", "stars")
                    },
                ),
                ImageAction::Label(label) => (
                    10 + label as usize,
                    current == Some(label as u8),
                    Self::label_tip(label),
                ),
                _ => continue,
            };
            // A box means "a key" everywhere, so the current rating takes
            // the selected fill (as choice-group segments do) and the current
            // label the inspector swatch's ring with an inset gap.
            let ring = on && matches!(value, ImageAction::Label(_));
            let mut chip = div()
                .id(("image-menu-chip", id))
                .size(px(MENU_CHIP))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(if ring {
                    self.text()
                } else {
                    transparent_black()
                })
                .cursor_pointer()
                .when(on && !ring, |s| s.bg(self.selected_bg()))
                .when(!on, |s| s.hover(|s| s.bg(self.hover_bg())))
                .tooltip(self.tip(&tip, ""));
            chip = match value {
                ImageAction::Rating(0) => chip
                    .text_color(if on { self.text() } else { self.muted() })
                    .child("–"),
                ImageAction::Rating(stars) => chip
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(if on { self.text() } else { self.muted() })
                    .child(stars.to_string()),
                // The same mark as the inspector's swatch: the key, or
                // Purple's dot (R5-design-01).
                ImageAction::Label(label) => match self.label_color(label) {
                    Some(color) => chip.child(
                        div()
                            .size(px(MENU_CHIP - 2. * space::XS))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(color)
                            .text_size(ui::font(ui::text::MICRO))
                            .line_height(ui::font(ui::text::MICRO))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(self.c("shadowroom.label.text", "#0f0f0f"))
                            .child(self.label_mark(label)),
                    ),
                    None => chip.child(
                        div()
                            .size(px(MENU_CHIP - 2. * space::XS))
                            .border_1()
                            .border_color(self.control_border()),
                    ),
                },
                _ => chip,
            };
            chips = chips.child(chip.on_click(cx.listener(move |this, _, window, cx| {
                this.run_image_action(value, window, cx);
            })));
        }
        div()
            .h(px(MENU_ROW))
            .flex_shrink_0()
            .px(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(icon(action.icon(), self.icon_muted()).size(px(14.)))
            .child(
                div()
                    .w(px(ui::scaled(48.)))
                    .flex_shrink_0()
                    .text_color(self.muted())
                    .child(action.label(false)),
            )
            .child(div().flex_1().flex().child(chips))
            .child(self.keycaps(action.keys()))
    }

    pub(super) fn image_context_view(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let menu = self.image_context.as_ref().expect("open image menu");
        let (x, y, width, _) = self.image_context_bounds();
        let title = if self.multi.len() > 1 {
            format!("{} selected", plural(self.multi.len(), "photo", "photos"))
        } else {
            self.photo_index
                .get(&menu.photo_id)
                .map(|&i| self.photos[i].filename.clone())
                .unwrap_or_else(|| "Image actions".into())
        };
        let targets = self.image_menu_targets(menu.photo_id);
        let all = |flag: Flag| !targets.is_empty() && targets.iter().all(|p| p.flag == flag);
        let (picked, rejected) = (all(Flag::Pick), all(Flag::Reject));
        let in_burst = self.burst_slot(menu.photo_id).is_some();
        let mut contents = self
            .menu_surface()
            .id("image-context-menu")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .p(px(MENU_PADDING))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(MENU_HEADER))
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(self.muted())
                    .child(title),
            );
        let mut index = 0;
        for row in self.image_menu_rows() {
            contents = match row {
                MenuRow::Action(action) => {
                    let checked = match action {
                        ImageAction::Pick => picked,
                        ImageAction::Reject => rejected,
                        _ => false,
                    };
                    let row = self.image_menu_row(
                        action,
                        index,
                        menu.selected == index,
                        checked,
                        in_burst,
                        cx,
                    );
                    index += 1;
                    contents.child(row)
                }
                MenuRow::Ratings => contents.child(self.image_menu_chips(true, &targets, cx)),
                MenuRow::Labels => contents.child(self.image_menu_chips(false, &targets, cx)),
                MenuRow::Separator => contents.child(
                    div()
                        .flex_shrink_0()
                        .my(px(space::XS))
                        .h(px(ui::HAIRLINE))
                        .bg(self.c("border.variant", "#333333")),
                ),
            };
        }
        let menu = div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.image_context = None;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.image_context = None;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(contents);
        menu.into_any_element()
    }
}
