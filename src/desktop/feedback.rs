//! Transient feedback and system status: the status-bar notice, decision
//! confirmations on the photo, the save indicator, the error toast and the
//! quit-with-unsaved-decisions prompt.
//!
//! `status` stays the plumbing string automation reads; anything the user
//! should *see* goes through [`Workspace::announce`], which also mirrors it
//! into `status`. Notices expire on the poll loop, so no extra timers run.
use super::*;

/// How long a plain status-bar notice stays up.
pub(super) const NOTICE_TTL: Duration = Duration::from_secs(4);
/// Notices that offer an action (Undo, Reveal, Scan again) stay a little longer.
pub(super) const ACTION_TTL: Duration = Duration::from_secs(6);
/// The on-photo confirmation after a decision moves on to the next frame.
pub(super) const DECISION_NOTE_TTL: Duration = Duration::from_millis(1200);
/// "Saved" lingers this long after the last decision write, then clears.
pub(super) const SAVED_TTL: Duration = Duration::from_millis(1500);
/// The on-photo scrim already used by filmstrip and grid badges.
const SCRIM: u32 = 0x0f0f0fd0;
const SCRIM_TEXT: u32 = 0xe6e6e6;
const SCRIM_MUTED: u32 = 0x9a9a9a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    /// Neutral acknowledgement: text colour, no glyph.
    Info,
    /// Completed work (import verified, export written): `success` check.
    Success,
    /// A guard or something that needs a second look: `warning` glyph.
    Warning,
}

/// A button carried by a notice, with the shortcut its tooltip teaches.
#[derive(Clone)]
pub(super) struct NoticeAction {
    pub label: &'static str,
    pub cmd: Cmd,
    pub keys: &'static str,
}

/// One status-bar notice; a newer notice replaces it.
#[derive(Clone)]
pub(super) struct Flash {
    pub text: String,
    pub tone: Tone,
    pub action: Option<NoticeAction>,
    pub until: Instant,
}
impl Flash {
    pub(super) fn new(
        text: impl Into<String>,
        tone: Tone,
        action: Option<NoticeAction>,
        now: Instant,
    ) -> Self {
        let ttl = if action.is_some() {
            ACTION_TTL
        } else {
            NOTICE_TTL
        };
        Self {
            text: text.into(),
            tone,
            action,
            until: now + ttl,
        }
    }
}

/// What the previous frame now carries, shown briefly over the next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecisionNote {
    pub flag: Flag,
    pub summary: String,
    pub subject: String,
    pub until: Instant,
}

/// Quit was requested while decisions or recovered preferences cannot be
/// saved. The recovery journal makes quitting anyway safe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct QuitPrompt {
    pub decisions: usize,
    /// Recovered preferences wait for the settings file to be repaired.
    pub preferences: bool,
    /// The journal could not be written, so Quit Anyway is refused.
    pub error: Option<String>,
}
impl QuitPrompt {
    pub(super) fn title(&self) -> String {
        match (self.decisions, self.preferences) {
            (0, _) => "Quit with unsaved preferences?".into(),
            (n, _) => format!(
                "Quit with {}?",
                copy::plural(n, "unsaved decision", "unsaved decisions")
            ),
        }
    }
    pub(super) fn body(&self) -> String {
        let what = match (self.decisions, self.preferences) {
            (0, _) => "your recovered preferences".to_owned(),
            (1, false) => "this decision".to_owned(),
            (_, false) => "these decisions".to_owned(),
            (_, true) => "these decisions and your preferences".to_owned(),
        };
        let them = if self.decisions == 1 && !self.preferences {
            "it"
        } else {
            "them"
        };
        format!(
            "The catalog could not save {them}. Retry now, or quit and Shadowroom restores {what} from its recovery journal at next launch."
        )
    }
}

/// Every transient feedback state, kept together on the workspace.
#[derive(Default)]
pub(super) struct Feedback {
    pub flash: Option<Flash>,
    pub decision_note: Option<DecisionNote>,
    pub saved_until: Option<Instant>,
    pub quit_prompt: Option<QuitPrompt>,
    /// The error text whose raw details are expanded in the toast.
    pub details_for: Option<String>,
}
impl Feedback {
    /// Retires notices whose time is up; true when anything visible changed.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.flash.as_ref().is_some_and(|flash| now >= flash.until) {
            self.flash = None;
            changed = true;
        }
        if self
            .decision_note
            .as_ref()
            .is_some_and(|note| now >= note.until)
        {
            self.decision_note = None;
            changed = true;
        }
        if self.saved_until.is_some_and(|until| now >= until) {
            self.saved_until = None;
            changed = true;
        }
        changed
    }
}

