//! Explicit, frozen deletion confirmations. No filesystem work runs while rendering.
use super::*;
use shadowroom::deletion::{DeletionMode, DeletionPlan, DeletionReport};

#[derive(Default)]
pub(super) struct State {
    token: Option<u64>,
    confirmation: Option<Confirmation>,
    executing: bool,
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
}

enum Action {
    InApp(Vec<i64>),
    Files(DeletionPlan),
}

impl State {
    pub fn open(&self) -> bool {
        self.token.is_some() || self.executing
    }

    pub fn executing(&self) -> bool {
        self.executing
    }

    pub fn cancel(&mut self) {
        if !self.executing {
            self.token = None;
            self.confirmation = None;
            self.confirm_focused = false;
        }
    }

    pub fn snapshot(&self) -> Value {
        let confirmation = self.confirmation.as_ref().map(|c| json!({
            "photo_count":c.photos,"file_count":c.files,"sources":c.sources,
            "destination":c.destination,"permanent":c.permanent,"preserved_shared_sidecars":c.preserved,
            "mode":match &c.action {Action::InApp(_)=>"in_app", Action::Files(p)=>match p.mode {DeletionMode::SystemTrash=>"system",DeletionMode::Permanent=>"permanent"}}
        }));
        json!({"open":self.open(),"executing":self.executing,"confirmation":confirmation,"report":self.report})
    }
}

impl Workspace {
    fn deletion_ready(&self) -> std::result::Result<(), String> {
        if self.quitting || self.recovery_pending || self.recovery_broken {
            return Err("Finish recovery and pending writes before deleting photos".into());
        }
        if self.busy
            || self.import_ui.active()
            || self
                .export_jobs
                .iter()
                .any(|job| matches!(job["state"].as_str(), Some("queued" | "running")))
        {
            return Err(
                "Wait for indexing, import and exports to finish before deleting photos".into(),
            );
        }
        if !self.unsaved.is_empty() || !self.awaiting.is_empty() || !self.outbox.is_empty() {
            return Err(
                "Wait for decisions to save before deleting photos; retry any failed writes".into(),
            );
        }
        Ok(())
    }

    pub(super) fn request_deletion(&mut self, permanent: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.deletion_ready() {
            self.error = Some(error);
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
        self.deletion.report = None;
        self.error = None;
        self.palette = false;
        self.image_context = None;
        let token = rand::random();
        self.deletion.token = Some(token);
        self.deletion.confirm_focused = false;
        if !permanent && self.settings.trash_mode == "in_app" {
            let mut sources: Vec<_> = photos.iter().map(|p| p.location_display()).collect();
            sources.sort();
            sources.dedup();
            self.deletion.confirmation = Some(Confirmation {
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
            });
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
                if this.deletion.token != Some(token) {
                    return;
                }
                match result {
                    Ok(plan) => {
                        this.deletion.confirmation = Some(Confirmation {
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
                        });
                    }
                    Err(error) => {
                        this.deletion.cancel();
                        this.error = Some(format!("Cannot prepare deletion: {error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn confirm_deletion(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.deletion_ready() {
            self.deletion.cancel();
            self.error = Some(error);
            return;
        }
        let Some(confirmation) = self.deletion.confirmation.take() else {
            return;
        };
        match confirmation.action {
            Action::InApp(ids) => {
                self.deletion.cancel();
                if let Err(error) = self.trash_photos(&ids, true) {
                    self.error = Some(error);
                } else {
                    self.status = format!(
                        "{} photos hidden in in-app Trash; files remain in place",
                        ids.len()
                    );
                }
            }
            Action::Files(plan) => {
                if self.enqueue(IoCommand::DeleteFiles(plan)) {
                    self.deletion.executing = true;
                    self.status = "Deleting confirmed files…".into();
                } else {
                    self.deletion.cancel();
                }
            }
        }
        cx.notify();
    }

    pub(super) fn deletion_finished(&mut self, report: DeletionReport) {
        let completed = report.completed_file_count();
        if report.is_success() {
            self.status = format!(
                "{completed} files {}",
                if report.mode == DeletionMode::Permanent {
                    "permanently deleted"
                } else {
                    "moved to OS trash; restore using your file manager"
                }
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
            self.error = Some(format!(
                "{completed} files completed; {failed} failed; {remaining} not attempted. {}",
                errors.join(" · ")
            ));
        }
        self.deletion.executing = false;
        self.deletion.cancel();
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
            "tab" if self.deletion.confirmation.is_some() => {
                self.deletion.confirm_focused = !self.deletion.confirm_focused;
                cx.notify();
            }
            "enter" if self.deletion.confirmation.is_some() => {
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

    pub(super) fn deletion_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut content = div().flex().flex_col().gap(px(space::MD));
        if let Some(c) = &self.deletion.confirmation {
            content = content
                .child(div().text_size(px(18.)).child(if c.permanent {
                    "Permanently delete selected photos?"
                } else {
                    "Move selected photos to Trash?"
                }))
                .child(format!(
                    "{} {} · {} {}{}",
                    c.photos,
                    if c.photos == 1 { "photo" } else { "photos" },
                    c.files,
                    if c.files == 1 { "file" } else { "files" },
                    if matches!(c.action, Action::InApp(_)) {
                        " kept in place"
                    } else {
                        " including paired images and sidecars"
                    }
                ))
                .child(div().font_weight(FontWeight::MEDIUM).child("From"));
            let mut sources = div()
                .id("deletion-sources")
                .max_h(px(180.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(space::XS));
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
            content=content.child(sources).child(div().font_weight(FontWeight::MEDIUM).child("To")).child(c.destination.clone())
                .child(div().text_color(self.muted()).child(if c.permanent {"This permanently removes files from disk. Shadowroom cannot undo this."} else if matches!(c.action,Action::InApp(_)) {"Only the catalog changes. Restore from in-app Trash or Undo."} else {"Restore through your operating system’s Trash. If trash is unavailable, the operation fails without permanent deletion."}))
                .child(div().text_size(px(11.)).text_color(self.muted()).child("Cancel is selected by default. Tab switches buttons; Enter activates the selected button."))
                .child(div().flex().justify_end().gap(px(space::SM))
                    .child(self.button("cancel-deletion","Cancel","",Cmd::CancelDelete,!self.deletion.confirm_focused,cx))
                    .child(self.button("confirm-deletion",if c.permanent {"Delete permanently"} else {"Move to Trash"},"trash",Cmd::ConfirmDelete,self.deletion.confirm_focused,cx)));
        } else {
            content = content.child(if self.deletion.executing {
                "Deleting confirmed files…"
            } else {
                "Checking selected files…"
            });
            if !self.deletion.executing {
                content = content.child(self.button(
                    "cancel-deletion",
                    "Cancel",
                    "",
                    Cmd::CancelDelete,
                    true,
                    cx,
                ));
            }
        }
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000088))
            .flex()
            .items_center()
            .justify_center()
            .p(px(space::XL))
            .child(
                div()
                    .id("deletion-confirmation")
                    .occlude()
                    .w(px(620.))
                    .max_w_full()
                    .max_h_full()
                    .overflow_y_scroll()
                    .p(px(space::XL))
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .border_1()
                    .border_color(self.border())
                    .child(content),
            )
            .into_any_element()
    }
}
