//! Culling efficiency: zoom where you look, keyboard pan and the zoom
//! navigator, Compare's "winner stays" loop, confirmed batch decisions,
//! finishing and filtered-empty states, filter chips and keyboard paging.
use super::*;

/// Long edge of the zoom navigator inset, logical px.
const NAVIGATOR_EDGE: f32 = 120.;
/// The navigator steps aside once the zoomed view has been still this long.
pub(super) const NAVIGATOR_IDLE: Duration = Duration::from_millis(1500);
const NAVIGATOR_FADE: Duration = Duration::from_millis(200);
/// A press that moves less than this is a click, not a pan.
const CLICK_SLOP: f32 = 4.;
/// Widest line of empty-state copy at 100% text (~70 characters).
const EMPTY_MEASURE: f32 = 560.;

/// One removable constraint of the current view, for breadcrumb chips.
pub(super) struct Constraint {
    pub id: &'static str,
    pub label: String,
    pub dot: Option<ColorLabel>,
    pub clear: Cmd,
}

impl Workspace {
    /// Pans/geometry index of the active pane: 0 = Compare reference, 1 = current.
    pub(super) fn active_pane(&self) -> usize {
        usize::from(!self.reference_focused())
    }

    /// The photo pane under a window point, if its image has been painted.
    pub(super) fn pane_at(&self, at: Point<Pixels>) -> Option<usize> {
        if !matches!(self.mode, ViewMode::Loupe | ViewMode::Compare) {
            return None;
        }
        let geometry = self.pane_geometry.get();
        (0..2).find(|&i| (i == 1 || self.mode == ViewMode::Compare) && geometry[i].contains(at))
    }

    /// Zooms to `factor`. With an anchor, the image point under the pointer
    /// moves to the pane centre (`center`) or stays under the pointer.
    pub(super) fn zoom_at(
        &mut self,
        factor: f32,
        anchor: Option<(usize, Point<Pixels>)>,
        center: bool,
    ) {
        let factor = factor.clamp(0.125, 8.);
        if let Some((pane, at)) = anchor {
            let g = self.pane_geometry.get()[pane];
            if g.contains(at) {
                let before = if self.zoom {
                    g.zoom_scale(self.zoom_factor)
                } else {
                    g.fit_scale()
                };
                let u = g.image_point(at, before, self.pans[pane]);
                let target = if center {
                    [
                        f32::from(g.bounds.size.width) / 2.,
                        f32::from(g.bounds.size.height) / 2.,
                    ]
                } else {
                    [
                        f32::from(at.x - g.bounds.origin.x),
                        f32::from(at.y - g.bounds.origin.y),
                    ]
                };
                self.pans[pane] = g.pan_for(u, target, g.zoom_scale(factor));
                if self.compare_sync {
                    self.pans[1 - pane] = self.pans[pane];
                }
            }
        }
        self.zoom = true;
        self.zoom_factor = factor;
        self.read_ahead();
    }

    /// Z / Space: 100% centred on the image point under the pointer when it
    /// rests on a photo, else at the stored pan; again returns to Fit.
    pub(super) fn toggle_zoom_at_pointer(&mut self, window: &Window) {
        if self.zoom {
            self.zoom = false;
            self.read_ahead();
            return;
        }
        let at = window.mouse_position();
        let anchor = self.pane_at(at).map(|pane| (pane, at));
        self.zoom_at(1., anchor, true);
    }

    /// Ctrl/Cmd+scroll: one zoom step that keeps the point under the cursor.
    pub(super) fn scroll_zoom(&mut self, reference: bool, at: Point<Pixels>, direction: i8) {
        let pane = usize::from(!reference);
        let current = if self.zoom {
            self.zoom_factor
        } else {
            self.fit_scales.get()[pane]
        };
        let factor = current * if direction > 0 { 1.25 } else { 0.8 };
        self.zoom_at(factor, Some((pane, at)), false);
    }