impl Workspace {
    /// Shows `text` in the status bar for a few seconds (and mirrors it into
    /// the automation `status`).
    pub(super) fn announce(&mut self, text: impl Into<String>, tone: Tone) {
        let text = text.into();
        self.status.clone_from(&text);
        self.feedback.flash = Some(Flash::new(text, tone, None, Instant::now()));
    }
    /// [`Self::announce`] with a button, such as Undo after an auto-advance.
    pub(super) fn announce_action(
        &mut self,
        text: impl Into<String>,
        tone: Tone,
        label: &'static str,
        cmd: Cmd,
        keys: &'static str,
    ) {
        let text = text.into();
        self.status.clone_from(&text);
        self.feedback.flash = Some(Flash::new(
            text,
            tone,
            Some(NoticeAction { label, cmd, keys }),
            Instant::now(),
        ));
    }
    /// Confirms a decision whose frame just left the loupe (auto-advance, or
    /// the decision filtered it out of the view), on the photo and with Undo.
    pub(super) fn confirm_decision(
        &mut self,
        after: &[Decision],
        applied: (Option<u8>, Option<Flag>, Option<ColorLabel>),
    ) {
        let Some(first) = after.first() else {
            return;
        };
        let (flag, summary) = if after.len() == 1 {
            (
                first.flag,
                copy::decision_summary(first.flag, first.rating, first.label),
            )
        } else {
            let (rating, flag, label) = applied;
            let flag = flag.unwrap_or(Flag::None);
            (
                flag,
                copy::decision_summary(
                    flag,
                    rating.unwrap_or(0),
                    label.unwrap_or(ColorLabel::None),
                ),
            )
        };
        let filename = self
            .photo_index
            .get(&first.id)
            .map(|&i| self.photos[i].filename.as_str());
        let subject = copy::subject(after.len(), filename);
        self.feedback.decision_note = Some(DecisionNote {
            flag,
            summary: summary.clone(),
            subject: subject.clone(),
            until: Instant::now() + DECISION_NOTE_TTL,
        });
        self.announce_action(
            format!("{summary} · {subject}"),
            Tone::Info,
            "Undo",
            Cmd::Undo,
            "⌘Z",
        );
    }
    /// "Exported 4 files to Client-Picks" with a Reveal button.
    pub(super) fn announce_export(&mut self, job: &ExportJob) {
        let Some((destination, picks)) = job.target.clone() else {
            self.announce(
                format!(
                    "Export finished · {}",
                    plural(job.completed, "file", "files")
                ),
                Tone::Success,
            );
            return;
        };
        let folder = destination.file_name().map_or_else(
            || destination.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let text = if picks {
            format!(
                "Exported {} to {folder}",
                plural(job.completed, "file", "files")
            )
        } else {
            format!(
                "Wrote {} to {folder}",
                plural(job.completed, "XMP sidecar", "XMP sidecars")
            )
        };
        self.announce_action(
            text,
            Tone::Success,
            "Reveal",
            Cmd::RevealPath(destination),
            "",
        );
    }
    /// Names the photos an undo or redo touched.
    pub(super) fn decisions_subject(&self, decisions: &[Decision]) -> String {
        let filename = decisions
            .first()
            .and_then(|d| self.photo_index.get(&d.id))
            .map(|&i| self.photos[i].filename.as_str());
        copy::subject(decisions.len(), filename)
    }

    /// Status-bar notice in place of the workspace description.
    pub(super) fn status_notice(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.quit.closing() {
            return None;
        }
        let flash = self.feedback.flash.as_ref()?;
        let glyph = match flash.tone {
            Tone::Info => None,
            Tone::Success => Some(("check", self.c("success", "#7fbf7f"))),
            Tone::Warning => Some(("warning", self.c("warning", "#e0b341"))),
        };
        Some(
            div()
                .id("status-notice")
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .text_color(self.text())
                .children(glyph.map(|(glyph, color)| icon(glyph, color).size(px(14.))))
                .child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_normal()
                        .line_clamp(1)
                        .child(flash.text.clone()),
                )
                .children(flash.action.as_ref().map(|action| {
                    self.compact_button(
                        "status-notice-action",
                        action.label,
                        "",
                        action.cmd.clone(),
                        false,
                        cx,
                    )
                    .border_color(self.c("border.variant", "#333333"))
                    .tooltip(self.tip(action.label, action.keys))
                }))
                .into_any_element(),
        )
    }

