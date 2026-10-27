mod deletion_ui;
mod help;
mod ingest_ui;
mod layout;
mod preferences;
mod review_actions;
mod scrollbar;
mod text_input;
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use gpui::{prelude::*, *};
use layout as ui;
use layout::DockSide;
use layout::{
    DECISION_HEIGHT, FILMSTRIP_HEIGHT, INFO_WIDTH, LIBRARY_WIDTH, TITLE_HEIGHT, TOOLBAR_HEIGHT,
    space,
};
use serde_json::{Value, json};
use shadowroom::{
    automation::{AutomationRequest, AutomationServer},
    indexer::Drive,
    media::{ColorStatus, MediaPipeline, PreviewKind},
    model::{ColorLabel, Flag, Photo, PhotoDecision},
    service::{GroupSlot, IoCommand, IoEvent, PhotoSlot, start_io},
    settings::{INTERFACE_FONTS, Settings},
    theme::Theme,
    theme_watch::{ThemeUpdate, ThemeWatcher, ZedSelection, start_theme_watch},
    workspace::{Decision, Filter, History, SaveRevisions, fuzzy_score},
};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::Duration,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    Import,
    AddRemoteHost,
    SaveRemoteHost,
    RemoteAction(String, String, Option<String>),
    ExportPicks,
    ExportXmp,
    Palette,
    Preferences,
    ApplyPreferences,
    ApplyClosePreferences,
    Appearance,
    PreferencesSection(PreferencesSection),
    Close,
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
    SetAutoDrives(bool),
    Font(String),
    Theme(String),
    SystemTheme(String),
    ImportTheme,
    Refresh,
    Reveal,
    AllSelect,
    Gap(u64),
    Threshold(u32),
    Controls,
    RetrySaves,
    RetryPreview,
    Quit,
    CancelImport,
    HistogramLog,
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
    ScanDuplicates,
    CancelDuplicates,
    ShowDuplicates,
    PreviewDetails,
    LabelFilter(ColorLabel),
    BurstsOnly,
    RatedAtLeast(u8),
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
    items: Vec<PaletteItem>,
    matches: Vec<usize>,
    query: Option<String>,
    grouping: String,
    availability: [bool; 7],
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
            scored.sort_by_key(|&(_, i)| {
                recent
                    .iter()
                    .position(|title| *title == self.items[i].title)
                    .unwrap_or(usize::MAX)
            });
        } else {
            scored.sort_by(|a, b| b.0.cmp(&a.0));
        }
        self.matches = scored.into_iter().map(|(_, i)| i).collect();
        self.query = Some(query.into());
    }
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

