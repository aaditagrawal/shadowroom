use super::*;

/// The platform's primary modifier as the guide spells it: "Ctrl" or "⌘".
fn primary() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl"
    }
}

/// The platform's Alt key name: "Alt" or "Option".
fn alt() -> &'static str {
    if cfg!(target_os = "macos") {
        "Option"
    } else {
        "Alt"
    }
}

/// One line of keyboard scrolling in the guide.
const GUIDE_LINE: f32 = 40.;

/// Guide sections: title and body, with platform-correct key names.
fn guide_sections() -> Vec<(&'static str, String)> {
    let (primary, alt, settings) = (primary(), alt(), settings_noun());
    vec![
        (
            "1. Open or import photos",
            format!(
                "{open} ({}) culls photos where they are; nothing is copied. {import} ({}) copies and verifies a card or folder into your library and optional backups; Import Picks from This Card lets you cull first and copy only what you keep. Originals keep their bytes and metadata; the folder list switches between shoots.",
                shortcut("⌘O"),
                shortcut("⌘⇧I"),
                open = copy::OPEN_FOLDER,
                import = copy::IMPORT_FROM_CARD,
            ),
        ),
        (
            "2. Work on one set at a time",
            format!(
                "In Catalog, Enter or L opens Review at the outlined photo; double-click opens the full folder at the photo under the pointer. Select multiple photos first to review only that set. {primary}-click adds frames; Shift-click / Shift+Arrow selects a range. G returns to Catalog; {} leaves a subset.",
                copy::SHOW_FULL_FOLDER
            ),
        ),
        (
            "3. Cull, then rate your keepers",
            format!(
                "Use P to pick, X to reject and U to clear the flag. Keys 0–5 set stars; repeating a rating or flag keeps that decision. R ({reset}) clears stars and pick/reject together, keeps the color label and never auto-advances. Use Unreviewed for a first pass, then Picks for keeper ratings. The bottom bar shows your position (2 / 7) and the auto-advance toggle; Shift+A toggles it. {} undoes a decision, including a reset. A decision on 1,000 or more selected photos waits for Enter.",
                shortcut("⌘Z"),
                reset = copy::RESET_DECISION,
            ),
        ),
        (
            "4. Inspect fine detail",
            format!(
                "Left/Right switches frames and preserves zoom and pan. Click the photo, or press Z or Space with the pointer on it, to see 100% right there; click again or press Esc for Fit. - and = change magnification, and {primary}+scroll zooms around the pointer. Drag, scroll, {alt}+arrows or the navigator inset move the view; Home re-centres it. B pins a reference and C compares: in Compare, B keeps the better frame as the reference and brings in the next one, Shift+B swaps the panes, Tab changes the active pane and Shift+C links or unlinks their pans. F toggles focus peaking (red by default; color and sensitivity in {settings} → Culling), a detail aid rather than a focus guarantee, and J shows clipped highlights in red and shadows in blue. {alt}+1, +2 and +4 jump to 100%, 200% and 400%; {alt}+0 returns to Fit."
            ),
        ),
        (
            "Fast previews and caching",
            "Inspection prefers the paired JPEG or the camera's embedded JPEG. Neighboring frames are loaded ahead in your direction of travel, including while zoomed. Memory keeps decoded images ready; the disk cache reuses thumbnails and previews across launches and can show cached photos while a drive is offline. Automatic memory sizing adapts to this computer.".into(),
        ),
        (
            "Time groups",
            "The import Gap value is the maximum time between consecutive captures in one time group. A larger gap combines more photos. A dash means some photos in that group or day are selected; a check means all are selected.".into(),
        ),
        (
            "Color labels",
            "Labels are your own categories, not a universal meaning or rating standard. For example, red can mean retouch and green can mean delivery-ready. Keys 6–9 assign red, yellow, green and blue; pressing the key of the label a photo already has clears it without moving on. The inspector also offers purple and clear. Export XMP explicitly when you want to carry decisions into another editor.".into(),
        ),
        (
            "Image actions",
            format!(
                "Right-click an image, or press Shift+F10 or the Menu key, for {} (outside Review), Survey, Compare, Pick, Reject, Rating, Label, {}, {}, then {} and {} at the bottom. Right-click a selected Catalog photo to keep the batch, or another photo to target it instead. Up/Down and Enter choose an action; Escape closes the menu.",
                copy::OPEN_IN_REVIEW,
                copy::RESET_DECISION,
                reveal_label(),
                copy::MOVE_TO_TRASH,
                copy::DELETE_PERMANENTLY,
            ),
        ),
        (
            "Delete deliberately",
            format!(
                "Delete defaults to OS Trash. {settings} → Culling → Trash destination also offers in-app Trash, which keeps files in place and supports Restore or Undo. Shift+Delete requests permanent deletion. Check the photo/file counts, source folders and destination in every confirmation. RAWs, companions and recognized sidecars are included. Restore OS-trashed files through your file manager. Trash failures never silently cause permanent deletion; partial failures are reported. Physical deletion is local-only and cannot be undone in Shadowroom."
            ),
        ),
        (
            "Your files and settings",
            format!(
                "Ratings, flags and labels are staged in the local catalog. Export Picks copies picked assets; XMP export writes only to an explicit destination. {settings} stay unchanged until you apply them: Apply saves and stays open; Apply & Close saves and closes. Cancel discards unapplied changes; Esc asks before discarding."
            ),
        ),
    ]
}

