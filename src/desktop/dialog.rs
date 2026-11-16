//! Shared modal frame, dialog button kinds and keyboard focus for dialog controls.
//!
//! Every modal uses one scrim strength (`shadowroom.scrim.modal`), drawn once
//! beneath the topmost modal so stacked dialogs never compound, one elevated surface and
//! one 13 px title bar. Dialog controls that take keyboard focus register a
//! persistent [`FocusHandle`] here; `Workspace::key` still sees every key first
//! (focused children have no key handlers), so Ctrl+Q keeps quitting.
use super::*;

/// Dialog widths. A dialog nested over another is `NESTED_INSET` narrower.
pub(super) const DIALOG_SMALL: f32 = 560.;
pub(super) const DIALOG_MEDIUM: f32 = 620.;
pub(super) const DIALOG_LARGE: f32 = 680.;
pub(super) const NESTED_INSET: f32 = 40.;

/// One button hierarchy for every dialog: at most one filled button per dialog.
/// Quiet text-and-icon controls (close, toggles) stay on `Workspace::button`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ButtonKind {
    /// 1 px outline: Cancel and other non-default actions.
    Secondary,
    /// Accent fill: the default action (Return).
    Primary,
    /// Error fill: irreversible actions.
    Destructive,
    /// Error outline and text: recoverable destructive actions.
    DestructiveQuiet,
}

/// What Space/Return/arrow keys do on a focused dialog control.
#[derive(Clone)]
pub(super) struct FocusTarget {
    pub cmd: Option<Cmd>,
    /// `(group, index, count)` for options in a choice group or list; arrows
    /// select the neighbouring option.
    pub group: Option<(SharedString, usize, usize)>,
    /// Buttons take Return themselves; elsewhere Return is the dialog default.
    pub button: bool,
    /// Scroll container and child index to reveal when arrows focus this option.
    pub reveal: Option<(ScrollHandle, usize)>,
}
impl FocusTarget {
    pub fn button(cmd: Cmd) -> Self {
        Self {
            cmd: Some(cmd),
            group: None,
            button: true,
            reveal: None,
        }
    }
    pub fn option(cmd: Cmd, group: SharedString, index: usize, count: usize) -> Self {
        Self {
            cmd: Some(cmd),
            group: Some((group, index, count)),
            button: false,
            reveal: None,
        }
    }
    pub fn reveal(mut self, scroll: &ScrollHandle, child: usize) -> Self {
        self.reveal = Some((scroll.clone(), child));
        self
    }
}

/// One dialog's own Tab cycle: Preferences or the quit prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FocusScope {
    #[default]
    Preferences,
    QuitPrompt,
}

/// A control registered this frame, in paint order, for the Tab cycle.
struct TabStop {
    key: SharedString,
    tab_index: isize,
    tab_stop: bool,
    scope: FocusScope,
}