    /// Decision durability only: preference writes stay silent unless they
    /// fail. Nothing shows when everything is saved.
    pub(super) fn save_state_view(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        if failed {
            let error = self.c("error", "#d46a6a");
            let hover = self.hover_bg();
            let unsaved = self.unsaved.len();
            let label = if unsaved > 0 {
                format!("{unsaved} unsaved · Retry")
            } else {
                "Not saved · Retry".into()
            };
            return Some(
                div()
                    .id("save-state")
                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                    .flex_shrink_0()
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(space::XS))
                    .text_color(error)
                    .cursor_pointer()
                    .occlude()
                    .hover(move |s| s.bg(hover))
                    .child(icon("warning", error).size(px(12.)))
                    .child(label)
                    .tooltip(self.tip("Retry saving decisions and preferences", ""))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.execute(Cmd::RetrySaves, window, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        let text = if self.awaiting > 0 || !self.outbox.is_empty() || self.recovery_pending {
            "Saving…"
        } else if self.feedback.saved_until.is_some() {
            "Saved"
        } else {
            return None;
        };
        Some(
            div()
                .id("save-state")
                .flex_shrink_0()
                .px(px(space::XS))
                .child(text)
                .into_any_element(),
        )
    }

    /// The current photo's decision over the image: flag, stars and label.
    pub(super) fn decision_chip(&self, photo: &Photo) -> Option<Div> {
        if photo.flag == Flag::None && photo.rating == 0 && photo.label == ColorLabel::None {
            return None;
        }
        Some(
            div()
                .photo_chip()
                .when(photo.flag != Flag::None, |s| {
                    s.child(self.flag_glyph(photo.flag))
                })
                .when(photo.rating > 0, |s| {
                    s.child(
                        div()
                            .text_color(self.rating_color())
                            .child(stars(photo.rating)),
                    )
                })
                .children(label_rgb(photo.label).map(|color| div().size(px(8.)).bg(rgb(color)))),
        )
    }
    /// The trailing "Pick · ★★★★★ · 02-lakeside.jpg" note after auto-advance.
    pub(super) fn decision_note_chip(&self) -> Option<Div> {
        let note = self.feedback.decision_note.as_ref()?;
        Some(
            div()
                .photo_chip()
                .when(note.flag != Flag::None, |s| {
                    s.child(self.flag_glyph(note.flag))
                })
                .child(note.summary.clone())
                .child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(rgb(SCRIM_MUTED))
                        .child(format!("· {}", note.subject)),
                ),
        )
    }
    fn flag_glyph(&self, flag: Flag) -> Svg {
        svg()
            .path(if flag == Flag::Pick {
                "pick-badge"
            } else {
                "reject-badge"
            })
            .size(px(12.))
            .flex_shrink_0()
            .text_color(if flag == Flag::Pick {
                self.pick_color()
            } else {
                self.reject_color()
            })
    }

    /// The reference slot of Compare when there is nothing to compare against.
    pub(super) fn compare_placeholder(&self, cx: &Context<Self>) -> AnyElement {
        let (title, body, button) = if self.filtered.len() < 2 {
            (
                "Compare needs two photos in this view",
                "Widen the filter, or select more photos before comparing.",
                self.button(
                    "compare-show-all",
                    "Show all photos",
                    "library",
                    Cmd::RatedAtLeast(0),
                    false,
                    cx,
                ),
            )
        } else {
            (
                "No reference pinned",
                "Pin the current photo as the reference, then move to a candidate.",
                self.button("compare-pin", "Pin reference", "pin", Cmd::Pin, false, cx)
                    .tooltip(self.tip("Pin reference", "B")),
            )
        };
        div()
            .id("reference-placeholder")
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::MD))
            .p(px(space::XL))
            .text_center()
            .border_1()
            .border_color(self.border())
            .bg(self.canvas_bg())
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().text_color(self.muted()).child(body))
            .child(
                button
                    .border_1()
                    .border_color(self.c("border.variant", "#333333")),
            )
            .into_any_element()
    }

    /// Where the error toast floats: centred over the centre column, above
    /// the decision bar and filmstrip so it never covers either.
    fn toast_insets(&self) -> (f32, f32, f32) {
        let mut bottom = ui::STATUS_HEIGHT + space::SM;
        if !self.import_ui.open && !self.filtered.is_empty() {
            bottom += DECISION_HEIGHT;
            if self.mode != ViewMode::Grid {
                bottom += FILMSTRIP_HEIGHT;
            }
        }
        (
            self.docks.library.max(0.),
            self.docks.inspector.max(0.),
            bottom,
        )
    }

    /// The error toast, painted above every overlay so a modal scrim never
    /// dims it or hides its buttons.
    pub(super) fn error_toast(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let notice = self.error.as_ref()?;
        let toast = copy::toast_copy(notice.kind, &notice.text, self.unsaved.len());
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let retry = failed
            && matches!(
                notice.kind,
                NoticeKind::UnsavedDecisions | NoticeKind::UnsavedPreferences
            );
        let expanded = toast.raw.is_some()
            && self.feedback.details_for.as_deref() == Some(notice.text.as_str());
        let error = self.c("error", "#d46a6a");
        let (left, right, bottom) = self.toast_insets();
        let hover = self.c("element.hover", "#303030");
        let details = toast.raw.as_ref().map(|_| {
            let text = notice.text.clone();
            div()
                .id("error-details")
                .h(px(ui::COMPACT_CONTROL_HEIGHT))
                .px(px(space::SM))
                .flex()
                .flex_shrink_0()
                .items_center()
                .cursor_pointer()
                .occlude()
                .text_color(self.muted())
                .hover(move |s| s.bg(hover))
                .child(if expanded { "Hide details" } else { "Details" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let open = this.feedback.details_for.as_deref() == Some(text.as_str());
                    this.feedback.details_for = (!open).then(|| text.clone());
                    cx.notify();
                }))
        });
        let body = div()
            .id("error-toast-text")
            .flex_1()
            .min_w(px(0.))
            .max_h(px(136.))
            .overflow_y_scroll()
            .pt(px(space::XS))
            .flex()
            .flex_col()
            .gap(px(space::TINY))
            .whitespace_normal()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(self.text())
                    .child(toast.title),
            )
            .children(toast.detail.map(|detail| {
                div()
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child(detail)
            }))
            .when(expanded, |s| {
                s.child(
                    div()
                        .mt(px(space::XS))
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(notice.text.clone()),
                )
            });
        let card = div()
            .id("error-toast")
            .occlude()
            .relative()
            .w(px(460.))
            .max_w_full()
            .max_h(px(160.))
            .overflow_hidden()
            .py(px(space::SM))
            .pl(px(space::MD))
            .pr(px(space::SM))
            .flex()
            .items_start()
            .gap(px(space::SM))
            .border_1()
            .border_color(self.c("border.variant", "#333333"))
            .bg(self.c("elevated_surface.background", "#1b1b1b"))
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
            }])
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(error),
            )
            .child(
                div()
                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .child(icon("warning", error)),
            )
            .child(body)
            .when(retry, |s| {
                s.child(
                    self.compact_button(
                        "toast-retry",
                        "Retry",
                        "refresh",
                        Cmd::RetrySaves,
                        false,
                        cx,
                    )
                    .border_color(self.c("border.variant", "#333333"))
                    .tooltip(self.tip("Retry saving", "")),
                )
            })
            .children(details)
            .child(
                self.compact_button("dismiss-error", "", "close", Cmd::DismissError, false, cx)
                    .tooltip(self.tip("Dismiss", "Esc")),
            );
        Some(
            div()
                .absolute()
                .left(px(left + space::LG))
                .right(px(right + space::LG))
                .bottom(px(bottom))
                .flex()
                .justify_center()
                .child(card)
                .into_any_element(),
        )
    }

    /// Ctrl+Q / Cmd+Q from the keyboard or the app menu. With the quit prompt
    /// already showing, a second press means Quit Anyway.
    pub(super) fn user_quit(&mut self, cx: &mut Context<Self>) {
        if self.feedback.quit_prompt.is_some() {
            self.quit_anyway(cx);
        } else {
            self.request_quit(cx);
        }
    }
    /// Replaces the dead-end "use Retry before closing" toast.
    pub(super) fn open_quit_prompt(&mut self, preferences: bool) {
        self.feedback.quit_prompt = Some(QuitPrompt {
            decisions: self.unsaved.len(),
            preferences,
            error: None,
        });
    }
    /// Closes the prompt once nothing blocks quitting any more.
    pub(super) fn reconcile_quit_prompt(&mut self) -> bool {
        let blocked = self.save_state() == SaveState::Failed
            || (self.recovery_settings.is_some() && !self.settings_writable);
        if self.feedback.quit_prompt.is_some() && !blocked {
            self.feedback.quit_prompt = None;
            return true;
        }
        false
    }
    /// The decisions and preferences that must survive a quit right now.
    pub(super) fn pending_journal(&self) -> Option<shadowroom::recovery::RecoveryJournal> {
        if self.unsaved.is_empty() && !self.prefs_sync.dirty() && !self.recovery_pending {
            return None;
        }
        let decisions = self
            .unsaved
            .values()
            .map(|d| PhotoDecision {
                id: d.id,
                rating: d.rating,
                flag: d.flag,
                label: d.label,
                trashed: d.trashed,
            })
            .collect();
        let settings = (self.prefs_sync.dirty() || self.recovery_settings.is_some())
            .then(|| self.settings.clone());
        Some(shadowroom::recovery::RecoveryJournal::new(
            decisions, settings,
        ))
    }
    /// Writes the recovery journal first and quits only if it is durable.
    fn quit_anyway(&mut self, cx: &mut Context<Self>) {
        self.flush_navigation();
        self.drain_settings();
        if let Some(journal) = self.pending_journal()
            && let Err(error) = journal.save(&self.recovery_path)
        {
            if let Some(prompt) = &mut self.feedback.quit_prompt {
                prompt.error = Some(format!(
                    "The recovery journal could not be written, so quitting now would lose these changes: {error:#}"
                ));
            }
            cx.notify();
            return;
        }
        self.feedback.quit_prompt = None;
        cx.quit();
    }
    fn quit_prompt_retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback.quit_prompt = None;
        self.execute(Cmd::RetrySaves, window, cx);
        // Keep the quit request: once the retried writes land, the drain loop
        // finishes closing; another failure reopens this prompt.
        self.quit = QuitState::Draining;
        self.status = "Retrying saves before closing…".into();
        cx.notify();
    }
    /// Keys while the prompt is up: Return retries, Escape cancels, and
    /// nothing reaches the views beneath it.
    pub(super) fn quit_prompt_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.feedback.quit_prompt.is_none() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                self.feedback.quit_prompt = None;
                cx.notify();
            }
            "enter" => self.quit_prompt_retry(window, cx),
            _ => {}
        }
        true
    }
    pub(super) fn quit_prompt_view(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let prompt = self.feedback.quit_prompt.as_ref()?;
        let border = self.c("border.variant", "#333333");
        let hover = self.c("element.hover", "#303030");
        let action = |id: &'static str, label: &'static str, keys: &'static str| {
            div()
                .id(id)
                .h(px(ui::CONTROL_HEIGHT))
                .px(px(space::MD))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .occlude()
                .border_1()
                .border_color(border)
                .hover(move |s| s.bg(hover))
                .child(label)
                .tooltip(self.tip(label, keys))
        };
        let cancel =
            action("quit-cancel", "Cancel", "Esc").on_click(cx.listener(|this, _, _, cx| {
                this.feedback.quit_prompt = None;
                cx.notify();
            }));
        let quit = action("quit-anyway", "Quit Anyway", "⌘Q")
            .when(prompt.error.is_some(), |s| s.opacity(0.5))
            .text_color(self.c("error", "#d46a6a"))
            .on_click(cx.listener(|this, _, _, cx| this.quit_anyway(cx)));
        let retry = action("quit-retry", "Retry", "Enter")
            .bg(self.selected_bg())
            .border_color(self.c("border.focused", "#5b9bd5"))
            .on_click(cx.listener(|this, _, window, cx| this.quit_prompt_retry(window, cx)));
        // Platform order: macOS puts the destructive choice apart on the left;
        // GNOME keeps Cancel on the left and the default on the right.
        let buttons = if cfg!(target_os = "macos") {
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(quit)
                .child(div().flex_1())
                .child(cancel)
                .child(retry)
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(cancel)
                .child(div().flex_1())
                .child(quit)
                .child(retry)
        };
        let frame = div()
            .id("quit-prompt")
            .w(px(480.))
            .max_w_full()
            .p(px(space::XL))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .border_1()
            .border_color(border)
            .bg(self.c("elevated_surface.background", "#1b1b1b"))
            .occlude()
            .child(div().text_size(px(16.)).child(prompt.title()))
            .child(
                div()
                    .text_color(self.muted())
                    .whitespace_normal()
                    .child(prompt.body()),
            )
            .children(prompt.error.as_ref().map(|error| {
                div()
                    .text_color(self.c("error", "#d46a6a"))
                    .whitespace_normal()
                    .child(error.clone())
            }))
            .child(div().pt(px(space::SM)).child(buttons));
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(space::LG))
                .bg(rgba(0x00000088))
                .child(frame)
                .into_any_element(),
        )
    }
}

