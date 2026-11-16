//! Explicit, frozen deletion confirmations. No filesystem work runs while rendering.
use super::*;
use shadowroom::deletion::{DeletionMode, DeletionPlan, DeletionReport};

/// Deletion dialog lifecycle.
///
/// ```text
/// Idle ─request─▶ Preparing{token} ─plan─▶ Confirming ─confirm─▶ Executing ─report─▶ Idle
///                    │                       │    (in-app trash completes at once)
///                    └──────cancel / error───┴──▶ Idle
/// ```
///
/// Only one stage exists at a time, so a stale preparation (cancelled or replaced
/// by a new request) can never install a confirmation, a confirmation can never be
/// executed twice, and nothing may be requested while confirmed files are moving.
#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// The frozen plan is computed off the UI thread; only a result carrying this
    /// token may advance to `Confirming`.
    Preparing {
        token: u64,
    },
    Confirming(Box<Confirmation>),
    Executing,
}

#[derive(Default)]
pub(super) struct State {
    stage: Stage,
    confirm_focused: bool,
    pub report: Option<DeletionReport>,
}

struct Confirmation {
    action: Action,
    photos: usize,
    files: usize,
    sources: Vec<String>,
    destination: String,
    permanent: bool,
    preserved: Vec<PathBuf>,
    /// What is being deleted, frozen with the plan: photo ids in path order
    /// (for thumbnails), the file name of a single photo, and how many keepers
    /// (picks, or photos rated 3 stars and up) the batch contains.
    subject: Subject,
}

#[derive(Default)]
struct Subject {
    ids: Vec<i64>,
    name: Option<String>,
    picks: usize,
    rated: usize,
}
impl Subject {
    fn of(photos: &[Photo]) -> Self {
        Self {
            ids: photos.iter().map(|p| p.id).collect(),
            name: match photos {
                [photo] => photo
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                _ => None,
            },
            picks: photos.iter().filter(|p| p.flag == Flag::Pick).count(),
            rated: photos
                .iter()
                .filter(|p| p.flag != Flag::Pick && p.rating >= 3)
                .count(),
        }
    }
}

impl Confirmation {
    /// "Move “03-forest.jpg” to Trash?" / "Permanently delete 3 photos?"
    fn title(&self) -> String {
        let what = match &self.subject.name {
            Some(name) if self.photos == 1 => format!("“{name}”"),
            _ => plural(self.photos, "photo", "photos"),
        };
        if self.permanent {
            format!("Permanently delete {what}?")
        } else {
            format!("Move {what} to Trash?")
        }
    }

    fn confirm_label(&self) -> String {
        if self.permanent {
            format!(
                "Delete {} permanently",
                plural(self.photos, "photo", "photos")
            )
        } else {
            "Move to Trash".into()
        }
    }

    /// "3 files", or "3 photos · 5 files (RAW+JPEG pairs and sidecars)".
    fn files_line(&self) -> String {
        if self.files == self.photos {
            plural(self.files, "file", "files")
        } else {
            format!(
                "{} · {} (RAW+JPEG pairs and sidecars)",
                plural(self.photos, "photo", "photos"),
                plural(self.files, "file", "files")
            )
        }
    }

    /// "Includes 2 picks and 1 photo rated 3★ or higher", when any.
    fn keepers_warning(&self) -> Option<String> {
        let picks = (self.subject.picks > 0).then(|| plural(self.subject.picks, "pick", "picks"));
        let rated = (self.subject.rated > 0).then(|| {
            format!(
                "{} rated 3★ or higher",
                plural(self.subject.rated, "photo", "photos")
            )
        });
        match (picks, rated) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => Some(format!("Includes {one}")),
            (Some(picks), Some(rated)) => Some(format!("Includes {picks} and {rated}")),
        }
    }
}

enum Action {
    InApp(Vec<i64>),
    Files(DeletionPlan),
}