/// Persistent focus handles keyed by control, plus this frame's targets.
#[derive(Default)]
pub(super) struct DialogFocus {
    handles: HashMap<SharedString, FocusHandle>,
    targets: HashMap<SharedString, FocusTarget>,
    /// This frame's controls in paint order, so Tab stays inside the topmost
    /// dialog instead of walking the window (and the dock grips under it).
    stops: Vec<TabStop>,
    /// The dialog whose controls are being registered.
    scope: FocusScope,
}
impl DialogFocus {
    /// Forget last frame's targets; `render` registers the visible ones again.
    pub fn begin_frame(&mut self) {
        self.targets.clear();
        self.stops.clear();
        self.scope = FocusScope::default();
    }
    /// The control Tab (or Shift+Tab with `back`) moves to inside `scope`:
    /// ordered by tab index, then paint order, wrapping at both ends. With
    /// nothing in the scope focused, Tab starts at the first stop.
    pub fn cycle(&self, scope: FocusScope, back: bool, window: &Window) -> Option<FocusHandle> {
        let mut stops: Vec<&TabStop> = self
            .stops
            .iter()
            .filter(|stop| stop.scope == scope && stop.tab_stop)
            .collect();
        stops.sort_by_key(|stop| stop.tab_index);
        let keys: Vec<&str> = stops.iter().map(|stop| stop.key.as_ref()).collect();
        let current = keys.iter().position(|key| {
            self.handles
                .get(*key)
                .is_some_and(|handle| handle.is_focused(window))
        });
        let next = cycle_index(current, keys.len(), back)?;
        self.handles.get(keys[next]).cloned()
    }
    /// Whether a control of `scope` drawn this frame holds keyboard focus.
    pub fn scope_focused(&self, scope: FocusScope, window: &Window) -> bool {
        self.stops.iter().any(|stop| {
            stop.scope == scope
                && self
                    .handles
                    .get(&stop.key)
                    .is_some_and(|handle| handle.is_focused(window))
        })
    }
    /// The focused control rendered this frame, if any.
    pub fn focused(&self, window: &Window) -> Option<(SharedString, FocusTarget)> {
        self.handles.iter().find_map(|(key, handle)| {
            if !handle.is_focused(window) {
                return None;
            }
            self.targets
                .get(key)
                .map(|target| (key.clone(), target.clone()))
        })
    }
    /// A focused handle whose control is no longer rendered (its dialog closed
    /// or its section changed): focus must return to the workspace root, or
    /// keys would dispatch to no listener.
    pub fn focus_is_stale(&self, window: &Window) -> bool {
        self.handles
            .iter()
            .any(|(key, handle)| handle.is_focused(window) && !self.targets.contains_key(key))
    }
    /// Any dialog control (rendered yet or not) holds keyboard focus.
    pub fn owns_focus(&self, window: &Window) -> bool {
        self.handles
            .values()
            .any(|handle| handle.is_focused(window))
    }
    pub fn handle(&self, key: &str) -> Option<FocusHandle> {
        self.handles.get(key).cloned()
    }
    pub fn target(&self, key: &str) -> Option<FocusTarget> {
        self.targets.get(key).cloned()
    }
}

/// The next position in a Tab cycle of `count` stops from `current`.
fn cycle_index(current: Option<usize>, count: usize, back: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, back) {
        (None, false) => 0,
        (None, true) => count - 1,
        (Some(i), false) => (i + 1) % count,
        (Some(i), true) => (i + count - 1) % count,
    })
}

impl Workspace {
    /// Controls registered from here on belong to `scope`'s Tab cycle.
    pub(super) fn focus_scope(&self, scope: FocusScope) {
        self.dialog_focus.borrow_mut().scope = scope;
    }

    /// Tab / Shift+Tab inside the topmost dialog: focus wraps within it.
    pub(super) fn cycle_dialog_focus(
        &mut self,
        scope: FocusScope,
        back: bool,
        window: &mut Window,
    ) {
        self.focus_visible = true;
        let next = self.dialog_focus.borrow().cycle(scope, back, window);
        if let Some(handle) = next {
            window.focus(&handle);
        }
    }

    /// Keyboard focus belongs to the topmost dialog: when the quit prompt or
    /// Preferences is on top and none of its controls holds focus (it just
    /// opened, or the focused control went away), its default control takes
    /// it, so the first Tab moves from a known place.
    pub(super) fn claim_dialog_focus(&self, window: &mut Window, cx: &App) {
        let (scope, key, tab_index) = if self.feedback.quit_prompt.is_some() {
            (FocusScope::QuitPrompt, "quit-retry".to_owned(), 1)
        } else if self.top_modal() == Some(Overlay::Preferences) {
            let index = preferences::SECTIONS
                .iter()
                .position(|(section, _)| *section == self.preferences_section)
                .unwrap_or(0);
            (FocusScope::Preferences, format!("prefs-tab-{index}"), 1)
        } else {
            return;
        };
        if self.dialog_focus.borrow().scope_focused(scope, window) {
            return;
        }
        let handle = self.dialog_handle(&key, tab_index, cx);
        window.focus(&handle);
    }