fn index_filtered_positions(positions: &mut Vec<usize>, photo_count: usize, filtered: &[usize]) {
    positions.resize(photo_count, usize::MAX);
    positions.fill(usize::MAX);
    for (position, &i) in filtered.iter().enumerate() {
        positions[i] = position;
    }
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
    photos: Vec<Photo>,
    photo_index: HashMap<i64, usize>,
    counts: CollectionCounts,
    filtered_positions: Vec<usize>,
    filtered_reviewed: usize,
    decision_membership_changed: bool,
    bursts: BurstIndex,
    duplicate_slots: HashMap<i64, (usize, usize, usize)>,
    survey_cache: Option<((bool, usize), Arc<SurveySummary>)>,
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
    drag_origin: Option<Point<Pixels>>,
    zoom: bool,
    zoom_factor: f32,
    fit_scales: std::rc::Rc<std::cell::Cell<[f32; 2]>>,
    focus_peaking: bool,
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
    palette_query: String,
    palette_index: usize,
    palette_cache: RefCell<Option<PaletteCache>>,
    palette_revision: u64,
    preferences: bool,
    preferences_draft: Option<preferences::Draft>,
    preferences_section: PreferencesSection,
    deletion: deletion_ui::State,
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
    settings_dirty: bool,
    settings_to_send: bool,
    settings_failure: Option<String>,
    unsaved: HashMap<i64, Decision>,
    outbox: VecDeque<Vec<PhotoDecision>>,
    awaiting: VecDeque<Vec<PhotoDecision>>,
    save_revisions: SaveRevisions,
    recovery_path: PathBuf,
    recovery_pending: bool,
    recovery_broken: bool,
    recovery_settings: Option<Settings>,
    quitting: bool,
    quit_inflight: bool,
    import_ui: ingest_ui::ImportUi,
    pending_ingest_scope: Option<PathBuf>,
    export_jobs: Vec<Value>,
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
    error: Option<String>,
    busy: bool,
    pending_saves: usize,
    drives: Vec<Drive>,
    remote_hosts: std::collections::BTreeMap<String, shadowroom::network::HostSnapshot>,
    add_host: Option<([String; 2], usize)>,
    grid_scroll: UniformListScrollHandle,
    grid_range: Option<(usize, usize)>,
    last_media: u64,
    automation: Option<AutomationServer>,
    _poll: Task<()>,
    _automation_wake: Option<Task<()>>,
    _shutdown: Subscription,
}

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
        let themes = vec![Theme::dark(), Theme::light()];
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
            if !this.unsaved.is_empty()||this.settings_dirty||this.recovery_pending {
                let decisions=this.unsaved.values().map(|d|PhotoDecision{id:d.id,rating:d.rating,flag:d.flag,label:d.label,trashed:d.trashed}).collect();
                let journal=shadowroom::recovery::RecoveryJournal::new(decisions,if this.settings_dirty||this.recovery_settings.is_some(){Some(this.settings.clone())}else{None});
                if let Err(error)=journal.save(&this.recovery_path){eprintln!("CRITICAL: pending decision recovery could not be saved: {error:#}");}
            }
            let (ack,done)=crossbeam_channel::bounded(1);
            if shutdown.send_timeout(IoCommand::Shutdown(ack),Duration::from_secs(1)).is_ok(){
                if let Err(e)=done.recv_timeout(Duration::from_secs(6)){eprintln!("Shutdown did not finish; pending decisions remain in the recovery journal: {e}");}
            }async {}
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(50)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        });
        let import_ui = ingest_ui::ImportUi::new(&settings);
        let inspector_info =
            settings.extra.get("inspector_tab").and_then(Value::as_str) == Some("info");
        let getting_started = settings.show_getting_started;
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
            drag_origin: None,
            zoom: false,
            zoom_factor: 1.,
            fit_scales: std::rc::Rc::new(std::cell::Cell::new([1.; 2])),
            focus_peaking: false,
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
            palette_query: String::new(),
            palette_index: 0,
            palette_cache: RefCell::default(),
            palette_revision: 0,
            preferences: false,
            preferences_draft: None,
            preferences_section: PreferencesSection::default(),
            deletion: deletion_ui::State::default(),
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
            settings_dirty: false,
            settings_to_send: false,
            settings_failure: None,
            history: History::default(),
            status: "Opening local catalog…".into(),
            error: settings_error,
            unsaved: HashMap::new(),
            outbox: VecDeque::new(),
            awaiting: VecDeque::new(),
            save_revisions: SaveRevisions::default(),
            recovery_path,
            recovery_pending,
            recovery_broken,
            recovery_settings,
            quitting: false,
            quit_inflight: false,
            import_ui,
            pending_ingest_scope: None,
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
            pending_saves: 0,
            drives: Vec::new(),
            remote_hosts: Default::default(),
            add_host: None,
            grid_scroll: UniformListScrollHandle::new(),
            grid_range: None,
            last_media: 0,
            automation,
            _poll: poll,
            _automation_wake: None,
            _shutdown: shutdown_subscription,
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
                workspace.awaiting.push_back(recovery.decisions.clone());
                workspace.outbox.push_back(recovery.decisions);
            }
            if recovery.settings.is_some() {
                // Keep this obligation even when malformed normal settings block
                // writes. Only an explicit preferences ACK releases the payload.
                workspace.settings_dirty = true;
                workspace.settings_to_send = workspace.settings_writable;
                if !workspace.settings_writable {
                    workspace.settings_failure=Some("Repair the settings file, then use Retry unsaved decisions to recover preferences.".into());
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
        let value = self
            .theme
            .colors
            .get(key)
            .or_else(|| self.fallback_theme.colors.get(key))
            .map(String::as_str)
            .unwrap_or(fallback);
        let c = shadowroom::theme::Rgba::parse(value)
            .or_else(|| {
                self.fallback_theme
                    .colors
                    .get(key)
                    .and_then(|s| shadowroom::theme::Rgba::parse(s))
            })
            .unwrap_or(shadowroom::theme::Rgba {
                r: 128,
                g: 128,
                b: 128,
                a: 255,
            });
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
    fn review_photo(&self) -> Option<&Photo> {
        if self.filter == Filter::Trash {
            return self.selected_photo();
        }
        if self.mode == ViewMode::Compare && self.reference_focus {
            self.reference
                .and_then(|id| self.photo_index.get(&id).map(|&i| &self.photos[i]))
        } else {
            self.selected_photo()
        }
    }
    fn selected_id(&self) -> Option<i64> {
        self.selected_photo().map(|p| p.id)
    }
    fn save_settings(&mut self) {
        if self.settings_writable {
            self.settings_dirty = true;
            self.settings_to_send = true;
        }
    }
    fn drain_settings(&mut self) {
        if self.settings_to_send && self.settings_writable {
            match self.io.try_send(IoCommand::Settings(self.settings.clone())) {
                Ok(()) => self.settings_to_send = false,
                Err(crossbeam_channel::TrySendError::Full(_)) => {}
                Err(_) => self.settings_failure = Some("Preferences writer is disconnected".into()),
            }
        }
    }
    fn apply_theme_mode(&mut self, window: &Window) {
        let dark = matches!(
            window.appearance(),
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
                self.error = Some(format!("Catalog is unavailable or busy: {e}"));
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
        if self.quitting {
            return Err("The app is closing; new exports are not accepted.".into());
        }
        if !self.unsaved.is_empty() {
            return Err(
                "Wait for pending decisions to save, or retry failed decisions, before exporting."
                    .into(),
            );
        }
        for photo in self.photos.iter().filter(|photo| {
            !photo.trashed
                && if picks {
                    photo.flag == Flag::Pick
                } else {
                    ids.contains(&photo.id)
                }
        }) {
            photo
                .require_local("Export and XMP writing")
                .map_err(|e| e.to_string())?;
        }
        self.next_job_id += 1;
        let id = self.next_job_id;
        if !self.enqueue(IoCommand::ExportTracked {
            id,
            destination,
            picks,
            ids,
        }) {
            return Err("Export was not queued".into());
        }
        self.export_jobs
            .push(json!({"id":id,"state":"queued","completed":0,"error":null}));
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
                    self.awaiting.clear();
                    self.save_revisions.disconnect();
                    self.error=Some("Decision writer is disconnected. Your changes remain unsaved in this window.".into());
                    break;
                }
                _ => unreachable!(),
            }
        }
        self.pending_saves = self.awaiting.len();
    }
    fn request_quit(&mut self, cx: &mut gpui::Context<Self>) {
        if self.deletion.executing() {
            self.status = "Wait for confirmed file deletion to finish before closing".into();
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
        if self.quit_inflight {
            return;
        }
        self.quitting = true;
        self.status = "Finishing decision writes…".into();
        if self
            .export_jobs
            .iter()
            .any(|job| matches!(job["state"].as_str(), Some("queued" | "running")))
        {
            self.status = "Finishing exports before closing…".into();
            cx.notify();
            return;
        }
        // Imports publish verified groups atomically; let the active one finish (or
        // cancel a paused one, which rolls back only the in-flight group) before closing.
        match self.import_ui.progress.phase.as_str() {
            "queued" | "copying" | "verifying" | "indexing" => {
                self.status = "Finishing import before closing…".into();
                cx.notify();
                return;
            }
            "paused" => {
                self.enqueue(IoCommand::IngestCancel);
                self.status = "Cancelling paused import before closing…".into();
                cx.notify();
                return;
            }
            _ => {}
        }
        if !self.unsaved.is_empty() && self.awaiting.is_empty() {
            self.quitting = false;
            self.error = Some(format!(
                "{} photographs have unsaved decisions. Use Retry unsaved decisions before closing.",
                self.unsaved.len()
            ));
            cx.notify();
            return;
        }
        if self.recovery_settings.is_some() && !self.settings_writable {
            self.quitting = false;
            self.error=Some("Recovered preferences are waiting for repair of the settings file. Repair it and use Retry unsaved decisions before closing.".into());
            cx.notify();
            return;
        }
        self.drain_settings();
        if !self.awaiting.is_empty() || !self.outbox.is_empty() || self.settings_to_send {
            cx.notify();
            return;
        }
        self.quit_inflight = true;
        let (ack, done) = crossbeam_channel::bounded(1);
        if !self.enqueue(IoCommand::Flush(ack)) {
            self.quit_inflight = false;
            self.quitting = false;
            return;
        }
        let task = cx
            .background_executor()
            .spawn(async move { done.recv_timeout(Duration::from_secs(5)) });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            if result.is_ok(){this.settings_dirty=false;cx.quit();}else{this.quitting=false;this.quit_inflight=false;this.error=Some("Could not confirm catalog flush. The window stays open; your decisions have not been discarded.".into());cx.notify();}
        });}).detach();
    }

    fn poll(&mut self, cx: &mut gpui::Context<Self>) {
        let mut dirty = std::mem::take(&mut self.texture_retry);
        if self.palette || self.add_host.is_some() || self.import_ui.active_field.is_some() {
            dirty |= self.text_edit.blink();
        }
        while let Ok(update) = self.theme_updates.try_recv() {
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
                        self.import_ui.progress = shadowroom::ingest::Progress {
                            phase: "queued".into(),
                            ..Default::default()
                        };
                        self.import_ui.source = path;
                    } else {
                        self.open_ingest(path);
                    }
                }
                IoEvent::RemoteProvider(provider) => self.media.set_remote_provider(provider),
                IoEvent::Remote(snapshot) => {
                    self.palette_revision = self.palette_revision.wrapping_add(1);
                    if snapshot.state == "connected" {
                        self.media.retry_remote_errors();
                        if self
                            .remote_hosts
                            .get(&snapshot.config.id)
                            .is_some_and(|old| old.state == "indexing")
                        {
                            self.status =
                                format!("{} · metadata synced", snapshot.config.display_name);
                        }
                    }
                    if let Some(error) = &snapshot.error {
                        self.error = Some(format!("{}: {error}", snapshot.config.display_name));
                    }
                    self.remote_hosts
                        .insert(snapshot.config.id.clone(), snapshot);
                }
                IoEvent::RemoteRemoved(id) => {
                    self.remote_hosts.remove(&id);
                    self.palette_revision = self.palette_revision.wrapping_add(1);
                }
                IoEvent::Drives(d) => self.drives = d,
                IoEvent::Status(s) => self.status = s,
                IoEvent::Error(s) => self.error = Some(s),
                IoEvent::FilesDeleted(report) => self.deletion_finished(report),
                IoEvent::Busy(b) => self.busy = b,
                IoEvent::Saved => {
                    self.awaiting.pop_front();
                    for id in self.save_revisions.acknowledge(true) {
                        self.unsaved.remove(&id);
                    }
                    if self.unsaved.is_empty()
                        && self.error.as_ref().is_some_and(|e| {
                            e.starts_with("Decisions remain unsaved:")
                                || e.contains("photographs have unsaved decisions")
                        })
                    {
                        self.error = None;
                    }
                }
                IoEvent::SaveFailed(e) => {
                    self.awaiting.pop_front();
                    self.save_revisions.acknowledge(false);
                    self.error = Some(format!("Decisions remain unsaved: {e}"));
                }
                IoEvent::PreferencesSaved(settings) => {
                    if settings == self.settings {
                        self.settings_dirty = false;
                        self.settings_failure = None;
                        self.recovery_settings = None;
                        if self
                            .error
                            .as_ref()
                            .is_some_and(|e| e.starts_with("Preferences remain unsaved:"))
                        {
                            self.error = None;
                        }
                    }
                }
                IoEvent::PreferencesFailed(error) => {
                    self.settings_dirty = true;
                    self.settings_failure = Some(error.clone());
                    self.error = Some(format!("Preferences remain unsaved: {error}"));
                }
                IoEvent::DuplicateScan(mut scan) => {
                    if self
                        .duplicate_scan
                        .as_ref()
                        .is_none_or(|old| old.id <= scan.id)
                    {
                        if scan.phase == "completed" && !self.duplicate_results_valid(&scan.groups)
                        {
                            scan.phase = "stale".into();
                            scan.groups.clear();
                            self.duplicate_ids.clear();
                            self.duplicate_lookup.clear();
                            self.duplicate_slots.clear();
                        }
                        if scan.phase == "completed" {
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
                            self.status = format!(
                                "{} duplicate sets · {} photos",
                                scan.groups.len(),
                                self.duplicate_ids.len()
                            );
                            if !scan.errors.is_empty() || scan.errors_omitted > 0 {
                                self.status.push_str(&format!(
                                    " · {} errors",
                                    scan.errors.len() + scan.errors_omitted
                                ));
                            }
                            if scan.truncated {
                                self.status.push_str(" · results capped");
                            }
                        } else if scan.phase == "running" {
                            self.status = format!(
                                "Checking duplicates · {} of {} candidates",
                                scan.processed, scan.candidates
                            );
                        } else {
                            self.status = format!("Duplicate scan {}", scan.phase);
                            if scan.phase == "failed" {
                                self.error = scan.errors.first().cloned();
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
                    let value = json!({"id":id,"state":state,"completed":completed,"error":error});
                    if let Some(job) = self
                        .export_jobs
                        .iter_mut()
                        .find(|job| job["id"].as_u64() == Some(id))
                    {
                        *job = value;
                    } else {
                        self.export_jobs.push(value);
                    }
                    while self.export_jobs.len() > 100 {
                        // Preserve every nonterminal job for the close barrier;
                        // only completed history may be retired under a flood.
                        if let Some(index) = self.export_jobs.iter().position(|job| {
                            matches!(job["state"].as_str(), Some("completed" | "failed"))
                        }) {
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
        self.ingest_tick();
        if self.recovery_pending
            && self.unsaved.is_empty()
            && !self.settings_dirty
            && self.recovery_settings.is_none()
        {
            match shadowroom::recovery::RecoveryJournal::clear(&self.recovery_path) {
                Ok(()) => {
                    self.recovery_pending = false;
                    self.status = "Recovered pending decisions".into();
                }
                Err(error) => self.error = Some(format!("Could not finalize recovery: {error:#}")),
            }
        }
        if self.quitting && !self.quit_inflight {
            self.request_quit(cx);
        }
        if self.unsaved.is_empty() {
            let photos = self
                .photo_slot
                .try_lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(photos) = photos {
                if self.status == "Opening local catalog…" {
                    self.status = format!("{} photos", photos.len());
                }
                let selected = self.selected_id().or(self.settings.last_photo_id);
                self.analysis_cursor = 0;
                self.photos = photos;
                self.photo_index = self
                    .photos
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.id, i))
                    .collect();
                self.counts = CollectionCounts::from_photos(&self.photos);
                self.bursts = BurstIndex::new(&self.groups, &self.photos, &self.photo_index);
                self.rebuild(selected);
                self.refresh_folders();
                self.ingest_scope_arrived();
                self.invalidate_duplicate_results();
                dirty = true;
                let mut revisions: HashMap<_, _> = self
                    .photos
                    .iter()
                    .map(|p| (p.id, (preview_mtime(p), preview_size(p))))
                    .collect();
                revisions.extend(
                    self.import_ui
                        .previews
                        .iter()
                        .map(|p| (p.id, (preview_mtime(p), preview_size(p)))),
                );
                let obsolete: Vec<_> = self
                    .textures
                    .keys()
                    .filter(|key| revisions.get(&key.0) != Some(&(key.2, key.3)))
                    .copied()
                    .collect();
                for key in obsolete {
                    if let Some(t) = self.textures.remove(&key) {
                        self.texture_bytes -= t.bytes;
                        self.retired.push(t);
                    }
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
            cx.notify();
        }
    }

    fn refresh_folders(&mut self) {
        let mut folders = HashMap::new();
        for p in self.photos.iter().filter(|p| !p.trashed) {
            *folders.entry(p.folder_key()).or_insert(0) += 1;
        }
        self.folders = folders.into_iter().collect();
        self.folders.sort_by(|a, b| a.0.cmp(&b.0));
    }
    fn accepts_photo(&self, p: &Photo) -> bool {
        self.filter.accepts(p)
            && (self.mode == ViewMode::Grid
                || self.review_folder != self.scope
                || self
                    .review_subset
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&p.id)))
            && (!self.duplicates_only || self.duplicate_ids.contains(&p.id))
            && p.rating >= self.min_rating
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
        self.selected = keep
            .and_then(|id| self.visible_position(id))
            .unwrap_or(self.selected.min(self.filtered.len().saturating_sub(1)));
    }
    fn rebuild(&mut self, keep: Option<i64>) {
        self.survey_cache = None;
        // Reuse the allocation; positions also serve as the membership set.
        let mut filtered = std::mem::take(&mut self.filtered);
        filtered.clear();
        let mut reviewed = 0;
        filtered.extend(self.photos.iter().enumerate().filter_map(|(i, p)| {
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
            scan.phase == "completed" && !self.duplicate_results_valid(&scan.groups)
        }) {
            if let Some(scan) = &mut self.duplicate_scan {
                scan.phase = "stale".into();
                scan.groups.clear();
            }
            self.duplicate_ids.clear();
            self.duplicate_lookup.clear();
            self.duplicate_slots.clear();
            self.status = "Duplicate results changed · scan again".into();
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
    fn navigate(&mut self, delta: isize, cx: &mut gpui::Context<Self>) {
        self.catalog_hover = None;
        if self.filtered.is_empty() {
            return;
        }
        self.media.cancel_pending();
        self.navigation_direction = delta.signum();
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.filtered.len() - 1);
        self.multi.clear();
        if self.mode == ViewMode::Grid {
            if let Some(id) = self.selected_id() {
                self.multi.insert(id);
            }
        }
        self.selection_anchor = self.selected_id();
        self.settings.last_photo_id = self.selected_id();
        self.save_settings();
        self.grid_scroll
            .scroll_to_item(self.selected / self.grid_cols(), ScrollStrategy::Center);
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
        self.save_settings();
        self.read_ahead();
        cx.notify();
    }
    fn apply_decisions(&mut self, decisions: Vec<Decision>) -> bool {
        if self.recovery_pending || self.recovery_broken {
            self.error = Some(
                "Finish recovery before making new decisions. Retry unsaved decisions if needed."
                    .into(),
            );
            return false;
        }
        if self.quitting {
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
        for d in decisions_by_id.values() {
            let Some(&i) = self.photo_index.get(&d.id) else {
                continue;
            };
            let before = Decision::from(&self.photos[i]);
            let accepted_before = self.accepts_photo(&self.photos[i]);
            let reviewed_before = !Filter::Unrated.accepts(&self.photos[i]);
            self.counts.adjust(before, false);
            let p = &mut self.photos[i];
            p.rating = d.rating;
            p.flag = d.flag;
            p.label = d.label;
            p.trashed = d.trashed;
            self.counts.adjust(*d, true);
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
        self.awaiting.push_back(updates.clone());
        self.outbox.push_back(updates);
        self.drain_decisions();
        if self.import_ui.scan.is_some() && !self.import_ui.active() {
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
        let ids = if self.multi.is_empty() {
            self.review_photo().map(|p| p.id).into_iter().collect()
        } else {
            self.multi.clone()
        };
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
        self.history.push(before, after);
        let old = self.selected_id();
        let position = self.selected;
        self.rebuild_after_decisions(old);
        if allow_advance
            && self.settings.auto_advance
            && !self.reference_focus
            && (self.multi.is_empty()
                || (self.multi.len() == 1 && old.is_some_and(|id| self.multi.contains(&id))))
            && self.selected_id() == old
        {
            self.navigate(1, cx);
        } else {
            self.selected = position.min(self.filtered.len().saturating_sub(1));
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
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Trash change was not admitted".into()));
        }
        self.history.push(before, after);
        self.rebuild_after_decisions(self.selected_id());
        Ok(())
    }

    fn start_duplicate_scan(&mut self, ids: Vec<i64>) -> std::result::Result<u64, String> {
        if self
            .duplicate_scan
            .as_ref()
            .is_some_and(|s| matches!(s.phase.as_str(), "queued" | "running"))
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
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Duplicate scan was not admitted".into()));
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
            phase: "queued".into(),
            stage: "sizing".into(),
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

    fn choose_folder(&mut self, export: Option<bool>, cx: &mut gpui::Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                if export.is_some() {
                    "Choose export destination"
                } else {
                    "Import from card or folder"
                }
                .into(),
            ),
        });
        let ids = if self.multi.is_empty() {
            self.filtered
                .iter()
                .map(|&i| self.photos[i].id)
                .collect::<Vec<_>>()
        } else {
            self.multi.iter().copied().collect()
        };
        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(picks) = export {
                            this.status = "Exporting in background…".into();
                            if let Err(e) = this.queue_export(path, picks, ids) {
                                this.error = Some(e);
                            }
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
                    this.error = Some(format!("File picker: {e:#}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    fn execute(&mut self, cmd: Cmd, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.deletion.open()
            && !matches!(
                cmd,
                Cmd::ConfirmDelete | Cmd::CancelDelete | Cmd::Close | Cmd::Quit
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
        if self.quitting && !matches!(cmd, Cmd::Quit) {
            self.error = Some("The app is finishing writes and exports before closing.".into());
            cx.notify();
            return;
        }
        if self.recovery_pending
            && !matches!(cmd, Cmd::Quit | Cmd::RetrySaves | Cmd::Palette | Cmd::Close)
        {
            self.error = Some(
                "Recovery is pending. Wait for writes or use Retry unsaved decisions after repair."
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
        match cmd {
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
                    self.error = Some(error);
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
                    self.status = "No photos to scan in this view".into();
                    cx.notify();
                    return;
                }
                if let Err(error) = self.start_duplicate_scan(ids) {
                    self.error = Some(error);
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
            Cmd::AddRemoteHost => {
                self.close_preferences();
                self.palette = false;
                self.add_host =
                    Some(([String::new(), "shadowroom-remote serve --stdio".into()], 0));
                self.text_edit.focus("");
            }
            Cmd::SaveRemoteHost => {
                if let Some((fields, _)) = &self.add_host {
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
                        self.error = Some("Enter an SSH alias or user@host, without spaces".into());
                    } else if self.remote_hosts.contains_key(&config.id) {
                        self.error = Some(
                            "This host already exists; use Connect in the command palette".into(),
                        );
                    } else if self.enqueue(IoCommand::Remote(
                        shadowroom::network::RemoteCommand::Add(config),
                    )) {
                        self.add_host = None;
                        self.status = "Checking remote agent protocol and version…".into();
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
            Cmd::Import => self.choose_folder(None, cx),
            Cmd::ExportPicks => self.choose_folder(Some(true), cx),
            Cmd::ExportXmp => self.choose_folder(Some(false), cx),
            Cmd::Palette => {
                self.getting_started = false;
                self.import_ui.active_field = None;
                self.palette = !self.palette;
                self.palette_query.clear();
                self.text_edit.focus("");
                self.palette_cache.borrow_mut().take();
                self.palette_index = 0;
                self.close_preferences();
            }
            Cmd::Preferences => {
                self.getting_started = false;
                if self.preferences {
                    self.close_preferences();
                } else {
                    self.open_preferences();
                }
                self.palette = false;
            }
            Cmd::ApplyPreferences => self.apply_preferences(false, cx),
            Cmd::ApplyClosePreferences => self.apply_preferences(true, cx),
            Cmd::GettingStarted => {
                self.close_preferences();
                self.palette = false;
                self.getting_started = true;
            }
            Cmd::StartupGuide(enabled) => {
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
                if self.deletion.open() {
                    self.deletion.cancel();
                    cx.notify();
                    return;
                }
                if self.getting_started {
                    self.getting_started = false;
                    cx.notify();
                    return;
                }
                if self.rating_filter_open {
                    self.rating_filter_open = false;
                    cx.notify();
                    return;
                }
                if self.palette {
                    self.palette = false;
                } else if self.preferences {
                    self.close_preferences();
                } else if self.add_host.is_some() {
                    self.add_host = None;
                } else {
                    self.cancel_ingest_resize();
                    self.import_ui.active_field = None;
                    self.import_ui.open = false;
                    self.error = None;
                }
            }
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
            Cmd::Label(l) => self.decide(None, None, Some(l), true, cx),
            Cmd::Undo => {
                if let Some(d) = self.history.undo() {
                    let id = d.first().map(|d| d.id);
                    if self.apply_decisions(d) {
                        self.rebuild_after_decisions(id);
                        self.status = "Decision undone".into();
                    } else {
                        self.history.redo();
                    }
                }
            }
            Cmd::Redo => {
                if let Some(d) = self.history.redo() {
                    let id = d.first().map(|d| d.id);
                    if self.apply_decisions(d) {
                        self.rebuild_after_decisions(id);
                        self.status = "Decision restored".into();
                    } else {
                        self.history.undo();
                    }
                }
            }
            Cmd::View(mode) => self.change_photo_view(mode, mode == ViewMode::Compare, cx),
            Cmd::ReviewSelection => self.activate_review(cx),
            Cmd::ClearReviewSubset => {
                self.review_subset = None;
                self.rebuild(self.selected_id());
                self.read_ahead();
            }
            Cmd::Pin => {
                self.reference = self.selected_id();
                self.status = "Reference pinned · navigate to compare a candidate".into();
            }
            Cmd::Zoom => {
                self.zoom = !self.zoom;
                if self.zoom {
                    self.zoom_factor = 1.;
                }
                self.read_ahead();
            }
            Cmd::ZoomTo(factor) => {
                self.zoom = true;
                self.zoom_factor = factor.clamp(0.125, 8.);
                self.read_ahead();
            }
            Cmd::ZoomStep(direction) => {
                let current = if self.zoom {
                    self.zoom_factor
                } else {
                    self.fit_scales.get()
                        [usize::from(!(self.mode == ViewMode::Compare && self.reference_focus))]
                };
                self.zoom_factor =
                    (current * if direction > 0 { 1.25 } else { 0.8 }).clamp(0.125, 8.);
                self.zoom = true;
                self.read_ahead();
            }
            Cmd::FocusPeaking => {
                self.focus_peaking = !self.focus_peaking;
                self.status = if self.focus_peaking {
                    "Focus peaking on · cyan highlights fine edges · F toggles"
                } else {
                    "Focus peaking off"
                }
                .into();
            }
            Cmd::Inspector => {
                self.inspector = self.focus_mode || !self.inspector || self.docks.inspector == 0.;
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
            }
            Cmd::Histogram => self.histogram = !self.histogram,
            Cmd::PreviewDetails => self.preview_details = !self.preview_details,
            Cmd::Focus => self.focus_mode = !self.focus_mode,
            Cmd::Sidebar => {
                self.sidebar = self.focus_mode || !self.sidebar || self.docks.library == 0.;
                self.focus_mode = false;
                self.dock_priority = DockSide::Library;
                self.settings.show_sidebar = self.sidebar;
                self.save_settings();
            }
            Cmd::ResizeDock(side) => {
                if self.import_ui.open && side == DockSide::Inspector {
                    self.status =
                        "Inspector is available in Catalog, Review, Compare and Survey".into();
                    cx.notify();
                    return;
                }
                self.focus_mode = false;
                self.dock_priority = side;
                match side {
                    DockSide::Library => {
                        self.sidebar = true;
                        self.settings.show_sidebar = true;
                        window.focus(&self.library_resize_focus);
                    }
                    DockSide::Inspector => {
                        self.inspector = true;
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
                self.duplicates_only = false;
                self.label_filter = None;
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
                let id = self.selected_id();
                self.scope = None;
                self.duplicates_only = false;
                self.label_filter = None;
                self.bursts_only = false;
                self.filter = Filter::All;
                self.min_rating = r;
                self.settings.filter = "all".into();
                self.settings.min_rating = r;
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
            }
            Cmd::SetAutoAdvance(enabled) => {
                self.settings.auto_advance = enabled;
                self.save_settings();
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
            Cmd::ResolveBurst => {
                if let Some(id) = self.review_photo().map(|p| p.id) {
                    let group = self
                        .group_lookup
                        .get(&id)
                        .and_then(|g| self.groups.get(*g))
                        .cloned()
                        .unwrap_or_else(|| vec![id]);
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
                    if self.apply_decisions(after.clone()) {
                        self.history.push(before, after);
                        self.rebuild_after_decisions(Some(id));
                        self.status =
                            "Burst resolved · existing decisions preserved · undo available".into();
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
                        self.error = Some(error.to_string());
                    } else {
                        cx.reveal_path(&p.path);
                    }
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
            Cmd::HistogramLog => {
                self.histogram_log = !self.histogram_log;
                self.settings
                    .extra
                    .insert("histogram_log".into(), json!(self.histogram_log));
                self.save_settings();
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
                            self.error = Some(format!("Preferences still need repair: {error:#}"))
                        }
                    }
                }
                if self.awaiting.is_empty() && !self.unsaved.is_empty() {
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
        window.focus(match resize_focus {
            Some(DockSide::Library) => &self.library_resize_focus,
            Some(DockSide::Inspector) => &self.inspector_resize_focus,
            None => &self.focus,
        });
        cx.notify();
    }

    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = vec![
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
                "Import from Card/Folder…",
                "File",
                "⌘O",
                "folder-opened",
                Cmd::Import,
            ),
            item(
                "Export Picks…",
                "File · all catalog picks",
                "⌘⇧E",
                "export",
                Cmd::ExportPicks,
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
                "Reset Rating and Flag",
                "Review · clear stars and pick/reject without advancing; keep color label",
                "R",
                "unflagged",
                Cmd::ResetDecision,
            ),
            item(
                "Move to Trash",
                "Review · confirm configured Trash destination",
                "Delete",
                "trash",
                Cmd::Trash(true),
            ),
            item(
                "Delete Permanently…",
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
                "Find Duplicates…",
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
                "Survey Burst",
                "View · every frame of the current burst",
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
            item("Zoom In", "View", "+", "zoom", Cmd::ZoomStep(1)),
            item("Zoom Out", "View", "−", "zoom", Cmd::ZoomStep(-1)),
            item(
                "Focus Peaking",
                "View · cyan fine-detail highlights",
                "F",
                "focus",
                Cmd::FocusPeaking,
            ),
            item(
                "Show Full Folder",
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
            item("Toggle Catalog", "View", "⌘B", "panel-left", Cmd::Sidebar),
            item(
                "Resize Catalog",
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
                "Tab",
                "focus",
                Cmd::Focus,
            ),
            item("Pin as Reference", "Compare", "B", "pin", Cmd::Pin),
            item(
                "Toggle Synced Pan",
                "Compare",
                "",
                "sync-pan",
                Cmd::CompareSync,
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
            item("Preferences", "App", "⌘,", "settings", Cmd::Preferences),
            item(
                "Appearance & Themes…",
                "Preferences · Zed themes and fonts",
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
        ];
        for rating in 0..=5 {
            items.push(item(
                &if rating == 0 {
                    "Clear Rating".to_string()
                } else {
                    format!("Rate {}", "★".repeat(rating as usize))
                },
                &format!("Review · {rating} stars"),
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
                "Review",
                "",
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
                    &format!("Network · {}", host.state),
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
            .is_some_and(|s| matches!(s.phase.as_str(), "queued" | "running"));
        let save_failed = self.recovery_broken
            || self.settings_failure.is_some()
            || (!self.unsaved.is_empty() && self.pending_saves == 0);
        let in_trash = self.filter == Filter::Trash;
        let grouped = self.duplicates_only || !self.groups.is_empty();
        items.retain(|i| match &i.command {
            Cmd::CancelImport => self.busy,
            Cmd::RetryPreview => preview_failed,
            Cmd::CancelDuplicates => scanning,
            Cmd::RetrySaves => save_failed,
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
                .is_some_and(|s| matches!(s.phase.as_str(), "queued" | "running")),
            self.recovery_broken
                || self.settings_failure.is_some()
                || (!self.unsaved.is_empty() && self.pending_saves == 0),
            self.filter == Filter::Trash,
            self.duplicates_only || !self.groups.is_empty(),
            self.recovery_pending,
        ];
        let mut cell = self.palette_cache.borrow_mut();
        if cell.as_ref().is_none_or(|cache| {
            cache.availability != availability
                || cache.grouping != self.settings.grouping_mode
                || cache.revision != self.palette_revision
        }) {
            *cell = Some(PaletteCache {
                items: self.palette_items(),
                availability,
                grouping: self.settings.grouping_mode.clone(),
                revision: self.palette_revision,
                ..PaletteCache::default()
            });
        }
        let cache = cell.as_mut().expect("palette cache");
        cache.match_query(&self.palette_query, &self.palette_recent);
        drop(cell);
        std::cell::Ref::map(self.palette_cache.borrow(), |cache| {
            cache.as_ref().expect("palette cache")
        })
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut gpui::Context<Self>) {
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
        if key == "escape" {
            self.execute(Cmd::Close, window, cx);
            return;
        }
        if self.getting_started {
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
        if self.preferences {
            return;
        }
        if !self.palette && self.add_host.is_none() && self.ingest_key(event, cx) {
            return;
        }
        if self.add_host.is_some() && !self.palette {
            if key == "enter" {
                self.execute(Cmd::SaveRemoteHost, window, cx);
                return;
            }
            if let Some((fields, field)) = &mut self.add_host {
                if key == "tab" {
                    *field = 1 - *field;
                    self.text_edit.focus(&fields[*field]);
                } else {
                    self.text_edit.key(&mut fields[*field], event, cx);
                }
            }
            cx.notify();
            return;
        }
        if self.palette {
            match key {
                "up" => self.palette_index = self.palette_index.saturating_sub(1),
                "down" => {
                    let last = self.matches().matches.len().saturating_sub(1);
                    self.palette_index = (self.palette_index + 1).min(last);
                }
                "enter" => {
                    let selected = {
                        let cache = self.matches();
                        cache.matches.get(self.palette_index).map(|&i| {
                            (cache.items[i].command.clone(), cache.items[i].title.clone())
                        })
                    };
                    if let Some((cmd, title)) = selected {
                        self.remember_command(title);
                        self.palette = false;
                        self.execute(cmd, window, cx);
                    }
                }
                _ => {
                    self.text_edit.key(&mut self.palette_query, event, cx);
                    self.palette_index = 0;
                }
            }
            cx.notify();
            return;
        }
        if key == "tab" && self.mode == ViewMode::Compare {
            self.reference_focus = !self.reference_focus;
            cx.notify();
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
            self.grid_scroll
                .scroll_to_item(target / self.grid_cols(), ScrollStrategy::Center);
            return;
        }
        if modifiers.shift && !primary {
            let command = match key {
                "delete" | "backspace" => Some(Cmd::PermanentDelete),
                "right" => Some(Cmd::Unreviewed(1)),
                "left" => Some(Cmd::Unreviewed(-1)),
                "p" => Some(Cmd::ResolveBurst),
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
                C::ToggleZoom => Cmd::Zoom,
                C::ZoomIn => Cmd::ZoomStep(1),
                C::ZoomOut => Cmd::ZoomStep(-1),
                C::ToggleHistogram => Cmd::Histogram,
                C::ToggleInspector => Cmd::Inspector,
                C::ToggleAutoAdvance => Cmd::AutoAdvance,
                C::OpenFolder => Cmd::Import,
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
                "o" => Some(Cmd::Import),
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
                "z" | "f21" | "space" => Some(Cmd::Zoom),
                "f" => Some(Cmd::FocusPeaking),
                "+" | "=" => Some(Cmd::ZoomStep(1)),
                "-" => Some(Cmd::ZoomStep(-1)),
                "h" | "f22" => Some(Cmd::Histogram),
                "i" => Some(Cmd::Inspector),
                "tab" => Some(Cmd::Focus),
                "[" | "f23" => Some(Cmd::PreviousGroup),
                "]" | "f24" => Some(Cmd::NextGroup),
                "b" => Some(Cmd::Pin),
                "a" => Some(Cmd::AutoAdvance),
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

/// Swatch colour shared by the inspector, filmstrip and grid.
fn label_rgb(label: ColorLabel) -> Option<u32> {
    match label {
        ColorLabel::None => None,
        ColorLabel::Red => Some(0xd58b85),
        ColorLabel::Yellow => Some(0xd6bb7e),
        ColorLabel::Green => Some(0x9cbb95),
        ColorLabel::Blue => Some(0x8caed1),
        ColorLabel::Purple => Some(0xb39acb),
    }
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

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn icon(name: &'static str, color: Hsla) -> Svg {
    svg()
        .path(name)
        .size(px(16.))
        .text_color(color)
        .flex_shrink_0()
}
/// Platform spelling for shortcut hints: ⌘/⇧ glyphs on macOS, words elsewhere.
fn shortcut(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.to_owned()
    } else {
        keys.replace('⌘', "Ctrl+").replace('⇧', "Shift+")
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
            .text_size(px(11.))
            .text_color(self.fg)
            .bg(self.bg)
            .border_1()
            .border_color(self.border)
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
                if this.palette || this.preferences || this.add_host.is_some() || this.quitting { return; }
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
                if this.palette || this.preferences || this.add_host.is_some() || this.rating_filter_open { return; }
                let key = event.keystroke.key.as_str();
                if (event.keystroke.modifiers.platform || event.keystroke.modifiers.control) && key == "k" { return; }
                cx.stop_propagation();
                match key {
                    "left" | "right" => {
                        let step = if event.keystroke.modifiers.shift { 32 } else { 8 };
                        let sign = if (key == "right") == (side == DockSide::Library) { 1 } else { -1 };
                        let width = match side { DockSide::Library => this.docks.library, DockSide::Inspector => this.docks.inspector };
                        this.set_manual_dock_width(side, width + (step * sign) as f32);
                        this.save_settings();
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
            .text_size(px(11.))
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
                    .border_color(self.border())
                    .bg(self.canvas_bg())
                    .text_size(px(10.))
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
        // Controls also appear on elevated surfaces; a ghost hover can match
        // the dialog background exactly. Use the filled control hover there.
        let hover = self.c("element.hover", "#303030");
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
            .when(active, |s| {
                s.bg(self.selected_bg())
                    .border_color(self.c("border.focused", "#5b9bd5"))
            })
            .hover(move |s| s.bg(hover))
            .when(!glyph.is_empty(), |s| {
                s.child(icon(
                    glyph,
                    if active {
                        self.accent()
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
    fn control_divider(&self) -> Div {
        div()
            .w(px(1.))
            .flex_shrink_0()
            .h(px(space::LG))
            .mx(px(space::XS))
            .bg(self.border())
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
            .text_size(px(12.))
            .line_height(px(16.))
            .overflow_hidden()
            .cursor_pointer()
            .occlude()
            .text_color(if active { self.text() } else { self.muted() })
            .when(active, |s| s.bg(self.selected_bg()))
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
            .h(px(ui::SECTION_HEIGHT))
            .mt(px(space::LG))
            .px(px(ui::PANEL_INSET))
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(11.))
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
        let narrow = f32::from(window.viewport_size().width) < 1120.;
        let mut views = div().h_full().flex().items_center().gap(px(space::XS));
        for (mode, label, glyph, key) in [
            (ViewMode::Grid, "Catalog", "grid", "G"),
            (ViewMode::Loupe, "Review", "loupe", "L"),
            (ViewMode::Compare, "Compare", "compare", "C"),
            (ViewMode::Survey, "Survey", "group", "S"),
        ] {
            let active = self.mode == mode;
            let hover = self.hover_bg();
            views = views.child(
                div()
                    .id(SharedString::from(format!(
                        "header-{}",
                        label.to_lowercase()
                    )))
                    .h_full()
                    .px(px(if narrow { space::SM } else { space::MD }))
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
                    .cursor_pointer()
                    .occlude()
                    .hover(move |s| s.bg(hover))
                    .child(icon(
                        glyph,
                        if active {
                            self.accent()
                        } else {
                            self.icon_muted()
                        },
                    ))
                    .when(!narrow, |s| s.child(label))
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
                        self.button(
                            "title-library",
                            "",
                            "panel-left",
                            Cmd::Sidebar,
                            self.docks.library > 0.,
                            cx,
                        )
                        .tooltip(self.tip("Toggle catalog", "⌘B")),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Shadowroom"),
                    ),
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
                            .tooltip(self.tip("Search commands", palette_keys)),
                    )
                    .child(
                        self.button(
                            "getting-started-trigger",
                            "?",
                            "",
                            Cmd::GettingStarted,
                            self.getting_started,
                            cx,
                        )
                        .tooltip(self.tip("Getting Started · workflow and shortcuts", "F1")),
                    )
                    .child(
                        self.button(
                            "title-export",
                            if narrow { "" } else { "Export" },
                            "export",
                            Cmd::ExportPicks,
                            false,
                            cx,
                        )
                        .h(px(ui::CONTROL_HEIGHT))
                        .px(px(if narrow { 0. } else { space::MD }))
                        .bg(self.selected_bg())
                        .when(narrow, |s| s.w(px(ui::CONTROL_HEIGHT)))
                        .tooltip(self.tip("Export picks…", "⇧E")),
                    ),
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
        let picks = self.counts.picks;
        let unrated = self.counts.unrated;
        let rejects = self.counts.rejects;
        let hover = self.hover_bg();
        let row = |id: ElementId,
                   glyph: &'static str,
                   label: String,
                   count: Option<usize>,
                   active: bool| {
            let count = count.filter(|n| *n > 0 || label == "Unreviewed");
            div()
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
                        self.accent()
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
                        .child(label),
                )
                .when_some(count, |s, n| {
                    s.child(
                        div()
                            .text_size(px(11.))
                            .text_color(self.muted())
                            .child(n.to_string()),
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
                            "Add photos",
                            "add",
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
                        .tooltip(self.tip("Add a folder to the catalog", "⌘O")),
                    ),
            )
            .child(self.section("Catalog"));
        for (n, (f, g, count)) in [
            (Filter::All, "library", self.counts.all),
            (Filter::Picks, "pick", picks),
            (Filter::Unrated, "unflagged", unrated),
            (Filter::Rejects, "reject", rejects),
            (Filter::Trash, "trash", self.counts.trash),
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.filter == f
                && self.scope.is_none()
                && !self.duplicates_only
                && self.label_filter.is_none()
                && !self.bursts_only
                && (f != Filter::All || self.min_rating == 0);
            body = body.child(
                row(
                    ("collection", n).into(),
                    g,
                    collection_label(f).into(),
                    Some(count),
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scope = None;
                    this.duplicates_only = false;
                    this.label_filter = None;
                    this.bursts_only = false;
                    this.reference_focus = false;
                    this.filter = f;
                    if f == Filter::Trash {
                        this.min_rating = 0;
                        this.settings.min_rating = 0;
                    }
                    this.settings.filter = format!("{f:?}").to_lowercase();
                    this.save_settings();
                    this.rebuild(this.selected_id());
                    cx.notify();
                })),
            );
        }
        body = body.child(
            row(
                "collection-duplicates".into(),
                "duplicates",
                "Duplicates".into(),
                Some(self.duplicate_ids.len()),
                self.duplicates_only,
            )
            .tooltip(self.tip("Review exact duplicates; scan from the command palette", ""))
            .on_click(
                cx.listener(|this, _, window, cx| this.execute(Cmd::ShowDuplicates, window, cx)),
            ),
        );
        body = body.child(self.section("Selections"));
        let rated = self.counts.rated;
        body = body.child(
            row(
                "smart-rated".into(),
                "star",
                "3 stars and up".into(),
                Some(rated),
                self.filter == Filter::All
                    && self.min_rating == 3
                    && self.scope.is_none()
                    && self.label_filter.is_none()
                    && !self.bursts_only
                    && !self.duplicates_only,
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
            let count = self.counts.labels[label_index(label)];
            if count == 0 && self.label_filter != Some(label) {
                continue;
            }
            let active = self.label_filter == Some(label);
            body = body.child(
                div()
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
                                    .bg(rgb(label_rgb(label).unwrap_or(0x737b86))),
                            ),
                    )
                    .child(div().flex_1().child(format!("{label:?}")))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(self.muted())
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute(Cmd::LabelFilter(label), window, cx)
                    })),
            );
        }
        body = body.child(
            self.section("Folders").child(
                self.compact_button("sidebar-open", "", "add", Cmd::Import, false, cx)
                    .tooltip(self.tip("Open folder…", "⌘O")),
            ),
        );
        if self.folders.is_empty() {
            body = body.child(
                row(
                    "sidebar-open-empty".into(),
                    "folder-opened",
                    "Open folder…".into(),
                    None,
                    false,
                )
                .text_color(self.muted())
                .on_click(cx.listener(|this, _, window, cx| this.execute(Cmd::Import, window, cx))),
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
        body = body.child(
            self.section("Network").child(
                self.compact_button("network-add", "", "add", Cmd::AddRemoteHost, false, cx)
                    .tooltip(self.tip("Add SSH host…", "")),
            ),
        );
        for (host_index, (id, host)) in self.remote_hosts.iter().enumerate() {
            let color = match host.state.as_str() {
                "connected" => self.c("success", "#7fbf7f"),
                "connecting" | "indexing" => self.c("warning", "#e0b341"),
                "error" => self.c("error", "#d46a6a"),
                _ => self.muted(),
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
                            host.state,
                            host.config.ssh_alias,
                            host.error
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
                        host.state
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
                    if drive.path == self.import_ui.source && self.import_ui.scan.is_some() {
                        format!("{} · {} new", drive.name, self.import_ui.new_count)
                    } else {
                        drive.name.clone()
                    },
                    None,
                    false,
                )
                .tooltip(self.tip("Import from drive", ""))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_ingest(path.clone());
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
    fn review_extras_fit(&self) -> bool {
        self.center_size.0 >= ui::REVIEW_EXTRAS_MIN_WIDTH + 100.
    }
    fn filters(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let name = self
            .scope
            .as_ref()
            .map(|s| self.scope_display(s))
            .unwrap_or_else(|| {
                if self.duplicates_only {
                    "Duplicates".into()
                } else if self.bursts_only {
                    "Unresolved bursts".into()
                } else if let Some(label) = self.label_filter {
                    format!("{label:?} label")
                } else {
                    collection_label(self.filter).to_string()
                }
            });
        let mut crumb = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center()
            .gap(px(space::SM))
            .whitespace_nowrap()
            .child(
                div()
                    .id("breadcrumb-scope")
                    .max_w(px((self.center_size.0 * 0.35).min(200.)))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_normal()
                    .line_clamp(1)
                    .tooltip(self.tip(&name, ""))
                    .child(name),
            );
        if self.mode != ViewMode::Grid && self.review_folder == self.scope {
            if let Some(ids) = &self.review_subset {
                crumb = crumb.child(self.button(
                    "clear-review-subset",
                    &format!("{} selected · Show full folder", ids.len()),
                    "",
                    Cmd::ClearReviewSubset,
                    true,
                    cx,
                ));
            }
        }
        match (self.mode, self.selected_photo()) {
            (ViewMode::Grid, _) | (_, None) => {
                crumb = crumb.child(
                    div()
                        .text_color(self.muted())
                        .child(self.filtered.len().to_string()),
                );
            }
            (_, Some(photo)) => {
                crumb = crumb
                    .child(
                        svg()
                            .path("chevron")
                            .size(px(12.))
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
            .button(
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
            .when(!self.review_extras_fit() && self.multi.len() > 1, |toolbar| {
                toolbar.child(
                    div().id("compact-selection-count").flex_shrink_0()
                        .text_size(px(10.)).text_color(self.accent())
                        .child(format!("{} selected", self.multi.len()))
                        .tooltip(self.tip("Decisions apply to every selected photo, including those outside the viewport", "")),
                )
            })
            .when(self.duplicates_only, |row| {
                let running = self
                    .duplicate_scan
                    .as_ref()
                    .is_some_and(|s| matches!(s.phase.as_str(), "queued" | "running"));
                row.child(
                    self.button(
                        "duplicate-scan",
                        if running { "Stop scan" } else { "Scan" },
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
            .child(
                self.button(
                    "header-group",
                    if grouped && !self.compact {
                        grouping.as_str()
                    } else {
                        ""
                    },
                    "group",
                    Cmd::Group(if grouped {
                        "off".into()
                    } else {
                        "burst".into()
                    }),
                    grouped,
                    cx,
                )
                .tooltip(self.tip(
                    if grouped {
                        "Grouping on · click to turn off"
                    } else {
                        "Group bursts"
                    },
                    "",
                )),
            )
            .child(self.control_divider())
            .child(ratings)
            .child(
                self.button(
                    "toolbar-info",
                    "",
                    "panel-right",
                    Cmd::Inspector,
                    self.docks.inspector > 0.,
                    cx,
                )
                .tooltip(self.tip("Toggle photo panel", "I")),
            )
            .into_any_element()
    }
    fn other_overlay_open(&self) -> bool {
        self.deletion.open()
            || self.image_context.is_some()
            || self.palette
            || self.preferences
            || self.getting_started
            || self.import_ui.open
            || self.add_host.is_some()
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
                self.button(
                    ("filter-option", r as usize),
                    &if r == 0 {
                        "All ratings".into()
                    } else {
                        format!("{r} stars and up")
                    },
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
        let effect = if self.focus_peaking && kind != PreviewKind::Thumbnail {
            4
        } else {
            0
        };
        self.textures
            .contains_key(&(p.id, tier + effect, preview_mtime(p), preview_size(p)))
            || (tier == 1
                && self.textures.contains_key(&(
                    p.id,
                    2 + effect,
                    preview_mtime(p),
                    preview_size(p),
                )))
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
        let photo = self.photos[photo_index].clone();
        self.texture_photo(&photo, wanted, cx)
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
        let mut fallback = None;
        let peaking = self.focus_peaking && wanted != PreviewKind::Thumbnail;
        let effect = if peaking { 4 } else { 0 };
        let search_max = if desired == 1 { 2 } else { desired };
        for k in (0..=search_max).rev() {
            let key = (p.id, k + effect, preview_mtime(p), preview_size(p));
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
        // Keep the ordinary image visible while its peaking copy is prepared.
        if fallback.is_none() && peaking {
            for k in (0..=search_max).rev() {
                let key = (p.id, k, preview_mtime(p), preview_size(p));
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
        let k = match preview.kind {
            PreviewKind::Thumbnail => 0,
            PreviewKind::Fit => 1,
            PreviewKind::Full => 2,
        };
        let key = (p.id, k + effect, preview_mtime(p), preview_size(p));
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
        let width = preview.width;
        let height = preview.height;
        let bytes = preview.rgba.len();
        let kind = preview.kind;
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
            let mut bgra = preview.rgba.clone();
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            if peaking {
                shadowroom::peaking::highlight(&preview.rgba, &mut bgra, width, height);
            }
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
        let active = self.mode == ViewMode::Compare && self.reference_focus == reference;
        let show_quality = self.zoom
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
            .border_1()
            .border_color(if active {
                self.accent()
            } else {
                transparent_black()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reference_focus = reference;
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
            } else {
                format!("Fit · {w} × {h}{origin}")
            };
            quality.push_str(match color_status {
                ColorStatus::ConvertedToSrgb => " · converted to sRGB",
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
            pane = pane.child(
                gpui::canvas(
                    move |bounds, window, _| {
                        measured.set(bounds.size);
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
                    .cursor(CursorStyle::ClosedHand)
                    .on_scroll_wheel(cx.listener(
                        move |this, event: &ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(px(24.));
                            if event.modifiers.control || event.modifiers.platform {
                                if f32::from(delta.y).abs() > 0. {
                                    this.execute(
                                        Cmd::ZoomStep(if delta.y > px(0.) { 1 } else { -1 }),
                                        window,
                                        cx,
                                    );
                                }
                                return;
                            }
                            let size = scroll_size.get();
                            this.pan_photo(reference, delta, size, w, h, scale);
                            cx.notify();
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, _| {
                            this.drag_origin = Some(event.position);
                        }),
                    );
                let drag_size = pane_size.clone();
                pane =
                    pane.on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        if event.dragging() && this.dock_drag.is_none() {
                            if let Some(previous) = this.drag_origin.replace(event.position) {
                                let delta = event.position - previous;
                                this.pan_photo(reference, delta, drag_size.get(), w, h, scale);
                                cx.notify();
                            }
                        } else {
                            this.drag_origin = None;
                        }
                    }));
            } else {
                pane = pane.on_scroll_wheel(cx.listener(
                    |this, event: &ScrollWheelEvent, window, cx| {
                        if event.modifiers.control || event.modifiers.platform {
                            let delta = event.delta.pixel_delta(px(24.));
                            if delta.y != px(0.) {
                                this.execute(
                                    Cmd::ZoomStep(if delta.y > px(0.) { 1 } else { -1 }),
                                    window,
                                    cx,
                                );
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
        let pane = pane.when(compare, |pane| {
            pane.child(
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(8.))
                    .max_w(px(360.))
                    .px(px(space::SM))
                    .py(px(space::TINY))
                    .flex()
                    .gap(px(space::SM))
                    .bg(rgba(0x0f0f0fd0))
                    .text_size(px(11.))
                    .text_color(rgb(0xe6e6e6))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(div().text_color(rgb(0x9a9a9a)).child(if reference {
                        "Reference"
                    } else {
                        "Candidate"
                    }))
                    .child(photo.filename.clone()),
            )
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
                        .text_size(px(10.))
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
    /// Columns that keep grid cells near 240 logical px wide.
    fn grid_cols(&self) -> usize {
        ((self.center_size.0 - scrollbar::WIDTH - 2. * ui::PANEL_INSET + ui::GRID_GAP)
            / (ui::GRID_CELL_MIN_WIDTH + ui::GRID_GAP))
            .floor()
            .clamp(1., 64.) as usize
    }
    fn grid(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let cols = self.grid_cols();
        let rows = self.filtered.len().div_ceil(cols);
        let handle = self.grid_scroll.clone();
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
                            .h(px(ui::GRID_ROW_HEIGHT))
                            .w_full()
                            .min_w(px(0.))
                            .flex()
                            .gap(px(ui::GRID_GAP))
                            .px(px(ui::PANEL_INSET))
                            .pt(px(space::LG));
                        for col in 0..cols {
                            let position = row * cols + col;
                            if let Some(&index) = this.filtered.get(position) {
                                let photo = &this.photos[index];
                                this.request_preview(photo, PreviewKind::Thumbnail);
                                let selected =
                                    position == this.selected || this.multi.contains(&photo.id);
                                let texture = this.texture(index, PreviewKind::Thumbnail, cx);
                                let photo = &this.photos[index];
                                let photo_id = photo.id;
                                let mut image = div()
                                    .h(px(ui::GRID_IMAGE_HEIGHT))
                                    .w_full()
                                    .relative()
                                    .bg(this.canvas_bg())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .overflow_hidden();
                                if let Some((texture, _, _, _, _, _)) = texture {
                                    image = image.child(
                                        img(texture)
                                            .absolute()
                                            .inset_0()
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    );
                                } else {
                                    image = image.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .gap(px(space::SM))
                                            .child(icon("camera", this.icon_muted()))
                                            .when_some(
                                                this.media
                                                    .photo_error(photo, PreviewKind::Thumbnail),
                                                |s, error| {
                                                    s.child(
                                                        div()
                                                            .text_size(px(10.))
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
                                            ),
                                    );
                                }
                                if photo.flag != Flag::None {
                                    image = image.child(this.flag_badge(photo.flag));
                                }
                                if this.multi.contains(&photo.id) {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .top(px(4.))
                                            .right(px(4.))
                                            .size(px(18.))
                                            .bg(rgba(0x0f0f0fd0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icon("selected", this.accent())),
                                    );
                                }
                                let burst = this.burst_slot(photo.id);
                                if let Some((_, frame, count)) = burst {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .bottom(px(7.))
                                            .right(px(4.))
                                            .px(px(space::XS))
                                            .h(px(18.))
                                            .flex()
                                            .items_center()
                                            .gap(px(space::XS))
                                            .bg(rgba(0x0f0f0fd0))
                                            .text_color(rgb(0xe6e6e6))
                                            .text_size(px(10.))
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
                                                    .text_color(rgb(0xb8b8b8)),
                                            )
                                            .child(format!("{}/{count}", frame + 1)),
                                    );
                                }
                                if let Some(bar) = this.label_bar(photo.label) {
                                    image = image.child(bar.h(px(4.)));
                                }
                                // Adjacent bursts alternate intensity so boundaries read at a glance.
                                let burst_bar = burst.map(|(group, ..)| {
                                    this.accent()
                                        .opacity(if group % 2 == 0 { 0.6 } else { 0.3 })
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
                                                .text_size(px(11.))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_normal()
                                                        .line_clamp(1)
                                                        .text_color(if selected {
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
                                                        .child("★".repeat(photo.rating as usize)),
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
                                line = line.child(div().flex_1());
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
    fn survey_summary(&mut self, id: i64) -> Option<Arc<SurveySummary>> {
        let key = (
            self.duplicates_only,
            if self.duplicates_only {
                *self.duplicate_lookup.get(&id)?
            } else {
                *self.group_lookup.get(&id)?
            },
        );
        if self
            .survey_cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != key)
        {
            let group = self.review_group(id).filter(|g| g.len() > 1)?;
            let positions = group
                .iter()
                .filter_map(|&id| self.visible_position(id))
                .collect();
            self.survey_cache = Some((
                key,
                Arc::new(SurveySummary::new(positions, &self.filtered, &self.photos)),
            ));
        }
        self.survey_cache
            .as_ref()
            .map(|(_, summary)| summary.clone())
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
                .child(if grouped {
                    "This photo isn't part of a burst."
                } else {
                    "Burst grouping is off."
                })
                .when(!grouped, |s| {
                    s.child(self.button(
                        "survey-enable-bursts",
                        "Group bursts",
                        "group",
                        Cmd::Group("burst".into()),
                        false,
                        cx,
                    ))
                })
                .into_any_element();
        }
        // Frames in view order; a bounded window keeps memory and disk reads flat
        // even for very long bursts.
        let summary = summary.expect("survey group");
        let all = &summary.positions;
        let at = all.binary_search(&self.selected).unwrap_or(0);
        const MAX: usize = 16;
        let first = at
            .saturating_sub(MAX / 2)
            .min(all.len().saturating_sub(MAX));
        let frames = &all[first..(first + MAX).min(all.len())];
        let n = frames.len();
        let cols = match n {
            0..=2 => n.max(1),
            3..=4 => 2,
            5..=9 => 3,
            _ => 4,
        };
        let span = &summary.span;
        let rows = n.div_ceil(cols);
        let cell_w = ((self.center_size.0 - 2. * ui::PANEL_INSET - space::SM * (cols - 1) as f32)
            / cols as f32)
            .max(1.);
        let cell_h = ((self.center_size.1
            - 2. * ui::PANEL_INSET
            - ui::SURVEY_HEADER_HEIGHT
            - space::SM * (rows - 1) as f32)
            / rows as f32)
            .max(1.);
        let mut grid = div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap(px(space::SM));
        let mut row = div().flex_1().min_h(px(0.)).flex().gap(px(space::SM));
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
                        .when(p.flag == Flag::Reject, |s| s.opacity(0.4)),
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
                            s.child(div().text_size(px(10.)).child(
                                if error.message.contains("Unavailable offline") {
                                    "Unavailable offline"
                                } else {
                                    "Preview unavailable"
                                },
                            ))
                        }),
                );
            }
            if p.flag != Flag::None {
                photo_box = photo_box.child(self.flag_badge(p.flag));
            }
            photo_box = photo_box.child(
                div()
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .px(px(space::XS))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .bg(rgba(0x0f0f0fd0))
                    .text_size(px(11.))
                    .text_color(if active { rgb(0xffffff) } else { rgb(0xb8b8b8) })
                    .child((first + i + 1).to_string()),
            );
            if p.rating > 0 {
                photo_box = photo_box.child(
                    div()
                        .absolute()
                        .bottom(px(7.))
                        .right(px(4.))
                        .px(px(space::XS))
                        .bg(rgba(0x0f0f0fd0))
                        .text_size(px(11.))
                        .text_color(self.rating_color())
                        .child("★".repeat(p.rating as usize)),
                );
            }
            if let Some(bar) = self.label_bar(p.label) {
                photo_box = photo_box.child(bar.h(px(4.)));
            }
            let cell = div()
                .id(("survey", position))
                .flex_1()
                .min_w(px(0.))
                .min_h(px(0.))
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
                    row = row.child(div().flex_1().min_w(px(0.)));
                }
                grid = grid.child(row);
                row = div().flex_1().min_h(px(0.)).flex().gap(px(space::SM));
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
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child(div().text_color(self.text()).child(format!(
                        "Frame {} of {}",
                        at + 1,
                        all.len()
                    )))
                    .child(format!(
                        "{}{span}",
                        plural(decided, "frame decided", "frames decided")
                    ))
                    .when(all.len() > n, |s| {
                        s.child(format!("showing {}–{}", first + 1, first + n))
                    })
                    .child(div().flex_1())
                    .when(!self.compact, |s| {
                        s.child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_normal()
                                .line_clamp(1)
                                .child(format!(
                                    "{} keeps this frame and rejects undecided ones",
                                    shortcut("⇧P")
                                )),
                        )
                    }),
            )
            .child(grid)
            .into_any_element()
    }
    fn flag_badge(&self, flag: Flag) -> Div {
        div()
            .absolute()
            .top(px(4.))
            .left(px(4.))
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x0f0f0fd0))
            .child(
                svg()
                    .path(if flag == Flag::Pick {
                        "pick-badge"
                    } else {
                        "reject-badge"
                    })
                    .size(px(12.))
                    .text_color(if flag == Flag::Pick {
                        self.pick_color()
                    } else {
                        self.reject_color()
                    }),
            )
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

    fn label_bar(&self, label: ColorLabel) -> Option<Div> {
        label_rgb(label).map(|color| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(3.))
                .bg(rgb(color))
        })
    }
    fn group_count_badge(&self, count: usize) -> Div {
        div()
            .absolute()
            .bottom(px(5.))
            .left(px(3.))
            .px(px(space::XS))
            .h(px(14.))
            .flex()
            .items_center()
            .gap(px(space::TINY))
            .bg(rgba(0x0f0f0fd0))
            .text_color(rgb(0xe6e6e6))
            .text_size(px(9.))
            .child(
                svg()
                    .path(if self.duplicates_only {
                        "duplicates"
                    } else {
                        "group-badge"
                    })
                    .size(px(10.))
                    .text_color(rgb(0xb8b8b8)),
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
            .h(px(FILMSTRIP_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(ui::FILM_GAP))
            .px(px(ui::PANEL_INSET))
            .bg(self.panel())
            .border_t_1()
            .border_color(self.border())
            .overflow_hidden();
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
            // Preserve the full photograph within an equal-sized slot.
            let mut image = div()
                .relative()
                .w(px(ui::FILM_WIDTH))
                .h(px(ui::FILM_HEIGHT))
                .bg(self.canvas_bg())
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center();
            if let Some((texture, _, _, _, _, _)) = texture {
                image = image.child(img(texture).size_full().object_fit(ObjectFit::Contain));
            } else {
                image = image.child(icon("camera", self.icon_muted()));
            }
            if p.flag != Flag::None {
                image = image.child(self.flag_badge(p.flag));
            }
            if p.rating > 0 {
                image = image.child(
                    div()
                        .absolute()
                        .bottom(px(5.))
                        .right(px(3.))
                        .px(px(space::XS))
                        .bg(rgba(0x0f0f0fd0))
                        .text_size(px(9.))
                        .text_color(self.rating_color())
                        .child("★".repeat(p.rating as usize)),
                );
            }
            let burst = self.burst_slot(p.id);
            let run_start =
                burst.is_some_and(|(group, ..)| run.as_ref().is_none_or(|(g, _)| *g != group));
            if let (true, Some((_, _, count))) = (run_start, burst) {
                image = image.child(self.group_count_badge(count));
            }
            if let Some(bar) = self.label_bar(p.label) {
                image = image.child(bar);
            }
            let photo_id = p.id;
            let cell = div()
                .id(("film", position))
                .flex_shrink_0()
                .p(px(1.))
                .border_2()
                .border_color(if active {
                    self.accent()
                } else if multi {
                    self.accent().opacity(0.45)
                } else {
                    transparent_black()
                })
                .cursor_pointer()
                .child(image)
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
                                    .gap(px(space::TINY))
                                    .px(px(space::TINY))
                                    .pb(px(1.))
                                    .bg(self.hover_bg())
                                    .border_t_2()
                                    .border_color(self.accent().opacity(0.55))
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
        let compact = self.center_size.0 < 760.;
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
                .child(div().min_w(px(0.)).overflow_hidden().text_ellipsis().whitespace_normal().line_clamp(1).text_size(px(11.)).text_color(self.muted()).child(
                    "Originals stay in their source folders. Ratings and flags are preserved.",
                ))
                .into_any_element();
        }
        let p = self.review_photo();
        let rating = p.map_or(0, |p| p.rating);
        let flag = p.map_or(Flag::None, |p| p.flag);
        let hover = self.hover_bg();
        let flag_button = |id: &'static str,
                           label: &'static str,
                           key: &'static str,
                           glyph: &'static str,
                           target: Flag,
                           color: Hsla| {
            let on = flag == target;
            div()
                .id(id)
                .h(px(ui::CONTROL_HEIGHT))
                .px(px(space::SM))
                .when(compact, |s| s.w(px(ui::COMPACT_CONTROL_HEIGHT)).px(px(0.)))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .gap(px(space::SM))
                .cursor_pointer()
                .when(on, |s| s.bg(color.opacity(0.15)))
                .when(!on, |s| s.hover(move |s| s.bg(hover)))
                .child(icon(
                    if on {
                        if target == Flag::Pick {
                            "pick-on"
                        } else {
                            "reject-on"
                        }
                    } else {
                        glyph
                    },
                    if on { color } else { self.icon_muted() },
                ))
                .when(!compact, |s| {
                    s.child(
                        div()
                            .text_color(if on { color } else { self.text() })
                            .child(label),
                    )
                    .child(self.keycap(key))
                })
                .tooltip(self.tip(label, key))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.execute(Cmd::Flag(target), window, cx)
                }))
        };
        let mut stars = div().flex().flex_shrink_0().items_center();
        for r in 1..=5 {
            stars = stars.child(
                self.star(("rate", r as usize), r <= rating, Cmd::Rating(r), cx)
                    .when(compact, |s| s.w(px(20.)))
                    .tooltip(self.tip(&format!("Rate {r}"), &r.to_string())),
            );
        }
        let pinned = self.reference == self.selected_id() && self.reference.is_some();
        let reviewed = self.filtered_reviewed;
        div()
            .h(px(DECISION_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .p(px(ui::BAR_INSET))
            .when(compact, |s| s.gap(px(space::TINY)).p(px(space::XS)))
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
            .child(
                self.control_divider()
                    .when(compact, |s| s.w(px(0.)).mx(px(0.))),
            )
            .child(stars)
            .child(
                self.button(
                    "reset-decision",
                    if compact { "" } else { "Reset" },
                    "unflagged",
                    Cmd::ResetDecision,
                    false,
                    cx,
                )
                .tooltip(self.tip(
                    "Reset rating and pick/reject · keep color label · stay on this photo",
                    "R",
                )),
            )
            .child(div().flex_1())
            .when(self.review_extras_fit(), |bar| {
                bar.child(
                    div()
                        .id("review-progress")
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .px(px(space::SM))
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .when(!self.compact || self.multi.len() > 1, |s| {
                            s.child(if self.multi.len() > 1 {
                                format!("{} selected", self.multi.len())
                            } else {
                                format!(
                                    "{} of {}",
                                    if self.filtered.is_empty() {
                                        0
                                    } else {
                                        self.selected + 1
                                    },
                                    self.filtered.len()
                                )
                            })
                        })
                        .child(
                            div().w(px(64.)).h(px(3.)).bg(self.selected_bg()).child(
                                div()
                                    .h_full()
                                    .w(
                                        px(64. * reviewed as f32
                                            / self.filtered.len().max(1) as f32),
                                    )
                                    .bg(self.pick_color()),
                            ),
                        )
                        .tooltip(self.tip(
                            &format!(
                                "{reviewed} of {} reviewed ({:.0}%)",
                                self.filtered.len(),
                                100. * reviewed as f32 / self.filtered.len().max(1) as f32
                            ),
                            "",
                        )),
                )
                .child(
                    self.button(
                        "header-advance",
                        "",
                        "auto-advance",
                        Cmd::AutoAdvance,
                        self.settings.auto_advance,
                        cx,
                    )
                    .tooltip(self.tip(
                        if self.settings.auto_advance {
                            "Auto-advance on"
                        } else {
                            "Auto-advance off"
                        },
                        "Shift A",
                    )),
                )
                .child(
                    self.button("pin-reference", "", "pin", Cmd::Pin, pinned, cx)
                        .tooltip(self.tip("Pin as compare reference", "B")),
                )
                .child(
                    self.button(
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
                    .tooltip(self.tip("Survey burst · every frame side by side", "S")),
                )
            })
            .child(
                self.button(
                    "zoom-photo",
                    &if self.zoom {
                        format!("{:.0}%", self.zoom_factor * 100.)
                    } else {
                        "Fit".into()
                    },
                    if compact { "" } else { "zoom" },
                    Cmd::Zoom,
                    self.zoom,
                    cx,
                )
                .when(compact, |s| s.w(px(48.)).px(px(space::XS)))
                .tooltip(self.tip(
                    if self.zoom {
                        "Fit to window"
                    } else {
                        "View at 100%"
                    },
                    "Z",
                )),
            )
            .child(
                self.button("zoom-out", "−", "", Cmd::ZoomStep(-1), false, cx)
                    .when(compact, |s| s.w(px(22.)).px(px(0.)))
                    .tooltip(self.tip("Zoom out", "−")),
            )
            .child(
                self.button("zoom-in", "+", "", Cmd::ZoomStep(1), false, cx)
                    .when(compact, |s| s.w(px(22.)).px(px(0.)))
                    .tooltip(self.tip("Zoom in", "+")),
            )
            .child(
                self.button(
                    "focus-peaking",
                    "",
                    "focus",
                    Cmd::FocusPeaking,
                    self.focus_peaking,
                    cx,
                )
                .when(compact, |s| s.w(px(ui::COMPACT_CONTROL_HEIGHT)))
                .tooltip(self.tip("Focus peaking · cyan fine-detail highlights", "F")),
            )
            // Destructive action sits apart from rating, at the far edge.
            .child(
                self.control_divider()
                    .when(compact, |s| s.w(px(0.)).mx(px(0.))),
            )
            .child(
                self.button("trash-selection", "", "trash", Cmd::Trash(true), false, cx)
                    .when(compact, |s| s.w(px(ui::COMPACT_CONTROL_HEIGHT)))
                    .tooltip(self.tip("Move to Trash…", "Delete")),
            )
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
            .h(px(ui::INSPECTOR_SECTION_HEIGHT))
            .flex_shrink_0()
            .mt(px(space::LG))
            .pt(px(space::LG))
            .pb(px(space::SM))
            .border_t_1()
            .border_color(self.border())
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(11.))
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
        if let Some(p) = self.review_photo() {
            let mut facts = Vec::new();
            if p.width > 0 && p.height > 0 {
                facts.push(format!("{} × {}", p.width, p.height));
            }
            facts.push(format!("{:.1} MB", p.size_bytes as f64 / 1_000_000.));
            if p.companion_path.is_some() {
                facts.push(
                    if p.companion_path
                        .as_deref()
                        .is_some_and(shadowroom::heif::is_heif)
                    {
                        "RAW + HEIF"
                    } else {
                        "RAW + JPEG"
                    }
                    .into(),
                );
            }
            // The exposure triangle is what distinguishes frames in a burst, so
            // it leads, in one scannable line.
            let shutter = p.shutter.trim();
            let exposure: Vec<String> = [
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
            .collect();
            let captured = p.capture_display();
            body = body
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(13.))
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
                            .pt(px(space::SM))
                            .flex()
                            .flex_wrap()
                            .gap(px(space::XS))
                            .children(exposure.into_iter().map(|fact| {
                                div()
                                    .px(px(space::XS))
                                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .bg(self.selected_bg())
                                    .text_size(px(12.))
                                    .child(fact)
                            })),
                    )
                })
                .child(
                    div()
                        .pt(px(space::SM))
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(facts.join(" · ")),
                );
            if self.inspector_info && !captured.is_empty() && captured != "—" {
                body = body
                    .child(self.inspector_section("File information"))
                    .child(self.metadata_row("Captured", captured));
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
                        self.button(
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
                    .border_color(self.border())
                    .child(
                        self.segment(
                            "histogram-lin",
                            "LIN",
                            Cmd::HistogramLog,
                            !self.histogram_log,
                            cx,
                        )
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .px(px(space::SM))
                        .text_size(px(10.)),
                    )
                    .child(
                        self.segment(
                            "histogram-log",
                            "LOG",
                            Cmd::HistogramLog,
                            self.histogram_log,
                            cx,
                        )
                        .h(px(ui::COMPACT_CONTROL_HEIGHT))
                        .px(px(space::SM))
                        .text_size(px(10.)),
                    )
                    .tooltip(self.tip("Histogram scale · log lifts shadows and highlights", ""));
                body = body
                    .child(self.inspector_section("Histogram").child(scale))
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
                            ..Default::default()
                        },
                    ));
                } else {
                    body = body.child(div().h(px(122.)).bg(self.canvas_bg()));
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
                    let color = label_rgb(label);
                    labels = labels.child(
                        div()
                            .id(("label", n))
                            .size(px(ui::COMPACT_CONTROL_HEIGHT))
                            .p(px(space::XS))
                            .border_1()
                            .border_color(if p.label == label {
                                self.text()
                            } else {
                                transparent_black()
                            })
                            .cursor_pointer()
                            .child(
                                div()
                                    .size_full()
                                    .when_some(color, |s, c| s.bg(rgb(c)))
                                    .when(color.is_none(), |s| {
                                        s.border_1().border_color(self.muted())
                                    }),
                            )
                            .tooltip(self.tip(
                                &if color.is_none() {
                                    "No label".to_string()
                                } else {
                                    format!("{label:?}")
                                },
                                label_key(label),
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.execute(Cmd::Label(label), window, cx)
                            })),
                    );
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
                body = body
                    .child(self.inspector_section("Selection"))
                    .child(
                        self.metadata_row(
                            "Flag",
                            match p.flag {
                                Flag::Pick => "Pick",
                                Flag::Reject => "Reject",
                                Flag::None => "Unreviewed",
                            }
                            .into(),
                        ),
                    )
                    .child(self.metadata_row(
                        "Rating",
                        if p.rating == 0 {
                            "Unrated".into()
                        } else {
                            format!("{} / 5", p.rating)
                        },
                    ));
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
                                .text_size(px(11.))
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
            body = body.child(div().text_color(self.muted()).child("No photo selected"));
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
                    .items_center()
                    .justify_start()
                    .gap(px(ui::BAR_INSET))
                    .p(px(ui::BAR_INSET))
                    .border_b_1()
                    .border_color(self.border())
                    .child(
                        self.button(
                            "panel-review",
                            "Review",
                            "pick",
                            Cmd::InspectorTab(false),
                            !self.inspector_info,
                            cx,
                        )
                        .flex_1(),
                    )
                    .child(
                        self.button(
                            "panel-info",
                            "Info",
                            "camera",
                            Cmd::InspectorTab(true),
                            self.inspector_info,
                            cx,
                        )
                        .flex_1(),
                    ),
            )
            .child(body)
            .into_any_element()
    }
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
        json!({"state":if self.unsaved.is_empty(){"durable"}else if self.awaiting.is_empty(){"failed"}else{"pending"},
            "pending_batches":self.awaiting.len(),"queued_batches":self.outbox.len(),"unsaved_ids":self.unsaved.keys().copied().collect::<Vec<_>>(),"revision":self.save_revisions.revision()})
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
            "closing":self.quitting,"settings_writable":self.settings_writable,"settings_persistence":{"state":if self.settings_failure.is_some(){"failed"}else if self.settings_dirty{"pending"}else{"durable"},"error":self.settings_failure}});
        state["inspector_tab"] = json!(if self.inspector_info {
            "info"
        } else {
            "review"
        });
        state["overlays"] = json!({"rating_filter":self.rating_filter_open && !self.other_overlay_open(),"palette":self.palette,"preferences":self.preferences});
        state["overlays"]["getting_started"] = json!(self.getting_started);
        state["deletion"] = self.deletion.snapshot();
        state["zoom_percent"] = if self.zoom {
            json!(self.zoom_factor * 100.)
        } else {
            Value::Null
        };
        state["focus_peaking"] = json!(self.focus_peaking);
        state["image_context"] = self.image_context_state();
        state["catalog_hover"] = json!(self.catalog_hover);
        state["fit_zoom_percent"] = json!(self.fit_scales.get()[1] * 100.);
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
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Decision was not admitted".into()));
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
        if self.quitting
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
                return Ok(json!({"host":id,"state":host.state,"roots":roots}));
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
                            let effect = if self.focus_peaking { 4 } else { 0 };
                            self.textures.get(&(p.id, tier + effect, preview_mtime(p), preview_size(p)))
                                .map(|texture| (texture, effect != 0))
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
                let ids = if p.get("ids").is_some() {
                    self.api_ids(p)?
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
        let start = self.palette_index.saturating_sub(6);
        let end = (start + 12).min(total);
        let mut results = div().flex().flex_col().py(px(space::XS));
        for (n, &index) in cache
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start)
        {
            let item = &cache.items[index];
            let active = n == self.palette_index;
            let hover = self.hover_bg();
            let cmd = item.command.clone();
            let title = item.title.clone();
            let recent = self.palette_query.is_empty() && self.palette_recent.contains(&item.title);
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
                    .cursor_pointer()
                    .when(active, |s| s.bg(self.selected_bg()))
                    .when(!active, |s| s.hover(move |s| s.bg(hover)))
                    .child(icon(
                        item.icon,
                        if active {
                            self.accent()
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
                            .child(item.title.clone())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(self.muted())
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(if recent { self.accent() } else { self.muted() })
                            .child(if recent {
                                "Recent".to_string()
                            } else {
                                category
                            }),
                    )
                    .when(!item.shortcut.is_empty(), |s| {
                        s.child(self.keycaps(&item.shortcut))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remember_command(title.clone());
                        this.palette = false;
                        this.execute(cmd.clone(), window, cx);
                    })),
            )
        }
        if total == 0 {
            results = results.child(
                div()
                    .px(px(ui::PANEL_INSET))
                    .py(px(space::MD))
                    .text_color(self.muted())
                    .child("No matching commands"),
            );
        }
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000040))
            .occlude()
            .flex()
            .justify_center()
            .pt(px(72.))
            .child(
                div()
                    .id("command-palette")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Left {
                            cx.stop_propagation();
                            this.execute(Cmd::Close, window, cx);
                        }
                    }))
                    .w(px(560.))
                    .h_auto()
                    .max_h(px(440.))
                    .flex()
                    .flex_col()
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .border_1()
                    .border_color(self.c("border.variant", "#333333"))
                    .shadow(vec![BoxShadow {
                        color: hsla(0., 0., 0., 0.35),
                        offset: point(px(0.), px(6.)),
                        blur_radius: px(18.),
                        spread_radius: px(0.),
                    }])
                    .overflow_hidden()
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
                            .text_size(px(13.))
                            .child(icon("search", self.icon_muted()))
                            .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(
                                self.input_text(&self.palette_query, "Execute a command…", true),
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
                            .text_size(px(11.))
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
            .border_color(self.border());
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
    fn preferences_view(&self, window: &Window, cx: &gpui::Context<Self>) -> AnyElement {
        let s = self.preference_settings();
        let fonts = self.choices(
            "font",
            INTERFACE_FONTS
                .iter()
                .copied()
                .map(|f| (f.to_string(), Cmd::Font(f.into()), s.interface_font == f)),
            cx,
        );
        let groups = self.choices(
            "group-mode",
            [
                ("off", "Off"),
                ("time", "Time"),
                ("visual", "Visual"),
                ("burst", "Burst"),
            ]
            .map(|(k, l)| (l.to_string(), Cmd::Group(k.into()), s.grouping_mode == k)),
            cx,
        );
        let gaps = self.choices(
            "gap",
            [500u64, 1000, 2000, 5000].map(|g| {
                (
                    format!("{}s", g as f32 / 1000.),
                    Cmd::Gap(g),
                    s.burst_gap_ms == g,
                )
            }),
            cx,
        );
        let thresholds = self.choices(
            "threshold",
            [(4, "Strict"), (8, "Balanced"), (14, "Loose")]
                .map(|(n, l)| (l.to_string(), Cmd::Threshold(n), s.visual_threshold == n)),
            cx,
        );
        let advance = self.choices(
            "pref-advance",
            [
                (
                    "Off".to_string(),
                    Cmd::SetAutoAdvance(false),
                    !s.auto_advance,
                ),
                ("On".to_string(), Cmd::SetAutoAdvance(true), s.auto_advance),
            ],
            cx,
        );
        let drives = self.choices(
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
            cx,
        );
        let memory = self.choices(
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
            cx,
        );
        let disk = self.choices(
            "disk",
            [2048, 8192, 32768].map(|mb| {
                (
                    format!("{} GB", mb / 1024),
                    Cmd::Disk(mb),
                    s.cache_budget_mb == mb,
                )
            }),
            cx,
        );
        let mode = s
            .extra
            .get("theme_mode")
            .and_then(Value::as_str)
            .unwrap_or("manual");
        let theme_mode = self.choices(
            "theme-mode",
            [
                ("manual", "Manual"),
                ("system", "System"),
                ("zed", "Follow Zed"),
            ]
            .map(|(k, l)| (l.to_string(), Cmd::ThemeMode(k.into()), mode == k)),
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
        for (i, theme) in sorted_themes {
            let light = theme.appearance == "light";
            if last_appearance != Some(light) {
                theme_list = theme_list.child(
                    div()
                        .flex_shrink_0()
                        .px(px(space::MD))
                        .pt(px(space::SM))
                        .pb(px(space::XS))
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(self.muted())
                        .child(if light { "LIGHT" } else { "DARK" }),
                );
                last_appearance = Some(light);
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
            theme_list = theme_list.child(
                div()
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
                    })),
            );
        }
        let stats = self.media.stats();
        let heading = |label: &str| {
            div()
                .pt(px(space::LG))
                .pb(px(space::SM))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(self.text())
                .child(label.to_owned())
        };
        let body = div()
            .id("preferences-body")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(ui::DIALOG_INSET))
            .pb(px(ui::DIALOG_INSET))
            .when(self.preferences_section == PreferencesSection::Import, |body| body.child(self.ingest_preferences(cx)))
            .when(self.preferences_section == PreferencesSection::Culling, |body| body
            .child(heading("Culling"))
            .child(self.preference_row("Auto-advance", "Move to the next photo after a decision", advance))
            .child(self.preference_row("Trash destination", "OS Trash moves files; in-app Trash keeps originals in place. Always confirm before deleting.", self.choices("trash-mode", [("OS Trash".into(), Cmd::TrashMode("system".into()), s.trash_mode == "system"), ("In-app".into(), Cmd::TrashMode("in_app".into()), s.trash_mode == "in_app")], cx)))
            .child(self.preference_row("Grouping", "Keep bursts and similar frames together", groups))
            .child(self.preference_row("Burst gap", "Maximum time between frames in a burst", gaps))
            .child(self.preference_row("Similarity", "How alike frames must look to group", thresholds))
            .child(self.preference_row("Drives", "Discover mounted cards and photo drives", drives)))
            .when(self.preferences_section == PreferencesSection::Appearance, |body| body
            .child(heading("Appearance"))
            .child(self.preference_row("Font", "Interface typeface", fonts))
            .child(self.preference_row("Theme source", "Where the active palette comes from", theme_mode))
            .child(div().pt(px(space::MD)).flex().items_center().gap(px(space::SM)).child(div().text_color(self.muted()).child("Selected theme")).child(s.theme.clone()))
            .child(div().pt(px(space::XS)).text_size(px(11.)).text_color(self.muted()).child(source_description))
            .child(
                div()
                    .pt(px(space::MD))
                    .pb(px(space::XS))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(format!("Available themes · {}", self.preference_themes().len()))
                    .child(
                        self.button("import-theme", "Import Zed theme…", "folder-opened", Cmd::ImportTheme, false, cx),
                    ),
            )
            .child(theme_list)
            .child(div().pt(px(space::SM)).text_size(px(11.)).text_color(self.muted()).child(match mode {
                "system" => "Choose a light or dark theme to update its system appearance slot.",
                "zed" => "Installed Zed themes appear automatically. Choosing a theme switches to Manual.",
                _ => "Installed Zed themes appear automatically. Theme files reload when changed.",
            })))
            .when(self.preferences_section == PreferencesSection::Performance, |body| body
            .child(heading("Performance"))
            .child(self.preference_row("Preview memory", "Auto adapts to this computer; shared by decoded images and display copies", memory))
            .child(self.preference_row("Disk cache", "Reuses thumbnails and previews across launches; originals stay read-only", disk))
            .child(div().pt(px(space::SM)).text_color(self.muted()).child("The next and previous frames are read ahead for comparison, including while zoomed. Inspection prefers a paired JPEG or embedded camera preview."))
            .child(
                div()
                    .pt(px(space::MD))
                    .text_color(self.muted())
                    .text_size(px(11.))
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
            .when(self.preferences_section == PreferencesSection::Help, |body| body
                .child(heading("Getting Started"))
                .child(self.preference_row("Startup guide", "Workflow, shortcuts, time groups, labels and caching", self.choices("guide-startup-pref", [("Show".into(), Cmd::StartupGuide(true), s.show_getting_started), ("Hide".into(), Cmd::StartupGuide(false), !s.show_getting_started)], cx)))
                .child(self.button("open-guide", "Open Getting Started", "files", Cmd::GettingStarted, false, cx)))
            .when(self.preferences_section == PreferencesSection::Controller, |body| body
            .child(heading("Creator Micro"))
            .child(
                div()
                    .text_color(self.muted())
                    .text_size(px(11.))
                    .child("Arrows navigate · F13–F15 flag · F16/F17 rating · F18–F20 views · F21 zoom · F22 histogram · F23/F24 bursts. Profiles: docs/CONTROLS.md"),
            ));
        let mut sections = div()
            .flex()
            .flex_shrink_0()
            .gap(px(ui::BAR_INSET))
            .p(px(ui::BAR_INSET))
            .border_b_1()
            .border_color(self.border());
        for (section, label) in [
            (PreferencesSection::Appearance, "Appearance"),
            (PreferencesSection::Culling, "Culling"),
            (PreferencesSection::Import, "Import"),
            (PreferencesSection::Performance, "Performance"),
            (PreferencesSection::Controller, "Controller"),
            (PreferencesSection::Help, "Help"),
        ] {
            sections = sections.child(
                self.segment(
                    SharedString::from(format!("preferences-{section:?}")),
                    label,
                    Cmd::PreferencesSection(section),
                    self.preferences_section == section,
                    cx,
                )
                .px(px(space::LG)),
            );
        }
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000060))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(space::XXL))
            .child(
                div()
                    .id("preferences")
                    .occlude()
                    .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Left {
                            cx.stop_propagation();
                            this.execute(Cmd::Close, window, cx);
                        }
                    }))
                    .w(px(680.))
                    .max_h(px(660.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .border_1()
                    .border_color(self.c("border.variant", "#333333"))
                    .child(
                        div()
                            .h(px(TOOLBAR_HEIGHT))
                            .flex_shrink_0()
                            .pl(px(ui::DIALOG_INSET))
                            .pr(px(ui::BAR_INSET))
                            .py(px(ui::BAR_INSET))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(self.border())
                            .child(div().font_weight(FontWeight::MEDIUM).child("Preferences"))
                            .child(
                                self.button(
                                    "close-preferences",
                                    "",
                                    "close",
                                    Cmd::Close,
                                    false,
                                    cx,
                                )
                                .tooltip(self.tip("Close", "Esc")),
                            ),
                    )
                    .child(sections)
                    .child(body)
                    .child(
                        div()
                            .min_h(px(52.))
                            .flex_shrink_0()
                            .px(px(ui::DIALOG_INSET))
                            .py(px(space::SM))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(self.border())
                            .text_size(px(11.))
                            .text_color(self.muted())
                            .child(
                                div().flex_1().min_w(px(0.)).child(
                                    if !self.settings_writable || self.settings_failure.is_some() {
                                        "Settings need attention · use Retry unsaved decisions"
                                    } else if self
                                        .preferences_draft
                                        .as_ref()
                                        .is_some_and(|draft| draft.applying)
                                    {
                                        "Applying changes…"
                                    } else {
                                        "Changes take effect when you Apply"
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(space::SM))
                                    .child(self.button(
                                        "cancel-preferences",
                                        "Cancel",
                                        "",
                                        Cmd::Close,
                                        false,
                                        cx,
                                    ))
                                    .child(self.button(
                                        "apply-preferences",
                                        "Apply",
                                        "check",
                                        Cmd::ApplyPreferences,
                                        true,
                                        cx,
                                    ))
                                    .child(self.button(
                                        "apply-close-preferences",
                                        "Apply & Close",
                                        "",
                                        Cmd::ApplyClosePreferences,
                                        false,
                                        cx,
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn empty(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let library_empty = self.photos.is_empty();
        let hint = |key: &str, what: &str| {
            div()
                .flex()
                .gap(px(space::SM))
                .child(div().text_color(self.text()).child(key.to_owned()))
                .child(what.to_owned())
        };
        div()
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(ui::PANEL_INSET))
            .text_center()
            .gap(px(space::MD))
            .bg(self.canvas_bg())
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(if library_empty {
                        "Insert a card or Import a folder"
                    } else if self.duplicates_only {
                        "Review duplicate photos"
                    } else if self.filter == Filter::Trash {
                        "Trash is empty"
                    } else {
                        "Nothing matches this view"
                    }),
            )
            .child(div().text_color(self.muted()).child(if library_empty {
                "Open a folder or card to start culling. Originals are never modified."
            } else if self.duplicates_only {
                "Scan to verify exact file matches. Nothing is moved or removed automatically."
            } else if self.filter == Filter::Trash {
                "Trashed photos can be restored with their ratings and flags."
            } else {
                "Choose another collection or clear the rating filter."
            }))
            .when(self.duplicates_only, |s| {
                s.child(self.button(
                    "empty-scan-duplicates",
                    "Find Duplicates",
                    "duplicates",
                    Cmd::ScanDuplicates,
                    false,
                    cx,
                ))
            })
            .when(library_empty, |s| {
                s.child(
                    div().pt(px(space::SM)).child(
                        self.button(
                            "empty-open",
                            "Open Folder…",
                            "folder-opened",
                            Cmd::Import,
                            false,
                            cx,
                        )
                        .border_1()
                        .border_color(self.c("border.variant", "#333333"))
                        .tooltip(self.tip("Open folder", "⌘O")),
                    ),
                )
            })
            .child(
                div()
                    .pt(px(space::LG))
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(space::LG))
                    .text_size(px(11.))
                    .text_color(self.muted())
                    .child(hint("← →", "navigate"))
                    .child(hint("P", "pick"))
                    .child(hint("X", "reject"))
                    .child(hint("1–5", "rate"))
                    .child(hint(&shortcut("⌘K"), "commands")),
            )
            .into_any_element()
    }
}

impl Workspace {
    fn status_bar(&self, window: &Window, cx: &gpui::Context<Self>) -> AnyElement {
        let stats = self.media.stats();
        let failed = self.recovery_broken
            || self.settings_failure.is_some()
            || (!self.unsaved.is_empty() && self.pending_saves == 0);
        let (save_text, save_color) = if failed {
            ("Not saved · retry", self.c("error", "#d46a6a"))
        } else if self.pending_saves > 0 || self.settings_dirty || self.recovery_pending {
            ("Saving…", self.muted())
        } else {
            ("Saved", self.muted())
        };
        let retry = failed;
        // The footer describes the current workspace, rather than retaining
        // the last successful action as a permanent notification.
        let context = if let Some(drag) = self.dock_drag {
            let (name, minimum, actual) = match drag.side {
                DockSide::Library => ("Catalog", ui::LIBRARY_MIN_WIDTH, self.docks.library),
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
        } else if self.quitting {
            self.status.clone()
        } else if self.import_ui.active() {
            if self.import_ui.progress.phase == "paused" {
                "Import paused".into()
            } else {
                format!(
                    "Importing · {} of {}",
                    self.import_ui.progress.completed, self.import_ui.progress.total
                )
            }
        } else if self.import_ui.scanning {
            "Scanning import…".into()
        } else if self.busy {
            "Indexing photos…".into()
        } else if let Some(scan) = self
            .duplicate_scan
            .as_ref()
            .filter(|s| s.phase == "running")
        {
            format!(
                "Checking duplicates · {} of {}",
                scan.processed, scan.candidates
            )
        } else {
            let exports = self
                .export_jobs
                .iter()
                .filter(|job| matches!(job["state"].as_str(), Some("queued" | "running")))
                .count();
            if exports > 0 {
                format!("Exporting · {}", plural(exports, "job", "jobs"))
            } else if self.multi.len() > 1 {
                format!(
                    "{} selected · {}",
                    self.multi.len(),
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
            .text_size(px(11.))
            .text_color(self.muted())
            .child(
                self.compact_button(
                    "toggle-sidebar",
                    "",
                    "panel-left",
                    Cmd::Sidebar,
                    self.docks.library > 0.,
                    cx,
                )
                .tooltip(self.tip("Toggle catalog", "⌘B")),
            )
            .when(self.import_ui.active(), |s| {
                s.child(
                    div().w(px(80.)).h(px(3.)).bg(self.border()).child(
                        div()
                            .h_full()
                            .w(relative(
                                (self.import_ui.progress.completed as f32
                                    / self.import_ui.progress.total.max(1) as f32)
                                    .clamp(0., 1.),
                            ))
                            .bg(self.accent()),
                    ),
                )
                .child(self.compact_button(
                    "ingest-status-show",
                    "Import",
                    "",
                    Cmd::Ingest("show".into()),
                    false,
                    cx,
                ))
                .child(self.compact_button(
                    "ingest-status-pause",
                    if self.import_ui.progress.phase == "paused" {
                        "Resume"
                    } else {
                        "Pause"
                    },
                    "",
                    Cmd::Ingest("pause".into()),
                    false,
                    cx,
                ))
                .child(self.compact_button(
                    "ingest-status-cancel",
                    "Cancel",
                    "",
                    Cmd::Ingest("cancel".into()),
                    false,
                    cx,
                ))
            })
            .child(
                div()
                    .id("workspace-status")
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_normal()
                    .line_clamp(1)
                    .child(context),
            )
            .when(
                self.workspace_size.0 >= 1120.
                    && self
                        .remote_hosts
                        .values()
                        .any(|host| host.state != "connected"),
                |s| {
                    let text = if let Some(host) = self
                        .remote_hosts
                        .values()
                        .find(|h| h.state == "indexing" || h.state == "connecting")
                    {
                        format!(
                            "{} · {}{}",
                            host.config.display_name,
                            host.state,
                            host.progress
                                .as_ref()
                                .map(|p| format!(" · {} scanned", p.scanned))
                                .unwrap_or_default()
                        )
                    } else {
                        let offline = self
                            .remote_hosts
                            .values()
                            .filter(|h| h.state != "connected")
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
            .child(
                div()
                    .id("save-state")
                    .px(px(space::XS))
                    .text_color(save_color)
                    .when(retry, |s| {
                        s.cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.execute(Cmd::RetrySaves, window, cx)
                            }))
                    })
                    .child(save_text),
            )
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
                self.compact_button(
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
                self.compact_button(
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
                self.compact_button(
                    "preferences",
                    "",
                    "settings",
                    Cmd::Preferences,
                    self.preferences,
                    cx,
                )
                .tooltip(self.tip("Preferences", "⌘,")),
            )
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if self._automation_wake.is_none() {
            self.drain_automation(window, cx);
        }
        self.reconcile_rating_filter();
        self.apply_theme_mode(window);
        for texture in self.retired.drain(..) {
            let _ = window.drop_image(texture.image);
        }
        self.visible_textures.clear();
        let viewport = window.viewport_size();
        let (mut width, mut height) = (f32::from(viewport.width), f32::from(viewport.height));
        if let Decorations::Client { tiling } = window.window_decorations() {
            let edge = if std::env::var_os("WAYLAND_DISPLAY").is_some() && !tiling.is_tiled() {
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
        self.docks = ui::fit_docks(
            self.workspace_size.0,
            (self.sidebar && !self.focus_mode).then_some(self.settings.sidebar_width as f32),
            (self.inspector && !self.focus_mode && !self.import_ui.open)
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
        let strip_height =
            if self.filtered.is_empty() || self.mode == ViewMode::Grid || self.import_ui.open {
                0.
            } else {
                FILMSTRIP_HEIGHT
            };
        self.center_size = (
            self.docks.center,
            self.workspace_size.1
                - TITLE_HEIGHT
                - if self.import_ui.open {
                    0.
                } else {
                    TOOLBAR_HEIGHT
                }
                - decision_height
                - strip_height
                - ui::STATUS_HEIGHT,
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
                }
            }
            viewer
                .child(self.photo_view(photo, false, window, cx))
                .into_any_element()
        };
        let mut main = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .child(self.filters(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(center),
            );
        if !self.filtered.is_empty() {
            main = main.child(self.rating_bar(cx));
            if self.mode != ViewMode::Grid {
                main = main.child(self.filmstrip(cx));
            }
        }
        let mut body = div().relative().flex().flex_1().min_h(px(0.)).min_w(px(0.));
        if self.docks.library > 0. {
            body = body.child(self.sidebar(cx));
        }
        if self.import_ui.open {
            body = body.child(self.ingest_view(cx));
        } else {
            body = body.child(main);
        }
        if self.docks.inspector > 0. {
            body = body.child(self.inspector_view(cx));
        }
        if self.docks.library > 0. {
            body = body.child(self.dock_resize_grip(DockSide::Library, window, cx));
        }
        if self.docks.inspector > 0. {
            body = body.child(self.dock_resize_grip(DockSide::Inspector, window, cx));
        }
        let mut root = div()
            .id("shadowroom")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.move_scrollbar(event, cx);
                this.move_dock_resize(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.scroll_drag = None;
                    this.finish_dock_resize(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.scroll_drag = None;
                    this.finish_dock_resize(cx);
                }),
            )
            .on_action(cx.listener(|this, _: &QuitApp, _window, cx| this.request_quit(cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(self.settings.interface_font.clone())
            .text_size(px(12.))
            .line_height(px(ui::TEXT_LINE_HEIGHT))
            .text_color(self.text())
            .bg(self.bg())
            .child(self.title_bar(window, cx))
            .child(body)
            .child(self.status_bar(window, cx));
        if let Some(error) = &self.error {
            root = root.child(
                div()
                    .id("error-toast")
                    .occlude()
                    .absolute()
                    .bottom(px(ui::STATUS_HEIGHT + space::SM))
                    .right(px(ui::PANEL_INSET))
                    .w(px(420.))
                    .p(px(ui::BAR_INSET))
                    .border_1()
                    .border_color(self.c("error", "#d46a6a"))
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .flex()
                    .items_start()
                    .gap(px(space::MD))
                    .child(div().flex_1().child(error.clone()))
                    .child(self.button("dismiss-error", "", "close", Cmd::Close, false, cx)),
            );
        }
        if let Some((fields, active)) = &self.add_host {
            let mut form = div().w(px(560.)).p(px(space::XL)).flex().flex_col().gap(px(space::MD)).border_1().border_color(self.border()).bg(self.c("elevated_surface.background", "#1b1b1b"))
                .child(div().text_size(px(16.)).child("Add network host"))
                .child(div().text_color(self.muted()).child("Use your existing SSH configuration. Tab changes fields; Enter checks the agent."));
            for (n, label) in ["SSH alias or user@host", "Remote command (optional)"]
                .into_iter()
                .enumerate()
            {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::XS))
                        .child(label)
                        .child(
                            div()
                                .id(("host-field", n))
                                .min_h(px(ui::FIELD_HEIGHT))
                                .p(px(ui::FIELD_INSET))
                                .border_1()
                                .border_color(if *active == n {
                                    self.accent()
                                } else {
                                    self.border()
                                })
                                .bg(self.bg())
                                .overflow_hidden()
                                .child(self.input_text(&fields[n], "", *active == n))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some((_, active)) = &mut this.add_host {
                                        *active = n;
                                    }
                                    if let Some((fields, _)) = &this.add_host {
                                        this.text_edit.focus(&fields[n]);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            form = form.child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(space::SM))
                    .child(self.button("host-cancel", "Cancel", "close", Cmd::Close, false, cx))
                    .child(self.button(
                        "host-save",
                        "Add & connect",
                        "add",
                        Cmd::SaveRemoteHost,
                        false,
                        cx,
                    )),
            );
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x00000088))
                    .child(form),
            );
        }
        if self.rating_filter_open && !self.palette && !self.preferences && !self.import_ui.open {
            root = root.child(self.rating_filter_menu(cx));
        }
        if self.palette {
            root = root.child(self.palette_view(cx));
        }
        if self.preferences {
            root = root.child(self.preferences_view(window, cx));
        }
        if self.getting_started {
            root = root.child(self.getting_started_view(cx));
        }
        if self.image_context.is_some() {
            root = root.child(self.image_context_view(cx));
        }
        if self.deletion.open() {
            root = root.child(self.deletion_view(cx));
        }
        self.trim_display_cache();
        // Client-side decorations (Wayland, composited X11): transparent resize
        // margin around a 1px framed window. Server/macOS decorations need none.
        let Decorations::Client { tiling } = window.window_decorations() else {
            return root.into_any_element();
        };
        // Wayland surfaces are transparent, so the resize margin sits outside a
        // shadowed frame. GPUI's X11 surface is opaque: grips go inside the edge.
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
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
                    window_min_size: Some(size(px(960.), px(640.))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| Workspace::new(cx, import, window.window_handle()));
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
        BurstIndex, Cmd, CollectionCounts, PaletteCache, SurveySummary, UiInbox, adjust_folder,
        index_filtered_positions, item, label_index,
    };
    use shadowroom::{
        model::{ColorLabel, Flag, Photo},
        workspace::{Decision, Filter, fuzzy_score},
    };
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
                    recent
                        .iter()
                        .position(|title| *title == cache.items[i].title)
                        .unwrap_or(usize::MAX)
                });
            } else {
                expected.sort_by(|a, b| b.0.cmp(&a.0));
            }
            assert_eq!(
                cache.matches,
                expected.into_iter().map(|(_, i)| i).collect::<Vec<_>>()
            );
        }
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
}
