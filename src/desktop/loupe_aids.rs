//! Inspection aids around the loupe: Focus mode without chrome, focus-peaking
//! colour and sensitivity, the clipping overlay and zoom presets.
use super::*;

impl Workspace {
    /// The title bar, view toolbar and status bar. Focus mode hides them so
    /// the photo gets the height (REV-25); Import always keeps them.
    pub(super) fn chrome_visible(&self) -> bool {
        !self.focus_mode || self.import_ui.open
    }

    /// Notices and save failures while Focus mode hides the status bar: a
    /// plate at the top of the canvas, so "Focus mode · Esc to exit", Undo
    /// and Retry stay reachable. Nothing shows when there is nothing to say.
    pub(super) fn focus_notices(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let notice = self.status_notice(cx);
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let save = failed.then(|| self.save_state_view(cx)).flatten();
        if notice.is_none() && save.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(px(space::SM))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("focus-notices")
                        .max_w(px(640.))
                        .min_w(px(0.))
                        .h(px(ui::COMPACT_CONTROL_HEIGHT + 2. * ui::STATUS_INSET + 2.))
                        .px(px(space::SM))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .bg(self.c("status_bar.background", "#141414"))
                        .border_1()
                        .border_color(self.border())
                        .text_size(px(ui::text::LABEL))
                        .text_color(self.muted())
                        .occlude()
                        .children(notice)
                        .children(save),
                )
                .into_any_element(),
        )
    }
}
