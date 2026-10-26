use super::*;
use std::time::Instant;

pub(super) struct Edit {
    cursor: usize,
    anchor: usize,
    epoch: Instant,
    pub visible: bool,
}
impl Default for Edit {
    fn default() -> Self {
        Self {
            cursor: 0,
            anchor: 0,
            epoch: Instant::now(),
            visible: true,
        }
    }
}
impl Edit {
    pub fn focus(&mut self, text: &str) {
        self.cursor = text.len();
        self.anchor = self.cursor;
        self.reset();
    }
    fn reset(&mut self) {
        self.epoch = Instant::now();
        self.visible = true;
    }
    pub fn blink(&mut self) -> bool {
        let visible = self.epoch.elapsed().as_millis() % 1000 < 500;
        let changed = visible != self.visible;
        self.visible = visible;
        changed
    }
    fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
    fn replace(&mut self, text: &mut String, insert: &str) {
        let range = self.range();
        self.cursor = range.start + insert.len();
        text.replace_range(range, insert);
        self.anchor = self.cursor;
    }
    pub fn key(&mut self, text: &mut String, event: &KeyDownEvent, cx: &mut App) -> bool {
        self.cursor = boundary(text, self.cursor);
        self.anchor = boundary(text, self.anchor);
        let before = text.clone();
        let m = event.keystroke.modifiers;
        let primary = m.control || m.platform;
        let key = event.keystroke.key.as_str();
        match key {
            "a" if primary => {
                self.anchor = 0;
                self.cursor = text.len();
            }
            "c" | "x" if primary => {
                let range = self.range();
                if !range.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text[range].into()));
                    if key == "x" {
                        self.replace(text, "");
                    }
                }
            }
            "v" if primary => {
                if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace(text, &value.replace(['\r', '\n'], ""));
                }
            }
            "left" | "right" | "home" | "end" => {
                self.cursor = match key {
                    "home" => 0,
                    "end" => text.len(),
                    "left" if !m.shift && !self.range().is_empty() => self.range().start,
                    "right" if !m.shift && !self.range().is_empty() => self.range().end,
                    "left" => previous(text, self.cursor),
                    _ => next(text, self.cursor),
                };
                if !m.shift {
                    self.anchor = self.cursor;
                }
            }
            "backspace" | "delete" => {
                if self.range().is_empty() {
                    self.anchor = if key == "backspace" {
                        previous(text, self.cursor)
                    } else {
                        next(text, self.cursor)
                    };
                }
                self.replace(text, "");
            }
            _ if !primary && !m.alt => {
                if let Some(value) = &event.keystroke.key_char {
                    if !value.chars().any(char::is_control) {
                        self.replace(text, value);
                    }
                }
            }
            _ => {}
        }
        self.reset();
        *text != before
    }
}
fn boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}
fn previous(text: &str, index: usize) -> usize {
    text[..index]
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i)
}
fn next(text: &str, index: usize) -> usize {
    text[index..]
        .chars()
        .next()
        .map_or(index, |c| index + c.len_utf8())
}

impl Workspace {
    pub(super) fn input_text(&self, text: &str, placeholder: &str, active: bool) -> AnyElement {
        let color = if text.is_empty() {
            self.muted()
        } else {
            self.text()
        };
        let shown: SharedString = if text.is_empty() { placeholder } else { text }
            .to_string()
            .into();
        let cursor = boundary(text, self.text_edit.cursor);
        let range = boundary(text, self.text_edit.anchor).min(cursor)
            ..boundary(text, self.text_edit.anchor).max(cursor);
        let caret = active && self.text_edit.visible;
        let accent = self.accent();
        canvas(
            move |bounds, window, _| {
                let style = window.text_style();
                let run = TextRun {
                    len: shown.len(),
                    font: style.font(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(
                    shown,
                    style.font_size.to_pixels(window.rem_size()),
                    &[run],
                    None,
                );
                let shift = if active {
                    (line.x_for_index(cursor) - bounds.size.width + px(3.)).max(px(0.))
                } else {
                    px(0.)
                };
                (line, shift)
            },
            move |bounds, (line, shift), window, cx| {
                // The field container's clip includes its padding and border.
                // Keep horizontally scrolled text and selection inside the
                // actual text area instead of painting over those edges.
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let origin = point(bounds.left() - shift, bounds.top());
                    if active && !range.is_empty() {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + line.x_for_index(range.start), bounds.top()),
                                size(
                                    line.x_for_index(range.end) - line.x_for_index(range.start),
                                    bounds.size.height,
                                ),
                            ),
                            accent.opacity(0.3),
                        ));
                    }
                    let _ = line.paint(origin, bounds.size.height, window, cx);
                    if caret {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + line.x_for_index(cursor), bounds.top()),
                                size(px(2.), bounds.size.height),
                            ),
                            accent,
                        ));
                    }
                });
            },
        )
        .h(px(ui::TEXT_LINE_HEIGHT))
        .w_full()
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Edit, boundary, next, previous};
    use std::time::{Duration, Instant};
    #[test]
    fn selection_replacement_and_unicode_positions_are_safe() {
        let mut text = "Aα📷Z".to_string();
        let mut edit = Edit::default();
        edit.focus(&text);
        assert_eq!(previous(&text, text.len()), 7);
        assert_eq!(next(&text, 3), 7);
        assert_eq!(boundary(&text, 5), 3);
        edit.anchor = 1;
        edit.cursor = 7;
        edit.replace(&mut text, "猫");
        assert_eq!(text, "A猫Z");
        assert_eq!(edit.cursor, 4);
        assert!(edit.range().is_empty());
    }

    #[test]
    fn focus_transfer_discards_the_old_selection_and_restarts_the_caret() {
        let mut edit = Edit::default();
        edit.focus("previous field");
        edit.anchor = 0;
        edit.visible = false;
        edit.focus("猫");
        assert_eq!(edit.cursor, 3);
        assert!(edit.range().is_empty());
        assert!(edit.visible);
        let mut replacement = "猫".to_string();
        edit.replace(&mut replacement, "A");
        assert_eq!(replacement, "猫A");
        edit.epoch = Instant::now() - Duration::from_millis(600);
        assert!(edit.blink());
        assert!(!edit.visible);
        edit.reset();
        assert!(edit.visible);
        assert!(!edit.blink());
    }
}