impl Workspace {
    /// Keys while the guide is on top (Escape, F1 and the palette are routed
    /// before this): arrows scroll a line, Page keys and Space a screen,
    /// Home/End jump, and Return runs the footer's primary button.
    pub(super) fn getting_started_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scroll = &self.guide_scroll;
        let viewport = f32::from(scroll.bounds().size.height).max(GUIDE_LINE);
        let maximum = f32::from(scroll.max_offset().height).max(0.);
        let top = -f32::from(scroll.offset().y);
        let shift = event.keystroke.modifiers.shift;
        let target = match event.keystroke.key.as_str() {
            "up" => top - GUIDE_LINE,
            "down" => top + GUIDE_LINE,
            "pageup" => top - viewport + GUIDE_LINE,
            "space" if shift => top - viewport + GUIDE_LINE,
            "pagedown" | "space" => top + viewport - GUIDE_LINE,
            "home" => 0.,
            "end" => maximum,
            "enter" => {
                self.execute(Cmd::Close, window, cx);
                if self.photos.is_empty() {
                    self.execute(Cmd::Import, window, cx);
                }
                return;
            }
            _ => return,
        };
        scroll.set_offset(point(px(0.), px(-target.clamp(0., maximum))));
        cx.notify();
    }

    pub(super) fn getting_started_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut content = div()
            .id("getting-started-content")
            .track_scroll(&self.guide_scroll)
            .overflow_y_scroll()
            .size_full()
            .p(px(space::XL))
            .flex()
            .flex_col()
            .gap(px(space::LG));
        for (title, text) in guide_sections() {
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap(px(space::XS))
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_color(self.muted()).child(text)),
            );
        }
        let content = div().flex_1().min_h(px(0.)).child(self.scrollable(
            "guide-scrollbar",
            &self.guide_scroll,
            content.into_any_element(),
            cx,
        ));
        self.modal_scrim(Overlay::GettingStarted)
            .p(px(space::XL))
            .child(
                self.modal_surface("getting-started", dialog::DIALOG_LARGE)
                    .h(px(
                        (self.workspace_size.1 - 2. * space::XL).clamp(200., 680.)
                    ))
                    .child(self.modal_header(
                        "Getting Started",
                        Some(("close-guide", Cmd::Close)),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(ui::DIALOG_INSET))
                            .py(px(space::LG))
                            .flex()
                            .flex_col()
                            .gap(px(space::XS))
                            .border_b_1()
                            .border_color(self.border())
                            .child(div().font_weight(FontWeight::MEDIUM).child("Quick keys"))
                            .child("←/→ Navigate · P Pick · X Reject · U Clear flag · 0–5 Stars · R Reset")
                            .child(format!(
                                "G Catalog · L Review · C Compare · S Survey · B Pin reference · {} Swap",
                                shortcut("⇧B")
                            ))
                            .child(format!(
                                "Z/Space or click Fit/100% · -/= Zoom · {}+arrows Pan · F Peaking · J Clipping · H Histogram",
                                alt()
                            ))
                            .child(
                                div()
                                    .text_color(self.muted())
                                    .child(format!(
                                        "{} Undo · {} Commands · {} {} · F1 Help",
                                        shortcut("⌘Z"),
                                        shortcut("⌘K"),
                                        shortcut("⌘,"),
                                        settings_noun()
                                    )),
                            ),
                    )
                    .child(content)
                    .child(
                        self.modal_footer()
                            .child(self.button(
                                "guide-startup",
                                "Show at startup",
                                if self.settings.show_getting_started {
                                    "selected"
                                } else {
                                    "select"
                                },
                                Cmd::StartupGuide(!self.settings.show_getting_started),
                                self.settings.show_getting_started,
                                cx,
                            ))
                            .child(if self.photos.is_empty() {
                                // An empty library's next step is adding photos:
                                // the empty state's two verbs, in its order and
                                // weight, so both answer "how do I start" alike.
                                div()
                                    .flex()
                                    .gap(px(space::SM))
                                    .child(
                                        self.dialog_button(
                                            "guide-open-folder",
                                            copy::OPEN_FOLDER,
                                            ButtonKind::Secondary,
                                            false,
                                        )
                                        .tooltip(self.tip(
                                            "Cull where the files are; nothing is copied",
                                            "⌘O",
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.execute(Cmd::Close, window, cx);
                                            this.execute(Cmd::OpenFolder, window, cx);
                                        })),
                                    )
                                    .child(
                                        self.dialog_button(
                                            "guide-done",
                                            copy::IMPORT_FROM_CARD,
                                            ButtonKind::Primary,
                                            false,
                                        )
                                        .tooltip(self.tip("Copy and verify into your library", "Enter"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.execute(Cmd::Close, window, cx);
                                            this.execute(Cmd::Import, window, cx);
                                        })),
                                    )
                                    .into_any_element()
                            } else {
                                self.action_button(
                                    "guide-done",
                                    "Start culling",
                                    Cmd::Close,
                                    ButtonKind::Primary,
                                    false,
                                    cx,
                                )
                                .tooltip(self.tip("Start culling", "Enter"))
                                .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Workspace, copy, guide_sections, reveal_label, shortcut};

    #[test]
    fn guide_copy_matches_current_behaviour() {
        let text: String = guide_sections()
            .into_iter()
            .map(|(title, body)| format!("{title} {body} "))
            .collect();
        // Escape asks before discarding preference edits (R2-overlays-05).
        assert!(!text.contains("Cancel or Escape discards"));
        assert!(text.contains("Esc asks before discarding"));
        // Enter follows the outline, not the pointer (R2-catalog-08).
        assert!(!text.contains("hovered or active"));
        assert!(!text.contains(" ;"));
        assert!(
            !text.contains("…."),
            "an ellipsis label must not end a sentence"
        );
        // One platform's key names, never "Ctrl/Cmd" (OV-17).
        assert!(!text.contains("Ctrl/Cmd"));
        assert!(!text.contains("Alt/Option"));
        if cfg!(target_os = "macos") {
            assert!(!text.contains("Preferences"));
        }
        // The two entry verbs, each with its own shortcut (R4-overlays-01).
        assert!(text.contains(&format!("{} ({})", copy::OPEN_FOLDER, shortcut("⌘O"))));
        assert!(text.contains(&format!("{} ({})", copy::IMPORT_FROM_CARD, shortcut("⌘⇧I"))));
        for stale in [
            "Add Photos",
            "Show in folder",
            "Show in review",
            "Reset Stars",
        ] {
            assert!(!text.contains(stale), "the guide still says {stale:?}");
        }
    }

    /// Every control the guide names exists under that exact name in the
    /// image menu or the command palette (R4-overlays-01, R4-catalog-01).
    #[test]
    fn guide_names_only_labels_that_exist() {
        let text: String = guide_sections()
            .into_iter()
            .map(|(title, body)| format!("{title} {body} "))
            .collect();
        let mut labels: Vec<String> = super::super::review_actions::image_action_labels()
            .into_iter()
            .map(str::to_owned)
            .collect();
        labels.extend(
            Workspace::command_items()
                .into_iter()
                .map(|item| item.title),
        );
        let known = |label: &str| labels.iter().any(|l| l == label);
        for quoted in [
            copy::OPEN_FOLDER,
            copy::IMPORT_FROM_CARD,
            "Import Picks from This Card",
            copy::SHOW_FULL_FOLDER,
            copy::RESET_DECISION,
            copy::OPEN_IN_REVIEW,
            copy::MOVE_TO_TRASH,
            copy::DELETE_PERMANENTLY,
            reveal_label(),
            "Survey",
            "Compare",
            "Rating",
            "Label",
        ] {
            assert!(
                text.contains(quoted),
                "the guide no longer names {quoted:?}"
            );
            assert!(
                known(quoted),
                "the guide names {quoted:?}, which no menu or command has"
            );
        }
        // Any "Title Case…" command in the guide is a real one: the last one
        // to four words before each ellipsis must form a known label.
        for (n, _) in text.match_indices('…') {
            let words: Vec<&str> = text[..n].split_whitespace().rev().take(4).collect();
            let found = (1..=words.len()).any(|count| {
                let phrase: Vec<&str> = words[..count].iter().rev().copied().collect();
                known(&format!("{}…", phrase.join(" ")))
            });
            assert!(found, "unknown command before an ellipsis: {:?}", words);
        }
    }
}
