mod appearance;
mod copy;
mod culling;
mod deletion_ui;
mod dialog;
mod feedback;
mod folder_picker;
mod frame_trace;
mod help;
mod ingest_ui;
mod layout;
mod ledger;
mod loupe_aids;
mod motion;
mod panes;
mod platform_prefs;
mod preferences;
mod prefetch;
mod review_actions;
mod scrollbar;
mod search;
mod text_input;
use anyhow::{Context as _, Result};
use copy::plural;
use crossbeam_channel::{Receiver, Sender};
use dialog::{ButtonKind, FocusScope, FocusTarget};
use feedback::Tone;
use gpui::{prelude::*, *};
use layout as ui;
use layout::DockSide;
use layout::{
    DECISION_HEIGHT, FILMSTRIP_HEIGHT, INFO_WIDTH, LIBRARY_WIDTH, TITLE_HEIGHT, TOOLBAR_HEIGHT,
    space,
};
use serde_json::{Value, json};
use shadowroom::network::HostState;
use shadowroom::{
    automation::{AutomationRequest, AutomationServer},
    duplicates::{ScanPhase, ScanStage},
    indexer::Drive,
    ingest::Phase,
    media::{ColorStatus, MediaPipeline, PreviewKind},
    model::{ColorLabel, Flag, Photo, PhotoDecision},
    service::{GroupSlot, IoCommand, IoEvent, JobState, PhotoSlot, start_io},
    settings::{INTERFACE_FONTS, Settings},
    theme::Theme,
    theme_watch::{ThemeUpdate, ThemeWatcher, ZedSelection, start_theme_watch},
    workspace::{Decision, Filter, History, SaveRevisions, fuzzy_score},
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::{Duration, Instant},
};
actions!(shadowroom, [QuitApp]);

struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "folder" => Some(include_bytes!("../assets/icons/folder.svg")),
            "folder-opened" => Some(include_bytes!("../assets/icons/folder-opened.svg")),
            "camera" => Some(include_bytes!("../assets/icons/device-camera.svg")),
            "search" => Some(include_bytes!("../assets/icons/search.svg")),
            "settings" => Some(include_bytes!("../assets/icons/gear.svg")),
            "filter" => Some(include_bytes!("../assets/icons/list-filter.svg")),
            "chevron" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
            "down" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "check" => Some(include_bytes!("../assets/icons/check.svg")),
            "close" => Some(include_bytes!("../assets/icons/close.svg")),
            "compare" => Some(include_bytes!(
                "../assets/icons/shadowroom/view-compare.svg"
            )),
            "zoom" => Some(include_bytes!("../assets/icons/zoom-in.svg")),
            "zoom-out" => Some(include_bytes!("../assets/icons/shadowroom/zoom-out.svg")),
            "graph" => Some(include_bytes!("../assets/icons/shadowroom/histogram.svg")),
            "histogram" => Some(include_bytes!("../assets/icons/shadowroom/histogram.svg")),
            "undo" => Some(include_bytes!("../assets/icons/discard.svg")),
            "redo" => Some(include_bytes!("../assets/icons/redo.svg")),
            "next" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
            "previous" => Some(include_bytes!("../assets/icons/arrow-left.svg")),
            "auto-advance" => Some(include_bytes!(
                "../assets/icons/shadowroom/auto-advance.svg"
            )),
            "star" => Some(include_bytes!("../assets/icons/star-empty.svg")),
            "star-full" => Some(include_bytes!("../assets/icons/star-full.svg")),
            "pick" => Some(include_bytes!("../assets/icons/pass.svg")),
            "pick-on" => Some(include_bytes!("../assets/icons/pass-filled.svg")),
            "reject" => Some(include_bytes!("../assets/icons/circle-slash.svg")),
            "reject-on" => Some(include_bytes!(
                "../assets/icons/shadowroom/reject-filled.svg"
            )),
            "unflagged" => Some(include_bytes!("../assets/icons/circle-large.svg")),
            "pick-badge" => Some(include_bytes!("../assets/icons/pass-filled-compact.svg")),
            "reject-badge" => Some(include_bytes!(
                "../assets/icons/shadowroom/reject-filled-compact.svg"
            )),
            "group-badge" => Some(include_bytes!(
                "../assets/icons/shadowroom/layers-compact.svg"
            )),
            "library" => Some(include_bytes!("../assets/icons/library.svg")),
            "files" => Some(include_bytes!("../assets/icons/file-code.svg")),
            "xmp" => Some(include_bytes!("../assets/icons/file-code.svg")),
            "export" => Some(include_bytes!("../assets/icons/export.svg")),
            "import" => Some(include_bytes!("../assets/icons/shadowroom/import.svg")),
            "grid" => Some(include_bytes!("../assets/icons/shadowroom/view-grid.svg")),
            "group" => Some(include_bytes!("../assets/icons/layers.svg")),
            "duplicates" => Some(include_bytes!("../assets/icons/shadowroom/duplicate.svg")),
            "trash" => Some(include_bytes!("../assets/icons/trash.svg")),
            "restore" => Some(include_bytes!(
                "../assets/icons/shadowroom/trash-restore.svg"
            )),
            "drive" => Some(include_bytes!("../assets/icons/shadowroom/drive.svg")),
            "select" => Some(include_bytes!("../assets/icons/shadowroom/checkbox.svg")),
            "selected" => Some(include_bytes!(
                "../assets/icons/shadowroom/checkbox-checked.svg"
            )),
            "partial" => Some(include_bytes!(
                "../assets/icons/shadowroom/checkbox-partial.svg"
            )),
            "pin" => Some(include_bytes!("../assets/icons/pinned.svg")),
            "sync-pan" => Some(include_bytes!("../assets/icons/link.svg")),
            "add" => Some(include_bytes!("../assets/icons/add.svg")),
            "external" => Some(include_bytes!("../assets/icons/link-external.svg")),
            "eject" => Some(include_bytes!("../assets/icons/shadowroom/eject.svg")),
            "focus" => Some(include_bytes!("../assets/icons/screen-full.svg")),
            "peaking" => Some(include_bytes!("../assets/icons/shadowroom/peaking.svg")),
            "reset" => Some(include_bytes!(
                "../assets/icons/shadowroom/reset-decision.svg"
            )),
            "loupe" => Some(include_bytes!("../assets/icons/shadowroom/view-loupe.svg")),
            "save" => Some(include_bytes!("../assets/icons/save.svg")),
            "refresh" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "stop" => Some(include_bytes!("../assets/icons/stop-circle.svg")),
            "warning" => Some(include_bytes!("../assets/icons/warning.svg")),
            "symbol-color" => Some(include_bytes!("../assets/icons/symbol-color.svg")),
            "color-mode" => Some(include_bytes!("../assets/icons/color-mode.svg")),
            "panel-left" => Some(include_bytes!("../assets/icons/shadowroom/panel-left.svg")),
            "panel-right" => Some(include_bytes!("../assets/icons/shadowroom/panel-right.svg")),
            "win-min" => Some(include_bytes!(
                "../assets/icons/shadowroom/window-minimize.svg"
            )),
            "win-max" => Some(include_bytes!(
                "../assets/icons/shadowroom/window-maximize.svg"
            )),
            "win-restore" => Some(include_bytes!(
                "../assets/icons/shadowroom/window-restore.svg"
            )),
            "win-close" => Some(include_bytes!(
                "../assets/icons/shadowroom/window-close.svg"
            )),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }
    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Grid,
    Loupe,
    Compare,
    /// Every frame of the current burst side by side.
    Survey,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum PreferencesSection {
    #[default]
    Appearance,
    Culling,
    Performance,
    Import,
    Controller,
    Help,
}
#[derive(Clone)]
enum Cmd {
    Ingest(String),
    EjectDrive(PathBuf),
    /// "Import from Card…": copy and verify into the library (Import view).
    Import,
    /// "Open Folder…": index a folder where it is and cull it; nothing is copied.
    OpenFolder,
    AddRemoteHost,
    SaveRemoteHost,
    RemoteAction(String, String, Option<String>),
    ExportPicks,
    /// Every pick in the catalog, not just the current view (palette only).
    ExportAllPicks,
    ExportXmp,
    Palette,
    /// Ctrl/Cmd+F: the palette for photo filenames only.
    SearchPhotos,
    /// A palette photo result: select it (widening the view if needed).
    RevealPhoto(i64),
    /// A palette folder result: scope the view to it.
    ScopeFolder(String),
    Preferences,
    ApplyPreferences,
    ApplyClosePreferences,
    /// Cancel: drop the Preferences draft at once.
    DiscardPreferences,
    /// Dismiss the "Discard changes?" prompt and return to the draft.
    KeepEditingPreferences,
    Appearance,
    PreferencesSection(PreferencesSection),
    Close,
    /// Clear the error toast only; unlike Close it never closes a view.
    DismissError,
    GettingStarted,
    StartupGuide(bool),
    Next,
    Previous,
    Rating(u8),
    RatingDelta(i8),
    Flag(Flag),
    ResetDecision,
    Label(ColorLabel),
    Undo,
    Redo,
    View(ViewMode),
    ReviewSelection,
    Zoom,
    ZoomTo(f32),
    ZoomStep(i8),
    FocusPeaking,
    /// J: paint blown highlights and crushed shadows on the photo.
    Clipping,
    /// Preferences → Culling: focus-peaking ink and edge threshold.
    PeakingColor(shadowroom::peaking::PeakingColor),
    PeakingSensitivity(shadowroom::peaking::PeakingSensitivity),
    ClearReviewSubset,
    Inspector,
    InspectorTab(bool),
    RatingFilter,
    Histogram,
    Focus,
    Sidebar,
    ResizeDock(DockSide),
    ResetLayout,
    Pin,
    NextGroup,
    PreviousGroup,
    Group(String),
    Filter(Filter),
    SetMinRating(u8),
    AutoAdvance,
    SetAutoAdvance(bool),
    ReduceMotion(bool),
    /// Text size in percent; 0 follows the desktop's text scaling.
    TextSize(u32),
    SetAutoDrives(bool),
    Font(String),
    Theme(String),
    SystemTheme(String),
    ImportTheme,
    Refresh,
    Reveal,
    /// Show a finished import or export destination in the file manager.
    RevealPath(PathBuf),
    AllSelect,
    Gap(u64),
    Threshold(u32),
    Controls,
    RetrySaves,
    RetryPreview,
    Quit,
    CancelImport,
    HistogramLog,
    /// Segmented LIN/LOG control: select a scale; never toggles.
    SetHistogramLog(bool),
    Memory(usize),
    Disk(usize),
    ThemeMode(String),
    CompareSync,
    Unreviewed(isize),
    ResolveBurst,
    Trash(bool),
    PermanentDelete,
    ConfirmDelete,
    CancelDelete,
    TrashMode(String),
    /// Show the operating system's Trash in the file manager.
    OpenSystemTrash,
    ScanDuplicates,
    CancelDuplicates,
    ShowDuplicates,
    PreviewDetails,
    LabelFilter(ColorLabel),
    BurstsOnly,
    RatedAtLeast(u8),
    /// Catalog thumbnail size S / M / L (0–2), and a step through them.
    GridSize(usize),
    GridSizeStep(i8),
    /// Compare: exchange reference and candidate.
    SwapPanes,
    /// Z / Space: 100% at the pointer, or back to Fit.
    ZoomAtPointer,
    /// Pan a zoomed photo by a third of the view (x, y).
    PanStep(i8, i8),
    ApplyBulk,
    CancelBulk,
    ClearScope,
    /// Whole library: no scope, collection, label, rating or burst filter.
    ClearFilters,
}
struct PaletteItem {
    title: String,
    detail: String,
    shortcut: String,
    icon: &'static str,
    command: Cmd,
    search: String,
}

#[derive(Default)]
struct PaletteCache {
    /// Commands first (`command_count` of them), then this query's photo and
    /// folder results.
    items: Vec<PaletteItem>,
    command_count: usize,
    matches: Vec<usize>,
    query: Option<String>,
    grouping: String,
    availability: [bool; 11],
    revision: u64,
}

impl PaletteCache {
    fn match_query(&mut self, query: &str, recent: &[String]) {
        if self.query.as_deref() == Some(query) {
            return;
        }
        let mut scored: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(query, &item.search).map(|score| (score, i)))
            .collect();
        if query.is_empty() {
            // Recent commands first, then culling before housekeeping.
            scored.sort_by_key(|&(_, i)| {
                let item = &self.items[i];
                (
                    recent
                        .iter()
                        .position(|title| *title == item.title)
                        .unwrap_or(usize::MAX),
                    palette_category_rank(&item.detail),
                )
            });
        } else {
            // Titles that contain the query, then titles that match it
            // loosely, then rows matched only through their description.
            scored.sort_by_cached_key(|&(score, i)| {
                (match_tier(query, &self.items[i].title), -score)
            });
        }
        self.matches = scored.into_iter().map(|(_, i)| i).collect();
        self.query = Some(query.into());
    }
    /// Adds photo and folder results after the commands whose title contains
    /// the query; photo mode shows only them. Beside real results the palette
    /// stays precise (R4-overlays-04): at most [`PALETTE_LOOSE`] commands whose
    /// title matches loosely follow, and description-only matches are dropped.
    fn merge_found(&mut self, found: Vec<PaletteItem>, query: &str, photos_only: bool) {
        let start = self.items.len();
        let found_rows = start..start + found.len();
        self.items.extend(found);
        if photos_only {
            self.matches = found_rows.collect();
            return;
        }
        if found_rows.is_empty() {
            return;
        }
        let tier = |i: usize| match_tier(query, &self.items[i].title);
        let exact = self.matches.iter().copied().filter(|&i| tier(i) == 0);
        let loose = self.matches.iter().copied().filter(|&i| tier(i) == 1);
        self.matches = exact
            .chain(found_rows)
            .chain(loose.take(PALETTE_LOOSE))
            .collect();
    }
}

/// Rows the palette shows at once; PageUp/PageDown move by this many.
const PALETTE_PAGE: usize = 12;
/// The palette's right-hand category column ("Appearance" fits), at 100% text.
const PALETTE_CATEGORY_WIDTH: f32 = 80.;
/// Loose command matches kept beside photo and folder results.
const PALETTE_LOOSE: usize = 3;

/// How a palette title answers the query: 0 contains it, 1 matches its
/// letters in order, 2 does not (the row matched through its description).
fn match_tier(query: &str, title: &str) -> u8 {
    let query = query.trim();
    if query.is_empty() || title.to_lowercase().contains(&query.to_lowercase()) {
        0
    } else if fuzzy_score(query, title).is_some() {
        1
    } else {
        2
    }
}

/// Byte ranges of `title` that answer `query`, for highlighting: the first
/// case-insensitive occurrence, else the in-order letters fuzzy matching
/// used (whitespace in the query is skipped). Empty when nothing matches.
fn title_match(query: &str, title: &str) -> Vec<std::ops::Range<usize>> {
    let wanted: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = title.char_indices().collect();
    let same = |c: char, q: char| c.to_lowercase().eq(std::iter::once(q));
    let phrase: Vec<char> = query.trim().chars().flat_map(char::to_lowercase).collect();
    // A contiguous occurrence of the query as typed (spaces included).
    if !phrase.is_empty() {
        for start in 0..chars.len() {
            if start + phrase.len() > chars.len() {
                break;
            }
            if (0..phrase.len()).all(|k| same(chars[start + k].1, phrase[k])) {
                let end = chars
                    .get(start + phrase.len())
                    .map_or(title.len(), |&(byte, _)| byte);
                return vec![chars[start].0..end];
            }
        }
    }
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut next = 0;
    for &(byte, c) in &chars {
        if next < wanted.len() && same(c, wanted[next]) {
            next += 1;
            let end = byte + c.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == byte => last.end = end,
                _ => ranges.push(byte..end),
            }
        }
    }
    if next == wanted.len() {
        ranges
    } else {
        Vec::new()
    }
}

/// The palette selection after an arrow or page key: arrows wrap around
/// the ends, pages stop at them.
fn palette_step(index: usize, total: usize, key: &str) -> usize {
    let last = total.saturating_sub(1);
    let index = index.min(last);
    match key {
        "up" if index == 0 => last,
        "up" => index - 1,
        "down" if index >= last => 0,
        "down" => index + 1,
        "pageup" => index.saturating_sub(PALETTE_PAGE),
        "pagedown" => (index + PALETTE_PAGE).min(last),
        _ => index,
    }
}

/// Empty-query order of palette categories (the detail's first segment):
/// review decisions, then navigation and filtering, then file and app commands.
fn palette_category_rank(detail: &str) -> usize {
    const ORDER: [&str; 13] = [
        "Review",
        "Edit",
        "View",
        "Compare",
        "Bursts",
        "Filter",
        "Library",
        "Catalog",
        "Layout",
        "File",
        "Help",
        "App",
        "Appearance",
    ];
    let category = detail.split(" · ").next().unwrap_or(detail);
    ORDER
        .iter()
        .position(|&c| c == category)
        .unwrap_or(ORDER.len())
}

#[derive(Clone, PartialEq, Eq)]
struct SourceIdentity {
    source: shadowroom::model::PhotoSource,
    primary: (PathBuf, i64, u64),
    companion: Option<(PathBuf, i64, u64)>,
}
impl From<&Photo> for SourceIdentity {
    fn from(p: &Photo) -> Self {
        Self {
            source: p.source.clone(),
            primary: (p.path.clone(), p.modified_at, p.size_bytes),
            companion: p.companion_path.as_ref().map(|path| {
                (
                    path.clone(),
                    p.companion_modified_at,
                    p.companion_size_bytes,
                )
            }),
        }
    }
}

// Convert the existing bounded crossbeam request channel into an async wake-up
// without blocking GPUI's executor. At most one extra request is held here; the
// producer waits for its consumption before reading another. Dropping the task
// wakes the worker through stop, including while it is waiting for consumption.
struct InboxState<T> {
    value: Option<T>,
    closed: bool,
    waker: Option<Waker>,
}
struct UiInbox<T> {
    state: Arc<Mutex<InboxState<T>>>,
    consumed: Sender<()>,
    stop: Sender<()>,
}
impl<T: Send + 'static> UiInbox<T> {
    fn new(receiver: Receiver<T>) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(InboxState {
            value: None,
            closed: false,
            waker: None,
        }));
        let (consumed, consumption) = crossbeam_channel::bounded(1);
        let (stop, stopped) = crossbeam_channel::bounded(1);
        let shared = state.clone();
        std::thread::Builder::new().name("automation-ui-wake".into()).spawn(move || {
            loop {
                let value = crossbeam_channel::select! {
                    recv(stopped) -> _ => break,
                    recv(receiver) -> value => match value { Ok(value) => value, Err(_) => break },
                };
                let waker = {
                    let mut state = shared.lock().expect("automation inbox");
                    state.value = Some(value);
                    state.waker.take()
                };
                if let Some(waker) = waker { waker.wake(); }
                crossbeam_channel::select! {
                    recv(stopped) -> _ => break,
                    recv(consumption) -> result => if result.is_err() { break; },
                }
            }
            let waker = {
                let mut state = shared.lock().expect("automation inbox");
                state.closed = true;
                state.waker.take()
            };
            if let Some(waker) = waker { waker.wake(); }
        })?;
        Ok(Self {
            state,
            consumed,
            stop,
        })
    }
    async fn recv(&self) -> Option<T> {
        std::future::poll_fn(|cx| {
            let mut state = self.state.lock().expect("automation inbox");
            if let Some(value) = state.value.take() {
                let _ = self.consumed.try_send(());
                Poll::Ready(Some(value))
            } else if state.closed {
                Poll::Ready(None)
            } else {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
impl<T> Drop for UiInbox<T> {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

// One contribution per photo; independent of the current folder/filter.
#[derive(Default, Debug, PartialEq, Eq)]
struct CollectionCounts {
    all: usize,
    picks: usize,
    unrated: usize,
    rejects: usize,
    trash: usize,
    rated: usize,
    labels: [usize; 6],
}
fn label_index(label: ColorLabel) -> usize {
    match label {
        ColorLabel::None => 0,
        ColorLabel::Red => 1,
        ColorLabel::Yellow => 2,
        ColorLabel::Green => 3,
        ColorLabel::Blue => 4,
        ColorLabel::Purple => 5,
    }
}
impl CollectionCounts {
    fn adjust(&mut self, d: Decision, add: bool) {
        let bump = |n: &mut usize| {
            if add {
                *n += 1;
            } else {
                *n -= 1;
            }
        };
        if d.trashed {
            bump(&mut self.trash);
            return;
        }
        bump(&mut self.all);
        if d.flag == Flag::Pick {
            bump(&mut self.picks);
        }
        if d.flag == Flag::Reject {
            bump(&mut self.rejects);
        }
        if d.flag == Flag::None && d.rating == 0 {
            bump(&mut self.unrated);
        }
        if d.rating >= 3 {
            bump(&mut self.rated);
        }
        bump(&mut self.labels[label_index(d.label)]);
    }
    fn from_photos(photos: &[Photo]) -> Self {
        let mut counts = Self::default();
        for photo in photos {
            counts.adjust(Decision::from(photo), true);
        }
        counts
    }
}

/// Selection after the visible list changes: the kept photo's new position
/// while it is still visible, else the old index clamped to the new list.
fn retained_position(kept: Option<usize>, previous: usize, len: usize) -> usize {
    kept.unwrap_or(previous.min(len.saturating_sub(1)))
}
fn index_filtered_positions(positions: &mut Vec<usize>, photo_count: usize, filtered: &[usize]) {
    positions.resize(photo_count, usize::MAX);
    positions.fill(usize::MAX);
    for (position, &i) in filtered.iter().enumerate() {
        positions[i] = position;
    }
}

/// Sorted (folder key, visible photo count) pairs for the sidebar.
fn folder_counts(photos: &[Photo]) -> Vec<(String, usize)> {
    let mut folders = HashMap::new();
    for p in photos.iter().filter(|p| !p.trashed) {
        *folders.entry(p.folder_key()).or_insert(0) += 1;
    }
    let mut folders: Vec<_> = folders.into_iter().collect();
    folders.sort_by(|a, b| a.0.cmp(&b.0));
    folders
}

fn adjust_folder(folders: &mut Vec<(String, usize)>, folder: &str, add: bool) {
    match folders.binary_search_by(|(path, _)| path.as_str().cmp(folder)) {
        Ok(i) => {
            if add {
                folders[i].1 += 1;
            } else {
                folders[i].1 -= 1;
                if folders[i].1 == 0 {
                    folders.remove(i);
                }
            }
        }
        Err(i) => {
            if add {
                folders.insert(i, (folder.into(), 1));
            }
        }
    }
}

#[derive(Default)]
struct BurstIndex {
    slots: HashMap<i64, (usize, usize, usize)>,
    picks: Vec<usize>,
    unresolved: HashSet<i64>,
}
impl BurstIndex {
    fn new(groups: &[Vec<i64>], photos: &[Photo], index: &HashMap<i64, usize>) -> Self {
        let mut result = Self::default();
        for (g, ids) in groups.iter().enumerate() {
            let picks = ids
                .iter()
                .filter(|id| index.get(id).is_some_and(|&i| photos[i].flag == Flag::Pick))
                .count();
            result.picks.push(picks);
            if ids.len() <= 1 {
                continue;
            }
            for (frame, &id) in ids.iter().enumerate() {
                result.slots.insert(id, (g, frame, ids.len()));
                if picks == 0 {
                    result.unresolved.insert(id);
                }
            }
        }
        result
    }
    // Only a zero/nonzero transition changes the unresolved collection.
    fn change_pick(&mut self, id: i64, picked: bool, groups: &[Vec<i64>]) -> bool {
        let Some(&(g, _, _)) = self.slots.get(&id) else {
            return false;
        };
        let was_unresolved = self.picks[g] == 0;
        if picked {
            self.picks[g] += 1;
        } else {
            self.picks[g] -= 1;
        }
        let unresolved = self.picks[g] == 0;
        if unresolved == was_unresolved {
            return false;
        }
        for id in &groups[g] {
            if unresolved {
                self.unresolved.insert(*id);
            } else {
                self.unresolved.remove(id);
            }
        }
        true
    }
}

#[derive(Clone)]
struct SurveySummary {
    positions: Vec<usize>,
    decided: usize,
    span: String,
}
impl SurveySummary {
    fn new(mut positions: Vec<usize>, filtered: &[usize], photos: &[Photo]) -> Self {
        positions.sort_unstable();
        positions.dedup();
        let mut decided = 0;
        let mut times: Option<(i64, i64)> = None;
        for &position in &positions {
            let p = &photos[filtered[position]];
            decided += usize::from(!Filter::Unrated.accepts(p));
            if let Some(t) = p.captured_at {
                times = Some(times.map_or((t, t), |(first, last)| (first.min(t), last.max(t))));
            }
        }
        let span = match times {
            Some((first, last)) if last > first => {
                format!(" · {:.1} s", (last - first) as f64 / 1000.)
            }
            _ => String::new(),
        };
        Self {
            positions,
            decided,
            span,
        }
    }
    fn update_reviewed(&mut self, position: usize, before: bool, after: bool) {
        if before == after || self.positions.binary_search(&position).is_err() {
            return;
        }
        if after {
            self.decided += 1;
        } else {
            self.decided -= 1;
        }
    }
}

type TextureKey = (i64, u8, i64, u64);
#[derive(Clone)]
struct TextureSource {
    embedded_raw: bool,
    rendered_raw: bool,
    note: Option<String>,
    width: u32,
    height: u32,
}
struct Texture {
    image: Arc<RenderImage>,
    bytes: usize,
    tick: u64,
    width: u32,
    height: u32,
    source: TextureSource,
    kind: PreviewKind,
    color_status: ColorStatus,
    _reservation: shadowroom::media::DisplayReservation,
}
/// Desktop-side view of one tracked export, reported through automation.
#[derive(Clone, Debug, serde::Serialize)]
struct ExportJob {
    id: u64,
    state: JobState,
    completed: usize,
    error: Option<String>,
    /// Where it writes and whether it exports picks (else XMP sidecars), for
    /// the completion notice and its Reveal button.
    #[serde(skip)]
    target: Option<(PathBuf, bool)>,
}
/// What raised the error toast, so a later success clears only its own notice
/// instead of matching message text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NoticeKind {
    #[default]
    General,
    /// Decision writes failed or a close was refused over unsaved decisions;
    /// cleared once every decision is acknowledged.
    UnsavedDecisions,
    /// A preferences write failed; cleared by the matching durable ACK.
    UnsavedPreferences,
}
/// The error toast: user-visible text plus the source that may retire it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    kind: NoticeKind,
    text: String,
}
impl Notice {
    fn new(kind: NoticeKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}
impl From<String> for Notice {
    fn from(text: String) -> Self {
        Self::new(NoticeKind::General, text)
    }
}
impl From<&str> for Notice {
    fn from(text: &str) -> Self {
        Self::new(NoticeKind::General, text)
    }
}
impl serde::Serialize for Notice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}
/// Clears `slot` only when it holds a notice of `kind`.
fn clear_notice(slot: &mut Option<Notice>, kind: NoticeKind) {
    if slot.as_ref().is_some_and(|notice| notice.kind == kind) {
        *slot = None;
    }
}
const ACTIVE_POLL: Duration = Duration::from_millis(50);
/// Idle windows still notice drives, remote hosts and theme files, and keep a
/// focused caret blinking, at a fifth of the wakeups.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Input keeps the fast poll for a while: its decodes and saves follow shortly.
const INPUT_GRACE: Duration = Duration::from_secs(2);
/// Longest a navigation-only settings change (selected photo, keyboard dock
/// resizing) waits before it is handed to the preferences writer.
const NAVIGATION_PERSIST: Duration = Duration::from_secs(1);

/// Throttles persistence of high-frequency navigation state.
///
/// `Clean ─touch─▶ Due(t₀ + NAVIGATION_PERSIST) ─poll at/after due─▶ Clean`.
/// Later touches ride along with the scheduled write instead of extending it,
/// so a held arrow key persists at most once per interval and never loses more
/// than one interval of position on a crash. Quit, window deactivation and any
/// other settings save flush it at once (a settings snapshot carries it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeferredSave {
    due: Option<Instant>,
}
impl DeferredSave {
    fn touch(&mut self, now: Instant) {
        self.due.get_or_insert(now + NAVIGATION_PERSIST);
    }
    #[cfg(test)]
    fn pending(self) -> bool {
        self.due.is_some()
    }
    /// Clears and reports a write whose interval has elapsed.
    fn take_due(&mut self, now: Instant) -> bool {
        if self.due.is_some_and(|due| now >= due) {
            self.due = None;
            true
        } else {
            false
        }
    }
    /// Clears and reports any scheduled write, due or not.
    fn take(&mut self) -> bool {
        self.due.take().is_some()
    }
}

/// Poll cadence: `Active` (50 ms) while any background state machine has work
/// or input was recent, `Idle` (250 ms) otherwise. An idle sleep is cut short
/// by input, so going idle never delays the response to a keystroke.
#[derive(Default)]
struct PollWake {
    idle: Cell<bool>,
    kicked: Cell<bool>,
    waker: RefCell<Option<Waker>>,
    active_until: Cell<Option<Instant>>,
}
impl PollWake {
    fn kick(&self) {
        self.active_until.set(Some(Instant::now() + INPUT_GRACE));
        if self.idle.get() {
            self.kicked.set(true);
            if let Some(waker) = self.waker.borrow_mut().take() {
                waker.wake();
            }
        }
    }
    fn recently_active(&self) -> bool {
        self.active_until
            .get()
            .is_some_and(|until| Instant::now() < until)
    }
    async fn sleep(&self, idle: bool) {
        self.idle.set(idle);
        self.kicked.set(false);
        let mut timer = std::pin::pin!(Timer::after(if idle { IDLE_POLL } else { ACTIVE_POLL }));
        std::future::poll_fn(|cx| {
            if self.kicked.get() || timer.as_mut().poll(cx).is_ready() {
                Poll::Ready(())
            } else {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        self.idle.set(false);
        self.waker.borrow_mut().take();
    }
}

/// A surface that owns input above the workspace; see `Workspace::overlays`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    FolderPicker,
    Deletion,
    ImageContext,
    GettingStarted,
    RatingFilter,
    Preferences,
    Palette,
    AddHost,
}
impl Overlay {
    /// Every overlay, topmost first. `render` paints them in reverse of this
    /// order and `key()` routes to the first open one; the palette and the
    /// guide layer over Preferences so they never discard its draft.
    const STACK: [Self; 8] = [
        Self::FolderPicker,
        Self::Deletion,
        Self::ImageContext,
        Self::GettingStarted,
        Self::RatingFilter,
        Self::Palette,
        Self::Preferences,
        Self::AddHost,
    ];
    fn trace_name(self) -> &'static str {
        match self {
            Self::FolderPicker => "folder_picker",
            Self::Deletion => "deletion",
            Self::ImageContext => "image_context",
            Self::GettingStarted => "getting_started",
            Self::RatingFilter => "rating_filter",
            Self::Palette => "palette",
            Self::Preferences => "preferences",
            Self::AddHost => "add_host",
        }
    }
}
/// Durability of review decisions, derived from the optimistic `unsaved` map
/// and the count of unacknowledged writer batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveState {
    Durable,
    /// Batches are queued or in flight; they may still clear `unsaved`.
    Pending,
    /// Decisions remain unsaved with nothing in flight: only a retry helps.
    Failed,
}
impl SaveState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Pending => "pending",
            Self::Failed => "failed",
        }
    }
}
/// Preference persistence, from the latest in-memory edit to a durable ACK.
///
/// `Durable ─edit─▶ Queued ─sent─▶ Writing ─matching ACK─▶ Durable`. A recovered
/// snapshot that cannot be written (malformed settings file) is `Held` until a
/// repair makes it `Queued`. Write failures stay in `Writing`: the writer keeps
/// and retries the payload, and `settings_failure` carries the last error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrefsSync {
    #[default]
    Durable,
    Queued,
    Writing,
    Held,
}
impl PrefsSync {
    fn dirty(self) -> bool {
        self != Self::Durable
    }
}
/// Window close lifecycle.
///
/// `Open ─request─▶ Draining ─writes drained─▶ Flushing ─ack─▶ (exit)`; a refused
/// barrier (unsaved decisions, unrepaired preferences) or a failed flush returns
/// to `Open`. Each poll re-enters the barrier while `Draining`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum QuitState {
    #[default]
    Open,
    /// Waiting for exports, imports and decision/preference writes to finish.
    Draining,
    /// The final catalog flush is in flight; nothing may re-request it.
    Flushing,
}
impl QuitState {
    fn closing(self) -> bool {
        self != Self::Open
    }
}
/// Pointer panning of a zoomed photo pane.
///
/// `Idle ─press in pane─▶ Pressed ─move─▶ Dragging`; any left-button release
/// (inside or outside the window) returns to `Idle`, as does a move that
/// arrives without the button held (a release the window never saw). Only the
/// pane that took the press pans, so a drag that starts on the filmstrip,
/// a scrollbar or the other compare pane never moves the image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum PanDrag {
    #[default]
    Idle,
    /// The left button went down in a zoomed pane; nothing has moved yet.
    Pressed { reference: bool, at: Point<Pixels> },
    /// The button is held and the pointer has moved: the pan follows it.
    Dragging { reference: bool, at: Point<Pixels> },
}
impl PanDrag {
    fn press(reference: bool, at: Point<Pixels>) -> Self {
        Self::Pressed { reference, at }
    }
    fn holds(self, pane: bool) -> bool {
        matches!(self, Self::Pressed { reference, .. } | Self::Dragging { reference, .. } if reference == pane)
    }
    /// Advances on a pointer move over `pane` and returns the pan delta.
    fn motion(
        &mut self,
        pane: bool,
        position: Point<Pixels>,
        button_held: bool,
    ) -> Option<Point<Pixels>> {
        match *self {
            Self::Idle => None,
            _ if !button_held => {
                *self = Self::Idle;
                None
            }
            Self::Pressed { reference, at } | Self::Dragging { reference, at }
                if reference == pane =>
            {
                *self = Self::Dragging {
                    reference,
                    at: position,
                };
                Some(position - at)
            }
            _ => None,
        }
    }
    /// Returns whether a press was released.
    fn release(&mut self) -> bool {
        std::mem::take(self) != Self::Idle
    }
}
/// The "Add network host" form. It owns its caret and selection: the palette
/// can open above it and must not disturb where the user was typing.
struct AddHostForm {
    fields: [String; 2],
    active: usize,
    edit: text_input::Edit,
    /// Validation message shown under the alias field; cleared by typing.
    error: Option<String>,
}
impl AddHostForm {
    fn new() -> Self {
        let mut edit = text_input::Edit::default();
        edit.focus("");
        Self {
            fields: [String::new(), "shadowroom-remote serve --stdio".into()],
            active: 0,
            edit,
            error: None,
        }
    }
    /// Moves keyboard focus to `field`, caret at its end.
    fn focus(&mut self, field: usize) {
        self.active = field;
        self.edit.focus(&self.fields[field]);
    }
}
#[derive(Clone, Copy)]
struct DockDrag {
    side: DockSide,
    x: f32,
    width: f32,
    proposed: f32,
}

pub struct Workspace {
    focus: FocusHandle,
    library_resize_focus: FocusHandle,
    inspector_resize_focus: FocusHandle,
    dock_drag: Option<DockDrag>,
    suppress_dock_click: bool,
    docks: ui::DockLayout,
    dock_priority: DockSide,
    workspace_size: (f32, f32),
    settings: Settings,
    themes: Vec<Theme>,
    theme: Theme,
    fallback_theme: Theme,
    theme_colors: RefCell<HashMap<(&'static str, &'static str), Hsla>>,
    /// Settings or theme files changed since `apply_theme_mode` last resolved.
    theme_stale: bool,
    /// Last seen `platform_prefs::revision()`.
    platform_revision: u64,
    #[cfg(target_os = "macos")]
    platform_polled: Instant,
    /// The window appearance `apply_theme_mode` last resolved against.
    theme_appearance: Option<WindowAppearance>,
    photos: Vec<Photo>,
    photo_index: HashMap<i64, usize>,
    counts: CollectionCounts,
    filtered_positions: Vec<usize>,
    filtered_reviewed: usize,
    decision_membership_changed: bool,
    bursts: BurstIndex,
    duplicate_slots: HashMap<i64, (usize, usize, usize)>,
    survey_cache: Option<(SurveyKey, Arc<SurveySummary>)>,
    filtered: Vec<usize>,
    selected: usize,
    multi: HashSet<i64>,
    selection_anchor: Option<i64>,
    review_subset: Option<HashSet<i64>>,
    review_folder: Option<String>,
    review_selection: Option<HashSet<i64>>,
    catalog_hover: Option<i64>,
    image_context: Option<review_actions::ImageContextMenu>,
    analysis_cursor: usize,
    analysis_pending: HashSet<i64>,
    analysis_errors: usize,
    filter: Filter,
    min_rating: u8,
    scope: Option<String>,
    folders: Vec<(String, usize)>,
    groups: Vec<Vec<i64>>,
    group_lookup: HashMap<i64, usize>,
    mode: ViewMode,
    reference: Option<i64>,
    reference_focus: bool,
    compare_sync: bool,
    pans: [[f32; 2]; 2],
    pan_drag: PanDrag,
    zoom: bool,
    zoom_factor: f32,
    fit_scales: std::rc::Rc<std::cell::Cell<[f32; 2]>>,
    focus_peaking: bool,
    /// Clipping overlay (J): blown highlights red, crushed shadows blue.
    clipping: bool,
    navigation_direction: isize,
    preview_foreground: RefCell<Option<(Option<i64>, Option<i64>, PreviewKind, usize)>>,
    inspector: bool,
    inspector_info: bool,
    rating_filter_open: bool,
    rating_filter_index: u8,
    histogram: bool,
    histogram_log: bool,
    preview_details: bool,
    palette_recent: Vec<String>,
    /// Session-only smart collections layered over the main filter.
    label_filter: Option<ColorLabel>,
    bursts_only: bool,
    /// Centre pane is too narrow for labelled controls; set each frame.
    compact: bool,
    last_grid_cols: usize,
    /// Logical centre-pane size from the last frame, for fitted overlays.
    center_size: (f32, f32),
    focus_mode: bool,
    sidebar: bool,
    title_drag: bool,
    palette: bool,
    /// The palette was opened with Ctrl/Cmd+F: photo results only.
    palette_photos: bool,
    palette_query: String,
    palette_edit: text_input::Edit,
    palette_index: usize,
    palette_cache: RefCell<Option<PaletteCache>>,
    palette_revision: u64,
    preferences: bool,
    preferences_draft: Option<preferences::Draft>,
    preferences_section: PreferencesSection,
    /// Keyboard focus handles for dialog controls (`dialog.rs`).
    dialog_focus: RefCell<dialog::DialogFocus>,
    theme_list_scroll: ScrollHandle,
    /// Getting Started body: the scrollbar and keyboard scrolling share it.
    guide_scroll: ScrollHandle,
    /// The keyboard, not the pointer, moved focus last: show the focus ring.
    focus_visible: bool,
    /// The pane F6 moved keyboard focus to (`panes.rs`).
    pane: panes::Pane,
    /// F6 was used and the pointer has not taken over since: ring the pane.
    pane_ring: bool,
    /// Keyboard cursor over `library_targets()` while the library has focus.
    library_cursor: usize,
    deletion: deletion_ui::State,
    folder_picker: Option<folder_picker::State>,
    getting_started: bool,
    text_edit: text_input::Edit,
    scroll_drag: Option<scrollbar::Drag>,
    io: Sender<IoCommand>,
    events: Receiver<IoEvent>,
    photo_slot: PhotoSlot,
    group_slot: GroupSlot,
    media: MediaPipeline,
    textures: HashMap<TextureKey, Texture>,
    texture_bytes: usize,
    tick: u64,
    texture_inflight: HashSet<TextureKey>,
    texture_retry: bool,
    retired: Vec<Texture>,
    visible_textures: HashSet<TextureKey>,
    settings_writable: bool,
    prefs_sync: PrefsSync,
    /// Navigation state changed in `settings` but not yet queued; see `DeferredSave`.
    navigation_save: DeferredSave,
    settings_failure: Option<String>,
    unsaved: HashMap<i64, Decision>,
    outbox: VecDeque<Vec<PhotoDecision>>,
    /// Decision batches queued or sent but not yet acknowledged; the writer
    /// answers each with exactly one Saved/SaveFailed.
    awaiting: usize,
    save_revisions: SaveRevisions,
    recovery_path: PathBuf,
    recovery_pending: bool,
    recovery_broken: bool,
    recovery_settings: Option<Settings>,
    quit: QuitState,
    import_ui: ingest_ui::ImportUi,
    pending_ingest_scope: Option<PathBuf>,
    /// What the folder picker's choice does: Open Folder culls in place,
    /// Import from Card copies through the Import view.
    folder_intent: ingest_ui::FolderIntent,
    /// The window has focus: desktop notifications are only for walk-away.
    window_active: bool,
    export_jobs: Vec<ExportJob>,
    next_job_id: u64,
    duplicate_scan: Option<shadowroom::duplicates::DuplicateScanState>,
    duplicate_ids: HashSet<i64>,
    duplicate_lookup: HashMap<i64, usize>,
    duplicate_sources: HashMap<i64, SourceIdentity>,
    duplicates_only: bool,
    theme_updates: Receiver<ThemeUpdate>,
    _theme_watcher: ThemeWatcher,
    zed_theme: Option<ZedSelection>,
    theme_diagnostics: Vec<String>,
    history: History,
    status: String,
    error: Option<Notice>,
    feedback: feedback::Feedback,
    busy: bool,
    drives: Vec<Drive>,
    remote_hosts: std::collections::BTreeMap<String, shadowroom::network::HostSnapshot>,
    add_host: Option<AddHostForm>,
    grid_scroll: UniformListScrollHandle,
    grid_range: Option<(usize, usize)>,
    last_media: u64,
    automation: Option<AutomationServer>,
    /// The platform file chooser failed once (no portal): use the built-in picker.
    native_chooser_failed: bool,
    /// An automation client has sent a request this session.
    automation_driven: bool,
    _poll: Task<()>,
    poll_wake: Rc<PollWake>,
    /// The pending repaint was requested by poll, not by input.
    poll_render: bool,
    _automation_wake: Option<Task<()>>,
    _shutdown: Subscription,
    /// Catalog thumbnail size: 0 = S, 1 = M, 2 = L (`settings.extra.grid_size`).
    grid_size: usize,
    /// Painted image geometry per pane ([reference, current]) from the last
    /// frame, so pointer zoom and keyboard pan can map screen to image.
    pane_geometry: Rc<Cell<[PaneGeometry; 2]>>,
    /// The zoom navigator of this pane is being dragged.
    navigator_drag: Option<bool>,
    /// A batch decision over many photos waiting for Enter (CAT-11).
    pending_bulk: Option<PendingBulk>,
    /// Counts for the current folder scope; `counts` stays library-wide.
    scope_counts: Option<CollectionCounts>,
    /// With a rating filter: in-scope counts of photos it lets through.
    rated_counts: Option<CollectionCounts>,
    /// Bumped whenever a photo's decision changes; keys derived caches.
    decision_epoch: u64,
    selection_summary_cache: RefCell<Option<((usize, usize, u64), SelectionSummary)>>,
    /// Cull ledger cells keyed by (decision epoch, frames, cells).
    ledger_cache: RefCell<Option<(ledger::CacheKey, Rc<ledger::Ledger>)>>,
    /// Painted ledger track (left edge, geometry) for pointer hit-testing.
    ledger_track: Rc<Cell<(f32, ledger::Track)>>,
    /// The guide opened by itself at launch; closing it stops that.
    startup_guide: bool,
    /// One-time notices already shown this session.
    shown_hints: HashSet<&'static str>,
    window_title: String,
    /// The user reopened a dock that small-window fitting had hidden:
    /// [library, inspector] stay until hidden again.
    dock_override: [bool; 2],
    /// Opt-in frame/input trace (`SHADOWROOM_FRAME_TRACE`); see `frame_trace`.
    trace: frame_trace::Trace,
    _trace_keys: Option<Subscription>,
}

/// Image placement in one photo pane, measured while painting.
#[derive(Clone, Copy, Debug, Default)]
struct PaneGeometry {
    bounds: Bounds<Pixels>,
    /// Texture size in pixels.
    width: u32,
    height: u32,
    /// Source pixels per texture pixel (a reduced preview refines in place).
    source_scale: f32,
    scale_factor: f32,
}
impl PaneGeometry {
    fn valid(&self) -> bool {
        self.width > 0 && self.height > 0 && f32::from(self.bounds.size.width) > 0.
    }
    /// Logical pixels per texture pixel at Fit.
    fn fit_scale(&self) -> f32 {
        (f32::from(self.bounds.size.width) / self.width as f32)
            .min(f32::from(self.bounds.size.height) / self.height as f32)
    }
    /// Logical pixels per texture pixel at a zoom factor (1 = 100% of source).
    fn zoom_scale(&self, factor: f32) -> f32 {
        factor * self.source_scale / self.scale_factor.max(0.01)
    }
    /// Image position (0–1 per axis) under a window point for a given scale
    /// and pan, clamped to the image.
    fn image_point(&self, at: Point<Pixels>, scale: f32, pan: [f32; 2]) -> [f32; 2] {
        let size = [
            f32::from(self.bounds.size.width),
            f32::from(self.bounds.size.height),
        ];
        let start = [
            f32::from(self.bounds.origin.x),
            f32::from(self.bounds.origin.y),
        ];
        let image = [self.width as f32 * scale, self.height as f32 * scale];
        let at = [f32::from(at.x), f32::from(at.y)];
        std::array::from_fn(|i| {
            let origin = if image[i] > size[i] {
                start[i] - (image[i] - size[i]) * pan[i]
            } else {
                start[i] + (size[i] - image[i]) / 2.
            };
            ((at[i] - origin) / image[i].max(1.)).clamp(0., 1.)
        })
    }
    /// The pan that puts image point `u` at pane offset `target` (logical
    /// px from the pane origin) at `scale`.
    fn pan_for(&self, u: [f32; 2], target: [f32; 2], scale: f32) -> [f32; 2] {
        let size = [
            f32::from(self.bounds.size.width),
            f32::from(self.bounds.size.height),
        ];
        let image = [self.width as f32 * scale, self.height as f32 * scale];
        std::array::from_fn(|i| {
            if image[i] > size[i] {
                ((u[i] * image[i] - target[i]) / (image[i] - size[i])).clamp(0., 1.)
            } else {
                0.5
            }
        })
    }
    fn contains(&self, at: Point<Pixels>) -> bool {
        self.valid() && self.bounds.contains(&at)
    }
}

/// A decision on more photos than the confirmation threshold, held until
/// Enter applies it or Escape cancels.
#[derive(Clone, Debug)]
struct PendingBulk {
    rating: Option<u8>,
    flag: Option<Flag>,
    label: Option<ColorLabel>,
    count: usize,
}

/// Decisions of a multi-selection, for mixed-state display (CAT-10).
#[derive(Clone, Debug, Default, PartialEq)]
struct SelectionSummary {
    count: usize,
    /// Pick, Reject, None.
    flags: [usize; 3],
    /// Photos per star count, 0–5.
    ratings: [usize; 6],
    labels: [usize; 6],
}
impl SelectionSummary {
    fn add(&mut self, p: &Photo) {
        self.count += 1;
        self.flags[match p.flag {
            Flag::Pick => 0,
            Flag::Reject => 1,
            Flag::None => 2,
        }] += 1;
        self.ratings[usize::from(p.rating.min(5))] += 1;
        self.labels[label_index(p.label)] += 1;
    }
    /// The flag every member shares, if any.
    fn common_flag(&self) -> Option<Flag> {
        [Flag::Pick, Flag::Reject, Flag::None]
            .into_iter()
            .zip(self.flags)
            .find(|&(_, n)| n == self.count)
            .map(|(flag, _)| flag)
    }
    fn common_rating(&self) -> Option<u8> {
        (0..=5u8).find(|&r| self.ratings[usize::from(r)] == self.count)
    }
    fn common_label(&self) -> Option<ColorLabel> {
        [
            ColorLabel::None,
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ]
        .into_iter()
        .find(|&l| self.labels[label_index(l)] == self.count)
    }
    fn has_flag(&self, flag: Flag) -> bool {
        self.flags[match flag {
            Flag::Pick => 0,
            Flag::Reject => 1,
            Flag::None => 2,
        }] > 0
    }
}

/// What Survey shows: a review selection, the burst, or the neighbours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurveyKey {
    /// The frozen Catalog selection being reviewed (its length).
    Selection(usize),
    /// A burst or duplicate set: (duplicates view, group index).
    Group(bool, usize),
    /// Frames around the current one when it has no group (first position).
    Neighbours(usize),
}
impl SurveyKey {
    fn label(self, len: usize) -> Option<String> {
        match self {
            Self::Selection(_) => Some(format!("Selection · {}", plural(len, "photo", "photos"))),
            Self::Neighbours(_) => Some(format!("Neighbours · {}", plural(len, "photo", "photos"))),
            Self::Group(..) => None,
        }
    }
}

/// A batch decision this large asks for Enter first (CAT-11).
const BULK_CONFIRM_AT: usize = 1000;
/// Smallest window (logical px): fits a 200% display of 1920x1080 (960x540)
/// and tablets; layouts below 1024x680 drop docks and bar items first.
const MIN_WINDOW: (f32, f32) = (720., 480.);
/// A batch decision this large is confirmed with a counted Undo notice.
const BULK_NOTICE_AT: usize = 50;

impl Workspace {
    fn new(cx: &mut gpui::Context<Self>, import: Option<PathBuf>, window: AnyWindowHandle) -> Self {
        let (mut settings, mut settings_error) = match Settings::load_default() {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("Preferences: {e:#}"))),
        };
        let dirs = directories::ProjectDirs::from("app", "Shadowroom", "shadowroom")
            .expect("user data directory");
        let recovery_path = dirs.data_local_dir().join("pending-decisions.json");
        let mut recovery_broken = false;
        let recovery = match shadowroom::recovery::RecoveryJournal::load(&recovery_path) {
            Ok(value) => value,
            Err(error) => {
                recovery_broken = true;
                settings_error = Some(format!(
                    "Recovery journal needs repair at {}: {error:#}",
                    recovery_path.display()
                ));
                None
            }
        };
        let recovery_pending = recovery.is_some();
        let recovery_settings = recovery
            .as_ref()
            .and_then(|journal| journal.settings.clone());
        if let Some(saved) = recovery
            .as_ref()
            .and_then(|journal| journal.settings.clone())
        {
            settings = saved;
        }
        let settings_writable = settings_error.is_none();
        let mode = match settings.view_mode.as_str() {
            "grid" => ViewMode::Grid,
            "compare" => ViewMode::Compare,
            "survey" => ViewMode::Survey,
            _ => ViewMode::Loupe,
        };
        let inspector = settings.show_inspector;
        let sidebar = settings.show_sidebar;
        let filter = match settings.filter.as_str() {
            "picks" => Filter::Picks,
            "unrated" => Filter::Unrated,
            "rejects" => Filter::Rejects,
            "trash" => Filter::Trash,
            _ => Filter::All,
        };
        let min_rating = settings.min_rating.min(5);
        let histogram_log = settings
            .extra
            .get("histogram_log")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let themes = Theme::builtins();
        let (theme_watcher, theme_updates) = start_theme_watch();
        let theme = themes
            .iter()
            .find(|t| t.name == settings.theme)
            .cloned()
            .unwrap_or_default();
        let fallback_theme = if theme.appearance == "light" {
            Theme::light()
        } else {
            Theme::dark()
        };
        let media = MediaPipeline::new(
            dirs.cache_dir().join("previews"),
            settings.memory_budget_bytes(),
        );
        media.set_disk_budget(settings.cache_budget_bytes() as u64);
        let (io, events, photo_slot, group_slot) = start_io(
            dirs.data_local_dir().to_path_buf(),
            settings.clone(),
            import,
        );
        let automation = match shadowroom::automation::start_server() {
            Ok(server) => Some(server),
            Err(e) => {
                eprintln!("Automation unavailable: {e:#}");
                None
            }
        };
        let shutdown = io.clone();
        let shutdown_subscription=cx.on_app_quit(move|this,_cx|{
            // applicationWillTerminate cannot veto exit. Preserve the exact latest
            // optimistic/failed values, including batches still in the UI outbox,
            // before asking the actor to flush. Startup replays this synced journal.
            this.flush_navigation();
            this.drain_settings();
            this.trace.flush();
            if let Some(journal)=this.pending_journal() && let Err(error)=journal.save(&this.recovery_path){eprintln!("CRITICAL: pending decision recovery could not be saved: {error:#}");}
            // Stop decoding now; queued preview-cache writes drain (at most 1 s)
            // while the catalog flushes, and are awaited below.
            this.media.shutdown(Duration::ZERO);
            let (ack,done)=crossbeam_channel::bounded(1);
            if shutdown.send_timeout(IoCommand::Shutdown(ack),Duration::from_secs(1)).is_ok(){
                if let Err(e)=done.recv_timeout(Duration::from_secs(6)){eprintln!("Shutdown did not finish; pending decisions remain in the recovery journal: {e}");}
            }
            this.media.shutdown(Duration::from_secs(1));
            async {}
        });
        let trace = frame_trace::Trace::from_env();
        motion::refresh_system_preference();
        // Interceptors run before any key dispatch; the deferred effect runs
        // once every handler for the keystroke has returned.
        let trace_keys = trace.enabled().then(|| {
            let trace = trace.clone();
            cx.intercept_keystrokes(move |event, _, cx| {
                trace.input("key", &event.keystroke.unparse());
                let trace = trace.clone();
                cx.defer(move |_| trace.handled());
            })
        });
        let poll_wake = Rc::new(PollWake::default());
        let wake = poll_wake.clone();
        let poll = cx.spawn(async move |this, cx| {
            let mut idle = false;
            loop {
                wake.sleep(idle).await;
                match this.update(cx, |this, cx| {
                    this.poll(cx);
                    this.background_idle()
                }) {
                    Ok(background_idle) => idle = background_idle && !wake.recently_active(),
                    Err(_) => break,
                }
            }
        });
        let import_ui = ingest_ui::ImportUi::new(&settings);
        let inspector_info =
            settings.extra.get("inspector_tab").and_then(Value::as_str) == Some("info");
        let getting_started = settings.show_getting_started;
        let grid_size = match settings.extra.get("grid_size").and_then(Value::as_str) {
            Some("s") => 0,
            Some("l") => 2,
            _ => 1,
        };
        let mut workspace = Self {
            focus: cx.focus_handle(),
            library_resize_focus: cx.focus_handle(),
            inspector_resize_focus: cx.focus_handle(),
            dock_drag: None,
            suppress_dock_click: false,
            docks: ui::DockLayout::default(),
            dock_priority: DockSide::Library,
            workspace_size: (1280., 800.),
            settings,
            themes,
            theme,
            fallback_theme,
            theme_colors: RefCell::default(),
            theme_stale: true,
            platform_revision: u64::MAX,
            #[cfg(target_os = "macos")]
            platform_polled: Instant::now(),
            theme_appearance: None,
            photos: Vec::new(),
            photo_index: HashMap::new(),
            counts: CollectionCounts::default(),
            filtered_positions: Vec::new(),
            filtered_reviewed: 0,
            decision_membership_changed: false,
            bursts: BurstIndex::default(),
            duplicate_slots: HashMap::new(),
            survey_cache: None,
            analysis_cursor: 0,
            analysis_pending: HashSet::new(),
            analysis_errors: 0,
            filtered: Vec::new(),
            selected: 0,
            multi: HashSet::new(),
            selection_anchor: None,
            review_subset: None,
            review_folder: None,
            review_selection: None,
            catalog_hover: None,
            image_context: None,
            filter,
            min_rating,
            scope: None,
            folders: Vec::new(),
            groups: Vec::new(),
            group_lookup: HashMap::new(),
            mode,
            reference: None,
            reference_focus: false,
            compare_sync: true,
            pans: [[0.5, 0.5]; 2],
            pan_drag: PanDrag::Idle,
            zoom: false,
            zoom_factor: 1.,
            fit_scales: std::rc::Rc::new(std::cell::Cell::new([1.; 2])),
            focus_peaking: false,
            clipping: false,
            navigation_direction: 1,
            preview_foreground: RefCell::new(None),
            inspector,
            inspector_info,
            rating_filter_open: false,
            rating_filter_index: min_rating,
            histogram: true,
            histogram_log,
            preview_details: false,
            palette_recent: Vec::new(),
            label_filter: None,
            bursts_only: false,
            compact: false,
            last_grid_cols: 0,
            center_size: (800., 600.),
            focus_mode: false,
            sidebar,
            title_drag: false,
            palette: false,
            palette_photos: false,
            palette_query: String::new(),
            palette_edit: text_input::Edit::default(),
            palette_index: 0,
            palette_cache: RefCell::default(),
            palette_revision: 0,
            preferences: false,
            preferences_draft: None,
            preferences_section: PreferencesSection::default(),
            dialog_focus: RefCell::default(),
            theme_list_scroll: ScrollHandle::new(),
            guide_scroll: ScrollHandle::new(),
            focus_visible: false,
            pane: panes::Pane::default(),
            pane_ring: false,
            library_cursor: 0,
            deletion: deletion_ui::State::default(),
            folder_picker: None,
            getting_started,
            text_edit: text_input::Edit::default(),
            scroll_drag: None,
            io,
            events,
            photo_slot,
            group_slot,
            media,
            textures: HashMap::new(),
            texture_bytes: 0,
            tick: 0,
            texture_inflight: HashSet::new(),
            texture_retry: false,
            retired: Vec::new(),
            visible_textures: HashSet::new(),
            settings_writable,
            prefs_sync: PrefsSync::Durable,
            navigation_save: DeferredSave::default(),
            settings_failure: None,
            history: History::default(),
            status: "Opening local catalog…".into(),
            error: settings_error.map(Notice::from),
            feedback: feedback::Feedback::default(),
            unsaved: HashMap::new(),
            outbox: VecDeque::new(),
            awaiting: 0,
            save_revisions: SaveRevisions::default(),
            recovery_path,
            recovery_pending,
            recovery_broken,
            recovery_settings,
            quit: QuitState::Open,
            import_ui,
            pending_ingest_scope: None,
            folder_intent: ingest_ui::FolderIntent::Import,
            window_active: true,
            export_jobs: Vec::new(),
            next_job_id: 0,
            duplicate_scan: None,
            duplicate_ids: HashSet::new(),
            duplicate_lookup: HashMap::new(),
            duplicate_sources: HashMap::new(),
            duplicates_only: false,
            theme_updates,
            _theme_watcher: theme_watcher,
            zed_theme: None,
            theme_diagnostics: Vec::new(),
            busy: false,
            drives: Vec::new(),
            remote_hosts: Default::default(),
            add_host: None,
            grid_scroll: UniformListScrollHandle::new(),
            grid_range: None,
            last_media: 0,
            automation,
            native_chooser_failed: false,
            automation_driven: false,
            _poll: poll,
            poll_wake,
            poll_render: false,
            _automation_wake: None,
            _shutdown: shutdown_subscription,
            grid_size,
            pane_geometry: Rc::new(Cell::new([PaneGeometry::default(); 2])),
            navigator_drag: None,
            pending_bulk: None,
            scope_counts: None,
            rated_counts: None,
            decision_epoch: 0,
            selection_summary_cache: RefCell::new(None),
            ledger_cache: RefCell::new(None),
            ledger_track: Rc::new(Cell::new((0., ledger::Track::default()))),
            startup_guide: getting_started,
            shown_hints: HashSet::new(),
            window_title: String::new(),
            dock_override: [false; 2],
            trace,
            _trace_keys: trace_keys,
        };
        if let Some(server) = &workspace.automation {
            match UiInbox::new(server.requests.clone()) {
                Ok(inbox) => {
                    workspace._automation_wake = Some(cx.spawn(async move |this, cx| {
                        while let Some(request) = inbox.recv().await {
                            let result = window.update(cx, |_, window, cx| {
                                this.update(cx, |this, cx| {
                                    this.handle_automation(request, window, cx)
                                })
                            });
                            if !matches!(result, Ok(Ok(()))) {
                                break;
                            }
                        }
                    }));
                }
                Err(error) => {
                    eprintln!("Automation wake-up unavailable; using poll fallback: {error}")
                }
            }
        }
        if let Some(recovery) = recovery {
            if !recovery.decisions.is_empty() {
                workspace
                    .save_revisions
                    .stage(recovery.decisions.iter().map(|d| d.id));
                workspace.unsaved = recovery
                    .decisions
                    .iter()
                    .map(|d| {
                        (
                            d.id,
                            Decision {
                                id: d.id,
                                rating: d.rating,
                                flag: d.flag,
                                label: d.label,
                                trashed: d.trashed,
                            },
                        )
                    })
                    .collect();
                workspace.awaiting += 1;
                workspace.outbox.push_back(recovery.decisions);
            }
            if recovery.settings.is_some() {
                // Keep this obligation even when malformed normal settings block
                // writes. Only an explicit preferences ACK releases the payload.
                workspace.prefs_sync = if workspace.settings_writable {
                    PrefsSync::Queued
                } else {
                    PrefsSync::Held
                };
                if !workspace.settings_writable {
                    workspace.settings_failure=Some("Repair the settings file, then use Retry Unsaved Decisions to recover preferences.".into());
                }
            }
            workspace.status = "Recovering pending decisions from the previous session…".into();
        }
        workspace
    }

    fn c(&self, key: &'static str, fallback: &'static str) -> Hsla {
        if let Some(color) = self.theme_colors.borrow().get(&(key, fallback)) {
            return *color;
        }
        let c = shadowroom::theme::resolve_role(&self.theme, &self.fallback_theme, key, fallback);
        let color = rgba(
            (u32::from(c.r) << 24)
                | (u32::from(c.g) << 16)
                | (u32::from(c.b) << 8)
                | u32::from(c.a),
        )
        .into();
        self.theme_colors
            .borrow_mut()
            .insert((key, fallback), color);
        color
    }
    fn text(&self) -> Hsla {
        self.c("text", "#d7dce2")
    }
    fn muted(&self) -> Hsla {
        self.c("text.muted", "#8c929e")
    }
    fn border(&self) -> Hsla {
        self.c("border", "#30343c")
    }
    fn panel(&self) -> Hsla {
        self.c("panel.background", "#1c1f26")
    }
    fn bg(&self) -> Hsla {
        self.c("background", "#20242b")
    }
    fn accent(&self) -> Hsla {
        self.c("text.accent", "#a8c2a2")
    }
    fn selected_photo(&self) -> Option<&Photo> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.photos.get(i))
    }
    /// The pinned reference pane owns review shortcuts: Compare is showing,
    /// Tab moved focus to it, and the reference is still in the catalog. A
    /// focus flag left over from Compare (or a reference that has since left
    /// the catalog) must neither swallow decisions nor stop auto-advance.
    fn reference_focused(&self) -> bool {
        self.mode == ViewMode::Compare && self.reference_focus && self.reference_available()
    }
    fn reference_available(&self) -> bool {
        self.reference
            .is_some_and(|id| self.photo_index.contains_key(&id))
    }
    fn review_photo(&self) -> Option<&Photo> {
        if self.filter == Filter::Trash {
            return self.selected_photo();
        }
        if self.reference_focused() {
            self.reference
                .and_then(|id| self.photo_index.get(&id).map(|&i| &self.photos[i]))
        } else {
            self.selected_photo()
        }
    }
    /// Decisions across a multi-selection of two or more photos. Large
    /// selections are summarised once per selection/decision change.
    fn selection_summary(&self) -> Option<SelectionSummary> {
        if self.multi.len() < 2 {
            return None;
        }
        let key = (self.multi.len(), self.selected, self.decision_epoch);
        if let Some((cached, summary)) = &*self.selection_summary_cache.borrow()
            && *cached == key
        {
            return Some(summary.clone());
        }
        let mut summary = SelectionSummary::default();
        for id in &self.multi {
            if let Some(&i) = self.photo_index.get(id) {
                summary.add(&self.photos[i]);
            }
        }
        *self.selection_summary_cache.borrow_mut() = Some((key, summary.clone()));
        Some(summary)
    }
    fn selected_id(&self) -> Option<i64> {
        self.selected_photo().map(|p| p.id)
    }
    fn save_state(&self) -> SaveState {
        if self.unsaved.is_empty() {
            SaveState::Durable
        } else if self.awaiting == 0 {
            SaveState::Failed
        } else {
            SaveState::Pending
        }
    }
    fn save_settings(&mut self) {
        // Every persisted settings change passes here, including theme mode
        // and system theme slots.
        self.theme_stale = true;
        if self.settings_writable {
            self.prefs_sync = PrefsSync::Queued;
            // The snapshot carries any deferred navigation state too.
            self.navigation_save.take();
        }
    }
    /// Persists navigation-only state (`last_photo_id`, keyboard dock widths)
    /// on the `DeferredSave` throttle instead of once per keypress.
    fn save_navigation(&mut self) {
        if self.settings_writable {
            self.navigation_save.touch(Instant::now());
        }
    }
    /// Queues deferred navigation state now (quit, window deactivation).
    fn flush_navigation(&mut self) {
        if self.navigation_save.take() {
            self.save_settings();
        }
    }
    fn drain_settings(&mut self) {
        if self.navigation_save.take_due(Instant::now()) {
            self.save_settings();
        }
        if self.prefs_sync == PrefsSync::Queued && self.settings_writable {
            match self.io.try_send(IoCommand::Settings(self.settings.clone())) {
                Ok(()) => self.prefs_sync = PrefsSync::Writing,
                Err(crossbeam_channel::TrySendError::Full(_)) => {}
                Err(_) => self.settings_failure = Some("Preferences writer is disconnected".into()),
            }
        }
    }
    /// Resolves the active theme from the theme mode. Render calls this every
    /// frame; it only does work after a settings save, a theme-file update or
    /// a system appearance change.
    fn apply_theme_mode(&mut self, window: &Window) {
        let appearance = window.appearance();
        if !std::mem::take(&mut self.theme_stale) && self.theme_appearance == Some(appearance) {
            return;
        }
        self.theme_appearance = Some(appearance);
        let dark = matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let mode = self
            .settings
            .extra
            .get("theme_mode")
            .and_then(Value::as_str)
            .unwrap_or("manual");
        let desired = match mode {
            "system" => self
                .settings
                .extra
                .get(if dark { "theme_dark" } else { "theme_light" })
                .and_then(Value::as_str)
                .unwrap_or(if dark {
                    "Shadowroom Dark"
                } else {
                    "Shadowroom Light"
                }),
            "zed" => self
                .zed_theme
                .as_ref()
                .and_then(|z| z.selected_name(dark))
                .unwrap_or(&self.settings.theme),
            _ => &self.settings.theme,
        };
        // The system asks for more contrast: the standard Shadowroom palettes
        // become their high-contrast variants (imported themes are kept).
        let desired = self.contrast_theme(desired).to_owned();
        let desired = desired.as_str();
        if self.theme.name != desired {
            if let Some(theme) = self.themes.iter().find(|t| t.name == desired) {
                self.theme = theme.clone();
                self.fallback_theme = if theme.appearance == "light" {
                    Theme::light()
                } else {
                    Theme::dark()
                };
                self.theme_colors.borrow_mut().clear();
                self.palette_revision = self.palette_revision.wrapping_add(1);
            }
        }
    }
    fn enqueue(&mut self, command: IoCommand) -> bool {
        match self.io.try_send(command) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("Catalog is unavailable or busy: {e}").into());
                false
            }
        }
    }
    fn queue_export(
        &mut self,
        destination: PathBuf,
        picks: bool,
        ids: Vec<i64>,
    ) -> std::result::Result<u64, String> {
        if self.quit.closing() {
            return Err("The app is closing; new exports are not accepted.".into());
        }
        if !self.unsaved.is_empty() {
            return Err(
                "Wait for pending decisions to save, or retry failed decisions, before exporting."
                    .into(),
            );
        }
        // Picks with no ids are every catalog pick; ids scope either kind.
        let scope: HashSet<i64> = ids.iter().copied().collect();
        for photo in self.photos.iter().filter(|photo| {
            !photo.trashed
                && (!picks || photo.flag == Flag::Pick)
                && ((picks && scope.is_empty()) || scope.contains(&photo.id))
        }) {
            photo
                .require_local("Export and XMP writing")
                .map_err(|e| e.to_string())?;
        }
        self.next_job_id += 1;
        let id = self.next_job_id;
        let target = Some((destination.clone(), picks));
        if !self.enqueue(IoCommand::ExportTracked {
            id,
            destination,
            picks,
            ids,
        }) {
            return Err("Export was not queued".into());
        }
        self.export_jobs.push(ExportJob {
            id,
            state: JobState::Queued,
            completed: 0,
            error: None,
            target,
        });
        Ok(id)
    }

    fn drain_decisions(&mut self) {
        for _ in 0..8 {
            let Some(batch) = self.outbox.pop_front() else {
                break;
            };
            match self.io.try_send(IoCommand::Decisions(batch)) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(IoCommand::Decisions(batch))) => {
                    self.outbox.push_front(batch);
                    break;
                }
                Err(crossbeam_channel::TrySendError::Disconnected(IoCommand::Decisions(_))) => {
                    self.outbox.clear();
                    self.awaiting = 0;
                    self.save_revisions.disconnect();
                    self.error=Some("Decision writer is disconnected. Your changes remain unsaved in this window.".into());
                    break;
                }
                _ => unreachable!(),
            }
        }
    }
    fn request_quit(&mut self, cx: &mut gpui::Context<Self>) {
        if self.deletion.executing() {
            // Keep the request: the drain loop re-enters once the report arrives.
            if self.quit == QuitState::Open {
                self.quit = QuitState::Draining;
            }
            self.status = "Finishing confirmed file deletion before closing…".into();
            cx.notify();
            return;
        }
        self.deletion.cancel();
        if self
            .preferences_draft
            .as_ref()
            .is_some_and(|draft| draft.applying)
        {
            self.error = Some("Wait for preferences to finish applying before closing".into());
            cx.notify();
            return;
        }
        if self.quit == QuitState::Flushing {
            return;
        }
        self.quit = QuitState::Draining;
        self.status = "Finishing decision writes…".into();
        if self.export_jobs.iter().any(|job| job.state.is_active()) {
            self.status = "Finishing exports before closing…".into();
            cx.notify();
            return;
        }
        // Imports publish verified groups atomically; let the active one finish (or
        // cancel a paused one, which rolls back only the in-flight group) before closing.
        // Cards queued behind it are not started: nothing of theirs was written.
        if !self.import_ui.queue.is_empty() {
            self.enqueue(IoCommand::IngestClearQueue);
        }
        match self.import_ui.progress.phase {
            Phase::Paused => {
                self.enqueue(IoCommand::IngestCancel);
                self.status = "Cancelling paused import before closing…".into();
                cx.notify();
                return;
            }
            phase if phase.is_active() => {
                self.status = "Finishing import before closing…".into();
                cx.notify();
                return;
            }
            _ => {}
        }
        // Refused quits ask instead of dead-ending in a toast: Retry, Quit
        // Anyway (the recovery journal restores the changes) or Cancel.
        let held_preferences = self.recovery_settings.is_some() && !self.settings_writable;
        if self.save_state() == SaveState::Failed || held_preferences {
            self.quit = QuitState::Open;
            self.open_quit_prompt(held_preferences);
            cx.notify();
            return;
        }
        self.flush_navigation();
        self.drain_settings();
        if self.awaiting > 0 || !self.outbox.is_empty() || self.prefs_sync == PrefsSync::Queued {
            cx.notify();
            return;
        }
        self.quit = QuitState::Flushing;
        let (ack, done) = crossbeam_channel::bounded(1);
        if !self.enqueue(IoCommand::Flush(ack)) {
            self.quit = QuitState::Open;
            return;
        }
        let task = cx
            .background_executor()
            .spawn(async move { done.recv_timeout(Duration::from_secs(5)) });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            if result.is_ok(){/* A save re-queued during the flush stays dirty so on_app_quit journals it. */if this.prefs_sync==PrefsSync::Writing{this.prefs_sync=PrefsSync::Durable;}cx.quit();}else{this.quit=QuitState::Open;this.error=Some("Could not confirm catalog flush. The window stays open; your decisions have not been discarded.".into());cx.notify();}
        });}).detach();
    }

    fn poll(&mut self, cx: &mut gpui::Context<Self>) {
        let mut dirty = std::mem::take(&mut self.texture_retry);
        if let Some(edit) = self.focused_edit() {
            dirty |= edit.blink();
        }
        // System contrast, text scaling and reduced motion changed (live,
        // `platform_prefs`): re-resolve the theme and the interface scale.
        #[cfg(target_os = "macos")]
        if self.platform_polled.elapsed() >= Duration::from_secs(2) {
            self.platform_polled = Instant::now();
            platform_prefs::poll_macos();
        }
        let platform = platform_prefs::revision();
        if platform != self.platform_revision {
            self.platform_revision = platform;
            self.theme_stale = true;
            self.apply_ui_scale();
            dirty = true;
        }
        while let Ok(update) = self.theme_updates.try_recv() {
            self.theme_stale = true;
            self.themes = update.themes;
            self.palette_revision = self.palette_revision.wrapping_add(1);
            self.zed_theme = update.zed_theme;
            self.theme_diagnostics = update.diagnostics;
            if let Some(theme) = self
                .themes
                .iter()
                .find(|theme| theme.name == self.settings.theme)
            {
                self.theme = theme.clone();
                self.fallback_theme = if theme.appearance == "light" {
                    Theme::light()
                } else {
                    Theme::dark()
                };
                self.theme_colors.borrow_mut().clear();
                self.palette_revision = self.palette_revision.wrapping_add(1);
            }
            dirty = true;
        }
        if self._automation_wake.is_none()
            && self
                .automation
                .as_ref()
                .is_some_and(|server| !server.requests.is_empty())
        {
            dirty = true;
        }
        for _ in 0..128 {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            dirty = true;
            match event {
                IoEvent::Ingest(event) => self.ingest_event(event),
                IoEvent::IngestDetected(path) => {
                    if self.settings.ingest_on_insert == "auto" {
                        self.import_ui.open = true;
                    }
                    // A second card is queued behind a running job by the worker;
                    // never replace a running job's progress with a placeholder.
                    // The card being copied coming back (after a pull) resumes
                    // its own job and is not scanned again.
                    let copying = self.import_ui.active()
                        && self.import_ui.progress.source.as_ref() == Some(&path);
                    let auto = self.settings.ingest_on_insert == "auto";
                    if !copying && auto && !self.import_ui.active() {
                        self.import_ui.progress = shadowroom::ingest::Progress {
                            phase: Phase::Queued,
                            source: Some(path.clone()),
                            ..Default::default()
                        };
                        self.import_ui.source = path;
                    } else if !copying && !auto {
                        self.scan_for_import(path);
                    }
                }
                IoEvent::RemoteProvider(provider) => self.media.set_remote_provider(provider),
                IoEvent::Remote(snapshot) => {
                    self.palette_revision = self.palette_revision.wrapping_add(1);
                    if snapshot.state() == HostState::Connected {
                        self.media.retry_remote_errors();
                        if self
                            .remote_hosts
                            .get(&snapshot.config.id)
                            .is_some_and(|old| old.state() == HostState::Indexing)
                        {
                            self.announce(
                                format!("{} · metadata synced", snapshot.config.display_name),
                                Tone::Success,
                            );
                        }
                    }
                    // Host failures arrive once as IoEvent::Error on each transition
                    // into failure; snapshots only carry the current state, so a
                    // dismissed error is not re-raised by every progress update.
                    self.remote_hosts
                        .insert(snapshot.config.id.clone(), snapshot);
                }
                IoEvent::RemoteRemoved(id) => {
                    self.remote_hosts.remove(&id);
                    self.palette_revision = self.palette_revision.wrapping_add(1);
                }
                IoEvent::Drives(d) => self.drives = d,
                IoEvent::Status(s) => self.status = s,
                IoEvent::Error(s) => self.error = Some(s.into()),
                IoEvent::FilesDeleted(report) => self.deletion_finished(report),
                IoEvent::Busy(b) => self.busy = b,
                IoEvent::Saved => {
                    self.awaiting = self.awaiting.saturating_sub(1);
                    for id in self.save_revisions.acknowledge(true) {
                        self.unsaved.remove(&id);
                    }
                    if self.unsaved.is_empty() {
                        let recovered = self
                            .error
                            .as_ref()
                            .is_some_and(|n| n.kind == NoticeKind::UnsavedDecisions);
                        clear_notice(&mut self.error, NoticeKind::UnsavedDecisions);
                        if recovered {
                            self.announce("Unsaved decisions are now saved", Tone::Success);
                        }
                        if self.awaiting == 0 {
                            self.feedback.saved_until = Some(Instant::now() + feedback::SAVED_TTL);
                        }
                    }
                }
                IoEvent::SaveFailed(e) => {
                    self.awaiting = self.awaiting.saturating_sub(1);
                    self.save_revisions.acknowledge(false);
                    // The toast headline is written from the kind and count;
                    // the writer's own text stays available under Details.
                    self.error = Some(Notice::new(NoticeKind::UnsavedDecisions, e));
                }
                IoEvent::PreferencesSaved(settings) => {
                    if settings == self.settings {
                        self.prefs_sync = PrefsSync::Durable;
                        self.settings_failure = None;
                        self.recovery_settings = None;
                        clear_notice(&mut self.error, NoticeKind::UnsavedPreferences);
                    }
                }
                IoEvent::PreferencesFailed(error) => {
                    // The writer keeps the payload and retries it.
                    if self.prefs_sync == PrefsSync::Durable {
                        self.prefs_sync = PrefsSync::Writing;
                    }
                    self.settings_failure = Some(error.clone());
                    self.error = Some(Notice::new(
                        NoticeKind::UnsavedPreferences,
                        format!("Preferences remain unsaved: {error}"),
                    ));
                }
                IoEvent::DuplicateScan(mut scan) => {
                    if self
                        .duplicate_scan
                        .as_ref()
                        .is_none_or(|old| old.id <= scan.id)
                    {
                        if scan.phase == ScanPhase::Completed
                            && !self.duplicate_results_valid(&scan.groups)
                        {
                            scan.phase = ScanPhase::Stale;
                            scan.groups.clear();
                            self.duplicate_ids.clear();
                            self.duplicate_lookup.clear();
                            self.duplicate_slots.clear();
                        }
                        if scan.phase == ScanPhase::Completed {
                            self.duplicate_ids = scan.groups.iter().flatten().copied().collect();
                            self.duplicate_slots = scan
                                .groups
                                .iter()
                                .enumerate()
                                .filter(|(_, ids)| ids.len() > 1)
                                .flat_map(|(g, ids)| {
                                    ids.iter()
                                        .enumerate()
                                        .map(move |(frame, &id)| (id, (g, frame, ids.len())))
                                })
                                .collect();
                            self.duplicate_lookup = scan
                                .groups
                                .iter()
                                .enumerate()
                                .flat_map(|(i, group)| group.iter().map(move |id| (*id, i)))
                                .collect();
                            let mut summary = if scan.groups.is_empty() {
                                format!(
                                    "No exact duplicates · checked {}",
                                    plural(scan.total, "photo", "photos")
                                )
                            } else {
                                format!(
                                    "{} · {}",
                                    plural(scan.groups.len(), "duplicate set", "duplicate sets"),
                                    plural(self.duplicate_ids.len(), "photo", "photos")
                                )
                            };
                            let errors = scan.errors.len() + scan.errors_omitted;
                            if errors > 0 {
                                summary
                                    .push_str(&format!(" · {}", plural(errors, "error", "errors")));
                            }
                            if scan.truncated {
                                summary.push_str(" · results capped");
                            }
                            self.announce(
                                summary,
                                if errors > 0 || scan.truncated {
                                    Tone::Warning
                                } else {
                                    Tone::Success
                                },
                            );
                        } else if scan.phase == ScanPhase::Running {
                            self.status = format!(
                                "Checking duplicates · {} of {} candidates",
                                scan.processed, scan.candidates
                            );
                        } else if scan.phase == ScanPhase::Cancelled {
                            self.announce("Duplicate scan cancelled", Tone::Info);
                        } else {
                            self.status = format!("Duplicate scan {}", scan.phase);
                            if scan.phase == ScanPhase::Failed {
                                self.error = scan.errors.first().cloned().map(Notice::from);
                            }
                        }
                        self.duplicate_scan = Some(scan);
                        self.rebuild(self.selected_id());
                    }
                }
                IoEvent::ExportJob {
                    id,
                    state,
                    completed,
                    error,
                } => {
                    let mut job = ExportJob {
                        id,
                        state,
                        completed,
                        error,
                        target: None,
                    };
                    let previous = self.export_jobs.iter().position(|job| job.id == id);
                    let finished = state == JobState::Completed
                        && previous
                            .is_none_or(|i| self.export_jobs[i].state != JobState::Completed);
                    if let Some(i) = previous {
                        job.target = self.export_jobs[i].target.take();
                    }
                    if finished {
                        self.announce_export(&job);
                    }
                    if let Some(i) = previous {
                        self.export_jobs[i] = job;
                    } else {
                        self.export_jobs.push(job);
                    }
                    while self.export_jobs.len() > 100 {
                        // Preserve every nonterminal job for the close barrier;
                        // only completed history may be retired under a flood.
                        if let Some(index) = self
                            .export_jobs
                            .iter()
                            .position(|job| !job.state.is_active())
                        {
                            self.export_jobs.remove(index);
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        self.drain_decisions();
        self.drain_settings();
        dirty |= self.ingest_tick();
        if self.recovery_pending
            && self.unsaved.is_empty()
            && self.prefs_sync == PrefsSync::Durable
            && self.recovery_settings.is_none()
        {
            match shadowroom::recovery::RecoveryJournal::clear(&self.recovery_path) {
                Ok(()) => {
                    self.recovery_pending = false;
                    self.announce(
                        "Restored unsaved changes from the previous session",
                        Tone::Success,
                    );
                }
                Err(error) => {
                    self.error = Some(format!("Could not finalize recovery: {error:#}").into())
                }
            }
        }
        if self.quit == QuitState::Draining {
            self.request_quit(cx);
        }
        dirty |= self.reconcile_quit_prompt();
        dirty |= self.feedback.expire(Instant::now());
        if self.unsaved.is_empty() {
            let photos = self
                .photo_slot
                .try_lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(delta) = photos {
                let selected = self.selected_id().or(self.settings.last_photo_id);
                // Rows equal to what the UI already shows (typically its own
                // acknowledged decisions) change nothing and skip every rebuild.
                let changed =
                    shadowroom::snapshot::apply_delta(&mut self.photos, &self.photo_index, delta);
                if self.status == "Opening local catalog…" {
                    self.status = format!("{} photos", self.photos.len());
                    dirty = true;
                }
                if changed {
                    self.analysis_cursor = 0;
                    self.photo_index = shadowroom::snapshot::positions(&self.photos);
                    self.counts = CollectionCounts::from_photos(&self.photos);
                    self.bursts = BurstIndex::new(&self.groups, &self.photos, &self.photo_index);
                    self.rebuild(selected);
                    self.refresh_folders();
                    self.ingest_scope_arrived();
                    self.invalidate_duplicate_results();
                    self.retire_obsolete_textures();
                    dirty = true;
                }
            }
        }
        let groups = self
            .group_slot
            .try_lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(groups) = groups {
            self.groups = groups;
            self.survey_cache = None;
            self.group_lookup.clear();
            for (g, ids) in self.groups.iter().enumerate() {
                for id in ids {
                    self.group_lookup.insert(*id, g);
                }
            }
            self.bursts = BurstIndex::new(&self.groups, &self.photos, &self.photo_index);
            if self.bursts_only {
                self.rebuild(self.selected_id());
            }
            dirty = true;
        }
        // Only request previews for the current viewport and its immediate neighbors.
        if self.mode != ViewMode::Grid && !self.import_ui.open {
            self.read_ahead();
        }
        let mut hashes = Vec::new();
        let mut candidates: HashSet<_> = self
            .visible_textures
            .iter()
            .filter_map(|key| self.photo_index.get(&key.0).copied())
            .collect();
        candidates.extend(
            self.analysis_pending
                .iter()
                .filter_map(|id| self.photo_index.get(id).copied()),
        );
        candidates.extend(
            (self.selected.saturating_sub(2)..(self.selected + 4).min(self.filtered.len()))
                .map(|position| self.filtered[position]),
        );
        for index in candidates {
            let photo = &self.photos[index];
            if photo.dhash.is_none() {
                if let Some(preview) = self.media.get_photo(photo, PreviewKind::Thumbnail) {
                    hashes.push((index, preview.dhash));
                }
            }
        }
        for (i, hash) in hashes {
            if self
                .io
                .try_send(IoCommand::Hash(self.photos[i].id, hash))
                .is_ok()
            {
                self.photos[i].dhash = Some(hash);
                dirty = true;
            }
        }
        self.analysis_pending.retain(|id| {
            self.photo_index
                .get(id)
                .is_some_and(|&i| self.photos[i].dhash.is_none())
        });
        if !self.zoom && matches!(self.settings.grouping_mode.as_str(), "visual" | "burst") {
            for id in self.analysis_pending.clone() {
                if let Some(&i) = self.photo_index.get(&id) {
                    let p = &self.photos[i];
                    if self.media.photo_error(p, PreviewKind::Thumbnail).is_some() {
                        self.analysis_pending.remove(&id);
                        self.analysis_errors += 1;
                    } else {
                        self.media.request_photo(p, PreviewKind::Thumbnail);
                    }
                }
            }
            let active = self.media.stats();
            if active.queued + active.active < 3 && self.analysis_pending.len() < 2 {
                for _ in 0..256 {
                    if self.analysis_cursor >= self.photos.len() {
                        break;
                    }
                    let p = &self.photos[self.analysis_cursor];
                    self.analysis_cursor += 1;
                    if !p.trashed && !p.is_remote() && p.dhash.is_none() {
                        self.analysis_pending.insert(p.id);
                        self.media.request_photo(p, PreviewKind::Thumbnail);
                        dirty = true;
                        break;
                    }
                }
            }
        }
        let stats = self.media.stats();
        let completed = stats.decoded + stats.disk_hits + stats.errors;
        if completed != self.last_media {
            self.last_media = completed;
            dirty = true;
        }
        if dirty {
            self.poll_render = true;
            cx.notify();
        } else {
            self.trace.flush();
        }
    }

    /// No background state machine needs the fast poll: every write is
    /// acknowledged, no job or decode is in flight, and analysis has finished.
    /// External events (drives, remotes, themes) still arrive on the idle poll.
    fn background_idle(&self) -> bool {
        let analysis_running = matches!(self.settings.grouping_mode.as_str(), "visual" | "burst")
            && !self.zoom
            && (!self.analysis_pending.is_empty() || self.analysis_cursor < self.photos.len());
        let media = self.media.stats();
        self.quit == QuitState::Open
            && self.save_state() != SaveState::Pending
            && self.outbox.is_empty()
            && self.prefs_sync != PrefsSync::Queued
            && !self.recovery_pending
            && !self.busy
            && !self.texture_retry
            && self.texture_inflight.is_empty()
            && !self.import_ui.open
            && !self.import_ui.scanning()
            && !self.import_ui.active()
            && self
                .duplicate_scan
                .as_ref()
                .is_none_or(|scan| !scan.phase.is_active())
            && !self.export_jobs.iter().any(|job| job.state.is_active())
            && !analysis_running
            && media.queued + media.active == 0
            // Without the wake bridge, automation requests are only seen by poll.
            && (self._automation_wake.is_some() || self.automation.is_none())
    }

    fn refresh_folders(&mut self) {
        self.folders = folder_counts(&self.photos);
    }
    /// Retire textures whose photo is gone or whose display identity changed.
    /// Looks up only the resident textures, not every catalog photo.
    fn retire_obsolete_textures(&mut self) {
        // Import previews take precedence over a catalog row with the same ID.
        let obsolete: Vec<_> = self
            .textures
            .keys()
            .filter(|key| {
                let photo = self
                    .import_ui
                    .photo_lookup
                    .get(&key.0)
                    .map(|&index| &self.import_ui.previews[index])
                    .or_else(|| {
                        self.photo_index
                            .get(&key.0)
                            .map(|&index| &self.photos[index])
                    });
                photo.is_none_or(|p| (preview_mtime(p), preview_size(p)) != (key.2, key.3))
            })
            .copied()
            .collect();
        for key in obsolete {
            if let Some(t) = self.textures.remove(&key) {
                self.texture_bytes -= t.bytes;
                self.retired.push(t);
            }
        }
    }
    fn accepts_photo(&self, p: &Photo) -> bool {
        p.rating >= self.min_rating && self.accepts_ignoring_rating(p)
    }
    fn accepts_ignoring_rating(&self, p: &Photo) -> bool {
        self.filter.accepts(p)
            && (self.mode == ViewMode::Grid
                || self.review_folder != self.scope
                || self
                    .review_subset
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&p.id)))
            && (!self.duplicates_only || self.duplicate_ids.contains(&p.id))
            && self.scope.as_ref().is_none_or(|s| p.matches_scope(s))
            && self.label_filter.is_none_or(|l| p.label == l)
            && (!self.bursts_only || self.bursts.unresolved.contains(&p.id))
    }
    fn visible_position(&self, id: i64) -> Option<usize> {
        let &index = self.photo_index.get(&id)?;
        self.filtered_positions
            .get(index)
            .copied()
            .filter(|&pos| pos != usize::MAX)
    }
    fn retain_selection(&mut self, keep: Option<i64>) {
        self.selected = retained_position(
            keep.and_then(|id| self.visible_position(id)),
            self.selected,
            self.filtered.len(),
        );
    }
    fn rebuild(&mut self, keep: Option<i64>) {
        self.survey_cache = None;
        // Reuse the allocation; positions also serve as the membership set.
        let mut filtered = std::mem::take(&mut self.filtered);
        filtered.clear();
        let mut reviewed = 0;
        // Sidebar counts follow the folder scope; the same pass collects them.
        let mut scoped = self.scope.as_ref().map(|_| CollectionCounts::default());
        let mut rated = (self.min_rating > 0).then(CollectionCounts::default);
        filtered.extend(self.photos.iter().enumerate().filter_map(|(i, p)| {
            if self.scope.as_ref().is_none_or(|s| p.matches_scope(s)) {
                if let Some(counts) = &mut scoped {
                    counts.adjust(Decision::from(p), true);
                }
                if let Some(counts) = rated.as_mut().filter(|_| p.rating >= self.min_rating) {
                    counts.adjust(Decision::from(p), true);
                }
            }
            if !self.accepts_photo(p) {
                return None;
            }
            reviewed += usize::from(!Filter::Unrated.accepts(p));
            Some(i)
        }));
        if self.duplicates_only {
            filtered.sort_by_key(|&i| {
                (
                    self.duplicate_lookup
                        .get(&self.photos[i].id)
                        .copied()
                        .unwrap_or(usize::MAX),
                    i,
                )
            });
        }
        index_filtered_positions(&mut self.filtered_positions, self.photos.len(), &filtered);
        self.scope_counts = scoped;
        self.rated_counts = rated;
        self.decision_epoch = self.decision_epoch.wrapping_add(1);
        self.filtered_reviewed = reviewed;
        self.filtered = filtered;
        self.retain_selection(keep);
        self.multi.retain(|id| {
            self.photo_index
                .get(id)
                .is_some_and(|&i| self.filtered_positions[i] != usize::MAX)
        });
        self.decision_membership_changed = false;
    }
    fn rebuild_after_decisions(&mut self, keep: Option<i64>) {
        if self.decision_membership_changed {
            self.rebuild(keep);
        } else {
            self.retain_selection(keep);
        }
    }
    // Preserve catalog order for history and API responses, without scanning the catalog.
    fn decisions_for(&self, ids: &HashSet<i64>) -> Vec<Decision> {
        let mut indices: Vec<_> = ids
            .iter()
            .filter_map(|id| self.photo_index.get(id).copied())
            .collect();
        indices.sort_unstable();
        indices
            .into_iter()
            .map(|i| Decision::from(&self.photos[i]))
            .collect()
    }
    fn regroup(&mut self) {
        self.save_settings();
        self.enqueue(IoCommand::Regroup);
        if self.settings.grouping_mode == "off" {
            self.groups.clear();
            self.survey_cache = None;
            self.group_lookup.clear();
            self.bursts = BurstIndex::default();
            self.analysis_pending.clear();
            if self.bursts_only {
                self.rebuild(self.selected_id());
            }
        } else {
            self.analysis_cursor = 0;
            self.analysis_errors = 0;
        }
    }

    fn review_group(&self, id: i64) -> Option<&Vec<i64>> {
        if self.duplicates_only {
            self.duplicate_lookup
                .get(&id)
                .and_then(|&i| self.duplicate_scan.as_ref()?.groups.get(i))
        } else {
            self.group_lookup.get(&id).and_then(|&i| self.groups.get(i))
        }
    }

    fn duplicate_results_valid(&self, groups: &[Vec<i64>]) -> bool {
        groups.iter().flatten().all(|id| {
            self.photo_index
                .get(id)
                .and_then(|&i| self.photos.get(i))
                .is_some_and(|photo| {
                    self.duplicate_sources.get(id) == Some(&SourceIdentity::from(photo))
                })
        })
    }

    fn invalidate_duplicate_results(&mut self) {
        if self.duplicate_scan.as_ref().is_some_and(|scan| {
            scan.phase == ScanPhase::Completed && !self.duplicate_results_valid(&scan.groups)
        }) {
            if let Some(scan) = &mut self.duplicate_scan {
                scan.phase = ScanPhase::Stale;
                scan.groups.clear();
            }
            self.duplicate_ids.clear();
            self.duplicate_lookup.clear();
            self.duplicate_slots.clear();
            self.announce_action(
                "Duplicate results changed · scan again",
                Tone::Warning,
                "Scan again",
                Cmd::ScanDuplicates,
                "",
            );
            self.rebuild(self.selected_id());
        }
    }
    fn request_preview(&self, photo: &Photo, kind: PreviewKind) {
        // The inspector also needs CPU preview data for histogram/details, even
        // when the corresponding GPU texture survived eviction of that data.
        let inspector_photo = self.inspector
            && !self.focus_mode
            && self.review_photo().is_some_and(|p| p.id == photo.id);
        if inspector_photo
            || (kind == PreviewKind::Thumbnail && photo.dhash.is_none())
            || !self.has_texture(photo, kind)
        {
            self.media.request_photo(photo, kind);
        }
    }
    fn read_ahead(&self) {
        for position in
            self.selected.saturating_sub(2)..(self.selected + 5).min(self.filtered.len())
        {
            let p = &self.photos[self.filtered[position]];
            self.request_preview(p, PreviewKind::Thumbnail);
        }
        let kind = if self.zoom {
            PreviewKind::Full
        } else {
            PreviewKind::Fit
        };
        let foreground = (
            self.selected_id(),
            if self.mode == ViewMode::Compare {
                self.reference
            } else {
                None
            },
            kind,
            self.settings.memory_budget_bytes(),
        );
        let retry = self.preview_foreground.replace(Some(foreground)) != Some(foreground);
        // Enqueue far-to-near, then the current frame. The queue favors the
        // newest request within a tier, so speculative work cannot jump ahead.
        let distances: &[isize] = if self.zoom { &[-1, 1] } else { &[2, -1, 1] };
        for &distance in distances {
            let delta = distance * self.navigation_direction;
            if let Some(position) = self.selected.checked_add_signed(delta) {
                if let Some(&index) = self.filtered.get(position) {
                    self.request_preview(&self.photos[index], kind);
                }
            }
        }
        if self.mode == ViewMode::Compare {
            if let Some(p) = self
                .reference
                .and_then(|id| self.photo_index.get(&id).map(|&i| &self.photos[i]))
            {
                if retry {
                    self.media.retry_photo_memory_error(p, kind);
                }
                self.request_preview(
                    p,
                    if self.zoom {
                        PreviewKind::Full
                    } else {
                        PreviewKind::Fit
                    },
                );
            }
        }
        if let Some(p) = self.selected_photo() {
            if retry {
                self.media.retry_photo_memory_error(p, kind);
            }
            self.request_preview(
                p,
                if self.zoom {
                    PreviewKind::Full
                } else {
                    PreviewKind::Fit
                },
            );
        }
    }
    /// Moves through the view; at either end a notice says so, since a held
    /// arrow or a dial detent that does nothing looks like a missed key.
    fn navigate(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        let before = self.selected;
        self.navigate_quietly(delta, cx);
        if delta == 0 || self.filtered.is_empty() || self.selected != before {
            return;
        }
        let len = self.filtered.len();
        let last = delta > 0;
        let unreviewed = len - self.filtered_reviewed.min(len);
        let mut text = if last {
            format!("Last photo · {} of {}", copy::count(len), copy::count(len))
        } else {
            format!("First photo · 1 of {}", copy::count(len))
        };
        let current_unreviewed = self
            .selected_photo()
            .is_some_and(|p| Filter::Unrated.accepts(p));
        if unreviewed > usize::from(current_unreviewed) {
            text.push_str(&format!(
                " · {} unreviewed · {} finds them",
                copy::count(unreviewed),
                shortcut(if last { "Shift ←" } else { "Shift →" })
            ));
        }
        self.announce(text, Tone::Info);
    }
    fn navigate_quietly(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        self.catalog_hover = None;
        if self.filtered.is_empty() {
            return;
        }
        self.media.cancel_pending();
        self.navigation_direction = delta.signum();
        let len = self.filtered.len();
        let mut target = self.selected.saturating_add_signed(delta).min(len - 1);
        // Compare never shows the reference against itself: step over it.
        if self.mode == ViewMode::Compare
            && delta != 0
            && target != self.selected
            && self.reference.and_then(|id| self.visible_position(id)) == Some(target)
        {
            let next = target as isize + delta.signum();
            target = if (0..len as isize).contains(&next) {
                next as usize
            } else {
                self.selected
            };
        }
        self.selected = target;
        self.multi.clear();
        if self.mode == ViewMode::Grid {
            if let Some(id) = self.selected_id() {
                self.multi.insert(id);
            }
        }
        self.selection_anchor = self.selected_id();
        self.settings.last_photo_id = self.selected_id();
        self.save_navigation();
        // The grid moves only when the selection leaves the viewport.
        self.reveal_grid_row(self.selected / self.grid_cols(), false);
        self.read_ahead();
        cx.notify();
    }
    fn select(&mut self, position: usize, extend: bool, range: bool, cx: &mut gpui::Context<Self>) {
        if position >= self.filtered.len() {
            return;
        }
        self.media.cancel_pending();
        if range {
            let anchor = self
                .selection_anchor
                .and_then(|id| self.visible_position(id))
                .unwrap_or(self.selected);
            self.selection_anchor = self.filtered.get(anchor).map(|&i| self.photos[i].id);
            if !extend {
                self.multi.clear();
            }
            for pos in anchor.min(position)..=anchor.max(position) {
                self.multi.insert(self.photos[self.filtered[pos]].id);
            }
        } else if extend {
            if self.multi.is_empty() {
                if let Some(id) = self.selected_id() {
                    self.multi.insert(id);
                }
            }
            let id = self.photos[self.filtered[position]].id;
            if !self.multi.insert(id) {
                self.multi.remove(&id);
            }
        } else {
            self.multi.clear();
            if self.mode == ViewMode::Grid {
                self.multi.insert(self.photos[self.filtered[position]].id);
            }
        }
        if !range {
            self.selection_anchor = Some(self.photos[self.filtered[position]].id);
        }
        self.reference_focus = false;
        self.selected = position;
        self.settings.last_photo_id = self.selected_id();
        self.save_navigation();
        self.read_ahead();
        cx.notify();
    }
    fn apply_decisions(&mut self, decisions: Vec<Decision>) -> bool {
        if self.recovery_pending || self.recovery_broken {
            self.error = Some(
                "Finish recovery before making new decisions. Retry Unsaved Decisions if needed."
                    .into(),
            );
            return false;
        }
        if self.quit.closing() {
            self.error =
                Some("The catalog is closing. Wait for the write barrier to finish.".into());
            return false;
        }
        if self
            .outbox
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_add(decisions.len())
            > 4_000_000
        {
            self.error =
                Some("Decision queue is full. Wait for pending writes before continuing.".into());
            return false;
        }
        // Last decision for an ID wins, as in the former map + full-library pass.
        let decisions_by_id: HashMap<_, _> = decisions.iter().map(|d| (d.id, *d)).collect();
        let mut ledger_changes = Vec::new();
        for d in decisions_by_id.values() {
            let Some(&i) = self.photo_index.get(&d.id) else {
                continue;
            };
            let before = Decision::from(&self.photos[i]);
            let accepted_before = self.accepts_photo(&self.photos[i]);
            let reviewed_before = !Filter::Unrated.accepts(&self.photos[i]);
            let mark_before = ledger::Mark::of(&self.photos[i]);
            self.counts.adjust(before, false);
            let in_scope = self
                .scope
                .as_ref()
                .is_none_or(|s| self.photos[i].matches_scope(s));
            let p = &mut self.photos[i];
            p.rating = d.rating;
            p.flag = d.flag;
            p.label = d.label;
            p.trashed = d.trashed;
            self.counts.adjust(*d, true);
            if let Some(counts) = self.scope_counts.as_mut().filter(|_| in_scope) {
                counts.adjust(before, false);
                counts.adjust(*d, true);
            }
            if let Some(counts) = self.rated_counts.as_mut().filter(|_| in_scope) {
                if before.rating >= self.min_rating {
                    counts.adjust(before, false);
                }
                if d.rating >= self.min_rating {
                    counts.adjust(*d, true);
                }
            }
            if before.trashed != d.trashed {
                adjust_folder(&mut self.folders, &p.folder_key(), !d.trashed);
            }
            if (before.flag == Flag::Pick) != (d.flag == Flag::Pick) {
                let changed = self
                    .bursts
                    .change_pick(d.id, d.flag == Flag::Pick, &self.groups);
                self.decision_membership_changed |= self.bursts_only && changed;
            }
            self.decision_membership_changed |=
                accepted_before != self.accepts_photo(&self.photos[i]);
            if self.filtered_positions[i] != usize::MAX {
                let mark_after = ledger::Mark::of(&self.photos[i]);
                if mark_after != mark_before {
                    ledger_changes.push((self.filtered_positions[i], mark_before, mark_after));
                }
                let reviewed_after = !Filter::Unrated.accepts(&self.photos[i]);
                if reviewed_before != reviewed_after {
                    if let Some((_, summary)) = &mut self.survey_cache {
                        Arc::make_mut(summary).update_reviewed(
                            self.filtered_positions[i],
                            reviewed_before,
                            reviewed_after,
                        );
                    }
                    if reviewed_after {
                        self.filtered_reviewed += 1;
                    } else {
                        self.filtered_reviewed -= 1;
                    }
                }
            }
        }
        // The cull ledger follows in place, without another pass over the view
        // (a membership change rebuilds the view, and the ledger with it).
        if let Some(((epoch, ..), ledger)) = self.ledger_cache.get_mut().as_mut()
            && *epoch == self.decision_epoch
        {
            let ledger = Rc::make_mut(ledger);
            for (position, before, after) in ledger_changes {
                ledger.update(position, before, after);
            }
            *epoch = self.decision_epoch.wrapping_add(1);
        }
        self.decision_epoch = self.decision_epoch.wrapping_add(1);
        self.save_revisions.stage(decisions.iter().map(|d| d.id));
        for d in &decisions {
            self.unsaved.insert(d.id, *d);
        }
        let updates: Vec<_> = decisions
            .into_iter()
            .map(|d| PhotoDecision {
                id: d.id,
                rating: d.rating,
                flag: d.flag,
                label: d.label,
                trashed: d.trashed,
            })
            .collect();
        self.awaiting += 1;
        self.outbox.push_back(updates);
        self.drain_decisions();
        if self.import_ui.scan.is_some() && !self.import_ui.job_here() {
            self.import_ui.invalidate_plan();
        }
        true
    }
    fn decide(
        &mut self,
        rating: Option<u8>,
        flag: Option<Flag>,
        label: Option<ColorLabel>,
        allow_advance: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        let count = self.decision_targets().len();
        if self.hold_bulk(count, rating, flag, label) {
            cx.notify();
            return;
        }
        self.decide_now(rating, flag, label, allow_advance, cx);
    }
    fn decide_now(
        &mut self,
        rating: Option<u8>,
        flag: Option<Flag>,
        label: Option<ColorLabel>,
        allow_advance: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        let ids = self.decision_targets();
        let before = self.decisions_for(&ids);
        let after: Vec<_> = before
            .iter()
            .map(|d| Decision {
                rating: rating.unwrap_or(d.rating),
                flag: flag.unwrap_or(d.flag),
                label: label.unwrap_or(d.label),
                ..*d
            })
            .collect();
        if before.is_empty() || (!allow_advance && before == after) {
            return;
        }
        if !self.apply_decisions(after.clone()) {
            cx.notify();
            return;
        }
        self.history.push(before, after.clone());
        let old = self.selected_id();
        // Keeps the selected photo when it is still visible (even if earlier
        // batch members left the view and shifted it), else holds the position
        // so the next photo slides in. Never re-pin the old index: that would
        // move the selection onto a different photo.
        self.rebuild_after_decisions(old);
        if allow_advance
            && self.settings.auto_advance
            && !self.reference_focused()
            && (self.multi.is_empty()
                || (self.multi.len() == 1 && old.is_some_and(|id| self.multi.contains(&id))))
            && self.selected_id() == old
        {
            self.navigate(1, cx);
        }
        // A batch larger than a screenful is confirmed with its count, so a
        // stray key after Select All is visible and one Undo away.
        if after.len() >= BULK_NOTICE_AT {
            self.announce_action(
                copy::batch_done(rating, flag, label, after.len()),
                Tone::Info,
                "Undo",
                Cmd::Undo,
                "⌘Z",
            );
        } else if old.is_some() && self.selected_id() != old {
            // The decided frame is no longer on screen (auto-advance, or the
            // decision filtered it out): confirm what it now carries.
            self.confirm_decision(&after, (rating, flag, label));
        }
        cx.notify();
    }

    fn trash_photos(&mut self, ids: &[i64], trashed: bool) -> std::result::Result<(), String> {
        if ids.is_empty() || ids.iter().any(|id| !self.photo_index.contains_key(id)) {
            return Err("Select known photographs before changing Trash".into());
        }
        let ids: HashSet<_> = ids.iter().copied().collect();
        let before = self.decisions_for(&ids);
        let after: Vec<_> = before.iter().map(|d| Decision { trashed, ..*d }).collect();
        if before == after {
            return Ok(());
        }
        if !self.apply_decisions(after.clone()) {
            return Err(self.error.as_ref().map_or_else(
                || "Trash change was not admitted".into(),
                |notice| notice.text.clone(),
            ));
        }
        self.history.push(before, after);
        self.rebuild_after_decisions(self.selected_id());
        Ok(())
    }

    fn start_duplicate_scan(&mut self, ids: Vec<i64>) -> std::result::Result<u64, String> {
        if self
            .duplicate_scan
            .as_ref()
            .is_some_and(|s| s.phase.is_active())
        {
            return Err("A duplicate scan is already running".into());
        }
        if ids.iter().any(|id| !self.photo_index.contains_key(id)) {
            return Err("Unknown photograph in duplicate scan".into());
        }
        for photo in self
            .photos
            .iter()
            .filter(|p| !p.trashed && (ids.is_empty() || ids.contains(&p.id)))
        {
            photo
                .require_local("Duplicate scanning")
                .map_err(|e| e.to_string())?;
        }
        let id = self.next_job_id + 1;
        let requested: HashSet<_> = ids.iter().copied().collect();
        if !self.enqueue(IoCommand::ScanDuplicates { id, ids }) {
            return Err(self.error.as_ref().map_or_else(
                || "Duplicate scan was not admitted".into(),
                |notice| notice.text.clone(),
            ));
        }
        self.next_job_id = id;
        self.duplicate_sources = self
            .photos
            .iter()
            .filter(|p| !p.trashed && (requested.is_empty() || requested.contains(&p.id)))
            .map(|p| (p.id, SourceIdentity::from(p)))
            .collect();
        self.duplicate_ids.clear();
        self.duplicate_lookup.clear();
        self.duplicate_slots.clear();
        self.rebuild(self.selected_id());
        self.duplicate_scan = Some(shadowroom::duplicates::DuplicateScanState {
            id,
            phase: ScanPhase::Queued,
            stage: ScanStage::Sizing,
            total: 0,
            candidates: 0,
            processed: 0,
            bytes_read: 0,
            cache_hits: 0,
            groups: Vec::new(),
            errors: Vec::new(),
            truncated: false,
            errors_omitted: 0,
        });
        Ok(id)
    }

    /// Picks in the current view (the title-bar Export scope).
    fn view_pick_ids(&self) -> Vec<i64> {
        self.filtered
            .iter()
            .map(|&i| &self.photos[i])
            .filter(|p| p.flag == Flag::Pick && !p.trashed)
            .map(|p| p.id)
            .collect()
    }

    fn view_pick_count(&self) -> usize {
        self.filtered
            .iter()
            .filter(|&&i| self.photos[i].flag == Flag::Pick && !self.photos[i].trashed)
            .count()
    }

    /// Export every pick in the catalog (palette): an empty scope.
    fn choose_export_all_picks(&mut self, cx: &mut gpui::Context<Self>) {
        if self.counts.picks == 0 {
            self.announce(
                "No picks in the catalog yet · press P to pick",
                Tone::Warning,
            );
            return;
        }
        self.choose_destination(Some(true), Vec::new(), cx);
    }

    fn choose_folder(&mut self, export: Option<bool>, cx: &mut gpui::Context<Self>) {
        let ids = if export == Some(true) {
            // Export picks the current view; the whole catalog is a palette command.
            let picks = self.view_pick_ids();
            if picks.is_empty() {
                self.announce("No picks in this view · press P to pick", Tone::Warning);
                return;
            }
            picks
        } else if self.multi.is_empty() {
            self.filtered.iter().map(|&i| self.photos[i].id).collect()
        } else {
            self.multi.iter().copied().collect()
        };
        self.choose_destination(export, ids, cx);
    }

    /// Queues an export chosen in a picker and remembers its folder for next time.
    fn start_export(&mut self, path: PathBuf, picks: bool, ids: Vec<i64>) {
        match self.queue_export(path.clone(), picks, ids) {
            Ok(_) => {
                self.announce("Exporting in background…", Tone::Info);
                self.settings
                    .extra
                    .insert("last_export_dir".into(), json!(path.to_string_lossy()));
                self.save_settings();
            }
            Err(error) => self.error = Some(error.into()),
        }
    }

    /// Where the next export picker starts: the last export folder, else the library.
    fn export_start(&self) -> PathBuf {
        self.settings
            .extra
            .get("last_export_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| self.settings.ingest_library_root.clone())
    }

    fn choose_destination(
        &mut self,
        export: Option<bool>,
        ids: Vec<i64>,
        cx: &mut gpui::Context<Self>,
    ) {
        if cfg!(target_os = "linux") {
            let initial = if export.is_some() {
                self.export_start()
            } else {
                self.settings.ingest_library_root.clone()
            };
            self.show_path_picker(
                folder_picker::Purpose::Workspace { export, ids },
                initial,
                cx,
            );
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                match export {
                    Some(true) if ids.is_empty() => "Export all catalog picks".into(),
                    Some(true) => format!("Export {}", plural(ids.len(), "pick", "picks")),
                    Some(false) => "Choose export destination".into(),
                    None if self.folder_intent == ingest_ui::FolderIntent::Cull => {
                        "Open folder to cull in place".into()
                    }
                    None => "Import from card or folder".into(),
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(picks) = export {
                            this.start_export(path, picks, ids);
                        } else {
                            this.status = format!("Opening {}", path.display());
                            this.open_ingest(path);
                        }
                        cx.notify();
                    });
                }
            }
            Ok(Err(e)) => {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("File picker: {e:#}").into());
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    fn execute(&mut self, cmd: Cmd, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.folder_picker.is_some()
            && !matches!(cmd, Cmd::Close | Cmd::DismissError | Cmd::Quit)
        {
            return;
        }
        if self.deletion.open()
            && !matches!(
                cmd,
                Cmd::ConfirmDelete | Cmd::CancelDelete | Cmd::Close | Cmd::DismissError | Cmd::Quit
            )
        {
            return;
        }
        self.image_context = None;
        let resize_focus = match &cmd {
            Cmd::ResizeDock(side) => Some(*side),
            _ => None,
        };
        self.reconcile_rating_filter();
        if self.quit.closing() && !matches!(cmd, Cmd::Quit | Cmd::DismissError) {
            self.error = Some("The app is finishing writes and exports before closing.".into());
            cx.notify();
            return;
        }
        if self.recovery_pending
            && !matches!(
                cmd,
                Cmd::Quit | Cmd::RetrySaves | Cmd::Palette | Cmd::Close | Cmd::DismissError
            )
        {
            self.error = Some(
                "Recovery is pending. Wait for writes or use Retry Unsaved Decisions after repair."
                    .into(),
            );
            cx.notify();
            return;
        }
        if self
            .preferences_draft
            .as_ref()
            .is_some_and(|draft| draft.applying)
        {
            cx.notify();
            return;
        }
        if let Some(draft) = &mut self.preferences_draft {
            if draft.edit(
                &cmd,
                self.import_ui
                    .scan
                    .as_ref()
                    .map(|scan| scan.volume.as_str()),
            ) {
                cx.notify();
                return;
            }
        }
        if !matches!(cmd, Cmd::RatingFilter | Cmd::Close) {
            self.rating_filter_open = false;
        }
        // A held batch decision is answered by Enter/Apply or Escape/Cancel;
        // doing anything else drops it.
        if !matches!(
            cmd,
            Cmd::ApplyBulk | Cmd::CancelBulk | Cmd::Close | Cmd::DismissError | Cmd::Quit
        ) && self.pending_bulk.take().is_some()
        {
            self.announce("Batch decision cancelled", Tone::Info);
        }
        match cmd {
            Cmd::GridSize(size) if size > 0 && self.grid_size_forced() => {
                // Nothing would change: say why instead of storing a size
                // that can't show.
                self.announce(
                    format!(
                        "Larger thumbnails need a wider catalog · hide a panel ({} or {})",
                        shortcut("⌘B"),
                        shortcut("I")
                    ),
                    Tone::Info,
                );
            }
            Cmd::GridSize(size) => {
                self.grid_size = size.min(ui::GRID_SIZES.len() - 1);
                self.settings
                    .extra
                    .insert("grid_size".into(), json!(["s", "m", "l"][self.grid_size]));
                self.save_settings();
                self.reveal_grid_row(self.selected / self.grid_cols().max(1), true);
            }
            Cmd::GridSizeStep(step) => {
                let size = self
                    .grid_size_index()
                    .saturating_add_signed(step as isize)
                    .min(ui::GRID_SIZES.len() - 1);
                self.execute(Cmd::GridSize(size), window, cx);
                return;
            }
            Cmd::SwapPanes => {
                if self.mode == ViewMode::Compare {
                    self.swap_panes(cx);
                }
            }
            Cmd::ZoomAtPointer => self.toggle_zoom_at_pointer(window),
            Cmd::PanStep(dx, dy) => {
                if !self.pan_step(f32::from(dx), f32::from(dy)) && !self.zoom {
                    self.hint_once(
                        "pan-keys",
                        "Zoom in first · Z or Space · then Alt+arrows pan",
                    );
                }
            }
            Cmd::ApplyBulk => self.apply_bulk(cx),
            Cmd::CancelBulk => {
                if self.pending_bulk.take().is_some() {
                    self.announce("Batch decision cancelled", Tone::Info);
                }
            }
            Cmd::ClearScope => {
                let id = self.selected_id();
                self.scope = None;
                self.reference_focus = false;
                self.rebuild(id);
            }
            Cmd::ClearFilters => self.clear_filters(),
            Cmd::SearchPhotos => self.open_photo_search(),
            Cmd::RevealPhoto(id) => self.reveal_photo(id, cx),
            Cmd::ScopeFolder(scope) => self.scope_folder(scope),
            Cmd::Trash(trashed) => {
                if trashed {
                    self.request_deletion(false, cx);
                    cx.notify();
                    return;
                }
                let ids: Vec<i64> = if self.multi.is_empty() {
                    self.review_photo().map(|p| p.id).into_iter().collect()
                } else {
                    self.multi.iter().copied().collect()
                };
                if let Err(error) = self.trash_photos(&ids, trashed) {
                    self.error = Some(error.into());
                }
            }
            Cmd::PermanentDelete => self.request_deletion(true, cx),
            Cmd::ConfirmDelete => self.confirm_deletion(cx),
            Cmd::CancelDelete => self.deletion.cancel(),
            Cmd::ScanDuplicates => {
                let ids: Vec<_> = self
                    .photos
                    .iter()
                    .filter(|p| {
                        !p.trashed && self.scope.as_ref().is_none_or(|s| p.matches_scope(s))
                    })
                    .map(|p| p.id)
                    .collect();
                if ids.is_empty() {
                    self.announce("No photos to scan in this view", Tone::Warning);
                    cx.notify();
                    return;
                }
                if let Err(error) = self.start_duplicate_scan(ids) {
                    self.error = Some(error.into());
                } else {
                    self.duplicate_ids.clear();
                    self.duplicate_lookup.clear();
                    self.duplicate_slots.clear();
                    self.duplicates_only = true;
                    self.label_filter = None;
                    self.bursts_only = false;
                    self.filter = Filter::All;
                    self.min_rating = 0;
                    self.reference_focus = false;
                    self.settings.filter = "all".into();
                    self.settings.min_rating = 0;
                    self.save_settings();
                    self.rebuild(None);
                }
            }
            Cmd::CancelDuplicates => {
                self.enqueue(IoCommand::CancelDuplicates);
            }
            Cmd::ShowDuplicates => {
                self.duplicates_only = true;
                self.label_filter = None;
                self.bursts_only = false;
                self.filter = Filter::All;
                self.min_rating = 0;
                self.scope = None;
                self.settings.filter = "all".into();
                self.settings.min_rating = 0;
                self.save_settings();
                self.rebuild(None);
            }
            Cmd::AddRemoteHost
                if self
                    .preferences_draft
                    .as_ref()
                    .is_some_and(|draft| draft.changed()) =>
            {
                // Like Import: never drop unapplied preference edits.
                self.palette = false;
                self.error = Some(
                    "Apply or cancel your preference changes before adding a network host".into(),
                );
            }
            Cmd::AddRemoteHost => {
                self.close_preferences();
                self.palette = false;
                self.add_host = Some(AddHostForm::new());
            }
            Cmd::SaveRemoteHost => {
                if let Some(AddHostForm { fields, .. }) = &self.add_host {
                    let mut invalid = None;
                    let alias = fields[0].trim().to_string();
                    let config = shadowroom::network::HostConfig {
                        id: alias.clone(),
                        display_name: alias.clone(),
                        ssh_alias: alias,
                        remote_command: if fields[1].trim().is_empty() {
                            "shadowroom-remote serve --stdio".into()
                        } else {
                            fields[1].clone()
                        },
                        extra_ssh_args: vec![],
                        local_program: None,
                        local_args: vec![],
                    };
                    if config.ssh_alias.is_empty()
                        || config.ssh_alias.starts_with('-')
                        || config.ssh_alias.chars().any(char::is_whitespace)
                    {
                        invalid =
                            Some("Enter an SSH alias (e.g. studio) or user@host, without spaces");
                    } else if self.remote_hosts.contains_key(&config.id) {
                        invalid =
                            Some("This host already exists; use Connect in the command palette");
                    } else if self.enqueue(IoCommand::Remote(
                        shadowroom::network::RemoteCommand::Add(config),
                    )) {
                        self.add_host = None;
                        self.announce("Checking remote agent protocol and version…", Tone::Info);
                    }
                    // Validation stays next to the field; the toast is for
                    // connection failures after the dialog closes.
                    if let (Some(message), Some(form)) = (invalid, &mut self.add_host) {
                        form.error = Some(message.into());
                        form.focus(0);
                    }
                }
            }
            Cmd::RemoteAction(host, action, root) => {
                use shadowroom::network::RemoteCommand as R;
                let command = match action.as_str() {
                    "connect" => R::Connect(host),
                    "disconnect" => R::Disconnect(host),
                    "cancel" => R::Cancel(host),
                    "index" => R::Index {
                        host,
                        root: root.unwrap_or_default(),
                    },
                    "sync" => R::Sync {
                        host,
                        root: root.unwrap_or_default(),
                    },
                    _ => return,
                };
                self.enqueue(IoCommand::Remote(command));
            }
            Cmd::Ingest(action) => self.ingest_action(&action, cx),
            Cmd::EjectDrive(path) => {
                self.enqueue(IoCommand::Eject(path));
            }
            Cmd::Import => self.import_from_card(cx),
            Cmd::OpenFolder => {
                self.folder_intent = ingest_ui::FolderIntent::Cull;
                self.choose_folder(None, cx);
            }
            Cmd::ExportPicks => self.choose_folder(Some(true), cx),
            Cmd::ExportAllPicks => self.choose_export_all_picks(cx),
            Cmd::ExportXmp => self.choose_folder(Some(false), cx),
            Cmd::Palette => {
                // The palette layers over Preferences and the import form: an
                // unapplied preferences draft and the import field's focus and
                // caret survive it (the palette has its own edit state, and
                // `focused_edit`/`ingest_key` defer to it while it is open).
                self.getting_started = false;
                self.import_ui.menu = None;
                if self.palette && self.palette_photos {
                    // From photo search, Ctrl/Cmd+K widens to everything and
                    // keeps the query.
                    self.palette_photos = false;
                } else {
                    self.palette = !self.palette;
                    self.palette_photos = false;
                    self.palette_query.clear();
                    self.palette_edit.focus("");
                }
                self.palette_cache.borrow_mut().take();
                self.palette_index = 0;
            }
            Cmd::Preferences => {
                self.getting_started = false;
                if self.preferences {
                    self.dismiss_preferences(window, cx);
                } else {
                    self.open_preferences();
                }
                self.palette = false;
            }
            Cmd::ApplyPreferences => self.apply_preferences(false, cx),
            Cmd::ApplyClosePreferences => self.apply_preferences(true, cx),
            Cmd::DiscardPreferences => self.close_preferences(),
            Cmd::KeepEditingPreferences => {
                if let Some(draft) = &mut self.preferences_draft {
                    draft.discard_prompt = false;
                }
            }
            Cmd::GettingStarted => {
                // The guide layers over Preferences; Escape returns to the
                // draft instead of discarding it.
                self.palette = false;
                self.getting_started = true;
            }
            Cmd::StartupGuide(enabled) => {
                // An explicit choice: closing the guide no longer clears it.
                self.startup_guide = false;
                self.settings.show_getting_started = enabled;
                self.save_settings();
            }
            Cmd::TrashMode(mode) => {
                self.settings.trash_mode = mode;
                self.settings.normalize();
                self.save_settings();
            }
            Cmd::Appearance => {
                self.open_preferences();
                self.preferences_section = PreferencesSection::Appearance;
                self.palette = false;
            }
            Cmd::PreferencesSection(section) => self.preferences_section = section,
            Cmd::Close => {
                if self.folder_picker.take().is_some() {
                    self.refocus_ingest_field();
                    cx.notify();
                    return;
                }
                if self.deletion.open() {
                    self.deletion.cancel();
                    cx.notify();
                    return;
                }
                if self.getting_started {
                    self.getting_started = false;
                    // The guide opened by itself once; F1 and ? keep it close.
                    if std::mem::take(&mut self.startup_guide) && self.settings.show_getting_started
                    {
                        self.settings.show_getting_started = false;
                        self.save_settings();
                        self.announce(
                            "Getting Started won't open at launch · F1 or ? shows it",
                            Tone::Info,
                        );
                    }
                    cx.notify();
                    return;
                }
                if self.rating_filter_open {
                    self.rating_filter_open = false;
                    cx.notify();
                    return;
                }
                if self.import_ui.menu.take().is_some() {
                    cx.notify();
                    return;
                }
                if !self.palette
                    && !self.preferences
                    && self.import_ui.open
                    && self.import_ui.dismiss_confirmation()
                {
                    cx.notify();
                    return;
                }
                if self.palette {
                    self.palette = false;
                } else if self.preferences {
                    self.dismiss_preferences(window, cx);
                } else if self.add_host.is_some() {
                    self.add_host = None;
                } else if self.pending_bulk.take().is_some() {
                    self.announce("Batch decision cancelled", Tone::Info);
                } else if self.import_ui.open {
                    self.cancel_ingest_resize();
                    self.import_ui.active_field = None;
                    self.import_ui.open = false;
                    self.error = None;
                } else if self.mode == ViewMode::Grid && self.multi.len() > 1 {
                    // Escape steps out of modes one at a time: a batch
                    // selection collapses to the outlined photo first…
                    self.multi.clear();
                    if let Some(id) = self.selected_id() {
                        self.multi.insert(id);
                        self.selection_anchor = Some(id);
                    }
                } else if self.zoom && self.mode != ViewMode::Grid {
                    // …then zoom returns to Fit, then Focus mode ends.
                    self.zoom = false;
                    self.read_ahead();
                } else if self.focus_mode {
                    self.focus_mode = false;
                } else {
                    self.cancel_ingest_resize();
                    self.import_ui.active_field = None;
                    self.error = None;
                }
            }
            Cmd::DismissError => self.error = None,
            Cmd::Next => self.navigate(1, cx),
            Cmd::Previous => self.navigate(-1, cx),
            Cmd::Rating(r) => self.decide(Some(r), None, None, true, cx),
            Cmd::RatingDelta(d) => {
                let rating = self
                    .review_photo()
                    .map_or(0, |p| p.rating)
                    .saturating_add_signed(d)
                    .min(5);
                self.decide(Some(rating), None, None, false, cx);
            }
            Cmd::Flag(f) => self.decide(None, Some(f), None, true, cx),
            Cmd::ResetDecision => self.decide(Some(0), Some(Flag::None), None, false, cx),
            Cmd::Label(l) => {
                // The active label's key clears it, in place: fixing a label
                // never moves on to another photo.
                let ids = self.decision_targets();
                let all_have = !ids.is_empty()
                    && l != ColorLabel::None
                    && ids.iter().all(|id| {
                        self.photo_index
                            .get(id)
                            .is_some_and(|&i| self.photos[i].label == l)
                    });
                if all_have || l == ColorLabel::None {
                    self.decide(None, None, Some(ColorLabel::None), false, cx);
                    if let Some(subject) = (ids.len() < BULK_NOTICE_AT)
                        .then(|| {
                            let first = ids.iter().next().copied();
                            first
                                .and_then(|id| self.photo_index.get(&id))
                                .map(|&i| copy::subject(ids.len(), Some(&self.photos[i].filename)))
                        })
                        .flatten()
                    {
                        self.announce(format!("Label cleared · {subject}"), Tone::Info);
                    }
                } else {
                    self.decide(None, None, Some(l), true, cx);
                }
            }
            Cmd::Undo => {
                if let Some(d) = self.history.undo() {
                    let id = d.first().map(|d| d.id);
                    let subject = self.decisions_subject(&d);
                    if self.apply_decisions(d) {
                        self.rebuild_after_decisions(id);
                        self.announce_action(
                            format!("Undone · {subject}"),
                            Tone::Info,
                            "Redo",
                            Cmd::Redo,
                            "⌘⇧Z",
                        );
                    } else {
                        self.history.redo();
                    }
                }
            }
            Cmd::Redo => {
                if let Some(d) = self.history.redo() {
                    let id = d.first().map(|d| d.id);
                    let subject = self.decisions_subject(&d);
                    if self.apply_decisions(d) {
                        self.rebuild_after_decisions(id);
                        self.announce_action(
                            format!("Redone · {subject}"),
                            Tone::Info,
                            "Undo",
                            Cmd::Undo,
                            "⌘Z",
                        );
                    } else {
                        self.history.undo();
                    }
                }
            }
            Cmd::View(mode) => self.change_photo_view(
                mode,
                matches!(mode, ViewMode::Compare | ViewMode::Survey),
                cx,
            ),
            Cmd::ReviewSelection => self.activate_review(cx),
            Cmd::ClearReviewSubset => {
                self.review_subset = None;
                self.rebuild(self.selected_id());
                self.read_ahead();
            }
            Cmd::Pin if self.mode == ViewMode::Compare && self.filtered.len() > 1 => {
                self.promote_candidate(cx);
            }
            Cmd::Pin => {
                self.reference = self.selected_id();
                let name = self.selected_photo().map(|p| p.filename.clone());
                self.announce(
                    match name {
                        Some(name) => format!("Reference pinned · {name} · move to a candidate"),
                        None => "Reference pinned · move to a candidate".into(),
                    },
                    Tone::Info,
                );
            }
            Cmd::Zoom => {
                self.zoom = !self.zoom;
                if self.zoom {
                    self.zoom_factor = 1.;
                }
                self.read_ahead();
            }
            Cmd::ZoomTo(factor) => self.zoom_preset(factor),
            Cmd::ZoomStep(direction) => {
                let current = if self.zoom {
                    self.zoom_factor
                } else {
                    let reference_pane = self.reference_focused();
                    self.fit_scales.get()[usize::from(!reference_pane)]
                };
                self.zoom_factor =
                    (current * if direction > 0 { 1.25 } else { 0.8 }).clamp(0.125, 8.);
                self.zoom = true;
                self.read_ahead();
            }
            Cmd::FocusPeaking => {
                self.focus_peaking = !self.focus_peaking;
                self.announce(
                    if self.focus_peaking {
                        format!(
                            "Focus peaking on · {} marks sharp edges · F toggles",
                            self.peaking_style().color.name().to_lowercase()
                        )
                    } else {
                        "Focus peaking off".into()
                    },
                    Tone::Info,
                );
            }
            Cmd::Clipping => self.toggle_clipping(),
            // Preferences-only: the open draft takes them before this match.
            Cmd::PeakingColor(_) | Cmd::PeakingSensitivity(_) => {}
            Cmd::Inspector => {
                let hidden = self.docks.inspector == 0.;
                self.inspector = self.focus_mode || !self.inspector || hidden;
                self.dock_override[1] = self.inspector && hidden;
                self.focus_mode = false;
                self.dock_priority = DockSide::Inspector;
                self.settings.show_inspector = self.inspector;
                self.save_settings();
            }
            Cmd::InspectorTab(info) => {
                self.inspector_info = info;
                self.inspector = true;
                self.dock_priority = DockSide::Inspector;
                self.focus_mode = false;
                self.settings.show_inspector = true;
                self.settings.extra.insert(
                    "inspector_tab".into(),
                    json!(if info { "info" } else { "review" }),
                );
                self.save_settings();
            }
            Cmd::RatingFilter => {
                self.rating_filter_open = !self.rating_filter_open && !self.other_overlay_open();
                self.rating_filter_index = self.min_rating;
                // The menu hangs from the view toolbar, which Focus mode hides.
                if self.rating_filter_open {
                    self.focus_mode = false;
                }
            }
            Cmd::Histogram => self.histogram = !self.histogram,
            Cmd::PreviewDetails => self.preview_details = !self.preview_details,
            Cmd::Focus => {
                self.focus_mode = !self.focus_mode;
                if self.focus_mode {
                    self.hint_once("focus-mode", "Focus mode · Esc or Shift+Tab to exit");
                }
            }
            Cmd::Sidebar => {
                let hidden = self.docks.library == 0.;
                self.sidebar = self.focus_mode || !self.sidebar || hidden;
                self.dock_override[0] = self.sidebar && hidden;
                self.focus_mode = false;
                self.dock_priority = DockSide::Library;
                self.settings.show_sidebar = self.sidebar;
                self.save_settings();
            }
            Cmd::ResizeDock(side) => {
                if self.import_ui.open && side == DockSide::Inspector {
                    self.announce(
                        "Inspector is available in Catalog, Review, Compare and Survey",
                        Tone::Warning,
                    );
                    cx.notify();
                    return;
                }
                self.focus_mode = false;
                self.dock_priority = side;
                match side {
                    DockSide::Library => {
                        self.sidebar = true;
                        self.dock_override[0] = true;
                        self.settings.show_sidebar = true;
                        window.focus(&self.library_resize_focus);
                    }
                    DockSide::Inspector => {
                        self.inspector = true;
                        self.dock_override[1] = true;
                        self.settings.show_inspector = true;
                        window.focus(&self.inspector_resize_focus);
                    }
                }
                self.save_settings();
                self.status = "Use Left/Right to resize · Shift for larger steps · Home to reset · Enter to collapse".into();
            }
            Cmd::ResetLayout => {
                self.dock_drag = None;
                self.settings.sidebar_width = LIBRARY_WIDTH as u32;
                self.settings.inspector_width = INFO_WIDTH as u32;
                self.settings.show_sidebar = true;
                self.settings.show_inspector = true;
                self.sidebar = true;
                self.inspector = true;
                self.focus_mode = false;
                self.dock_priority = DockSide::Library;
                self.save_settings();
            }
            Cmd::NextGroup | Cmd::PreviousGroup => {
                let direction = if matches!(cmd, Cmd::NextGroup) { 1 } else { -1 };
                let groups = if self.duplicates_only {
                    self.duplicate_scan
                        .as_ref()
                        .map(|s| s.groups.as_slice())
                        .unwrap_or(&[])
                } else {
                    self.groups.as_slice()
                };
                if let Some(g) = self.selected_id().and_then(|id| {
                    if self.duplicates_only {
                        self.duplicate_lookup.get(&id).copied()
                    } else {
                        self.group_lookup.get(&id).copied()
                    }
                }) {
                    let mut next = g as isize + direction;
                    let mut position = None;
                    while next >= 0 && (next as usize) < groups.len() {
                        position = groups[next as usize]
                            .iter()
                            .find_map(|id| self.visible_position(*id));
                        if position.is_some() {
                            break;
                        }
                        next += direction;
                    }
                    if let Some(pos) = position {
                        self.select(pos, false, false, cx);
                    }
                }
            }
            Cmd::Group(mode) => {
                self.settings.grouping_mode = mode;
                self.regroup();
                self.save_settings();
            }
            Cmd::Filter(f) => {
                // Collections compose with the folder scope and a label filter
                // (breadcrumb chips show and clear each one).
                self.duplicates_only = false;
                self.bursts_only = false;
                self.reference_focus = false;
                let id = self.selected_id();
                self.filter = f;
                if f == Filter::Trash {
                    self.min_rating = 0;
                    self.settings.min_rating = 0;
                }
                self.settings.filter = format!("{f:?}").to_lowercase();
                self.save_settings();
                self.rebuild(id);
            }
            Cmd::LabelFilter(label) => {
                let id = self.selected_id();
                self.label_filter = (self.label_filter != Some(label)).then_some(label);
                self.bursts_only = false;
                self.duplicates_only = false;
                self.rebuild(id);
            }
            Cmd::BurstsOnly => {
                let id = self.selected_id();
                self.bursts_only = !self.bursts_only;
                self.label_filter = None;
                self.duplicates_only = false;
                self.rebuild(id);
            }
            Cmd::RatedAtLeast(r) => {
                // Like the toolbar's rating menu: narrows what is in view
                // (folder, collection, label) and toggles off when active.
                let id = self.selected_id();
                if self.filter == Filter::Trash {
                    self.filter = Filter::All;
                    self.settings.filter = "all".into();
                }
                self.min_rating = if self.min_rating == r { 0 } else { r };
                self.settings.min_rating = self.min_rating;
                self.save_settings();
                self.rebuild(id);
            }
            Cmd::SetMinRating(r) => {
                self.rating_filter_open = false;
                let id = self.selected_id();
                self.min_rating = r;
                self.settings.min_rating = r;
                self.save_settings();
                self.rebuild(id);
            }
            Cmd::AutoAdvance => {
                self.settings.auto_advance = !self.settings.auto_advance;
                self.save_settings();
                self.announce(
                    format!(
                        "Auto-advance {} · {} toggles",
                        if self.settings.auto_advance {
                            "on"
                        } else {
                            "off"
                        },
                        shortcut("Shift A")
                    ),
                    Tone::Info,
                );
            }
            Cmd::SetAutoAdvance(enabled) => {
                self.settings.auto_advance = enabled;
                self.save_settings();
            }
            Cmd::ReduceMotion(enabled) => {
                self.settings
                    .extra
                    .insert("reduce_motion".into(), json!(enabled));
                self.save_settings();
            }
            Cmd::TextSize(percent) => {
                self.settings
                    .extra
                    .insert("text_size".into(), appearance::text_size_value(percent));
                self.save_settings();
                self.apply_ui_scale();
            }
            Cmd::SetAutoDrives(enabled) => {
                self.settings.auto_discover_drives = enabled;
                self.save_settings();
            }
            Cmd::Font(font) => {
                self.settings.interface_font = font;
                self.save_settings();
            }
            Cmd::Theme(name) => {
                if let Some(theme) = self.themes.iter().find(|t| t.name == name) {
                    self.theme = theme.clone();
                    self.fallback_theme = if theme.appearance == "light" {
                        Theme::light()
                    } else {
                        Theme::dark()
                    };
                    self.theme_colors.borrow_mut().clear();
                    self.palette_revision = self.palette_revision.wrapping_add(1);
                    self.settings.theme = name;
                    self.settings
                        .extra
                        .insert("theme_mode".into(), json!("manual"));
                    self.save_settings();
                }
            }
            Cmd::ThemeMode(mode) => {
                self.settings.extra.insert("theme_mode".into(), json!(mode));
                self.save_settings();
            }
            Cmd::SystemTheme(name) => {
                if let Some(theme) = self.themes.iter().find(|theme| theme.name == name) {
                    let slot = if theme.appearance == "light" {
                        "theme_light"
                    } else {
                        "theme_dark"
                    };
                    self.settings.extra.insert(slot.into(), json!(name));
                    self.settings
                        .extra
                        .insert("theme_mode".into(), json!("system"));
                    self.save_settings();
                }
            }
            Cmd::CompareSync => {
                self.compare_sync = !self.compare_sync;
                if self.compare_sync {
                    self.pans[0] = self.pans[1];
                }
                self.announce(
                    if self.compare_sync {
                        format!(
                            "Panes linked · pan and zoom together · {}",
                            shortcut("Shift C")
                        )
                    } else {
                        format!("Panes unlinked · {} relinks", shortcut("Shift C"))
                    },
                    Tone::Info,
                );
            }
            Cmd::Unreviewed(direction) => {
                let mut position = self.selected as isize + direction;
                while position >= 0 && (position as usize) < self.filtered.len() {
                    if Filter::Unrated.accepts(&self.photos[self.filtered[position as usize]]) {
                        self.select(position as usize, false, false, cx);
                        break;
                    }
                    position += direction;
                }
            }
            Cmd::ResolveBurst if self.mode == ViewMode::Grid => {
                self.announce(
                    format!("{} keeps one frame from Review or Survey", shortcut("⇧P")),
                    Tone::Warning,
                );
            }
            Cmd::ResolveBurst => {
                if let Some(id) = self.review_photo().map(|p| p.id) {
                    // Survey acts on what it shows (a selection or the
                    // neighbours); Review on the photo's burst.
                    let group = if self.mode == ViewMode::Survey {
                        self.survey_members()
                    } else {
                        self.group_lookup
                            .get(&id)
                            .and_then(|g| self.groups.get(*g))
                            .cloned()
                    }
                    .unwrap_or_else(|| vec![id]);
                    if group.len() < 2 {
                        self.announce("This photo isn't part of a burst", Tone::Warning);
                        cx.notify();
                        return;
                    }
                    let group: HashSet<_> = group
                        .into_iter()
                        .filter(|member| {
                            self.photo_index.get(member).is_some_and(|&i| {
                                *member == id || Filter::Unrated.accepts(&self.photos[i])
                            })
                        })
                        .collect();
                    let before = self.decisions_for(&group);
                    let after: Vec<_> = before
                        .iter()
                        .map(|d| Decision {
                            flag: if d.id == id { Flag::Pick } else { Flag::Reject },
                            ..*d
                        })
                        .collect();
                    let rejected = before
                        .iter()
                        .filter(|d| d.id != id && d.flag != Flag::Reject)
                        .count();
                    if self.apply_decisions(after.clone()) {
                        self.history.push(before, after);
                        self.rebuild_after_decisions(Some(id));
                        let kept = self
                            .photo_index
                            .get(&id)
                            .map_or_else(String::new, |&i| self.photos[i].filename.clone());
                        self.announce_action(
                            format!(
                                "Kept {kept} · rejected {} · decided frames unchanged",
                                plural(rejected, "other frame", "other frames")
                            ),
                            Tone::Info,
                            "Undo",
                            Cmd::Undo,
                            "⌘Z",
                        );
                    }
                }
            }
            Cmd::ImportTheme => self.choose_preference_theme(cx),
            Cmd::Refresh => {
                self.enqueue(IoCommand::Refresh);
            }
            Cmd::Reveal => {
                if let Some(p) = self.selected_photo() {
                    if let Err(error) = p.require_local("Reveal in Files") {
                        self.error = Some(error.to_string().into());
                    } else {
                        cx.reveal_path(&p.path);
                    }
                }
            }
            Cmd::RevealPath(path) => cx.reveal_path(&path),
            Cmd::OpenSystemTrash => {
                if cfg!(target_os = "macos") {
                    if let Some(home) = std::env::var_os("HOME") {
                        cx.open_with_system(&PathBuf::from(home).join(".Trash"));
                    }
                } else {
                    // GIO's trash: location, which xdg-open hands to the
                    // desktop's file manager.
                    cx.open_with_system(Path::new("trash:///"));
                }
            }
            Cmd::AllSelect => {
                self.multi = self.filtered.iter().map(|&i| self.photos[i].id).collect();
            }
            Cmd::Gap(g) => {
                self.settings.burst_gap_ms = g;
                self.regroup();
                self.save_settings();
            }
            Cmd::Threshold(h) => {
                self.settings.visual_threshold = h;
                self.regroup();
                self.save_settings();
            }
            Cmd::HistogramLog | Cmd::SetHistogramLog(_) => {
                let log = match cmd {
                    Cmd::SetHistogramLog(log) => log,
                    _ => !self.histogram_log,
                };
                if log != self.histogram_log {
                    self.histogram_log = log;
                    self.settings
                        .extra
                        .insert("histogram_log".into(), json!(self.histogram_log));
                    self.save_settings();
                }
            }
            Cmd::Memory(mb) => {
                self.settings.memory_budget_mb = mb;
                self.media
                    .set_memory_budget(self.settings.memory_budget_bytes());
                self.save_settings();
            }
            Cmd::Disk(mb) => {
                self.settings.cache_budget_mb = mb;
                self.media
                    .set_disk_budget(self.settings.cache_budget_bytes() as u64);
                self.save_settings();
            }
            Cmd::Controls => cx.open_url("https://worklouder.cc/input/"),
            Cmd::RetrySaves => {
                if self.recovery_settings.is_some() && !self.settings_writable {
                    match Settings::load_default() {
                        Ok(_) => {
                            self.settings_writable = true;
                            self.settings_failure = None;
                            self.save_settings();
                        }
                        Err(error) => {
                            self.error =
                                Some(format!("Preferences still need repair: {error:#}").into())
                        }
                    }
                }
                if self.save_state() == SaveState::Failed {
                    let decisions = self.unsaved.values().copied().collect();
                    let recovering = std::mem::replace(&mut self.recovery_pending, false);
                    self.apply_decisions(decisions);
                    self.recovery_pending = recovering;
                }
            }
            Cmd::RetryPreview => {
                for photo in self.selected_photo().into_iter().chain(
                    self.reference
                        .and_then(|id| self.photo_index.get(&id).map(|&i| &self.photos[i])),
                ) {
                    self.media.retry_photo(
                        photo,
                        if self.zoom {
                            PreviewKind::Full
                        } else {
                            PreviewKind::Fit
                        },
                    );
                }
            }
            Cmd::Quit => self.request_quit(cx),
            Cmd::CancelImport => {
                self.enqueue(IoCommand::CancelImport);
            }
        }
        // A focused dialog control keeps keyboard focus (render hands it back
        // to the root once its dialog closes).
        if resize_focus.is_some() || !self.dialog_focus.borrow().owns_focus(window) {
            window.focus(match resize_focus {
                Some(DockSide::Library) => &self.library_resize_focus,
                Some(DockSide::Inspector) => &self.inspector_resize_focus,
                None => &self.focus,
            });
        }
        cx.notify();
    }

    /// The fixed commands; `palette_items` adds the rating, grouping, label,
    /// font, theme and host rows.
    fn command_items() -> Vec<PaletteItem> {
        vec![
            item(
                "Show Import Panel",
                "File",
                "",
                "drive",
                Cmd::Ingest("show".into()),
            ),
            item(
                "Import Picks from This Card",
                "File · cull before copy",
                "",
                "pick",
                Cmd::Ingest("picks".into()),
            ),
            item(
                "Import History",
                "File",
                "",
                "files",
                Cmd::Ingest("history".into()),
            ),
            item(
                "Start Import",
                "File · copy and verify the selected photos",
                "⌘Enter",
                "import",
                Cmd::Ingest("start".into()),
            ),
            item(
                "Pause or Resume Import",
                "File · the running import",
                "",
                "import",
                Cmd::Ingest("pause".into()),
            ),
            item(
                "Cancel Import",
                "File · verified photos stay imported",
                "",
                "stop",
                Cmd::Ingest("cancel_now".into()),
            ),
            item(
                "Refresh Import Plan",
                "File · recheck destination space and names",
                "",
                "refresh",
                Cmd::Ingest("plan".into()),
            ),
            item(
                copy::OPEN_FOLDER,
                "File · cull where the files are; nothing is copied",
                "⌘O",
                "folder-opened",
                Cmd::OpenFolder,
            ),
            item(
                copy::IMPORT_FROM_CARD,
                "File · copy and verify into your library",
                "⌘⇧I",
                "import",
                Cmd::Import,
            ),
            item(
                "Export Picks in This View…",
                "File · picks in the current view",
                "⇧E",
                "export",
                Cmd::ExportPicks,
            ),
            item(
                "Export All Catalog Picks…",
                "File · every pick in the catalog",
                "",
                "export",
                Cmd::ExportAllPicks,
            ),
            item(
                "Export XMP Sidecars…",
                "File · selection or current view",
                "",
                "xmp",
                Cmd::ExportXmp,
            ),
            item(reveal_label(), "File", "", "external", Cmd::Reveal),
            item("Refresh Catalog", "File", "", "refresh", Cmd::Refresh),
            item("Stop Indexing", "File", "", "stop", Cmd::CancelImport),
            item(
                "Retry Preview",
                "View · retry after reconnecting or releasing memory",
                "",
                "refresh",
                Cmd::RetryPreview,
            ),
            item("Undo", "Edit", "⌘Z", "undo", Cmd::Undo),
            item("Redo", "Edit", "⌘⇧Z", "redo", Cmd::Redo),
            item("Pick", "Review", "P", "pick", Cmd::Flag(Flag::Pick)),
            item("Reject", "Review", "X", "reject", Cmd::Flag(Flag::Reject)),
            item(
                "Clear Flag",
                "Review",
                "U",
                "unflagged",
                Cmd::Flag(Flag::None),
            ),
            item(
                copy::RESET_DECISION,
                "Review · clear stars and pick/reject without advancing; keep color label",
                "R",
                "reset",
                Cmd::ResetDecision,
            ),
            item(
                copy::MOVE_TO_TRASH,
                "Review · confirm configured Trash destination",
                "Delete",
                "trash",
                Cmd::Trash(true),
            ),
            item(
                copy::DELETE_PERMANENTLY,
                "File · confirm photo/file counts and original locations",
                "Shift Delete",
                "trash",
                Cmd::PermanentDelete,
            ),
            item(
                "Restore from Trash",
                "Review · keep ratings and flags",
                "",
                "restore",
                Cmd::Trash(false),
            ),
            item(
                "Show Trash",
                "Library",
                "",
                "trash",
                Cmd::Filter(Filter::Trash),
            ),
            item(
                "Open System Trash",
                "Library · where deleted files go",
                "",
                "trash",
                Cmd::OpenSystemTrash,
            ),
            item(
                copy::SCAN_DUPLICATES,
                "Library · exact bytes in current folder or library",
                "",
                "duplicates",
                Cmd::ScanDuplicates,
            ),
            item(
                "Show Duplicates",
                "Library · review verified matches",
                "",
                "duplicates",
                Cmd::ShowDuplicates,
            ),
            item(
                "Stop Duplicate Scan",
                "Library",
                "",
                "stop",
                Cmd::CancelDuplicates,
            ),
            item(
                "Next Unreviewed",
                "Review",
                "Shift →",
                "next",
                Cmd::Unreviewed(1),
            ),
            item(
                "Previous Unreviewed",
                "Review",
                "Shift ←",
                "previous",
                Cmd::Unreviewed(-1),
            ),
            item(
                "Toggle Auto-Advance",
                "Review",
                "Shift A",
                "auto-advance",
                Cmd::AutoAdvance,
            ),
            item("Grid View", "View", "G", "grid", Cmd::View(ViewMode::Grid)),
            item(
                "Loupe View",
                "View",
                "L",
                "loupe",
                Cmd::View(ViewMode::Loupe),
            ),
            item(
                "Compare View",
                "View",
                "C",
                "compare",
                Cmd::View(ViewMode::Compare),
            ),
            item(
                "Survey View",
                "View · selection, burst or neighbours side by side",
                "S",
                "group",
                Cmd::View(ViewMode::Survey),
            ),
            item(
                "Toggle 100% Zoom",
                "View · one image pixel per display pixel",
                "Z",
                "zoom",
                Cmd::Zoom,
            ),
            item("Zoom In", "View", "=", "zoom", Cmd::ZoomStep(1)),
            item("Zoom Out", "View", "-", "zoom-out", Cmd::ZoomStep(-1)),
            item("Zoom to Fit", "View", "⌥0", "zoom-out", Cmd::ZoomTo(0.)),
            item("Zoom to 100%", "View", "⌥1", "zoom", Cmd::ZoomTo(1.)),
            item("Zoom to 200%", "View", "⌥2", "zoom", Cmd::ZoomTo(2.)),
            item("Zoom to 400%", "View", "⌥4", "zoom", Cmd::ZoomTo(4.)),
            item(
                "Thumbnail Size: Small",
                "Catalog",
                "",
                "grid",
                Cmd::GridSize(0),
            ),
            item(
                "Thumbnail Size: Medium",
                "Catalog",
                "",
                "grid",
                Cmd::GridSize(1),
            ),
            item(
                "Thumbnail Size: Large",
                "Catalog",
                "",
                "grid",
                Cmd::GridSize(2),
            ),
            item(
                "Focus Peaking",
                &format!(
                    "View · mark sharp edges; color in {} → Culling",
                    settings_noun()
                ),
                "F",
                "peaking",
                Cmd::FocusPeaking,
            ),
            item(
                "Show Clipping",
                "View · blown highlights red, crushed shadows blue",
                "J",
                "histogram",
                Cmd::Clipping,
            ),
            item(
                "Find Photo by Filename",
                "Library · whole library; type in this palette to search too",
                "⌘F",
                "search",
                Cmd::SearchPhotos,
            ),
            item(
                copy::SHOW_FULL_FOLDER,
                "Review · leave selected subset",
                "",
                "folder",
                Cmd::ClearReviewSubset,
            ),
            item(
                "Getting Started",
                "Help · workflow, labels, cache and keyboard shortcuts",
                "F1",
                "files",
                Cmd::GettingStarted,
            ),
            item("Toggle Sidebar", "View", "⌘B", "panel-left", Cmd::Sidebar),
            item(
                "Resize Sidebar",
                "Layout · Left/Right adjust width; Enter collapses",
                "",
                "panel-left",
                Cmd::ResizeDock(DockSide::Library),
            ),
            item(
                "Resize Inspector",
                "Layout · Left/Right adjust width; Enter collapses",
                "",
                "panel-right",
                Cmd::ResizeDock(DockSide::Inspector),
            ),
            item(
                "Reset Workspace Layout",
                "Layout · restore panel widths and visibility",
                "",
                "grid",
                Cmd::ResetLayout,
            ),
            item(
                "Toggle Inspector",
                "View",
                "I",
                "panel-right",
                Cmd::Inspector,
            ),
            item("Toggle Histogram", "View", "H", "graph", Cmd::Histogram),
            item(
                "Filter by Rating…",
                "Filter",
                "",
                "filter",
                Cmd::RatingFilter,
            ),
            item(
                "Show Review Panel",
                "View",
                "",
                "pick",
                Cmd::InspectorTab(false),
            ),
            item(
                "Show Photo Info",
                "View",
                "",
                "camera",
                Cmd::InspectorTab(true),
            ),
            item(
                "Toggle Histogram Log Scale",
                "View",
                "",
                "graph",
                Cmd::HistogramLog,
            ),
            item(
                "Toggle Focus Mode",
                "View · hide side panels",
                "Shift Tab",
                "focus",
                Cmd::Focus,
            ),
            item(
                "Pin as Reference",
                "Compare · in Compare, the candidate becomes the reference",
                "B",
                "pin",
                Cmd::Pin,
            ),
            item(
                "Swap Panes",
                "Compare · exchange reference and candidate",
                "Shift B",
                "compare",
                Cmd::SwapPanes,
            ),
            item(
                "Toggle Synced Pan",
                "Compare",
                "Shift C",
                "sync-pan",
                Cmd::CompareSync,
            ),
            item(
                "Clear All Filters",
                "Filter · whole library, no folder, label or rating filter",
                "",
                "library",
                Cmd::ClearFilters,
            ),
            item(
                "Show All Photos",
                "Filter",
                "",
                "library",
                Cmd::Filter(Filter::All),
            ),
            item(
                "Show Picks",
                "Filter",
                "",
                "pick",
                Cmd::Filter(Filter::Picks),
            ),
            item(
                "Show Unreviewed",
                "Filter",
                "",
                "unflagged",
                Cmd::Filter(Filter::Unrated),
            ),
            item(
                "Show Rejects",
                "Filter",
                "",
                "reject",
                Cmd::Filter(Filter::Rejects),
            ),
            item(
                "Show Rated 3★ and Up",
                "Filter",
                "",
                "star",
                Cmd::RatedAtLeast(3),
            ),
            item(
                "Show Unresolved Bursts",
                "Filter · bursts with no pick yet",
                "",
                "group",
                Cmd::BurstsOnly,
            ),
            item("Next Burst", "Bursts", "]", "group", Cmd::NextGroup),
            item("Previous Burst", "Bursts", "[", "group", Cmd::PreviousGroup),
            item(
                "Pick Burst Winner",
                "Bursts · rejects only undecided frames",
                "Shift P",
                "group",
                Cmd::ResolveBurst,
            ),
            item(settings_noun(), "App", "⌘,", "settings", Cmd::Preferences),
            item(
                "Appearance & Themes…",
                &format!("{} · Zed themes and fonts", settings_noun()),
                "",
                "color-mode",
                Cmd::Appearance,
            ),
            item(
                "Import Zed Theme…",
                "Appearance",
                "",
                "folder-opened",
                Cmd::ImportTheme,
            ),
            item(
                "Theme Source: Manual",
                "Appearance",
                "",
                "color-mode",
                Cmd::ThemeMode("manual".into()),
            ),
            item(
                "Theme Source: System",
                "Appearance",
                "",
                "color-mode",
                Cmd::ThemeMode("system".into()),
            ),
            item(
                "Theme Source: Follow Zed",
                "Appearance",
                "",
                "color-mode",
                Cmd::ThemeMode("zed".into()),
            ),
            item("Creator Micro Setup", "App", "", "settings", Cmd::Controls),
            item(
                "Retry Unsaved Decisions",
                "App",
                "",
                "save",
                Cmd::RetrySaves,
            ),
            item(
                "Quit",
                "App · finishes pending writes first",
                "⌘Q",
                "close",
                Cmd::Quit,
            ),
        ]
    }
    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = Self::command_items();
        for rating in 0..=5 {
            items.push(item(
                &if rating == 0 {
                    "Clear Rating".to_string()
                } else {
                    format!("Rate {}", "★".repeat(rating as usize))
                },
                &if rating == 0 {
                    "Review · remove stars".to_string()
                } else {
                    format!("Review · {}", plural(rating as usize, "star", "stars"))
                },
                &rating.to_string(),
                "star",
                Cmd::Rating(rating),
            ));
        }
        for (mode, title) in [
            ("off", "Off"),
            ("time", "Time"),
            ("visual", "Visual"),
            ("burst", "Burst"),
        ] {
            items.push(item(
                &format!("Group By: {title}"),
                "Bursts",
                "",
                "group",
                Cmd::Group(mode.into()),
            ));
        }
        for label in [
            ColorLabel::None,
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ] {
            items.push(item(
                &if label == ColorLabel::None {
                    "Clear Label".to_string()
                } else {
                    format!("Label {label:?}")
                },
                "Review · the active label's key clears it",
                label_key(label),
                "symbol-color",
                Cmd::Label(label),
            ));
        }
        for &font in INTERFACE_FONTS {
            items.push(item(
                &format!("Font: {font}"),
                "Appearance",
                "",
                "settings",
                Cmd::Font(font.into()),
            ));
        }
        for theme in &self.themes {
            items.push(item(
                &format!("Theme: {}", theme.name),
                &format!("Appearance · {}", theme.appearance),
                "",
                "color-mode",
                Cmd::Theme(theme.name.clone()),
            ));
        }
        items.push(item(
            "Add host…",
            "Network · SSH alias and remote command",
            "",
            "add",
            Cmd::AddRemoteHost,
        ));
        for (id, host) in &self.remote_hosts {
            for (action, label) in [
                ("connect", "Connect"),
                ("disconnect", "Disconnect"),
                ("cancel", "Cancel"),
            ] {
                items.push(item(
                    &format!("{label}: {}", host.config.display_name),
                    &format!("Network · {}", host.state()),
                    "",
                    "drive",
                    Cmd::RemoteAction(id.clone(), action.into(), None),
                ));
            }
            for root in &host.roots {
                for (action, label) in [("index", "Index"), ("sync", "Refresh")] {
                    items.push(item(
                        &format!("{label}: {} / {}", host.config.display_name, root.label),
                        "Network root",
                        "",
                        "refresh",
                        Cmd::RemoteAction(id.clone(), action.into(), Some(root.id.clone())),
                    ));
                }
            }
        }
        // Commands that cannot apply right now are hidden rather than listed inert.
        let preview_failed = self.selected_photo().is_some_and(|p| {
            [PreviewKind::Thumbnail, PreviewKind::Fit, PreviewKind::Full]
                .into_iter()
                .any(|kind| self.media.photo_error(p, kind).is_some())
        });
        let scanning = self
            .duplicate_scan
            .as_ref()
            .is_some_and(|s| s.phase.is_active());
        let save_failed = self.recovery_broken
            || self.settings_failure.is_some()
            || self.save_state() == SaveState::Failed;
        let in_trash = self.filter == Filter::Trash;
        let grouped = self.duplicates_only || !self.groups.is_empty();
        let (import_open, importing) = (self.import_ui.open, self.import_ui.active());
        let importing_here = self.import_ui.job_here();
        items.retain(|i| match &i.command {
            Cmd::ClearReviewSubset => self.review_subset.is_some(),
            // Import controls appear only where they act: Start and Refresh on
            // the open panel, Pause and Cancel while an import runs.
            Cmd::Ingest(action) => match action.as_str() {
                "start" | "plan" => import_open && !importing_here,
                "pause" | "cancel_now" => importing,
                _ => true,
            },
            Cmd::CancelImport => self.busy,
            Cmd::RetryPreview => preview_failed,
            Cmd::CancelDuplicates => scanning,
            Cmd::RetrySaves => save_failed,
            Cmd::OpenSystemTrash => self.settings.trash_mode == "system",
            Cmd::Trash(restore) => *restore != in_trash,
            Cmd::Filter(Filter::Trash) => !in_trash,
            Cmd::NextGroup | Cmd::PreviousGroup | Cmd::ResolveBurst => grouped,
            Cmd::Group(mode) => *mode != self.settings.grouping_mode,
            _ => true,
        });
        for drive in &self.drives {
            items.push(item(
                &format!("Eject {}", drive.name),
                "File",
                "",
                "drive",
                Cmd::EjectDrive(drive.path.clone()),
            ));
        }
        items
    }
    fn run_palette_command(
        &mut self,
        cmd: Cmd,
        title: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // Photos and folders are results, not commands to offer again.
        if !matches!(cmd, Cmd::RevealPhoto(_) | Cmd::ScopeFolder(_)) {
            self.remember_command(title);
        }
        self.palette = false;
        // Preferences already open beneath the palette: reveal them rather
        // than toggling them shut and discarding the draft.
        if matches!(cmd, Cmd::Preferences) && self.preferences {
            cx.notify();
            return;
        }
        self.execute(cmd, window, cx);
    }
    fn remember_command(&mut self, title: String) {
        self.palette_recent.retain(|t| *t != title);
        self.palette_recent.insert(0, title);
        self.palette_recent.truncate(5);
    }
    fn matches(&self) -> std::cell::Ref<'_, PaletteCache> {
        // Context changes can hide commands while the palette is open. Keep this
        // cheap key separate from item construction and query scoring.
        let preview_failed = self.selected_photo().is_some_and(|p| {
            [PreviewKind::Thumbnail, PreviewKind::Fit, PreviewKind::Full]
                .into_iter()
                .any(|kind| self.media.photo_error(p, kind).is_some())
        });
        let availability = [
            self.busy,
            preview_failed,
            self.duplicate_scan
                .as_ref()
                .is_some_and(|s| s.phase.is_active()),
            self.recovery_broken
                || self.settings_failure.is_some()
                || self.save_state() == SaveState::Failed,
            self.filter == Filter::Trash,
            self.duplicates_only || !self.groups.is_empty(),
            self.recovery_pending,
            self.import_ui.open,
            self.import_ui.active(),
            self.import_ui.job_here(),
            self.review_subset.is_some(),
        ];
        let mut cell = self.palette_cache.borrow_mut();
        if cell.as_ref().is_none_or(|cache| {
            cache.availability != availability
                || cache.grouping != self.settings.grouping_mode
                || cache.revision != self.palette_revision
        }) {
            let items = self.palette_items();
            *cell = Some(PaletteCache {
                command_count: items.len(),
                items,
                availability,
                grouping: self.settings.grouping_mode.clone(),
                revision: self.palette_revision,
                ..PaletteCache::default()
            });
        }
        let cache = cell.as_mut().expect("palette cache");
        if cache.query.as_deref() != Some(self.palette_query.as_str()) {
            // Photo and folder rows belong to one query; commands persist.
            cache.items.truncate(cache.command_count);
            cache.match_query(&self.palette_query, &self.palette_recent);
            let found = self.search_items(&self.palette_query);
            cache.merge_found(found, &self.palette_query, self.palette_photos);
        }
        drop(cell);
        std::cell::Ref::map(self.palette_cache.borrow(), |cache| {
            cache.as_ref().expect("palette cache")
        })
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut gpui::Context<Self>) {
        // Quit works from every view, field, menu and dialog; request_quit still
        // waits for decision writes, exports and imports to settle.
        if is_quit_chord(&event.keystroke) {
            self.user_quit(cx);
            return;
        }
        // The quit prompt is system-modal: it sits above every overlay.
        if self.quit_prompt_key(event, window, cx) {
            return;
        }
        if self.picker_key(event, cx) {
            return;
        }
        if self.deletion_key(event, window, cx) {
            return;
        }
        self.reconcile_rating_filter();
        let key = event.keystroke.key.as_str();
        if key == "escape" && self.cancel_ingest_resize() {
            cx.notify();
            return;
        }
        if self.dock_drag.is_some() {
            if key == "escape" {
                self.cancel_dock_resize();
                cx.notify();
            }
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let primary = modifiers.platform || modifiers.control;
        if self.image_context.is_some() {
            self.image_context_key(event, window, cx);
            return;
        }
        if key == "f1" {
            self.execute(Cmd::GettingStarted, window, cx);
            return;
        }
        if primary && key == "k" {
            self.execute(Cmd::Palette, window, cx);
            return;
        }
        if primary && key == "f" && !modifiers.shift && !modifiers.alt {
            self.execute(Cmd::SearchPhotos, window, cx);
            return;
        }
        if key == "escape" {
            // Escape dismisses a showing toast first, and nothing else. In an
            // Import text field it only leaves the field; a second Escape
            // closes Import.
            if self.error.take().is_some() {
                cx.notify();
            } else if self.import_ui.open
                && self.import_ui.active_field.is_some()
                && !self.palette
                && !self.preferences
                && self.add_host.is_none()
            {
                self.import_ui.active_field = None;
                cx.notify();
            } else if self.top_overlay().is_none() && self.focused_pane() != panes::Pane::Canvas {
                // From the library or inspector, Escape returns to the photos.
                self.pane = panes::Pane::Canvas;
                cx.notify();
            } else {
                self.execute(Cmd::Close, window, cx);
            }
            return;
        }
        if self.getting_started {
            // Scrolling keys and Return; nothing reaches the views beneath.
            self.getting_started_key(event, window, cx);
            return;
        }
        if self.rating_filter_open {
            // A filtering menu owns navigation and digit keys; photo shortcuts
            // must never make review decisions behind it.
            match key {
                "up" => self.rating_filter_index = self.rating_filter_index.saturating_sub(1),
                "down" => self.rating_filter_index = (self.rating_filter_index + 1).min(5),
                "enter" => self.execute(Cmd::SetMinRating(self.rating_filter_index), window, cx),
                "0" | "1" | "2" | "3" | "4" | "5" if !primary && !modifiers.alt => {
                    self.execute(Cmd::SetMinRating(key.parse().unwrap_or(0)), window, cx);
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        if self.preferences && !self.palette {
            // Quit, F1, the palette and Esc were routed above.
            self.preferences_key(event, window, cx);
            return;
        }
        if !self.palette && self.add_host.is_none() {
            // F6 / Shift+F6 move focus between library, photos and inspector
            // (`panes.rs`); Shift+F10 or the Menu key opens the image menu
            // for the current photo, as a right-click does.
            if key == "f6" && !primary && !modifiers.alt {
                self.cycle_pane(modifiers.shift, cx);
                return;
            }
            if (key == "f10" && modifiers.shift && !primary && !modifiers.alt) || key == "menu" {
                if !self.import_ui.open {
                    self.open_image_context_from_keyboard(cx);
                }
                return;
            }
            if self.pane_key(event, window, cx) {
                return;
            }
        }
        if !self.palette && self.add_host.is_none() && self.ingest_key(event, cx) {
            return;
        }
        if self.add_host.is_some() && !self.palette {
            if key == "enter" {
                self.execute(Cmd::SaveRemoteHost, window, cx);
                return;
            }
            if let Some(form) = &mut self.add_host {
                if key == "tab" {
                    form.focus(1 - form.active);
                } else {
                    let before = form.fields[form.active].len();
                    form.edit.key(&mut form.fields[form.active], event, cx);
                    if form.active == 0 && form.fields[0].len() != before {
                        form.error = None;
                    }
                }
            }
            cx.notify();
            return;
        }
        if self.palette {
            let total = self.matches().matches.len();
            // Home/End move the caret while there is a query to edit.
            let list_ends = primary || self.palette_query.is_empty();
            match key {
                "up" | "down" | "pageup" | "pagedown" => {
                    self.palette_index = palette_step(self.palette_index, total, key);
                }
                "home" if list_ends => self.palette_index = 0,
                "end" if list_ends => self.palette_index = total.saturating_sub(1),
                "enter" => {
                    let selected = {
                        let cache = self.matches();
                        let last = cache.matches.len().saturating_sub(1);
                        cache.matches.get(self.palette_index.min(last)).map(|&i| {
                            (cache.items[i].command.clone(), cache.items[i].title.clone())
                        })
                    };
                    if let Some((cmd, title)) = selected {
                        self.run_palette_command(cmd, title, window, cx);
                    }
                }
                _ => {
                    self.palette_edit.key(&mut self.palette_query, event, cx);
                    self.palette_index = 0;
                }
            }
            cx.notify();
            return;
        }
        // A held batch decision waits for Enter (Escape cancels via Close).
        if self.bulk_key(event, window, cx) {
            return;
        }
        if key == "tab" && !modifiers.shift && self.mode == ViewMode::Compare {
            // Only toggle onto a reference pane that is actually shown.
            self.reference_focus = !self.reference_focus && self.reference_available();
            cx.notify();
            return;
        }
        // Alt/Option+arrows pan a zoomed photo; plain arrows keep changing photo.
        if modifiers.alt
            && !primary
            && self.mode != ViewMode::Grid
            && matches!(key, "up" | "down" | "left" | "right")
        {
            let (dx, dy) = match key {
                "left" => (-1, 0),
                "right" => (1, 0),
                "up" => (0, -1),
                _ => (0, 1),
            };
            self.execute(Cmd::PanStep(dx, dy), window, cx);
            return;
        }
        // Alt/Option+0/1/2/4: Fit, 100%, 200%, 400% (REV-22). Alt+digit never
        // rates: only the bare digits decide.
        if modifiers.alt && !primary && !modifiers.shift && key.len() == 1 {
            if let Some(&(_, factor)) = loupe_aids::ZOOM_PRESETS.iter().find(|(k, _)| *k == key) {
                if self.mode != ViewMode::Grid {
                    self.execute(Cmd::ZoomTo(factor), window, cx);
                }
                return;
            }
            if key.as_bytes()[0].is_ascii_digit() {
                return;
            }
        }
        if !primary && !modifiers.alt && self.jump_key(key, modifiers.shift, cx) {
            return;
        }
        if modifiers.shift
            && !primary
            && !modifiers.alt
            && self.mode == ViewMode::Grid
            && matches!(key, "up" | "down" | "left" | "right")
            && !self.filtered.is_empty()
        {
            let delta = match key {
                "down" => self.grid_cols() as isize,
                "up" => -(self.grid_cols() as isize),
                "left" => -1,
                _ => 1,
            };
            let target = self
                .selected
                .saturating_add_signed(delta)
                .min(self.filtered.len() - 1);
            self.select(target, false, true, cx);
            self.reveal_grid_row(target / self.grid_cols(), false);
            return;
        }
        if modifiers.shift && !primary {
            let command = match key {
                "delete" | "backspace" => Some(Cmd::PermanentDelete),
                "right" => Some(Cmd::Unreviewed(1)),
                "left" => Some(Cmd::Unreviewed(-1)),
                "p" => Some(Cmd::ResolveBurst),
                "b" => Some(Cmd::SwapPanes),
                "c" => Some(Cmd::CompareSync),
                // Focus mode from every view, Compare included (Tab switches panes there).
                "tab" => Some(Cmd::Focus),
                _ => None,
            };
            if let Some(command) = command {
                self.execute(command, window, cx);
                return;
            }
        }
        if let Some(command) =
            shadowroom::controls::command_for_keystroke(&event.keystroke.unparse())
        {
            use shadowroom::controls::Command as C;
            let command = match command {
                C::Previous => Cmd::Previous,
                C::Next => Cmd::Next,
                C::PreviousGroup => Cmd::PreviousGroup,
                C::NextGroup => Cmd::NextGroup,
                C::Pick => Cmd::Flag(Flag::Pick),
                C::Reject => Cmd::Flag(Flag::Reject),
                C::ClearFlag => Cmd::Flag(Flag::None),
                C::ResetDecision => Cmd::ResetDecision,
                C::SetRating(r) => Cmd::Rating(r),
                C::RatingDown => Cmd::RatingDelta(-1),
                C::RatingUp => Cmd::RatingDelta(1),
                C::Grid => Cmd::View(ViewMode::Grid),
                C::Loupe => Cmd::ReviewSelection,
                C::Compare => Cmd::View(ViewMode::Compare),
                // In Catalog, zoom keys size thumbnails and Space opens Review;
                // they never change loupe zoom behind the user's back.
                C::ToggleZoom if self.mode == ViewMode::Grid => {
                    if key == "space" {
                        Cmd::ReviewSelection
                    } else {
                        return;
                    }
                }
                C::ZoomIn if self.mode == ViewMode::Grid => Cmd::GridSizeStep(1),
                C::ZoomOut if self.mode == ViewMode::Grid => Cmd::GridSizeStep(-1),
                C::ToggleZoom => Cmd::ZoomAtPointer,
                C::ZoomIn => Cmd::ZoomStep(1),
                C::ZoomOut => Cmd::ZoomStep(-1),
                C::ToggleHistogram => Cmd::Histogram,
                C::ToggleInspector => Cmd::Inspector,
                C::ToggleAutoAdvance => Cmd::AutoAdvance,
                C::OpenFolder => Cmd::OpenFolder,
                C::Settings => Cmd::Preferences,
                C::ExportXmp => Cmd::ExportXmp,
                C::ExportPicks => Cmd::ExportPicks,
            };
            self.execute(command, window, cx);
            return;
        }
        let command = if primary {
            match key {
                "q" => Some(Cmd::Quit),
                "o" => Some(Cmd::OpenFolder),
                "i" if modifiers.shift => Some(Cmd::Import),
                "b" => Some(if modifiers.alt {
                    Cmd::Inspector
                } else {
                    Cmd::Sidebar
                }),
                "," => Some(Cmd::Preferences),
                "z" => Some(if modifiers.shift {
                    Cmd::Redo
                } else {
                    Cmd::Undo
                }),
                "a" => Some(Cmd::AllSelect),
                "e" if modifiers.shift => Some(Cmd::ExportPicks),
                _ => None,
            }
        } else {
            match key {
                "right" => Some(Cmd::Next),
                "left" => Some(Cmd::Previous),
                "down" => {
                    self.navigate(
                        if self.mode == ViewMode::Grid {
                            self.grid_cols() as isize
                        } else {
                            1
                        },
                        cx,
                    );
                    None
                }
                "up" => {
                    self.navigate(
                        if self.mode == ViewMode::Grid {
                            -(self.grid_cols() as isize)
                        } else {
                            -1
                        },
                        cx,
                    );
                    None
                }
                "0" | "1" | "2" | "3" | "4" | "5" => Some(Cmd::Rating(key.parse().unwrap_or(0))),
                "p" | "f13" => Some(Cmd::Flag(Flag::Pick)),
                "x" | "f14" => Some(Cmd::Flag(Flag::Reject)),
                "u" | "f15" => Some(Cmd::Flag(Flag::None)),
                "r" => Some(Cmd::ResetDecision),
                "f16" => Some(Cmd::RatingDelta(-1)),
                "f17" => Some(Cmd::RatingDelta(1)),
                "g" | "f18" => Some(Cmd::View(ViewMode::Grid)),
                "l" | "f19" | "enter" => Some(Cmd::ReviewSelection),
                "c" | "f20" => Some(Cmd::View(ViewMode::Compare)),
                "s" => Some(Cmd::View(ViewMode::Survey)),
                "z" | "f21" | "space" if self.mode != ViewMode::Grid => Some(Cmd::ZoomAtPointer),
                "f" => Some(Cmd::FocusPeaking),
                "j" => Some(Cmd::Clipping),
                "+" | "=" if self.mode == ViewMode::Grid => Some(Cmd::GridSizeStep(1)),
                "-" if self.mode == ViewMode::Grid => Some(Cmd::GridSizeStep(-1)),
                "+" | "=" => Some(Cmd::ZoomStep(1)),
                "-" => Some(Cmd::ZoomStep(-1)),
                "h" | "f22" => Some(Cmd::Histogram),
                "i" => Some(Cmd::Inspector),
                "tab" => Some(Cmd::Focus),
                "[" | "f23" => Some(Cmd::PreviousGroup),
                "]" | "f24" => Some(Cmd::NextGroup),
                "b" => Some(Cmd::Pin),
                // Auto-advance is Shift+A only (controls.rs): a stray bare A
                // must never change how decisions move.
                "6" => Some(Cmd::Label(ColorLabel::Red)),
                "7" => Some(Cmd::Label(ColorLabel::Yellow)),
                "8" => Some(Cmd::Label(ColorLabel::Green)),
                "9" => Some(Cmd::Label(ColorLabel::Blue)),
                "delete" | "backspace" => Some(Cmd::Trash(true)),
                _ => None,
            }
        };
        if let Some(command) = command {
            self.execute(command, window, cx);
        }
    }
}

/// Ctrl+Q (Cmd+Q on macOS) quits from every view, field, menu and dialog.
/// `key()` tests it before any other routing, and handlers on focused child
/// elements must let it bubble.
fn is_quit_chord(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    (m.control || m.platform) && !m.shift && !m.alt && keystroke.key == "q"
}
/// Whether the focused dock grip handles (and stops) this key itself.
fn grip_owns_key(keystroke: &Keystroke, overlay_open: bool) -> bool {
    let m = keystroke.modifiers;
    let palette = (m.control || m.platform) && keystroke.key == "k";
    !overlay_open && !palette && !is_quit_chord(keystroke)
}
/// Star rating text without a per-cell, per-frame allocation.
fn stars(rating: u8) -> &'static str {
    const STARS: [&str; 6] = ["", "★", "★★", "★★★", "★★★★", "★★★★★"];
    STARS[usize::from(rating.min(5))]
}
/// The one rating notation on thumbnails (grid, filmstrip, Survey): "★5".
/// Repeated stars stay for the decision bar and the loupe chip.
fn thumb_rating(rating: u8) -> &'static str {
    const COMPACT: [&str; 6] = ["", "★1", "★2", "★3", "★4", "★5"];
    COMPACT[usize::from(rating.min(5))]
}
/// Inner cell of a thumbnail badge plate (glyph or label chip); the plate
/// adds 2 px of scrim around it, keeping badges 18 px tall.
const BADGE_CELL: f32 = 14.;
/// Thumbnail-preview dimensions for a larger preview, or `None` when it is
/// already no larger (`PreviewKind::Thumbnail` decodes to a 384 px long edge).
fn thumbnail_size(width: u32, height: u32) -> Option<(u32, u32)> {
    const EDGE: u32 = 384;
    let long = width.max(height);
    (long > EDGE).then(|| {
        let scale =
            |side: u32| ((side as u64 * EDGE as u64 + long as u64 / 2) / long as u64).max(1) as u32;
        (scale(width), scale(height))
    })
}

fn preview_mtime(p: &Photo) -> i64 {
    if p.companion_path.is_some() {
        p.companion_modified_at
    } else {
        p.modified_at
    }
}
fn preview_size(p: &Photo) -> u64 {
    if let Some(source) = p.remote_source() {
        use std::hash::{Hash, Hasher};
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut fingerprint);
        (p.width, p.height, p.orientation).hash(&mut fingerprint);
        return fingerprint.finish();
    }

    if p.companion_path.is_some() {
        use std::hash::{Hash, Hasher};
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        // Keep both group revisions in the display identity even when all
        // preview tiers prefer the companion.
        (
            &p.path,
            p.modified_at,
            p.size_bytes,
            &p.companion_path,
            p.companion_modified_at,
            p.companion_size_bytes,
        )
            .hash(&mut fingerprint);
        fingerprint.finish()
    } else {
        p.size_bytes
    }
}
fn item(
    title: &str,
    detail: &str,
    shortcut: &str,
    icon: &'static str,
    command: Cmd,
) -> PaletteItem {
    PaletteItem {
        search: format!("{title} {detail}"),
        title: title.into(),
        detail: detail.into(),
        shortcut: shortcut.into(),
        icon,
        command,
    }
}

/// Theme roles for a colour label: (chrome swatch, on-photo badge, fallback).
/// Chrome swatches follow the theme; thumbnail badges sit on the photo scrim.
fn label_roles(label: ColorLabel) -> Option<(&'static str, &'static str, &'static str)> {
    match label {
        ColorLabel::None => None,
        ColorLabel::Red => Some((
            "shadowroom.label.red",
            "shadowroom.scrim.label.red",
            "#e0918a",
        )),
        ColorLabel::Yellow => Some((
            "shadowroom.label.yellow",
            "shadowroom.scrim.label.yellow",
            "#d9bd72",
        )),
        ColorLabel::Green => Some((
            "shadowroom.label.green",
            "shadowroom.scrim.label.green",
            "#86bfa8",
        )),
        ColorLabel::Blue => Some((
            "shadowroom.label.blue",
            "shadowroom.scrim.label.blue",
            "#82abe0",
        )),
        ColorLabel::Purple => Some((
            "shadowroom.label.purple",
            "shadowroom.scrim.label.purple",
            "#c497d0",
        )),
    }
}

/// Initial drawn on label swatches and badges, so labels never rely on hue
/// alone (no five-hue palette survives deuteranopia).
fn label_initial(label: ColorLabel) -> &'static str {
    match label {
        ColorLabel::None => "",
        ColorLabel::Red => "R",
        ColorLabel::Yellow => "Y",
        ColorLabel::Green => "G",
        ColorLabel::Blue => "B",
        ColorLabel::Purple => "P",
    }
}

/// Size of a photo drawn with `ObjectFit::Contain` inside a well, so overlays
/// can be pinned to the picture instead of the letterboxed slot.
fn contained(well_w: f32, well_h: f32, width: u32, height: u32) -> (f32, f32) {
    if width == 0 || height == 0 {
        return (well_w, well_h);
    }
    let scale = (well_w / width as f32).min(well_h / height as f32);
    (
        (width as f32 * scale).round().clamp(1., well_w.max(1.)),
        (height as f32 * scale).round().clamp(1., well_h.max(1.)),
    )
}

fn label_key(label: ColorLabel) -> &'static str {
    match label {
        ColorLabel::Red => "6",
        ColorLabel::Yellow => "7",
        ColorLabel::Green => "8",
        ColorLabel::Blue => "9",
        _ => "",
    }
}

fn icon(name: &'static str, color: Hsla) -> Svg {
    svg()
        .path(name)
        .size(px(16.))
        .text_color(color)
        .flex_shrink_0()
}
/// Platform spelling for shortcut hints: ⌘/⇧ glyphs on macOS, words elsewhere.
/// Survey grid for `n` frames of a typical `aspect` in a `width` × `height`
/// area: the column count whose cells fit the largest frame, and the cell
/// size, so small windows and short bursts get the biggest frames that fit.
fn survey_layout(n: usize, aspect: f32, width: f32, height: f32) -> (usize, f32, f32) {
    let n = n.max(1);
    let mut best = (1, 1., 1.);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let cell_w = ((width - space::SM * (cols - 1) as f32) / cols as f32).max(1.);
        let cell_h = ((height - space::SM * (rows - 1) as f32) / rows as f32).max(1.);
        let frame_w = cell_w.min(cell_h * aspect);
        // Fewer columns win ties, keeping rows full.
        if frame_w > best.1 + 0.5 {
            best = (cols, frame_w, frame_w / aspect);
        }
    }
    best
}
fn shortcut(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.to_owned()
    } else {
        keys.replace('⌘', "Ctrl+")
            .replace('⇧', "Shift+")
            .replace('⌥', "Alt+")
    }
}

fn collection_label(filter: Filter) -> &'static str {
    match filter {
        Filter::All => "All photos",
        Filter::Picks => "Picks",
        Filter::Unrated => "Unreviewed",
        Filter::Rejects => "Rejects",
        Filter::Trash => "Trash",
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in Files"
    }
}

/// macOS (13+) calls the window Settings; GNOME and KDE keep Preferences.
fn settings_noun() -> &'static str {
    if cfg!(target_os = "macos") {
        "Settings"
    } else {
        "Preferences"
    }
}

#[derive(Clone)]
struct Tip {
    label: SharedString,
    keys: SharedString,
    font: SharedString,
    fg: Hsla,
    muted: Hsla,
    bg: Hsla,
    border: Hsla,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .font_family(self.font.clone())
            .text_size(ui::font(11.))
            .text_color(self.fg)
            .bg(self.bg)
            .border_1()
            .border_color(self.border)
            // The shared floating-surface elevation (toast, menus).
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
            }])
            .px(px(space::SM))
            .py(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(self.label.clone())
            .when(!self.keys.is_empty(), |d| {
                d.child(div().text_color(self.muted).child(self.keys.clone()))
            })
    }
}

fn resize_edge(
    pos: Point<Pixels>,
    inset: Pixels,
    size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let top = !tiling.top && pos.y < inset;
    let bottom = !tiling.bottom && pos.y > size.height - inset;
    let left = !tiling.left && pos.x < inset;
    let right = !tiling.right && pos.x > size.width - inset;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

impl Workspace {
    fn dock_resize_grip(
        &self,
        side: DockSide,
        window: &Window,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let accent = self.accent();
        let focus = match side {
            DockSide::Library => &self.library_resize_focus,
            DockSide::Inspector => &self.inspector_resize_focus,
        };
        let group = if side == DockSide::Library {
            "catalog-grip"
        } else {
            "inspector-grip"
        };
        let active =
            focus.is_focused(window) || self.dock_drag.is_some_and(|drag| drag.side == side);
        div()
            .id(match side { DockSide::Library => "resize-catalog", DockSide::Inspector => "resize-inspector" })
            .group(group)
            .flex().items_center().justify_center()
            .track_focus(focus)
            .tab_index(0)
            .absolute().top_0().bottom_0().w(px(space::SM))
            .when(side == DockSide::Library, |s| s.left(px(self.docks.library - space::XS)))
            .when(side == DockSide::Inspector, |s| s.right(px(self.docks.inspector - space::XS)))
            .occlude()
            .cursor(CursorStyle::ResizeLeftRight)
            .child(div().w(px(space::TINY)).h_full().bg(if active { accent } else { transparent_black() }).group_hover(group, move |s| s.bg(accent)))
            .tooltip(self.tip("Drag to resize · drag inward to collapse · double-click to reset · arrows resize when focused", ""))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if this.top_overlay().is_some() || this.quit.closing() { return; }
                this.rating_filter_open = false;
                this.title_drag = false;
                this.dock_priority = side;
                this.suppress_dock_click = false;
                let width = match side { DockSide::Library => this.docks.library, DockSide::Inspector => this.docks.inspector };
                this.dock_drag = Some(DockDrag { side, x: event.position.x.into(), width, proposed: width });
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.move_dock_resize(event, cx)))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                cx.stop_propagation();
                if this.suppress_dock_click { this.suppress_dock_click = false; return; }
                if event.click_count() == 2 {
                    this.dock_drag = None;
                    match side { DockSide::Library => this.settings.sidebar_width = LIBRARY_WIDTH as u32, DockSide::Inspector => this.settings.inspector_width = INFO_WIDTH as u32 }
                    this.save_settings();
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                // Any overlay owns the keyboard, even if focus stayed on this grip;
                // global chords (quit, palette) always bubble to `key()`.
                if !grip_owns_key(&event.keystroke, this.top_overlay().is_some()) { return; }
                let key = event.keystroke.key.as_str();
                cx.stop_propagation();
                match key {
                    "left" | "right" => {
                        let step = if event.keystroke.modifiers.shift { 32 } else { 8 };
                        let sign = if (key == "right") == (side == DockSide::Library) { 1 } else { -1 };
                        let width = match side { DockSide::Library => this.docks.library, DockSide::Inspector => this.docks.inspector };
                        this.set_manual_dock_width(side, width + (step * sign) as f32);
                        this.save_navigation();
                    }
                    "home" => { match side { DockSide::Library => this.settings.sidebar_width = LIBRARY_WIDTH as u32, DockSide::Inspector => this.settings.inspector_width = INFO_WIDTH as u32 }; this.save_settings(); }
                    "enter" | "space" => { this.execute(if side == DockSide::Library { Cmd::Sidebar } else { Cmd::Inspector }, window, cx); window.focus(&this.focus); }
                    "escape" => { this.cancel_dock_resize(); window.focus(&this.focus); }
                    "tab" => { window.focus(&this.focus); }
                    _ => {}
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// A deliberate resize commits the displayed split. Retaining a larger,
    /// temporarily squeezed opposite preference here would make both dividers
    /// jump on release. Passive window fitting and no-movement clicks never commit.
    fn set_manual_dock_width(&mut self, side: DockSide, proposed: f32) {
        let minimum = if side == DockSide::Library {
            ui::LIBRARY_MIN_WIDTH
        } else {
            ui::INFO_MIN_WIDTH
        };
        let center_minimum =
            if !self.import_ui.open && matches!(self.mode, ViewMode::Compare | ViewMode::Survey) {
                560.
            } else {
                ui::CENTER_MIN_WIDTH
            };
        let current = if side == DockSide::Library {
            self.docks.library
        } else {
            self.docks.inspector
        };
        let maximum =
            (self.docks.center + current - center_minimum).clamp(minimum, ui::DOCK_MAX_WIDTH);
        let width = proposed.clamp(minimum, maximum).round() as u32;
        if self.docks.library > 0. {
            self.settings.sidebar_width = self.docks.library.round() as u32;
        }
        if self.docks.inspector > 0. {
            self.settings.inspector_width = self.docks.inspector.round() as u32;
        }
        match side {
            DockSide::Library => self.settings.sidebar_width = width,
            DockSide::Inspector => self.settings.inspector_width = width,
        }
    }

    fn cancel_dock_resize(&mut self) {
        if self.dock_drag.take().is_some() {
            // Escape cancels the pending click as well as the width proposal.
            self.suppress_dock_click = true;
        }
    }

    fn move_dock_resize(&mut self, event: &MouseMoveEvent, cx: &mut gpui::Context<Self>) {
        let Some(drag) = &mut self.dock_drag else {
            return;
        };
        if !event.dragging() {
            self.finish_dock_resize(cx);
            return;
        }
        let delta = f32::from(event.position.x) - drag.x;
        drag.proposed = (drag.width
            + if drag.side == DockSide::Library {
                delta
            } else {
                -delta
            })
        .round();
        cx.stop_propagation();
        cx.notify();
    }

    fn finish_dock_resize(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(drag) = self.dock_drag.take() else {
            return;
        };
        if (drag.proposed - drag.width).abs() < 1. {
            cx.notify();
            return;
        }
        self.suppress_dock_click = true;
        let minimum = match drag.side {
            DockSide::Library => ui::LIBRARY_MIN_WIDTH,
            DockSide::Inspector => ui::INFO_MIN_WIDTH,
        };
        if drag.proposed < minimum * 0.6 {
            match drag.side {
                DockSide::Library => {
                    self.sidebar = false;
                    self.settings.show_sidebar = false;
                }
                DockSide::Inspector => {
                    self.inspector = false;
                    self.settings.show_inspector = false;
                }
            }
        } else {
            let width = match drag.side {
                DockSide::Library => self.docks.library,
                DockSide::Inspector => self.docks.inspector,
            };
            self.set_manual_dock_width(drag.side, width);
        }
        self.save_settings();
        cx.notify();
    }

    /// Histogram plot plate: the dark plate in dark themes, white in light.
    fn histogram_plate(&self) -> Hsla {
        self.c("shadowroom.histogram.background", "#0f0f0f")
    }
    fn canvas_bg(&self) -> Hsla {
        self.c("shadowroom.canvas", "#0f0f0f")
    }
    fn hover_bg(&self) -> Hsla {
        self.c("ghost_element.hover", "#1b1b1b")
    }
    fn selected_bg(&self) -> Hsla {
        self.c("ghost_element.selected", "#232323")
    }
    fn icon_muted(&self) -> Hsla {
        self.c("icon.muted", "#8a8a8a")
    }
    fn pick_color(&self) -> Hsla {
        self.c("shadowroom.pick", "#7fbf7f")
    }
    fn reject_color(&self) -> Hsla {
        self.c("shadowroom.reject", "#d46a6a")
    }
    fn rating_color(&self) -> Hsla {
        self.c("shadowroom.rating", "#e0b341")
    }
    /// Thumbnail image well: a step between the grid background and panels.
    fn well_bg(&self) -> Hsla {
        self.c("shadowroom.well", "#161616")
    }
    /// Outline of an input with no other boundary (3:1 class): text fields
    /// and the empty label swatch. Not for groups whose parts carry their own
    /// state cue (segments, tiles, keycaps); those use `group_border`.
    fn control_border(&self) -> Hsla {
        self.c("shadowroom.control_border", "#707070")
    }
    /// Thin outline of a segmented control or keycap: the chosen segment is
    /// marked by its text-coloured underline and fill, so the box itself
    /// carries no state and stays a hairline.
    fn group_border(&self) -> Hsla {
        self.c("border.variant", "#333333")
    }
    /// Plate under every on-photo badge. Theme-independent: it sits on pixels.
    fn scrim(&self) -> Hsla {
        self.c("shadowroom.scrim", "#0f0f0fd0")
    }
    fn scrim_text(&self) -> Hsla {
        self.c("shadowroom.scrim.text", "#e6e6e6")
    }
    fn scrim_muted(&self) -> Hsla {
        self.c("shadowroom.scrim.muted", "#b8b8b8")
    }
    fn scrim_flag(&self, flag: Flag) -> Hsla {
        if flag == Flag::Pick {
            self.c("shadowroom.scrim.pick", "#7fbf7f")
        } else {
            self.c("shadowroom.scrim.reject", "#e07a7a")
        }
    }
    fn scrim_rating(&self) -> Hsla {
        self.c("shadowroom.scrim.rating", "#e0b341")
    }
    fn scrim_accent(&self) -> Hsla {
        self.c("shadowroom.scrim.accent", "#7fb3e6")
    }
    /// Chrome swatch colour for a label (inspector, sidebar).
    fn label_color(&self, label: ColorLabel) -> Option<Hsla> {
        label_roles(label).map(|(key, _, fallback)| self.c(key, fallback))
    }
    /// Badge colour for a label drawn on a photo's scrim plate.
    fn photo_label_color(&self, label: ColorLabel) -> Option<Hsla> {
        label_roles(label).map(|(_, key, fallback)| self.c(key, fallback))
    }
    /// Rejected frames recede in grid and filmstrip (Survey uses the same
    /// rule) unless the Rejects collection is the subject; the badge stays
    /// at full strength. The active frame is dimmed less so it stays legible.
    fn reject_opacity(&self, flag: Flag, active: bool) -> f32 {
        if flag != Flag::Reject || self.filter == Filter::Rejects {
            1.
        } else if active {
            0.75
        } else {
            0.45
        }
    }
    fn tip(&self, label: &str, keys: &str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let tip = Tip {
            label: label.to_owned().into(),
            keys: shortcut(keys).into(),
            font: self.settings.interface_font.clone().into(),
            fg: self.text(),
            muted: self.muted(),
            bg: self.c("elevated_surface.background", "#1b1b1b"),
            border: self.c("border.variant", "#333333"),
        };
        move |_, cx| cx.new(|_| tip.clone()).into()
    }
    fn keycap(&self, keys: &str) -> Div {
        div()
            .text_size(ui::font(ui::text::LABEL))
            .text_color(self.muted())
            .child(shortcut(keys))
    }
    /// Boxed keys, one cap per key, as Zed's palette draws bindings.
    fn keycaps(&self, keys: &str) -> Div {
        let text = shortcut(keys);
        let caps: Vec<String> = if cfg!(target_os = "macos") {
            // ⌘⇧E → ⌘ ⇧ E; "Shift →" → Shift →
            if text.contains(' ') {
                text.split(' ').map(str::to_owned).collect()
            } else {
                text.chars().map(String::from).collect()
            }
        } else {
            text.split(['+', ' '])
                .filter(|k| !k.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut row = div().flex().flex_shrink_0().gap(px(space::TINY));
        for cap in caps {
            row = row.child(
                div()
                    .min_w(px(18.))
                    .h(px(18.))
                    .px(px(space::XS))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(self.group_border())
                    .bg(self.canvas_bg())
                    .text_size(ui::font(ui::text::MICRO))
                    .text_color(self.muted())
                    .child(cap),
            );
        }
        row
    }
    /// Shared 28 px control, with an 8 px icon/label gap.
    fn button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        glyph: &'static str,
        cmd: Cmd,
        active: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        // The translucent ghost hover sits a step below the selected fill on
        // every surface (panel, dialog, canvas), so hovering a plain button
        // never outranks a toggle that is on.
        let hover = self.hover_bg();
        let icon_only = label.is_empty();
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(px(space::SM))
            .h(px(ui::CONTROL_HEIGHT))
            .when(icon_only, |s| s.w(px(ui::CONTROL_HEIGHT)))
            .when(!icon_only, |s| s.px(px(space::SM)))
            .cursor_pointer()
            .occlude()
            // Reserve the outline so keyboard selection never shifts layout.
            .border_1()
            .border_color(transparent_black())
            // Active is the toggle idiom: a quiet fill and a full-strength
            // icon. The accent box is keyboard focus's alone.
            .when(active, |s| s.bg(self.selected_bg()).text_color(self.text()))
            .when(!active, |s| s.hover(move |s| s.bg(hover)))
            .when(!glyph.is_empty(), |s| {
                s.child(icon(
                    glyph,
                    if active {
                        self.c("icon", "#b8b8b8")
                    } else {
                        self.icon_muted()
                    },
                ))
            })
            .when(!icon_only, |s| s.child(label.to_owned()))
            .on_click(cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)))
    }
    fn compact_button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        glyph: &'static str,
        cmd: Cmd,
        active: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        self.button(id, label, glyph, cmd, active, cx)
            .h(px(ui::COMPACT_CONTROL_HEIGHT))
            .when(label.is_empty(), |s| s.w(px(ui::COMPACT_CONTROL_HEIGHT)))
    }
    /// Two-state control (panels, histogram, zoom, peaking, grouping, …).
    /// On is a quiet filled state with a full-strength icon, as Zed draws dock
    /// toggles; the accent stays reserved for "where you are" (view tab,
    /// collection, current photo) and keyboard focus. Same box as `button`.
    fn toggle_button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        glyph: &'static str,
        cmd: Cmd,
        on: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let hover = self.hover_bg();
        let icon_only = label.is_empty();
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(px(space::SM))
            .h(px(ui::CONTROL_HEIGHT))
            .when(icon_only, |s| s.w(px(ui::CONTROL_HEIGHT)))
            .when(!icon_only, |s| s.px(px(space::SM)))
            .cursor_pointer()
            .occlude()
            .border_1()
            .border_color(transparent_black())
            .when(on, |s| s.bg(self.selected_bg()).text_color(self.text()))
            .when(!on, |s| s.hover(move |s| s.bg(hover)))
            .when(!glyph.is_empty(), |s| {
                s.child(icon(
                    glyph,
                    if on {
                        self.c("icon", "#b8b8b8")
                    } else {
                        self.icon_muted()
                    },
                ))
            })
            .when(!icon_only, |s| s.child(label.to_owned()))
            .on_click(cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)))
    }
    fn compact_toggle(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        glyph: &'static str,
        cmd: Cmd,
        on: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        self.toggle_button(id, label, glyph, cmd, on, cx)
            .h(px(ui::COMPACT_CONTROL_HEIGHT))
            .when(label.is_empty(), |s| s.w(px(ui::COMPACT_CONTROL_HEIGHT)))
    }
    fn control_divider(&self) -> Div {
        div()
            .w(px(1.))
            .flex_shrink_0()
            .h(px(space::LG))
            .mx(px(space::XS))
            .bg(self.border())
    }
    /// Tab in a bar: the title-bar view-tab idiom (no box; muted to text and
    /// a 2 px accent underline flush with the bar's hairline when active).
    fn panel_tab(
        &self,
        id: &'static str,
        label: &'static str,
        glyph: &'static str,
        cmd: Cmd,
        active: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let hover = self.hover_bg();
        div()
            .id(id)
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(space::SM))
            .border_b_2()
            .border_color(if active {
                self.accent()
            } else {
                transparent_black()
            })
            .text_color(if active { self.text() } else { self.muted() })
            .when(active, |s| s.font_weight(FontWeight::MEDIUM))
            .cursor_pointer()
            .occlude()
            .when(!active, |s| s.hover(move |s| s.bg(hover)))
            .child(icon(
                glyph,
                if active {
                    self.c("icon", "#b8b8b8")
                } else {
                    self.icon_muted()
                },
            ))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)))
    }
    /// Text-only segment used for view tabs and preference choices.
    fn segment(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        cmd: Cmd,
        active: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let hover = self.hover_bg();
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .h(px(ui::CONTROL_HEIGHT))
            .px(px(space::SM))
            // Explicit metrics keep the label vertically centred at every scale factor.
            .text_size(ui::font(12.))
            .line_height(px(ui::scaled(16.)))
            .overflow_hidden()
            .cursor_pointer()
            .occlude()
            .text_color(if active { self.text() } else { self.muted() })
            // A 2 px bar in the text colour marks the chosen option (not the
            // accent: a choice is state, not location); reserved on every
            // segment so selection never shifts the label.
            .border_b_2()
            .border_color(if active {
                self.text()
            } else {
                transparent_black()
            })
            .when(active, |s| {
                s.bg(self.selected_bg()).font_weight(FontWeight::MEDIUM)
            })
            .when(!active, |s| s.hover(move |s| s.bg(hover)))
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)))
    }
    fn star(
        &self,
        id: impl Into<ElementId>,
        filled: bool,
        cmd: Cmd,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let hover = self.hover_bg();
        div()
            .id(id)
            .size(px(ui::CONTROL_HEIGHT))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .occlude()
            .hover(move |s| s.bg(hover))
            .child(icon(
                if filled { "star-full" } else { "star" },
                if filled {
                    self.rating_color()
                } else {
                    self.icon_muted()
                },
            ))
            .on_click(cx.listener(move |this, _, window, cx| this.execute(cmd.clone(), window, cx)))
    }
    fn section(&self, label: &str) -> Div {
        div()
            .h(px(ui::scaled(ui::SECTION_HEIGHT)))
            .mt(px(space::LG))
            .px(px(ui::PANEL_INSET))
            .flex()
            .items_center()
            .justify_between()
            .text_size(ui::font(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.muted())
            .child(label.to_owned())
    }
    /// Display name for a library scope: "Host / root" for remote roots,
    /// otherwise the folder's own name. Never shows internal scope ids.
    fn scope_display(&self, scope: &str) -> String {
        if scope.starts_with("remote-root:") {
            // Folder keys are "remote-root:<id>/<relative folder>"; root scopes omit the suffix.
            let (root_scope, sub) = scope.split_once('/').unwrap_or((scope, ""));
            for host in self.remote_hosts.values() {
                if let Some(hello) = &host.hello {
                    if let Some(root) = host.roots.iter().find(|r| {
                        shadowroom::model::remote_root_scope(&hello.device_id, &r.id) == root_scope
                    }) {
                        let base = format!("{} / {}", host.config.display_name, root.label);
                        return match Path::new(sub).file_name() {
                            Some(leaf) => format!("{base} / {}", leaf.to_string_lossy()),
                            None => base,
                        };
                    }
                }
            }
            return "Network folder".into();
        }
        Path::new(scope)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| scope.to_owned())
    }
    fn window_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        area: WindowControlArea,
        close: bool,
    ) -> Stateful<Div> {
        let hover = if close {
            self.c("error.background", "#d46a6a33")
        } else {
            self.hover_bg()
        };
        div()
            .id(id)
            .w(px(36.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .window_control_area(area)
            .hover(move |s| s.bg(hover))
            .child(icon(glyph, self.c("icon", "#b8b8b8")))
            .tooltip(self.tip(
                match id {
                    "window-minimize" => "Minimize",
                    "window-maximize" => "Maximize or restore",
                    _ => "Close window",
                },
                "",
            ))
    }
    /// One row: platform window chrome, library name, view switcher, command hint.
    /// macOS keeps native traffic lights in a transparent titlebar; Linux draws
    /// controls only when the compositor asks for client-side decorations.
    fn title_bar(&self, window: &Window, cx: &gpui::Context<Self>) -> AnyElement {
        let mac = cfg!(target_os = "macos");
        let csd = !mac && matches!(window.window_decorations(), Decorations::Client { .. });
        let viewport = f32::from(window.viewport_size().width);
        // The redundant wordmark goes first; tab labels only when truly tight.
        let narrow = viewport < 1120.;
        let tabs_iconic = viewport < 900.;
        let mut views = div().h_full().flex().items_center().gap(px(space::XS));
        for (mode, label, glyph, key) in [
            (ViewMode::Grid, "Catalog", "grid", "G"),
            (ViewMode::Loupe, "Review", "loupe", "L"),
            (ViewMode::Compare, "Compare", "compare", "C"),
            (ViewMode::Survey, "Survey", "group", "S"),
        ] {
            // While Import replaces the centre, no view tab claims the location.
            let active = self.mode == mode && !self.import_ui.open;
            let hover = self.hover_bg();
            views = views.child(
                div()
                    .id(SharedString::from(format!(
                        "header-{}",
                        label.to_lowercase()
                    )))
                    .h_full()
                    .px(px(if tabs_iconic { space::SM } else { space::MD }))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(space::SM))
                    .border_b_2()
                    .border_color(if active {
                        self.accent()
                    } else {
                        transparent_black()
                    })
                    .text_color(if active { self.text() } else { self.muted() })
                    .when(active, |s| s.font_weight(FontWeight::MEDIUM))
                    .cursor_pointer()
                    .occlude()
                    .when(!active, |s| s.hover(move |s| s.bg(hover)))
                    .child(icon(
                        glyph,
                        if active {
                            self.c("icon", "#b8b8b8")
                        } else {
                            self.icon_muted()
                        },
                    ))
                    .when(!tabs_iconic, |s| s.child(label))
                    .tooltip(self.tip(&format!("{label} view"), key))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute(Cmd::View(mode), window, cx)
                    })),
            );
        }
        let palette_keys = if mac { "⌘K" } else { "Ctrl+K" };
        let mut bar = div()
            .id("title-bar")
            .h(px(TITLE_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .pr(px(if csd { 0. } else { ui::BAR_INSET }))
            .pl(px(if mac && !window.is_fullscreen() {
                78.
            } else {
                ui::BAR_INSET
            }))
            .bg(if window.is_window_active() {
                self.c("title_bar.background", "#181818")
            } else {
                self.c("title_bar.inactive_background", "#181818")
            })
            .border_b_1()
            .border_color(self.border())
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .py(px(ui::BAR_INSET))
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(space::SM))
                    .child(
                        self.toggle_button(
                            "title-library",
                            "",
                            "panel-left",
                            Cmd::Sidebar,
                            self.docks.library > 0.,
                            cx,
                        )
                        .tooltip(self.tip("Toggle sidebar", "⌘B")),
                    )
                    .when(!narrow, |s| {
                        s.child(
                            div()
                                .text_size(ui::font(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Shadowroom"),
                        )
                    }),
            )
            .child(views)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .py(px(ui::BAR_INSET))
                    .min_w(px(0.))
                    .items_center()
                    .justify_end()
                    .gap(px(space::SM))
                    .child(
                        self.button("palette-trigger", "", "search", Cmd::Palette, false, cx)
                            .h(px(ui::CONTROL_HEIGHT))
                            .w(px(ui::CONTROL_HEIGHT))
                            .tooltip(self.tip("Search photos, folders and commands", palette_keys)),
                    )
                    .child(
                        self.toggle_button(
                            "getting-started-trigger",
                            "?",
                            "",
                            Cmd::GettingStarted,
                            self.getting_started,
                            cx,
                        )
                        .tooltip(self.tip("Getting Started · workflow and shortcuts", "F1")),
                    )
                    .child({
                        // Export says what it will export: the picks in this view.
                        let picks = self.view_pick_count();
                        let label = format!("Export {}…", plural(picks, "pick", "picks"));
                        self.button(
                            "title-export",
                            if narrow { "" } else { &label },
                            "export",
                            Cmd::ExportPicks,
                            false,
                            cx,
                        )
                        .h(px(ui::CONTROL_HEIGHT))
                        .px(px(if narrow { 0. } else { space::MD }))
                        .bg(self.selected_bg())
                        .when(narrow, |s| s.w(px(ui::CONTROL_HEIGHT)))
                        .when(picks == 0, |s| s.opacity(0.45))
                        .tooltip(if picks == 0 {
                            self.tip(
                                "No picks in this view · press P to pick. Export All Catalog Picks is in the command palette.",
                                "",
                            )
                        } else {
                            self.tip(&format!("{label} · picks in this view"), "⇧E")
                        })
                    }),
            );
        if mac {
            bar = bar.on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            });
        } else if csd {
            bar = bar
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.title_drag = true),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.title_drag = false),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                    if this.title_drag && event.dragging() {
                        this.title_drag = false;
                        window.start_window_move();
                    }
                }))
                .on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position)
                })
                .on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                });
            let controls = window.window_controls();
            let mut buttons = div().flex().h_full().ml(px(space::XS));
            if controls.minimize {
                buttons = buttons.child(
                    self.window_button("window-minimize", "win-min", WindowControlArea::Min, false)
                        .on_click(|_, window, _| window.minimize_window()),
                );
            }
            if controls.maximize {
                buttons = buttons.child(
                    self.window_button(
                        "window-maximize",
                        if window.is_maximized() {
                            "win-restore"
                        } else {
                            "win-max"
                        },
                        WindowControlArea::Max,
                        false,
                    )
                    .on_click(|_, window, _| window.zoom_window()),
                );
            }
            buttons = buttons.child(
                self.window_button("window-close", "win-close", WindowControlArea::Close, true)
                    .on_click(cx.listener(|this, _, _, cx| this.request_quit(cx))),
            );
            bar = bar.child(buttons);
        }
        bar.into_any_element()
    }
    fn sidebar(&self, cx: &gpui::Context<Self>) -> AnyElement {
        // Counts follow the folder scope, so they agree with the breadcrumb.
        let counts = self.scope_counts.as_ref().unwrap_or(&self.counts);
        let picks = counts.picks;
        let unrated = counts.unrated;
        let rejects = counts.rejects;
        let hover = self.hover_bg();
        // While Import replaces the centre, only its source drive is current.
        let importing = self.import_ui.open;
        // F6 keyboard cursor (`panes.rs`).
        let cursor = self.library_cursor_id();
        let row = |id: ElementId,
                   glyph: &'static str,
                   label: String,
                   count: Option<usize>,
                   active: bool| {
            let active = active && (!importing || glyph == "drive");
            // One zero rule: no "0" except on the collection being viewed.
            let count = count.filter(|n| *n > 0 || active);
            let focused = cursor.as_ref() == Some(&id);
            div()
                .when(focused, |s| s.relative().child(self.library_cursor_ring()))
                .id(id)
                .h(px(ui::ROW_HEIGHT))
                .pl(px(ui::PANEL_INSET - 2.))
                .pr(px(ui::PANEL_INSET))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .cursor_pointer()
                .border_l_2()
                .border_color(if active {
                    self.accent()
                } else {
                    transparent_black()
                })
                .when(active, |s| s.bg(self.selected_bg()))
                .when(!active, |s| s.hover(move |s| s.bg(hover)))
                .child(icon(
                    glyph,
                    if active {
                        self.c("icon", "#b8b8b8")
                    } else {
                        self.icon_muted()
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_normal()
                        .line_clamp(1)
                        .when(active, |s| s.font_weight(FontWeight::MEDIUM))
                        .child(label),
                )
                .when_some(count, |s, n| {
                    s.child(
                        div()
                            .text_size(ui::font(11.))
                            .text_color(self.muted())
                            .child(copy::count(n)),
                    )
                })
        };
        let mut body = div()
            .id("library-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pb(px(ui::PANEL_INSET))
            .child(
                div()
                    .h(px(ui::CONTROL_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .p(px(ui::BAR_INSET))
                    .child(
                        self.button(
                            "catalog-add-photos",
                            copy::IMPORT_FROM_CARD,
                            "import",
                            Cmd::Import,
                            false,
                            cx,
                        )
                        .h(px(ui::CONTROL_HEIGHT))
                        .w_full()
                        .justify_start()
                        .px(px(space::SM))
                        .border_1()
                        .border_color(self.c("border.variant", "#333333"))
                        .bg(self.c("element.background", "#222222"))
                        .tooltip(self.tip("Copy and verify photos into your library", "⌘⇧I")),
                    ),
            )
            .child(self.section("Library"));
        for (n, (f, g, count)) in [
            (Filter::All, "library", counts.all),
            (Filter::Picks, "pick", picks),
            (Filter::Unrated, "unflagged", unrated),
            (Filter::Rejects, "reject", rejects),
            (Filter::Trash, "trash", counts.trash),
        ]
        .into_iter()
        .enumerate()
        {
            // Every constraint in effect marks its row (Picks and Red both).
            let active = !self.duplicates_only
                && if f == Filter::All {
                    self.filter == Filter::All
                        && self.scope.is_none()
                        && self.label_filter.is_none()
                        && !self.bursts_only
                        && self.min_rating == 0
                } else {
                    self.filter == f
                };
            // Under a rating filter: "0 of 1", what the filter lets through.
            let rated = self
                .rated_counts
                .as_ref()
                .filter(|_| f != Filter::Trash)
                .map(|rc| match f {
                    Filter::All => rc.all,
                    Filter::Picks => rc.picks,
                    Filter::Unrated => rc.unrated,
                    _ => rc.rejects,
                });
            body = body.child(
                row(
                    ("collection", n).into(),
                    g,
                    collection_label(f).into(),
                    if rated.is_some() { None } else { Some(count) },
                    active,
                )
                .when_some(rated.filter(|_| count > 0 || active), |s, rated| {
                    s.child(
                        div()
                            .text_size(ui::font(11.))
                            .text_color(self.muted())
                            .child(format!("{} of {}", copy::count(rated), copy::count(count))),
                    )
                })
                // A collection composes with the folder and label in view;
                // "All photos" means the whole library.
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(
                        if f == Filter::All {
                            Cmd::ClearFilters
                        } else {
                            Cmd::Filter(f)
                        },
                        window,
                        cx,
                    );
                })),
            );
        }
        body = body.child(
            row(
                "collection-duplicates".into(),
                "duplicates",
                "Duplicates".into(),
                // A count only means something once a scan has completed.
                self.duplicate_scan
                    .as_ref()
                    .filter(|scan| scan.phase == ScanPhase::Completed)
                    .map(|_| self.duplicate_ids.len()),
                self.duplicates_only,
            )
            .tooltip(self.tip("Review exact duplicates; scan from the command palette", ""))
            .on_click(
                cx.listener(|this, _, window, cx| this.execute(Cmd::ShowDuplicates, window, cx)),
            ),
        );
        body = body.child(self.section("Smart filters"));
        let rated = counts.rated;
        body = body.child(
            row(
                "smart-rated".into(),
                "star",
                "3 stars and up".into(),
                Some(rated),
                self.min_rating == 3 && self.filter != Filter::Trash,
            )
            .on_click(
                cx.listener(|this, _, window, cx| this.execute(Cmd::RatedAtLeast(3), window, cx)),
            ),
        );
        if !self.groups.is_empty() {
            let unresolved = self.bursts.unresolved.len();
            body = body.child(
                row(
                    "smart-bursts".into(),
                    "group",
                    "Unresolved bursts".into(),
                    Some(unresolved),
                    self.bursts_only,
                )
                .tooltip(self.tip("Bursts with no pick yet", ""))
                .on_click(
                    cx.listener(|this, _, window, cx| this.execute(Cmd::BurstsOnly, window, cx)),
                ),
            );
        }
        for (n, label) in [
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ]
        .into_iter()
        .enumerate()
        {
            let count = counts.labels[label_index(label)];
            if count == 0 && self.label_filter != Some(label) {
                continue;
            }
            let active = self.label_filter == Some(label);
            let focused = cursor.as_ref() == Some(&ElementId::from(("smart-label", n)));
            body = body.child(
                div()
                    .when(focused, |s| s.relative().child(self.library_cursor_ring()))
                    .id(("smart-label", n))
                    .h(px(ui::ROW_HEIGHT))
                    .pl(px(ui::PANEL_INSET - 2.))
                    .pr(px(ui::PANEL_INSET))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .cursor_pointer()
                    .border_l_2()
                    .border_color(if active {
                        self.accent()
                    } else {
                        transparent_black()
                    })
                    .when(active, |s| s.bg(self.selected_bg()))
                    .when(!active, |s| s.hover(move |s| s.bg(hover)))
                    .child(
                        div()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px(10.))
                                    .when_some(self.label_color(label), |s, c| s.bg(c)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .when(active, |s| s.font_weight(FontWeight::MEDIUM))
                            .child(format!("{label:?}")),
                    )
                    .child(
                        div()
                            .text_size(ui::font(11.))
                            .text_color(self.muted())
                            .child(copy::count(count)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute(Cmd::LabelFilter(label), window, cx)
                    })),
            );
        }
        body = body.child(
            self.section("Folders").child(
                self.compact_button("sidebar-open", "", "add", Cmd::OpenFolder, false, cx)
                    .tooltip(self.tip("Open Folder… · cull in place, nothing is copied", "⌘O")),
            ),
        );
        if self.folders.is_empty() {
            body = body.child(
                row(
                    "sidebar-open-empty".into(),
                    "folder-opened",
                    copy::OPEN_FOLDER.into(),
                    None,
                    false,
                )
                .text_color(self.muted())
                .on_click(
                    cx.listener(|this, _, window, cx| this.execute(Cmd::OpenFolder, window, cx)),
                ),
            );
        }
        for (n, (path, count)) in self.folders.iter().enumerate() {
            if path.starts_with("remote-root:") {
                continue;
            }
            let scope = path.clone();
            let active = self.scope.as_ref() == Some(path);
            let name = Path::new(path)
                .file_name()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            body = body.child(
                row(("folder", n).into(), "folder", name, Some(*count), active)
                    .tooltip(self.tip(path, ""))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute(Cmd::ScopeFolder(scope.clone()), window, cx);
                    })),
            );
        }
        body = body.child(
            self.section("Network").child(
                self.compact_button("network-add", "", "add", Cmd::AddRemoteHost, false, cx)
                    .tooltip(self.tip("Add SSH host…", "")),
            ),
        );
        for (host_index, (id, host)) in self.remote_hosts.iter().enumerate() {
            let color = match host.state() {
                HostState::Connected => self.c("success", "#7fbf7f"),
                HostState::Connecting | HostState::Indexing => self.c("warning", "#e0b341"),
                HostState::Error => self.c("error", "#d46a6a"),
                HostState::Offline => self.muted(),
            };
            let host_id = id.clone();
            body = body.child(
                row(
                    ("network-host", host_index).into(),
                    "drive",
                    host.config.display_name.clone(),
                    None,
                    false,
                )
                .child(div().size(px(6.)).rounded_full().bg(color))
                .tooltip(self.tip(
                    &format!(
                            "{} · {}{}",
                            host.state(),
                            host.config.ssh_alias,
                            host.error()
                                .as_ref()
                                .map(|e| format!(" · {e}"))
                                .unwrap_or_default()
                        ),
                    "",
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(
                        Cmd::RemoteAction(host_id.clone(), "connect".into(), None),
                        window,
                        cx,
                    )
                })),
            );
            if let Some(hello) = &host.hello {
                for (root_index, root) in host.roots.iter().enumerate() {
                    let scope = shadowroom::model::remote_root_scope(&hello.device_id, &root.id);
                    let active = self.scope.as_ref() == Some(&scope);
                    let identity = format!(
                        "{} · {} ({}) · {}",
                        root.path.display(),
                        root.volume_id,
                        root.identity_kind,
                        host.state()
                    );
                    body = body.child(
                        row(
                            SharedString::from(format!("network-root-{host_index}-{root_index}"))
                                .into(),
                            "folder",
                            root.label.clone(),
                            None,
                            active,
                        )
                        .pl(px(ui::PANEL_INSET + 16. + space::SM - 2.))
                        .tooltip(self.tip(&identity, ""))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.scope = Some(scope.clone());
                            this.duplicates_only = false;
                            this.label_filter = None;
                            this.bursts_only = false;
                            this.filter = Filter::All;
                            this.settings.filter = "all".into();
                            this.save_settings();
                            this.rebuild(None);
                            cx.notify();
                        })),
                    );
                }
            }
        }
        if !self.drives.is_empty() {
            body = body.child(self.section("Drives"));
        }
        for (n, drive) in self.drives.iter().enumerate() {
            let path = drive.path.clone();
            body = body.child(
                row(
                    ("drive", n).into(),
                    "drive",
                    match self.import_ui.drive_state(&drive.path) {
                        Some(state) => format!("{} · {state}", drive.name),
                        None => drive.name.clone(),
                    },
                    None,
                    importing && drive.path == self.import_ui.source,
                )
                .tooltip(self.tip("Import from drive", ""))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scan_for_import(path.clone());
                    cx.notify();
                }))
                // Eject lives on the drive's own row; occlude keeps the row click from firing.
                .child({
                    let hover = self.hover_bg();
                    let eject = drive.path.clone();
                    div()
                        .id(("eject-drive", n))
                        .size(px(ui::COMPACT_CONTROL_HEIGHT))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .occlude()
                        .cursor_pointer()
                        .text_color(self.icon_muted())
                        .hover(move |s| s.bg(hover))
                        .child(icon("eject", self.icon_muted()))
                        .tooltip(self.tip(&format!("Eject {}", drive.name), ""))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.execute(Cmd::EjectDrive(eject.clone()), window, cx);
                        }))
                }),
            );
        }
        div()
            .w(px(self.docks.library))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(self.panel())
            .border_r_1()
            .border_color(self.border())
            .child(body)
            .into_any_element()
    }
    /// Pane toolbar: breadcrumb on the left, view filters on the right.
    fn filters(&self, cx: &gpui::Context<Self>) -> AnyElement {
        // The breadcrumb names every constraint actually narrowing the view,
        // each removable on its own (CAT-04).
        // It leads with the collection as plain text: "All photos", or Trash
        // and Duplicates, which are places reached from the sidebar rather
        // than filters to clear (R2-catalog-07).
        let place = self.duplicates_only || self.filter == Filter::Trash;
        let root = if self.duplicates_only {
            "Duplicates"
        } else {
            collection_label(if place { Filter::Trash } else { Filter::All })
        };
        let constraints: Vec<_> = self
            .constraints()
            .into_iter()
            .filter(|c| !(place && c.id == "chip-collection"))
            .collect();
        let chips = constraints.len();
        let chip_width = (self.center_size.0 * 0.3).clamp(96., 200.);
        let separator = || div().flex_shrink_0().text_color(self.muted()).child("·");
        let mut crumb = div()
            .id("breadcrumb")
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center()
            .gap(px(space::XS))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(
                div()
                    .id("breadcrumb-scope")
                    .flex_shrink_0()
                    .pr(px(space::XS))
                    .child(root),
            );
        if chips > 0 {
            crumb = crumb.child(separator());
        }
        for constraint in constraints {
            crumb = crumb.child(self.constraint_chip(constraint, chip_width, cx));
        }
        if self.mode != ViewMode::Grid && self.review_folder == self.scope {
            if let Some(ids) = &self.review_subset {
                crumb = crumb.child(self.toggle_button(
                    "clear-review-subset",
                    &format!(
                        "{} selected · {}",
                        copy::count(ids.len()),
                        copy::SHOW_FULL_FOLDER
                    ),
                    "",
                    Cmd::ClearReviewSubset,
                    true,
                    cx,
                ));
            }
        }
        match (self.mode, self.selected_photo()) {
            (ViewMode::Grid, _) | (_, None) => {
                crumb = crumb.child(separator()).child(
                    div().flex_shrink_0().text_color(self.muted()).child(plural(
                        self.filtered.len(),
                        "photo",
                        "photos",
                    )),
                );
            }
            (_, Some(photo)) => {
                crumb = crumb
                    .child(
                        svg()
                            .path("chevron")
                            .size(px(12.))
                            .flex_shrink_0()
                            .text_color(self.icon_muted()),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .id("breadcrumb-filename")
                            .tooltip(self.tip(&photo.filename, ""))
                            .child(photo.filename.clone()),
                    );
            }
        }
        let filtering = self.min_rating > 0;
        let ratings = self
            .toggle_button(
                "rating-filter-menu",
                if self.compact {
                    ""
                } else if filtering {
                    "Rated"
                } else {
                    "All ratings"
                },
                "filter",
                Cmd::RatingFilter,
                filtering || self.rating_filter_open,
                cx,
            )
            .h(px(ui::CONTROL_HEIGHT))
            .when(self.compact, |s| s.w(px(if filtering { 64. } else { 42. })))
            .gap(px(space::SM))
            .when(filtering, |s| s.child(format!("{}+", self.min_rating)))
            .child(
                svg()
                    .path("down")
                    .size(px(12.))
                    .text_color(self.icon_muted()),
            )
            .tooltip(self.tip("Filter by minimum rating", ""));
        let grouping = self.settings.grouping_mode.clone();
        let grouped = grouping != "off";
        let (group_label, group_tip) = match grouping.as_str() {
            "time" => (
                format!("Time · {}", copy::duration(self.settings.burst_gap_ms)),
                format!(
                    "Grouped by time gap ({}) · click to turn off",
                    copy::duration(self.settings.burst_gap_ms)
                ),
            ),
            "visual" => (
                "Similar".to_owned(),
                "Grouped by visual similarity · click to turn off".to_owned(),
            ),
            _ if grouped => (
                format!("Bursts · {}", copy::duration(self.settings.burst_gap_ms)),
                format!(
                    "Grouped into bursts ({} gap) · click to turn off",
                    copy::duration(self.settings.burst_gap_ms)
                ),
            ),
            _ => (String::new(), "Group bursts".to_owned()),
        };
        div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .pl(px(ui::PANEL_INSET))
            .pr(px(ui::BAR_INSET))
            .py(px(ui::BAR_INSET))
            .bg(self.c("toolbar.background", "#141414"))
            .border_b_1()
            .border_color(self.border())
            .child(crumb)
            // Clear sits after the result count, apart from it, so the count
            // never reads as part of the action.
            .when(chips >= 2, |row| {
                row.child(
                    self.compact_button(
                        "clear-filters",
                        "Clear all",
                        "",
                        Cmd::ClearFilters,
                        false,
                        cx,
                    )
                    .px(px(space::SM))
                    .text_color(self.muted())
                    .tooltip(self.tip("Show the whole library", "")),
                )
            })
            .when(self.duplicates_only, |row| {
                let running = self
                    .duplicate_scan
                    .as_ref()
                    .is_some_and(|s| s.phase.is_active());
                row.child(
                    self.toggle_button(
                        "duplicate-scan",
                        if running {
                            "Stop scan"
                        } else {
                            copy::SCAN_DUPLICATES
                        },
                        if running { "stop" } else { "duplicates" },
                        if running {
                            Cmd::CancelDuplicates
                        } else {
                            Cmd::ScanDuplicates
                        },
                        running,
                        cx,
                    )
                    .tooltip(self.tip(
                        "Verify duplicate file bytes in the current folder or library",
                        "",
                    )),
                )
            })
            // An empty library has nothing to group or filter (CAT-21).
            .when(!self.photos.is_empty(), |toolbar| {
                toolbar
                    .child(
                        self.toggle_button(
                            "header-group",
                            if grouped && !self.compact {
                                group_label.as_str()
                            } else {
                                ""
                            },
                            // Grouping wears the thumbnails' burst glyph; the
                            // layers glyph means Survey.
                            "group-badge",
                            Cmd::Group(if grouped {
                                "off".into()
                            } else {
                                "burst".into()
                            }),
                            grouped,
                            cx,
                        )
                        .tooltip(self.tip(&group_tip, "")),
                    )
                    .child(self.control_divider())
                    .child(ratings)
            })
            .child(
                self.toggle_button(
                    "toolbar-info",
                    "",
                    "panel-right",
                    Cmd::Inspector,
                    self.docks.inspector > 0.,
                    cx,
                )
                .tooltip(self.tip("Toggle inspector", "I")),
            )
            .into_any_element()
    }
    /// Every open overlay, topmost first — the order `key()` routes input in.
    /// Several can be set at once (a folder picker over preferences or import,
    /// add-host beneath the palette), so guards must ask here rather than
    /// re-listing fields.
    fn overlay_open(&self, overlay: Overlay) -> bool {
        match overlay {
            Overlay::FolderPicker => self.folder_picker.is_some(),
            Overlay::Deletion => self.deletion.open(),
            Overlay::ImageContext => self.image_context.is_some(),
            Overlay::GettingStarted => self.getting_started,
            Overlay::RatingFilter => self.rating_filter_open,
            Overlay::Palette => self.palette,
            Overlay::Preferences => self.preferences,
            Overlay::AddHost => self.add_host.is_some(),
        }
    }
    /// Open overlays, topmost first (`Overlay::STACK`).
    fn overlays(&self) -> impl Iterator<Item = Overlay> + '_ {
        Overlay::STACK
            .into_iter()
            .filter(|&overlay| self.overlay_open(overlay))
    }
    /// The text field that owns the keyboard, following `key()`'s routing order;
    /// only its caret blinks, so idle windows do not repaint for hidden fields.
    fn focused_edit(&mut self) -> Option<&mut text_input::Edit> {
        // A field beneath Preferences or another overlay keeps its caret
        // for when it regains the keyboard, but must not blink meanwhile.
        let covered = self.top_overlay().is_some();
        if let Some(picker) = &mut self.folder_picker {
            return picker.focused_edit();
        }
        if self.palette {
            return Some(&mut self.palette_edit);
        }
        if let Some(form) = &mut self.add_host {
            return Some(&mut form.edit);
        }
        (self.import_ui.active_field.is_some() && !covered).then_some(&mut self.text_edit)
    }
    fn top_overlay(&self) -> Option<Overlay> {
        self.overlays().next()
    }
    fn other_overlay_open(&self) -> bool {
        self.import_ui.open
            || self
                .overlays()
                .any(|overlay| overlay != Overlay::RatingFilter)
    }
    fn reconcile_rating_filter(&mut self) {
        if self.other_overlay_open() {
            self.rating_filter_open = false;
        }
    }
    fn rating_filter_menu(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let mut options = div()
            .w(px(208.))
            .p(px(space::XS))
            .flex()
            .flex_col()
            .gap(px(0.))
            .bg(self.c("elevated_surface.background", "#202020"))
            .border_1()
            .border_color(self.c("border.variant", "#333333"))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for r in 0..=5u8 {
            let active = self.min_rating == r;
            options = options.child(
                self.toggle_button(
                    ("filter-option", r as usize),
                    &copy::rating_threshold(r),
                    if active { "check" } else { "star" },
                    Cmd::SetMinRating(r),
                    active,
                    cx,
                )
                .h(px(ui::ROW_HEIGHT))
                .w_full()
                .when(self.rating_filter_index == r, |s| s.bg(self.hover_bg()))
                .justify_start(),
            );
        }
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.rating_filter_open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(TITLE_HEIGHT
                        + ui::BAR_INSET
                        + ui::CONTROL_HEIGHT
                        + space::XS))
                    .right(px(self.docks.inspector
                        + ui::BAR_INSET
                        + ui::CONTROL_HEIGHT
                        + space::XS))
                    .child(options),
            )
            .into_any_element()
    }
    fn has_texture(&self, p: &Photo, kind: PreviewKind) -> bool {
        let tier = match kind {
            PreviewKind::Thumbnail => 0,
            PreviewKind::Fit => 1,
            PreviewKind::Full => 2,
        };
        let effect = self.display_effect(kind);
        let (mtime, size) = (preview_mtime(p), preview_size(p));
        self.textures
            .contains_key(&(p.id, tier + effect, mtime, size))
            || (tier == 1 && self.textures.contains_key(&(p.id, 2 + effect, mtime, size)))
    }
    fn trim_display_cache(&mut self) {
        let budget = (self.settings.memory_budget_bytes() / 3).max(32 * 1024 * 1024);
        if self.texture_bytes <= budget {
            return;
        }
        let mut candidates: Vec<_> = self
            .textures
            .iter()
            .filter(|(key, _)| !self.visible_textures.contains(key))
            .map(|(&key, texture)| (texture.tick, key))
            .collect();
        candidates.sort_unstable_by_key(|&(tick, _)| tick);
        for (_, key) in candidates {
            if self.texture_bytes <= budget {
                break;
            }
            if let Some(texture) = self.textures.remove(&key) {
                self.texture_bytes -= texture.bytes;
                self.retired.push(texture);
                // A following frame drops the renderer's image and reservation.
                self.texture_retry = true;
            }
        }
    }
    fn texture(
        &mut self,
        photo_index: usize,
        wanted: PreviewKind,
        cx: &mut gpui::Context<Self>,
    ) -> Option<(
        Arc<RenderImage>,
        u32,
        u32,
        TextureSource,
        PreviewKind,
        ColorStatus,
    )> {
        // Lend the catalog out instead of cloning a Photo (paths, strings,
        // metadata) for every visible cell on every frame. texture_photo never
        // reads self.photos synchronously; its completion task runs later.
        let photos = std::mem::take(&mut self.photos);
        let result = self.texture_photo(&photos[photo_index], wanted, cx);
        self.photos = photos;
        result
    }
    fn texture_photo(
        &mut self,
        p: &Photo,
        wanted: PreviewKind,
        cx: &mut gpui::Context<Self>,
    ) -> Option<(
        Arc<RenderImage>,
        u32,
        u32,
        TextureSource,
        PreviewKind,
        ColorStatus,
    )> {
        let desired = match wanted {
            PreviewKind::Thumbnail => 0,
            PreviewKind::Fit => 1,
            PreviewKind::Full => 2,
        };
        // Display identity; preview_size hashes paths for companions/remotes.
        let (mtime, size) = (preview_mtime(p), preview_size(p));
        let mut fallback = None;
        // Peaking and clipping copies are separate cache entries (effect bits).
        let effect = self.display_effect(wanted);
        let style = self.peaking_style();
        let search_max = if desired == 1 { 2 } else { desired };
        for k in (0..=search_max).rev() {
            let key = (p.id, k + effect, mtime, size);
            if let Some(texture) = self.textures.get_mut(&key) {
                self.tick += 1;
                texture.tick = self.tick;
                self.visible_textures.insert(key);
                fallback = Some((
                    texture.image.clone(),
                    texture.width,
                    texture.height,
                    texture.source.clone(),
                    texture.kind,
                    texture.color_status.clone(),
                ));
                if k >= desired {
                    return fallback;
                }
                break;
            }
        }
        // Keep the ordinary image visible while its overlay copy is prepared.
        if fallback.is_none() && effect != 0 {
            for k in (0..=search_max).rev() {
                let key = (p.id, k, mtime, size);
                if let Some(texture) = self.textures.get(&key) {
                    self.visible_textures.insert(key);
                    fallback = Some((
                        texture.image.clone(),
                        texture.width,
                        texture.height,
                        texture.source.clone(),
                        texture.kind,
                        texture.color_status.clone(),
                    ));
                    break;
                }
            }
        }
        let available = if wanted == PreviewKind::Fit {
            // A full JPEG can satisfy Fit without decoding it again.
            self.media.best_photo(p)
        } else {
            self.media
                .get_photo(p, wanted)
                .or_else(|| self.media.best_photo(p))
        };
        let Some(preview) = available else {
            return fallback;
        };
        // A thumbnail slot (grid, filmstrip) whose thumbnail has left the
        // preview cache falls back to a larger decoded preview. Its texture is
        // scaled to thumbnail size off the UI thread: converting and uploading
        // the full preview cost 10–25 ms of paint per newly visible cell
        // (docs/PERFORMANCE.md, "Human interface responsiveness").
        let shrink = (wanted == PreviewKind::Thumbnail && preview.kind != PreviewKind::Thumbnail)
            .then(|| thumbnail_size(preview.width, preview.height))
            .flatten();
        let k = match preview.kind {
            _ if shrink.is_some() => 0,
            PreviewKind::Thumbnail => 0,
            PreviewKind::Fit => 1,
            PreviewKind::Full => 2,
        };
        let key = (p.id, k + effect, mtime, size);
        self.visible_textures.insert(key);
        self.tick += 1;
        if let Some(texture) = self.textures.get_mut(&key) {
            texture.tick = self.tick;
            return Some((
                texture.image.clone(),
                texture.width,
                texture.height,
                texture.source.clone(),
                texture.kind,
                texture.color_status.clone(),
            ));
        }
        if self.texture_inflight.len() >= 12 || self.texture_inflight.contains(&key) {
            return fallback;
        }
        let (width, height) = shrink.unwrap_or((preview.width, preview.height));
        let bytes = width as usize * height as usize * 4;
        let kind = if shrink.is_some() {
            PreviewKind::Thumbnail
        } else {
            preview.kind
        };
        let source = TextureSource {
            embedded_raw: preview.embedded_raw,
            rendered_raw: preview.rendered_raw,
            note: preview.rendering_note.clone(),
            width: preview.source_width,
            height: preview.source_height,
        };
        let color_status = preview.color_status.clone();
        let Some(reservation) = self.media.reserve_display_bytes(bytes) else {
            // Admission is nonblocking: None can mean brief mutex contention.
            // Release old nonvisible textures and retry on the next 50ms poll;
            // only the decoder can report a definite image/budget failure.
            self.texture_retry = true;
            if let Some(old) = self
                .textures
                .iter()
                .filter(|(key, _)| !self.visible_textures.contains(key))
                .min_by_key(|(_, texture)| texture.tick)
                .map(|(key, _)| *key)
            {
                if let Some(texture) = self.textures.remove(&old) {
                    self.texture_bytes -= texture.bytes;
                    self.retired.push(texture);
                }
            }
            return fallback;
        };
        self.texture_inflight.insert(key);
        let task = cx.background_executor().spawn(async move {
            let mut bgra = if shrink.is_some() {
                let full =
                    image::RgbaImage::from_raw(preview.width, preview.height, preview.rgba.clone())
                        .expect("validated preview dimensions");
                image::imageops::thumbnail(&full, width, height).into_raw()
            } else {
                preview.rgba.clone()
            };
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Self::paint_effects(effect, style, &preview.rgba, &mut bgra, width, height);
            let buffer = image::RgbaImage::from_raw(width, height, bgra)
                .expect("validated preview dimensions");
            (
                Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
                reservation,
            )
        });
        cx.spawn(async move |this, cx| {
            let (image, reservation) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.texture_inflight.remove(&key);
                let valid = this
                    .photo_index
                    .get(&key.0)
                    .map(|&i| &this.photos[i])
                    .or_else(|| {
                        this.import_ui
                            .photo_lookup
                            .get(&key.0)
                            .map(|&i| &this.import_ui.previews[i])
                    })
                    .is_some_and(|p| preview_mtime(p) == key.2 && preview_size(p) == key.3);
                if !valid {
                    return;
                }
                this.texture_bytes += bytes;
                this.tick += 1;
                this.textures.insert(
                    key,
                    Texture {
                        image,
                        bytes,
                        tick: this.tick,
                        width,
                        height,
                        kind,
                        source,
                        color_status,
                        _reservation: reservation,
                    },
                );
                let budget = (this.settings.memory_budget_bytes() / 3).max(32 * 1024 * 1024);
                if this.texture_bytes > budget && this.textures.len() > 1 {
                    let mut candidates: Vec<_> = this
                        .textures
                        .iter()
                        .filter(|(key, _)| !this.visible_textures.contains(key))
                        .map(|(&key, texture)| (texture.tick, key))
                        .collect();
                    candidates.sort_unstable_by_key(|&(tick, _)| tick);
                    for (_, key) in candidates {
                        if this.texture_bytes <= budget || this.textures.len() <= 1 {
                            break;
                        }
                        if let Some(texture) = this.textures.remove(&key) {
                            this.texture_bytes -= texture.bytes;
                            this.retired.push(texture);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        fallback
    }
    fn photo_view(
        &mut self,
        photo_index: usize,
        reference: bool,
        window: &Window,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let kind = if self.zoom {
            PreviewKind::Full
        } else {
            PreviewKind::Fit
        };
        let photo = &self.photos[photo_index];
        if !self.has_texture(photo, kind) {
            self.media.request_photo(photo, kind);
        }
        let texture = self.texture(photo_index, kind, cx);
        let photo = &self.photos[photo_index];
        let context_photo_id = photo.id;
        let requested_error = self.media.photo_error(photo, kind);
        let mut quality = requested_error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| {
                if self.zoom {
                    "Loading full size…".into()
                } else {
                    "Loading preview…".into()
                }
            });
        let mut quality_note = None;
        let active = self.mode == ViewMode::Compare && self.reference_focused() == reference;
        // Peaking at Fit marks a downscaled preview's edges: say so.
        let fit_peaking = self.focus_peaking && !self.zoom;
        let show_quality = self.zoom
            || fit_peaking
            || requested_error.is_some()
            || matches!(&texture, Some((.., source, _, _)) if source.note.is_some())
            || !matches!(&texture, Some((.., kind, ColorStatus::ConvertedToSrgb | ColorStatus::AssumedSrgb)) if *kind == PreviewKind::Fit || *kind == PreviewKind::Full);
        let mut pane = div()
            .id(if reference {
                "reference-photo"
            } else {
                "current-photo"
            })
            .relative()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(self.canvas_bg())
            .overflow_hidden()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.click_photo(reference, event);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_image_context(context_photo_id, event.position, cx);
                    cx.stop_propagation();
                }),
            );
        if let Some((image, w, h, source, actual_kind, color_status)) = texture {
            let origin = if source.rendered_raw {
                " · sensor RAW"
            } else if source.embedded_raw {
                " · embedded JPEG"
            } else if photo
                .companion_path
                .as_deref()
                .is_some_and(shadowroom::heif::is_heif)
            {
                " · paired HEIF"
            } else if photo.companion_path.is_some() {
                " · paired JPEG"
            } else {
                ""
            };
            quality = if self.zoom && actual_kind != PreviewKind::Full {
                format!(
                    "Reduced {w} × {h}{origin} · {}",
                    requested_error
                        .as_ref()
                        .map(|e| format!("full size unavailable: {}", e.message))
                        .unwrap_or_else(|| "loading full size…".into())
                )
            } else if self.zoom {
                format!("{:.0}% · {w} × {h}{origin}", self.zoom_factor * 100.)
            } else if actual_kind == PreviewKind::Thumbnail {
                format!(
                    "Thumbnail · {w} × {h}{origin} · {}",
                    requested_error
                        .as_ref()
                        .map(|e| e.message.clone())
                        .unwrap_or_else(|| "loading fit preview…".into())
                )
            } else if fit_peaking {
                format!("Peaking on fit preview · Z for 100% · {w} × {h}{origin}")
            } else {
                format!("Fit · {w} × {h}{origin}")
            };
            quality.push_str(match color_status {
                // Routine conversion is in Info → Preview details.
                ColorStatus::ConvertedToSrgb => "",
                ColorStatus::AssumedSrgb => " · assumed sRGB",
                ColorStatus::InvalidProfile(_) => " · invalid ICC profile",
            });
            if let Some(note) = &source.note {
                quality_note = Some(note.clone());
            }
            let inspect = self.zoom;
            let zoom_factor = self.zoom_factor;
            // Keep the same view while a cached reduced preview refines.
            let source_scale =
                (source.width.max(w) as f32 / w as f32).max(source.height.max(h) as f32 / h as f32);
            let pan = self.pans[if reference { 0 } else { 1 }];
            let pane_size = std::rc::Rc::new(std::cell::Cell::new(size(px(1.), px(1.))));
            let measured = pane_size.clone();
            let fit_scales = self.fit_scales.clone();
            let geometry = self.pane_geometry.clone();
            let navigator_image = image.clone();
            pane = pane.child(
                gpui::canvas(
                    move |bounds, window, _| {
                        measured.set(bounds.size);
                        let mut panes = geometry.get();
                        panes[if reference { 0 } else { 1 }] = PaneGeometry {
                            bounds,
                            width: w,
                            height: h,
                            source_scale,
                            scale_factor: window.scale_factor(),
                        };
                        geometry.set(panes);
                        let mut scales = fit_scales.get();
                        scales[if reference { 0 } else { 1 }] = (f32::from(bounds.size.width)
                            / w as f32)
                            .min(f32::from(bounds.size.height) / h as f32)
                            * window.scale_factor()
                            / source_scale;
                        fit_scales.set(scales);
                    },
                    move |bounds, (), window, _cx| {
                        let scale = if inspect {
                            zoom_factor * source_scale / window.scale_factor()
                        } else {
                            (f32::from(bounds.size.width) / w as f32)
                                .min(f32::from(bounds.size.height) / h as f32)
                        };
                        let image_size = size(px(w as f32 * scale), px(h as f32 * scale));
                        let position =
                            |start: Pixels, available: Pixels, image: Pixels, pan: f32| {
                                if image > available {
                                    start - (image - available) * pan
                                } else {
                                    start + (available - image) / 2.
                                }
                            };
                        let origin = point(
                            position(bounds.origin.x, bounds.size.width, image_size.width, pan[0]),
                            position(
                                bounds.origin.y,
                                bounds.size.height,
                                image_size.height,
                                pan[1],
                            ),
                        );
                        let _ = window.paint_image(
                            Bounds::new(origin, image_size),
                            Corners::default(),
                            image.clone(),
                            0,
                            false,
                        );
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            );
            if inspect {
                let scroll_size = pane_size.clone();
                let scale = window.scale_factor() / (zoom_factor * source_scale);
                pane = pane
                    .cursor(if self.pan_drag.holds(reference) {
                        CursorStyle::ClosedHand
                    } else {
                        CursorStyle::OpenHand
                    })
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(px(24.));
                            if event.modifiers.control || event.modifiers.platform {
                                if f32::from(delta.y).abs() > 0. {
                                    this.scroll_zoom(
                                        reference,
                                        event.position,
                                        if delta.y > px(0.) { 1 } else { -1 },
                                    );
                                    cx.notify();
                                }
                                return;
                            }
                            let _ = window;
                            let size = scroll_size.get();
                            this.pan_photo(reference, delta, size, w, h, scale);
                            cx.notify();
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            if this.dock_drag.is_none() {
                                this.pan_drag = PanDrag::press(reference, event.position);
                                cx.notify();
                            }
                        }),
                    );
                let g = self.pane_geometry.get()[usize::from(!reference)];
                let larger = !g.valid()
                    || w as f32 * g.zoom_scale(zoom_factor) > f32::from(g.bounds.size.width) + 1.
                    || h as f32 * g.zoom_scale(zoom_factor) > f32::from(g.bounds.size.height) + 1.;
                if larger {
                    pane = pane.child(self.navigator(reference, navigator_image, w, h, cx));
                }
                let drag_size = pane_size.clone();
                pane =
                    pane.on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        let held = event.dragging() && this.dock_drag.is_none();
                        let was = this.pan_drag;
                        if let Some(delta) = this.pan_drag.motion(reference, event.position, held) {
                            this.pan_photo(reference, delta, drag_size.get(), w, h, scale);
                            cx.notify();
                        } else if was != this.pan_drag {
                            cx.notify();
                        }
                    }));
            } else {
                pane = pane.on_scroll_wheel(cx.listener(
                    move |this, event: &ScrollWheelEvent, _, cx| {
                        if event.modifiers.control || event.modifiers.platform {
                            let delta = event.delta.pixel_delta(px(24.));
                            if delta.y != px(0.) {
                                this.scroll_zoom(
                                    reference,
                                    event.position,
                                    if delta.y > px(0.) { 1 } else { -1 },
                                );
                                cx.notify();
                            }
                        }
                    },
                ));
            }
        } else {
            let error = self.media.photo_error(photo, kind);
            pane = pane.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(space::MD))
                    .text_color(self.muted())
                    .child(icon("camera", self.muted()))
                    .child(if let Some(error) = error {
                        error.message
                    } else {
                        "Loading preview…".into()
                    }),
            );
        }
        let compare = self.mode == ViewMode::Compare;
        // The frame's own decision rides on the photo, next to the Compare role,
        // anchored to the picture's painted edge (last frame's geometry).
        let chip = self.decision_chip(photo);
        let (inset_x, inset_y) = {
            let g = self.pane_geometry.get()[usize::from(!reference)];
            if g.valid() && !self.zoom {
                let fit = g.fit_scale();
                (
                    ((f32::from(g.bounds.size.width) - g.width as f32 * fit) / 2.).max(0.),
                    ((f32::from(g.bounds.size.height) - g.height as f32 * fit) / 2.).max(0.),
                )
            } else {
                (0., 0.)
            }
        };
        let pane = pane.when(compare || chip.is_some(), |pane| {
            pane.child(
                div()
                    .absolute()
                    .top(px(inset_y.floor() + space::SM))
                    .left(px(inset_x.floor() + space::SM))
                    .right(px(inset_x.floor() + space::SM))
                    .flex()
                    .items_start()
                    .gap(px(space::XS))
                    .when(compare, |row| {
                        row.child(
                            div()
                                .max_w(px(360.))
                                .min_w(px(0.))
                                .px(px(space::SM))
                                .h(px(22.))
                                .items_center()
                                .flex()
                                .gap(px(space::SM))
                                .bg(self.scrim())
                                .text_size(ui::font(ui::text::LABEL))
                                .text_color(if active {
                                    self.scrim_text()
                                } else {
                                    self.scrim_muted()
                                })
                                // The pane that decisions apply to is underlined.
                                .border_b_2()
                                .border_color(if active {
                                    self.scrim_accent()
                                } else {
                                    transparent_black()
                                })
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(div().text_color(self.scrim_muted()).child(if reference {
                                    "Reference"
                                } else {
                                    "Candidate"
                                }))
                                .child(photo.filename.clone()),
                        )
                    })
                    .children(chip.map(|chip| chip.flex_shrink_0())),
            )
        });
        // What the previous frame now carries, briefly, after auto-advance:
        // inside the picture's bottom-left corner, like the chip at its top.
        let pane = pane.when(!reference, |pane| {
            pane.children(self.decision_note_chip().map(|chip| {
                div()
                    .absolute()
                    .bottom(px(inset_y.floor() + space::SM))
                    .left(px(inset_x.floor() + space::SM))
                    .right(px(inset_x.floor() + space::SM))
                    .flex()
                    .child(chip.max_w_full())
            }))
        });
        let details = match quality_note {
            Some(note) => format!("{quality} · {note}"),
            None => quality.clone(),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .h_full()
            .child(pane)
            .when(show_quality, |view| {
                view.child(
                    div()
                        .id(if reference {
                            "reference-quality"
                        } else {
                            "photo-quality"
                        })
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .flex_shrink_0()
                        .px(px(space::SM))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .text_size(ui::font(ui::text::LABEL))
                        .text_color(if requested_error.is_some() {
                            self.c("warning", "#e0b341")
                        } else {
                            self.muted()
                        })
                        .when(requested_error.is_some(), |s| {
                            s.child(icon("warning", self.c("warning", "#e0b341")).size(px(12.)))
                        })
                        .child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_normal()
                                .line_clamp(1)
                                .child(quality),
                        )
                        .tooltip(self.tip(&details, "")),
                )
            })
            .into_any_element()
    }
    fn pan_photo(
        &mut self,
        reference: bool,
        delta: Point<Pixels>,
        viewport: Size<Pixels>,
        width: u32,
        height: u32,
        scale: f32,
    ) {
        let index = if reference { 0 } else { 1 };
        let x = (width as f32 / scale - f32::from(viewport.width)).max(1.);
        let y = (height as f32 / scale - f32::from(viewport.height)).max(1.);
        self.pans[index][0] = (self.pans[index][0] - f32::from(delta.x) / x).clamp(0., 1.);
        self.pans[index][1] = (self.pans[index][1] - f32::from(delta.y) / y).clamp(0., 1.);
        if self.compare_sync {
            self.pans[1 - index] = self.pans[index];
        }
    }
    /// The Catalog thumbnail size in effect: the chosen S / M / L, or S when
    /// the centre pane is too narrow for more than two larger columns.
    fn grid_size_index(&self) -> usize {
        if self.grid_size_forced() {
            0
        } else {
            self.grid_size.min(ui::GRID_SIZES.len() - 1)
        }
    }
    /// The centre pane is too narrow for M or L: S is the only size.
    fn grid_size_forced(&self) -> bool {
        self.center_size.0 < ui::GRID_SMALL_CENTER
    }
    /// (minimum cell width, image height, row height) for the Catalog grid.
    fn grid_metrics(&self) -> (f32, f32, f32) {
        let (cell, image) = ui::GRID_SIZES[self.grid_size_index()];
        (cell, image, image + ui::GRID_ROW_CHROME)
    }
    /// Columns that keep grid cells at least the chosen size wide. The
    /// scrollbar lives inside the right gutter, so both gutters are equal.
    fn grid_cols(&self) -> usize {
        ((self.center_size.0 - 2. * ui::PANEL_INSET + ui::GRID_GAP)
            / (self.grid_metrics().0 + ui::GRID_GAP))
            .floor()
            .clamp(1., 64.) as usize
    }
    fn grid(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let cols = self.grid_cols();
        let (_, image_height, row_height) = self.grid_metrics();
        let rows = self.filtered.len().div_ceil(cols);
        let handle = self.grid_scroll.clone();
        // Equal cells (placeholders share the box), so the well width is known
        // and badges can be pinned to each photo's contained rectangle.
        let cell_w =
            (self.center_size.0 - 2. * ui::PANEL_INSET - ui::GRID_GAP * (cols as f32 - 1.))
                / cols as f32;
        let well_w = (cell_w - 2. * space::XS - 2. * ui::HAIRLINE).max(1.);
        let list = uniform_list(
            "photo-grid",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let visible = (range.start, range.end);
                if this.grid_range != Some(visible) {
                    this.grid_range = Some(visible);
                    this.media.cancel_pending();
                }
                range
                    .map(|row| {
                        let mut line = div()
                            .h(px(row_height))
                            .w_full()
                            .min_w(px(0.))
                            .flex()
                            .gap(px(ui::GRID_GAP))
                            // Equal gutters: the 12 px scrollbar column beside
                            // the list is part of the right one.
                            .pl(px(ui::PANEL_INSET))
                            .pr(px(ui::PANEL_INSET - scrollbar::WIDTH))
                            .pt(px(space::LG));
                        for col in 0..cols {
                            let position = row * cols + col;
                            if let Some(&index) = this.filtered.get(position) {
                                let photo = &this.photos[index];
                                this.request_preview(photo, PreviewKind::Thumbnail);
                                let selected =
                                    position == this.selected || this.multi.contains(&photo.id);
                                // Hover follows the pointer state the app tracks, so
                                // keyboard navigation (which clears it) hides it.
                                let hovered = !selected && this.catalog_hover == Some(photo.id);
                                let dim =
                                    this.reject_opacity(photo.flag, position == this.selected);
                                let texture = this.texture(index, PreviewKind::Thumbnail, cx);
                                let photo = &this.photos[index];
                                let photo_id = photo.id;
                                // Badges anchor to the picture itself, so a portrait
                                // frame's badges sit on the photo, not in the letterbox.
                                let mut frame = div().relative().flex_shrink_0();
                                if let Some((texture, width, height, _, _, _)) = texture {
                                    let (w, h) = contained(well_w, image_height, width, height);
                                    frame = frame.w(px(w)).h(px(h)).child(
                                        div().size_full().opacity(dim).child(
                                            img(texture).size_full().object_fit(ObjectFit::Contain),
                                        ),
                                    );
                                } else {
                                    frame = frame
                                        .size_full()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap(px(space::SM))
                                        .child(icon("camera", this.icon_muted()))
                                        .when_some(
                                            this.media.photo_error(photo, PreviewKind::Thumbnail),
                                            |s, error| {
                                                s.child(
                                                    div()
                                                        .text_size(ui::font(ui::text::LABEL))
                                                        .text_color(this.muted())
                                                        .child(
                                                            if error
                                                                .message
                                                                .contains("Unavailable offline")
                                                            {
                                                                "Unavailable offline"
                                                            } else {
                                                                "Preview unavailable"
                                                            },
                                                        ),
                                                )
                                            },
                                        );
                                }
                                if let Some(badges) =
                                    this.decision_badges(photo.id, photo.flag, photo.label)
                                {
                                    frame = frame.child(badges);
                                }
                                // A check only when it means something: part of a
                                // deliberate multi-selection, never a lone selection.
                                if this.multi.len() > 1 && this.multi.contains(&photo.id) {
                                    frame = frame.child(
                                        div()
                                            .absolute()
                                            .top(px(space::XS))
                                            .right(px(space::XS))
                                            .size(px(18.))
                                            .bg(this.scrim())
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icon("selected", this.scrim_accent())),
                                    );
                                }
                                let checked =
                                    this.multi.len() > 1 && this.multi.contains(&photo.id);
                                frame = frame.children(this.pin_badge(
                                    photo.id,
                                    if checked {
                                        space::XS + 18. + space::TINY
                                    } else {
                                        space::XS
                                    },
                                ));
                                let burst = this.burst_slot(photo.id);
                                if let Some((_, frame_no, count)) = burst {
                                    frame = frame.child(
                                        div()
                                            .absolute()
                                            .bottom(px(space::XS))
                                            .right(px(space::XS))
                                            .px(px(space::XS))
                                            .h(px(18.))
                                            .flex()
                                            .items_center()
                                            .gap(px(space::XS))
                                            .bg(this.scrim())
                                            .text_color(this.scrim_text())
                                            .text_size(ui::font(ui::text::MICRO))
                                            .child(
                                                svg()
                                                    .path(if this.duplicates_only {
                                                        "duplicates"
                                                    } else {
                                                        "group-badge"
                                                    })
                                                    .size(px(if this.duplicates_only {
                                                        16.
                                                    } else {
                                                        12.
                                                    }))
                                                    .text_color(this.scrim_muted()),
                                            )
                                            .child(format!("{}/{count}", frame_no + 1)),
                                    );
                                }
                                let image = div()
                                    .h(px(image_height))
                                    .w_full()
                                    .relative()
                                    .bg(this.well_bg())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .overflow_hidden()
                                    .child(frame)
                                    // Same place for every aspect ratio: the well's edge.
                                    .children(this.label_strip(photo.label, 3., 0.));
                                // Adjacent bursts alternate intensity so boundaries read
                                // at a glance; neutral, since blue means "current".
                                let burst_bar = burst.map(|(group, ..)| {
                                    this.icon_muted().opacity(if group % 2 == 0 {
                                        0.9
                                    } else {
                                        0.45
                                    })
                                });
                                line = line.child(
                                    div()
                                        .id(("photo", position))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .p(px(space::XS))
                                        .border_1()
                                        .border_color(if selected {
                                            this.accent()
                                        } else {
                                            transparent_black()
                                        })
                                        .when(selected, |s| s.bg(this.selected_bg()))
                                        .when(hovered, |s| s.bg(this.hover_bg()))
                                        .cursor_pointer()
                                        .child(
                                            div()
                                                .h(px(2.))
                                                .mb(px(space::TINY))
                                                .bg(burst_bar.unwrap_or(transparent_black())),
                                        )
                                        .child(image)
                                        .child(
                                            div()
                                                .h(px(ui::COMPACT_CONTROL_HEIGHT))
                                                .px(px(space::XS))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(space::SM))
                                                .text_size(ui::font(ui::text::LABEL))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_normal()
                                                        .line_clamp(1)
                                                        .text_color(if selected || hovered {
                                                            this.text()
                                                        } else {
                                                            this.muted()
                                                        })
                                                        .child(photo.filename.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_color(this.rating_color())
                                                        .child(thumb_rating(photo.rating)),
                                                ),
                                        )
                                        .on_hover(cx.listener(
                                            move |this, hovered: &bool, _, cx| {
                                                this.hover_catalog_photo(photo_id, *hovered, cx);
                                            },
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(
                                                move |this, event: &MouseDownEvent, _, cx| {
                                                    this.open_image_context(
                                                        photo_id,
                                                        event.position,
                                                        cx,
                                                    );
                                                    cx.stop_propagation();
                                                },
                                            ),
                                        )
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, window, cx| {
                                                let m = event.modifiers();
                                                this.select(
                                                    position,
                                                    m.control || m.platform,
                                                    m.shift,
                                                    cx,
                                                );
                                                if event.click_count() == 2 {
                                                    this.execute(
                                                        Cmd::View(ViewMode::Loupe),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        )),
                                );
                            } else {
                                // Same box as a real cell, so partial rows keep
                                // the columns of the rows above them.
                                line = line.child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .p(px(space::XS))
                                        .border_1()
                                        .border_color(transparent_black()),
                                );
                            }
                        }
                        line
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(handle)
        .h_full()
        .w_full()
        .min_w(px(0.))
        .into_any_element();
        self.scrollable_list("catalog-scrollbar", &self.grid_scroll, list, cx)
    }
    /// Burst survey: every frame of the current burst at once, so the
    /// sharpest or best-timed frame can be chosen without paging.
    /// Frames for Survey, in view order: a reviewed selection of two or more
    /// photos first, else the current burst, else up to nine neighbours.
    fn survey_summary(&mut self, id: i64) -> Option<(SurveyKey, Arc<SurveySummary>)> {
        let selection = self.review_subset.is_some()
            && self.review_folder == self.scope
            && self.filtered.len() >= 2;
        let group = if self.duplicates_only {
            self.duplicate_lookup.get(&id).copied()
        } else {
            self.group_lookup.get(&id).copied()
        }
        .filter(|_| self.review_group(id).is_some_and(|g| g.len() > 1));
        const NEIGHBOURS: usize = 9;
        let key = if selection {
            SurveyKey::Selection(self.filtered.len())
        } else if let Some(group) = group {
            SurveyKey::Group(self.duplicates_only, group)
        } else if self.filtered.len() >= 2 {
            let first = self
                .selected
                .saturating_sub(NEIGHBOURS / 2)
                .min(self.filtered.len().saturating_sub(NEIGHBOURS));
            SurveyKey::Neighbours(first)
        } else {
            return None;
        };
        if self
            .survey_cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            let positions = match key {
                SurveyKey::Selection(len) => (0..len).collect(),
                SurveyKey::Group(..) => self
                    .review_group(id)?
                    .iter()
                    .filter_map(|&id| self.visible_position(id))
                    .collect(),
                SurveyKey::Neighbours(first) => {
                    (first..(first + NEIGHBOURS).min(self.filtered.len())).collect()
                }
            };
            self.survey_cache = Some((
                key,
                Arc::new(SurveySummary::new(positions, &self.filtered, &self.photos)),
            ));
        }
        self.survey_cache.clone()
    }
    /// Photo IDs Survey shows for the current photo.
    fn survey_members(&mut self) -> Option<Vec<i64>> {
        let id = self.selected_id()?;
        let (_, summary) = self.survey_summary(id)?;
        Some(
            summary
                .positions
                .iter()
                .map(|&position| self.photos[self.filtered[position]].id)
                .collect(),
        )
    }
    fn survey(&mut self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let Some(current) = self.selected_id() else {
            return self.empty(cx);
        };
        let summary = self.survey_summary(current);
        if summary.is_none() {
            let grouped = self.settings.grouping_mode != "off";
            return div()
                .flex_1()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::MD))
                .bg(self.canvas_bg())
                .text_color(self.muted())
                .child(icon("group", self.icon_muted()).size(px(24.)))
                .child("Survey needs two photos in this view.")
                .when(!grouped, |s| {
                    s.child(self.action_button(
                        "survey-enable-bursts",
                        "Group bursts",
                        Cmd::Group("burst".into()),
                        ButtonKind::Secondary,
                        false,
                        cx,
                    ))
                })
                .into_any_element();
        }
        // Frames in view order; a bounded window keeps memory and disk reads flat
        // even for very long bursts.
        let (key, summary) = summary.expect("survey group");
        let all = &summary.positions;
        let at = all.binary_search(&self.selected).unwrap_or(0);
        const MAX: usize = 16;
        let first = at
            .saturating_sub(MAX / 2)
            .min(all.len().saturating_sub(MAX));
        let frames = &all[first..(first + MAX).min(all.len())];
        let n = frames.len();
        let span = &summary.span;
        // The frames' typical shape (median), so a portrait burst gets
        // portrait cells.
        let mut aspects: Vec<f32> = frames
            .iter()
            .map(|&position| {
                let p = &self.photos[self.filtered[position]];
                let (w, h) = if (5..=8).contains(&p.orientation) {
                    (p.height, p.width)
                } else {
                    (p.width, p.height)
                };
                if w > 0 && h > 0 {
                    w as f32 / h as f32
                } else {
                    1.5
                }
            })
            .collect();
        aspects.sort_by(f32::total_cmp);
        let aspect = aspects
            .get(aspects.len() / 2)
            .copied()
            .unwrap_or(1.5)
            .clamp(0.4, 2.5);
        let (cols, cell_w, cell_h) = survey_layout(
            n,
            aspect,
            self.center_size.0 - 2. * ui::PANEL_INSET,
            self.center_size.1 - 2. * ui::PANEL_INSET - ui::SURVEY_HEADER_HEIGHT,
        );
        let rows = n.div_ceil(cols);
        // The block hugs its cells and sits centred, so spare width becomes
        // margin around the frames instead of wide gaps between them.
        let mut grid = div()
            .w(px(cols as f32 * cell_w + space::SM * (cols - 1) as f32))
            .h(px(rows as f32 * cell_h + space::SM * (rows - 1) as f32))
            .flex()
            .flex_col()
            .gap(px(space::SM));
        let mut row = div().h(px(cell_h)).flex().gap(px(space::SM));
        for (i, &position) in frames.iter().enumerate() {
            let index = self.filtered[position];
            let p = &self.photos[index];
            let active = position == self.selected;
            // Fit previews only where they are legible; thumbnails elsewhere.
            let kind = if active || n <= 9 {
                PreviewKind::Fit
            } else {
                PreviewKind::Thumbnail
            };
            for wanted in [PreviewKind::Thumbnail, kind] {
                if !self.has_texture(p, wanted) {
                    self.media.request_photo(p, wanted);
                }
            }
            let texture = self.texture(index, kind, cx);
            let p = &self.photos[index];
            // Overlays belong on the photo, not the letterbox: size a box to the
            // photo's aspect inside the cell.
            let aspect = texture
                .as_ref()
                .map(|(_, w, h, ..)| *w as f32 / (*h).max(1) as f32)
                .unwrap_or(1.5);
            let (box_w, box_h) = if cell_w / cell_h > aspect {
                (cell_h * aspect, cell_h)
            } else {
                (cell_w, cell_w / aspect)
            };
            let mut photo_box = div()
                .relative()
                .w(px(box_w))
                .h(px(box_h))
                .overflow_hidden()
                .border_2()
                .border_color(if active {
                    self.accent()
                } else {
                    transparent_black()
                });
            if let Some((texture, ..)) = texture {
                photo_box = photo_box.child(
                    img(texture)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .opacity(self.reject_opacity(p.flag, active)),
                );
            } else {
                photo_box = photo_box.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .gap(px(space::SM))
                        .items_center()
                        .justify_center()
                        .child(icon("camera", self.icon_muted()))
                        .when_some(self.media.photo_error(p, kind), |s, error| {
                            s.child(div().text_size(ui::font(ui::text::LABEL)).child(
                                if error.message.contains("Unavailable offline") {
                                    "Unavailable offline"
                                } else {
                                    "Preview unavailable"
                                },
                            ))
                        }),
                );
            }
            if let Some(badges) = self.decision_badges(p.id, p.flag, p.label) {
                photo_box = photo_box.child(badges);
            }
            // Frame number in a fixed plate (up to "16"), the pin beside it.
            photo_box = photo_box.children(self.pin_badge(p.id, space::XS + 22. + space::TINY));
            photo_box = photo_box.child(
                div()
                    .absolute()
                    .top(px(space::XS))
                    .right(px(space::XS))
                    .w(px(22.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(self.scrim())
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(if active {
                        self.scrim_text()
                    } else {
                        self.scrim_muted()
                    })
                    .child((first + i + 1).to_string()),
            );
            photo_box = photo_box.children(self.rating_badge(p.rating));
            let cell = div()
                .id(("survey", position))
                .w(px(cell_w))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(photo_box);
            let photo_id = p.id;
            row = row.child(
                cell.tooltip(self.tip(&p.filename, ""))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.open_image_context(photo_id, event.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let m = event.modifiers();
                        this.select(position, m.control || m.platform, m.shift, cx);
                        if event.click_count() == 2 {
                            this.execute(Cmd::View(ViewMode::Loupe), window, cx);
                        }
                    })),
            );
            if (i + 1) % cols == 0 || i + 1 == n {
                // Pad the last row so frames keep a uniform size.
                for _ in 0..(cols - 1 - i % cols) {
                    row = row.child(div().w(px(cell_w)).flex_shrink_0());
                }
                grid = grid.child(row);
                row = div().h(px(cell_h)).flex().gap(px(space::SM));
            }
        }
        let decided = summary.decided;
        div()
            .flex_1()
            .min_w(px(0.))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .p(px(ui::PANEL_INSET))
            .bg(self.canvas_bg())
            .child(
                div()
                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .px(px(space::TINY))
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(self.muted())
                    .when_some(key.label(all.len()), |s, label| {
                        s.child(div().flex_shrink_0().text_color(self.text()).child(label))
                    })
                    .child(div().flex_shrink_0().text_color(self.text()).child(format!(
                        "Frame {} of {}",
                        at + 1,
                        all.len()
                    )))
                    // The descriptive text yields first; the action never collapses.
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!(
                                "{}{span}{}",
                                plural(decided, "frame decided", "frames decided"),
                                if all.len() > n {
                                    format!(" · showing {}–{}", first + 1, first + n)
                                } else {
                                    String::new()
                                }
                            )),
                    )
                    .child({
                        let hover = self.hover_bg();
                        div()
                            .id("survey-resolve")
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(space::SM))
                            .h(px(ui::COMPACT_CONTROL_HEIGHT))
                            .px(px(space::SM))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .child(self.keycaps("⇧P"))
                            .when(!self.compact, |s| {
                                s.child(div().text_color(self.text()).child("Keep this · reject undecided"))
                            })
                            .tooltip(self.tip(
                                "Pick this frame and reject the undecided ones · decided frames keep their flags",
                                "⇧P",
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.execute(Cmd::ResolveBurst, window, cx)
                            }))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(grid),
            )
            .into_any_element()
    }
    /// Label chip: filled with the label's photo-overlay colour, its initial
    /// in near-black, so a label reads at a glance by colour area and survives
    /// colour-vision deficiency by its letter. Sits on a scrim plate.
    fn label_badge(&self, label: ColorLabel) -> Option<Div> {
        self.photo_label_color(label).map(|color| {
            div()
                .size(px(BADGE_CELL))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .bg(color)
                .text_size(ui::font(ui::text::MICRO))
                .line_height(ui::font(ui::text::MICRO))
                .font_weight(FontWeight::MEDIUM)
                .text_color(self.c("shadowroom.scrim.label.text", "#0f0f0f"))
                .child(label_initial(label))
        })
    }
    /// Decisions at the photo's top-left on one continuous 18 px scrim plate:
    /// the pick/reject glyph, then the label chip, 2 px apart; a warning glyph
    /// last while that photo's decision failed to save (gone once it saves).
    fn decision_badges(&self, id: i64, flag: Flag, label: ColorLabel) -> Option<Div> {
        let label = self.label_badge(label);
        let unsaved = self.save_state() == SaveState::Failed && self.unsaved.contains_key(&id);
        if flag == Flag::None && label.is_none() && !unsaved {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(px(space::XS))
                .left(px(space::XS))
                .flex()
                .items_center()
                .gap(px(space::TINY))
                .p(px(space::TINY))
                .bg(self.scrim())
                .when(flag != Flag::None, |s| {
                    s.child(
                        div()
                            .size(px(BADGE_CELL))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path(if flag == Flag::Pick {
                                        "pick-badge"
                                    } else {
                                        "reject-badge"
                                    })
                                    .size(px(12.))
                                    .text_color(self.scrim_flag(flag)),
                            ),
                    )
                })
                .children(label)
                .when(unsaved, |s| {
                    s.child(
                        div()
                            .size(px(BADGE_CELL))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("warning")
                                    .size(px(12.))
                                    .text_color(self.c("shadowroom.scrim.reject", "#e07a7a")),
                            ),
                    )
                }),
        )
    }
    /// Compact "★5" rating chip, bottom-right of a thumbnail (filmstrip, Survey).
    fn rating_badge(&self, rating: u8) -> Option<Div> {
        (rating > 0).then(|| {
            div()
                .absolute()
                .bottom(px(space::XS))
                .right(px(space::XS))
                .h(px(14.))
                .px(px(space::XS))
                .flex()
                .items_center()
                .bg(self.scrim())
                .text_size(ui::font(ui::text::MICRO))
                .text_color(self.scrim_rating())
                .child(thumb_rating(rating))
        })
    }
    /// Inspector label swatch: the chrome label colour carrying its key
    /// (6-9; Purple has none), a text-coloured ring with an inset gap when
    /// chosen. The R/Y/G/B/P initials stay on photo badges, where they are the
    /// colour-blind shape code; here a letter would read as a keycap.
    fn label_swatch(
        &self,
        id: impl Into<ElementId>,
        label: ColorLabel,
        selected: bool,
        tip: String,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let color = self.label_color(label);
        div()
            .id(id)
            .size(px(ui::COMPACT_CONTROL_HEIGHT))
            .p(px(space::XS))
            .border_1()
            .border_color(if selected {
                self.text()
            } else {
                transparent_black()
            })
            .cursor_pointer()
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ui::font(ui::text::MICRO))
                    .line_height(ui::font(ui::text::MICRO))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(self.c("shadowroom.label.text", "#0f0f0f"))
                    .when_some(color, |s, c| s.bg(c).child(label_key(label)))
                    // The empty choice is an input with no fill of its own.
                    .when(color.is_none(), |s| {
                        s.border_1().border_color(self.control_border())
                    }),
            )
            .tooltip(self.tip(&tip, label_key(label)))
            .on_click(
                cx.listener(move |this, _, window, cx| this.execute(Cmd::Label(label), window, cx)),
            )
    }
    /// Colour-label strip along a thumbnail's bottom edge: colour area for
    /// scanning a shoot, in the chrome label colour because it sits on the
    /// well, not the photo.
    fn label_strip(&self, label: ColorLabel, height: f32, inset: f32) -> Option<Div> {
        self.label_color(label).map(|color| {
            div()
                .absolute()
                .left(px(inset))
                .right(px(inset))
                .bottom(px(inset))
                .h(px(height))
                .bg(color)
        })
    }
    /// (group index, frame position, frame count) for photos in a burst or
    /// duplicate set of two or more.
    fn burst_slot(&self, id: i64) -> Option<(usize, usize, usize)> {
        if self.duplicates_only {
            self.duplicate_slots.get(&id).copied()
        } else {
            self.bursts.slots.get(&id).copied()
        }
    }

    /// The pinned Compare reference, marked on its thumbnail: a pin plate at
    /// the photo's top-right, `right` px in from the edge.
    fn pin_badge(&self, id: i64, right: f32) -> Option<Div> {
        (self.reference == Some(id)).then(|| {
            div()
                .absolute()
                .top(px(space::XS))
                .right(px(right))
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .bg(self.scrim())
                .child(
                    svg()
                        .path("pin")
                        .size(px(12.))
                        .text_color(self.scrim_text()),
                )
        })
    }
    fn group_count_badge(&self, count: usize) -> Div {
        div()
            .absolute()
            .bottom(px(space::XS))
            .left(px(space::XS))
            .px(px(space::XS))
            .h(px(14.))
            .flex()
            .items_center()
            .gap(px(space::TINY))
            .bg(self.scrim())
            .text_color(self.scrim_text())
            .text_size(ui::font(ui::text::MICRO))
            .child(
                svg()
                    .path(if self.duplicates_only {
                        "duplicates"
                    } else {
                        "group-badge"
                    })
                    .size(px(10.))
                    .text_color(self.scrim_muted()),
            )
            .child(count.to_string())
    }
    fn filmstrip(&mut self, cx: &mut gpui::Context<Self>) -> AnyElement {
        // A bounded moving window sized to the strip's width: grow outward from
        // the selected frame while cells fit, so it is never clipped.
        let cell_width = |_: &Photo| ui::FILM_CELL_BUDGET;
        let available = self.center_size.0 - 2. * ui::PANEL_INSET;
        let (mut start, mut end) = (self.selected, (self.selected + 1).min(self.filtered.len()));
        let mut used = self
            .filtered
            .get(self.selected)
            .map_or(0., |&i| cell_width(&self.photos[i]));
        for step in 0..((available / ui::FILM_CELL_BUDGET).ceil() as usize).min(128) {
            let (grow_right, grow_left) = (end < self.filtered.len(), start > 0);
            let right = (step % 2 == 0 && grow_right) || !grow_left;
            let next = if right && grow_right {
                end
            } else if grow_left {
                start - 1
            } else {
                break;
            };
            let width = cell_width(&self.photos[self.filtered[next]]);
            if used + width > available {
                break;
            }
            used += width;
            if next == end { end += 1 } else { start -= 1 }
        }
        let mut strip = div()
            .relative()
            .h(px(FILMSTRIP_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(ui::FILM_GAP))
            .px(px(ui::PANEL_INSET))
            // The cull ledger rides in the top padding; frames stay centred
            // below it (the same top padding whether or not it is shown).
            .pt(px(ledger::TRACK_TOP + ledger::TRACK))
            .pb(px(ui::HAIRLINE))
            .bg(self.panel())
            .border_t_1()
            .border_color(self.border())
            .overflow_hidden();
        if self.ledger_visible() {
            // When every frame fits, each ledger cell sits exactly over its
            // frame; otherwise the track is a minimap of the whole view.
            let all = start == 0 && end == self.filtered.len();
            let (left, track) = if all {
                let width = used - ui::FILM_GAP;
                (
                    ui::PANEL_INSET + ((available - width) / 2.).max(0.),
                    ledger::Track {
                        width,
                        gap: ui::FILM_GAP,
                    },
                )
            } else {
                (
                    ui::PANEL_INSET,
                    ledger::Track {
                        width: self.ledger_width(),
                        gap: 1.,
                    },
                )
            };
            strip = strip.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(left))
                    .child(self.ledger(track, cx)),
            );
        }
        // Consecutive frames of one burst share a tinted run so group
        // boundaries are visible without opening the inspector.
        let mut run: Option<(usize, Div)> = None;
        for position in start..end {
            let index = self.filtered[position];
            let p = &self.photos[index];
            let active = position == self.selected;
            let multi = self.multi.contains(&p.id);
            self.request_preview(p, PreviewKind::Thumbnail);
            let texture = self.texture(index, PreviewKind::Thumbnail, cx);
            let p = &self.photos[index];
            // Preserve the full photograph within an equal-sized slot; badges
            // pin to the picture so portrait frames keep them on the photo.
            let mut frame = div().relative().flex_shrink_0();
            if let Some((texture, width, height, _, _, _)) = texture {
                let (w, h) = contained(ui::FILM_WIDTH, ui::FILM_HEIGHT, width, height);
                frame = frame.w(px(w)).h(px(h)).child(
                    div()
                        .size_full()
                        .opacity(self.reject_opacity(p.flag, active))
                        .child(img(texture).size_full().object_fit(ObjectFit::Contain)),
                );
            } else {
                frame = frame
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("camera", self.icon_muted()));
            }
            if let Some(badges) = self.decision_badges(p.id, p.flag, p.label) {
                frame = frame.child(badges);
            }
            frame = frame.children(self.pin_badge(p.id, space::XS));
            // Compact "★5" chip: the count, not five glyphs over the photo.
            frame = frame.children(self.rating_badge(p.rating));
            let burst = self.burst_slot(p.id);
            let run_start =
                burst.is_some_and(|(group, ..)| run.as_ref().is_none_or(|(g, _)| *g != group));
            if let (true, Some((_, _, count))) = (run_start, burst) {
                frame = frame.child(self.group_count_badge(count));
            }
            let image = div()
                .relative()
                .w(px(ui::FILM_WIDTH))
                .h(px(ui::FILM_HEIGHT))
                // The strip sits on a panel, so its slots use the canvas, which
                // steps away from the panel in both themes (the grid well steps
                // away from the window background instead).
                .bg(self.canvas_bg())
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .child(frame)
                // 2 px label strip inside the 2 px selection inset.
                .children(self.label_strip(p.label, 2., 2.));
            let photo_id = p.id;
            // Selection is an inset overlay, so selected and unselected cells
            // have identical boxes and the strip keeps one constant gap.
            let ring = div()
                .absolute()
                .inset_0()
                .border_2()
                .border_color(if active {
                    self.accent()
                } else if multi {
                    self.accent().opacity(0.8)
                } else {
                    transparent_black()
                })
                .when(!active && !multi, |s| {
                    let hover = self.icon_muted();
                    s.group_hover("film-cell", move |s| s.border_color(hover))
                });
            let cell = div()
                .id(("film", position))
                .group("film-cell")
                .relative()
                .flex_shrink_0()
                .cursor_pointer()
                .child(image)
                .child(ring)
                .tooltip(self.tip(
                    &match burst {
                        Some((_, frame, count)) => {
                            format!("{} · frame {} of {count}", p.filename, frame + 1)
                        }
                        None => p.filename.clone(),
                    },
                    "",
                ))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.open_image_context(photo_id, event.position, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    let m = event.modifiers();
                    this.select(position, m.control || m.platform, m.shift, cx);
                }));
            match (burst, run.take()) {
                (Some((group, ..)), Some((g, container))) if g == group => {
                    run = Some((g, container.child(cell)));
                }
                (burst, previous) => {
                    if let Some((_, container)) = previous {
                        strip = strip.child(container);
                    }
                    match burst {
                        Some((group, ..)) => {
                            run = Some((
                                group,
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(ui::FILM_GAP))
                                    .pb(px(space::TINY))
                                    .border_t_2()
                                    .border_color(self.icon_muted().opacity(0.6))
                                    .child(cell),
                            ));
                        }
                        None => strip = strip.child(cell),
                    }
                }
            }
        }
        if let Some((_, container)) = run {
            strip = strip.child(container);
        }
        strip.into_any_element()
    }
    /// Decision bar: flag, rate, then navigation/inspection on the right.
    fn rating_bar(&self, cx: &gpui::Context<Self>) -> AnyElement {
        if self.filter == Filter::Trash {
            return div()
                .h(px(DECISION_HEIGHT))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .p(px(ui::BAR_INSET))
                .border_t_1()
                .border_color(self.border())
                .bg(self.c("toolbar.background", "#141414"))
                .child(self.button(
                    "restore-selection",
                    "Restore",
                    "restore",
                    Cmd::Trash(false),
                    false,
                    cx,
                ))
                .child(div().min_w(px(0.)).overflow_hidden().text_ellipsis().whitespace_normal().line_clamp(1).text_size(ui::font(11.)).text_color(self.muted()).child(
                    "Originals stay in their source folders. Ratings and flags are preserved.",
                ))
                .into_any_element();
        }
        let p = self.review_photo();
        // A multi-selection shows what its members share, never just the
        // primary photo's state (decisions apply to every member).
        let summary = self.selection_summary();
        let rating = match &summary {
            Some(summary) => summary.common_rating().unwrap_or(0),
            None => p.map_or(0, |p| p.rating),
        };
        let mixed_rating = summary
            .as_ref()
            .is_some_and(|s| s.common_rating().is_none());
        let flag = match &summary {
            Some(summary) => summary.common_flag(),
            None => Some(p.map_or(Flag::None, |p| p.flag)),
        };
        let grid = self.mode == ViewMode::Grid;
        let compare = self.mode == ViewMode::Compare;
        // Survey shows fitted frames: loupe zoom and peaking don't apply there.
        let zoomable = matches!(self.mode, ViewMode::Loupe | ViewMode::Compare);
        // Responsive priority: the essentials always stay (flags, stars,
        // position, auto-advance, zoom or thumbnail size, Trash); optional
        // items drop lowest value first as the bar narrows.
        // Geist Mono advances at 12 and 11 px, at the current text size.
        let char_w = 7.2 * ui::text_scale();
        let small_char = 6.6 * ui::text_scale();
        let width = self.center_size.0;
        let tight = width < 760.;
        let gap = if tight { space::TINY } else { space::XS };
        let control = ui::CONTROL_HEIGHT + gap;
        let position = if self.multi.len() > 1 {
            format!("{} selected", copy::count(self.multi.len()))
        } else {
            format!(
                "{} / {}",
                copy::count(if self.filtered.is_empty() {
                    0
                } else {
                    self.selected + 1
                }),
                copy::count(self.filtered.len())
            )
        };
        let zoom_label = self.zoom_label();
        let reviewed = self.filtered_reviewed;
        let reviewed_text = format!("{} reviewed", copy::count(reviewed));
        let stars_width = 5. * if tight { 20. } else { ui::CONTROL_HEIGHT };
        let view_controls = if grid {
            3. * (2. * space::SM + char_w) + 2. + gap
        } else if !zoomable {
            0.
        } else {
            let zoom = 2. * space::SM + zoom_label.len() as f32 * char_w;
            zoom + gap + control + if compare { control } else { 0. }
        };
        // Groups: decide | rate … position | workflow | view | delete.
        let view_group = grid || zoomable;
        let dividers = if view_group { 4. } else { 3. };
        let base = 2. * if tight { space::XS } else { ui::BAR_INSET }
            + 2. * control // flag icons
            + dividers * (1. + 2. * space::XS + gap)
            + stars_width
            + gap
            + if mixed_rating { 5. * small_char + gap } else { 0. }
            + control // reset icon
            + position.chars().count() as f32 * small_char
            + 2. * space::SM
            + gap
            + control // auto-advance
            + view_controls
            + control; // trash
        let optional = [
            // P / X keycaps beside the flag icons, and "1-5" after the stars.
            2. * (space::SM + small_char + space::XS) + 3. * small_char + gap,
            // "Pick" / "Reject" words.
            10. * char_w + 2. * space::SM,
            // Pin and Survey (Review modes only; Catalog has S and the tab).
            if grid { 0. } else { 2. * control },
            // Zoom − / + (Review and Compare only).
            if zoomable { 2. * control } else { 0. },
            // "N reviewed" (the cull ledger under the bar draws the progress).
            reviewed_text.len() as f32 * small_char + space::SM,
            // "Reset" text.
            space::SM + 5. * char_w,
        ];
        let kept = ui::fit_prefix(width, base, &optional);
        let (flag_keys, flag_labels, pin_survey, zoom_steps, progress, reset_label) =
            (kept > 0, kept > 1, kept > 2, kept > 3, kept > 4, kept > 5);
        let hover = self.hover_bg();
        let flag_button = |id: &'static str,
                           label: &'static str,
                           key: &'static str,
                           glyph: &'static str,
                           target: Flag,
                           color: Hsla| {
            let on = flag == Some(target);
            // Some, not all, selected photos carry this flag.
            let partial = !on && summary.as_ref().is_some_and(|s| s.has_flag(target));
            div()
                .id(id)
                .h(px(ui::CONTROL_HEIGHT))
                .px(px(space::SM))
                .when(!flag_keys && !flag_labels, |s| {
                    s.w(px(ui::CONTROL_HEIGHT)).px(px(0.))
                })
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .gap(px(space::SM))
                .cursor_pointer()
                // 10% keeps the coloured label at 4.5:1 on its own tint in both themes.
                .when(on, |s| s.bg(color.opacity(0.10)))
                .when(!on, |s| s.hover(move |s| s.bg(hover)))
                .child(icon(
                    if on {
                        if target == Flag::Pick {
                            "pick-on"
                        } else {
                            "reject-on"
                        }
                    } else if partial {
                        "partial"
                    } else {
                        glyph
                    },
                    if on || partial {
                        color
                    } else {
                        self.icon_muted()
                    },
                ))
                .when(flag_labels, |s| {
                    s.child(
                        div()
                            .text_color(if on { color } else { self.text() })
                            .child(label),
                    )
                })
                .when(flag_keys, |s| s.child(self.keycap(key)))
                .tooltip(self.tip(
                    &if partial {
                        format!("{label} all selected · some already are")
                    } else {
                        label.to_owned()
                    },
                    key,
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(Cmd::Flag(target), window, cx)
                }))
        };
        let mut stars = div().flex().flex_shrink_0().items_center();
        for r in 1..=5 {
            stars = stars.child(
                self.star(("rate", r as usize), r <= rating, Cmd::Rating(r), cx)
                    .when(tight, |s| s.w(px(20.)))
                    .tooltip(self.tip(&format!("Rate {r}"), &r.to_string())),
            );
        }
        let pinned = self.reference == self.selected_id() && self.reference.is_some();
        let share = reviewed as f32 / self.filtered.len().max(1) as f32;
        div()
            .id("decision-bar")
            .h(px(DECISION_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(gap))
            .p(px(ui::BAR_INSET))
            .when(tight, |s| s.p(px(space::XS)))
            .border_t_1()
            .border_color(self.border())
            .bg(self.c("toolbar.background", "#141414"))
            .child(flag_button(
                "pick",
                "Pick",
                "P",
                "pick",
                Flag::Pick,
                self.pick_color(),
            ))
            .child(flag_button(
                "reject",
                "Reject",
                "X",
                "reject",
                Flag::Reject,
                self.reject_color(),
            ))
            .child(self.control_divider())
            .child(stars)
            .when(flag_keys, |bar| {
                bar.child(self.keycap("1-5").flex_shrink_0())
            })
            .when(mixed_rating, |bar| {
                bar.child(
                    div()
                        .id("rating-mixed")
                        .flex_shrink_0()
                        .text_size(ui::font(ui::text::LABEL))
                        .text_color(self.muted())
                        .child("mixed")
                        .tooltip(self.tip("Selected photos have different ratings", "")),
                )
            })
            .child(
                self.button(
                    "reset-decision",
                    if reset_label { "Reset" } else { "" },
                    "reset",
                    Cmd::ResetDecision,
                    false,
                    cx,
                )
                .tooltip(self.tip(
                    &format!(
                        "{} · keep color label · stay on this photo",
                        copy::RESET_DECISION
                    ),
                    "R",
                )),
            )
            .child(div().flex_1().min_w(px(0.)))
            .child(
                div()
                    .id("review-progress")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::SM))
                    .text_size(ui::font(ui::text::LABEL))
                    .text_color(self.muted())
                    .child(div().text_color(self.text()).child(position))
                    .when(progress && self.multi.len() < 2, |s| s.child(reviewed_text))
                    .tooltip(self.tip(
                        &format!(
                            "{} of {} reviewed ({:.0}%)",
                            copy::count(reviewed),
                            copy::count(self.filtered.len()),
                            100. * share
                        ),
                        "",
                    )),
            )
            .child(self.control_divider())
            .child(
                self.toggle_button(
                    "header-advance",
                    "",
                    "auto-advance",
                    Cmd::AutoAdvance,
                    self.settings.auto_advance,
                    cx,
                )
                // On is marked by shape as well as tint: the segment's 2 px
                // text-colour bar (a state, not a location).
                .relative()
                .when(self.settings.auto_advance, |s| {
                    s.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left(px(space::XS))
                            .right(px(space::XS))
                            .h(px(2.))
                            .bg(self.text()),
                    )
                })
                .tooltip(self.tip(
                    if self.settings.auto_advance {
                        "Auto-advance on · a decision moves to the next photo"
                    } else {
                        "Auto-advance off · decisions stay on this photo"
                    },
                    "Shift A",
                )),
            )
            .when(pin_survey && !grid, |bar| {
                bar.child(
                    self.toggle_button("pin-reference", "", "pin", Cmd::Pin, pinned, cx)
                        .tooltip(self.tip(
                            if compare {
                                "Make the candidate the reference · next candidate"
                            } else {
                                "Pin as compare reference"
                            },
                            "B",
                        )),
                )
            })
            .when(pin_survey && !grid, |bar| {
                bar.child(
                    self.toggle_button(
                        "survey-burst",
                        "",
                        "group",
                        Cmd::View(if self.mode == ViewMode::Survey {
                            ViewMode::Loupe
                        } else {
                            ViewMode::Survey
                        }),
                        self.mode == ViewMode::Survey,
                        cx,
                    )
                    .tooltip(self.tip(
                        if self.multi.len() > 1 {
                            "Survey the selection · every frame side by side"
                        } else {
                            "Survey burst · every frame side by side"
                        },
                        "S",
                    )),
                )
            })
            .when(view_group, |bar| bar.child(self.control_divider()))
            .when(grid, |bar| bar.child(self.grid_size_control(cx)))
            .when(zoomable, |bar| {
                bar.child(
                    // Text only: the lens glyph belongs to the Zoom-in step.
                    self.toggle_button("zoom-photo", &zoom_label, "", Cmd::Zoom, self.zoom, cx)
                        .tooltip(self.tip(
                            if self.zoom {
                                "Fit to window"
                            } else {
                                "View at 100% · or click the photo where you want to look"
                            },
                            "Z",
                        )),
                )
                .when(zoom_steps, |bar| {
                    bar.child(
                        self.button("zoom-out", "", "zoom-out", Cmd::ZoomStep(-1), false, cx)
                            .tooltip(self.tip("Zoom out", "-")),
                    )
                    .child(
                        self.button("zoom-in", "", "zoom", Cmd::ZoomStep(1), false, cx)
                            .tooltip(self.tip("Zoom in", "=")),
                    )
                })
                .when(compare, |bar| {
                    bar.child(
                        self.toggle_button(
                            "compare-sync",
                            "",
                            "sync-pan",
                            Cmd::CompareSync,
                            self.compare_sync,
                            cx,
                        )
                        .tooltip(self.tip(
                            if self.compare_sync {
                                "Panes pan and zoom together"
                            } else {
                                "Panes pan independently"
                            },
                            "Shift C",
                        )),
                    )
                })
                .child(
                    self.toggle_button(
                        "focus-peaking",
                        "",
                        "peaking",
                        Cmd::FocusPeaking,
                        self.focus_peaking,
                        cx,
                    )
                    .tooltip(self.tip("Focus peaking · cyan fine-detail highlights", "F")),
                )
            })
            // Destructive action sits apart from rating, at the far edge.
            .child(self.control_divider())
            .child(
                self.button("trash-selection", "", "trash", Cmd::Trash(true), false, cx)
                    .tooltip(self.tip(copy::MOVE_TO_TRASH, "Delete")),
            )
            .into_any_element()
    }
    /// "Fit 45%" (the actual magnification at Fit) or the zoom percentage.
    fn zoom_label(&self) -> String {
        let fit = self.fit_scales.get()[usize::from(!self.reference_focused())];
        if self.zoom {
            format!("{:.0}%", self.zoom_factor * 100.)
        } else if fit.is_finite() && fit > 0. && fit != 1. {
            format!("Fit {:.0}%", fit * 100.)
        } else {
            "Fit".into()
        }
    }
    /// Catalog thumbnail size: S / M / L segments in place of loupe zoom.
    fn grid_size_control(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let effective = self.grid_size_index();
        let mut segments = div()
            .id("grid-size")
            .flex()
            .flex_shrink_0()
            .border_1()
            .border_color(self.group_border());
        let forced = self.grid_size_forced();
        for (i, label) in ui::GRID_SIZE_LABELS.into_iter().enumerate() {
            segments = segments.child(if forced && i > 0 {
                // M and L can't apply while the catalog is narrow: shown
                // disabled, inert to hover and click.
                div()
                    .id(("grid-size", i))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h(px(ui::CONTROL_HEIGHT - 2.))
                    .px(px(space::SM))
                    .text_size(ui::font(12.))
                    .line_height(px(ui::scaled(16.)))
                    .border_b_2()
                    .border_color(transparent_black())
                    .text_color(self.c("text.disabled", "#666666"))
                    .child(label)
            } else {
                self.segment(
                    ("grid-size", i),
                    label,
                    Cmd::GridSize(i),
                    effective == i,
                    cx,
                )
                .h(px(ui::CONTROL_HEIGHT - 2.))
            });
        }
        segments
            .tooltip(self.tip(
                if forced {
                    "Thumbnail size · M and L need a wider catalog · hide a panel"
                } else {
                    "Thumbnail size"
                },
                "- =",
            ))
            .into_any_element()
    }
    fn metadata_row(&self, name: &str, value: String) -> AnyElement {
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap(px(space::MD))
            .min_h(px(ui::COMPACT_CONTROL_HEIGHT))
            .py(px(space::XS))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(self.muted())
                    .child(name.to_owned()),
            )
            .child(
                div()
                    .id(SharedString::from(format!("metadata-{name}")))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_normal()
                    .line_clamp(1)
                    .text_right()
                    .child(if value.is_empty() {
                        "—".into()
                    } else {
                        value.clone()
                    })
                    .tooltip(self.tip(&value, "")),
            )
            .into_any_element()
    }
    fn inspector_section(&self, label: &str) -> Div {
        div()
            .h(px(ui::scaled(ui::INSPECTOR_SECTION_HEIGHT)))
            .flex_shrink_0()
            .mt(px(space::LG))
            .pt(px(space::LG))
            .pb(px(space::SM))
            .border_t_1()
            .border_color(self.border())
            .flex()
            .items_center()
            .justify_between()
            .text_size(ui::font(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.muted())
            .child(label.to_owned())
    }
    fn inspector_view(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let mut body = div()
            .id("inspector-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(ui::PANEL_INSET))
            .pt(px(space::LG))
            .pb(px(space::LG));
        if let Some(summary) = self.selection_summary() {
            // A batch shows what its members carry, not the primary photo.
            body = body.child(self.selection_inspector(&summary, cx));
        } else if let Some(p) = self.review_photo() {
            // The culling line (design R7): what a culler checks between burst
            // frames, under the filename, in the order it is read.
            let mut facts = Vec::new();
            if p.width > 0 && p.height > 0 {
                facts.push(format!("{} × {}", p.width, p.height));
            }
            facts.push(format!("{:.1} MB", p.size_bytes as f64 / 1_000_000.));
            facts.push(file_type(p));
            let exposure = exposure_line(p);
            let captured = p.capture_display();
            let capture_line: Vec<String> = [
                p.capture_short(),
                Some(p.camera.clone()),
                Some(p.lens.clone()),
            ]
            .into_iter()
            .flatten()
            .map(|fact| fact.trim().to_owned())
            .filter(|fact| !fact.is_empty())
            .collect();
            body = body
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(ui::font(ui::text::TITLE))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_normal()
                        .line_clamp(1)
                        .id("inspector-filename")
                        .tooltip(self.tip(&p.filename, ""))
                        .child(p.filename.clone()),
                )
                .when(!exposure.is_empty(), |s| {
                    s.child(
                        div()
                            .id("inspector-exposure")
                            .pt(px(space::XS))
                            .text_size(ui::font(ui::text::BODY))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(exposure.join("  "))
                            .tooltip(self.tip("Shutter · aperture · ISO · focal length", "")),
                    )
                })
                .when(!capture_line.is_empty(), |s| {
                    // The lens is last, so it is what ellipsizes first.
                    s.child(
                        div()
                            .id("inspector-capture")
                            .pt(px(space::XS))
                            .text_size(ui::font(ui::text::LABEL))
                            .text_color(self.muted())
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(capture_line.join(" · "))
                            .tooltip(
                                self.tip(
                                    &[
                                        (captured != "—").then(|| format!("Captured {captured}")),
                                        (!p.camera.trim().is_empty()).then(|| p.camera.clone()),
                                        (!p.lens.trim().is_empty()).then(|| p.lens.clone()),
                                    ]
                                    .into_iter()
                                    .flatten()
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                                    "",
                                ),
                            ),
                    )
                })
                .child(
                    div()
                        .pt(px(space::XS))
                        .text_size(ui::font(ui::text::LABEL))
                        .text_color(self.muted())
                        .child(facts.join(" · ")),
                );
            if self.inspector_info {
                body = body
                    .child(self.inspector_section("File"))
                    .child(
                        self.metadata_row(
                            "Folder",
                            copy::middle_path(
                                &photo_folder(p),
                                // Geist Mono advances 0.6 em; the label and gap take the rest.
                                ((self.docks.inspector
                                    - 2. * ui::PANEL_INSET
                                    - 6. * 7.2
                                    - space::MD)
                                    / 7.2)
                                    .max(8.) as usize,
                            ),
                        ),
                    )
                    .child(self.metadata_row("Type", file_type(p)))
                    .child(self.metadata_row(
                        "Size",
                        format!("{:.1} MB", p.size_bytes as f64 / 1_000_000.),
                    ));
                if captured != "—" {
                    body = body
                        .child(self.inspector_section("Capture"))
                        .child(self.metadata_row("Captured", captured));
                }
            }
            if p.is_remote() {
                body = body.child(
                    div()
                        .id("remote-location")
                        .pt(px(space::SM))
                        .text_color(self.muted())
                        .child("Remote original · decisions saved locally")
                        .tooltip(self.tip(&p.location_display(), "")),
                );
            }

            if let Some((_, frame, count)) = self.burst_slot(p.id).filter(|_| !self.inspector_info)
            {
                body = body.child(
                    self.inspector_section(if self.duplicates_only {
                        "Duplicate set"
                    } else {
                        "Burst"
                    })
                    .child(
                        self.toggle_button(
                            "inspector-survey",
                            "Survey",
                            "group",
                            Cmd::View(ViewMode::Survey),
                            self.mode == ViewMode::Survey,
                            cx,
                        )
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .tooltip(self.tip("Every frame side by side", "S")),
                    ),
                );
                body = body.child(self.metadata_row("Frame", format!("{} of {count}", frame + 1)));
            }
            if self.histogram && !self.inspector_info {
                let preview = self.media.best_photo(p);
                let scale = div()
                    .id("histogram-scale")
                    .flex()
                    .border_1()
                    .border_color(self.group_border())
                    .child(
                        self.segment(
                            "histogram-lin",
                            "LIN",
                            Cmd::SetHistogramLog(false),
                            !self.histogram_log,
                            cx,
                        )
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .px(px(space::SM))
                        .text_size(ui::font(ui::text::LABEL)),
                    )
                    .child(
                        self.segment(
                            "histogram-log",
                            "LOG",
                            Cmd::SetHistogramLog(true),
                            self.histogram_log,
                            cx,
                        )
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .px(px(space::SM))
                        .text_size(ui::font(ui::text::LABEL)),
                    )
                    .tooltip(self.tip("Histogram scale · log lifts shadows and highlights", ""));
                let clipping = self
                    .compact_toggle(
                        "histogram-clipping",
                        "",
                        "warning",
                        Cmd::Clipping,
                        self.clipping,
                        cx,
                    )
                    .tooltip(self.tip(
                        "Show clipping on the photo · blown highlights red, crushed shadows blue",
                        "J",
                    ));
                body = body
                    .child(
                        self.inspector_section("Histogram").child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(space::XS))
                                .child(clipping)
                                .child(scale),
                        ),
                    )
                    .child(div().h(px(4.)));
                if let Some(preview) = &preview {
                    body = body.child(crate::histogram::histogram_with(
                        &preview.histogram,
                        (self.docks.inspector - 2. * ui::PANEL_INSET - 1.).max(1.),
                        122.,
                        crate::histogram::HistogramOptions {
                            scale: if self.histogram_log {
                                crate::histogram::HistogramScale::Log
                            } else {
                                crate::histogram::HistogramScale::Linear
                            },
                            source: if preview.rendered_raw {
                                crate::histogram::HistogramSource::RenderedRaw
                            } else if preview.embedded_raw {
                                crate::histogram::HistogramSource::EmbeddedRaw
                            } else {
                                crate::histogram::HistogramSource::Preview
                            },
                            label: self.muted().into(),
                            label_strong: self.text().into(),
                            label_warning: self.c("warning", "#e0b341").into(),
                            plot: self.histogram_plate().into(),
                            plot_border: self.border().into(),
                            plot_grid: self.c("shadowroom.histogram.grid", "#1c1c1c").into(),
                            ..Default::default()
                        },
                    ));
                } else {
                    body = body.child(div().h(px(122.)).bg(self.histogram_plate()));
                }
            }
            if !self.inspector_info {
                let mut labels = div().flex().gap(px(space::XS)).pt(px(space::XS));
                for (n, label) in [
                    ColorLabel::None,
                    ColorLabel::Red,
                    ColorLabel::Yellow,
                    ColorLabel::Green,
                    ColorLabel::Blue,
                    ColorLabel::Purple,
                ]
                .into_iter()
                .enumerate()
                {
                    labels = labels.child(self.label_swatch(
                        ("label", n),
                        label,
                        p.label == label,
                        if label == ColorLabel::None {
                            "No label".to_owned()
                        } else {
                            format!("{label:?}")
                        },
                        cx,
                    ));
                }
                let label_name = if p.label == ColorLabel::None {
                    String::new()
                } else {
                    format!("{:?}", p.label)
                };
                body = body
                    .child(
                        self.inspector_section("Label")
                            .child(div().text_color(self.text()).child(label_name)),
                    )
                    .child(labels);
            }
            if self.inspector_info {
                let gear: Vec<(&str, String)> =
                    [("Camera", p.camera.clone()), ("Lens", p.lens.clone())]
                        .into_iter()
                        .filter(|(_, v)| !v.trim().is_empty())
                        .collect();
                body = body.child(self.inspector_section("Equipment"));
                if gear.is_empty() {
                    body = body.child(
                        div()
                            .py(px(space::XS))
                            .text_color(self.muted())
                            .child("No camera metadata"),
                    );
                }
                for (name, value) in gear {
                    body = body.child(self.metadata_row(name, value));
                }
                body = body.child(
                    div().pt(px(space::MD)).flex().child(
                        self.button(
                            "reveal-original",
                            reveal_label(),
                            "external",
                            Cmd::Reveal,
                            false,
                            cx,
                        )
                        .tooltip(self.tip("Show the original file; it is never modified", "")),
                    ),
                );
                if let Some(preview) = self.media.best_photo(p) {
                    if let Some(note) = &preview.rendering_note {
                        body = body.child(self.inspector_section("Rendering")).child(
                            div()
                                .text_size(ui::font(11.))
                                .text_color(self.muted())
                                .child(note.clone()),
                        );
                    }
                }
                // Preview diagnostics matter when judging colour or sharpness, not
                // for every frame, so they stay collapsed until asked for.
                let preview = self.media.best_photo(p);
                if let Some(preview) = preview {
                    use shadowroom::media::ColorStatus;
                    let hover = self.hover_bg();
                    body = body.child(
                        self.inspector_section("").child(
                            div()
                                .id("preview-details")
                                .flex_1()
                                .h(px(ui::CONTROL_HEIGHT))
                                .px(px(space::SM))
                                .flex()
                                .items_center()
                                .gap(px(space::XS))
                                .cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .child(
                                    svg()
                                        .path(if self.preview_details {
                                            "down"
                                        } else {
                                            "chevron"
                                        })
                                        .size(px(12.))
                                        .text_color(self.icon_muted()),
                                )
                                .child("Preview details")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.execute(Cmd::PreviewDetails, window, cx)
                                })),
                        ),
                    );
                    if self.preview_details {
                        body = body
                            .child(self.metadata_row(
                                "Source",
                                if preview.rendered_raw {
                                    "Sensor RAW → sRGB".into()
                                } else if preview.embedded_raw {
                                    "Embedded RAW JPEG".into()
                                } else if p.companion_path.is_some() {
                                    if p.companion_path
                                        .as_deref()
                                        .is_some_and(shadowroom::heif::is_heif)
                                    {
                                        "Paired HEIF"
                                    } else {
                                        "Paired JPEG"
                                    }
                                    .into()
                                } else {
                                    "Original".into()
                                },
                            ))
                            .child(self.metadata_row(
                                "Size",
                                format!("{} × {}", preview.width, preview.height),
                            ))
                            .child(self.metadata_row(
                                "Source pixels",
                                format!("{} × {}", preview.source_width, preview.source_height),
                            ))
                            .child(self.metadata_row(
                                "Color",
                                match &preview.color_status {
                                    ColorStatus::ConvertedToSrgb => "ICC → sRGB".into(),
                                    ColorStatus::AssumedSrgb => "Assumed sRGB".into(),
                                    ColorStatus::InvalidProfile(_) => "Invalid ICC profile".into(),
                                },
                            ));
                    }
                }
            }
        } else {
            body = body.child(
                div()
                    .text_color(self.muted())
                    .child("Nothing to inspect in this view"),
            );
        }
        div()
            .w(px(self.docks.inspector))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .h_full()
            .bg(self.panel())
            .border_l_1()
            .border_color(self.border())
            .child(
                div()
                    .id("inspector-tabs")
                    .h(px(TOOLBAR_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .px(px(ui::BAR_INSET))
                    .border_b_1()
                    .border_color(self.border())
                    .child(self.panel_tab(
                        "panel-review",
                        "Review",
                        "loupe",
                        Cmd::InspectorTab(false),
                        !self.inspector_info,
                        cx,
                    ))
                    .child(self.panel_tab(
                        "panel-info",
                        "Info",
                        "camera",
                        Cmd::InspectorTab(true),
                        self.inspector_info,
                        cx,
                    )),
            )
            .child(body)
            .into_any_element()
    }
}

/// The exposure triangle and focal length, the facts that separate burst
/// frames: "1/250 s", "ƒ/2.8", "ISO 400", "35 mm".
fn exposure_line(p: &Photo) -> Vec<String> {
    let shutter = p.shutter.trim();
    [
        (!shutter.is_empty()).then(|| {
            if shutter.ends_with('s') {
                shutter.to_string()
            } else {
                format!("{shutter} s")
            }
        }),
        p.aperture.map(|n| format!("ƒ/{n:.1}")),
        p.iso.map(|n| format!("ISO {n}")),
        p.focal_length.map(|n| format!("{n:.0} mm")),
    ]
    .into_iter()
    .flatten()
    .collect()
}
/// "JPEG", "CR3 + JPEG", "ARW + HEIF": the original's extension and its pair.
fn file_type(p: &Photo) -> String {
    let extension = |path: &Path| {
        path.extension()
            .map(|ext| ext.to_string_lossy().to_uppercase())
            .map(|ext| if ext == "JPG" { "JPEG".to_owned() } else { ext })
    };
    let primary = extension(&p.path).unwrap_or_else(|| "File".into());
    match p.companion_path.as_deref() {
        Some(companion) if shadowroom::heif::is_heif(companion) => format!("{primary} + HEIF"),
        Some(_) => format!("{primary} + JPEG"),
        None => primary,
    }
}
/// The folder an original lives in, as the Info tab shows it.
fn photo_folder(p: &Photo) -> String {
    if p.is_remote() {
        format!("Remote · {}", p.folder)
    } else {
        p.folder.clone()
    }
}
/// Whether this process talks to a Wayland compositor. The environment is
/// fixed for the process lifetime, so render reads a cached answer instead of
/// locking and scanning the environment twice per frame.
fn wayland_session() -> bool {
    static WAYLAND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WAYLAND.get_or_init(|| std::env::var_os("WAYLAND_DISPLAY").is_some())
}
fn transparent_black() -> Hsla {
    rgba(0x00000000).into()
}

fn read_theme_family(path: &Path) -> Result<(String, Vec<Theme>)> {
    use std::io::Read;
    let mut source = String::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut source)?;
    anyhow::ensure!(source.len() <= 4 * 1024 * 1024, "Theme family exceeds 4 MB");
    let themes = Theme::from_zed_json(&source)?;
    Ok((source, themes))
}

fn persist_theme_family(path: &Path, deadline: Option<std::time::Instant>) -> Result<Vec<Theme>> {
    let (source, _) = read_theme_family(path)?;
    persist_theme_source(&source, deadline)
}

fn persist_theme_source(source: &str, deadline: Option<std::time::Instant>) -> Result<Vec<Theme>> {
    let themes = Theme::from_zed_json(&source)?;
    anyhow::ensure!(
        deadline.is_none_or(|deadline| std::time::Instant::now() < deadline),
        "Theme request expired before persistence"
    );
    let directory = shadowroom::settings::settings_path()?
        .parent()
        .context("Configuration directory")?
        .join("themes");
    let hash = blake3::hash(source.as_bytes()).to_hex();
    shadowroom::settings::atomic_write(
        &directory.join(format!("imported-{}.json", &hash[..16])),
        source.as_bytes(),
    )?;
    Ok(themes)
}

impl Workspace {
    fn persistence_state(&self) -> Value {
        json!({"state":self.save_state().as_str(),
            "pending_batches":self.awaiting,"queued_batches":self.outbox.len(),"unsaved_ids":self.unsaved.keys().copied().collect::<Vec<_>>(),"revision":self.save_revisions.revision()})
    }
    fn api_state(&self) -> Value {
        let mut state = json!({"app":"Shadowroom","version":env!("CARGO_PKG_VERSION"),"view":format!("{:?}",self.mode).to_lowercase(),
            "zoom":if self.zoom{"full"}else{"fit"},"selected_id":self.selected_id(),"selected_index":self.selected,
            "selection":self.multi.iter().copied().collect::<Vec<_>>(),"reference_id":self.reference,"focused_id":self.review_photo().map(|p|p.id),"compare_sync":self.compare_sync,"pan":self.pans,
            "total":self.photos.len(),"filtered_count":self.filtered.len(),"filter":format!("{:?}",self.filter).to_lowercase(),
            "min_rating":self.min_rating,"folder":self.scope,"remote_hosts":self.remote_hosts,"grouping":self.settings.grouping_mode,"group_count":self.groups.len(),"similarity_analysis":{"visited":self.analysis_cursor,"total":self.photos.len(),"pending":self.analysis_pending.len(),"errors":self.analysis_errors},
            "groups":self.groups.iter().filter(|g|g.len()>1).take(200).collect::<Vec<_>>(),
            "theme":self.theme.name,"theme_diagnostics":self.theme_diagnostics,"interface_font":self.settings.interface_font,"sidebar":self.sidebar,"inspector":self.inspector,"histogram":self.histogram,
            "trash_count":self.counts.trash,"duplicates_only":self.duplicates_only,"duplicate_scan":self.duplicate_scan,
            "recovery":{"pending":self.recovery_pending,"needs_repair":self.recovery_broken},"persistence":self.persistence_state(),"indexing":self.busy,"status":self.status,"error":self.error,"export_jobs":self.export_jobs,
            "closing":self.quit.closing(),"settings_writable":self.settings_writable,"settings_persistence":{"state":if self.settings_failure.is_some(){"failed"}else if self.prefs_sync.dirty(){"pending"}else{"durable"},"error":self.settings_failure}});
        state["notice"] = json!(self.feedback.flash.as_ref().map(|flash| &flash.text));
        state["auto_advance"] = json!(self.settings.auto_advance);
        state["quit_prompt"] = json!(self.feedback.quit_prompt.as_ref().map(|p| p.title()));
        state["inspector_tab"] = json!(if self.inspector_info {
            "info"
        } else {
            "review"
        });
        state["overlays"] = json!({"rating_filter":self.rating_filter_open && !self.other_overlay_open(),"palette":self.palette,"preferences":self.preferences});
        state["overlays"]["getting_started"] = json!(self.getting_started);
        state["deletion"] = self.deletion.snapshot();
        state["folder_picker"] = self
            .folder_picker
            .as_ref()
            .map(folder_picker::State::snapshot)
            .unwrap_or(Value::Null);
        state["zoom_percent"] = if self.zoom {
            json!(self.zoom_factor * 100.)
        } else {
            Value::Null
        };
        state["focus_peaking"] = json!(self.focus_peaking);
        state["clipping"] = json!(self.clipping);
        state["peaking_style"] = json!({"color": self.peaking_style().color.key(), "sensitivity": self.peaking_style().sensitivity.key()});
        state["focus_mode"] = json!(self.focus_mode);
        state["grid_size"] = json!(["s", "m", "l"][self.grid_size.min(2)]);
        state["pending_bulk"] = json!(self.pending_bulk.as_ref().map(|bulk| bulk.count));
        state["label_filter"] = json!(self.label_filter.map(ColorLabel::as_str));
        state["image_context"] = self.image_context_state();
        state["pane"] = json!({"focused":self.focused_pane().as_str(),"ring":self.pane_ring,
            "library_cursor":self.library_cursor_id().map(|id| id.to_string())});
        state["catalog_hover"] = json!(self.catalog_hover);
        state["fit_zoom_percent"] = json!(self.fit_scales.get()[1] * 100.);
        state["accessibility"] = json!({"text_scale":ui::text_scale(),
            "high_contrast":platform_prefs::high_contrast(),
            "reduce_motion":self.reduce_motion(),
            "native_chooser_failed":self.native_chooser_failed,
            "automation_driven":self.automation_driven});
        state["review_subset"] = if self.review_folder == self.scope {
            json!(self.review_subset)
        } else {
            Value::Null
        };
        state["catalog_scroll"] = json!({"offset":f32::from(self.grid_scroll.0.borrow().base_handle.offset().y),"visible_rows":self.grid_range,"columns":self.grid_cols()});
        state["layout"] = json!({"workspace_width":self.workspace_size.0,"workspace_height":self.workspace_size.1,"center_width":self.docks.center,"photo_height":self.center_size.1,"sidebar_width":self.docks.library,"inspector_width":self.docks.inspector,"preferred_sidebar_width":self.settings.sidebar_width,"preferred_inspector_width":self.settings.inspector_width,"sidebar_visible":self.docks.library>0.,"inspector_visible":self.docks.inspector>0.,"resizing":self.dock_drag.is_some()});
        if self.import_ui.open {
            state["layout"]["import"] = self.ingest_layout_state();
        }
        state
    }
    fn api_ids(&self, params: &Value) -> std::result::Result<Vec<i64>, String> {
        let ids = if let Some(ids) = params.get("ids").and_then(Value::as_array) {
            ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>()
        } else if self.multi.is_empty() {
            self.selected_id().into_iter().collect()
        } else {
            self.multi.iter().copied().collect()
        };
        if ids.is_empty() {
            return Err("No photographs selected".into());
        }
        for id in &ids {
            if !self.photo_index.contains_key(id) {
                return Err(format!("Unknown photo ID: {id}"));
            }
        }
        Ok(ids)
    }
    fn api_decide(
        &mut self,
        params: &Value,
        rating: Option<u8>,
        flag: Option<Flag>,
        label: Option<ColorLabel>,
    ) -> std::result::Result<Value, String> {
        let ids = self.api_ids(params)?;
        let ids: HashSet<_> = ids.into_iter().collect();
        let before = self.decisions_for(&ids);
        let after: Vec<_> = before
            .iter()
            .map(|d| Decision {
                rating: rating.unwrap_or(d.rating),
                flag: flag.unwrap_or(d.flag),
                label: label.unwrap_or(d.label),
                ..*d
            })
            .collect();
        if !self.apply_decisions(after.clone()) {
            return Err(self.error.as_ref().map_or_else(
                || "Decision was not admitted".into(),
                |notice| notice.text.clone(),
            ));
        }
        self.history.push(before, after);
        self.rebuild_after_decisions(self.selected_id());
        Ok(json!({"accepted":true,"changed_ids":ids,"persistence":self.persistence_state()}))
    }
    fn api_cache(&self) -> Value {
        let s = self.media.stats();
        json!({"queued":s.queued,"active":s.active,"memory_bytes":s.memory_bytes,"memory_budget_bytes":s.memory_budget_bytes,"texture_cpu_bytes":self.texture_bytes,"gpu_pixel_bytes_estimate":self.texture_bytes,"source_reads":s.source_reads,"memory_hits":s.memory_hits,"disk_hits":s.disk_hits,"decoded":s.decoded,"errors":s.errors,"recent_errors":self.media.recent_errors(),"disk_bytes":s.disk_bytes,"inflight_bytes":s.inflight_bytes,"retired_bytes":s.retired_bytes,"display_bytes":s.display_bytes,"peak_estimated_bytes":s.peak_estimated_bytes,"disk_writes":s.disk_writes,"persistence_dropped":s.persistence_dropped})
    }
    fn update_settings(&mut self, patch: &Value) -> std::result::Result<Value, String> {
        if !self.settings_writable {
            return Err(
                "Preferences file could not be loaded; repair it before replacing settings".into(),
            );
        }
        let mut next = serde_json::to_value(&self.settings).map_err(|e| e.to_string())?;
        for (key, value) in patch.as_object().ok_or("Settings object required")? {
            next[key] = value.clone();
        }
        let mut next: Settings = serde_json::from_value(next).map_err(|e| e.to_string())?;
        next.normalize();
        let theme = self
            .themes
            .iter()
            .find(|t| t.name == next.theme)
            .cloned()
            .ok_or_else(|| format!("Unknown theme: {}", next.theme))?;
        if !INTERFACE_FONTS.contains(&next.interface_font.as_str()) {
            return Err("Interface font must be DM Sans, Geist Mono, or DM Mono".into());
        }
        if let Some(id) = patch.get("last_photo_id").and_then(Value::as_i64) {
            if !self.photo_index.contains_key(&id) {
                return Err(format!("Unknown photo ID: {id}"));
            }
        }
        for role in ["theme_light", "theme_dark"] {
            if let Some(name) = next.extra.get(role).and_then(Value::as_str) {
                if !self.themes.iter().any(|theme| theme.name == name) {
                    return Err(format!("Unknown {role}: {name}"));
                }
            }
        }
        let memory_changed = next.memory_budget_mb != self.settings.memory_budget_mb;
        let import_changed = patch
            .as_object()
            .is_some_and(|settings| settings.keys().any(|key| key.starts_with("ingest_")));
        self.settings = next;
        if import_changed && !self.import_ui.active() {
            self.import_ui.defaults(&self.settings);
        }
        self.theme = theme;
        self.fallback_theme = if self.theme.appearance == "light" {
            Theme::light()
        } else {
            Theme::dark()
        };
        self.theme_colors.borrow_mut().clear();
        self.palette_revision = self.palette_revision.wrapping_add(1);
        self.inspector = self.settings.show_inspector;
        self.sidebar = self.settings.show_sidebar;
        self.dock_drag = None;
        self.mode = match self.settings.view_mode.as_str() {
            "grid" => ViewMode::Grid,
            "compare" => ViewMode::Compare,
            "survey" => ViewMode::Survey,
            _ => ViewMode::Loupe,
        };
        self.filter = match self.settings.filter.as_str() {
            "picks" => Filter::Picks,
            "unrated" => Filter::Unrated,
            "rejects" => Filter::Rejects,
            "trash" => Filter::Trash,
            _ => Filter::All,
        };
        self.min_rating = self.settings.min_rating;
        self.media
            .set_disk_budget(self.settings.cache_budget_bytes() as u64);
        if memory_changed {
            self.media
                .set_memory_budget(self.settings.memory_budget_bytes());
        }
        self.regroup();
        self.rebuild(self.selected_id());
        self.save_settings();
        return Ok(
            json!({"accepted":true,"settings":self.settings,"restart_required":[],"settings_save":"pending","decision_persistence":self.persistence_state()}),
        );
    }
    fn api_call(
        &mut self,
        method: &str,
        p: &Value,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> std::result::Result<Value, String> {
        if method == "duplicates" && p["action"] == "status" {
            return Ok(json!({"scan":self.duplicate_scan,"duplicates_only":self.duplicates_only}));
        }
        if self.deletion.open()
            && !matches!(
                method,
                "workspace_state"
                    | "settings_get"
                    | "library_list"
                    | "photo_get"
                    | "cache_stats"
                    | "quit"
            )
        {
            return Err(
                "A file-deletion confirmation or operation is open; finish it in the app first"
                    .into(),
            );
        }
        if self.quit.closing()
            && ![
                "workspace_state",
                "settings_get",
                "library_list",
                "photo_get",
                "cache_stats",
                "ingest_status",
                "remote_hosts",
                "remote_status",
                "remote_roots",
            ]
            .contains(&method)
        {
            return Err("The app is closing; no further mutation was performed".into());
        }
        if self.recovery_pending
            && !matches!(
                method,
                "workspace_state"
                    | "settings_get"
                    | "library_list"
                    | "photo_get"
                    | "cache_stats"
                    | "ingest_status"
                    | "remote_hosts"
                    | "remote_status"
                    | "remote_roots"
                    | "quit"
            )
            && !(method == "command" && p["name"] == "retry_saves")
        {
            return Err("Recovery is pending; new mutations wait for durable replay.".into());
        }
        if method.starts_with("ingest_") || method == "eject" {
            return self.ingest_api(method, p);
        }
        match method {
            "remote_hosts" | "remote_status" => {
                if let Some(id) = p["host"].as_str() {
                    return self
                        .remote_hosts
                        .get(id)
                        .map(|h| json!({"host":h}))
                        .ok_or_else(|| "Unknown remote host".into());
                }
                return Ok(json!({"hosts":self.remote_hosts.values().collect::<Vec<_>>() }));
            }
            "remote_roots" => {
                let id = p["host"].as_str().ok_or("Host id required")?;
                let host = self.remote_hosts.get(id).ok_or("Unknown remote host")?;
                let roots: Vec<_> = host.roots.iter().map(|root| json!({"id":root.id,"volume_id":root.volume_id,"identity_kind":root.identity_kind,"path":root.path,"label":root.label,"scope":host.hello.as_ref().map(|h|shadowroom::model::remote_root_scope(&h.device_id,&root.id))})).collect();
                return Ok(json!({"host":id,"state":host.state(),"roots":roots}));
            }
            "remote_host_add" => {
                let alias = p["ssh_alias"].as_str().unwrap_or("");
                let config = shadowroom::network::HostConfig {
                    id: p["id"].as_str().unwrap_or(alias).into(),
                    display_name: p["display_name"].as_str().unwrap_or(alias).into(),
                    ssh_alias: alias.into(),
                    remote_command: p["remote_command"]
                        .as_str()
                        .unwrap_or("shadowroom-remote serve --stdio")
                        .into(),
                    extra_ssh_args: p["extra_ssh_args"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    local_program: p["local_program"].as_str().map(PathBuf::from),
                    local_args: p["local_args"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                config.validate().map_err(|e| e.to_string())?;
                if self.remote_hosts.contains_key(&config.id) {
                    return Err("Host id already exists".into());
                }
                let id = config.id.clone();
                if !self.enqueue(IoCommand::Remote(shadowroom::network::RemoteCommand::Add(
                    config,
                ))) {
                    return Err("Host add was not queued".into());
                }
                return Ok(
                    json!({"accepted":true,"host":id,"state":"checking","diagnostic":"Poll remote_status for the protocol/hello result"}),
                );
            }
            "remote_host_remove" | "remote_connect" | "remote_disconnect" | "remote_index"
            | "remote_sync" | "remote_cancel" => {
                use shadowroom::network::RemoteCommand as R;
                let id = p["host"].as_str().ok_or("Host id required")?.to_string();
                let host = self
                    .remote_hosts
                    .get(&id)
                    .ok_or("Unknown remote host (wait for remote_host_add to complete)")?;
                let command = match method {
                    "remote_host_remove" => R::Remove(id.clone()),
                    "remote_connect" => R::Connect(id.clone()),
                    "remote_disconnect" => R::Disconnect(id.clone()),
                    "remote_cancel" => R::Cancel(id.clone()),
                    _ => {
                        let root = p["root"].as_str().ok_or("Root id required")?.to_string();
                        if !host.roots.iter().any(|r| r.id == root) {
                            return Err("Unknown root id; use remote_roots".into());
                        }
                        if method == "remote_index" {
                            R::Index {
                                host: id.clone(),
                                root,
                            }
                        } else {
                            if p["full"].as_bool().unwrap_or(false) {
                                R::SyncFull {
                                    host: id.clone(),
                                    root,
                                }
                            } else {
                                R::Sync {
                                    host: id.clone(),
                                    root,
                                }
                            }
                        }
                    }
                };
                if !self.enqueue(IoCommand::Remote(command)) {
                    return Err("Remote command was not queued".into());
                }
                return Ok(json!({"accepted":true,"host":id,"operation":method}));
            }
            "workspace_state" => return Ok(self.api_state()),
            "cache_stats" => return Ok(self.api_cache()),
            "settings_get" => {
                return serde_json::to_value(&self.settings).map_err(|e| e.to_string());
            }
            "library_list" => {
                let offset = p["offset"].as_u64().unwrap_or(0) as usize;
                let limit = p["limit"].as_u64().unwrap_or(200).min(1000) as usize;
                let all = p["scope"].as_str() == Some("all");
                let total = if all {
                    self.photos.len()
                } else {
                    self.filtered.len()
                };
                let photos = if all {
                    self.photos
                        .iter()
                        .skip(offset)
                        .take(limit)
                        .collect::<Vec<_>>()
                } else {
                    self.filtered
                        .iter()
                        .skip(offset)
                        .take(limit)
                        .map(|&i| &self.photos[i])
                        .collect()
                };
                return Ok(
                    json!({"photos":photos,"total":total,"offset":offset,"next_offset":if offset+photos.len()<total{Some(offset+photos.len())}else{None}}),
                );
            }
            "photo_get" => {
                let id = p["id"].as_i64().ok_or("Photo ID required")?;
                return self
                    .photo_index
                    .get(&id)
                    .map(|&i| &self.photos[i])
                    .map(|p| {
                        let display = [2u8, 1, 0].into_iter().find_map(|tier| {
                            let effect = self.display_effect(PreviewKind::Fit);
                            self.textures.get(&(p.id, tier + effect, preview_mtime(p), preview_size(p)))
                                .map(|texture| (texture, Self::effect_peaks(effect)))
                                .or_else(|| self.textures.get(&(p.id, tier, preview_mtime(p), preview_size(p))).map(|texture| (texture, false)))
                        }).map(|(t, peaking)| json!({"kind":t.kind,"width":t.width,"height":t.height,"peaking":peaking}));
                        let preview = self.media.best_photo(p).map(|v| json!({
                            "width":v.width,"height":v.height,"kind":v.kind,
                            "source_width":v.source_width,"source_height":v.source_height,
                            "embedded_raw":v.embedded_raw,"rendered_raw":v.rendered_raw,
                            "rendering_note":v.rendering_note,"color_status":v.color_status,
                        }));
                        json!({"photo":p,"unsaved":self.unsaved.contains_key(&id),
                            "preview":preview,"display_texture":display,"full_error":self.media.photo_error(p,PreviewKind::Full).map(|e|e.message)})
                    })
                    .ok_or_else(|| format!("Unknown photo ID: {id}"));
            }
            "trash" => {
                let ids = self.api_ids(p)?;
                let trashed = p["trashed"].as_bool().ok_or("Trash state required")?;
                self.trash_photos(&ids, trashed)?;
                cx.notify();
                return Ok(
                    json!({"accepted":true,"changed_ids":ids,"trashed":trashed,"originals_untouched":true,"persistence":self.persistence_state()}),
                );
            }
            "duplicates" => match p["action"].as_str() {
                Some("scan") => {
                    let ids = if p.get("ids").is_some() {
                        self.api_ids(p)?
                    } else {
                        Vec::new()
                    };
                    let id = self.start_duplicate_scan(ids)?;
                    cx.notify();
                    return Ok(
                        json!({"accepted":true,"job_id":id,"state":"queued","originals_untouched":true}),
                    );
                }
                Some("cancel") => {
                    if !self.enqueue(IoCommand::CancelDuplicates) {
                        return Err("Could not request duplicate scan cancellation".into());
                    }
                }
                _ => return Err("Invalid duplicate scan action".into()),
            },
            "rate" => {
                return self.api_decide(
                    p,
                    Some(p["rating"].as_u64().ok_or("Rating required")? as u8),
                    None,
                    None,
                );
            }
            "flag" => {
                let flag = match p["flag"].as_str() {
                    Some("pick") => Flag::Pick,
                    Some("reject") => Flag::Reject,
                    Some("none") => Flag::None,
                    _ => return Err("Invalid flag".into()),
                };
                return self.api_decide(p, None, Some(flag), None);
            }
            "label" => {
                let label = ColorLabel::from(p["label"].as_str().ok_or("Label required")?);
                return self.api_decide(p, None, None, Some(label));
            }
            "select" => {
                let ids = if let Some(id) = p["id"].as_i64() {
                    vec![id]
                } else if p.get("ids").is_some() {
                    self.api_ids(p)?
                } else {
                    let position = p["index"].as_u64().ok_or("Selection required")? as usize;
                    vec![
                        self.filtered
                            .get(position)
                            .and_then(|&i| self.photos.get(i))
                            .ok_or("Index is outside filtered results")?
                            .id,
                    ]
                };
                for id in &ids {
                    if !self.photo_index.contains_key(id) {
                        return Err(format!("Unknown photo ID: {id}"));
                    }
                    if self.visible_position(*id).is_none() {
                        return Err(format!("Photo {id} is excluded by the current filter"));
                    }
                }
                let pos = self
                    .visible_position(ids[0])
                    .ok_or("Photo is not visible")?;
                self.select(pos, p["extend"].as_bool().unwrap_or(false), false, cx);
                if ids.len() > 1 {
                    self.multi.extend(ids);
                }
            }
            "undo" => self.execute(Cmd::Undo, window, cx),
            "redo" => self.execute(Cmd::Redo, window, cx),
            "view" => {
                let mode = match p["mode"].as_str() {
                    Some("grid") => ViewMode::Grid,
                    Some("loupe") => ViewMode::Loupe,
                    Some("compare") => ViewMode::Compare,
                    Some("survey") => ViewMode::Survey,
                    _ => return Err("Invalid view".into()),
                };
                // Automation addresses its explicit selection, independently
                // of whichever image happens to be under the user's pointer.
                self.catalog_hover = None;
                self.execute(
                    if mode == ViewMode::Loupe {
                        Cmd::ReviewSelection
                    } else {
                        Cmd::View(mode)
                    },
                    window,
                    cx,
                );
                if let Some(panel) = p["panel"].as_str() {
                    self.execute(Cmd::InspectorTab(panel == "info"), window, cx);
                }
            }
            "zoom" => {
                let mode = p["mode"].as_str().ok_or("Zoom mode required")?;
                match mode {
                    "full" => self.execute(Cmd::ZoomTo(1.), window, cx),
                    "fit" => {
                        if self.zoom {
                            self.execute(Cmd::Zoom, window, cx);
                        }
                    }
                    "toggle" => self.execute(Cmd::Zoom, window, cx),
                    "in" => self.execute(Cmd::ZoomStep(1), window, cx),
                    "out" => self.execute(Cmd::ZoomStep(-1), window, cx),
                    "custom" => {
                        let percent = p["percent"].as_f64().ok_or("Zoom percent required")?;
                        if !(12.5..=800.).contains(&percent) {
                            return Err("Zoom percent must be between 12.5 and 800".into());
                        }
                        self.execute(Cmd::ZoomTo(percent as f32 / 100.), window, cx);
                    }
                    _ => return Err("Invalid zoom mode".into()),
                }
            }
            "pin" => {
                let id = p["id"]
                    .as_i64()
                    .or(self.selected_id())
                    .ok_or("No photograph to pin")?;
                if !self.photo_index.contains_key(&id) {
                    return Err(format!("Unknown photo ID: {id}"));
                }
                self.reference = Some(id);
            }
            "filter" => {
                let filter = if let Some(f) = p["filter"].as_str() {
                    match f {
                        "all" => Filter::All,
                        "picks" => Filter::Picks,
                        "unrated" => Filter::Unrated,
                        "rejects" => Filter::Rejects,
                        "trash" => Filter::Trash,
                        _ => return Err("Invalid filter".into()),
                    }
                } else {
                    self.filter
                };
                self.filter = filter;
                self.reference_focus = false;
                self.duplicates_only = p["duplicates_only"].as_bool().unwrap_or(false);
                if let Some(r) = p["min_rating"].as_u64() {
                    self.min_rating = r as u8;
                }
                if p["clear_folder"] == true {
                    self.scope = None;
                } else if let Some(folder) = p["folder"].as_str() {
                    self.scope = Some(folder.into());
                }
                self.settings.filter = format!("{filter:?}").to_lowercase();
                self.settings.min_rating = self.min_rating;
                self.save_settings();
                self.rebuild(self.selected_id());
            }
            "group" => {
                self.settings.grouping_mode =
                    p["mode"].as_str().ok_or("Grouping mode required")?.into();
                if let Some(g) = p["gap_ms"].as_u64() {
                    self.settings.burst_gap_ms = g;
                }
                if let Some(h) = p["threshold"].as_u64() {
                    self.settings.visual_threshold = h as u32;
                }
                self.regroup();
            }
            "settings_set" => return self.update_settings(&p["settings"]),
            "import" => {
                if !self.enqueue(IoCommand::Import(PathBuf::from(
                    p["path"].as_str().ok_or("Path required")?,
                ))) {
                    return Err("Import was not queued".into());
                }
            }
            "cancel_import" => {
                self.enqueue(IoCommand::CancelImport);
            }
            "export_picks" | "export_xmp" => {
                // Picks without ids are every catalog pick; ids scope them.
                let ids = if p.get("ids").is_some() {
                    self.api_ids(p)?
                } else if method == "export_picks" {
                    Vec::new()
                } else {
                    self.photos.iter().map(|p| p.id).collect()
                };
                let destination = PathBuf::from(
                    p["destination"]
                        .as_str()
                        .ok_or("Export destination required")?,
                );
                let id = self.queue_export(destination, method == "export_picks", ids)?;
                return Ok(
                    json!({"accepted":true,"job_id":id,"state":"queued","persistence":self.persistence_state()}),
                );
            }
            "command" => {
                if p["name"] == "show_rating_filter" && self.other_overlay_open() {
                    return Err("Close the current dialog before opening the rating filter".into());
                }
                let command = match p["name"].as_str().ok_or("Command required")? {
                    "open_folder_picker" => Cmd::OpenFolder,
                    "open_theme_picker" => Cmd::ImportTheme,
                    "next_unreviewed" => Cmd::Unreviewed(1),
                    "previous_unreviewed" => Cmd::Unreviewed(-1),
                    "resolve_burst" => Cmd::ResolveBurst,
                    "next" => Cmd::Next,
                    "previous" => Cmd::Previous,
                    "next_group" => Cmd::NextGroup,
                    "previous_group" => Cmd::PreviousGroup,
                    "toggle_histogram" => Cmd::Histogram,
                    "show_rating_filter" => Cmd::RatingFilter,
                    "toggle_histogram_log" => Cmd::HistogramLog,
                    "toggle_compare_sync" => Cmd::CompareSync,
                    "retry_saves" => Cmd::RetrySaves,
                    "retry_preview" => Cmd::RetryPreview,
                    "toggle_inspector" => Cmd::Inspector,
                    "toggle_sidebar" => Cmd::Sidebar,
                    "reset_layout" => Cmd::ResetLayout,
                    "resize_catalog" => Cmd::ResizeDock(DockSide::Library),
                    "resize_inspector" => Cmd::ResizeDock(DockSide::Inspector),
                    "show_duplicates" => Cmd::ShowDuplicates,
                    "scan_duplicates_in_view" => Cmd::ScanDuplicates,
                    "toggle_palette" => Cmd::Palette,
                    "toggle_preferences" => Cmd::Preferences,
                    "close_overlay" => Cmd::Close,
                    "getting_started" => Cmd::GettingStarted,
                    "toggle_focus_peaking" => Cmd::FocusPeaking,
                    "toggle_clipping" => Cmd::Clipping,
                    "show_full_folder" => Cmd::ClearReviewSubset,
                    "reset_decision" => Cmd::ResetDecision,
                    "show_appearance" => Cmd::Appearance,
                    "toggle_auto_advance" => Cmd::AutoAdvance,
                    "toggle_focus" => Cmd::Focus,
                    "select_all" => Cmd::AllSelect,
                    "refresh" => Cmd::Refresh,
                    _ => return Err("Unknown command".into()),
                };
                self.execute(command, window, cx);
            }
            "quit" => self.request_quit(cx),
            _ => return Err(format!("Unknown method: {method}")),
        }
        cx.notify();
        Ok(
            json!({"accepted":true,"workspace":self.api_state(),"persistence":self.persistence_state()}),
        )
    }
    fn drain_automation(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        for _ in 0..8 {
            let Some(request) = self
                .automation
                .as_ref()
                .and_then(|server| server.requests.try_recv().ok())
            else {
                break;
            };
            self.handle_automation(request, window, cx);
        }
    }
    fn handle_automation(
        &mut self,
        request: AutomationRequest,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // A driven session keeps the built-in path picker, whose state
        // automation can read; see `folder_picker::show_path_picker`.
        self.automation_driven = true;
        if request.is_expired() {
            let _ = request.reply.try_send(Err(
                "Request expired before reaching the UI; no action performed".into(),
            ));
            return;
        }
        if request.method == "theme_import" {
            let path = PathBuf::from(request.params["path"].as_str().unwrap_or_default());
            let deadline = request.deadline;
            let task = cx
                .background_executor()
                .spawn(async move { persist_theme_family(&path, Some(deadline)) });
            cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
                    if request.is_expired(){let _=request.reply.try_send(Err("Theme import expired before activation".into()));return;}
                    let response=match result{Ok(themes)=>{let name=themes.first().map(|t|t.name.clone());for theme in themes{this.themes.retain(|t|t.name!=theme.name);this.themes.push(theme);}if let Some(name)=name{this.settings.theme=name.clone();this.theme=this.themes.iter().find(|t|t.name==name).unwrap().clone();this.fallback_theme=if this.theme.appearance=="light"{Theme::light()}else{Theme::dark()};
                    this.theme_colors.borrow_mut().clear();
                    this.palette_revision = this.palette_revision.wrapping_add(1);this.settings.extra.insert("theme_mode".into(),json!("manual"));this.save_settings();}Ok(json!({"accepted":true,"theme":this.theme.name,"persisted_family":true}))},Err(e)=>Err(format!("Theme import: {e:#}"))};let _=request.reply.try_send(response);cx.notify();
                });}).detach();
            return;
        }
        let result = self.api_call(&request.method, &request.params, window, cx);
        if !matches!(
            request.method.as_str(),
            "workspace_state" | "settings_get" | "library_list" | "photo_get" | "cache_stats"
        ) {
            cx.notify();
            // Automation can change view state with no input event; force a full
            // window redraw so the change is presented even when the previous frame
            // did not track this entity (observed stale frames after Grid view).
            window.refresh();
        }
        let _ = request.reply.try_send(result);
    }
}

impl Workspace {
    fn palette_view(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let cache = self.matches();
        let total = cache.matches.len();
        // Matches can shrink while the palette is open (busy, scan or theme
        // changes); never index or window past the current list.
        let selected = self.palette_index.min(total.saturating_sub(1));
        // A full page of rows even when the selection wrapped to the end.
        let start = selected
            .saturating_sub(PALETTE_PAGE / 2)
            .min(total.saturating_sub(PALETTE_PAGE));
        let end = (start + PALETTE_PAGE).min(total);
        let mut results = div().flex().flex_col().py(px(space::XS));
        for (n, &index) in cache
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let item = &cache.items[index];
            let active = n == selected;
            let hover = self.hover_bg();
            let cmd = item.command.clone();
            let title = item.title.clone();
            let recent = self.palette_query.is_empty() && self.palette_recent.contains(&item.title);
            let matched = HighlightStyle {
                color: Some(self.text()),
                font_weight: Some(FontWeight::MEDIUM),
                ..HighlightStyle::default()
            };
            // "Category · description": description inline, category in a right column.
            let (category, description) = match item.detail.split_once(" · ") {
                Some((category, description)) => (category.to_string(), description.to_string()),
                None => (item.detail.clone(), String::new()),
            };
            results = results.child(
                div()
                    .id(("command", n))
                    .h(px(ui::ROW_HEIGHT))
                    .px(px(ui::PANEL_INSET))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .relative()
                    .cursor_pointer()
                    .when(active, |s| {
                        // The row Return runs: fill, a 2 px accent bar and full
                        // text, so it reads even where the fill is subtle.
                        s.bg(self.selected_bg()).child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(2.))
                                .bg(self.accent()),
                        )
                    })
                    .when(!active, |s| s.hover(move |s| s.bg(hover)))
                    // The pointer moves the selection, so hover and Return agree.
                    .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                        if this.palette_index != n {
                            this.palette_index = n;
                            cx.notify();
                        }
                    }))
                    .child(icon(
                        item.icon,
                        if active {
                            self.c("icon", "#b8b8b8")
                        } else {
                            self.icon_muted()
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_baseline()
                            .gap(px(space::SM))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            // Matched letters in MEDIUM and full text colour say
                            // why a row is listed (OV-19).
                            .child(
                                div().flex_shrink_0().child(
                                    StyledText::new(item.title.clone()).with_highlights(
                                        title_match(&self.palette_query, &item.title)
                                            .into_iter()
                                            .map(|range| (range, matched)),
                                    ),
                                ),
                            )
                            // The description gives way first and ends in an
                            // ellipsis, never mid-glyph (R4-overlays-07).
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(ui::font(11.))
                                    .text_color(self.muted())
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(description),
                            ),
                    )
                    // Keys, then the category in a fixed column, so categories
                    // line up whether or not a row has a key (OV-20).
                    .when(!item.shortcut.is_empty(), |s| {
                        s.child(self.keycaps(&item.shortcut))
                    })
                    .child(
                        div()
                            .w(px(ui::scaled(PALETTE_CATEGORY_WIDTH)))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(ui::font(11.))
                            // Blue marks only where you are; "Recent" is a
                            // plain tag (and full text on the active row).
                            .text_color(if active { self.text() } else { self.muted() })
                            .child(if recent {
                                "Recent".to_string()
                            } else {
                                category
                            }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_palette_command(cmd.clone(), title.clone(), window, cx);
                    })),
            )
        }
        if total == 0 {
            let query = self.palette_query.trim();
            let (title, hint) = match (self.palette_photos, query.is_empty()) {
                (true, true) => (
                    "Type part of a filename".to_owned(),
                    format!(
                        "“4512” finds IMG_4512.CR3 · {} also searches commands and folders",
                        shortcut("⌘K")
                    ),
                ),
                (true, false) => (
                    format!("No photos match “{query}”"),
                    "Filenames only, across the whole library · Esc closes".to_owned(),
                ),
                (false, _) => (
                    format!("No commands, photos or folders match “{query}”"),
                    "Try export, zoom or a filename · Esc closes".to_owned(),
                ),
            };
            results = results.child(
                div()
                    .px(px(ui::PANEL_INSET))
                    .py(px(space::MD))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .child(title)
                    .child(
                        div()
                            .text_size(ui::font(11.))
                            .text_color(self.muted())
                            .child(hint),
                    ),
            );
        }
        // `items_start` lets the panel hug its results instead of stretching to max_h.
        self.modal_scrim(Overlay::Palette)
            .items_start()
            .pt(px(72.))
            .child(
                self.modal_surface("command-palette", dialog::DIALOG_SMALL)
                    .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // Outside-click dismissal belongs to the topmost overlay only.
                        if event.button == MouseButton::Left
                            && this.top_overlay() == Some(Overlay::Palette)
                        {
                            cx.stop_propagation();
                            this.execute(Cmd::Close, window, cx);
                        }
                    }))
                    .h_auto()
                    .max_h(px(440.))
                    .child(
                        div()
                            .h(px(TOOLBAR_HEIGHT))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .px(px(ui::PANEL_INSET))
                            .gap(px(space::SM))
                            .border_b_1()
                            .border_color(self.border())
                            .text_size(ui::font(13.))
                            .child(icon("search", self.icon_muted()))
                            .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(
                                self.input_field(
                                    &self.palette_edit,
                                    &self.palette_query,
                                    if self.palette_photos {
                                        "Find a photo by filename…"
                                    } else {
                                        "Run a command, or find a photo or folder…"
                                    },
                                    true,
                                    true,
                                ),
                            )),
                    )
                    .child(results),
            )
            .into_any_element()
    }
    fn preference_row(&self, label: &str, description: &str, controls: AnyElement) -> AnyElement {
        div()
            .py(px(space::SM))
            .border_b_1()
            .border_color(self.border())
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::LG))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .child(label.to_owned())
                    .child(
                        div()
                            .text_color(self.muted())
                            .text_size(ui::font(11.))
                            .child(description.to_owned()),
                    ),
            )
            .child(controls)
            .into_any_element()
    }
    fn choices(
        &self,
        id: &str,
        options: impl IntoIterator<Item = (String, Cmd, bool)>,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .flex()
            .flex_shrink_0()
            .border_1()
            .border_color(self.group_border());
        for (i, (label, cmd, active)) in options.into_iter().enumerate() {
            row = row.child(self.segment(
                SharedString::from(format!("{id}-{i}")),
                &label,
                cmd,
                active,
                cx,
            ));
        }
        row.into_any_element()
    }
    /// Preference choice group: a radio group for the keyboard. Only the active
    /// option is a Tab stop; arrows select the neighbouring option. Disabled
    /// groups (settings that do not apply) are dimmed and inert.
    fn pref_choices(
        &self,
        id: &str,
        options: impl IntoIterator<Item = (String, Cmd, bool)>,
        disabled: bool,
        window: &Window,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let options: Vec<_> = options.into_iter().collect();
        let count = options.len();
        let active_index = options
            .iter()
            .position(|(_, _, active)| *active)
            .unwrap_or(0);
        let group = SharedString::from(id.to_owned());
        let mut row = div()
            .flex()
            .flex_shrink_0()
            .border_1()
            .border_color(self.border())
            .when(disabled, |s| s.opacity(0.45));
        for (i, (label, cmd, active)) in options.into_iter().enumerate() {
            let key = format!("{id}-{i}");
            let segment = if disabled {
                div()
                    .id(SharedString::from(key))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h(px(ui::CONTROL_HEIGHT))
                    .px(px(space::SM))
                    .text_size(ui::font(12.))
                    .line_height(px(ui::scaled(16.)))
                    .text_color(if active { self.text() } else { self.muted() })
                    .when(active, |s| s.bg(self.selected_bg()))
                    .child(label)
            } else {
                let segment = self.segment(
                    SharedString::from(key.clone()),
                    &label,
                    cmd.clone(),
                    active,
                    cx,
                );
                self.focusable(
                    segment,
                    &key,
                    2,
                    i == active_index,
                    FocusTarget::option(cmd, group.clone(), i, count),
                    window,
                    cx,
                )
            };
            row = row.child(segment);
        }
        row.into_any_element()
    }
    /// A focusable dialog button for Preferences (`tab_index` orders the
    /// sections: tabs 1, body 2, footer 3, close 4).
    fn pref_button(
        &self,
        element: Stateful<Div>,
        key: &str,
        tab_index: isize,
        cmd: Cmd,
        window: &Window,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        self.focusable(
            element,
            key,
            tab_index,
            true,
            FocusTarget::button(cmd),
            window,
            cx,
        )
    }
    /// Renders staged preferences with explicit apply and cancel actions.
    fn preferences_view(&self, window: &Window, cx: &gpui::Context<Self>) -> AnyElement {
        self.focus_scope(FocusScope::Preferences);
        let s = self.preference_settings();
        let fonts = self.pref_choices(
            "font",
            INTERFACE_FONTS
                .iter()
                .copied()
                .map(|f| (f.to_string(), Cmd::Font(f.into()), s.interface_font == f)),
            false,
            window,
            cx,
        );
        let groups = self.pref_choices(
            "group-mode",
            [
                ("off", "Off"),
                ("time", "Time"),
                ("visual", "Visual"),
                ("burst", "Burst"),
            ]
            .map(|(k, l)| (l.to_string(), Cmd::Group(k.into()), s.grouping_mode == k)),
            false,
            window,
            cx,
        );
        // Every grouping mode splits on the time gap; only Visual and Burst
        // compare frames (`grouping::group_nodes`).
        let gap_applies = s.grouping_mode != "off";
        let similarity_applies = matches!(s.grouping_mode.as_str(), "visual" | "burst");
        let gaps = self.pref_choices(
            "gap",
            [500u64, 1000, 2000, 5000].map(|g| {
                (
                    format!("{}s", g as f32 / 1000.),
                    Cmd::Gap(g),
                    s.burst_gap_ms == g,
                )
            }),
            !gap_applies,
            window,
            cx,
        );
        let thresholds = self.pref_choices(
            "threshold",
            [(4, "Strict"), (8, "Balanced"), (14, "Loose")]
                .map(|(n, l)| (l.to_string(), Cmd::Threshold(n), s.visual_threshold == n)),
            !similarity_applies,
            window,
            cx,
        );
        let advance = self.pref_choices(
            "pref-advance",
            [
                (
                    "Off".to_string(),
                    Cmd::SetAutoAdvance(false),
                    !s.auto_advance,
                ),
                ("On".to_string(), Cmd::SetAutoAdvance(true), s.auto_advance),
            ],
            false,
            window,
            cx,
        );
        let reduce_motion = s
            .extra
            .get("reduce_motion")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let motion = self.pref_choices(
            "pref-motion",
            [
                ("Off".to_string(), Cmd::ReduceMotion(false), !reduce_motion),
                ("On".to_string(), Cmd::ReduceMotion(true), reduce_motion),
            ],
            false,
            window,
            cx,
        );
        let (text_sizes, text_size_description) = self.text_size_choices(s);
        let text_sizes = self.pref_choices("pref-text-size", text_sizes, false, window, cx);
        let motion_description = if motion::system_reduces_motion() {
            "Menus, dialogs and notices appear without fading. The system asks for reduced motion, so fades are off either way."
        } else {
            "Menus, dialogs and notices appear without fading. Also on when the system asks for reduced motion."
        };
        let drives = self.pref_choices(
            "pref-drives",
            [
                (
                    "Manual".to_string(),
                    Cmd::SetAutoDrives(false),
                    !s.auto_discover_drives,
                ),
                (
                    "Automatic".to_string(),
                    Cmd::SetAutoDrives(true),
                    s.auto_discover_drives,
                ),
            ],
            false,
            window,
            cx,
        );
        let trash = self.pref_choices(
            "trash-mode",
            [
                (
                    "OS Trash".into(),
                    Cmd::TrashMode("system".into()),
                    s.trash_mode == "system",
                ),
                (
                    "In-app".into(),
                    Cmd::TrashMode("in_app".into()),
                    s.trash_mode == "in_app",
                ),
            ],
            false,
            window,
            cx,
        );
        let peaking = loupe_aids::peaking_style_of(s);
        let peaking_colors = self.pref_choices(
            "peaking-color",
            shadowroom::peaking::PeakingColor::ALL.map(|color| {
                (
                    color.name().to_string(),
                    Cmd::PeakingColor(color),
                    peaking.color == color,
                )
            }),
            false,
            window,
            cx,
        );
        let peaking_levels = self.pref_choices(
            "peaking-sensitivity",
            shadowroom::peaking::PeakingSensitivity::ALL.map(|level| {
                (
                    level.name().to_string(),
                    Cmd::PeakingSensitivity(level),
                    peaking.sensitivity == level,
                )
            }),
            false,
            window,
            cx,
        );
        let memory = self.pref_choices(
            "memory",
            [0, 128, 256, 512, 1024, 2048].map(|mb| {
                (
                    if mb == 0 {
                        "Auto".into()
                    } else if mb >= 1024 {
                        format!("{} GB", mb / 1024)
                    } else {
                        format!("{mb} MB")
                    },
                    Cmd::Memory(mb),
                    s.memory_budget_mb == mb,
                )
            }),
            false,
            window,
            cx,
        );
        let disk = self.pref_choices(
            "disk",
            [2048, 8192, 32768].map(|mb| {
                (
                    format!("{} GB", mb / 1024),
                    Cmd::Disk(mb),
                    s.cache_budget_mb == mb,
                )
            }),
            false,
            window,
            cx,
        );
        let guide = self.pref_choices(
            "guide-startup-pref",
            [
                (
                    "Show".into(),
                    Cmd::StartupGuide(true),
                    s.show_getting_started,
                ),
                (
                    "Hide".into(),
                    Cmd::StartupGuide(false),
                    !s.show_getting_started,
                ),
            ],
            false,
            window,
            cx,
        );
        let mode = s
            .extra
            .get("theme_mode")
            .and_then(Value::as_str)
            .unwrap_or("manual");
        let theme_mode = self.pref_choices(
            "theme-mode",
            [
                ("manual", "Manual"),
                ("system", "System"),
                ("zed", "Follow Zed"),
            ]
            .map(|(k, l)| (l.to_string(), Cmd::ThemeMode(k.into()), mode == k)),
            false,
            window,
            cx,
        );
        let hover = self.hover_bg();
        let light_theme = s
            .extra
            .get("theme_light")
            .and_then(Value::as_str)
            .unwrap_or("Shadowroom Light");
        let dark_theme = s
            .extra
            .get("theme_dark")
            .and_then(Value::as_str)
            .unwrap_or("Shadowroom Dark");
        let source_description = match mode {
            "system" => format!("Light: {light_theme} · Dark: {dark_theme}"),
            "zed" => {
                let dark = matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                );
                match self.zed_theme.as_ref().and_then(|zed| zed.selected_name(dark)) {
                    Some(name) if self.themes.iter().any(|theme| theme.name == name) => format!("Following Zed · {name}"),
                    Some(name) => format!("{name} isn't installed here. Import its Zed theme file to match it."),
                    None => "Zed settings aren't available. Using the current theme until they are found.".into(),
                }
            }
            _ => "Choose an installed theme or import a Zed JSON / JSONC file.".into(),
        };
        let mut theme_list = div()
            .id("theme-list")
            .track_scroll(&self.theme_list_scroll)
            .max_h(px(240.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .border_1()
            .border_color(self.border());
        let mut sorted_themes: Vec<_> = self.preference_themes().iter().enumerate().collect();
        sorted_themes.sort_by_cached_key(|(_, theme)| {
            (theme.appearance == "light", theme.name.to_lowercase())
        });
        let light_fallback = Theme::light();
        let dark_fallback = Theme::dark();
        let mut last_appearance = None;
        let mut child = 0;
        let theme_count = sorted_themes.len();
        let selected_position = sorted_themes
            .iter()
            .position(|(_, theme)| {
                let light = theme.appearance == "light";
                if mode == "system" {
                    theme.name == if light { light_theme } else { dark_theme }
                } else {
                    theme.name == s.theme
                }
            })
            .unwrap_or(0);
        for (position, (i, theme)) in sorted_themes.into_iter().enumerate() {
            let light = theme.appearance == "light";
            if last_appearance != Some(light) {
                theme_list = theme_list.child(
                    div()
                        .flex_shrink_0()
                        .px(px(space::MD))
                        .pt(px(space::SM))
                        .pb(px(space::XS))
                        .text_size(ui::font(ui::text::LABEL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(self.muted())
                        .child(if light { "LIGHT" } else { "DARK" }),
                );
                last_appearance = Some(light);
                child += 1;
            }
            let active = theme.name == s.theme;
            let selected = if mode == "system" {
                theme.name == if light { light_theme } else { dark_theme }
            } else {
                active
            };
            let name = theme.name.clone();
            let palette = shadowroom::theme::PreparedColors::new(
                theme,
                if light {
                    &light_fallback
                } else {
                    &dark_fallback
                },
                [
                    ("editor.background", "#0f0f0f"),
                    ("text.accent", "#5b9bd5"),
                    ("text", "#e6e6e6"),
                ],
            );
            let mut swatches = div().flex().flex_shrink_0().gap(px(space::XS));
            for role in 0..3 {
                let color = palette.get(role);
                let color = rgba(
                    (u32::from(color.r) << 24)
                        | (u32::from(color.g) << 16)
                        | (u32::from(color.b) << 8)
                        | u32::from(color.a),
                );
                swatches = swatches.child(
                    div()
                        .size(px(10.))
                        .bg(color)
                        .border_1()
                        .border_color(self.c("border.variant", "#333333")),
                );
            }
            let choose = if mode == "system" {
                Cmd::SystemTheme(name.clone())
            } else {
                Cmd::Theme(name.clone())
            };
            let row = div()
                .id(("theme", i))
                .h(px(ui::ROW_HEIGHT))
                .flex_shrink_0()
                .px(px(space::MD))
                .flex()
                .items_center()
                .gap(px(space::MD))
                .cursor_pointer()
                .when(selected, |s| s.bg(self.selected_bg()))
                .when(!selected, |s| s.hover(move |s| s.bg(hover)))
                .child(swatches)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_normal()
                        .line_clamp(1)
                        .child(theme.name.clone()),
                )
                .child(
                    div()
                        .w(px(16.))
                        .flex_shrink_0()
                        .when(selected, |s| s.child(icon("check", self.accent()))),
                )
                .tooltip(self.tip(
                    &if theme.author.is_empty() {
                        theme.name.clone()
                    } else {
                        format!("{} · {}", theme.name, theme.author)
                    },
                    "",
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    let system = this
                        .preference_settings()
                        .extra
                        .get("theme_mode")
                        .and_then(Value::as_str)
                        == Some("system");
                    this.execute(
                        if system {
                            Cmd::SystemTheme(name.clone())
                        } else {
                            Cmd::Theme(name.clone())
                        },
                        window,
                        cx,
                    )
                }));
            theme_list = theme_list.child(
                self.focusable(
                    row,
                    &format!("theme-{position}"),
                    2,
                    position == selected_position,
                    FocusTarget::option(choose, "theme".into(), position, theme_count)
                        .reveal(&self.theme_list_scroll, child),
                    window,
                    cx,
                ),
            );
            child += 1;
        }
        let stats = self.media.stats();
        let section = self.preferences_section;
        let body = div()
            .id("preferences-body")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(ui::DIALOG_INSET))
            .pt(px(space::SM))
            .pb(px(ui::DIALOG_INSET))
            .when(section == PreferencesSection::Import, |body| body.child(self.ingest_preferences(window, cx)))
            .when(section == PreferencesSection::Culling, |body| body
            .child(self.preference_row("Auto-advance", "Move to the next photo after a decision", advance))
            .child(self.preference_row("Trash destination", "OS Trash moves files to the system Trash. In-app keeps files in place and supports Undo. You'll always be asked to confirm.", trash))
            .child(self.preference_row("Peaking color", "Ink for focus peaking (F); pick one that stands out from the scene", peaking_colors))
            .child(self.preference_row("Peaking sensitivity", "High also marks softer edges; Low only the crispest", peaking_levels))
            .child(self.preference_row("Grouping", "Keep bursts and similar frames together", groups))
            .child(self.preference_row("Burst gap", if gap_applies { "Maximum time between frames in a group" } else { "Applies when Grouping is on" }, gaps))
            .child(self.preference_row("Similarity", if similarity_applies { "How alike frames must look to group" } else { "Applies to Visual and Burst grouping" }, thresholds))
            .child(self.preference_row("Drives", "Discover mounted cards and photo drives", drives)))
            .when(section == PreferencesSection::Appearance, |body| body
            .child(self.preference_row("Font", "Interface typeface", fonts))
            .child(self.preference_row("Text size", &text_size_description, text_sizes))
            .child(self.preference_row("Reduce motion", motion_description, motion))
            .child(self.preference_row("Theme source", "Where the active palette comes from", theme_mode))
            .child(div().pt(px(space::MD)).flex().items_center().gap(px(space::SM)).child(div().text_color(self.muted()).child("Selected theme")).child(s.theme.clone()))
            .child(div().pt(px(space::XS)).text_size(ui::font(11.)).text_color(self.muted()).child(source_description))
            .children(self.contrast_note(&s.theme).map(|note| div().pt(px(space::XS)).text_size(ui::font(11.)).text_color(self.muted()).child(note)))
            .child(
                div()
                    .pt(px(space::MD))
                    .pb(px(space::XS))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(format!("Available themes · {}", self.preference_themes().len()))
                    .child(self.pref_button(
                        self.button("import-theme", "Import Zed theme…", "folder-opened", Cmd::ImportTheme, false, cx),
                        "import-theme",
                        2,
                        Cmd::ImportTheme,
                        window,
                        cx,
                    )),
            )
            .child(theme_list)
            .child(div().pt(px(space::SM)).text_size(ui::font(11.)).text_color(self.muted()).child(match mode {
                "system" => "Choose a light or dark theme to update its system appearance slot.",
                "zed" => "Installed Zed themes appear automatically. Choosing a theme switches to Manual.",
                _ => "Installed Zed themes appear automatically. Theme files reload when changed.",
            })))
            .when(section == PreferencesSection::Performance, |body| body
            .child(self.preference_row("Preview memory", "Auto adapts to this computer; shared by decoded images and display copies", memory))
            .child(self.preference_row("Disk cache", "Reuses thumbnails and previews across launches; originals stay read-only", disk))
            .child(div().pt(px(space::SM)).text_color(self.muted()).child("The next and previous frames are read ahead for comparison, including while zoomed. Inspection prefers a paired JPEG or embedded camera preview."))
            .child(
                div()
                    .pt(px(space::MD))
                    .text_color(self.muted())
                    .text_size(ui::font(11.))
                    .child(format!(
                        "Memory {:.1} of {} MB · textures {:.1} MB · disk {:.1} MB · {} reads · {} cache hits · {} queued",
                        stats.memory_bytes as f64 / 1048576.,
                        stats.memory_budget_bytes / 1048576,
                        self.texture_bytes as f64 / 1048576.,
                        stats.disk_bytes as f64 / 1048576.,
                        stats.source_reads,
                        stats.disk_hits,
                        stats.queued
                    )),
            ))
            .when(section == PreferencesSection::Help, |body| body
                .child(self.preference_row("Startup guide", "Workflow, shortcuts, time groups, labels and caching", guide))
                .child(self.preference_row(
                    "Getting Started",
                    "Open the guide now; F1 opens it from anywhere",
                    self.pref_button(
                        self.button("open-guide", "Open Getting Started", "files", Cmd::GettingStarted, false, cx),
                        "open-guide",
                        2,
                        Cmd::GettingStarted,
                        window,
                        cx,
                    )
                    .into_any_element(),
                )))
            .when(section == PreferencesSection::Controller, |body| body
            .child(
                div()
                    .pt(px(space::LG))
                    .pb(px(space::SM))
                    .text_size(ui::font(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(self.text())
                    .child("Creator Micro"),
            )
            .child(
                div()
                    .text_color(self.muted())
                    .text_size(ui::font(11.))
                    .child("Arrows navigate · F13–F15 flag · F16/F17 rating · F18–F20 views · F21 zoom · F22 histogram · F23/F24 bursts. Profiles: docs/CONTROLS.md"),
            ));
        let changed: HashSet<PreferencesSection> = self
            .preferences_draft
            .as_ref()
            .map(|draft| {
                draft
                    .changed_keys()
                    .iter()
                    .map(|key| preferences::section_for(key))
                    .collect()
            })
            .unwrap_or_default();
        let mut sections = div()
            .flex()
            .flex_shrink_0()
            .gap(px(ui::BAR_INSET))
            .px(px(ui::BAR_INSET))
            .border_b_1()
            .border_color(self.border());
        let accent = self.accent();
        let warning = self.c("warning", "#e0b341");
        let section_count = preferences::SECTIONS.len();
        for (index, (tab, label)) in preferences::SECTIONS.into_iter().enumerate() {
            let active = section == tab;
            let element = div()
                .id(SharedString::from(format!("preferences-{tab:?}")))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(space::XS))
                .h(px(ui::CONTROL_ROW_HEIGHT))
                .px(px(space::LG))
                // The active tab matches the title-bar view tabs: text plus a
                // 2 px accent underline on the bar's hairline, no fill.
                .border_b_2()
                .border_color(if active { accent } else { transparent_black() })
                .text_color(if active { self.text() } else { self.muted() })
                .cursor_pointer()
                .when(!active, |s| s.hover(move |s| s.bg(hover)))
                .child(label)
                // Unsaved-change dot, positioned so tabs never shift as it appears.
                .relative()
                .when(changed.contains(&tab), |s| {
                    s.child(
                        div()
                            .absolute()
                            .top(px(space::MD))
                            .right(px(space::SM))
                            .size(px(5.))
                            .rounded_full()
                            .bg(warning),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(Cmd::PreferencesSection(tab), window, cx)
                }));
            sections = sections.child(self.focusable(
                element,
                &format!("prefs-tab-{index}"),
                1,
                active,
                FocusTarget::option(
                    Cmd::PreferencesSection(tab),
                    "prefs-tab".into(),
                    index,
                    section_count,
                ),
                window,
                cx,
            ));
        }
        let draft = self.preferences_draft.as_ref();
        let changes = draft.map_or(0, |draft| draft.changed_count());
        let applying = draft.is_some_and(|draft| draft.applying);
        let prompt = draft.is_some_and(|draft| draft.discard_prompt);
        let status: AnyElement = if !self.settings_writable || self.settings_failure.is_some() {
            div()
                .child("Settings need attention · use Retry unsaved decisions")
                .into_any_element()
        } else if applying {
            div().child("Applying changes…").into_any_element()
        } else if prompt {
            div()
                .text_size(ui::font(12.))
                .text_color(self.text())
                .child(format!(
                    "Discard {changes} {}?",
                    if changes == 1 { "change" } else { "changes" }
                ))
                .into_any_element()
        } else if changes == 0 {
            div().child("No changes").into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .text_color(warning)
                .child(div().size(px(6.)).rounded_full().bg(warning))
                .child(format!(
                    "{changes} unsaved {}",
                    if changes == 1 { "change" } else { "changes" }
                ))
                .into_any_element()
        };
        let buttons = if prompt {
            div()
                .flex()
                .gap(px(space::SM))
                // The filled default sits rightmost, as in every footer.
                .child(self.pref_button(
                    self.action_button(
                        "discard-preferences",
                        "Discard",
                        Cmd::DiscardPreferences,
                        ButtonKind::DestructiveQuiet,
                        false,
                        cx,
                    ),
                    "prefs-discard",
                    3,
                    Cmd::DiscardPreferences,
                    window,
                    cx,
                ))
                .child(self.pref_button(
                    self.action_button(
                        "keep-editing-preferences",
                        "Keep Editing",
                        Cmd::KeepEditingPreferences,
                        ButtonKind::Primary,
                        false,
                        cx,
                    ),
                    "prefs-keep-editing",
                    3,
                    Cmd::KeepEditingPreferences,
                    window,
                    cx,
                ))
        } else {
            let clean = changes == 0 || applying;
            let apply = self.action_button(
                "apply-preferences",
                "Apply",
                Cmd::ApplyPreferences,
                ButtonKind::Secondary,
                clean,
                cx,
            );
            div()
                .flex()
                .gap(px(space::SM))
                .child(self.pref_button(
                    self.action_button(
                        "cancel-preferences",
                        "Cancel",
                        Cmd::DiscardPreferences,
                        ButtonKind::Secondary,
                        false,
                        cx,
                    ),
                    "prefs-cancel",
                    3,
                    Cmd::DiscardPreferences,
                    window,
                    cx,
                ))
                .child(if clean {
                    apply
                } else {
                    self.pref_button(apply, "prefs-apply", 3, Cmd::ApplyPreferences, window, cx)
                })
                .child(self.pref_button(
                    self.action_button(
                        "apply-close-preferences",
                        "Apply & Close",
                        Cmd::ApplyClosePreferences,
                        ButtonKind::Primary,
                        applying,
                        cx,
                    ),
                    "prefs-apply-close",
                    3,
                    Cmd::ApplyClosePreferences,
                    window,
                    cx,
                ))
        };
        let close = self.pref_button(
            self.button("close-preferences", "", "close", Cmd::Close, false, cx)
                .tooltip(self.tip("Close", "Esc")),
            "prefs-close",
            4,
            Cmd::Close,
            window,
            cx,
        );
        self.modal_scrim(Overlay::Preferences)
            .p(px(space::XXL))
            .child(
                self.modal_surface("preferences", dialog::DIALOG_LARGE)
                    .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // A click in the palette, guide or theme picker layered
                        // above must not close them (or discard this draft);
                        // with unsaved changes Close asks first.
                        if event.button == MouseButton::Left
                            && this.top_overlay() == Some(Overlay::Preferences)
                        {
                            cx.stop_propagation();
                            this.execute(Cmd::Close, window, cx);
                        }
                    }))
                    .max_h(px(660.))
                    .h_full()
                    .child(self.modal_header(settings_noun(), None, cx).child(close))
                    .child(sections)
                    .child(body)
                    .child(
                        self.modal_footer()
                            .text_size(ui::font(11.))
                            .text_color(self.muted())
                            .child(div().flex_1().min_w(px(0.)).child(status))
                            .child(buttons),
                    ),
            )
            .into_any_element()
    }
}

impl Workspace {
    fn status_bar(&self, window: &Window, cx: &gpui::Context<Self>) -> AnyElement {
        let stats = self.media.stats();
        // The footer describes the current workspace; a completed action shows
        // as a transient notice in its place (feedback.rs), never permanently.
        let context = if let Some(drag) = self.dock_drag {
            let (name, minimum, actual) = match drag.side {
                DockSide::Library => ("Sidebar", ui::LIBRARY_MIN_WIDTH, self.docks.library),
                DockSide::Inspector => ("Inspector", ui::INFO_MIN_WIDTH, self.docks.inspector),
            };
            if drag.proposed < minimum * 0.6 {
                format!("Release to hide {name} · Escape cancels")
            } else {
                format!("{name}: {actual:.0} px · Escape cancels")
            }
        } else if self.library_resize_focus.is_focused(window)
            || self.inspector_resize_focus.is_focused(window)
        {
            "← → resize · Shift for larger steps · Home to reset · Esc to finish".into()
        } else if self.quit.closing() {
            self.status.clone()
        } else if self.import_ui.active() {
            if self.import_ui.cancel_pending() {
                "Cancel the import? Click Cancel again · verified photos stay imported".into()
            } else if self.import_ui.queue.is_empty() {
                copy::import_progress(&self.import_ui.progress)
            } else {
                format!(
                    "{} · {} queued",
                    copy::import_progress(&self.import_ui.progress),
                    copy::count(self.import_ui.queue.len())
                )
            }
        } else if self.import_ui.scanning() {
            "Scanning import…".into()
        } else if self.busy {
            "Indexing photos…".into()
        } else if let Some(scan) = self
            .duplicate_scan
            .as_ref()
            .filter(|s| s.phase == ScanPhase::Running)
        {
            format!(
                "Checking duplicates · {} of {}",
                copy::count(scan.processed),
                copy::count(scan.candidates)
            )
        } else {
            let exports = self
                .export_jobs
                .iter()
                .filter(|job| job.state.is_active())
                .count();
            if exports > 0 {
                format!("Exporting · {}", plural(exports, "job", "jobs"))
            } else if self.multi.len() > 1 {
                format!(
                    "{} selected · {}",
                    copy::count(self.multi.len()),
                    plural(self.filtered.len(), "photo", "photos")
                )
            } else {
                plural(self.filtered.len(), "photo", "photos")
            }
        };
        div()
            .h(px(ui::STATUS_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .p(px(ui::STATUS_INSET))
            .border_t_1()
            .border_color(self.border())
            .bg(self.c("status_bar.background", "#141414"))
            .text_size(ui::font(11.))
            .text_color(self.muted())
            .child(
                self.compact_toggle(
                    "toggle-sidebar",
                    "",
                    "panel-left",
                    Cmd::Sidebar,
                    self.docks.library > 0.,
                    cx,
                )
                .tooltip(self.tip("Toggle sidebar", "⌘B")),
            )
            .when(self.import_ui.active(), |s| {
                // Byte-weighted, so one large video never stalls the bar.
                s.child(
                    div()
                        .id("ingest-status-progress")
                        .w(px(120.))
                        .h(px(3.))
                        .bg(self.border())
                        .child(
                            div()
                                .h_full()
                                .w(relative(copy::import_fraction(&self.import_ui.progress)))
                                .bg(self.accent()),
                        )
                        .tooltip(self.tip(&copy::import_progress(&self.import_ui.progress), "")),
                )
                .child(self.compact_button(
                    "ingest-status-show",
                    "Import",
                    "",
                    Cmd::Ingest("show".into()),
                    false,
                    cx,
                ))
                // While the card is out the job resumes on reinsertion.
                .when(!self.import_ui.progress.source_missing, |s| {
                    s.child(self.compact_button(
                        "ingest-status-pause",
                        if self.import_ui.progress.phase == Phase::Paused {
                            "Resume"
                        } else {
                            "Pause"
                        },
                        "",
                        Cmd::Ingest("pause".into()),
                        false,
                        cx,
                    ))
                })
                .child(
                    self.compact_button(
                        "ingest-status-cancel",
                        if self.import_ui.cancel_pending() {
                            "Confirm cancel"
                        } else {
                            "Cancel…"
                        },
                        "",
                        Cmd::Ingest("cancel".into()),
                        false,
                        cx,
                    )
                    .when(self.import_ui.cancel_pending(), |s| {
                        s.text_color(self.c("error", "#d46a6a"))
                    }),
                )
            })
            // The workspace description (selection, import or scan progress)
            // always stays; a notice takes the space after it, behind a rule,
            // and truncates first (R2-catalog-01).
            .child(
                div()
                    .id("workspace-status")
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_normal()
                    .line_clamp(1)
                    .child(context),
            )
            .child(
                match self.status_notice(cx).filter(|_| self.dock_drag.is_none()) {
                    Some(notice) => div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(1.))
                                .h(px(14.))
                                .bg(self.c("border.variant", "#333333")),
                        )
                        .child(notice),
                    None => div().flex_1(),
                },
            )
            .when(
                self.workspace_size.0 >= 1120.
                    && self
                        .remote_hosts
                        .values()
                        .any(|host| host.state() != HostState::Connected),
                |s| {
                    let text = if let Some(host) = self
                        .remote_hosts
                        .values()
                        .find(|h| h.state().is_transient())
                    {
                        format!(
                            "{} · {}{}",
                            host.config.display_name,
                            host.state(),
                            host.progress
                                .as_ref()
                                .map(|p| format!(" · {} scanned", p.scanned))
                                .unwrap_or_default()
                        )
                    } else {
                        let offline = self
                            .remote_hosts
                            .values()
                            .filter(|h| h.state() != HostState::Connected)
                            .count();
                        if offline > 0 {
                            format!("Network · {offline} offline")
                        } else {
                            "Network connected".into()
                        }
                    };
                    s.child(div().px(px(space::SM)).child(text))
                },
            )
            .children(self.save_state_view(cx))
            .child(
                div()
                    .id("appearance")
                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                    .max_w(px(if self.workspace_size.0 < 1120. {
                        32.
                    } else {
                        160.
                    }))
                    .flex_shrink_0()
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .occlude()
                    .cursor_pointer()
                    .hover({
                        let hover = self.hover_bg();
                        move |style| style.bg(hover)
                    })
                    .child(icon("color-mode", self.icon_muted()))
                    .when(self.workspace_size.0 >= 1120., |s| {
                        s.child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_normal()
                                .line_clamp(1)
                                .child(self.theme.name.clone()),
                        )
                    })
                    .tooltip(self.tip("Appearance · themes, Zed sync and interface font", ""))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.execute(Cmd::Appearance, window, cx)
                        }),
                    ),
            )
            // Memory is only worth attention when previews approach their budget.
            .when(
                stats.memory_bytes * 4 >= stats.memory_budget_bytes.max(1) * 3,
                |s| {
                    s.child(
                        div()
                            .id("cache-usage")
                            .px(px(space::XS))
                            .text_color(self.c("warning", "#e0b341"))
                            .child(format!(
                                "{:.0} MB",
                                (stats.memory_bytes + self.texture_bytes) as f64 / 1048576.
                            ))
                            .tooltip(
                                self.tip("Preview memory near its budget · see Preferences", ""),
                            ),
                    )
                },
            )
            .child(
                self.compact_toggle(
                    "toggle-histogram",
                    "",
                    "graph",
                    Cmd::Histogram,
                    self.histogram && self.inspector,
                    cx,
                )
                .tooltip(self.tip("Toggle histogram", "H")),
            )
            .child(
                self.compact_toggle(
                    "header-inspector",
                    "",
                    "panel-right",
                    Cmd::Inspector,
                    self.docks.inspector > 0.,
                    cx,
                )
                .tooltip(self.tip("Toggle inspector", "I")),
            )
            .child(
                self.compact_toggle(
                    "preferences",
                    "",
                    "settings",
                    Cmd::Preferences,
                    self.preferences,
                    cx,
                )
                .tooltip(self.tip(settings_noun(), "⌘,")),
            )
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let trace_started = self.trace.begin();
        // Input (or any repaint poll did not cause) returns poll to full speed.
        let poll_render = std::mem::take(&mut self.poll_render);
        if !poll_render {
            self.poll_wake.kick();
        }
        let element = self.render_workspace(window, cx);
        if trace_started.is_some() {
            let info = frame_trace::FrameInfo {
                view: if self.import_ui.open {
                    "import"
                } else {
                    match self.mode {
                        ViewMode::Grid => "grid",
                        ViewMode::Loupe => "loupe",
                        ViewMode::Compare => "compare",
                        ViewMode::Survey => "survey",
                    }
                },
                overlay: self.top_overlay().map_or("none", Overlay::trace_name),
                textures: self.visible_textures.len(),
                photo: self.selected_id().map_or("none", |id| {
                    // Sharpest texture of the current photo this frame drew
                    // (keys add 4 for focus-peaking copies; a visible key
                    // without a texture is still being converted).
                    match self
                        .visible_textures
                        .iter()
                        .filter(|key| key.0 == id && self.textures.contains_key(key))
                        .map(|key| key.1 % 4)
                        .max()
                    {
                        Some(0) => "thumbnail",
                        Some(1) => "fit",
                        Some(_) => "full",
                        None => "none",
                    }
                }),
                poll: poll_render,
            };
            self.trace.end(trace_started, info, cx);
        }
        element
    }
}

impl Workspace {
    fn render_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        if self._automation_wake.is_none() {
            self.drain_automation(window, cx);
        }
        self.reconcile_rating_filter();
        self.apply_theme_mode(window);
        self.apply_ui_scale();
        self.dialog_focus.borrow_mut().begin_frame();
        for texture in self.retired.drain(..) {
            let _ = window.drop_image(texture.image);
        }
        self.visible_textures.clear();
        let viewport = window.viewport_size();
        let (mut width, mut height) = (f32::from(viewport.width), f32::from(viewport.height));
        if let Decorations::Client { tiling } = window.window_decorations() {
            let edge = if wayland_session() && !tiling.is_tiled() {
                9.
            } else {
                1.
            };
            width -= edge * ((!tiling.left) as u8 + (!tiling.right) as u8) as f32;
            height -= edge * ((!tiling.top) as u8 + (!tiling.bottom) as u8) as f32;
        }
        self.workspace_size = (width.max(0.), height.max(0.));
        let minimum_center =
            if !self.import_ui.open && matches!(self.mode, ViewMode::Compare | ViewMode::Survey) {
                560.
            } else {
                ui::CENTER_MIN_WIDTH
            };
        // Small windows give the photo room (settings are untouched): the
        // catalog yields first in Review/Compare/Survey and while importing,
        // and the inspector also yields in Compare/Survey.
        let photo_view =
            !self.import_ui.open && self.mode != ViewMode::Grid && !self.filtered.is_empty();
        let side_by_side = photo_view && matches!(self.mode, ViewMode::Compare | ViewMode::Survey);
        let (mut library_fits, inspector_fits) =
            ui::culling_docks(photo_view, side_by_side, self.workspace_size.0);
        if self.import_ui.open && self.workspace_size.0 < 1200. {
            library_fits = false;
        }
        // A dock the user reopened, or one being dragged, stays.
        if let Some(drag) = self.dock_drag {
            library_fits |= drag.side == DockSide::Library;
        }
        library_fits |= self.dock_override[0];
        let inspector_fits = inspector_fits
            || self.dock_override[1]
            || self
                .dock_drag
                .is_some_and(|drag| drag.side == DockSide::Inspector);
        if self.mode == ViewMode::Grid {
            self.dock_override = [false; 2];
        }
        let library_yields = self.sidebar && !self.focus_mode && !library_fits;
        if library_yields {
            let inspector_yields = self.inspector && !inspector_fits && !self.photos.is_empty();
            self.hint_once(
                "catalog-yields",
                &if inspector_yields {
                    format!(
                        "Library and inspector hidden to give the photos room · {} and I show them",
                        shortcut("⌘B")
                    )
                } else {
                    format!(
                        "Library hidden to give the photo room · {} shows it",
                        shortcut("⌘B")
                    )
                },
            );
        } else {
            // The panel is back (Catalog, a wider window): the hint is stale.
            self.withdraw_notice("catalog-yields");
        }
        self.docks = ui::fit_docks(
            self.workspace_size.0,
            (self.sidebar && !self.focus_mode && library_fits)
                .then_some(self.settings.sidebar_width as f32),
            // Nothing to inspect in an empty library.
            (self.inspector
                && !self.focus_mode
                && !self.import_ui.open
                && inspector_fits
                && !self.photos.is_empty())
            .then_some(self.settings.inspector_width as f32),
            self.dock_priority,
            minimum_center,
        );
        // A divider drag holds the other panel still. Window fitting and pointer
        // tracking are different operations: squeezing both here makes the grip lag.
        if let Some(drag) = self.dock_drag {
            let (minimum, other) = match drag.side {
                DockSide::Library => (ui::LIBRARY_MIN_WIDTH, self.docks.inspector),
                DockSide::Inspector => (ui::INFO_MIN_WIDTH, self.docks.library),
            };
            let maximum = (self.workspace_size.0 - minimum_center - other).min(ui::DOCK_MAX_WIDTH);
            if maximum >= minimum {
                let width = drag.proposed.clamp(minimum, maximum).floor();
                match drag.side {
                    DockSide::Library => self.docks.library = width,
                    DockSide::Inspector => self.docks.inspector = width,
                }
                self.docks.center =
                    self.workspace_size.0 - self.docks.library - self.docks.inspector;
            }
        }
        self.compact = self.docks.center < 600.;
        let decision_height = if self.filtered.is_empty() || self.import_ui.open {
            0.
        } else {
            DECISION_HEIGHT
        };
        let strip_height = if self.filtered.is_empty() || self.import_ui.open {
            0.
        } else if self.mode == ViewMode::Grid {
            // The Catalog has no filmstrip; the ledger gets its own band.
            if self.ledger_visible() {
                ledger::ROW_HEIGHT
            } else {
                0.
            }
        } else {
            FILMSTRIP_HEIGHT
        };
        // Focus mode hides every bar but the decision bar and filmstrip
        // (REV-25); notices move onto the canvas (`focus_notices`).
        let chrome = self.chrome_visible();
        if !self.focus_mode {
            // Its exit hint is stale once Focus mode ends, however it ended.
            self.withdraw_notice("focus-mode");
        }
        self.center_size = (
            self.docks.center,
            self.workspace_size.1
                - if chrome { TITLE_HEIGHT } else { 0. }
                - if self.import_ui.open || !chrome {
                    0.
                } else {
                    TOOLBAR_HEIGHT
                }
                - decision_height
                - strip_height
                - if chrome { ui::STATUS_HEIGHT } else { 0. },
        );
        if self.mode == ViewMode::Grid && !self.import_ui.open {
            let columns = self.grid_cols();
            if columns != self.last_grid_cols {
                self.last_grid_cols = columns;
                self.grid_scroll
                    .scroll_to_item(self.selected / columns, ScrollStrategy::Center);
            }
        } else {
            self.last_grid_cols = 0;
        }
        if self.mode != ViewMode::Compare || self.reference.is_none() {
            let mut panes = self.pane_geometry.get();
            panes[0] = PaneGeometry::default();
            self.pane_geometry.set(panes);
        }
        self.sync_window_title(window);
        let center = if self.import_ui.open {
            div().into_any_element()
        } else if self.filtered.is_empty() {
            self.empty(cx)
        } else if self.mode == ViewMode::Grid {
            self.grid(cx)
        } else if self.mode == ViewMode::Survey {
            self.survey(cx)
        } else {
            let photo = self.filtered[self.selected];
            let mut viewer = div()
                .flex()
                .flex_1()
                .min_h(px(0.))
                .gap(px(space::SM))
                .bg(self.canvas_bg())
                .p(px(ui::PANEL_INSET));
            if self.mode == ViewMode::Compare {
                if let Some(reference) = self
                    .reference
                    .and_then(|id| self.photo_index.get(&id).copied())
                {
                    viewer = viewer.child(self.photo_view(reference, true, window, cx));
                } else {
                    viewer = viewer.child(self.compare_placeholder(cx));
                }
            }
            viewer = viewer.child(self.photo_view(photo, false, window, cx));
            viewer
                .children(self.neighbour_textures(cx))
                .into_any_element()
        };
        let mut main = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .when(chrome, |main| {
                main.child(self.trace.timed("filters", self.filters(cx)))
            })
            .children(self.bulk_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(self.trace.timed("center", center)),
            );
        if !self.filtered.is_empty() {
            main = main.child(self.trace.timed("rating_bar", self.rating_bar(cx)));
            if self.mode != ViewMode::Grid {
                let strip = self.filmstrip(cx);
                main = main.child(self.trace.timed("filmstrip", strip));
            } else if self.ledger_visible() {
                let band = self.ledger_band(cx);
                main = main.child(self.trace.timed("ledger", band));
            }
        }
        let mut body = div().relative().flex().flex_1().min_h(px(0.)).min_w(px(0.));
        if self.docks.library > 0. {
            body = body.child(self.trace.timed("sidebar", self.sidebar(cx)));
        }
        if self.import_ui.open {
            body = body.child(self.ingest_view(cx));
        } else {
            body = body.child(main);
        }
        if self.docks.inspector > 0. {
            body = body.child(self.trace.timed("inspector", self.inspector_view(cx)));
        }
        if self.docks.library > 0. {
            body = body.child(self.dock_resize_grip(DockSide::Library, window, cx));
        }
        if self.docks.inspector > 0. {
            body = body.child(self.dock_resize_grip(DockSide::Inspector, window, cx));
        }
        body = body.children(self.pane_ring_view());
        let mut root = div()
            .id("shadowroom")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.trace.enabled() && event.pressed_button.is_some() {
                    this.trace.input("drag", "");
                }
                this.move_scrollbar(event, cx);
                this.move_dock_resize(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.scroll_drag = None;
                    this.navigator_drag = None;
                    if this.pan_drag.release() {
                        cx.notify();
                    }
                    this.finish_dock_resize(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.scroll_drag = None;
                    this.navigator_drag = None;
                    if this.pan_drag.release() {
                        cx.notify();
                    }
                    this.finish_dock_resize(cx);
                }),
            )
            .on_action(cx.listener(|this, _: &QuitApp, _window, cx| this.user_quit(cx)))
            // The pointer took over: hide the keyboard focus ring until Tab or arrows.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if this.trace.enabled() {
                    this.trace
                        .input("mouse_down", &format!("{:?}", event.button));
                }
                // `|` so both run: the F6 pane hands focus back to the photos.
                if std::mem::take(&mut this.focus_visible) | this.release_pane_focus() {
                    cx.notify();
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(self.settings.interface_font.clone())
            .text_size(ui::font(12.))
            .line_height(px(ui::scaled(ui::TEXT_LINE_HEIGHT)))
            .text_color(self.text())
            .bg(self.bg())
            .when(chrome, |root| {
                root.child(self.trace.timed("title_bar", self.title_bar(window, cx)))
            })
            .child(body)
            .when(chrome, |root| {
                root.child(self.trace.timed("status_bar", self.status_bar(window, cx)))
            })
            .children((!chrome).then(|| self.focus_notices(cx)).flatten());
        // Overlays fade in over 120 ms (`motion`); closing is instant. Each is
        // tagged with its overlay (None: the system-modal quit prompt) so the
        // one scrim goes directly beneath the topmost modal.
        let mut layers: Vec<(Option<Overlay>, AnyElement)> = Vec::new();
        if self.add_host.is_some() {
            let view = self.overlay_in("add-host-in", self.add_host_view(cx));
            layers.push((Some(Overlay::AddHost), view));
        }
        if self.rating_filter_open && !self.palette && !self.preferences && !self.import_ui.open {
            let view = self.overlay_in("rating-filter-in", self.rating_filter_menu(cx));
            layers.push((Some(Overlay::RatingFilter), view));
        }
        if self.preferences {
            let view = self.overlay_in("preferences-in", self.preferences_view(window, cx));
            layers.push((Some(Overlay::Preferences), view));
        }
        if self.palette {
            let view = self.overlay_in("palette-in", self.palette_view(cx));
            layers.push((Some(Overlay::Palette), view));
        }
        if self.getting_started {
            let view = self.overlay_in("getting-started-in", self.getting_started_view(cx));
            layers.push((Some(Overlay::GettingStarted), view));
        }
        if self.image_context.is_some() {
            let menu = self.image_context_view(cx);
            let view = self.overlay_in("image-context-in", menu);
            layers.push((Some(Overlay::ImageContext), view));
        }
        if self.deletion.open() {
            let deletion = self.deletion_view(cx);
            let view = self.overlay_in("deletion-in", deletion);
            layers.push((Some(Overlay::Deletion), view));
        }
        if self.folder_picker.is_some() {
            let view = self.overlay_in("picker-in", self.picker_view(cx));
            layers.push((Some(Overlay::FolderPicker), view));
        }
        // The quit prompt is system-modal: above every overlay, in the same
        // dialog stack, so lower modals lose their tint like any other.
        if let Some(prompt) = self.quit_prompt_view(window, cx) {
            layers.push((None, self.overlay_in("quit-prompt-in", prompt)));
        }
        let tinted = if self.feedback.quit_prompt.is_some() {
            Some(None)
        } else {
            self.top_modal().map(Some)
        };
        for (overlay, view) in layers {
            if tinted == Some(overlay) {
                root = root.child(self.modal_tint());
            }
            root = root.child(view);
        }
        // The error toast paints above every overlay so no scrim dims it.
        root = root
            .children(self.error_toast(cx))
            .children(self.trace.probe());
        // A focused dialog control that is no longer drawn (its dialog closed
        // or its tab changed) hands focus back to the root, which owns `key()`.
        if self.dialog_focus.borrow().focus_is_stale(window) {
            window.focus(&self.focus);
        }
        // The quit prompt or Preferences on top owns keyboard focus (R2-a11y-01).
        self.claim_dialog_focus(window, cx);
        self.trim_display_cache();
        if self.feedback.animating.take() {
            window.request_animation_frame();
        }
        // Client-side decorations (Wayland, composited X11): transparent resize
        // margin around a 1px framed window. Server/macOS decorations need none.
        let Decorations::Client { tiling } = window.window_decorations() else {
            return root.into_any_element();
        };
        // Wayland surfaces are transparent, so the resize margin sits outside a
        // shadowed frame. GPUI's X11 surface is opaque: grips go inside the edge.
        let wayland = wayland_session();
        let inset = if wayland && !tiling.is_tiled() {
            px(8.)
        } else {
            px(0.)
        };
        let grip = inset.max(px(5.));
        if window.client_inset() != Some(inset) {
            window.set_client_inset(inset);
        }
        let border = self.c("border.variant", "#333333");
        div()
            .id("window-frame")
            .size_full()
            .bg(transparent_black())
            .when(!tiling.top, |d| d.pt(inset))
            .when(!tiling.bottom, |d| d.pb(inset))
            .when(!tiling.left, |d| d.pl(inset))
            .when(!tiling.right, |d| d.pr(inset))
            .child(
                canvas(
                    |_bounds, window, _cx| {
                        window.insert_hitbox(
                            Bounds::new(point(px(0.), px(0.)), window.viewport_size()),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_bounds, hitbox, window, _cx| {
                        if let Some(edge) = resize_edge(
                            window.mouse_position(),
                            grip,
                            window.viewport_size(),
                            tiling,
                        ) {
                            window.set_cursor_style(resize_cursor(edge), &hitbox);
                        }
                    },
                )
                .size_full()
                .absolute(),
            )
            .on_mouse_move(move |event, window, _| {
                if resize_edge(event.position, grip, window.viewport_size(), tiling).is_some() {
                    window.refresh();
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if let Some(edge) =
                    resize_edge(event.position, grip, window.viewport_size(), tiling)
                {
                    window.start_window_resize(edge);
                }
            })
            .child(
                root.when(!tiling.top, |d| d.border_t_1())
                    .when(!tiling.bottom, |d| d.border_b_1())
                    .when(!tiling.left, |d| d.border_l_1())
                    .when(!tiling.right, |d| d.border_r_1())
                    .border_color(border)
                    .when(!tiling.is_tiled(), |d| {
                        d.shadow(vec![BoxShadow {
                            color: hsla(0., 0., 0., 0.4),
                            offset: point(px(0.), px(0.)),
                            blur_radius: inset / 2.,
                            spread_radius: px(0.),
                        }])
                    }),
            )
            .into_any_element()
    }
}

pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--mcp-config") {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"mcpServers":{"shadowroom":{"command":std::env::current_exe()?,"args":["--mcp"]}}})
            )?
        );
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--mcp") {
        return shadowroom::automation::run_mcp_stdio();
    }
    if args.first().is_some_and(|a| a == "--export-controls") {
        let path = args
            .get(1)
            .context("Usage: shadowroom --export-controls PATH")?;
        shadowroom::controls::export_shortcut_profile(Path::new(path))?;
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--patch-via") {
        let source = args
            .get(1)
            .context("Usage: shadowroom --patch-via BACKUP DEST [LAYER]")?;
        let dest = args
            .get(2)
            .context("Usage: shadowroom --patch-via BACKUP DEST [LAYER]")?;
        let layer = args
            .get(3)
            .map(|s| s.to_string_lossy().parse::<usize>())
            .transpose()?
            .unwrap_or(0);
        shadowroom::controls::export_via_layout(Path::new(source), Path::new(dest), layer)?;
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--help") {
        println!(
            "Shadowroom\n\n  shadowroom [FOLDER]\n  shadowroom --import FOLDER\n  shadowroom --mcp\n  shadowroom --mcp-config\n  shadowroom --export-controls PATH\n  shadowroom --patch-via BACKUP DEST [LAYER]\n\nNative photo culling. Command/Ctrl+K opens all commands."
        );
        return Ok(());
    }
    let import = if args.first().is_some_and(|a| a == "--import") {
        args.get(1).map(PathBuf::from)
    } else {
        args.first()
            .filter(|a| !a.to_string_lossy().starts_with('-'))
            .map(PathBuf::from)
    };
    let data = directories::ProjectDirs::from("app", "Shadowroom", "shadowroom")
        .context("User data directory unavailable")?;
    let _catalog_lease = shadowroom::recovery::CatalogLease::acquire(data.data_local_dir())?;
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let fonts: Vec<Cow<'static, [u8]>> = vec![
                Cow::Borrowed(include_bytes!(
                    "../assets/fonts/dmsans/DMSans[opsz,wght].ttf"
                )),
                Cow::Borrowed(include_bytes!(
                    "../assets/fonts/geistmono/GeistMono[wght].ttf"
                )),
                Cow::Borrowed(include_bytes!("../assets/fonts/dmmono/DMMono-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../assets/fonts/dmmono/DMMono-Medium.ttf")),
            ];
            if let Err(e) = cx.text_system().add_fonts(fonts) {
                eprintln!("Font loading: {e:#}");
            }
            cx.set_menus(vec![Menu {
                name: "Shadowroom".into(),
                items: vec![MenuItem::action("Quit Shadowroom", QuitApp)],
            }]);
            let bounds = Bounds::centered(None, size(px(1440.), px(940.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // macOS: native traffic lights over our own 45px title bar (Zed's
                    // layout). Linux: client-side decorations so the title bar is the
                    // only bar; GPUI falls back to server frames without a compositor.
                    titlebar: Some(TitlebarOptions {
                        title: Some("Shadowroom".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(14.), px(14.))),
                    }),
                    window_decorations: Some(WindowDecorations::Client),
                    app_id: Some("app.shadowroom.Shadowroom".into()),
                    window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        // Leaving the window is a natural checkpoint for the
                        // throttled selection/position write.
                        cx.observe_window_activation(window, |this: &mut Workspace, window, _| {
                            this.window_active = window.is_window_active();
                            if !window.is_window_active() {
                                this.flush_navigation();
                                this.drain_settings();
                            }
                        })
                        .detach();
                        Workspace::new(cx, import, window.window_handle())
                    });
                    window.focus(&view.read(cx).focus);
                    let weak = view.downgrade();
                    window.on_window_should_close(cx, move |_window, cx| {
                        let _ = weak.update(cx, |this, cx| this.request_quit(cx));
                        false
                    });
                    view
                },
            )
            .expect("open Shadowroom window");
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    Ok(())
}

#[cfg(test)]
mod ui_tests {
    use super::{
        AddHostForm, BurstIndex, Cmd, CollectionCounts, DeferredSave, NAVIGATION_PERSIST, Notice,
        NoticeKind, Overlay, PALETTE_PAGE, PaletteCache, PanDrag, PaneGeometry, SelectionSummary,
        SurveyKey, SurveySummary, UiInbox, ViewMode, adjust_folder, clear_notice, copy,
        grip_owns_key, index_filtered_positions, is_quit_chord, item, label_index, match_tier,
        palette_category_rank, palette_step, retained_position, survey_layout, text_input,
        thumbnail_size, title_match, ui::space,
    };

    #[test]
    fn thumbnail_fallbacks_scale_to_the_thumbnail_edge() {
        assert_eq!(thumbnail_size(2000, 1333), Some((384, 256)));
        assert_eq!(thumbnail_size(1333, 2000), Some((256, 384)));
        assert_eq!(thumbnail_size(384, 200), None);
        assert_eq!(thumbnail_size(1, 5000), Some((1, 384)));
    }
    use gpui::Keystroke;
    use gpui::{point, px};
    use serde_json::json;
    use shadowroom::{
        model::{ColorLabel, Flag, Photo},
        workspace::{Decision, Filter, fuzzy_score},
    };
    use std::time::Instant;
    use std::{
        collections::{HashMap, HashSet},
        future::Future,
        sync::Arc,
        task::{Poll, Wake, Waker},
        time::Duration,
    };

    fn sample_photos() -> Vec<Photo> {
        (0..120)
            .map(|id| Photo {
                id,
                rating: (id % 6) as u8,
                flag: [Flag::None, Flag::Pick, Flag::Reject][id as usize % 3],
                label: [
                    ColorLabel::None,
                    ColorLabel::Red,
                    ColorLabel::Yellow,
                    ColorLabel::Green,
                    ColorLabel::Blue,
                    ColorLabel::Purple,
                ][id as usize % 6],
                trashed: id % 7 == 0,
                ..Photo::default()
            })
            .collect()
    }

    fn assert_counts(counts: &CollectionCounts, photos: &[Photo]) {
        assert_eq!(counts.all, photos.iter().filter(|p| !p.trashed).count());
        assert_eq!(
            counts.picks,
            photos
                .iter()
                .filter(|p| !p.trashed && p.flag == Flag::Pick)
                .count()
        );
        assert_eq!(
            counts.rejects,
            photos
                .iter()
                .filter(|p| !p.trashed && p.flag == Flag::Reject)
                .count()
        );
        assert_eq!(
            counts.unrated,
            photos.iter().filter(|p| Filter::Unrated.accepts(p)).count()
        );
        assert_eq!(counts.trash, photos.iter().filter(|p| p.trashed).count());
        assert_eq!(
            counts.rated,
            photos
                .iter()
                .filter(|p| !p.trashed && p.rating >= 3)
                .count()
        );
        for label in [
            ColorLabel::None,
            ColorLabel::Red,
            ColorLabel::Yellow,
            ColorLabel::Green,
            ColorLabel::Blue,
            ColorLabel::Purple,
        ] {
            assert_eq!(
                counts.labels[label_index(label)],
                photos
                    .iter()
                    .filter(|p| !p.trashed && p.label == label)
                    .count()
            );
        }
    }

    #[test]
    fn incremental_collection_counts_match_rescans_and_undo() {
        let mut photos = sample_photos();
        let mut counts = CollectionCounts::from_photos(&photos);
        let original = CollectionCounts::from_photos(&photos);
        assert_counts(&counts, &photos);
        let mut history = Vec::new();
        for i in 0..photos.len() {
            let before = Decision::from(&photos[i]);
            history.push(before);
            counts.adjust(before, false);
            let p = &mut photos[i];
            p.rating = (p.rating + 3) % 6;
            p.flag = if p.flag == Flag::Pick {
                Flag::None
            } else {
                Flag::Pick
            };
            p.label = ColorLabel::Purple;
            p.trashed = !p.trashed;
            counts.adjust(Decision::from(&*p), true);
            assert_counts(&counts, &photos);
        }
        for before in history.into_iter().rev() {
            let p = &mut photos[before.id as usize];
            counts.adjust(Decision::from(&*p), false);
            p.rating = before.rating;
            p.flag = before.flag;
            p.label = before.label;
            p.trashed = before.trashed;
            counts.adjust(before, true);
        }
        assert_eq!(counts, original);
        assert_counts(&counts, &photos);
    }

    #[test]
    fn survey_summary_tracks_filtered_order_span_and_review_updates() {
        let mut photos = sample_photos();
        photos[0].captured_at = Some(1_000);
        photos[1].captured_at = Some(2_500);
        let filtered = [1, 4, 0, 8];
        let mut summary = SurveySummary::new(vec![2, 0, 2], &filtered, &photos);
        assert_eq!(summary.positions, [0, 2]);
        assert_eq!(summary.span, " · 1.5 s");
        let decided = summary.decided;
        summary.update_reviewed(1, false, true); // Another group in the same view.
        assert_eq!(summary.decided, decided);
        summary.update_reviewed(0, true, false);
        assert_eq!(summary.decided, decided - 1);
        summary.update_reviewed(0, false, true);
        assert_eq!(summary.decided, decided);
        summary.update_reviewed(0, true, true);
        assert_eq!(summary.decided, decided);
    }

    #[test]
    fn filtered_positions_preserve_duplicate_order_and_clear_obsolete_membership() {
        let mut positions = Vec::new();
        // Duplicate-set order can differ from catalog order.
        index_filtered_positions(&mut positions, 8, &[5, 1, 6, 2]);
        assert_eq!(
            positions,
            [usize::MAX, 1, 3, usize::MAX, usize::MAX, 0, 2, usize::MAX]
        );
        index_filtered_positions(&mut positions, 8, &[2, 5]);
        assert_eq!(positions[1], usize::MAX);
        assert_eq!(positions[2], 0);
        assert_eq!(positions[5], 1);
        index_filtered_positions(&mut positions, 3, &[1]);
        assert_eq!(positions, [usize::MAX, 0, usize::MAX]);
        index_filtered_positions(&mut positions, 0, &[]);
        assert!(positions.is_empty());
    }

    #[test]
    fn folder_counts_preserve_sort_order_prune_and_restore() {
        let mut folders = Vec::new();
        for folder in ["c", "a", "b", "a"] {
            adjust_folder(&mut folders, folder, true);
        }
        assert_eq!(
            folders,
            vec![("a".into(), 2), ("b".into(), 1), ("c".into(), 1)]
        );
        adjust_folder(&mut folders, "a", false);
        adjust_folder(&mut folders, "b", false);
        assert_eq!(folders, vec![("a".into(), 1), ("c".into(), 1)]);
        adjust_folder(&mut folders, "b", true);
        assert_eq!(folders[1], ("b".into(), 1));
    }

    #[test]
    fn burst_cache_matches_original_predicate_through_pick_transitions() {
        let mut photos = sample_photos();
        let index: HashMap<_, _> = photos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        // Include a missing member and a singleton, as may happen between snapshots.
        let groups = vec![vec![0, 1, 2, 3], vec![4, 5], vec![6], vec![9, 12, 999]];
        let mut bursts = BurstIndex::new(&groups, &photos, &index);
        for id in [0, 1, 4, 9, 12, 1, 0, 4, 9, 12] {
            let p = &mut photos[id];
            p.flag = if p.flag == Flag::Pick {
                Flag::None
            } else {
                Flag::Pick
            };
            bursts.change_pick(p.id, p.flag == Flag::Pick, &groups);
            let expected: HashSet<_> = groups
                .iter()
                .filter(|g| g.len() > 1)
                .filter(|g| {
                    !g.iter()
                        .any(|id| index.get(id).is_some_and(|&i| photos[i].flag == Flag::Pick))
                })
                .flatten()
                .copied()
                .collect();
            assert_eq!(bursts.unresolved, expected);
        }
        assert_eq!(bursts.slots[&3], (0, 3, 4));
        assert!(!bursts.slots.contains_key(&6));
        // Trash preserves flags, and hence preserves the original burst predicate.
        photos[1].flag = Flag::Pick;
        photos[1].trashed = true;
        let bursts = BurstIndex::new(&groups, &photos, &index);
        assert!(!bursts.unresolved.contains(&0));
    }

    #[test]
    fn palette_matches_preserve_stable_order_recents_and_query_cache() {
        let mut cache = PaletteCache {
            items: vec![
                item("Pick", "Review", "P", "pick", Cmd::Flag(Flag::Pick)),
                item("Reject", "Review", "X", "reject", Cmd::Flag(Flag::Reject)),
                item("Pin", "Compare", "B", "pin", Cmd::Pin),
            ],
            ..PaletteCache::default()
        };
        let recent = vec!["Pin".into()];
        cache.match_query("", &recent);
        assert_eq!(cache.matches, [2, 0, 1]);
        let allocation = cache.matches.as_ptr();
        cache.match_query("", &recent);
        assert_eq!(cache.matches.as_ptr(), allocation);
        for query in ["p", "rv", "review", "no such command", ""] {
            cache.match_query(query, &recent);
            let mut expected: Vec<_> = cache
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    fuzzy_score(query, &format!("{} {}", item.title, item.detail))
                        .map(|score| (score, i))
                })
                .collect();
            if query.is_empty() {
                expected.sort_by_key(|&(_, i)| {
                    (
                        recent
                            .iter()
                            .position(|title| *title == cache.items[i].title)
                            .unwrap_or(usize::MAX),
                        palette_category_rank(&cache.items[i].detail),
                    )
                });
            } else {
                expected
                    .sort_by_key(|&(score, i)| (match_tier(query, &cache.items[i].title), -score));
            }
            assert_eq!(
                cache.matches,
                expected.into_iter().map(|(_, i)| i).collect::<Vec<_>>()
            );
        }
    }

    /// Beside photo results the palette keeps title matches only, at most
    /// three loose ones; matched letters are the ones highlighted.
    #[test]
    fn palette_stays_precise_beside_photo_results() {
        let commands = || {
            vec![
                item(
                    "Label Red",
                    "Review · the active label's key clears it",
                    "6",
                    "symbol-color",
                    Cmd::Label(ColorLabel::Red),
                ),
                item(
                    "Getting Started",
                    "Help · workflow, labels, cache and keyboard shortcuts",
                    "F1",
                    "files",
                    Cmd::GettingStarted,
                ),
                item(
                    "Export Picks in This View…",
                    "File",
                    "⇧E",
                    "export",
                    Cmd::ExportPicks,
                ),
                item(
                    "Toggle 100% Zoom",
                    "View · one image pixel per display pixel",
                    "Z",
                    "zoom",
                    Cmd::Zoom,
                ),
                item(
                    copy::DELETE_PERMANENTLY,
                    "File · confirm photo/file counts and original locations",
                    "",
                    "trash",
                    Cmd::PermanentDelete,
                ),
            ]
        };
        let photo = || item("02-lakeside.jpg", "Photo · Demo", "", "image", Cmd::Close);
        let titles = |cache: &PaletteCache| -> Vec<String> {
            cache
                .matches
                .iter()
                .map(|&i| cache.items[i].title.clone())
                .collect()
        };
        for (query, expected) in [
            ("lake", vec!["02-lakeside.jpg"]),
            ("demo", vec!["02-lakeside.jpg"]),
        ] {
            let mut cache = PaletteCache {
                items: commands(),
                ..PaletteCache::default()
            };
            cache.match_query(query, &[]);
            cache.merge_found(vec![photo()], query, false);
            assert_eq!(titles(&cache), expected, "query {query:?}");
        }
        let mut cache = PaletteCache {
            items: commands(),
            ..PaletteCache::default()
        };
        cache.match_query("exp", &[]);
        assert_eq!(titles(&cache)[0], "Export Picks in This View…");
        assert_eq!(title_match("exp", "Export Picks in This View…"), [0..3]);
        assert_eq!(title_match("ep", "Export Picks"), [0..1, 2..3]);
        assert_eq!(title_match("pick bu", "Pick Burst Winner"), [0..7]);
        assert!(title_match("zz", "Export").is_empty());
        assert!(title_match("", "Export").is_empty());
    }

    #[test]
    fn empty_palette_lists_recents_then_culling_before_files() {
        let mut cache = PaletteCache {
            items: vec![
                item("Show Import Panel", "File", "", "drive", Cmd::Import),
                item("Grid View", "View", "G", "grid", Cmd::View(ViewMode::Grid)),
                item("Undo", "Edit", "⌘Z", "undo", Cmd::Undo),
                item("Pick", "Review", "P", "pick", Cmd::Flag(Flag::Pick)),
                item(
                    "Quit",
                    "App · finishes pending writes first",
                    "",
                    "close",
                    Cmd::Quit,
                ),
                item("Pin", "Compare", "B", "pin", Cmd::Pin),
            ],
            ..PaletteCache::default()
        };
        cache.match_query("", &[]);
        let titles: Vec<_> = cache
            .matches
            .iter()
            .map(|&i| cache.items[i].title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "Pick",
                "Undo",
                "Grid View",
                "Pin",
                "Show Import Panel",
                "Quit"
            ]
        );
        cache.query = None;
        cache.match_query("", &["Quit".into()]);
        assert_eq!(cache.items[cache.matches[0]].title, "Quit");
        assert_eq!(cache.items[cache.matches[1]].title, "Pick");
    }

    #[test]
    fn palette_arrows_wrap_and_pages_stop_at_the_ends() {
        assert_eq!(palette_step(0, 5, "up"), 4);
        assert_eq!(palette_step(4, 5, "down"), 0);
        assert_eq!(palette_step(2, 5, "down"), 3);
        assert_eq!(palette_step(9, 5, "up"), 3, "a stale index clamps first");
        assert_eq!(palette_step(3, 40, "pageup"), 0);
        assert_eq!(palette_step(30, 40, "pagedown"), 39);
        assert_eq!(palette_step(0, 40, "pagedown"), PALETTE_PAGE);
        assert_eq!(palette_step(0, 0, "down"), 0);
    }

    struct TestWake(std::sync::mpsc::Sender<()>);
    impl Wake for TestWake {
        fn wake(self: Arc<Self>) {
            let _ = self.0.send(());
        }
    }
    fn test_waker() -> (Waker, std::sync::mpsc::Receiver<()>) {
        let (send, recv) = std::sync::mpsc::channel();
        (Waker::from(Arc::new(TestWake(send))), recv)
    }

    #[test]
    fn automation_inbox_wakes_without_timer_and_preserves_fifo() {
        let (send, recv) = crossbeam_channel::bounded(4);
        let inbox = UiInbox::new(recv).unwrap();
        let (waker, woke) = test_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        for value in 0..20 {
            let mut next = Box::pin(inbox.recv());
            assert!(next.as_mut().poll(&mut cx).is_pending());
            send.send(value).unwrap();
            woke.recv_timeout(Duration::from_secs(2))
                .expect("request should wake UI");
            assert_eq!(next.as_mut().poll(&mut cx), Poll::Ready(Some(value)));
        }
        drop(send);
        let mut next = Box::pin(inbox.recv());
        let result = match next.as_mut().poll(&mut cx) {
            Poll::Pending => {
                woke.recv_timeout(Duration::from_secs(2))
                    .expect("disconnect should wake UI");
                next.as_mut().poll(&mut cx)
            }
            result => result,
        };
        assert_eq!(result, Poll::Ready(None));
    }

    #[test]
    fn automation_inbox_keeps_backpressure_and_stops_when_dropped() {
        let (send, recv) = crossbeam_channel::bounded(1);
        let inbox = UiInbox::new(recv).unwrap();
        let (waker, woke) = test_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut next = Box::pin(inbox.recv());
        assert!(next.as_mut().poll(&mut cx).is_pending());
        send.send(1).unwrap();
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        send.send(2).unwrap();
        assert!(matches!(
            send.try_send(3),
            Err(crossbeam_channel::TrySendError::Full(3))
        ));
        drop(next);
        drop(inbox);
        assert!(matches!(
            send.send_timeout(3, Duration::from_secs(2)),
            Err(crossbeam_channel::SendTimeoutError::Disconnected(3))
        ));
    }

    #[test]
    fn pan_drag_pans_only_the_pressed_pane_and_never_goes_stale() {
        let at = |x: f32, y: f32| point(px(x), px(y));
        // A drag that began elsewhere (filmstrip, scrollbar) never pans.
        let mut drag = PanDrag::Idle;
        assert_eq!(drag.motion(false, at(5., 5.), true), None);
        assert_eq!(drag, PanDrag::Idle);
        // Press then move: deltas follow the pointer from the press point.
        drag = PanDrag::press(false, at(10., 10.));
        assert!(drag.holds(false) && !drag.holds(true));
        assert_eq!(drag.motion(false, at(14., 7.), true), Some(at(4., -3.)));
        assert!(matches!(
            drag,
            PanDrag::Dragging {
                reference: false,
                ..
            }
        ));
        assert_eq!(drag.motion(false, at(15., 7.), true), Some(at(1., 0.)));
        // The other compare pane ignores this drag and leaves it intact.
        assert_eq!(drag.motion(true, at(40., 7.), true), None);
        assert!(drag.holds(false));
        // Release returns to Idle; a later drag into the pane does not jump.
        assert!(drag.release());
        assert!(!drag.release());
        assert_eq!(drag.motion(false, at(400., 300.), true), None);
        // A release the window never saw is detected on the next plain move.
        drag = PanDrag::press(true, at(1., 1.));
        assert_eq!(drag.motion(true, at(2., 2.), false), None);
        assert_eq!(drag, PanDrag::Idle);
    }

    #[test]
    fn notices_clear_by_kind_not_by_message_text() {
        let mut slot = Some(Notice::new(
            NoticeKind::UnsavedDecisions,
            "Decisions remain unsaved: disk full",
        ));
        clear_notice(&mut slot, NoticeKind::UnsavedPreferences);
        assert!(slot.is_some());
        clear_notice(&mut slot, NoticeKind::UnsavedDecisions);
        assert!(slot.is_none());
        // A general error that happens to quote the same words survives.
        let mut slot: Option<Notice> = Some("Decisions remain unsaved: from import".into());
        clear_notice(&mut slot, NoticeKind::UnsavedDecisions);
        assert_eq!(
            slot.as_ref().map(|n| n.text.as_str()),
            Some("Decisions remain unsaved: from import")
        );
        assert_eq!(
            serde_json::to_value(&slot).unwrap(),
            json!("Decisions remain unsaved: from import")
        );
    }

    #[test]
    fn add_host_form_keeps_its_own_caret() {
        let mut form = AddHostForm::new();
        assert_eq!(form.active, 0);
        form.fields[0] = "studio".into();
        form.focus(1);
        assert_eq!(form.active, 1);
        // The palette has a separate edit state (formerly one shared
        // `text_edit`), so focusing it cannot move the form's caret.
        let mut palette = text_input::Edit::default();
        palette.focus("");
        assert_eq!(form.edit.caret(), form.fields[1].len());
        form.focus(0);
        assert_eq!(form.edit.caret(), "studio".len());
    }

    #[test]
    fn navigation_persistence_is_throttled_and_flushes_on_demand() {
        // 100 arrow presses at a 30 Hz key repeat, polled at the 50 ms
        // active cadence. The old path queued a settings write whenever a
        // poll saw a dirty flag, i.e. once per press-bearing poll.
        let start = Instant::now();
        let press = Duration::from_millis(33);
        let poll = Duration::from_millis(50);
        let end = start + press * 100 + NAVIGATION_PERSIST + poll;
        let (mut before, mut after) = (0, 0);
        let (mut dirty, mut save) = (false, DeferredSave::default());
        let (mut next_press, mut presses) = (start, 0);
        let mut now = start;
        while now <= end {
            while presses < 100 && next_press <= now {
                dirty = true;
                save.touch(next_press);
                presses += 1;
                next_press += press;
            }
            before += usize::from(std::mem::take(&mut dirty));
            after += usize::from(save.take_due(now));
            now += poll;
        }
        assert_eq!(presses, 100);
        assert_eq!(before, 67, "writes per 100 presses before throttling");
        // 3.3 s of navigation: at most one write per elapsed interval, plus
        // the trailing one.
        assert_eq!(after, 4, "writes per 100 presses after throttling");
        assert!(!save.pending(), "the final position is always written");

        // Later presses never postpone a scheduled write (bounded loss).
        let mut save = DeferredSave::default();
        save.touch(start);
        save.touch(start + Duration::from_millis(900));
        assert!(!save.take_due(start + Duration::from_millis(999)));
        assert!(save.take_due(start + NAVIGATION_PERSIST));
        assert!(!save.take_due(start + NAVIGATION_PERSIST * 5));
        // Quit and blur flush a pending write before it is due.
        save.touch(start);
        assert!(save.pending() && save.take() && !save.pending() && !save.take());
    }

    #[test]
    fn quit_chord_is_recognised_and_never_swallowed() {
        let key = |s: &str| Keystroke::parse(s).unwrap();
        for chord in ["ctrl-q", "cmd-q"] {
            assert!(is_quit_chord(&key(chord)), "{chord}");
        }
        for other in [
            "q",
            "shift-q",
            "ctrl-shift-q",
            "alt-q",
            "ctrl-alt-q",
            "ctrl-w",
        ] {
            assert!(!is_quit_chord(&key(other)), "{other}");
        }
        // The focused dock grip is the only element below the root with its
        // own key handler; it must let quit (and the palette) bubble to
        // `key()` whether or not an overlay is up.
        for overlay_open in [false, true] {
            for chord in ["ctrl-q", "cmd-q", "ctrl-k"] {
                assert!(!grip_owns_key(&key(chord), overlay_open), "{chord}");
            }
        }
        assert!(grip_owns_key(&key("left"), false));
        assert!(!grip_owns_key(&key("left"), true));
    }

    /// `key()` must test the quit chord before consulting any overlay, view or
    /// field state, and every other key handler must let it through. Routing
    /// past that first check depends on `Overlay`/`ViewMode` state, so this
    /// holds for every surface in `Overlay::STACK` and every `ViewMode`.
    #[test]
    fn every_key_handler_lets_quit_through_before_routing() {
        let _surfaces = (
            Overlay::STACK,
            [
                ViewMode::Grid,
                ViewMode::Loupe,
                ViewMode::Compare,
                ViewMode::Survey,
            ],
        );
        let sources = [
            ("desktop.rs", include_str!("desktop.rs")),
            ("appearance.rs", include_str!("desktop/appearance.rs")),
            ("copy.rs", include_str!("desktop/copy.rs")),
            ("culling.rs", include_str!("desktop/culling.rs")),
            ("deletion_ui.rs", include_str!("desktop/deletion_ui.rs")),
            ("feedback.rs", include_str!("desktop/feedback.rs")),
            ("dialog.rs", include_str!("desktop/dialog.rs")),
            ("folder_picker.rs", include_str!("desktop/folder_picker.rs")),
            ("help.rs", include_str!("desktop/help.rs")),
            ("ingest_ui.rs", include_str!("desktop/ingest_ui.rs")),
            ("layout.rs", include_str!("desktop/layout.rs")),
            ("ledger.rs", include_str!("desktop/ledger.rs")),
            ("loupe_aids.rs", include_str!("desktop/loupe_aids.rs")),
            ("preferences.rs", include_str!("desktop/preferences.rs")),
            (
                "review_actions.rs",
                include_str!("desktop/review_actions.rs"),
            ),
            ("scrollbar.rs", include_str!("desktop/scrollbar.rs")),
            ("search.rs", include_str!("desktop/search.rs")),
            ("text_input.rs", include_str!("desktop/text_input.rs")),
            ("motion.rs", include_str!("desktop/motion.rs")),
            ("panes.rs", include_str!("desktop/panes.rs")),
            (
                "platform_prefs.rs",
                include_str!("desktop/platform_prefs.rs"),
            ),
            ("prefetch.rs", include_str!("desktop/prefetch.rs")),
            ("frame_trace.rs", include_str!("desktop/frame_trace.rs")),
        ];
        // Every desktop module is checked: a new file must join the list.
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/desktop");
        for entry in std::fs::read_dir(&directory).expect("src/desktop") {
            let path = entry.expect("desktop module").path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    sources.iter().any(|(listed, _)| *listed == name),
                    "src/desktop/{name} is missing from the quit-chord guard's sources"
                );
            }
        }
        let root = sources[0].1;
        let body = &root[root
            .find("    fn key(&mut self, event: &KeyDownEvent")
            .expect("root key handler")..];
        let first = body.lines().skip(1).find(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//")
        });
        assert_eq!(
            first.map(str::trim),
            Some("if is_quit_chord(&event.keystroke) {")
        );
        // The chord quits (or confirms Quit Anyway) before the system-modal
        // quit prompt, and that prompt routes before every other overlay.
        let chord = body.find("self.user_quit(cx);").expect("quit chord action");
        let prompt = body
            .find("self.quit_prompt_key(event, window, cx)")
            .expect("quit prompt routing");
        let picker = body.find("self.picker_key(event, cx)").expect("picker");
        assert!(chord < prompt && prompt < picker);
        let quit_app = root
            .find("_: &QuitApp")
            .map(|at| &root[at..at + 80])
            .expect("menu quit action");
        assert!(quit_app.contains("this.user_quit(cx)"));
        let mut handlers = 0;
        for (name, source) in sources {
            for marker in [".on_key_down(", ".capture_key_down(", ".on_key_up("] {
                for (at, _) in source.match_indices(marker) {
                    // Skip this test's own marker literals.
                    if source[..at].ends_with('"') {
                        continue;
                    }
                    handlers += 1;
                    let window = &source[at..(at + 600).min(source.len())];
                    assert!(
                        window.starts_with(".on_key_down(cx.listener(Self::key))")
                            || window.contains("grip_owns_key(")
                            || window.contains("is_quit_chord("),
                        "{name}: key handler at byte {at} must let Ctrl+Q reach request_quit"
                    );
                }
            }
        }
        assert_eq!(handlers, 2, "root and dock-grip handlers");

        // Preferences and dialog keyboard focus: focused controls are children
        // of the root with no key handlers of their own (counted above), so
        // keys bubble to `key()`, which routes Preferences only after the quit
        // check. A focus handle left on a closed dialog is handed back to the
        // root every frame, so the chord never dispatches to a dead node.
        let quit = body.find("is_quit_chord(&event.keystroke)").unwrap();
        let preferences = body
            .find("self.preferences_key(event, window, cx)")
            .expect("Preferences keyboard routing");
        assert!(quit < preferences, "Preferences keys route after quit");
        let preferences_source = sources
            .iter()
            .find(|(name, _)| *name == "preferences.rs")
            .unwrap()
            .1;
        let handler = &preferences_source[preferences_source
            .find("fn preferences_key(")
            .expect("preferences_key")..];
        let handler = &handler[..handler.find("\n    }\n").unwrap()];
        assert!(
            !handler.contains("\"q\""),
            "preferences_key must not claim the quit key"
        );
        assert!(root.contains(
            "if self.dialog_focus.borrow().focus_is_stale(window) {\n            window.focus(&self.focus);"
        ));

        // Culling keys (held batch decisions, Home/End/Page jumps, keyboard
        // pan, Shift+B/Shift+C/Shift+Tab) route after the quit check too, and
        // none of their handlers claims Q.
        for routed in [
            "self.bulk_key(event, window, cx)",
            "self.jump_key(key, modifiers.shift, cx)",
            "Cmd::PanStep(dx, dy)",
            "\"b\" => Some(Cmd::SwapPanes)",
            "\"c\" => Some(Cmd::CompareSync)",
            "\"tab\" => Some(Cmd::Focus)",
            // Inspection aids: the clipping overlay and Alt+digit zoom presets.
            "\"j\" => Some(Cmd::Clipping)",
            // Photo search (Ctrl/Cmd+F) opens the palette, after quit.
            "if primary && key == \"f\"",
            "loupe_aids::ZOOM_PRESETS.iter()",
        ] {
            let at = body
                .find(routed)
                .unwrap_or_else(|| panic!("{routed} routing"));
            assert!(quit < at, "{routed} routes after quit");
        }
        let culling = sources
            .iter()
            .find(|(name, _)| *name == "culling.rs")
            .unwrap()
            .1;
        for handler in ["fn bulk_key(", "fn jump_key("] {
            let start = culling.find(handler).expect(handler);
            let end = start + culling[start..].find("\n    }\n").unwrap();
            assert!(
                !culling[start..end].contains("\"q\""),
                "{handler} must not claim the quit key"
            );
        }
        // The quit prompt's Tab/arrow/Return handling and the guide's
        // scrolling keys route after the chord and never claim Q.
        for (file, handler) in [
            ("feedback.rs", "fn quit_prompt_key("),
            ("help.rs", "fn getting_started_key("),
        ] {
            let source = sources.iter().find(|(name, _)| *name == file).unwrap().1;
            let start = source.find(handler).expect(handler);
            let end = start + source[start..].find("\n    }\n").unwrap();
            assert!(
                !source[start..end].contains("\"q\""),
                "{handler} must not claim the quit key"
            );
        }
        let guide = body
            .find("self.getting_started_key(event, window, cx)")
            .expect("guide keyboard routing");
        assert!(quit < guide, "guide keys route after quit");
        // F6 pane focus, Shift+F10 / Menu (the keyboard image menu) and the
        // open image menu's own keys route after the chord, and neither the
        // menu nor a focused pane claims Q: Ctrl+Q quits with the image menu
        // open and with the library or inspector focused.
        for routed in [
            "self.image_context_key(event, window, cx)",
            "self.cycle_pane(modifiers.shift, cx)",
            "self.open_image_context_from_keyboard(cx)",
            "self.pane_key(event, window, cx)",
        ] {
            let at = body
                .find(routed)
                .unwrap_or_else(|| panic!("{routed} routing"));
            assert!(quit < at, "{routed} routes after quit");
        }
        for (file, handler) in [
            ("review_actions.rs", "fn image_context_key("),
            ("panes.rs", "fn pane_key("),
        ] {
            let source = sources.iter().find(|(name, _)| *name == file).unwrap().1;
            let start = source.find(handler).expect(handler);
            let end = start + source[start..].find("\n    }\n").unwrap();
            let handler_body = &source[start..end];
            assert!(
                !handler_body.contains("\"q\""),
                "{handler} must not claim the quit key"
            );
        }
        // Menu accelerators exclude chords: ImageAction::for_key never sees
        // Ctrl/Cmd, so Ctrl+Q cannot run a menu action.
        let menu = sources
            .iter()
            .find(|(name, _)| *name == "review_actions.rs")
            .unwrap()
            .1;
        let accelerators = &menu[menu.find("fn for_key(").expect("for_key")..];
        let accelerators = &accelerators[..accelerators.find("\n    }\n").unwrap()];
        assert!(!accelerators.contains("\"q\""));
        assert!(menu.contains("key if plain =>"));
    }

    #[test]
    fn zoom_geometry_maps_pointer_to_image_and_back() {
        use gpui::{Bounds, size};
        // A 900 x 600 pane at (100, 50) showing a 3000 x 2000 texture of a
        // 6000 x 4000 source on a 1x display.
        let g = PaneGeometry {
            bounds: Bounds::new(point(px(100.), px(50.)), size(px(900.), px(600.))),
            width: 3000,
            height: 2000,
            source_scale: 2.,
            scale_factor: 1.,
        };
        assert!(g.valid());
        assert!((g.fit_scale() - 0.3).abs() < 1e-6);
        // At Fit the picture fills the pane: its centre is image (0.5, 0.5),
        // and the left third of the pane is the left third of the photo.
        let centre = g.image_point(point(px(550.), px(350.)), g.fit_scale(), [0.5, 0.5]);
        assert!((centre[0] - 0.5).abs() < 1e-4 && (centre[1] - 0.5).abs() < 1e-4);
        let left = g.image_point(point(px(400.), px(350.)), g.fit_scale(), [0.5, 0.5]);
        assert!((left[0] - 1. / 3.).abs() < 1e-3);
        // Centring that point at 100% and reading it back lands on it.
        let scale = g.zoom_scale(1.);
        let pan = g.pan_for(left, [450., 300.], scale);
        let back = g.image_point(point(px(550.), px(350.)), scale, pan);
        assert!((back[0] - left[0]).abs() < 1e-3 && (back[1] - 0.5).abs() < 1e-3);
        // An anchored step keeps the point under a cursor fixed on screen.
        let cursor = point(px(700.), px(200.));
        let u = g.image_point(cursor, scale, pan);
        let zoomed = g.pan_for(u, [600., 150.], g.zoom_scale(1.25));
        let after = g.image_point(cursor, g.zoom_scale(1.25), zoomed);
        assert!((after[0] - u[0]).abs() < 1e-3 && (after[1] - u[1]).abs() < 1e-3);
        // A smaller-than-pane image stays centred.
        assert_eq!(g.pan_for([0.9, 0.9], [0., 0.], 0.1), [0.5, 0.5]);
    }

    #[test]
    fn selection_summary_reports_shared_and_mixed_decisions() {
        let photos = sample_photos();
        let mut summary = SelectionSummary::default();
        let mut pick = photos[0].clone();
        pick.flag = Flag::Pick;
        pick.rating = 5;
        let mut other = pick.clone();
        other.rating = 3;
        summary.add(&pick);
        summary.add(&other);
        assert_eq!(summary.count, 2);
        assert_eq!(summary.common_flag(), Some(Flag::Pick));
        assert_eq!(summary.common_rating(), None);
        assert_eq!(summary.common_label(), Some(other.label));
        let mut none = pick.clone();
        none.flag = Flag::None;
        summary.add(&none);
        assert_eq!(summary.common_flag(), None);
        assert!(summary.has_flag(Flag::Pick) && !summary.has_flag(Flag::Reject));
    }

    #[test]
    fn survey_names_selection_and_neighbour_sources() {
        assert_eq!(
            SurveyKey::Selection(4).label(4).as_deref(),
            Some("Selection · 4 photos")
        );
        assert_eq!(
            SurveyKey::Neighbours(0).label(9).as_deref(),
            Some("Neighbours · 9 photos")
        );
        assert_eq!(SurveyKey::Group(false, 2).label(7), None);
    }
    #[test]
    fn survey_layout_picks_the_columns_with_the_largest_frames() {
        // Seven landscape frames in a 1024 px window with both docks hidden:
        // four columns of ≥ 230 px cells, not three of ~166 px.
        let (cols, w, h) = survey_layout(7, 1.5, 992., 356.);
        assert_eq!(cols, 4);
        assert!(w >= 230., "{w}");
        assert!((w / h - 1.5).abs() < 0.01);
        // The cells and gaps never exceed the area.
        let rows = 7usize.div_ceil(cols) as f32;
        assert!(cols as f32 * w + 3. * space::SM <= 992. + 0.5);
        assert!(rows * h + (rows - 1.) * space::SM <= 356. + 0.5);
        // A pair side by side in a wide pane; a portrait burst gets tall cells.
        assert_eq!(survey_layout(2, 1.5, 1200., 600.).0, 2);
        let (_, w, h) = survey_layout(6, 2. / 3., 1200., 600.);
        assert!(h > w && h >= 290., "{w} × {h}");
        assert_eq!(survey_layout(0, 1.5, 100., 100.).0, 1);
    }

    #[test]
    fn overlays_stack_palette_and_guide_above_preferences() {
        let rank = |overlay| Overlay::STACK.iter().position(|&o| o == overlay).unwrap();
        assert!(rank(Overlay::Palette) < rank(Overlay::Preferences));
        assert!(rank(Overlay::GettingStarted) < rank(Overlay::Preferences));
        assert!(rank(Overlay::FolderPicker) < rank(Overlay::Preferences));
        let mut seen = Overlay::STACK.to_vec();
        seen.dedup();
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn decisions_keep_the_selected_photo_when_others_leave_the_view() {
        // Unreviewed view [A B C D]; C is selected while a ctrl-click batch
        // holds only A. Rating A removes it, shifting C to index 1.
        let before = ["A", "B", "C", "D"];
        let after = ["B", "C", "D"];
        let selected = 2;
        let kept = after.iter().position(|&id| id == before[selected]);
        let position = retained_position(kept, selected, after.len());
        assert_eq!(after[position], "C", "selection must not slide onto D");
        // The selected photo itself left the view: its successor slides in.
        let after = ["A", "B", "D"];
        let kept = after.iter().position(|&id| id == before[selected]);
        assert_eq!(after[retained_position(kept, selected, after.len())], "D");
        // The last photo left: clamp to the new end; empty views stay at 0.
        assert_eq!(retained_position(None, 3, 3), 2);
        assert_eq!(retained_position(None, 3, 0), 0);
    }

    /// Per-frame, per-cell work in the grid/filmstrip that does not need a
    /// window: display identity (twice per cell: `has_texture` and
    /// `texture_photo`), the filename label and the star string. Run with
    /// `--ignored --nocapture`; prints ns per cell over 10k RAW+JPEG photos.
    #[test]
    #[ignore]
    fn bench_grid_cell_frame_work() {
        use std::hint::black_box;
        let photos: Vec<Photo> = (0..10_000)
            .map(|id| Photo {
                id,
                rating: (id % 6) as u8,
                filename: format!("IMG_{id:05}.CR3"),
                path: format!("/home/user/Pictures/2026/11-09 Shoot/IMG_{id:05}.CR3").into(),
                companion_path: Some(
                    format!("/home/user/Pictures/2026/11-09 Shoot/IMG_{id:05}.JPG").into(),
                ),
                ..Photo::default()
            })
            .collect();
        let time = |label: &str, f: &dyn Fn(&Photo) -> usize| {
            let mut best = f64::MAX;
            for _ in 0..20 {
                let start = Instant::now();
                let mut sum = 0;
                for p in &photos {
                    sum += f(black_box(p));
                }
                black_box(sum);
                best = best.min(start.elapsed().as_secs_f64() * 1e9 / photos.len() as f64);
            }
            println!("{label}: {best:.1} ns/cell");
            best
        };
        time("display identity x2", &|p| {
            let a = (super::preview_mtime(p), super::preview_size(p));
            let b = (super::preview_mtime(p), super::preview_size(p));
            (a.1 ^ b.1) as usize
        });
        time("filename clone", &|p| p.filename.clone().len());
        let repeat = time("stars via repeat", &|p| "★".repeat(p.rating as usize).len());
        let table = time("stars via table", &|p| super::stars(p.rating).len());
        println!("star string saved {:.1} ns/cell", repeat - table);
    }

    /// UI-thread cost of receiving a catalog snapshot: the list update plus
    /// the catalog-wide indices `poll` rebuilds (positions, counts, bursts,
    /// the filtered list, folders, texture retirement). Run with
    /// `--release --bins -- --ignored --nocapture bench_snapshot_apply`.
    #[test]
    #[ignore]
    fn bench_snapshot_apply() {
        use shadowroom::snapshot::{PhotoDelta, apply_delta, positions, snapshot_order};
        use std::hint::black_box;
        fn photo(id: i64) -> Photo {
            Photo {
                id,
                path: format!("/bench/folder{:03}/frame{id:06}.nef", id / 100).into(),
                filename: format!("frame{id:06}.nef"),
                folder: format!("/bench/folder{:03}", id / 100),
                captured_at: Some(1_704_067_200_000 + id * 120),
                modified_at: 1_704_067_200_000 + id,
                camera: "NIKON Z 8".into(),
                lens: "NIKKOR Z 24-120mm f/4 S".into(),
                shutter: "1/500".into(),
                companion_path: Some(
                    format!("/bench/folder{:03}/frame{id:06}.jpg", id / 100).into(),
                ),
                rating: (id % 6) as u8,
                ..Photo::default()
            }
        }
        // Everything poll derives from the list once it changed.
        fn rebuild(photos: &[Photo], groups: &[Vec<i64>]) -> usize {
            let index = positions(photos);
            let counts = CollectionCounts::from_photos(photos);
            let bursts = BurstIndex::new(groups, photos, &index);
            let filtered: Vec<usize> = photos
                .iter()
                .enumerate()
                .filter(|(_, p)| Filter::All.accepts(p) && p.matches_scope("/bench/folder001"))
                .map(|(i, _)| i)
                .collect();
            let mut filtered_positions = Vec::new();
            index_filtered_positions(&mut filtered_positions, photos.len(), &filtered);
            let folders = super::folder_counts(photos);
            // Resident textures (a few hundred) look up their photo by ID.
            let textures = (0..400)
                .filter_map(|id| index.get(&(id * 37)).map(|&i| &photos[i]))
                .map(|p| (super::preview_mtime(p), super::preview_size(p)))
                .count();
            counts.all + bursts.slots.len() + filtered.len() + folders.len() + textures
        }
        fn best(mut run: impl FnMut() -> Duration) -> Duration {
            (0..7).map(|_| run()).min().unwrap()
        }
        for n in [10_000i64, 50_000] {
            let mut base: Vec<Photo> = (0..n).map(photo).collect();
            base.sort_by(snapshot_order);
            let groups: Vec<Vec<i64>> = (0..n)
                .collect::<Vec<_>>()
                .chunks(8)
                .map(<[i64]>::to_vec)
                .collect();
            // Previous behaviour: a full replacement list, every index rebuilt,
            // and a revision map over every photo for texture retirement.
            let full = best(|| {
                let snapshot = base.clone();
                let started = Instant::now();
                let photos = snapshot;
                let revisions: HashMap<_, _> = photos
                    .iter()
                    .map(|p| (p.id, (super::preview_mtime(p), super::preview_size(p))))
                    .collect();
                black_box((rebuild(&photos, &groups), revisions.len()));
                let elapsed = started.elapsed();
                drop(photos);
                elapsed
            });
            println!("BENCH {n}: full snapshot apply {full:?}");
            let index = positions(&base);
            let unchanged = best(|| {
                let mut photos = base.clone();
                let delta = PhotoDelta {
                    upserts: (0..100).map(|i| base[i * 7].clone()).collect(),
                    removed: Vec::new(),
                };
                let started = Instant::now();
                let changed = apply_delta(&mut photos, &index, delta);
                assert!(!changed);
                started.elapsed()
            });
            println!("BENCH {n}: delta of 100 already-applied decisions {unchanged:?}");
            let indexed = best(|| {
                let mut photos = base.clone();
                let delta = PhotoDelta {
                    upserts: (n..n + 500).map(photo).collect(),
                    removed: Vec::new(),
                };
                let started = Instant::now();
                assert!(apply_delta(&mut photos, &index, delta));
                black_box(rebuild(&photos, &groups));
                started.elapsed()
            });
            println!("BENCH {n}: delta of 500 indexed photos + rebuild {indexed:?}");
            let decided = best(|| {
                let mut photos = base.clone();
                let delta = PhotoDelta {
                    upserts: (0..100)
                        .map(|i| Photo {
                            rating: 5 - base[i * 7].rating,
                            ..base[i * 7].clone()
                        })
                        .collect(),
                    removed: Vec::new(),
                };
                let started = Instant::now();
                assert!(apply_delta(&mut photos, &index, delta));
                black_box(rebuild(&photos, &groups));
                started.elapsed()
            });
            println!("BENCH {n}: delta of 100 external decisions + rebuild {decided:?}");
        }
    }
}