/// The on-photo chip: scrim plate, 22 px tall, 11 px text, fixed light ink
/// because it sits on photographs rather than on themed chrome.
trait Chip {
    fn photo_chip(self) -> Self;
}
impl Chip for Div {
    fn photo_chip(self) -> Self {
        self.h(px(22.))
            .px(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .bg(rgba(SCRIM))
            .text_size(px(11.))
            .text_color(rgb(SCRIM_TEXT))
            .whitespace_nowrap()
            .overflow_hidden()
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent glob carries gpui::test, which shadows #[test].
    use super::{
        ACTION_TTL, Cmd, DECISION_NOTE_TTL, DecisionNote, Feedback, Flag, Flash, NOTICE_TTL,
        NoticeAction, QuitPrompt, SAVED_TTL, Tone,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn notices_with_actions_outlast_plain_ones() {
        let now = Instant::now();
        let plain = Flash::new("Focus peaking on", Tone::Info, None, now);
        let undo = Flash::new(
            "Pick · 02-lakeside.jpg",
            Tone::Info,
            Some(NoticeAction {
                label: "Undo",
                cmd: Cmd::Undo,
                keys: "⌘Z",
            }),
            now,
        );
        assert_eq!(plain.until - now, NOTICE_TTL);
        assert_eq!(undo.until - now, ACTION_TTL);
        assert!(ACTION_TTL > NOTICE_TTL);
    }

    #[test]
    fn feedback_expires_each_notice_on_its_own_clock() {
        let now = Instant::now();
        let mut feedback = Feedback {
            flash: Some(Flash::new("Exported 4 files", Tone::Success, None, now)),
            decision_note: Some(DecisionNote {
                flag: Flag::Pick,
                summary: "Pick".into(),
                subject: "a.jpg".into(),
                until: now + DECISION_NOTE_TTL,
            }),
            saved_until: Some(now + SAVED_TTL),
            ..Feedback::default()
        };
        assert!(!feedback.expire(now));
        assert!(feedback.expire(now + DECISION_NOTE_TTL));
        assert!(feedback.decision_note.is_none() && feedback.flash.is_some());
        assert!(feedback.expire(now + SAVED_TTL));
        assert!(feedback.saved_until.is_none() && feedback.flash.is_some());
        assert!(!feedback.expire(now + NOTICE_TTL - Duration::from_millis(1)));
        assert!(feedback.expire(now + NOTICE_TTL));
        assert!(feedback.flash.is_none());
        assert!(!feedback.expire(now + ACTION_TTL));
    }

    #[test]
    fn quit_prompt_counts_and_names_what_it_will_restore() {
        let one = QuitPrompt {
            decisions: 1,
            ..QuitPrompt::default()
        };
        assert_eq!(one.title(), "Quit with 1 unsaved decision?");
        assert!(one.body().contains("could not save it."));
        assert!(one.body().contains("restores this decision"));
        let many = QuitPrompt {
            decisions: 3,
            preferences: true,
            error: None,
        };
        assert_eq!(many.title(), "Quit with 3 unsaved decisions?");
        assert!(many.body().contains("these decisions and your preferences"));
        let prefs = QuitPrompt {
            preferences: true,
            ..QuitPrompt::default()
        };
        assert_eq!(prefs.title(), "Quit with unsaved preferences?");
        assert!(prefs.body().contains("recovered preferences"));
    }
}