    /// Keyboard focus ring. Themes may override it; otherwise it follows `border.focused`.
    pub(super) fn focus_ring(&self) -> Hsla {
        if self.theme.colors.contains_key("shadowroom.focus_ring") {
            self.c("shadowroom.focus_ring", "#5b9bd5")
        } else {
            self.c("border.focused", "#5b9bd5")
        }
    }

    /// Text colour with the higher WCAG contrast on `fill`: white or near-black.
    pub(super) fn text_on(&self, fill: Hsla) -> Hsla {
        let c = gpui::Rgba::from(fill);
        let channel = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let l = 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
        // #0b0b0b has relative luminance ~0.0034.
        if 1.05 / (l + 0.05) >= (l + 0.05) / 0.0534 {
            rgb(0xffffff).into()
        } else {
            rgb(0x0b0b0b).into()
        }
    }

    /// Topmost open modal; menus (rating filter, context menu) never draw a scrim.
    pub(super) fn top_modal(&self) -> Option<Overlay> {
        self.overlays()
            .find(|overlay| !matches!(overlay, Overlay::RatingFilter | Overlay::ImageContext))
    }

    /// Full-window, untinted layer that centres a modal and blocks the
    /// pointer beneath it. The tint is [`Self::modal_tint`], painted once.
    pub(super) fn modal_scrim(&self, _overlay: Overlay) -> Div {
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
    }

    /// The one scrim, painted directly beneath the topmost modal so stacked
    /// dialogs never compound. It keeps its element id while any modal is
    /// open, so it fades in only when the first modal opens: a dialog opened
    /// over Preferences fades its own surface while the tint holds steady.
    pub(super) fn modal_tint(&self) -> AnyElement {
        self.fade_in(
            "modal-tint-in",
            motion::OVERLAY,
            0.3,
            div()
                .absolute()
                .inset_0()
                .bg(self.c("shadowroom.scrim.modal", "#00000099")),
        )
    }