impl State {
    pub fn open(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    pub fn executing(&self) -> bool {
        matches!(self.stage, Stage::Executing)
    }

    fn confirmation(&self) -> Option<&Confirmation> {
        match &self.stage {
            Stage::Confirming(confirmation) => Some(confirmation.as_ref()),
            _ => None,
        }
    }

    /// Starts a new preparation, superseding any earlier one. Refused while
    /// confirmed files are being deleted.
    fn begin(&mut self) -> Option<u64> {
        if self.executing() {
            return None;
        }
        let token = rand::random();
        self.stage = Stage::Preparing { token };
        self.confirm_focused = false;
        self.report = None;
        Some(token)
    }

    /// Installs a prepared confirmation only if `token` is still the live request.
    fn prepared(&mut self, token: u64, confirmation: Confirmation) -> bool {
        if matches!(self.stage, Stage::Preparing { token: live } if live == token) {
            self.stage = Stage::Confirming(Box::new(confirmation));
            true
        } else {
            false
        }
    }

    /// Ends a preparation that failed; ignored if it was already superseded.
    fn preparation_failed(&mut self, token: u64) -> bool {
        if matches!(self.stage, Stage::Preparing { token: live } if live == token) {
            self.stage = Stage::Idle;
            true
        } else {
            false
        }
    }

    /// Takes the confirmation exactly once. File deletions move to `Executing`
    /// atomically so a second confirm (double Enter/click) finds nothing.
    fn take_confirmation(&mut self) -> Option<Confirmation> {
        match std::mem::take(&mut self.stage) {
            Stage::Confirming(confirmation) => {
                self.confirm_focused = false;
                if matches!(confirmation.action, Action::Files(_)) {
                    self.stage = Stage::Executing;
                }
                Some(*confirmation)
            }
            other => {
                self.stage = other;
                None
            }
        }
    }

    pub fn cancel(&mut self) {
        if !self.executing() {
            self.stage = Stage::Idle;
            self.confirm_focused = false;
        }
    }

    /// The IO actor answered (or never received) the confirmed deletion.
    fn finished(&mut self) {
        self.stage = Stage::Idle;
        self.confirm_focused = false;
    }

    pub fn snapshot(&self) -> Value {
        let confirmation = self.confirmation().map(|c| json!({
            "photo_count":c.photos,"file_count":c.files,"sources":c.sources,
            "destination":c.destination,"permanent":c.permanent,"preserved_shared_sidecars":c.preserved,
            "mode":match &c.action {Action::InApp(_)=>"in_app", Action::Files(p)=>match p.mode {DeletionMode::SystemTrash=>"system",DeletionMode::Permanent=>"permanent"}}
        }));
        json!({"open":self.open(),"executing":self.executing(),"confirmation":confirmation,"report":self.report})
    }
}

impl Workspace {
    fn deletion_ready(&self) -> std::result::Result<(), String> {
        if self.quit.closing() || self.recovery_pending || self.recovery_broken {
            return Err("Finish recovery and pending writes before deleting photos".into());
        }
        if self.busy
            || self.import_ui.active()
            || self.export_jobs.iter().any(|job| job.state.is_active())
        {
            return Err(
                "Wait for indexing, import and exports to finish before deleting photos".into(),
            );
        }
        if !self.unsaved.is_empty() || self.awaiting > 0 || !self.outbox.is_empty() {
            return Err(
                "Wait for decisions to save before deleting photos; retry any failed writes".into(),
            );
        }
        Ok(())
    }

