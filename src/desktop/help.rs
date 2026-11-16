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
            "1. Add or open photos",
            format!(
                "Add Photos ({}) opens Import for a card or folder. Copy and verify the photos into your library and optional backups, or cull them in place first and import only what you keep. Originals keep their bytes and metadata; the folder list switches between shoots.",
                shortcut("⌘O")
            ),
        ),
        (
            "2. Work on one set at a time",
            format!(
                "In Catalog, Enter or L opens Review at the outlined photo; double-click opens the full folder at the photo under the pointer. Select multiple photos first to review only that set. {primary}-click adds frames; Shift-click / Shift+Arrow selects a range. G returns to Catalog; Show full folder leaves a subset."
            ),
        ),
        (
            "3. Cull, then rate your keepers",
            format!(
                "Use P to pick, X to reject and U to clear the flag. Keys 0–5 set stars; repeating a rating or flag keeps that decision. R resets stars and pick/reject together, keeps the color label and never auto-advances. Use Unreviewed for a first pass, then Picks for keeper ratings. The bottom bar shows your position (2 / 7) and the auto-advance toggle; Shift+A toggles it. {} undoes a decision, including a reset. A decision on 1,000 or more selected photos waits for Enter.",
                shortcut("⌘Z")
            ),
        ),
        (
            "4. Inspect fine detail",
            format!(
                "Left/Right switches frames and preserves zoom and pan. Click the photo, or press Z or Space with the pointer on it, to see 100% right there; click again or press Esc for Fit. - and = change magnification, and {primary}+scroll zooms around the pointer. Drag, scroll, {alt}+arrows or the navigator inset move the view; Home re-centres it. B pins a reference and C compares: in Compare, B keeps the better frame as the reference and brings in the next one, Shift+B swaps the panes, Tab changes the active pane and Shift+C links or unlinks their pans. F toggles cyan focus peaking, a detail aid rather than a focus guarantee."
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
            "Right-click an image for Show in folder, Show in review, Survey, Compare, Pick, Reject, Reset and deletion actions. Right-click a selected Catalog photo to keep the batch, or another photo to target it instead. Up/Down and Enter choose an action; Escape closes the menu.".into(),
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
                    self.execute(Cmd::OpenFolder, window, cx);
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
                                "Z/Space or click Fit/100% · -/= Zoom · {}+arrows Pan · F Peaking · H Histogram",
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
                                // An empty library's next step is adding photos.
                                self.dialog_button(
                                    "guide-done",
                                    "Open Folder…",
                                    ButtonKind::Primary,
                                    false,
                                )
                                .tooltip(self.tip("Open Folder… · cull in place", "Enter"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.execute(Cmd::Close, window, cx);
                                    this.execute(Cmd::OpenFolder, window, cx);
                                }))
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
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::guide_sections;

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
        // One platform's key names, never "Ctrl/Cmd" (OV-17).
        assert!(!text.contains("Ctrl/Cmd"));
        assert!(!text.contains("Alt/Option"));
        if cfg!(target_os = "macos") {
            assert!(!text.contains("Preferences"));
        }
    }
}