    /// The one dialog surface: elevated background, variant border and shadow.
    pub(super) fn modal_surface(&self, id: impl Into<ElementId>, width: f32) -> Stateful<Div> {
        div()
            .id(id)
            .occlude()
            .w(px(width))
            .max_w_full()
            .max_h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(self.c("elevated_surface.background", "#202020"))
            .border_1()
            .border_color(self.c("border.variant", "#333333"))
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
            }])
    }

    /// 45 px title bar: 13 px medium title, optional close button on the right.
    pub(super) fn modal_header(
        &self,
        title: impl IntoElement,
        close: Option<(&'static str, Cmd)>,
        cx: &Context<Self>,
    ) -> Div {
        div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .pl(px(ui::DIALOG_INSET))
            .pr(px(ui::BAR_INSET))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::SM))
            .border_b_1()
            .border_color(self.border())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .text_size(ui::font(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .when_some(close, |s, (id, cmd)| {
                s.child(
                    self.button(id, "", "close", cmd, false, cx)
                        .tooltip(self.tip("Close", "Esc")),
                )
            })
    }

    /// Footer bar: status text on the left, right-aligned buttons.
    pub(super) fn modal_footer(&self) -> Div {
        div()
            .min_h(px(52.))
            .flex_shrink_0()
            .px(px(ui::DIALOG_INSET))
            .py(px(space::SM))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::MD))
            .border_t_1()
            .border_color(self.border())
    }

    /// A dialog button without a click handler, for callers with their own action.
    pub(super) fn dialog_button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        kind: ButtonKind,
        disabled: bool,
    ) -> Stateful<Div> {
        let hover = self.c("element.hover", "#303030");
        let accent = self.accent();
        let error = self.c("error", "#d46a6a");
        let base = div()
            .id(id)
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .h(px(ui::CONTROL_HEIGHT))
            .px(px(space::MD))
            .border_1()
            .border_color(transparent_black())
            .occlude()
            .whitespace_nowrap()
            .child(label.to_owned());
        let styled = match kind {
            ButtonKind::Secondary => base
                .border_color(self.c("border.variant", "#333333"))
                .when(!disabled, |s| s.hover(move |s| s.bg(hover))),
            ButtonKind::Primary => base
                .bg(accent)
                .border_color(accent)
                .text_color(self.text_on(accent))
                .when(!disabled, |s| s.hover(|s| s.opacity(0.88))),
            ButtonKind::Destructive => base
                .bg(error)
                .border_color(error)
                .text_color(self.text_on(error))
                .when(!disabled, |s| s.hover(|s| s.opacity(0.88))),
            ButtonKind::DestructiveQuiet => base
                .border_color(error)
                .text_color(error)
                .when(!disabled, |s| s.hover(move |s| s.bg(hover))),
        };
        styled
            .when(disabled, |s| s.opacity(0.45))
            .when(!disabled, |s| s.cursor_pointer())
    }

    /// A dialog button that runs `cmd`; disabled buttons ignore clicks.
    pub(super) fn action_button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        cmd: Cmd,
        kind: ButtonKind,
        disabled: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        self.dialog_button(id, label, kind, disabled)
            .when(!disabled, |s| {
                s.on_click(
                    cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)),
                )
            })
    }

    /// 2 px focus ring inside the control's bounds, so focus never shifts layout
    /// or gets clipped, with a 1 px surface-coloured gap inside it so the ring
    /// stays visible on accent-filled buttons.
    pub(super) fn focus_ring_overlay(&self) -> Div {
        div()
            .absolute()
            .inset_0()
            .border_2()
            .border_color(self.focus_ring())
            .child(
                div()
                    .size_full()
                    .border_1()
                    .border_color(self.c("elevated_surface.background", "#202020")),
            )
    }

    /// The persistent focus handle for a dialog control, created on first use.
    pub(super) fn dialog_handle(&self, key: &str, tab_index: isize, cx: &App) -> FocusHandle {
        self.dialog_focus
            .borrow_mut()
            .handles
            .entry(SharedString::from(key.to_owned()))
            .or_insert_with(|| cx.focus_handle().tab_index(tab_index))
            .clone()
    }

    /// Makes `element` a keyboard focus target. `tab_stop` false keeps it
    /// reachable by arrows only (roving focus inside choice groups). The ring
    /// shows only while the keyboard, not the pointer, moved focus last.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn focusable(
        &self,
        element: Stateful<Div>,
        key: &str,
        tab_index: isize,
        tab_stop: bool,
        target: FocusTarget,
        window: &Window,
        cx: &App,
    ) -> Stateful<Div> {
        let handle = self.dialog_handle(key, tab_index, cx).tab_stop(tab_stop);
        {
            let mut focus = self.dialog_focus.borrow_mut();
            let key = SharedString::from(key.to_owned());
            let scope = focus.scope;
            focus.stops.push(TabStop {
                key: key.clone(),
                tab_index,
                tab_stop,
                scope,
            });
            focus.targets.insert(key, target);
        }
        let ring = self.focus_visible && handle.is_focused(window);
        element
            .track_focus(&handle)
            .relative()
            .when(ring, |s| s.child(self.focus_ring_overlay()))
    }

    /// Runs the focused dialog control's command (Space, or Return on a button).
    pub(super) fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let focused = self.dialog_focus.borrow().focused(window);
        let Some((_, FocusTarget { cmd: Some(cmd), .. })) = focused else {
            return false;
        };
        self.execute(cmd, window, cx);
        true
    }

    /// Whether Return belongs to the focused control rather than the dialog default.
    pub(super) fn focused_button(&self, window: &Window) -> bool {
        self.dialog_focus
            .borrow()
            .focused(window)
            .is_some_and(|(_, target)| target.button)
    }

    /// Arrow keys inside a choice group or list: select and focus the neighbour.
    pub(super) fn move_in_group(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let focused = self.dialog_focus.borrow().focused(window);
        let Some((
            _,
            FocusTarget {
                group: Some((group, index, count)),
                ..
            },
        )) = focused
        else {
            return false;
        };
        let next = (index as isize + delta).clamp(0, count as isize - 1) as usize;
        if next == index {
            return true;
        }
        let key = format!("{group}-{next}");
        let (handle, target) = {
            let focus = self.dialog_focus.borrow();
            (focus.handle(&key), focus.target(&key))
        };
        if let Some(handle) = handle {
            window.focus(&handle);
        }
        if let Some(target) = target {
            if let Some((scroll, child)) = &target.reveal {
                scroll.scroll_to_item(*child);
            }
            if let Some(cmd) = target.cmd {
                self.execute(cmd, window, cx);
            }
        }
        true
    }
}