    pub(super) fn request_deletion(&mut self, permanent: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.deletion_ready() {
            self.error = Some(error.into());
            return;
        }
        if self.deletion.executing() {
            self.error = Some("Wait for the confirmed deletion to finish".into());
            return;
        }
        let ids: Vec<_> = if self.multi.is_empty() {
            self.review_photo().map(|p| p.id).into_iter().collect()
        } else {
            self.multi.iter().copied().collect()
        };
        if ids.is_empty() {
            self.error = Some("Select photos before deleting".into());
            return;
        }
        let mut photos: Vec<_> = ids
            .iter()
            .filter_map(|id| self.photo_index.get(id).map(|&i| self.photos[i].clone()))
            .collect();
        photos.sort_by(|a, b| a.path.cmp(&b.path));
        let Some(token) = self.deletion.begin() else {
            return;
        };
        self.error = None;
        self.palette = false;
        self.image_context = None;
        let subject = Subject::of(&photos);
        if !permanent && self.settings.trash_mode == "in_app" {
            let mut sources: Vec<_> = photos.iter().map(|p| p.location_display()).collect();
            sources.sort();
            sources.dedup();
            self.deletion.prepared(
                token,
                Confirmation {
                    photos: photos.len(),
                    files: photos
                        .iter()
                        .map(|p| 1 + usize::from(p.companion_path.is_some()))
                        .sum(),
                    sources,
                    destination: "Shadowroom in-app Trash. Files stay in their original locations."
                        .into(),
                    action: Action::InApp(ids),
                    permanent: false,
                    preserved: Vec::new(),
                    subject,
                },
            );
            return;
        }
        let mode = if permanent {
            DeletionMode::Permanent
        } else {
            DeletionMode::SystemTrash
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { DeletionPlan::prepare(&photos, mode) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(plan) => {
                        this.deletion.prepared(
                            token,
                            Confirmation {
                                photos: plan.photo_count(),
                                files: plan.file_count(),
                                sources: plan
                                    .source_directories
                                    .iter()
                                    .map(|p| p.display().to_string())
                                    .collect(),
                                destination: plan.destination_description().to_owned(),
                                permanent,
                                preserved: plan.preserved_shared_sidecars.clone(),
                                action: Action::Files(plan),
                                subject,
                            },
                        );
                    }
                    Err(error) => {
                        if !this.deletion.preparation_failed(token) {
                            return;
                        }
                        this.error = Some(format!("Cannot prepare deletion: {error:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn confirm_deletion(&mut self, cx: &mut Context<Self>) {
        if self.deletion.confirmation().is_none() {
            return;
        }
        if let Err(error) = self.deletion_ready() {
            self.deletion.cancel();
            self.error = Some(error.into());
            return;
        }
        let Some(confirmation) = self.deletion.take_confirmation() else {
            return;
        };
        match confirmation.action {
            Action::InApp(ids) => {
                if let Err(error) = self.trash_photos(&ids, true) {
                    self.error = Some(error.into());
                } else {
                    self.announce_action(
                        format!(
                            "Moved {} to Trash · files stay in place",
                            plural(ids.len(), "photo", "photos")
                        ),
                        Tone::Success,
                        "Undo",
                        Cmd::Undo,
                        "⌘Z",
                    );
                }
            }
            Action::Files(plan) => {
                if self.enqueue(IoCommand::DeleteFiles(plan)) {
                    self.announce("Deleting confirmed files…", Tone::Info);
                } else {
                    self.deletion.finished();
                }
            }
        }
        cx.notify();
    }

    pub(super) fn deletion_finished(&mut self, report: DeletionReport) {
        let completed = report.completed_file_count();
        if report.is_success() {
            let files = plural(completed, "file", "files");
            self.announce(
                if report.mode == DeletionMode::Permanent {
                    format!("Permanently deleted {files}")
                } else {
                    format!("Moved {files} to the system Trash · restore from your file manager")
                },
                Tone::Success,
            );
        } else {
            let mut errors: Vec<_> = report
                .files
                .iter()
                .filter_map(|file| {
                    file.error
                        .as_ref()
                        .map(|error| format!("{}: {error}", file.path.display()))
                })
                .take(8)
                .collect();
            errors.extend(report.error.iter().cloned());
            errors.extend(report.catalog_error.iter().cloned());
            let failed = report.failed_file_count();
            let remaining = report.files.len().saturating_sub(completed + failed);
            self.error = Some(
                format!(
                    "Some files could not be deleted. {} done · {failed} failed · {remaining} not attempted · {}",
                    plural(completed, "file", "files"),
                    errors.join(" · ")
                )
                .into(),
            );
        }
        self.deletion.finished();
        // File deletion cannot be undone by restoring a decision snapshot.
        if completed > 0 {
            self.history = History::default();
        }
        self.deletion.report = Some(report);
    }

    pub(super) fn deletion_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.deletion.open() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.execute(Cmd::CancelDelete, window, cx),
            "tab" if self.deletion.confirmation().is_some() => {
                self.deletion.confirm_focused = !self.deletion.confirm_focused;
                cx.notify();
            }
            "enter" if self.deletion.confirmation().is_some() => {
                self.execute(
                    if self.deletion.confirm_focused {
                        Cmd::ConfirmDelete
                    } else {
                        Cmd::CancelDelete
                    },
                    window,
                    cx,
                );
            }
            _ => {}
        }
        true
    }

    /// Up to six thumbnails of the photos being deleted, with "+N" for the rest.
    fn deletion_thumbnails(&mut self, ids: &[i64], cx: &mut Context<Self>) -> Div {
        const SHOWN: usize = 6;
        let mut row = div().flex().items_center().gap(px(space::XS));
        for id in ids.iter().take(SHOWN) {
            let Some(&index) = self.photo_index.get(id) else {
                continue;
            };
            self.request_preview(&self.photos[index], PreviewKind::Thumbnail);
            let texture = self.texture(index, PreviewKind::Thumbnail, cx);
            let mut cell = div()
                .w(px(64.))
                .h(px(48.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .bg(self.canvas_bg());
            cell = match texture {
                Some((texture, ..)) => {
                    cell.child(img(texture).size_full().object_fit(ObjectFit::Contain))
                }
                None => cell.child(icon("camera", self.icon_muted())),
            };
            row = row.child(cell);
        }
        if ids.len() > SHOWN {
            row = row.child(
                div()
                    .px(px(space::SM))
                    .text_color(self.muted())
                    .child(format!("+{}", ids.len() - SHOWN)),
            );
        }
        row
    }

    pub(super) fn deletion_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ids = self
            .deletion
            .confirmation()
            .map(|c| c.subject.ids.clone())
            .unwrap_or_default();
        let thumbnails = (!ids.is_empty()).then(|| self.deletion_thumbnails(&ids, cx));
        let error = self.c("error", "#d46a6a");
        let warning = self.c("warning", "#e0b341");
        let surface = self.modal_surface("deletion-confirmation", dialog::DIALOG_MEDIUM);
        let surface = if let Some(c) = self.deletion.confirmation() {
            let focus_ring = |button: Stateful<Div>, focused: bool| {
                // Enter activates the selected button: its ring is always shown.
                button
                    .relative()
                    .when(focused, |s| s.child(self.focus_ring_overlay()))
            };
            let mut body = div()
                .id("deletion-body")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .p(px(ui::DIALOG_INSET))
                .flex()
                .flex_col()
                .gap(px(space::MD))
                .when_some(thumbnails, |s, row| s.child(row))
                .child(c.files_line())
                .when_some(c.keepers_warning(), |s, text| {
                    s.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .text_color(warning)
                            .child(icon("warning", warning).size(px(12.)))
                            .child(text),
                    )
                })
                .child(div().font_weight(FontWeight::MEDIUM).child("From"));
            let mut sources = div().flex().flex_col().gap(px(space::XS));
            for source in &c.sources {
                sources = sources.child(div().whitespace_normal().child(source.clone()));
            }
            if !c.preserved.is_empty() {
                sources = sources.child(div().text_color(self.muted()).child(format!(
                    "{} shared sidecars kept for unselected photos:",
                    c.preserved.len()
                )));
                for path in &c.preserved {
                    sources = sources.child(
                        div()
                            .text_color(self.muted())
                            .whitespace_normal()
                            .child(path.display().to_string()),
                    );
                }
            }
            body = body.child(sources);
            body = if c.permanent {
                // Irreversible: an error-tinted callout instead of a destination.
                body.child(
                    div()
                        .p(px(space::MD))
                        .border_1()
                        .border_color(error)
                        .bg(self.c("error.background", "#d46a6a1a"))
                        .text_color(self.text())
                        .whitespace_normal()
                        .child("This can't be undone. Files are removed from disk and bypass the Trash."),
                )
            } else {
                body.child(div().font_weight(FontWeight::MEDIUM).child("To"))
                    .child(c.destination.clone())
                    .child(div().text_color(self.muted()).child(
                        if matches!(c.action, Action::InApp(_)) {
                            "Only the catalog changes. Restore from in-app Trash or Undo."
                        } else {
                            "Restore through your operating system’s Trash. If trash is unavailable, the operation fails without permanent deletion."
                        },
                    ))
            };
            let confirm_focused = self.deletion.confirm_focused;
            surface
                .child(
                    self.modal_header(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .min_w(px(0.))
                            .when(c.permanent, |s| s.child(icon("warning", error)))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(c.title()),
                            ),
                        Some(("close-deletion", Cmd::CancelDelete)),
                        cx,
                    ),
                )
                .child(body)
                .child(
                    self.modal_footer()
                        // Cancel is the default; Enter names the ringed button,
                        // and Esc is listed once it no longer repeats Enter.
                        .child(self.key_hints(&if confirm_focused {
                            vec![
                                ("Tab", "Switch"),
                                ("Enter", if c.permanent { "Delete" } else { "Move" }),
                                ("Esc", "Cancel"),
                            ]
                        } else {
                            vec![("Tab", "Switch"), ("Enter", "Cancel")]
                        }))
                        .child(
                            div()
                                .flex()
                                .gap(px(space::SM))
                                .child(focus_ring(
                                    self.action_button(
                                        "cancel-deletion",
                                        "Cancel",
                                        Cmd::CancelDelete,
                                        ButtonKind::Secondary,
                                        false,
                                        cx,
                                    ),
                                    !confirm_focused,
                                ))
                                .child(focus_ring(
                                    self.action_button(
                                        "confirm-deletion",
                                        &c.confirm_label(),
                                        Cmd::ConfirmDelete,
                                        if c.permanent {
                                            ButtonKind::Destructive
                                        } else {
                                            ButtonKind::DestructiveQuiet
                                        },
                                        false,
                                        cx,
                                    ),
                                    confirm_focused,
                                )),
                        ),
                )
        } else {
            let executing = self.deletion.executing();
            surface
                .child(self.modal_header(
                    if executing {
                        "Deleting confirmed files…"
                    } else {
                        "Checking selected files…"
                    },
                    None,
                    cx,
                ))
                .when(!executing, |s| {
                    s.child(
                        self.modal_footer().justify_end().child(
                            self.action_button(
                                "cancel-deletion",
                                "Cancel",
                                Cmd::CancelDelete,
                                ButtonKind::Secondary,
                                false,
                                cx,
                            )
                            .relative()
                            .child(self.focus_ring_overlay()),
                        ),
                    )
                })
        };
        self.modal_scrim(Overlay::Deletion)
            .p(px(space::XL))
            .child(surface)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Confirmation, DeletionMode, DeletionPlan, Flag, Photo, State, Subject};

    fn in_app(ids: Vec<i64>) -> Confirmation {
        Confirmation {
            photos: ids.len(),
            files: ids.len(),
            sources: Vec::new(),
            destination: String::new(),
            permanent: false,
            preserved: Vec::new(),
            action: Action::InApp(ids),
            subject: Default::default(),
        }
    }

    fn files(dir: &std::path::Path) -> Confirmation {
        let path = dir.join("a.jpg");
        std::fs::write(&path, b"fixture").unwrap();
        let photo = Photo {
            id: 1,
            path,
            ..Default::default()
        };
        Confirmation {
            action: Action::Files(
                DeletionPlan::prepare(&[photo], DeletionMode::Permanent).unwrap(),
            ),
            ..in_app(Vec::new())
        }
    }

    #[test]
    fn confirmation_copy_names_the_subject_and_warns_about_keepers() {
        let photo = |id, name: &str, flag, rating| Photo {
            id,
            path: format!("/shoot/{name}").into(),
            flag,
            rating,
            ..Default::default()
        };
        let single = Confirmation {
            subject: Subject::of(&[photo(1, "03-forest.jpg", Flag::None, 0)]),
            ..in_app(vec![1])
        };
        assert_eq!(single.title(), "Move “03-forest.jpg” to Trash?");
        assert_eq!(single.files_line(), "1 file");
        assert_eq!(single.keepers_warning(), None);
        let batch = [
            photo(1, "a.jpg", Flag::Pick, 0),
            photo(2, "b.jpg", Flag::Pick, 5),
            photo(3, "c.jpg", Flag::None, 5),
        ];
        let mut many = Confirmation {
            subject: Subject::of(&batch),
            files: 5,
            ..in_app(vec![1, 2, 3])
        };
        assert_eq!(many.title(), "Move 3 photos to Trash?");
        assert_eq!(
            many.files_line(),
            "3 photos · 5 files (RAW+JPEG pairs and sidecars)"
        );
        assert_eq!(
            many.keepers_warning().as_deref(),
            Some("Includes 2 picks and 1 photo rated 3★ or higher")
        );
        many.permanent = true;
        assert_eq!(many.title(), "Permanently delete 3 photos?");
        assert_eq!(many.confirm_label(), "Delete 3 photos permanently");
        let one = Confirmation {
            permanent: true,
            ..in_app(vec![1])
        };
        assert_eq!(one.confirm_label(), "Delete 1 photo permanently");
    }

    #[test]
    fn superseded_or_cancelled_preparations_never_install_a_confirmation() {
        let mut state = State::default();
        let first = state.begin().unwrap();
        let second = state.begin().unwrap();
        assert!(!state.prepared(first, in_app(vec![1])));
        assert!(state.confirmation().is_none());
        assert!(!state.preparation_failed(first));
        assert!(state.open());
        state.cancel();
        assert!(!state.prepared(second, in_app(vec![2])));
        assert!(!state.open());
    }

    #[test]
    fn a_confirmation_executes_once_and_blocks_new_requests_until_finished() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let token = state.begin().unwrap();
        assert!(state.prepared(token, files(temp.path())));
        assert!(state.take_confirmation().is_some());
        assert!(state.executing());
        assert!(state.take_confirmation().is_none(), "double confirm");
        state.cancel();
        assert!(state.executing(), "cancel cannot abandon running deletion");
        assert!(state.begin().is_none());
        state.finished();
        assert!(!state.open());
        assert!(state.begin().is_some());
    }

    #[test]
    fn in_app_confirmation_closes_immediately_and_confirm_after_cancel_is_inert() {
        let mut state = State::default();
        let token = state.begin().unwrap();
        assert!(state.prepared(token, in_app(vec![7])));
        assert!(matches!(
            state.take_confirmation().map(|c| c.action),
            Some(Action::InApp(ids)) if ids == [7]
        ));
        assert!(!state.open());
        let token = state.begin().unwrap();
        assert!(state.prepared(token, in_app(vec![8])));
        state.cancel();
        assert!(state.take_confirmation().is_none());
    }
}
