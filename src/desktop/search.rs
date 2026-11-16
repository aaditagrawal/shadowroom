//! Photo and folder results in the command palette (CAT-06): Ctrl/Cmd+K
//! finds commands, filenames and folders; Ctrl/Cmd+F opens it for photos only.
use super::*;

/// Rows per result group in the mixed palette.
const GROUP_ROWS: usize = 8;
/// Rows in photo-search mode (Ctrl/Cmd+F), which shows nothing else.
const PHOTO_MODE_ROWS: usize = 50;

/// ASCII case-insensitive substring test without allocating: filenames are
/// matched on every keystroke across the whole library.
fn contains_folded(haystack: &str, needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

fn starts_folded(haystack: &str, needle: &[u8]) -> bool {
    haystack
        .as_bytes()
        .get(..needle.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(needle))
}

impl Workspace {
    /// Opens the palette for photos only (Ctrl/Cmd+F). From the full palette
    /// it narrows to photos and keeps the query; pressing it again closes.
    pub(super) fn open_photo_search(&mut self) {
        if self.palette && self.palette_photos {
            self.palette = false;
        } else if !self.palette {
            self.getting_started = false;
            self.import_ui.menu = None;
            self.palette = true;
            self.palette_query.clear();
            self.palette_edit.focus("");
        }
        self.palette_index = 0;
        self.palette_photos = self.palette;
        self.palette_cache.borrow_mut().take();
    }

    /// Photo rows (filename substring, prefix matches first) then folder rows
    /// (name or path), each capped; in photo mode only photos.
    pub(super) fn search_items(&self, query: &str) -> Vec<PaletteItem> {
        let query = query.trim();
        let needle = query.as_bytes();
        // One character matches nearly every file: wait for a second one
        // unless the palette is searching photos only.
        if needle.is_empty() || (needle.len() < 2 && !self.palette_photos) {
            return Vec::new();
        }
        let limit = if self.palette_photos {
            PHOTO_MODE_ROWS
        } else {
            GROUP_ROWS
        };
        let mut photos: Vec<(bool, usize, usize)> = self
            .photos
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.trashed && contains_folded(&p.filename, needle))
            .map(|(i, p)| (!starts_folded(&p.filename, needle), p.filename.len(), i))
            .collect();
        let total = photos.len();
        photos.sort_unstable();
        let mut items: Vec<PaletteItem> = photos
            .into_iter()
            .take(limit)
            .map(|(_, _, i)| {
                let p = &self.photos[i];
                let folder = self.scope_display(&p.folder_key());
                item(
                    &p.filename,
                    &format!("Photo · {folder}"),
                    "",
                    "loupe",
                    Cmd::RevealPhoto(p.id),
                )
            })
            .collect();
        // The count of what the cap hides, on the last row.
        if total > limit
            && let Some(last) = items.last_mut()
        {
            last.detail = format!(
                "Photo · {} · keep typing",
                plural(total - limit, "more match", "more matches")
            );
        }
        if !self.palette_photos {
            let mut folders: Vec<(bool, &String)> = self
                .folders
                .iter()
                .map(|(path, _)| (path, self.scope_display(path)))
                .filter(|(path, name)| {
                    contains_folded(name, needle) || contains_folded(path, needle)
                })
                .map(|(path, name)| (!starts_folded(&name, needle), path))
                .collect();
            folders.sort();
            items.extend(folders.into_iter().take(GROUP_ROWS).map(|(_, path)| {
                let count = self
                    .folders
                    .iter()
                    .find(|(p, _)| p == path)
                    .map_or(0, |(_, n)| *n);
                item(
                    &self.scope_display(path),
                    &format!("Folder · {}", plural(count, "photo", "photos")),
                    "",
                    "folder",
                    Cmd::ScopeFolder(path.clone()),
                )
            }));
        }
        items
    }

    /// Selects a photo found by search: in the view when it is there,
    /// otherwise after showing the whole library. Catalog scrolls to it.
    pub(super) fn reveal_photo(&mut self, id: i64, cx: &mut Context<Self>) {
        let name = self
            .photo_index
            .get(&id)
            .map(|&i| self.photos[i].filename.clone());
        let Some(name) = name else {
            return;
        };
        let mut widened = false;
        if self.visible_position(id).is_none() {
            self.clear_filters();
            widened = true;
        }
        if self.visible_position(id).is_none() && self.review_subset.is_some() {
            self.review_subset = None;
            self.rebuild(Some(id));
        }
        let Some(position) = self.visible_position(id) else {
            self.announce(format!("{name} isn't in this view"), Tone::Warning);
            return;
        };
        self.navigation_direction = if position >= self.selected { 1 } else { -1 };
        self.select(position, false, false, cx);
        if self.mode == ViewMode::Grid {
            self.reveal_grid_row(position / self.grid_cols(), true);
        }
        if widened {
            self.announce(format!("Showing all photos to find {name}"), Tone::Info);
        }
    }

    /// Scopes the view to a folder, as its sidebar row does.
    pub(super) fn scope_folder(&mut self, scope: String) {
        self.scope = Some(scope);
        self.duplicates_only = false;
        self.label_filter = None;
        self.bursts_only = false;
        self.filter = Filter::All;
        self.settings.filter = "all".into();
        self.save_settings();
        self.rebuild(None);
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent glob carries gpui::test, which shadows #[test].
    use super::{contains_folded, starts_folded};

    #[test]
    fn filename_search_folds_ascii_case_without_allocating() {
        assert!(contains_folded("IMG_4512.CR3", b"4512"));
        assert!(contains_folded("06-Ridge copy.jpg", b"ridge"));
        assert!(!contains_folded("06-ridge.jpg", b"lake"));
        assert!(contains_folded("", b""));
        assert!(!contains_folded("ab", b"abc"));
        assert!(starts_folded("Demo Shoot", b"demo"));
        assert!(!starts_folded("06-ridge.jpg", b"ridge"));
    }
}
