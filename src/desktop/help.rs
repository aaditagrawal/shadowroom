use super::*;

impl Workspace {
    pub(super) fn getting_started_view(&self, cx: &Context<Self>) -> AnyElement {
        let mut content = div()
            .id("getting-started-content")
            .overflow_y_scroll()
            .flex_1()
            .min_h(px(0.))
            .p(px(space::XL))
            .flex()
            .flex_col()
            .gap(px(space::LG))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(self.muted())
                    .child("Scroll for the full guide ↓"),
            );
        for (title, text) in [
            (
                "1. Add or open photos",
                "Add photos plans a verified copy into your library and optional backups. Originals keep their bytes and metadata. Open an existing shoot folder to cull in place; the folder list lets you switch shoots.",
            ),
            (
                "2. Work on one set at a time",
                "In Catalog, click a frame, Ctrl/Cmd-click to add frames, or Shift-click / Shift+Arrow to select a range. Review and Compare use only that selection. G returns to Catalog; Show full folder leaves the subset.",
            ),
            (
                "3. Cull, then rate your keepers",
                "Use P to pick, X to reject and U to clear the flag. Keys 0–5 set stars; repeating a rating or flag keeps that decision. R resets stars and pick/reject together, keeps the color label and never auto-advances. Use Unreviewed for a first pass, then Picks for keeper ratings. Shift+A toggles auto-advance; Ctrl/Cmd+Z undoes a decision, including a reset.",
            ),
            (
                "4. Inspect fine detail",
                "Left/Right switches frames and preserves zoom and pan. Z or Space toggles Fit / 100%; + and − change magnification. Drag or scroll to pan; Ctrl/Cmd+scroll zooms. B pins a reference, C compares, Tab changes the active pane, and linked pans move together. F toggles cyan focus peaking, a detail aid rather than a focus guarantee.",
            ),
            (
                "Fast previews and caching",
                "Inspection prefers the paired JPEG or the camera's embedded JPEG. Neighboring frames are loaded ahead in your direction of travel, including while zoomed. Memory keeps decoded images ready; the disk cache reuses thumbnails and previews across launches and can show cached photos while a drive is offline. Automatic memory sizing adapts to this computer.",
            ),
            (
                "Time groups",
                "The import Gap value is the maximum time between consecutive captures in one time group. A larger gap combines more photos. A dash means some photos in that group or day are selected; a check means all are selected.",
            ),
            (
                "Color labels",
                "Labels are your own categories, not a universal meaning or rating standard. For example, red can mean retouch and green can mean delivery-ready. Keys 6–9 assign red, yellow, green and blue; the inspector also offers purple and clear. Export XMP explicitly when you want to carry decisions into another editor.",
            ),
            (
                "Your files and settings",
                "Decisions are saved in the local catalog and can be undone. In-app Trash hides whole photo groups and is restorable; it does not delete originals. Export Picks copies picked assets to a separate destination; XMP export writes only to an explicit destination. Preferences stay unchanged until Apply; Cancel or Escape discards the draft.",
            ),
        ] {
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
                    .id("getting-started")
                    .occlude()
                    .w(px(720.))
                    .max_w_full()
                    .h(px(
                        (self.workspace_size.1 - 2. * space::XL).clamp(200., 680.)
                    ))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .border_1()
                    .border_color(self.border())
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .child(
                        div()
                            .flex_shrink_0()
                            .p(px(space::LG))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(self.border())
                            .child(div().text_size(px(18.)).child("Getting Started"))
                            .child(self.button("close-guide", "", "close", Cmd::Close, false, cx)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .p(px(space::LG))
                            .flex()
                            .flex_col()
                            .gap(px(space::XS))
                            .border_b_1()
                            .border_color(self.border())
                            .child(div().font_weight(FontWeight::MEDIUM).child("Quick keys"))
                            .child("←/→ Navigate · P Pick · X Reject · U Clear flag · 0–5 Stars · R Reset")
                            .child("G Catalog · L Review · C Compare · S Survey · B Pin reference")
                            .child("Z Fit/100% · +/− Zoom · F Focus peaking · H Histogram · I Inspector")
                            .child(
                                div()
                                    .text_color(self.muted())
                                    .child("Ctrl/Cmd+Z Undo · Ctrl/Cmd+K Commands · Ctrl/Cmd+, Preferences · F1 Help"),
                            ),
                    )
                    .child(content)
                    .child(
                        div()
                            .flex_shrink_0()
                            .p(px(space::LG))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(space::SM))
                            .border_t_1()
                            .border_color(self.border())
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
                            .child(self.button(
                                "guide-done",
                                "Start culling",
                                "",
                                Cmd::Close,
                                false,
                                cx,
                            )),
                    ),
            )
            .into_any_element()
    }
}