impl Workspace {
    /// Add network host. Validation errors sit under the alias field instead
    /// of in the toast across the window.
    pub(super) fn add_host_view(&self, cx: &Context<Self>) -> AnyElement {
        let Some(AddHostForm {
            fields,
            active,
            edit,
            error,
        }) = &self.add_host
        else {
            return div().into_any_element();
        };
        let error_color = self.c("error", "#d46a6a");
        let mut body = div()
            .p(px(ui::DIALOG_INSET))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(div().text_color(self.muted()).child(
                "Use your existing SSH configuration. Tab changes fields; Enter checks the agent.",
            ));
        for (n, label) in ["SSH alias or user@host", "Remote command (optional)"]
            .into_iter()
            .enumerate()
        {
            let invalid = n == 0 && error.is_some();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .child(label)
                    .child(
                        div()
                            .id(("host-field", n))
                            .min_h(px(ui::scaled(ui::FIELD_HEIGHT)))
                            .p(px(ui::FIELD_INSET))
                            .border_1()
                            .border_color(if invalid {
                                error_color
                            } else if *active == n {
                                self.accent()
                            } else {
                                // Text-field boundary: 3:1 (WCAG 1.4.11).
                                self.control_border()
                            })
                            .bg(self.bg())
                            .overflow_hidden()
                            .child(self.input_field(
                                edit,
                                &fields[n],
                                "",
                                *active == n,
                                !self.palette,
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(form) = &mut this.add_host {
                                    form.focus(n);
                                }
                                cx.notify();
                            })),
                    )
                    .when_some(error.as_ref().filter(|_| n == 0), |s, message| {
                        s.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(space::XS))
                                .text_size(ui::font(11.))
                                .text_color(error_color)
                                .child(icon("warning", error_color).size(px(12.)))
                                .child(message.clone()),
                        )
                    }),
            );
        }
        self.modal_scrim(Overlay::AddHost)
            .p(px(space::XL))
            .child(
                self.modal_surface("add-host", DIALOG_SMALL)
                    .child(self.modal_header(
                        "Add network host",
                        Some(("host-close", Cmd::Close)),
                        cx,
                    ))
                    .child(body)
                    .child(
                        self.modal_footer().justify_end().child(
                            div()
                                .flex()
                                .gap(px(space::SM))
                                .child(self.action_button(
                                    "host-cancel",
                                    "Cancel",
                                    Cmd::Close,
                                    ButtonKind::Secondary,
                                    false,
                                    cx,
                                ))
                                .child(self.action_button(
                                    "host-save",
                                    "Add & connect",
                                    Cmd::SaveRemoteHost,
                                    ButtonKind::Primary,
                                    false,
                                    cx,
                                )),
                        ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Explicit import: the parent glob carries gpui::test, which shadows #[test].
    use super::cycle_index;

    #[test]
    fn dialog_tab_cycle_wraps_inside_the_dialog() {
        // Nothing focused yet: Tab starts at the first stop, Shift+Tab at the last.
        assert_eq!(cycle_index(None, 3, false), Some(0));
        assert_eq!(cycle_index(None, 3, true), Some(2));
        // Both ends wrap within the dialog instead of leaving it.
        assert_eq!(cycle_index(Some(2), 3, false), Some(0));
        assert_eq!(cycle_index(Some(0), 3, true), Some(2));
        assert_eq!(cycle_index(Some(1), 3, false), Some(2));
        assert_eq!(cycle_index(None, 0, false), None);
    }
}
