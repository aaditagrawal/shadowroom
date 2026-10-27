use super::*;

const MENU_WIDTH: f32 = 254.;
const MENU_HEADER: f32 = 30.;
const MENU_ROW: f32 = 28.;
const MENU_PADDING: f32 = 5.;

pub(super) struct ImageContextMenu {
    photo_id: i64,
    position: Point<Pixels>,
    selected: usize,
}

#[derive(Clone, Copy)]
enum ImageAction {
    Reveal,
    Review,
    Survey,
    Compare,
    Pick,
    Reject,
    Reset,
    Trash,
    PermanentDelete,
}

impl ImageAction {
    const ALL: [Self; 9] = [
        Self::Reveal,
        Self::Review,
        Self::Survey,
        Self::Compare,
        Self::Pick,
        Self::Reject,
        Self::Reset,
        Self::Trash,
        Self::PermanentDelete,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Reveal => "Show in folder",
            Self::Review => "Show in review",
            Self::Survey => "Survey burst",
            Self::Compare => "Compare",
            Self::Pick => "Pick",
            Self::Reject => "Reject",
            Self::Reset => "Reset stars and pick / reject",
            Self::Trash => "Move to Trash…",
            Self::PermanentDelete => "Permanently delete…",
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
            Self::Reset => "unflagged",
            Self::Trash | Self::PermanentDelete => "trash",
        }
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
                self.status = "Select at least two photographs to compare".into();
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

    pub(super) fn activate_review(&mut self, cx: &mut gpui::Context<Self>) {
        if self.mode == ViewMode::Grid && self.multi.len() <= 1 {
            if let Some(position) = self.catalog_hover.and_then(|id| self.visible_position(id)) {
                self.select(position, false, false, cx);
            }
        }
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
        if self.other_overlay_open() || self.quitting || self.recovery_pending {
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
        self.save_settings();
        self.read_ahead();
        self.rating_filter_open = false;
        self.image_context = Some(ImageContextMenu {
            photo_id: id,
            position,
            selected: 0,
        });
        cx.notify();
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
                    if let Err(error) = photo.require_local("Show in folder") {
                        self.error = Some(error.to_string());
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
            ImageAction::Pick => self.execute(Cmd::Flag(Flag::Pick), window, cx),
            ImageAction::Reject => self.execute(Cmd::Flag(Flag::Reject), window, cx),
            ImageAction::Reset => self.execute(Cmd::ResetDecision, window, cx),
            ImageAction::Trash => self.execute(Cmd::Trash(true), window, cx),
            ImageAction::PermanentDelete => self.execute(Cmd::PermanentDelete, window, cx),
        }
        cx.notify();
    }

    pub(super) fn image_context_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(menu) = self.image_context.as_mut() else {
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.image_context = None,
            "up" => menu.selected = menu.selected.saturating_sub(1),
            "down" => menu.selected = (menu.selected + 1).min(ImageAction::ALL.len() - 1),
            "enter" => {
                let action = ImageAction::ALL[menu.selected];
                self.run_image_action(action, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn image_context_bounds(&self) -> (f32, f32, f32, f32) {
        let height = MENU_HEADER + MENU_PADDING * 2. + MENU_ROW * ImageAction::ALL.len() as f32;
        let position = self.image_context.as_ref().map(|menu| menu.position);
        let x = position.map_or(0., |p| f32::from(p.x));
        let y = position.map_or(0., |p| f32::from(p.y));
        (
            x.clamp(0., (self.workspace_size.0 - MENU_WIDTH - 2.).max(0.)),
            y.clamp(0., (self.workspace_size.1 - height - 2.).max(0.)),
            MENU_WIDTH,
            height,
        )
    }

    pub(super) fn image_context_state(&self) -> Value {
        let Some(menu) = &self.image_context else {
            return Value::Null;
        };
        let (x, y, width, height) = self.image_context_bounds();
        json!({"photo_id":menu.photo_id,"selection_count":self.multi.len().max(1),
            "bounds":{"x":x,"y":y,"width":width,"height":height},"selected":menu.selected,
            "actions":ImageAction::ALL.iter().enumerate().map(|(i, action)|json!({
                "label":action.label(),"x":x+width/2.,"y":y+MENU_PADDING+MENU_HEADER+MENU_ROW*(i as f32+0.5)
            })).collect::<Vec<_>>()})
    }

    pub(super) fn image_context_view(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let menu = self.image_context.as_ref().expect("open image menu");
        let (x, y, width, _) = self.image_context_bounds();
        let title = if self.multi.len() > 1 {
            format!("{} selected photos", self.multi.len())
        } else {
            self.photo_index
                .get(&menu.photo_id)
                .map(|&i| self.photos[i].filename.clone())
                .unwrap_or_else(|| "Image actions".into())
        };
        let mut contents = div()
            .id("image-context-menu")
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .p(px(MENU_PADDING))
            .flex()
            .flex_col()
            .bg(self.c("elevated_surface.background", "#202020"))
            .border_1()
            .border_color(self.border())
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
            }])
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
                    .text_color(self.muted())
                    .child(title),
            );
        for (index, action) in ImageAction::ALL.into_iter().enumerate() {
            contents = contents.child(
                div()
                    .id(("image-action", index))
                    .h(px(MENU_ROW))
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .cursor_pointer()
                    .when(menu.selected == index, |row| row.bg(self.selected_bg()))
                    .hover(|row| row.bg(self.hover_bg()))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            if let Some(menu) = this.image_context.as_mut() {
                                menu.selected = index;
                            }
                            cx.notify();
                        }
                    }))
                    .child(icon(action.icon(), self.icon_muted()).size(px(14.)))
                    .child(action.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_image_action(action, window, cx);
                    })),
            );
        }
        div()
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
            .child(contents)
            .into_any_element()
    }
}