    /// A click (not a drag) on a photo: at Fit, 100% centred where it landed
    /// on the picture; zoomed, back to Fit. In Compare the first click on the
    /// other pane only makes it active.
    pub(super) fn click_photo(&mut self, reference: bool, event: &ClickEvent) {
        let ClickEvent::Mouse(click) = event else {
            return;
        };
        let moved = click.up.position - click.down.position;
        let still = f32::from(moved.x).abs() < CLICK_SLOP && f32::from(moved.y).abs() < CLICK_SLOP;
        let was_active = self.mode != ViewMode::Compare || self.reference_focused() == reference;
        self.reference_focus = reference && self.reference_available();
        if !still || !was_active || click.down.button != MouseButton::Left {
            return;
        }
        if self.zoom {
            self.zoom = false;
            self.read_ahead();
            return;
        }
        let pane = usize::from(!reference);
        let g = self.pane_geometry.get()[pane];
        if !g.valid() {
            return;
        }
        let fit = g.fit_scale();
        let size = size(px(g.width as f32 * fit), px(g.height as f32 * fit));
        let origin = point(
            g.bounds.origin.x + (g.bounds.size.width - size.width) / 2.,
            g.bounds.origin.y + (g.bounds.size.height - size.height) / 2.,
        );
        // The letterbox is not the picture.
        if Bounds::new(origin, size).contains(&click.up.position) {
            self.zoom_at(1., Some((pane, click.up.position)), true);
        }
    }

    /// Alt/Option+arrows while zoomed: pan the active pane by a third of
    /// the view. False when there is nothing to pan.
    pub(super) fn pan_step(&mut self, dx: f32, dy: f32) -> bool {
        if !self.zoom || !matches!(self.mode, ViewMode::Loupe | ViewMode::Compare) {
            return false;
        }
        let pane = self.active_pane();
        let g = self.pane_geometry.get()[pane];
        if !g.valid() {
            return false;
        }
        let scale = g.zoom_scale(self.zoom_factor);
        let view = [
            f32::from(g.bounds.size.width),
            f32::from(g.bounds.size.height),
        ];
        let image = [g.width as f32 * scale, g.height as f32 * scale];
        let mut moved = false;
        for (axis, step) in [(0, dx), (1, dy)] {
            if step != 0. && image[axis] > view[axis] {
                let pan = &mut self.pans[pane][axis];
                let next =
                    (*pan + step * view[axis] / 3. / (image[axis] - view[axis])).clamp(0., 1.);
                moved |= next != *pan;
                *pan = next;
            }
        }
        if self.compare_sync {
            self.pans[1 - pane] = self.pans[pane];
        }
        moved
    }

    fn navigator_pan(&mut self, reference: bool, at: Point<Pixels>, inset: Bounds<Pixels>) {
        let pane = usize::from(!reference);
        let g = self.pane_geometry.get()[pane];
        if !g.valid() || f32::from(inset.size.width) <= 0. {
            return;
        }
        let u = [
            (f32::from(at.x - inset.origin.x) / f32::from(inset.size.width)).clamp(0., 1.),
            (f32::from(at.y - inset.origin.y) / f32::from(inset.size.height)).clamp(0., 1.),
        ];
        let target = [
            f32::from(g.bounds.size.width) / 2.,
            f32::from(g.bounds.size.height) / 2.,
        ];
        self.pans[pane] = g.pan_for(u, target, g.zoom_scale(self.zoom_factor));
        if self.compare_sync {
            self.pans[1 - pane] = self.pans[pane];
        }
    }

    /// Where the zoomed view sits in the frame: a small copy of the photo with
    /// the visible rectangle, at the pane's bottom-right. Click or drag it to
    /// move the view.
    /// The navigator's opacity this frame, or `None` once it has faded: full
    /// while the zoomed view moves or the pointer is over it, then a 200 ms
    /// fade after 1.5 s idle (gone at once with reduced motion).
    pub(super) fn navigator_opacity(&mut self, cx: &gpui::Context<Self>) -> Option<f32> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            self.zoom_factor.to_bits(),
            self.pans.map(|p| p.map(f32::to_bits)),
            self.selected,
            self.reference,
            self.mode == ViewMode::Compare,
        )
            .hash(&mut hasher);
        let key = hasher.finish();
        if key != self.navigator_key {
            self.navigator_key = key;
            self.navigator_seen = Instant::now();
        }
        let idle = self.navigator_seen.elapsed();
        if let Some(left) = NAVIGATOR_IDLE.checked_sub(idle) {
            if !self.navigator_wake.replace(true) {
                let wake = self.navigator_wake.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(left).await;
                    wake.set(false);
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            return Some(1.);
        }
        if self.reduce_motion() || self.navigator_drag.is_some() {
            return self.navigator_drag.is_some().then_some(1.);
        }
        let t = (idle - NAVIGATOR_IDLE).as_secs_f32() / NAVIGATOR_FADE.as_secs_f32();
        if t >= 1. {
            return None;
        }
        self.feedback.animating.set(true);
        Some(1. - t)
    }

    /// The pointer moved over a zoomed photo: bring the navigator back.
    pub(super) fn touch_navigator(&mut self, cx: &mut gpui::Context<Self>) {
        let hidden = self.navigator_seen.elapsed() >= NAVIGATOR_IDLE;
        self.navigator_seen = Instant::now();
        if hidden {
            cx.notify();
        }
    }

    pub(super) fn navigator(
        &self,
        reference: bool,
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
        opacity: f32,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let long = width.max(height).max(1) as f32;
        let (w, h) = (
            NAVIGATOR_EDGE * width as f32 / long,
            NAVIGATOR_EDGE * height as f32 / long,
        );
        let pane = usize::from(!reference);
        let geometry = self.pane_geometry.clone();
        let zoom_factor = self.zoom_factor;
        let pan = self.pans[pane];
        let ink = self.scrim_text();
        let g = geometry.get()[pane];
        let visible = if g.valid() {
            let scale = g.zoom_scale(zoom_factor);
            [
                (f32::from(g.bounds.size.width) / (g.width as f32 * scale)).min(1.),
                (f32::from(g.bounds.size.height) / (g.height as f32 * scale)).min(1.),
            ]
        } else {
            [1., 1.]
        };
        let inset = Rc::new(Cell::new(Bounds::default()));
        let measured = inset.clone();
        let pressed = inset.clone();
        let dragged = inset.clone();
        div()
            .id(if reference {
                "reference-navigator"
            } else {
                "navigator"
            })
            .absolute()
            .bottom(px(space::MD))
            .right(px(space::MD))
            .w(px(w + 2.))
            .h(px(h + 2.))
            .p(px(1.))
            .opacity(opacity)
            .bg(self.scrim())
            .cursor(CursorStyle::PointingHand)
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    move |bounds, (), window, _| {
                        let _ =
                            window.paint_image(bounds, Corners::default(), image.clone(), 0, false);
                    },
                )
                .size_full(),
            )
            // The visible rectangle, from the pane geometry of the last frame
            // (a separate element so it draws above the picture).
            .child(
                div()
                    .absolute()
                    .left(px(1. + w * pan[0] * (1. - visible[0])))
                    .top(px(1. + h * pan[1] * (1. - visible[1])))
                    .w(px((w * visible[0]).max(4.)))
                    .h(px((h * visible[1]).max(4.)))
                    .border_1()
                    .border_color(ink),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.navigator_drag = Some(reference);
                    this.navigator_pan(reference, event.position, pressed.get());
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.navigator_drag == Some(reference) && event.dragging() {
                    cx.stop_propagation();
                    this.navigator_pan(reference, event.position, dragged.get());
                    cx.notify();
                }
            }))
            .tooltip(self.tip("Drag to move the view", ""))
            .into_any_element()
    }

    /// Compare B: the candidate becomes the reference and the next frame in
    /// the direction of travel becomes the candidate ("winner stays").
    pub(super) fn promote_candidate(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(id) = self.selected_id() else {
            return;
        };
        if self.reference != Some(id) {
            self.reference = Some(id);
            self.reference_focus = false;
            self.pans[0] = self.pans[1];
            let direction = if self.navigation_direction < 0 { -1 } else { 1 };
            let before = self.selected;
            self.navigate_quietly(direction, cx);
            if self.selected == before {
                self.navigate_quietly(-direction, cx);
            }
        }
        let name = self
            .photo_index
            .get(&id)
            .map_or_else(String::new, |&i| self.photos[i].filename.clone());
        self.announce_action(
            format!("Reference: {name}"),
            Tone::Info,
            "Swap",
            Cmd::SwapPanes,
            "⇧B",
        );
    }

    /// Compare Shift+B: exchange reference and candidate in place.
    pub(super) fn swap_panes(&mut self, cx: &mut gpui::Context<Self>) {
        let (Some(reference), Some(current)) = (self.reference, self.selected_id()) else {
            self.announce("Pin a reference first · B", Tone::Warning);
            return;
        };
        let Some(position) = self.visible_position(reference) else {
            self.announce("The reference isn't in this view", Tone::Warning);
            return;
        };
        if reference == current {
            return;
        }
        self.reference = Some(current);
        self.pans.swap(0, 1);
        self.select(position, false, false, cx);
        let name = self
            .photo_index
            .get(&current)
            .map_or_else(String::new, |&i| self.photos[i].filename.clone());
        self.announce(format!("Swapped · reference {name}"), Tone::Info);
    }

    /// The selection, or the photo being reviewed, that a decision targets.
    pub(super) fn decision_targets(&self) -> HashSet<i64> {
        if self.multi.is_empty() {
            self.review_photo().map(|p| p.id).into_iter().collect()
        } else {
            self.multi.clone()
        }
    }

    /// Holds a batch decision on very many photos until Enter (true), so a
    /// stray key after Select All can't rewrite a whole library.
    pub(super) fn hold_bulk(
        &mut self,
        count: usize,
        rating: Option<u8>,
        flag: Option<Flag>,
        label: Option<ColorLabel>,
    ) -> bool {
        if count < BULK_CONFIRM_AT {
            return false;
        }
        self.pending_bulk = Some(PendingBulk {
            rating,
            flag,
            label,
            count,
        });
        true
    }

    pub(super) fn apply_bulk(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(bulk) = self.pending_bulk.take() else {
            return;
        };
        if self.decision_targets().len() != bulk.count {
            self.announce(
                "Selection changed · batch decision cancelled",
                Tone::Warning,
            );
            return;
        }
        self.decide_now(bulk.rating, bulk.flag, bulk.label, false, cx);
    }

    /// Enter applies a held batch decision; other keys wait (Escape cancels
    /// through Close, quit was routed before this).
    pub(super) fn bulk_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.pending_bulk.is_none() {
            return false;
        }
        if event.keystroke.key == "enter" {
            self.execute(Cmd::ApplyBulk, window, cx);
        }
        cx.notify();
        true
    }

    /// The inline question above the grid for a held batch decision.
    pub(super) fn bulk_bar(&self, cx: &gpui::Context<Self>) -> Option<AnyElement> {
        let bulk = self.pending_bulk.as_ref()?;
        Some(
            div()
                .id("bulk-confirm")
                .flex_shrink_0()
                .h(px(ui::CONTROL_ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(px(space::MD))
                .px(px(ui::PANEL_INSET))
                .bg(self.c("elevated_surface.background", "#1b1b1b"))
                .border_b_1()
                .border_color(self.warning())
                .child(icon("warning", self.warning()))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(copy::batch_question(
                            bulk.rating,
                            bulk.flag,
                            bulk.label,
                            bulk.count,
                        )),
                )
                .child(self.action_button(
                    "bulk-cancel",
                    "Cancel",
                    Cmd::CancelBulk,
                    ButtonKind::Secondary,
                    false,
                    cx,
                ))
                .child(self.action_button(
                    "bulk-apply",
                    "Apply",
                    Cmd::ApplyBulk,
                    ButtonKind::Primary,
                    false,
                    cx,
                ))
                .child(
                    div()
                        .text_size(ui::font(ui::text::LABEL))
                        .text_color(self.muted())
                        .child(format!("{} applies · Esc cancels", shortcut("Enter"))),
                )
                .into_any_element(),
        )
    }

    /// Home/End (and PageUp/PageDown in Catalog) jump through the view;
    /// Shift extends a Catalog selection. Home re-centres a zoomed photo.
    pub(super) fn jump_key(
        &mut self,
        key: &str,
        shift: bool,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.filtered.is_empty() || !matches!(key, "home" | "end" | "pageup" | "pagedown") {
            return false;
        }
        let grid = self.mode == ViewMode::Grid;
        if key == "home" && self.zoom && !grid && !shift {
            self.pans = [[0.5, 0.5]; 2];
            cx.notify();
            return true;
        }
        let last = self.filtered.len() - 1;
        let cols = self.grid_cols();
        let page = if grid {
            let rows = self
                .grid_range
                .map_or(1, |(start, end)| {
                    end.saturating_sub(start).saturating_sub(1)
                })
                .max(1);
            cols * rows
        } else {
            9
        };
        let target = match key {
            "home" => 0,
            "end" => last,
            "pageup" => self.selected.saturating_sub(page),
            _ => (self.selected + page).min(last),
        };
        if grid {
            self.select(target, false, shift, cx);
            self.reveal_grid_row(target / cols, true);
        } else {
            self.select(target, false, false, cx);
        }
        true
    }

    /// Scrolls the Catalog only when the row leaves the viewport (arrows),
    /// or centres it for jumps.
    pub(super) fn reveal_grid_row(&self, row: usize, jump: bool) {
        let strategy = match self.grid_range {
            _ if jump => ScrollStrategy::Center,
            Some((start, end)) if row > start && row + 1 < end => return,
            Some((start, _)) if row <= start => ScrollStrategy::Top,
            Some(_) => ScrollStrategy::Bottom,
            None => ScrollStrategy::Center,
        };
        self.grid_scroll.scroll_to_item(row, strategy);
    }

    /// Every constraint narrowing the view, in breadcrumb order.
    pub(super) fn constraints(&self) -> Vec<Constraint> {
        let mut out = Vec::new();
        if let Some(scope) = &self.scope {
            out.push(Constraint {
                id: "chip-scope",
                label: self.scope_display(scope),
                dot: None,
                clear: Cmd::ClearScope,
            });
        }
        if self.duplicates_only {
            out.push(Constraint {
                id: "chip-collection",
                label: "Duplicates".into(),
                dot: None,
                clear: Cmd::Filter(Filter::All),
            });
        } else if self.filter != Filter::All {
            out.push(Constraint {
                id: "chip-collection",
                label: collection_label(self.filter).into(),
                dot: None,
                clear: Cmd::Filter(Filter::All),
            });
        }
        if let Some(label) = self.label_filter {
            out.push(Constraint {
                id: "chip-label",
                label: label.as_str().to_owned(),
                dot: Some(label),
                clear: Cmd::LabelFilter(label),
            });
        }
        if self.min_rating > 0 {
            out.push(Constraint {
                id: "chip-rating",
                label: format!("★{}+", self.min_rating),
                dot: None,
                clear: Cmd::SetMinRating(0),
            });
        }
        if self.bursts_only {
            out.push(Constraint {
                id: "chip-bursts",
                label: "Unresolved bursts".into(),
                dot: None,
                clear: Cmd::BurstsOnly,
            });
        }
        out
    }

    /// A removable breadcrumb chip: name plus a small × that clears it.
    pub(super) fn constraint_chip(
        &self,
        constraint: Constraint,
        max_width: f32,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let hover = self.hover_bg();
        let clear = constraint.clear.clone();
        let label = constraint.label.clone();
        div()
            .id(constraint.id)
            .flex()
            .flex_shrink_0()
            .min_w(px(0.))
            .max_w(px(max_width))
            .h(px(ui::COMPACT_CONTROL_HEIGHT))
            .items_center()
            .gap(px(space::XS))
            .pl(px(space::SM))
            .bg(self.selected_bg())
            .when_some(
                constraint.dot.and_then(|l| self.label_color(l)),
                |s, color| s.child(div().size(px(8.)).flex_shrink_0().bg(color)),
            )
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(constraint.label),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{}-clear", constraint.id)))
                    .size(px(ui::COMPACT_CONTROL_HEIGHT))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(icon("close", self.icon_muted()).size(px(12.)))
                    .tooltip(self.tip(&format!("Remove “{label}”"), ""))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute(clear.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// Clears every filter and the folder scope: the whole library.
    pub(super) fn clear_filters(&mut self) {
        let id = self.selected_id();
        self.scope = None;
        self.duplicates_only = false;
        self.label_filter = None;
        self.bursts_only = false;
        self.reference_focus = false;
        self.filter = Filter::All;
        self.min_rating = 0;
        self.settings.filter = "all".into();
        self.settings.min_rating = 0;
        self.save_settings();
        self.rebuild(id);
    }

    /// Photos the rating filter alone hides from this view.
    fn hidden_by_rating(&self) -> usize {
        if self.min_rating == 0 {
            return 0;
        }
        self.photos
            .iter()
            .filter(|p| p.rating < self.min_rating && self.accepts_ignoring_rating(p))
            .count()
    }

    /// The centre pane when nothing is visible: first run, a finished cull,
    /// or a filter with no matches, each with its way forward.
    pub(super) fn empty(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let library_empty = self.photos.is_empty();
        // The first folder is still being read: say so, not "insert a card".
        if library_empty && self.busy {
            let found = if self.status.starts_with("Indexing") {
                format!("{} · originals are not modified", self.status)
            } else {
                "Photos appear here as they are found · originals are not modified".into()
            };
            return div()
                .id("empty-reading")
                .flex_1()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(ui::PANEL_INSET))
                .text_center()
                .gap(px(space::MD))
                .bg(self.canvas_bg())
                .child(
                    div()
                        .text_size(ui::font(ui::text::DISPLAY))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Reading photos…"),
                )
                .child(div().text_color(self.muted()).child(found))
                .child(
                    div()
                        .w(px(160.))
                        .h(px(2.))
                        .bg(self.border())
                        .child(div().w(px(48.)).h_full().bg(self.accent())),
                )
                .into_any_element();
        }
        let scope = match &self.scope {
            Some(scope) => self.scope_display(scope),
            None => "the whole library".into(),
        };
        let counts = self.scope_counts.as_ref().unwrap_or(&self.counts);
        let only_unreviewed = self.filter == Filter::Unrated
            && !self.duplicates_only
            && self.label_filter.is_none()
            && !self.bursts_only
            && self.min_rating == 0
            && counts.all > 0;
        let collection = if self.duplicates_only {
            "duplicates".to_owned()
        } else if let Some(label) = self.label_filter {
            format!("{} photos", label.as_str().to_lowercase())
        } else {
            match self.filter {
                Filter::All => "photos".into(),
                f => collection_label(f).to_lowercase(),
            }
        };
        let (title, body): (String, String) = if library_empty {
            (
                "Insert a card or open a folder".into(),
                // One line per verb, at a readable measure (R4-catalog-03).
                "Import from Card copies and verifies photos into your library.\nOpen Folder culls them where they are; nothing is copied or modified.".into(),
            )
        } else if self.duplicates_only {
            let (title, body, _) = copy::duplicates_empty(self.duplicate_scan.as_ref(), &scope);
            (title, body)
        } else if self.filter == Filter::Trash && self.settings.trash_mode == "system" {
            // Deleting goes to the system Trash in this mode: this view only
            // holds photos trashed in-app earlier, so say where the rest went.
            (
                "Nothing in Shadowroom's Trash".into(),
                format!(
                    "Deleted photos go to the system Trash · restore them from your file manager. \
                     For in-app restore with ratings, choose the in-app Trash in {}.",
                    settings_noun()
                ),
            )
        } else if self.filter == Filter::Trash {
            (
                "Trash is empty".into(),
                "Trashed photos can be restored with their ratings and flags.".into(),
            )
        } else if only_unreviewed {
            let flagless = counts.all.saturating_sub(counts.picks + counts.rejects);
            (
                format!("All {} reviewed", plural(counts.all, "photo", "photos")),
                format!(
                    "{} · {} · {} without a flag",
                    plural(counts.picks, "pick", "picks"),
                    plural(counts.rejects, "reject", "rejects"),
                    copy::count(flagless)
                ),
            )
        } else if self.min_rating > 0 {
            let hidden = self.hidden_by_rating();
            (
                format!(
                    "No {collection} rated {}",
                    copy::rating_threshold(self.min_rating).to_lowercase()
                ),
                format!("{} hidden by the rating filter", copy::count(hidden)),
            )
        } else {
            (
                format!(
                    "No {collection} {}",
                    if self.scope.is_some() {
                        scope.clone()
                    } else {
                        "in this view".into()
                    }
                ),
                "Change or clear the filters to see more photos.".into(),
            )
        };
        let mut drives = None;
        let mut actions = div()
            .id("empty-actions")
            .pt(px(space::SM))
            .flex()
            .flex_wrap()
            .justify_center()
            .gap(px(space::SM));
        if library_empty {
            actions = actions
                .child(
                    self.action_button(
                        "empty-import",
                        copy::IMPORT_FROM_CARD,
                        Cmd::Import,
                        ButtonKind::Primary,
                        false,
                        cx,
                    )
                    .tooltip(self.tip("Copy and verify into your library", "⌘⇧I")),
                )
                .child(
                    self.action_button(
                        "empty-open",
                        copy::OPEN_FOLDER,
                        Cmd::OpenFolder,
                        ButtonKind::Secondary,
                        false,
                        cx,
                    )
                    .tooltip(self.tip("Cull where the files are; nothing is copied", "⌘O")),
                );
            // Two decisions, two buttons (R4-catalog-03). Other drives are a
            // quiet list below; the card Import from Card opens is not repeated.
            let card = self
                .drives
                .iter()
                .position(|drive| drive.removable && !self.import_ui.active());
            let others: Vec<_> = self
                .drives
                .iter()
                .enumerate()
                .filter(|(n, _)| Some(*n) != card)
                .collect();
            if !others.is_empty() {
                let mut list = div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .items_center()
                    .gap_x(px(space::SM))
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(self.muted())
                    .child("or import from");
                for (n, drive) in others {
                    let path = drive.path.clone();
                    list = list.child(
                        div()
                            .id(("empty-drive", n))
                            .cursor_pointer()
                            .text_color(self.text())
                            .hover(|s| s.underline())
                            .child(drive.name.clone())
                            .tooltip(self.tip(&format!("Copy and verify {}", drive.name), ""))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.scan_for_import(path.clone());
                                cx.notify();
                            })),
                    );
                }
                drives = Some(list);
            }
        } else if self.duplicates_only {
            let (_, _, button) = copy::duplicates_empty(self.duplicate_scan.as_ref(), &scope);
            if let Some(label) = button {
                actions = actions.child(self.action_button(
                    "empty-scan-duplicates",
                    label,
                    Cmd::ScanDuplicates,
                    ButtonKind::Secondary,
                    false,
                    cx,
                ));
            }
        } else if self.filter == Filter::Trash {
            if self.settings.trash_mode == "system" {
                actions = actions.child(self.action_button(
                    "empty-open-system-trash",
                    "Open System Trash",
                    Cmd::OpenSystemTrash,
                    ButtonKind::Secondary,
                    false,
                    cx,
                ));
            }
        } else if only_unreviewed {
            actions = actions
                .child(self.action_button(
                    "empty-show-picks",
                    "Show Picks",
                    Cmd::Filter(Filter::Picks),
                    ButtonKind::Secondary,
                    false,
                    cx,
                ))
                .child(
                    self.action_button(
                        "empty-export-picks",
                        "Export Picks…",
                        Cmd::ExportPicks,
                        ButtonKind::Primary,
                        counts.picks == 0,
                        cx,
                    )
                    .tooltip(self.tip("Copy picked photos to a folder", "⌘⇧E")),
                );
        } else {
            if self.min_rating > 0 {
                actions = actions.child(self.action_button(
                    "empty-clear-rating",
                    "Clear rating filter",
                    Cmd::SetMinRating(0),
                    ButtonKind::Primary,
                    false,
                    cx,
                ));
            }
            actions = actions.child(self.action_button(
                "empty-show-all",
                "Show all photos",
                Cmd::ClearFilters,
                ButtonKind::Secondary,
                false,
                cx,
            ));
        }
        div()
            .id("empty-state")
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(ui::PANEL_INSET))
            .text_center()
            .gap(px(space::MD))
            .bg(self.canvas_bg())
            .child(
                div()
                    .text_size(ui::font(ui::text::DISPLAY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(ui::scaled(EMPTY_MEASURE)))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .text_color(self.muted())
                    .children(body.lines().map(str::to_owned)),
            )
            .child(actions)
            .children(drives)
            // Key hints only where they can do something: an empty library.
            .when(library_empty, |s| {
                s.child(
                    div().pt(px(space::LG)).child(
                        self.key_hints(&[
                            ("⌘⇧I", "Import"),
                            ("⌘O", "Open"),
                            ("F1", "Guide"),
                            ("⌘K", "Commands"),
                        ])
                        .justify_center(),
                    ),
                )
            })
            .into_any_element()
    }

    /// The window title names where you are, for task switchers and
    /// assistive technology: "photo — collection — Shadowroom".
    pub(super) fn sync_window_title(&mut self, window: &mut Window) {
        let place = self
            .scope
            .as_ref()
            .map(|s| self.scope_display(s))
            .unwrap_or_else(|| {
                if self.duplicates_only {
                    "Duplicates".into()
                } else {
                    collection_label(self.filter).to_owned()
                }
            });
        let title = if self.import_ui.open {
            let source = self
                .import_ui
                .scan
                .as_ref()
                .map(|scan| scan.volume.clone())
                .unwrap_or_default();
            if source.is_empty() {
                "Import — Shadowroom".to_owned()
            } else {
                format!("Import — {source} — Shadowroom")
            }
        } else if self.photos.is_empty() {
            "Shadowroom".to_owned()
        } else if self.mode == ViewMode::Grid {
            format!("Catalog — {place} — Shadowroom")
        } else if let Some(photo) = self.selected_photo() {
            format!("{} — {place} — Shadowroom", photo.filename)
        } else {
            format!("{place} — Shadowroom")
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    /// Inspector for a multi-selection: what the batch carries, mixed or
    /// shared, and label swatches that apply to every member (CAT-10).
    pub(super) fn selection_inspector(
        &self,
        summary: &SelectionSummary,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let mut flags = Vec::new();
        for (n, name) in summary.flags.iter().zip(["Pick", "Reject", "no flag"]) {
            if *n > 0 {
                flags.push(format!("{} {name}", copy::count(*n)));
            }
        }
        let mut ratings = Vec::new();
        for r in (0..=5).rev() {
            let n = summary.ratings[r];
            if n > 0 {
                ratings.push(if r == 0 {
                    format!("Unrated ×{}", copy::count(n))
                } else {
                    format!("★{r} ×{}", copy::count(n))
                });
            }
        }
        let common_label = summary.common_label();
        let label_text = match common_label {
            Some(ColorLabel::None) => "None".to_owned(),
            Some(label) => label.as_str().to_owned(),
            None => "mixed".to_owned(),
        };
        let mut swatches = div().flex().gap(px(space::XS)).pt(px(space::XS));
        for (n, label) in [
            ColorLabel::None,
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ]
        .into_iter()
        .enumerate()
        {
            swatches = swatches.child(self.label_swatch(
                ("batch-label", n),
                label,
                common_label == Some(label),
                if label == ColorLabel::None {
                    "Clear the label of every selected photo".to_owned()
                } else {
                    format!("Label every selected photo {}", label.as_str())
                },
                cx,
            ));
        }
        div()
            .id("batch-inspector")
            .flex()
            .flex_col()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(ui::font(ui::text::TITLE))
                    .child(format!(
                        "{} selected",
                        plural(summary.count, "photo", "photos")
                    )),
            )
            .child(
                div()
                    .pt(px(space::SM))
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(self.muted())
                    .child("Decisions apply to every selected photo, including those off screen."),
            )
            .child(self.inspector_section("Selection"))
            .child(self.metadata_row("Flag", flags.join(" · ")))
            .child(self.metadata_row("Rating", ratings.join(" · ")))
            .child(self.metadata_row("Label", label_text))
            .child(self.inspector_section("Label"))
            .child(swatches)
            .into_any_element()
    }

    /// A short notice the first time something happens in a session, tagged
    /// with `key` so [`Self::withdraw_notice`] can retract it early.
    pub(super) fn hint_once(&mut self, key: &'static str, text: &str) {
        if self.shown_hints.insert(key) {
            self.announce(text, Tone::Info);
            if let Some(flash) = &mut self.feedback.flash {
                flash.key = Some(key);
            }
        }
    }
}
