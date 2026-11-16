use super::*;
use shadowroom::ingest::{
    self, IngestGroup, IngestOptions, Progress, Scan,
    worker::{Event, PlanSummary, QueuedJob},
};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportPanel {
    Photos,
    Timeline,
    Destination,
}

const IMPORT_RESIZE_HANDLE: f32 = space::SM;

/// Source scan lifecycle of the Import view.
///
/// ```text
/// Idle ─request─▶ Scanning ─Scanned─▶ Ready
///                    │  ▲
///   enqueue/worker   │  └─request (rescan)
///   error            ▼
///                  Failed
/// ```
///
/// Every path out of `Scanning` is explicit: a scan that could not be queued, or
/// that the worker rejected or aborted, ends in `Failed` instead of leaving the
/// view (and plan/start gating) stuck on "scanning" forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ScanState {
    #[default]
    Idle,
    Scanning,
    Ready,
    Failed,
}

/// What a click or key does to the Import grid's current tile and import set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GridPick {
    /// Plain click or arrow: move the current tile only.
    Focus,
    /// Checkbox, Space, Ctrl/Cmd-click: include or leave out one tile.
    Toggle,
    /// Shift-click or Shift-arrow: give the range the anchor's state.
    Range,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum IngestMenu {
    Preset,
    Selection,
    /// Time gap that splits the timeline into sessions.
    Gap,
}

const SELECTIONS: [(&str, &str); 4] = [
    ("all", "All photos"),
    ("picks", "Picks"),
    ("except_rejects", "Everything except rejects"),
    ("rated", "Rated photos"),
];

/// The same geometry drives the visible panels and the virtualized grid.
#[derive(Clone, Copy)]
struct ImportLayout {
    timeline: f32,
    destination: f32,
    grid: f32,
}

impl ImportLayout {
    fn new(width: f32, timeline: f32, destination: f32) -> Self {
        let width = width.max(0.);
        let has_timeline = width >= 880.;
        let has_destination = width >= 600.;
        let destination = if has_destination {
            destination.clamp(240., (width - IMPORT_RESIZE_HANDLE - 212.).min(400.))
        } else {
            0.
        };
        let timeline = if has_timeline {
            timeline.clamp(
                136.,
                (width - destination - 2. * IMPORT_RESIZE_HANDLE - 212.).min(240.),
            )
        } else {
            0.
        };
        let handles = if has_timeline {
            IMPORT_RESIZE_HANDLE
        } else {
            0.
        } + if has_destination {
            IMPORT_RESIZE_HANDLE
        } else {
            0.
        };
        Self {
            timeline,
            destination,
            grid: (width - timeline - destination - handles).max(0.),
        }
    }

    fn columns(self) -> usize {
        ((self.grid - scrollbar::WIDTH - 2. * ui::BAR_INSET + space::SM) / (180. + space::SM))
            .floor()
            .clamp(1., 6.) as usize
    }
}

/// Everything a grid tile shows that is derived from its group. Text is shared,
/// so a visible tile costs reference-count bumps per frame instead of cloning
/// an `IngestGroup` and a `Photo` and formatting its size, date and tooltip.
#[derive(Clone)]
struct IngestCell {
    id: usize,
    name: SharedString,
    detail: SharedString,
    tip: SharedString,
    badge: Option<&'static str>,
    /// Stills always have a preview; videos only when a THM substitute exists.
    has_preview: bool,
    seen: bool,
    /// Chip on a tile imported before: "Imported 12 Oct" or "Marked imported".
    seen_label: Option<SharedString>,
}

impl IngestCell {
    fn new(group: &IngestGroup, preview: &Photo) -> Self {
        // Badge only what differs from a plain still.
        let badge = if group.kind == "video" {
            Some("VIDEO")
        } else if group.companion.is_some() {
            Some("RAW+JPEG")
        } else {
            None
        };
        let tip = group.seen.as_ref().map_or_else(
            || {
                group
                    .warning
                    .clone()
                    .unwrap_or_else(|| group.primary.path.display().to_string())
            },
            |s| {
                format!(
                    "Imported {} → {}",
                    chrono::DateTime::from_timestamp_millis(s.imported_at)
                        .map_or_else(|| "previously".into(), |t| t.format("%Y-%m-%d").to_string()),
                    s.destination
                        .as_ref()
                        .map_or_else(|| "marked by user".into(), |p| p.display().to_string())
                )
            },
        );
        // "Imported 12 Oct" for a copy, "Marked imported" for Mark as imported.
        let seen_label = group.seen.as_ref().map(|s| {
            if s.destination.is_none() {
                SharedString::from("Marked imported")
            } else {
                chrono::DateTime::from_timestamp_millis(s.imported_at)
                    .map_or_else(
                        || "Imported".to_owned(),
                        |t| {
                            format!(
                                "Imported {}",
                                t.with_timezone(&chrono::Local).format("%-d %b")
                            )
                        },
                    )
                    .into()
            }
        });
        Self {
            id: group.id,
            name: group.photo.filename.clone().into(),
            detail: copy::fmt_bytes(group.bytes()).into(),
            tip: tip.into(),
            badge,
            has_preview: group.kind == "photo" || preview.path != group.primary.path,
            seen: group.seen.is_some(),
            seen_label,
        }
    }
}

pub(super) struct ImportUi {
    pub open: bool,
    scan_state: ScanState,
    pub scan: Option<Arc<Scan>>,
    pub items: Vec<IngestGroup>,
    pub selected: HashSet<usize>,
    focused: Option<usize>,
    anchor: Option<usize>,
    /// The import set when a Shift range began, and whether the range
    /// includes (the anchor's state); shrinking the range restores the base.
    range_base: Option<(HashSet<usize>, bool)>,
    pub lookup: HashMap<usize, usize>,
    pub photo_lookup: HashMap<i64, usize>,
    pub previews: Vec<Photo>,
    /// Per-item render model, parallel to `items`. Rebuilt only when an item's
    /// seen state changes, so the grid never clones a group or formats per frame.
    cells: Vec<IngestCell>,
    pub visible: Vec<usize>,
    pub timeline: Vec<ingest::TimeGroup>,
    pub timeline_lookup: HashMap<usize, usize>,
    pub timeline_selected: Vec<usize>,
    pub day_counts: HashMap<String, (usize, usize)>,
    pub new_only: bool,
    pub new_count: usize,
    pub photos: usize,
    pub videos: usize,
    pub bytes: u64,
    pub options: IngestOptions,
    pub fields: [String; 5],
    pub active_field: Option<usize>,
    pub dirty_at: Option<Instant>,
    pub plan: Option<Arc<PlanSummary>>,
    pub progress: Progress,
    pub history: Arc<Value>,
    pub show_history: bool,
    pub error: Option<String>,
    pub source: PathBuf,
    pub next_preview_id: i64,
    pub scroll: UniformListScrollHandle,
    pub timeline_scroll: UniformListScrollHandle,
    pub range: Option<(usize, usize)>,
    pub menu: Option<IngestMenu>,
    menu_index: usize,
    /// A click outside an open menu closes it; the same press must not reopen it.
    menu_dismissed: Option<(IngestMenu, Point<Pixels>)>,
    /// Template field that receives a token clicked in the reference.
    template_field: usize,
    pub expected_plan: Option<IngestOptions>,
    panel: ImportPanel,
    timeline_width: f32,
    destination_width: f32,
    resizing: Option<(ImportPanel, f32, f32, f32)>,
    resize_moved: bool,
    suppress_resize_click: bool,
    /// Groups this session's job committed (and verified) from the current source.
    recent: HashSet<usize>,
    /// What the last job did, shown above the footer until dismissed.
    outcome: Option<Outcome>,
    /// An action waiting for its inline second click (Cancel, Mark as imported).
    confirm: Option<(IngestConfirm, Instant)>,
    /// Keyboard choice in an open confirmation: the safe button (Tab/arrows
    /// move it; Enter activates it). Off means the confirm button.
    confirm_keep: bool,
    /// The naming token reference is open (always while a template is edited).
    tokens_open: bool,
    /// Imports waiting for the running job (another card), in start order.
    pub queue: Vec<QueuedJob>,
    /// The card of the last job that completed: its sidebar row reads
    /// "Done · safe to eject" until it is ejected or another job starts.
    pub done_source: Option<PathBuf>,
    /// The failed job's error list is unfolded under its outcome.
    details_open: bool,
    /// "Retry failed" picked the failed photos: start once their plan is ready.
    start_when_planned: bool,
    /// History rows unfolded to show their errors and destinations, by job ID.
    history_open: HashSet<String>,
    /// Library folder of the last completed job ("Show" in its notice).
    pub last_root: Option<PathBuf>,
}

/// What a folder chosen in the picker is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FolderIntent {
    /// "Import from Card…": copy and verify through the Import view.
    #[default]
    Import,
    /// "Open Folder…": index in place and cull; nothing is copied.
    Cull,
}

/// Actions that ask once, inline, before they run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum IngestConfirm {
    Cancel,
    Mark,
}

/// The last job's result, in the panel: what is safe, what is left.
#[derive(Clone, Debug)]
pub(super) struct Outcome {
    tone: Tone,
    text: String,
    /// Offer to import the photos that are still selected.
    remaining: bool,
    /// A completed job's library folder: offer to cull or reveal it.
    library: Option<PathBuf>,
    /// Every error of a failed job, listed under Details instead of joined
    /// into one long line.
    errors: Vec<String>,
    /// Scan IDs of the photos that failed, for Retry failed.
    failed_ids: Vec<usize>,
    /// The job's verified manifest (exact paths and BLAKE3 digests).
    manifest: Option<PathBuf>,
}
impl ImportUi {
    pub fn new(settings: &Settings) -> Self {
        let options = IngestOptions::from_settings(settings);
        let fields = [
            options.library_root.to_string_lossy().into_owned(),
            options.folder_template.clone(),
            options.file_template.clone(),
            options.job_code.clone(),
            options
                .backup_roots
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(";"),
        ];
        Self {
            open: false,
            scan_state: ScanState::Idle,
            scan: None,
            items: Vec::new(),
            selected: HashSet::new(),
            focused: None,
            anchor: None,
            range_base: None,
            lookup: HashMap::new(),
            photo_lookup: HashMap::new(),
            previews: Vec::new(),
            cells: Vec::new(),
            visible: Vec::new(),
            timeline: Vec::new(),
            timeline_lookup: HashMap::new(),
            timeline_selected: Vec::new(),
            day_counts: HashMap::new(),
            new_only: false,
            new_count: 0,
            photos: 0,
            videos: 0,
            bytes: 0,
            options,
            fields,
            active_field: None,
            dirty_at: None,
            plan: None,
            progress: Progress::default(),
            history: Arc::new(json!({})),
            show_history: false,
            error: None,
            source: PathBuf::new(),
            next_preview_id: -1,
            scroll: UniformListScrollHandle::new(),
            timeline_scroll: UniformListScrollHandle::new(),
            range: None,
            menu: None,
            menu_index: 0,
            menu_dismissed: None,
            template_field: 2,
            expected_plan: None,
            panel: ImportPanel::Photos,
            timeline_width: ui::INGEST_TIMELINE_WIDTH,
            destination_width: ui::INGEST_DESTINATION_WIDTH,
            resizing: None,
            resize_moved: false,
            suppress_resize_click: false,
            recent: HashSet::new(),
            outcome: None,
            confirm: None,
            confirm_keep: false,
            tokens_open: false,
            queue: Vec::new(),
            done_source: None,
            details_open: false,
            start_when_planned: false,
            history_open: HashSet::new(),
            last_root: None,
        }
    }
    /// Whether `confirm` is armed and waiting for its answer. It stays until
    /// answered or dismissed (Escape), never on a timer: the prompt takes
    /// a few seconds to read.
    fn confirming(&self, what: IngestConfirm) -> bool {
        self.confirm.is_some_and(|(c, _)| c == what)
    }
    fn arm(&mut self, what: IngestConfirm) {
        self.confirm = Some((what, Instant::now()));
        self.confirm_keep = false;
    }
    /// Escape answers an open confirmation with its safe choice.
    pub fn dismiss_confirmation(&mut self) -> bool {
        self.confirm.take().is_some()
    }
    /// Cancel was clicked once and waits for the second click.
    pub fn cancel_pending(&self) -> bool {
        self.active() && self.confirming(IngestConfirm::Cancel)
    }
    fn append(&mut self, group: IngestGroup) {
        let index = self.items.len();
        if group.seen.is_none() {
            self.new_count += 1;
        }
        let mut photo = group.photo.clone();
        photo.id = self.next_preview_id;
        self.next_preview_id -= 1;
        if group.kind == "video" {
            if let Some(thm) = group.sidecars.iter().find(|f| {
                f.path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("thm"))
            }) {
                photo.path = thm.path.clone();
                photo.modified_at = thm.mtime_ns / 1_000_000;
                photo.size_bytes = thm.size;
            }
        }
        self.photo_lookup.insert(photo.id, index);
        self.cells.push(IngestCell::new(&group, &photo));
        self.previews.push(photo);
        self.lookup.insert(group.id, index);
        if !self.new_only || group.seen.is_none() {
            self.visible.push(index);
        }
        let selected = group.selected.then_some(group.id);
        self.items.push(group);
        if let Some(id) = selected {
            self.select_count(index, true);
            self.selected.insert(id);
        }
    }
    fn clear_items(&mut self) {
        self.items.clear();
        self.selected.clear();
        self.focused = None;
        self.anchor = None;
        self.range_base = None;
        self.lookup.clear();
        self.photo_lookup.clear();
        self.previews.clear();
        self.cells.clear();
        self.visible.clear();
        self.timeline.clear();
        self.timeline_lookup.clear();
        self.timeline_selected.clear();
        self.day_counts.clear();
        self.new_count = 0;
        self.photos = 0;
        self.videos = 0;
        self.bytes = 0;
        self.range = None;
    }
    fn timeline_counts(&mut self) {
        self.timeline_lookup.clear();
        self.timeline_selected.clear();
        self.day_counts.clear();
        for (index, group) in self.timeline.iter().enumerate() {
            let selected = group
                .ids
                .iter()
                .filter(|id| self.selected.contains(id))
                .count();
            self.timeline_selected.push(selected);
            for id in &group.ids {
                self.timeline_lookup.insert(*id, index);
            }
            let day = self.day_counts.entry(group.day.clone()).or_default();
            day.0 += selected;
            day.1 += group.ids.len();
        }
    }
    pub(super) fn defaults(&mut self, settings: &Settings) {
        let options = IngestOptions::from_settings(settings);
        self.fields = [
            options.library_root.to_string_lossy().into_owned(),
            options.folder_template.clone(),
            options.file_template.clone(),
            String::new(),
            options
                .backup_roots
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(";"),
        ];
        self.options = options;
        self.active_field = None;
        self.changed();
    }
    fn select_count(&mut self, index: usize, add: bool) {
        let group = &self.items[index];
        let counter = if group.kind == "video" {
            &mut self.videos
        } else {
            &mut self.photos
        };
        let bytes = group.bytes();
        if add {
            *counter += 1;
            self.bytes += bytes;
        } else {
            *counter -= 1;
            self.bytes -= bytes;
        }
    }
    fn toggle(&mut self, id: usize) {
        if let Some(&index) = self.lookup.get(&id) {
            let add = if self.selected.remove(&id) {
                false
            } else {
                self.selected.insert(id);
                true
            };
            self.select_count(index, add);
            if let Some(&index) = self.timeline_lookup.get(&id) {
                let day = self
                    .day_counts
                    .get_mut(&self.timeline[index].day)
                    .expect("timeline day");
                if add {
                    self.timeline_selected[index] += 1;
                    day.0 += 1;
                } else {
                    self.timeline_selected[index] -= 1;
                    day.0 -= 1;
                }
            }
            self.changed();
        }
    }
    /// Click/arrow handling in the Import grid. The current tile and the
    /// import set are separate: a plain click or arrow only moves the current
    /// tile, so looking at a photo never shrinks the import.
    fn select_position(&mut self, position: usize, pick: GridPick) {
        let Some(&index) = self.visible.get(position) else {
            return;
        };
        let id = self.items[index].id;
        match pick {
            GridPick::Focus => {
                self.anchor = Some(id);
                self.range_base = None;
            }
            GridPick::Toggle => {
                self.toggle(id);
                self.anchor = Some(id);
                self.range_base = None;
            }
            GridPick::Range => {
                let visible_position =
                    |id| self.visible.iter().position(|&i| self.items[i].id == id);
                let anchor = self
                    .anchor
                    .and_then(visible_position)
                    .or_else(|| self.focused.and_then(visible_position))
                    .unwrap_or(0);
                let anchor_id = self.items[self.visible[anchor]].id;
                // A first Shift-click/arrow starts at the first visible photo.
                // Keep that anchor while the range grows or shrinks; the range
                // takes the anchor's state on top of the set as it was before.
                self.anchor = Some(anchor_id);
                let selected = &self.selected;
                let (base, include) = self
                    .range_base
                    .get_or_insert_with(|| (selected.clone(), selected.contains(&anchor_id)))
                    .clone();
                let mut wanted = base;
                for &i in &self.visible[anchor.min(position)..=anchor.max(position)] {
                    let id = self.items[i].id;
                    if include {
                        wanted.insert(id);
                    } else {
                        wanted.remove(&id);
                    }
                }
                let changes: Vec<_> = self
                    .selected
                    .symmetric_difference(&wanted)
                    .copied()
                    .collect();
                for id in changes {
                    self.toggle(id);
                }
            }
        }
        self.focused = Some(id);
    }
    /// The current tile's visible position, if it is shown.
    fn focused_position(&self) -> Option<usize> {
        self.focused
            .and_then(|id| self.visible.iter().position(|&i| self.items[i].id == id))
    }
    /// Space: include or leave out the current tile (the first one if none).
    fn toggle_focused(&mut self) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let position = self.focused_position().unwrap_or(0);
        self.select_position(position, GridPick::Toggle);
        Some(position)
    }
    fn navigate(&mut self, key: &str, columns: usize, range: bool) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let focused = self.focused_position();
        let position = focused.unwrap_or(0);
        let delta = match key {
            "down" => columns as isize,
            "up" => -(columns as isize),
            "right" => 1,
            "left" => -1,
            "home" => -(position as isize),
            "end" => self.visible.len() as isize,
            _ => return None,
        };
        let target = if focused.is_none() && !range && key != "end" {
            0
        } else {
            position
                .saturating_add_signed(delta)
                .min(self.visible.len() - 1)
        };
        self.select_position(
            target,
            if range {
                GridPick::Range
            } else {
                GridPick::Focus
            },
        );
        Some(target)
    }
    /// Groups recorded as imported, by a finished job or by Mark as imported.
    /// Only those groups change; every other checkbox keeps the user's choice.
    fn committed(&mut self, committed: Vec<(usize, ingest::store::SeenRecord)>) {
        let job = self.job_here();
        for (id, seen) in committed {
            if job {
                self.recent.insert(id);
            }
            if let Some(&index) = self.lookup.get(&id) {
                if self.items[index].seen.is_none() {
                    self.new_count = self.new_count.saturating_sub(1);
                }
                if self.selected.remove(&id) {
                    self.select_count(index, false);
                }
                self.items[index].seen = Some(seen);
                self.cells[index] = IngestCell::new(&self.items[index], &self.previews[index]);
            }
        }
        self.timeline_counts();
        if self.new_only {
            self.visible = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, g)| g.seen.is_none())
                .map(|(i, _)| i)
                .collect();
        }
        if job {
            self.plan = None;
        } else {
            // Marked as imported: the selection shrank, so replan it. The
            // debounced tick never plans while an import is running.
            self.changed();
        }
    }
    /// Finish a streamed scan without resetting choices made on early batches.
    /// Batches can be omitted under backpressure, so unseen groups still use
    /// the scanner's default selection.
    fn scanned(&mut self, scan: Arc<Scan>, time_group_minutes: u32) {
        let shown = std::mem::take(&mut self.lookup);
        let selected = std::mem::take(&mut self.selected);
        self.clear_items();
        for group in &scan.groups {
            let mut group = group.clone();
            if shown.contains_key(&group.id) {
                group.selected = selected.contains(&group.id);
            }
            self.append(group);
        }
        self.timeline = ingest::timeline(&scan, time_group_minutes);
        self.timeline_counts();
        self.source = scan.source.clone();
        self.scan = Some(scan);
        self.scan_state = ScanState::Ready;
        self.changed();
    }
    pub(super) fn invalidate_plan(&mut self) {
        self.changed();
    }
    fn changed(&mut self) {
        self.expected_plan = None;
        self.plan = None;
        self.dirty_at = Some(Instant::now());
        self.error = None;
    }
    fn options(&mut self) -> IngestOptions {
        self.options.library_root = PathBuf::from(self.fields[0].trim());
        self.options.folder_template = self.fields[1].clone();
        self.options.file_template = self.fields[2].clone();
        self.options.job_code = self.fields[3].clone();
        self.options.backup_roots = self.fields[4]
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect();
        self.options.selected_ids = Some(self.selected.iter().copied().collect());
        self.options.clone()
    }
    pub fn active(&self) -> bool {
        self.progress.phase.is_active()
    }
    /// The running job reads the source this view shows (not another card
    /// scanned while it copies).
    pub fn job_here(&self) -> bool {
        self.active() && self.job_of(&self.source)
    }
    /// Whether the current or last job belongs to `source`. Older progress
    /// without a source is taken as the shown one.
    fn job_of(&self, source: &std::path::Path) -> bool {
        self.progress
            .source
            .as_deref()
            .is_none_or(|job| job == source)
    }
    /// This source waits behind the running job.
    pub fn queued(&self, source: &std::path::Path) -> bool {
        self.queue.iter().any(|job| job.source == source)
    }
    /// What a sidebar drive row adds to its name: the copy state of its card.
    pub fn drive_state(&self, path: &std::path::Path) -> Option<String> {
        if self.active() && self.progress.source.as_deref() == Some(path) {
            let percent = (copy::import_fraction(&self.progress) * 100.).round();
            return Some(match self.progress.phase {
                Phase::Paused => format!("Paused {percent:.0}%"),
                Phase::Finalizing | Phase::Indexing => "Finishing".into(),
                Phase::Queued => "Starting".into(),
                _ => format!("Copying {percent:.0}%"),
            });
        }
        if self.queued(path) {
            return Some("Queued".into());
        }
        if self.done_source.as_deref() == Some(path) {
            return Some("Done · safe to eject".into());
        }
        (self.source == path && self.scan.is_some())
            .then(|| format!("{} new", copy::count(self.new_count)))
    }
    pub fn scanning(&self) -> bool {
        self.scan_state == ScanState::Scanning
    }
    /// Opens the view on `path`. Returns whether a scan should be queued.
    /// Another card scans (read-only) while a job copies, so it can be
    /// reviewed and queued; the card being copied is never scanned again, and
    /// the view keeps showing what it shows.
    fn request_scan(&mut self, path: PathBuf) -> bool {
        self.open = true;
        self.panel = ImportPanel::Photos;
        self.active_field = None;
        self.resizing = None;
        self.show_history = false;
        if self.active() && self.progress.source.as_ref() == Some(&path) {
            return false;
        }
        self.error = None;
        self.scan_state = ScanState::Scanning;
        self.source = path;
        self.plan = None;
        true
    }
    /// The scan never started (queue full or worker unavailable) or was aborted.
    fn scan_failed(&mut self, error: String) {
        if self.scan_state == ScanState::Scanning {
            self.scan_state = ScanState::Failed;
        }
        self.error = Some(error);
    }
}
impl Workspace {
    /// A desktop notification for a finished import or export, only while the
    /// window is in the background (the in-window notice covers the rest) and
    /// unless turned off in Preferences → Import.
    pub(super) fn notify_desktop(&self, summary: &str, body: &str) {
        if self.settings.ingest_notify
            && !self.window_active
            && let Err(error) = desktop_notification(summary, body)
        {
            eprintln!("Desktop notification unavailable: {error}");
        }
    }
    pub(super) fn cancel_ingest_resize(&mut self) -> bool {
        let Some((panel, _, _, preferred)) = self.import_ui.resizing.take() else {
            return false;
        };
        self.import_ui.suppress_resize_click = true;
        if panel == ImportPanel::Timeline {
            self.import_ui.timeline_width = preferred;
        } else {
            self.import_ui.destination_width = preferred;
        }
        true
    }

    fn finish_ingest_resize(&mut self) {
        if self.import_ui.resizing.take().is_some() && self.import_ui.resize_moved {
            self.import_ui.suppress_resize_click = true;
        }
    }

    fn move_ingest_resize(&mut self, event: &MouseMoveEvent, cx: &mut gpui::Context<Self>) {
        if !event.dragging() {
            self.finish_ingest_resize();
            return;
        }
        if let Some((panel, origin, width, _)) = self.import_ui.resizing {
            let delta = f32::from(event.position.x) - origin;
            self.import_ui.resize_moved |= delta.abs() >= 1.;
            if panel == ImportPanel::Timeline {
                self.import_ui.timeline_width = (width + delta).clamp(136., 240.);
            } else {
                self.import_ui.destination_width = (width - delta).clamp(240., 400.);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn ingest_layout_state(&self) -> Value {
        let layout = self.ingest_layout();
        json!({"timeline_width":layout.timeline,"destination_width":layout.destination,"grid_width":layout.grid,"resizing":self.import_ui.resizing.is_some(),
            "columns":self.ingest_columns(),"selected_ids":self.import_ui.selected,"focused_id":self.import_ui.focused,"active_field":self.import_ui.active_field,
            "active_text":self.import_ui.active_field.map(|i| &self.import_ui.fields[i]),
            "scroll_offset":f32::from(self.import_ui.scroll.0.borrow().base_handle.offset().y),"visible_rows":self.import_ui.range,
            "time_groups":self.import_ui.timeline.iter().enumerate().map(|(i,g)|json!({"day":g.day,"total":g.ids.len(),"selected":self.import_ui.timeline_selected.get(i),"state":selection_icon(self.import_ui.timeline_selected.get(i).copied().unwrap_or(0),g.ids.len())})).collect::<Vec<_>>()})
    }

    /// The folder picker's choice (Linux picker and macOS panel): Open
    /// Folder culls in place, Import from Card scans it for import.
    pub(super) fn open_ingest(&mut self, path: PathBuf) {
        match std::mem::take(&mut self.folder_intent) {
            FolderIntent::Cull => self.open_in_place(path),
            FolderIntent::Import => self.scan_for_import(path),
        }
    }
    /// "Open Folder…": index the folder where it is and show it for culling.
    /// Nothing is copied; Import can still copy it later (cull first).
    pub(super) fn open_in_place(&mut self, path: PathBuf) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.import_ui.open = false;
        self.import_ui.show_history = false;
        self.import_ui.active_field = None;
        if !self.enqueue(IoCommand::Import(path.clone())) {
            self.error = Some(format!("Could not open {name}; try again").into());
            return;
        }
        self.scope = None;
        self.mode = ViewMode::Grid;
        self.pending_ingest_scope = Some(path);
        self.ingest_scope_arrived();
        self.announce(
            format!("Opening {name} · culling in place, nothing is copied"),
            Tone::Info,
        );
    }
    /// "Import from Card…": the mounted card when there is one, else a
    /// folder chosen in the picker, opens in the Import view.
    pub(super) fn import_from_card(&mut self, cx: &mut gpui::Context<Self>) {
        let card = self
            .drives
            .iter()
            .filter(|drive| drive.removable)
            .find(|drive| {
                !(self.import_ui.active()
                    && self.import_ui.progress.source.as_ref() == Some(&drive.path))
            })
            .map(|drive| drive.path.clone());
        match card {
            Some(path) if self.import_ui.source == path && self.import_ui.scan.is_some() => {
                self.ingest_action("show", cx)
            }
            Some(path) => self.scan_for_import(path),
            None => {
                self.folder_intent = FolderIntent::Import;
                self.choose_folder(None, cx);
            }
        }
    }
    /// Opens the Import view on `path` (a card or a folder) and scans it.
    pub(super) fn scan_for_import(&mut self, path: PathBuf) {
        if self.import_ui.request_scan(path.clone()) {
            let command = if self
                .drives
                .iter()
                .any(|drive| drive.removable && drive.path == path)
            {
                IoCommand::IngestScanCard(path)
            } else {
                IoCommand::IngestScan(path)
            };
            if !self.enqueue(command) {
                self.import_ui
                    .scan_failed("The scan could not be queued; try again".into());
            }
        }
        self.palette_revision = self.palette_revision.wrapping_add(1);
    }
    pub(super) fn ingest_event(&mut self, event: Event) {
        match event {
            Event::Scanning(path) => {
                self.import_ui.clear_items();
                self.import_ui.recent.clear();
                self.import_ui.outcome = None;
                self.import_ui.scan = None;
                self.import_ui.plan = None;
                self.import_ui.source = path;
                self.import_ui.scan_state = ScanState::Scanning;
                self.import_ui.error = None;
            }
            Event::Batch(groups) => {
                for group in groups {
                    self.import_ui.append(group);
                }
            }
            Event::Scanned(scan) => {
                self.import_ui
                    .scanned(scan, self.settings.ingest_time_group_minutes);
            }
            Event::Plan(plan) => {
                if self.import_ui.expected_plan.as_ref() == Some(&plan.plan.options)
                    || (self.settings.ingest_on_insert == "auto" && self.import_ui.active())
                {
                    self.import_ui.error = plan.error.clone();
                    let startable = plan.ready && !plan.plan.groups.is_empty();
                    self.import_ui.plan = Some(plan);
                    // Retry failed: the failed photos' plan is the one to run.
                    if std::mem::take(&mut self.import_ui.start_when_planned)
                        && startable
                        && let Err(error) = self.ingest_api("ingest_start", &json!({}))
                    {
                        self.error = Some(error.into());
                    }
                }
            }
            Event::Control(progress) => {
                // A pause/resume acknowledgement that arrives after the job ended (or
                // for another job) must not resurrect it; terminal phases come from
                // the job's own Progress event.
                let same_job = self.import_ui.progress.job_id.is_empty()
                    || progress.job_id.is_empty()
                    || self.import_ui.progress.job_id == progress.job_id;
                if self.import_ui.active() && progress.phase.is_active() && same_job {
                    if progress.job_id.is_empty() {
                        self.import_ui.progress.phase = progress.phase;
                    } else {
                        self.import_ui.progress = progress;
                    }
                }
            }
            Event::Progress(mut progress) => {
                let was_completed = self.import_ui.progress.phase == Phase::Completed
                    && self.import_ui.progress.job_id == progress.job_id;
                let was_active = self.import_ui.active();
                let was_missing = self.import_ui.progress.source_missing;
                if progress.job_id.is_empty() {
                    self.import_ui.progress.phase = progress.phase;
                } else {
                    if self.import_ui.progress.phase == Phase::Paused
                        && self.import_ui.progress.job_id == progress.job_id
                        && progress.phase.is_transferring()
                    {
                        progress.phase = Phase::Paused;
                    }
                    if progress.source_missing {
                        progress.phase = Phase::Paused;
                    }
                    self.import_ui.progress = progress;
                }
                // A new job (the next queued card) owns the copier now.
                if self.import_ui.active() {
                    self.import_ui.done_source = None;
                }
                let p = self.import_ui.progress.clone();
                let p = &p;
                // Whether the job reads the card this view shows; a second card
                // scanned while it copies keeps its own selection and outcome.
                let here = self.import_ui.job_of(&self.import_ui.source.clone());
                let card = copy::source_name(p);
                // The open Import view on this card already says so in its footer.
                if p.source_missing && !was_missing && !(here && self.import_ui.open) {
                    let text = copy::import_progress(p);
                    self.announce_action(
                        text,
                        Tone::Warning,
                        "Show Import",
                        Cmd::Ingest("show".into()),
                        "",
                    );
                } else if was_missing && !p.source_missing && p.phase.is_active() {
                    self.announce(format!("{card} is back · import continues"), Tone::Info);
                }
                let destinations = p.destinations.len().max(1);
                let verified = if destinations > 1 {
                    format!("verified on {destinations} destinations")
                } else {
                    "verified".to_owned()
                };
                self.status = if p.phase == Phase::Completed {
                    format!(
                        "Imported {}{} · {verified}{}",
                        plural(p.photos, "photo", "photos"),
                        if p.videos > 0 {
                            format!(", {}", plural(p.videos, "video", "videos"))
                        } else {
                            String::new()
                        },
                        if p.skipped > 0 {
                            format!(" · {} already in library", copy::count(p.skipped))
                        } else {
                            String::new()
                        }
                    )
                } else {
                    copy::import_progress(p)
                };
                // The job's result stays in the panel: what is safe, what is left.
                if was_active && p.phase.is_terminal() {
                    let imported = plural(p.completed, "photo", "photos");
                    let skipped = (p.skipped > 0).then(|| {
                        format!(
                            " · {} already in the library (identical, skipped)",
                            p.skipped
                        )
                    });
                    let remaining = if here {
                        self.import_ui.selected.len()
                    } else {
                        0
                    };
                    let outcome = match p.phase {
                        Phase::Completed => Outcome {
                            tone: Tone::Success,
                            text: format!(
                                "Imported {imported} · {verified}{}",
                                skipped.unwrap_or_default()
                            ),
                            remaining: false,
                            library: p.library_root.clone(),
                            errors: Vec::new(),
                            failed_ids: Vec::new(),
                            manifest: p.manifest.clone(),
                        },
                        Phase::Cancelled => Outcome {
                            tone: Tone::Info,
                            text: format!(
                                "Cancelled · {} of {} imported and verified · {} still selected",
                                p.completed,
                                plural(p.total, "photo", "photos"),
                                remaining
                            ),
                            remaining: remaining > 0,
                            library: None,
                            errors: Vec::new(),
                            failed_ids: Vec::new(),
                            manifest: p.manifest.clone(),
                        },
                        _ => Outcome {
                            tone: Tone::Warning,
                            text: format!(
                                "Import stopped · {} of {} imported and verified{} · {}",
                                p.completed,
                                plural(p.total, "photo", "photos"),
                                skipped.unwrap_or_default(),
                                if p.failed > 0 {
                                    format!("{} failed", p.failed)
                                } else {
                                    "it could not finish".to_owned()
                                }
                            ),
                            remaining: remaining > 0 && p.failed_ids.is_empty(),
                            library: None,
                            errors: if p.errors.is_empty() {
                                vec![
                                    "Import failed; inspect Import History for the photos it committed"
                                        .to_owned(),
                                ]
                            } else {
                                p.errors.clone()
                            },
                            failed_ids: p.failed_ids.clone(),
                            manifest: p.manifest.clone(),
                        },
                    };
                    let summary = outcome.text.clone();
                    let tone = outcome.tone;
                    self.notify_desktop(
                        if tone == Tone::Success {
                            "Import finished"
                        } else if p.phase == Phase::Cancelled {
                            "Import cancelled"
                        } else {
                            "Import needs attention"
                        },
                        &format!("{card} · {summary}"),
                    );
                    if here {
                        self.import_ui.outcome = Some(outcome);
                        self.import_ui.details_open = false;
                        self.import_ui.confirm = None;
                        // Committed groups left the selection; plan what remains.
                        if self.import_ui.plan.is_none() {
                            self.import_ui.expected_plan = None;
                            self.import_ui.dirty_at = Some(Instant::now());
                        }
                    }
                    if p.phase == Phase::Failed && (!here || !self.import_ui.open) {
                        // The panel holds the grouped errors; outside it, one
                        // line says what happened and where the details are.
                        self.announce_action(
                            format!("{card} · {summary}"),
                            Tone::Warning,
                            "Show Import",
                            Cmd::Ingest("show".into()),
                            "",
                        );
                    }
                }
                if p.phase == Phase::Completed && !was_completed {
                    let p = &self.import_ui.progress;
                    self.import_ui.done_source = p.source.clone();
                    let root = p.library_root.clone();
                    let source = p.source.clone();
                    // The verified-offload sentence is the one that earns trust:
                    // show it once, with a way to the new photos.
                    let text = if here {
                        self.status.clone()
                    } else {
                        format!("{card} · {}", self.status)
                    };
                    // Behind an open Import view on this card, the catalog moves
                    // to the new photos (seen when Import closes). Anywhere else
                    // the view stays where the user is; the notice offers them.
                    let behind_panel = self.import_ui.open && here;
                    match root.clone() {
                        Some(_) if !behind_panel => self.announce_action(
                            text,
                            Tone::Success,
                            "Show",
                            Cmd::Ingest("show_imported".into()),
                            "",
                        ),
                        Some(root) => self.announce_action(
                            text,
                            Tone::Success,
                            "Reveal",
                            Cmd::RevealPath(root),
                            "",
                        ),
                        None => self.announce(text, Tone::Success),
                    }
                    self.import_ui.last_root = root.clone();
                    if self.settings.eject_after_ingest
                        && let Some(source) = source
                    {
                        self.enqueue(IoCommand::Eject(source));
                    }
                    if behind_panel {
                        if let Some(root) = root {
                            self.scope = None;
                            self.mode = ViewMode::Grid;
                            self.pending_ingest_scope = Some(root);
                        }
                        self.ingest_scope_arrived();
                    }
                }
            }
            Event::Queue(queue) => {
                let grew = queue.len() > self.import_ui.queue.len();
                self.import_ui.queue = queue;
                if grew && let Some(job) = self.import_ui.queue.last() {
                    let name = job.source.file_name().map_or_else(
                        || job.source.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.announce(
                        format!(
                            "Queued {} from {name} · starts when the current import finishes",
                            plural(job.photos + job.videos, "photo", "photos")
                        ),
                        Tone::Info,
                    );
                }
            }
            Event::History(history) => self.import_ui.history = Arc::new(history),
            Event::Committed { source, groups } => {
                // A job for another card than the one shown changes nothing here.
                if source == self.import_ui.source {
                    self.import_ui.committed(groups);
                }
            }
            Event::Marked(count) => self.announce(
                format!(
                    "Marked {} as imported · source unchanged",
                    plural(count, "photo", "photos")
                ),
                Tone::Success,
            ),
            Event::Ejected(path) => self.announce(
                format!(
                    "Ejected {} · safe to remove",
                    path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned()
                    )
                ),
                Tone::Success,
            ),
            Event::Error(error) => {
                if self.import_ui.progress.phase == Phase::Queued {
                    self.import_ui.progress.phase = Phase::Failed;
                }
                self.error = Some(format!("Import: {error}").into());
                self.import_ui.scan_failed(error);
            }
        }
        self.palette_revision = self.palette_revision.wrapping_add(1);
    }
    /// Debounced replanning. Returns whether the view must repaint.
    pub(super) fn ingest_tick(&mut self) -> bool {
        if self.outbox.is_empty()
            && self.awaiting == 0
            && self.unsaved.is_empty()
            && !self.import_ui.scanning()
            && !self.import_ui.job_here()
            && self
                .import_ui
                .dirty_at
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(300))
            && self.import_ui.scan.is_some()
        {
            self.import_ui.dirty_at = None;
            let options = self.import_ui.options();
            self.import_ui.expected_plan = Some(options.clone());
            self.enqueue(IoCommand::IngestPlan(options));
        }
        false
    }
    pub(super) fn ingest_scope_arrived(&mut self) {
        if let Some(root) = &self.pending_ingest_scope {
            // Source selection changes once on arrival, never scans each frame.
            let root = root.to_string_lossy();
            if let Some(photo) = self
                .photos
                .iter()
                .find(|p| p.path.starts_with(root.as_ref()))
            {
                self.scope = Some(photo.folder_key());
                self.rebuild(None);
                self.pending_ingest_scope = None;
            }
        }
    }
    pub(super) fn ingest_api(
        &mut self,
        method: &str,
        params: &Value,
    ) -> std::result::Result<Value, String> {
        match method {
            "ingest_scan" => {
                let path = PathBuf::from(params["path"].as_str().ok_or("path required")?);
                // Another card scans while a job copies; the copying one never.
                if self.import_ui.active() && self.import_ui.progress.source.as_ref() == Some(&path)
                {
                    return Err("This source is importing now".into());
                }
                self.scan_for_import(path);
                if !self.import_ui.scanning() {
                    return Err(self
                        .import_ui
                        .error
                        .clone()
                        .unwrap_or_else(|| "Scan was not queued".into()));
                }
            }
            "ingest_plan" => {
                if !self.unsaved.is_empty() || self.awaiting > 0 || !self.outbox.is_empty() {
                    return Err("Wait for cull decisions to finish saving before planning".into());
                }
                if self.import_ui.scanning() {
                    return Err("Wait for the scan to finish".into());
                }
                let mut value = serde_json::to_value(IngestOptions::from_settings(&self.settings))
                    .map_err(|e| e.to_string())?;
                for (key, val) in params["options"].as_object().ok_or("options required")? {
                    value[key] = val.clone();
                }
                let options: IngestOptions =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                self.import_ui.expected_plan = Some(options.clone());
                self.import_ui.options = options.clone();
                self.import_ui.fields = [
                    options.library_root.to_string_lossy().into_owned(),
                    options.folder_template.clone(),
                    options.file_template.clone(),
                    options.job_code.clone(),
                    options
                        .backup_roots
                        .iter()
                        .map(|p| p.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(";"),
                ];
                self.import_ui.plan = None;
                self.import_ui.dirty_at = None;
                self.import_ui.error = None;
                if !self.enqueue(IoCommand::IngestPlan(options)) {
                    return Err("Plan was not queued".into());
                }
            }
            "ingest_start" => {
                if !self.unsaved.is_empty() || self.awaiting > 0 || !self.outbox.is_empty() {
                    return Err("Wait for cull decisions to finish saving before importing".into());
                }
                if self.import_ui.job_here() {
                    return Err("An import is already running".into());
                }
                if self.import_ui.active() && self.import_ui.queued(&self.import_ui.source) {
                    return Err("This source is already queued".into());
                }
                if !self
                    .import_ui
                    .plan
                    .as_ref()
                    .is_some_and(|p| p.ready && !p.plan.groups.is_empty())
                {
                    return Err("A valid nonempty import plan is required".into());
                }
                if self.import_ui.active() {
                    // Another card copies: the worker queues this one behind it
                    // and reports the queue; the running job's progress stays.
                    if !self.enqueue(IoCommand::IngestStart) {
                        return Err("Import was not queued".into());
                    }
                    return Ok(json!({"accepted":true,"queued":true,"poll":"ingest_status"}));
                }
                self.import_ui.progress = Progress {
                    phase: Phase::Queued,
                    source: Some(self.import_ui.source.clone()),
                    ..Default::default()
                };
                if !self.enqueue(IoCommand::IngestStart) {
                    self.import_ui.progress.phase = Phase::Failed;
                    return Err("Import was not queued".into());
                }
            }
            "ingest_status" => {
                let start = params["offset"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(usize::MAX as u64) as usize;
                let limit = params["limit"].as_u64().unwrap_or(100).min(1000) as usize;
                let items: Vec<_> = self.import_ui.items.iter().skip(start).take(limit).map(|g| json!({"id":g.id,"path":g.primary.path,"filename":g.photo.filename,"kind":g.kind,"size":g.bytes(),"capture_ms":g.capture_ms,"capture_kind":g.capture_kind,"selected":self.import_ui.selected.contains(&g.id),"seen":g.seen,"warning":g.warning,"companion":g.companion,"sidecars":g.sidecars})).collect();
                let plan = self.import_ui.plan.as_ref().map(|s| json!({"ready":s.ready,"error":s.error,"photos":s.plan.photos,"videos":s.plan.videos,"files":s.plan.files,"bytes":s.plan.bytes,"example":s.plan.example,"space":s.space,"options":s.plan.options,"examples":s.plan.groups.iter().take(5).map(|g| &g.files).collect::<Vec<_>>()}));
                return Ok(
                    json!({"scanning":self.import_ui.scanning(),"source":self.import_ui.source,"total":self.import_ui.items.len(),"items":items,"plan":plan,"progress":self.import_ui.progress,"queue":self.import_ui.queue,"error":self.import_ui.error,"history":self.import_ui.history.as_ref(),"warnings":self.import_ui.scan.as_ref().map(|s| &s.warnings),"clock":self.import_ui.scan.as_ref().map(|s| s.clock)}),
                );
            }
            "ingest_cancel" => {
                self.enqueue(IoCommand::IngestCancel);
            }
            "ingest_pause" => {
                self.enqueue(IoCommand::IngestPause);
            }
            "ingest_resume" => {
                self.enqueue(IoCommand::IngestResume);
            }
            "ingest_history" => {
                self.enqueue(IoCommand::IngestHistory(
                    params["limit"].as_u64().unwrap_or(100).min(1000) as usize,
                ));
            }
            "ingest_mark_imported" => {
                let ids = params["ids"]
                    .as_array()
                    .ok_or("ids required")?
                    .iter()
                    .map(|v| v.as_u64().map(|v| v as usize).ok_or("Invalid group ID"))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.enqueue(IoCommand::IngestMarkImported(ids));
            }
            "eject" => {
                self.enqueue(IoCommand::Eject(PathBuf::from(
                    params["path"].as_str().ok_or("path required")?,
                )));
            }
            _ => return Err("Unknown ingest method".into()),
        }
        Ok(json!({"accepted":true,"poll":"ingest_status"}))
    }
    pub(super) fn ingest_action(&mut self, action: &str, cx: &mut gpui::Context<Self>) {
        match action {
            "show" => {
                // History is a separate page; Show always returns to the panel.
                self.import_ui.show_history = false;
                if self
                    .preferences_draft
                    .as_ref()
                    .is_some_and(|draft| draft.changed())
                {
                    self.error = Some(
                        "Apply or cancel your preference changes before opening Import".into(),
                    );
                    return;
                }
                self.close_preferences();
                self.import_ui.open = true;
                if !self.import_ui.job_here() {
                    self.import_ui.changed();
                }
            }
            // From a completion notice: the imported photos, in the catalog.
            "show_imported" => {
                if let Some(root) = self.import_ui.last_root.clone() {
                    self.import_ui.open = false;
                    self.import_ui.show_history = false;
                    self.scope = None;
                    self.mode = ViewMode::Grid;
                    self.pending_ingest_scope = Some(root);
                    self.ingest_scope_arrived();
                }
            }
            "details" => self.import_ui.details_open = !self.import_ui.details_open,
            // Import exactly the photos that failed; verified ones stay skipped.
            "retry_failed" => {
                let failed: Vec<usize> = self
                    .import_ui
                    .outcome
                    .as_ref()
                    .map(|o| o.failed_ids.clone())
                    .unwrap_or_default();
                if self.import_ui.active() || failed.is_empty() {
                    return;
                }
                let ui = &mut self.import_ui;
                ui.selected.clear();
                ui.photos = 0;
                ui.videos = 0;
                ui.bytes = 0;
                for id in failed {
                    if let Some(&index) = ui.lookup.get(&id) {
                        let group = &ui.items[index];
                        if ui.selected.insert(id) {
                            if group.kind == "video" {
                                ui.videos += 1;
                            } else {
                                ui.photos += 1;
                            }
                            ui.bytes += group.bytes();
                        }
                    }
                }
                ui.range_base = None;
                ui.timeline_counts();
                ui.outcome = None;
                ui.details_open = false;
                ui.changed();
                ui.start_when_planned = !ui.selected.is_empty();
            }
            action if action.starts_with("history_row:") => {
                let id = action["history_row:".len()..].to_owned();
                if !self.import_ui.history_open.remove(&id) {
                    self.import_ui.history_open.insert(id);
                }
            }
            "notify:on" | "notify:off" => {
                self.settings.ingest_notify = action == "notify:on";
                self.save_settings();
            }
            "history" => {
                self.import_ui.show_history = true;
                self.import_ui.open = true;
                self.enqueue(IoCommand::IngestHistory(100));
            }
            "close" => {
                self.import_ui.show_history = false;
                self.import_ui.open = false;
                self.import_ui.menu = None;
                self.import_ui.active_field = None;
                self.import_ui.resizing = None;
            }
            "panel:photos" => {
                self.import_ui.panel = ImportPanel::Photos;
                self.import_ui.active_field = None;
            }
            "panel:timeline" => {
                self.import_ui.panel = ImportPanel::Timeline;
                self.import_ui.active_field = None;
            }
            "panel:destination" => self.import_ui.panel = ImportPanel::Destination,
            "plan" => {
                if !self.import_ui.job_here() {
                    let options = self.import_ui.options();
                    if let Err(error) = self.ingest_api("ingest_plan", &json!({"options": options}))
                    {
                        self.import_ui.error = Some(error);
                    }
                }
            }
            "all" | "none" => {
                // Check all adds the shown photos and leaves hidden ones as
                // they are; Uncheck all leaves out everything.
                if action == "none" {
                    self.import_ui.selected.clear();
                    self.import_ui.photos = 0;
                    self.import_ui.videos = 0;
                    self.import_ui.bytes = 0;
                } else {
                    for &index in &self.import_ui.visible {
                        let group = &self.import_ui.items[index];
                        if !self.import_ui.selected.insert(group.id) {
                            continue;
                        }
                        if group.kind == "video" {
                            self.import_ui.videos += 1;
                        } else {
                            self.import_ui.photos += 1;
                        }
                        self.import_ui.bytes += group.bytes();
                    }
                }
                self.import_ui.range_base = None;
                self.import_ui.timeline_counts();
                self.import_ui.changed();
            }
            action if action.starts_with("backup_remove:") => {
                if let Ok(index) = action[14..].parse::<usize>() {
                    let mut backups: Vec<&str> = self.import_ui.fields[4]
                        .split(';')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .collect();
                    if index < backups.len() {
                        backups.remove(index);
                        self.import_ui.fields[4] = backups.join(";");
                        self.import_ui.active_field = None;
                        self.import_ui.changed();
                    }
                }
            }
            "new" => {
                self.import_ui.new_only = !self.import_ui.new_only;
                self.import_ui.visible = self
                    .import_ui
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| !self.import_ui.new_only || g.seen.is_none())
                    .map(|(i, _)| i)
                    .collect();
            }
            "save_preset" => {
                let name = if self.import_ui.fields[3].trim().is_empty() {
                    "Custom"
                } else {
                    self.import_ui.fields[3].trim()
                }
                .to_string();
                self.settings.ingest_presets.insert(
                    name.clone(),
                    (
                        self.import_ui.fields[1].clone(),
                        self.import_ui.fields[2].clone(),
                    ),
                );
                self.import_ui.menu = None;
                self.save_settings();
                self.announce(format!("Saved naming preset “{name}”"), Tone::Success);
            }
            "videos:include" | "videos:skip" => {
                self.import_ui.options.include_videos = action == "videos:include";
                self.import_ui.changed();
            }
            "collision:suffix" | "collision:skip" => {
                self.import_ui.options.collision = action[10..].into();
                self.import_ui.changed();
            }
            "gap" => {
                self.settings.ingest_time_group_minutes =
                    match self.settings.ingest_time_group_minutes {
                        15 => 30,
                        30 => 60,
                        60 => 120,
                        _ => 15,
                    };
                if let Some(scan) = &self.import_ui.scan {
                    self.import_ui.timeline =
                        ingest::timeline(scan, self.settings.ingest_time_group_minutes);
                    self.import_ui.timeline_counts();
                }
                self.save_settings();
            }
            "cull" => {
                self.import_ui.open = false;
                self.import_ui.active_field = None;
                self.enqueue(IoCommand::Import(self.import_ui.source.clone()));
                self.pending_ingest_scope = Some(self.import_ui.source.clone());
                self.ingest_scope_arrived();
                self.mode = ViewMode::Grid;
                self.announce_action(
                    "Cull in place, then import Picks, Except rejects or Rated",
                    Tone::Info,
                    "Show Import",
                    Cmd::Ingest("show".into()),
                    "",
                );
            }
            "picks" => {
                self.import_ui.open = true;
                self.import_ui.options.selection = "picks".into();
                self.import_ui.changed();
            }
            action if action.starts_with("selection:") => {
                if let Some((value, _)) = SELECTIONS.iter().find(|(v, _)| *v == &action[10..]) {
                    self.import_ui.options.selection = (*value).into();
                    self.import_ui.menu = None;
                    self.import_ui.changed();
                }
            }
            action if action.starts_with("rating:") => {
                if let Ok(rating) = action[7..].parse::<u8>() {
                    self.import_ui.options.min_rating = rating.clamp(1, 5);
                    self.import_ui.changed();
                }
            }
            action if action.starts_with("token:") => {
                let field = match self.import_ui.active_field {
                    Some(field @ (1 | 2)) => field,
                    _ => {
                        let field = self.import_ui.template_field;
                        self.import_ui.active_field = Some(field);
                        self.text_edit.focus(&self.import_ui.fields[field]);
                        field
                    }
                };
                self.import_ui.template_field = field;
                self.text_edit
                    .insert(&mut self.import_ui.fields[field], &action[6..]);
                self.import_ui.changed();
            }
            "mark" => {
                // Recording photos as imported without copying them is how a
                // never-copied photo gets left on a card: ask once, inline.
                if self.import_ui.selected.is_empty() {
                    self.announce("Select photos to mark as imported", Tone::Warning);
                } else if self.import_ui.confirming(IngestConfirm::Mark) {
                    self.import_ui.confirm = None;
                    self.enqueue(IoCommand::IngestMarkImported(
                        self.import_ui.selected.iter().copied().collect(),
                    ));
                } else {
                    self.import_ui.arm(IngestConfirm::Mark);
                }
            }
            "mark:keep" | "cancel:keep" => self.import_ui.confirm = None,
            // From the palette: the same question as the button, in the
            // Import view where it is answered.
            "cancel_now" => {
                if self.import_ui.active() {
                    self.import_ui.open = true;
                    self.import_ui.arm(IngestConfirm::Cancel);
                } else {
                    self.announce("No import is running", Tone::Info);
                }
            }
            "remaining" => {
                self.import_ui.outcome = None;
                self.ingest_action("start", cx);
            }
            "dismiss_outcome" => self.import_ui.outcome = None,
            "tokens" => self.import_ui.tokens_open = !self.import_ui.tokens_open,
            "focus_job" => {
                self.import_ui.menu = None;
                self.import_ui.active_field = Some(3);
                if self.ingest_layout().destination == 0. {
                    self.import_ui.panel = ImportPanel::Destination;
                }
                self.text_edit.focus(&self.import_ui.fields[3]);
            }
            action if action.starts_with("gap:") => {
                self.import_ui.menu = None;
                if let Ok(minutes) = action[4..].parse::<u32>() {
                    self.settings.ingest_time_group_minutes = minutes;
                    if let Some(scan) = &self.import_ui.scan {
                        self.import_ui.timeline = ingest::timeline(scan, minutes);
                        self.import_ui.timeline_counts();
                    }
                    self.save_settings();
                }
            }
            "show:all" | "show:new" => {
                if self.import_ui.new_only != (action == "show:new") {
                    self.ingest_action("new", cx);
                }
            }
            action if action.starts_with("insert:") => {
                self.settings.ingest_on_insert = action[7..].into();
                self.save_settings();
            }
            action if action.starts_with("day:") => {
                if let Ok(hour) = action[4..].parse::<u8>() {
                    self.settings.ingest_day_start_hour = hour % 24;
                    self.import_ui.options.day_start_hour = self.settings.ingest_day_start_hour;
                    self.import_ui.changed();
                    self.save_settings();
                }
            }
            "auto_eject:on" | "auto_eject:off" => {
                self.settings.eject_after_ingest = action == "auto_eject:on";
                self.save_settings();
            }
            "save" => {
                let options = self.import_ui.options();
                self.settings.ingest_library_root = options.library_root;
                self.settings.ingest_folder_template = options.folder_template;
                self.settings.ingest_file_template = options.file_template;
                self.settings.ingest_backup_roots = options.backup_roots;
                self.settings.ingest_include_videos = options.include_videos;
                self.settings.ingest_collision = options.collision;
                self.settings
                    .ingest_job_codes
                    .retain(|code| code != &options.job_code);
                if !options.job_code.trim().is_empty() {
                    self.settings.ingest_job_codes.insert(0, options.job_code);
                }
                self.save_settings();
                self.announce("Import defaults saved", Tone::Success);
            }
            "start" => {
                self.import_ui.outcome = None;
                self.import_ui.confirm = None;
                if let Err(error) = self.ingest_api("ingest_start", &json!({})) {
                    self.error = Some(error.into());
                }
            }
            "pause" if !self.import_ui.active() => {
                self.announce("No import is running", Tone::Info);
            }
            "pause" => {
                self.enqueue(if self.import_ui.progress.phase == Phase::Paused {
                    IoCommand::IngestResume
                } else {
                    IoCommand::IngestPause
                });
            }
            "cancel" => {
                // Cancelling a long copy by accident is costly: the button asks
                // once ("Cancel import?") and the confirm button cancels.
                if self.import_ui.confirming(IngestConfirm::Cancel) {
                    self.import_ui.confirm = None;
                    self.enqueue(IoCommand::IngestCancel);
                } else if self.import_ui.active() {
                    self.import_ui.arm(IngestConfirm::Cancel);
                }
            }
            "eject" => {
                self.enqueue(IoCommand::Eject(self.import_ui.source.clone()));
            }
            "insert" => {
                self.settings.ingest_on_insert = match self.settings.ingest_on_insert.as_str() {
                    "ask" => "auto",
                    "auto" => "index_in_place",
                    "index_in_place" => "ignore",
                    _ => "ask",
                }
                .into();
                self.save_settings();
            }
            "day" => {
                self.settings.ingest_day_start_hour =
                    (self.settings.ingest_day_start_hour + 1) % 24;
                self.import_ui.options.day_start_hour = self.settings.ingest_day_start_hour;
                self.import_ui.changed();
                self.save_settings();
            }
            "auto_eject" => {
                self.settings.eject_after_ingest = !self.settings.eject_after_ingest;
                self.save_settings();
            }
            "remember" => {
                if let Some(scan) = &self.import_ui.scan {
                    let volume = scan.volume.clone();
                    let rule = if self
                        .settings
                        .ingest_volumes
                        .get(&volume)
                        .is_some_and(|r| r == "deny")
                    {
                        "allow"
                    } else {
                        "deny"
                    };
                    self.settings.ingest_volumes.insert(volume, rule.into());
                    self.save_settings();
                }
            }
            action if action.starts_with("preset:") => {
                if let Ok(index) = action[7..].parse::<usize>() {
                    if let Some((_, folder, file)) = ingest::naming::PRESETS.get(index) {
                        self.import_ui.fields[1] = folder.to_string();
                        self.import_ui.fields[2] = file.to_string();
                    }
                    self.import_ui.menu = None;
                    self.refocus_ingest_field();
                    self.import_ui.changed();
                }
            }
            action if action.starts_with("custom:") => {
                if let Some((folder, file)) = self.settings.ingest_presets.get(&action[7..]) {
                    self.import_ui.fields[1] = folder.clone();
                    self.import_ui.fields[2] = file.clone();
                    self.import_ui.menu = None;
                    self.refocus_ingest_field();
                    self.import_ui.changed();
                }
            }
            action if action.starts_with("job:") => {
                self.import_ui.fields[3] = action[4..].to_string();
                self.import_ui.active_field = None;
                self.import_ui.changed();
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn ingest_key(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.import_ui.open {
            return false;
        }
        if let Some(menu) = self.import_ui.menu {
            let options = self.ingest_menu_options(menu);
            let m = event.keystroke.modifiers;
            if !m.control && !m.platform && !m.alt {
                match event.keystroke.key.as_str() {
                    "up" => self.import_ui.menu_index = self.import_ui.menu_index.saturating_sub(1),
                    "down" => {
                        self.import_ui.menu_index =
                            (self.import_ui.menu_index + 1).min(options.len().saturating_sub(1))
                    }
                    "enter" | "space" => {
                        if let Some(option) = options.get(self.import_ui.menu_index) {
                            let action = option.action.clone();
                            self.ingest_action(&action, cx);
                        }
                    }
                    _ => {}
                }
                cx.notify();
                return true;
            }
        }
        // An open confirmation answers Enter (the ringed button), with
        // Tab or the arrows moving between its two buttons.
        if let Some((what, _)) = self.import_ui.confirm {
            let m = event.keystroke.modifiers;
            if !m.control && !m.platform && !m.alt {
                match event.keystroke.key.as_str() {
                    "tab" | "left" | "right" => {
                        self.import_ui.confirm_keep = !self.import_ui.confirm_keep;
                        cx.notify();
                        return true;
                    }
                    "enter" | "space" => {
                        let action = match (what, self.import_ui.confirm_keep) {
                            (IngestConfirm::Mark, false) => "mark",
                            (IngestConfirm::Mark, true) => "mark:keep",
                            (IngestConfirm::Cancel, false) => "cancel",
                            (IngestConfirm::Cancel, true) => "cancel:keep",
                        };
                        self.ingest_action(action, cx);
                        return true;
                    }
                    _ => {}
                }
            }
        }
        if let Some(field) = self.import_ui.active_field {
            let layout = self.ingest_layout();
            let hidden = if layout.destination == 0. {
                self.import_ui.panel != ImportPanel::Destination
            } else {
                layout.timeline == 0. && self.import_ui.panel == ImportPanel::Timeline
            };
            if hidden {
                self.import_ui.active_field = None;
                return true;
            }
            let key = event.keystroke.key.as_str();
            let modifiers = event.keystroke.modifiers;
            let primary = modifiers.control || modifiers.platform;
            if key == "tab" {
                let next = next_field(field, modifiers.shift);
                self.import_ui.active_field = Some(next);
                self.text_edit.focus(&self.import_ui.fields[next]);
            } else if primary && key == "enter" {
                // Ctrl/Cmd+Return starts the import from anywhere in the view.
                self.import_ui.active_field = None;
                self.ingest_action("start", cx);
            } else if key == "enter" {
                self.import_ui.active_field = None;
            } else if self
                .text_edit
                .key(&mut self.import_ui.fields[field], event, cx)
            {
                self.import_ui.changed();
            }
            cx.notify();
        } else {
            let key = event.keystroke.key.as_str();
            let m = event.keystroke.modifiers;
            let primary = m.control || m.platform;
            let plain = !primary && !m.alt;
            if primary && key == "enter" {
                if !self.import_ui.job_here() {
                    self.ingest_action("start", cx);
                }
            } else if primary && key == "a" {
                self.ingest_action("all", cx);
            } else if plain && key == "tab" {
                // Tab moves from the grid into the fields (Job code first).
                let field = if m.shift {
                    FIELD_ORDER[4]
                } else {
                    FIELD_ORDER[0]
                };
                if self.ingest_layout().destination == 0. {
                    self.import_ui.panel = ImportPanel::Destination;
                }
                self.import_ui.menu = None;
                self.import_ui.active_field = Some(field);
                self.text_edit.focus(&self.import_ui.fields[field]);
                cx.notify();
            } else if plain && !m.shift && key == "j" {
                self.ingest_action("focus_job", cx);
            } else if plain && !m.shift && key == "space" {
                let columns = self.ingest_columns();
                if let Some(target) = self.import_ui.toggle_focused() {
                    self.import_ui
                        .scroll
                        .scroll_to_item(target / columns, ScrollStrategy::Center);
                }
                cx.notify();
            } else if plain && !self.import_ui.visible.is_empty() {
                let columns = self.ingest_columns();
                let Some(target) = self.import_ui.navigate(key, columns, m.shift) else {
                    return true;
                };
                self.import_ui
                    .scroll
                    .scroll_to_item(target / columns, ScrollStrategy::Center);
                cx.notify();
            }
        }
        true
    }
    /// `tip` for text that is already shared, without copying it per frame.
    fn shared_tip(
        &self,
        label: SharedString,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let tip = Tip {
            label,
            keys: SharedString::default(),
            font: self.settings.interface_font.clone().into(),
            fg: self.text(),
            muted: self.muted(),
            bg: self.c("elevated_surface.background", "#1b1b1b"),
            border: self.c("border.variant", "#333333"),
        };
        move |_, cx| cx.new(|_| tip.clone()).into()
    }
    /// Scrolls the Import grid to a group's tile (timeline clicks).
    fn ingest_scroll_to_group(&mut self, id: Option<usize>) {
        let position = id
            .and_then(|id| self.import_ui.lookup.get(&id))
            .and_then(|index| self.import_ui.visible.iter().position(|i| i == index));
        if let Some(position) = position {
            self.import_ui
                .scroll
                .scroll_to_item(position / self.ingest_columns(), ScrollStrategy::Top);
        }
    }
    fn ingest_layout(&self) -> ImportLayout {
        ImportLayout::new(
            self.center_size.0,
            self.import_ui.timeline_width,
            self.import_ui.destination_width,
        )
    }
    fn ingest_columns(&self) -> usize {
        self.ingest_layout().columns()
    }
    fn ingest_grid(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let columns = self.ingest_columns();
        let rows = self.import_ui.visible.len().div_ceil(columns);
        let list = uniform_list(
            "ingest-grid",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                if this.import_ui.range != Some((range.start, range.end)) {
                    this.import_ui.range = Some((range.start, range.end));
                    this.media.cancel_pending();
                }
                // Lend the previews out, like the catalog grid: texture_photo needs
                // `&mut self` but never reads them synchronously.
                let previews = std::mem::take(&mut this.import_ui.previews);
                let rows = range
                    .map(|row| {
                        let mut line = div()
                            .h(px(184.))
                            .w_full()
                            .flex()
                            .gap(px(space::SM))
                            .px(px(ui::BAR_INSET))
                            .pt(px(space::SM));
                        for col in 0..columns {
                            if let Some(&index) = this.import_ui.visible.get(row * columns + col) {
                                // Shared text only: no group/photo clone or formatting.
                                let cell = this.import_ui.cells[index].clone();
                                let selected = this.import_ui.selected.contains(&cell.id);
                                let texture = if cell.has_preview {
                                    let photo = &previews[index];
                                    this.request_preview(photo, PreviewKind::Thumbnail);
                                    this.texture_photo(photo, PreviewKind::Thumbnail, cx)
                                } else {
                                    None
                                };
                                let mut image = div()
                                    .w_full()
                                    .h(px(120.))
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
                                    image = image.child(icon("camera", this.icon_muted()));
                                }
                                if let Some(badge) = cell.badge {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .bottom(px(3.))
                                            .right(px(4.))
                                            .px(px(space::XS))
                                            .bg(this.scrim())
                                            .text_color(this.scrim_text())
                                            .text_size(px(ui::text::MICRO))
                                            .child(badge),
                                    );
                                }
                                // Imported in this job: done and verified, so a
                                // success check at full strength. Imported before:
                                // a dated chip on a receded tile.
                                let recent = this.import_ui.recent.contains(&cell.id);
                                if recent {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .top(px(3.))
                                            .left(px(4.))
                                            .flex()
                                            .items_center()
                                            .gap(px(2.))
                                            .px(px(space::XS))
                                            .bg(this.scrim())
                                            .text_color(this.scrim_text())
                                            .text_size(px(ui::text::MICRO))
                                            .child(
                                                icon("check", this.c("success", "#7fbf7f"))
                                                    .size(px(12.)),
                                            )
                                            .child("Verified"),
                                    );
                                } else if let Some(seen) = cell.seen_label.clone() {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .top(px(3.))
                                            .left(px(4.))
                                            .px(px(space::XS))
                                            .bg(this.scrim())
                                            .text_color(this.scrim_text())
                                            .text_size(px(ui::text::MICRO))
                                            .child(seen),
                                    );
                                }
                                let id = cell.id;
                                let current = this.import_ui.focused == Some(id);
                                let position = row * columns + col;
                                line = line.child(
                                    div()
                                        .id(("ingest-item", index))
                                        .relative()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .border_1()
                                        // A hairline either way: the checkbox, text and
                                        // image strength say "included"; the accent marks
                                        // only the current tile.
                                        .border_color(this.border())
                                        .when(cell.seen && !recent, |s| s.opacity(0.45))
                                        .cursor_pointer()
                                        .tooltip(this.shared_tip(cell.tip))
                                        .child(
                                            image.when(!selected && !cell.seen, |s| s.opacity(0.6)),
                                        )
                                        .child(
                                            div()
                                                .px(px(space::SM))
                                                .py(px(space::XS))
                                                .flex()
                                                .gap(px(space::XS))
                                                .text_color(if selected {
                                                    this.text()
                                                } else {
                                                    this.muted()
                                                })
                                                .child(
                                                    icon(
                                                        if selected {
                                                            "selected"
                                                        } else {
                                                            "select"
                                                        },
                                                        if selected {
                                                            this.text()
                                                        } else {
                                                            this.icon_muted()
                                                        },
                                                    )
                                                    .id(("ingest-checkbox", index))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(
                                                        move |this, event: &ClickEvent, _, cx| {
                                                            this.import_ui.active_field = None;
                                                            this.import_ui.select_position(
                                                                position,
                                                                if event.modifiers().shift {
                                                                    GridPick::Range
                                                                } else {
                                                                    GridPick::Toggle
                                                                },
                                                            );
                                                            cx.stop_propagation();
                                                            cx.notify();
                                                        },
                                                    )),
                                                )
                                                .child(
                                                    div()
                                                        .min_w(px(0.))
                                                        .text_ellipsis()
                                                        .whitespace_normal()
                                                        .line_clamp(1)
                                                        .overflow_hidden()
                                                        .child(cell.name),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .px(px(space::SM))
                                                .text_size(px(ui::text::LABEL))
                                                .text_color(this.muted())
                                                .child(cell.detail),
                                        )
                                        .when(current, |s| {
                                            s.child(
                                                div()
                                                    .absolute()
                                                    .inset_0()
                                                    .border_2()
                                                    .border_color(this.accent()),
                                            )
                                        })
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, _, cx| {
                                                this.import_ui.active_field = None;
                                                let m = event.modifiers();
                                                this.import_ui.select_position(
                                                    position,
                                                    if m.shift {
                                                        GridPick::Range
                                                    } else if m.control || m.platform {
                                                        GridPick::Toggle
                                                    } else {
                                                        GridPick::Focus
                                                    },
                                                );
                                                cx.notify();
                                            },
                                        )),
                                );
                            } else {
                                line = line.child(div().flex_1());
                            }
                        }
                        line
                    })
                    .collect();
                this.import_ui.previews = previews;
                rows
            }),
        )
        .track_scroll(self.import_ui.scroll.clone())
        .size_full()
        .into_any_element();
        self.scrollable_list("ingest-scrollbar", &self.import_ui.scroll, list, cx)
    }
    fn ingest_panel_handle(&self, panel: ImportPanel, cx: &gpui::Context<Self>) -> AnyElement {
        let hover = self.hover_bg();
        div()
            .id(if panel == ImportPanel::Timeline {
                "ingest-timeline-resize"
            } else {
                "ingest-destination-resize"
            })
            .w(px(IMPORT_RESIZE_HANDLE))
            .h_full()
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(move |s| s.bg(hover))
            .tooltip(self.tip("Drag to resize; double-click to reset", ""))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let layout = this.ingest_layout();
                    let width = if panel == ImportPanel::Timeline {
                        layout.timeline
                    } else {
                        layout.destination
                    };
                    let preferred = if panel == ImportPanel::Timeline {
                        this.import_ui.timeline_width
                    } else {
                        this.import_ui.destination_width
                    };
                    this.import_ui.resizing =
                        Some((panel, f32::from(event.position.x), width, preferred));
                    this.import_ui.resize_moved = false;
                    this.import_ui.suppress_resize_click = false;
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if this.import_ui.suppress_resize_click {
                    this.import_ui.suppress_resize_click = false;
                    return;
                }
                if event.click_count() == 2 {
                    if panel == ImportPanel::Timeline {
                        this.import_ui.timeline_width = ui::INGEST_TIMELINE_WIDTH;
                    } else {
                        this.import_ui.destination_width = ui::INGEST_DESTINATION_WIDTH;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.move_ingest_resize(event, cx)
                }),
            )
            .into_any_element()
    }
    fn ingest_preset_label(&self) -> String {
        let (folder, file) = (&self.import_ui.fields[1], &self.import_ui.fields[2]);
        ingest::naming::PRESETS
            .iter()
            .find(|(_, f, n)| f == folder && n == file)
            .map(|(label, _, _)| label.to_string())
            .or_else(|| {
                self.settings
                    .ingest_presets
                    .iter()
                    .find(|(_, (f, n))| f == folder && n == file)
                    .map(|(label, _)| label.clone())
            })
            .unwrap_or_else(|| "Custom".into())
    }
    fn ingest_menu_options(&self, menu: IngestMenu) -> Vec<MenuOption> {
        match menu {
            IngestMenu::Preset => {
                let current = self.ingest_preset_label();
                let mut options: Vec<_> = ingest::naming::PRESETS
                    .iter()
                    .enumerate()
                    .map(|(index, (label, folder, file))| MenuOption {
                        label: label.to_string(),
                        detail: preset_detail(folder, file),
                        action: format!("preset:{index}"),
                        active: current == *label,
                    })
                    .collect();
                options.extend(self.settings.ingest_presets.iter().map(
                    |(label, (folder, file))| MenuOption {
                        label: label.clone(),
                        detail: preset_detail(folder, file),
                        action: format!("custom:{label}"),
                        active: current == *label,
                    },
                ));
                let name = match self.import_ui.fields[3].trim() {
                    "" => "Custom",
                    code => code,
                };
                options.push(MenuOption {
                    label: format!("Save current as “{name}”"),
                    detail: "Named after the job code".into(),
                    action: "save_preset".into(),
                    active: false,
                });
                options
            }
            IngestMenu::Selection => SELECTIONS
                .iter()
                .map(|(value, label)| MenuOption {
                    label: label.to_string(),
                    detail: String::new(),
                    action: format!("selection:{value}"),
                    active: self.import_ui.options.selection == *value,
                })
                .collect(),
            IngestMenu::Gap => [15u32, 30, 60, 120]
                .into_iter()
                .map(|minutes| MenuOption {
                    label: format!("Split sessions after {minutes} min"),
                    detail: "Photos closer together than this stay in one session".into(),
                    action: format!("gap:{minutes}"),
                    active: self.settings.ingest_time_group_minutes == minutes,
                })
                .collect(),
        }
    }
    fn ingest_heading(&self, label: &str, first: bool) -> Div {
        div()
            .flex_shrink_0()
            .px(px(space::SM))
            .when(!first, |s| {
                s.mt(px(space::SM))
                    .pt(px(space::LG))
                    .border_t_1()
                    .border_color(self.border())
            })
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.muted())
            .child(label.to_uppercase())
    }
    fn ingest_label(&self, label: &str) -> Div {
        div()
            .text_size(px(11.))
            .text_color(self.muted())
            .child(label.to_owned())
    }
    fn ingest_hint(&self, hint: &str) -> Div {
        div()
            .text_size(px(ui::text::LABEL))
            .line_height(px(14.))
            .text_color(self.muted())
            .child(hint.to_owned())
    }
    fn ingest_field(&self, n: usize, label: &str, cx: &gpui::Context<Self>) -> Div {
        let active = self.import_ui.active_field == Some(n);
        let text = &self.import_ui.fields[n];
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w(px(0.))
            .px(px(space::SM))
            .gap(px(space::XS))
            .child(self.ingest_label(label))
            .child(
                div()
                    .id(("ingest-field", n))
                    .h(px(ui::FIELD_HEIGHT))
                    .flex_shrink_0()
                    .min_w(px(0.))
                    .p(px(ui::FIELD_INSET))
                    .border_1()
                    .border_color(if active {
                        self.accent()
                    } else {
                        self.control_border()
                    })
                    .bg(self.bg())
                    .overflow_hidden()
                    .cursor(CursorStyle::IBeam)
                    .child(self.input_text(
                        &if (n == 0 || n == 4) && !active {
                            // Paths keep their root and last folders: "/home/…/Pictures/Library".
                            text.split(';')
                                .map(|p| copy::middle_path(p.trim(), self.ingest_field_chars()))
                                .collect::<Vec<_>>()
                                .join("; ")
                        } else {
                            text.clone()
                        },
                        match n {
                            3 => "e.g. Iceland-2026",
                            4 => "None",
                            _ => "",
                        },
                        active,
                    ))
                    .tooltip(self.tip(text, ""))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.import_ui.menu = None;
                        this.import_ui.active_field = Some(n);
                        if n == 1 || n == 2 {
                            this.import_ui.template_field = n;
                        }
                        this.text_edit.focus(&this.import_ui.fields[n]);
                        cx.notify();
                    })),
            )
    }
    fn ingest_link(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        cx: &gpui::Context<Self>,
        on_click: impl Fn(&mut Self, &mut gpui::Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let hover = self.text();
        div()
            .id(id)
            .flex_shrink_0()
            .cursor_pointer()
            .text_size(px(11.))
            .text_color(self.muted())
            .hover(move |s| s.text_color(hover))
            .child(label.to_owned())
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
    }
    /// Field-styled trigger whose menu drops below it, floating above the panel.
    fn ingest_dropdown(
        &self,
        menu: IngestMenu,
        label: &str,
        value: &str,
        width: f32,
        cx: &gpui::Context<Self>,
    ) -> Div {
        let open = self.import_ui.menu == Some(menu);
        let hover = self.hover_bg();
        let id = match menu {
            IngestMenu::Preset => "ingest-preset",
            IngestMenu::Selection => "ingest-selection",
            IngestMenu::Gap => "ingest-gap",
        };
        let trigger = div()
            .id(id)
            .h(px(ui::FIELD_HEIGHT))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::SM))
            .px(px(ui::FIELD_INSET))
            .border_1()
            .border_color(if open { self.accent() } else { self.border() })
            .bg(self.bg())
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(
                div()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(value.to_owned()),
            )
            .child(icon("down", self.icon_muted()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let dismissed = this
                        .import_ui
                        .menu_dismissed
                        .take()
                        .is_some_and(|(m, at)| m == menu && at == event.position);
                    if dismissed || this.import_ui.menu == Some(menu) {
                        this.import_ui.menu = None;
                    } else {
                        this.import_ui.menu = Some(menu);
                        this.import_ui.active_field = None;
                        this.import_ui.menu_index = this
                            .ingest_menu_options(menu)
                            .iter()
                            .position(|o| o.active)
                            .unwrap_or(0);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        let mut column = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w(px(0.))
            .px(px(space::SM))
            .gap(px(space::XS))
            .when(!label.is_empty(), |s| s.child(self.ingest_label(label)))
            .child(trigger);
        if open {
            let mut list = div()
                .id("ingest-menu")
                .w(px(width))
                .max_h(px(360.))
                .overflow_y_scroll()
                .mt(px(space::XS))
                .p(px(space::XS))
                .flex()
                .flex_col()
                .bg(self.c("elevated_surface.background", "#202020"))
                .border_1()
                .border_color(self.c("border.variant", "#333333"))
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if this.import_ui.menu == Some(menu) {
                        this.import_ui.menu = None;
                        this.import_ui.menu_dismissed = Some((menu, event.position));
                        cx.notify();
                    }
                }));
            let selected_bg = self.selected_bg();
            for (index, option) in self.ingest_menu_options(menu).into_iter().enumerate() {
                let action = option.action.clone();
                let highlighted = self.import_ui.menu_index == index;
                list = list.child(
                    div()
                        .id(("ingest-menu-option", index))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .px(px(space::SM))
                        .py(px(space::XS + 1.))
                        .min_h(px(ui::CONTROL_HEIGHT))
                        .cursor_pointer()
                        .when(highlighted, |s| s.bg(selected_bg))
                        .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                            if this.import_ui.menu_index != index {
                                this.import_ui.menu_index = index;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .w(px(16.))
                                .flex_shrink_0()
                                .when(option.active, |s| s.child(icon("check", self.accent()))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(option.label)
                                .when(!option.detail.is_empty(), |s| {
                                    s.child(
                                        div()
                                            .text_size(px(ui::text::LABEL))
                                            .text_color(self.muted())
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .child(option.detail),
                                    )
                                }),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ingest_action(&action, cx);
                        })),
                );
            }
            column = column.child(
                div().relative().w_full().h(px(0.)).child(
                    deferred(
                        anchored()
                            .anchor(Corner::TopLeft)
                            .snap_to_window_with_margin(px(space::SM))
                            .child(list),
                    )
                    .with_priority(1),
                ),
            );
        }
        column
    }
    /// Clickable naming reference; each token inserts at the template caret.
    fn ingest_tokens(&self, cx: &gpui::Context<Self>) -> Div {
        let hover = self.hover_bg();
        let target = match self.import_ui.active_field {
            Some(field @ (1 | 2)) => field,
            _ => self.import_ui.template_field,
        };
        let mut reference = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w(px(0.))
            .px(px(space::SM))
            .gap(px(space::XS))
            .child(self.ingest_label(&format!(
                "Tokens · click to insert into {}",
                if target == 1 { "subfolder" } else { "filename" }
            )));
        let mut group_row: Option<(&str, Div)> = None;
        let mut rows: Vec<(&str, Div)> = Vec::new();
        for (index, &(group, token, meaning)) in ingest::naming::TOKENS.iter().enumerate() {
            if group_row.as_ref().is_none_or(|(g, _)| *g != group) {
                rows.extend(group_row.take());
                group_row = Some((
                    group,
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_wrap()
                        .gap(px(space::XS)),
                ));
            }
            let chip = div()
                .id(("ingest-token", index))
                .h(px(20.))
                .px(px(space::XS))
                .flex()
                .items_center()
                .border_1()
                .border_color(self.border())
                .text_size(px(11.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(self.tip(meaning, ""))
                .child(token)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ingest_action(&format!("token:{token}"), cx);
                }));
            if let Some((_, row)) = group_row.take() {
                group_row = Some((group, row.child(chip)));
            }
        }
        rows.extend(group_row);
        for (group, chips) in rows {
            reference = reference.child(
                div()
                    .flex()
                    .gap(px(space::XS))
                    .child(
                        div()
                            .w(px(40.))
                            .h(px(20.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .text_size(px(ui::text::LABEL))
                            .text_color(self.muted())
                            .child(group),
                    )
                    .child(chips),
            );
        }
        reference
    }
    fn ingest_choice_row(
        &self,
        label: &str,
        id: &str,
        options: impl IntoIterator<Item = (String, Cmd, bool)>,
        cx: &gpui::Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap(px(space::SM))
            .px(px(space::SM))
            .child(self.ingest_label(label))
            .child(self.choices(id, options, cx))
    }
    /// Characters a path field shows before it is shortened in the middle.
    fn ingest_field_chars(&self) -> usize {
        let layout = self.ingest_layout();
        let width = if layout.destination == 0. {
            self.center_size.0
        } else {
            layout.destination
        };
        ((width - 2. * (ui::BAR_INSET + space::SM + ui::FIELD_INSET)) / 7.4).max(16.) as usize
    }
    /// Display name of the volume holding `root`: a mounted drive's name,
    /// else "this disk" (never the folder's own name, which reads as a disk
    /// called "Library").
    fn volume_name(&self, root: &std::path::Path) -> String {
        self.drives
            .iter()
            .filter(|drive| root.starts_with(&drive.path))
            .max_by_key(|drive| drive.path.as_os_str().len())
            .map(|drive| drive.name.clone())
            .unwrap_or_else(|| "this disk".into())
    }
    /// One destination (library or backup): its path, what this import needs
    /// there and the space left, with a bar for this import's share.
    fn ingest_space_row(
        &self,
        index: usize,
        capacity: &ingest::SpaceProjection,
        labeled: bool,
        cx: &gpui::Context<Self>,
    ) -> Div {
        let error = self.c("error", "#d46a6a");
        let volume = self.volume_name(&capacity.root);
        let text = if capacity.sufficient {
            format!(
                "Needs {} · {} free on {volume} ({} after)",
                copy::fmt_bytes(capacity.filesystem_required),
                copy::fmt_bytes(capacity.available),
                copy::fmt_bytes(capacity.remaining)
            )
        } else {
            format!(
                "Not enough space on {volume}: needs {}, {} free",
                copy::fmt_bytes(capacity.filesystem_required),
                copy::fmt_bytes(capacity.available)
            )
        };
        let share = (capacity.filesystem_required as f64 / capacity.available.max(1) as f64)
            .clamp(0.01, 1.) as f32;
        let path = capacity.root.to_string_lossy();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .px(px(space::SM))
            .gap(px(space::XS))
            // The library's path is already in its field above: name each
            // destination only once backups make the rows ambiguous.
            .when(labeled, |s| {
                s.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::XS))
                        .child(
                            div()
                                .id(("ingest-space-path", index))
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(px(ui::text::LABEL))
                                .child(format!(
                                    "{} · {}",
                                    if index == 0 { "Library" } else { "Backup" },
                                    copy::middle_path(
                                        &path,
                                        self.ingest_field_chars().saturating_sub(10)
                                    )
                                ))
                                .tooltip(self.tip(&path, "")),
                        )
                        .when(index > 0, |s| {
                            s.child(
                                self.compact_button(
                                    ("ingest-backup-remove", index),
                                    "",
                                    "close",
                                    Cmd::Ingest(format!("backup_remove:{}", index - 1)),
                                    false,
                                    cx,
                                )
                                .tooltip(self.tip("Remove this backup destination", "")),
                            )
                        }),
                )
            })
            .child(
                div()
                    .text_size(px(ui::text::LABEL))
                    .text_color(if capacity.sufficient {
                        self.muted()
                    } else {
                        error
                    })
                    .child(text),
            )
            .child(
                // A neutral track with this import's share of the free space.
                div().h(px(3.)).w_full().bg(self.border()).child(
                    div()
                        .h_full()
                        .w(relative(share))
                        .bg(if capacity.sufficient {
                            self.accent()
                        } else {
                            error
                        }),
                ),
            )
    }
    fn ingest_destination(&self, width: f32, cx: &gpui::Context<Self>) -> AnyElement {
        let inner = (width - 2. * (ui::BAR_INSET + space::SM)).max(160.);
        let options = &self.import_ui.options;
        // The per-shoot field first: everything else rarely changes.
        let mut job = self.ingest_field(3, "Job code", cx);
        if !self.settings.ingest_job_codes.is_empty() {
            let mut recent = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(space::XS))
                .child(self.ingest_hint("Recent"));
            for (index, code) in self.settings.ingest_job_codes.iter().take(5).enumerate() {
                let action = format!("job:{code}");
                recent = recent.child(
                    self.button(
                        ("ingest-job", index),
                        code,
                        "",
                        Cmd::Ingest(action),
                        false,
                        cx,
                    )
                    .h(px(ui::COMPACT_CONTROL_HEIGHT))
                    .border_color(self.border()),
                );
            }
            job = job.child(recent);
        } else {
            job = job.child(self.ingest_hint("Used by the {job} token · J jumps here"));
        }
        let mut panel = div()
            .id("ingest-destination")
            .w(px(width))
            .min_w(px(0.))
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .overflow_y_scroll()
            .p(px(ui::BAR_INSET))
            .pb(px(space::XL))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(self.ingest_heading("Job", true))
            .child(job)
            .child(self.ingest_heading("Destination", false))
            .child(
                self.ingest_field(0, "Library folder", cx)
                    .child(self.ingest_link(
                        "ingest-pick-library",
                        "Choose folder…",
                        cx,
                        |this, cx| this.ingest_picker(0, cx),
                    )),
            )
            .child(
                self.ingest_field(4, "Backup copies (optional)", cx)
                    .child(
                        self.ingest_hint("Each backup is read back and checked like the library."),
                    )
                    .child(self.ingest_link(
                        "ingest-pick-backup",
                        "+ Add backup folder…",
                        cx,
                        |this, cx| this.ingest_picker(4, cx),
                    )),
            );
        if let Some(plan) = &self.import_ui.plan {
            let labeled = plan.space.len() > 1;
            for (index, capacity) in plan.space.iter().enumerate() {
                panel = panel.child(self.ingest_space_row(index, capacity, labeled, cx));
            }
        }
        panel = panel
            .child(self.ingest_heading("Naming", false))
            .child(self.ingest_dropdown(
                IngestMenu::Preset,
                "Preset",
                &self.ingest_preset_label(),
                inner,
                cx,
            ))
            .child(self.ingest_field(1, "Subfolder template", cx).child(
                self.ingest_hint("Use / to nest folders; empty imports into the library folder."),
            ))
            .child(
                self.ingest_field(2, "Filename template", cx)
                    .child(self.ingest_hint("The original extension is added automatically.")),
            );
        let example = self.import_ui.plan.as_ref().map_or_else(
            || "Shown once the import is planned".into(),
            |plan| {
                plan.plan.example.as_ref().map_or_else(
                    || "Select a photo to preview its name".into(),
                    // Relative to the library root: the folder/file pattern is what matters.
                    |p| {
                        let root = std::path::Path::new(&self.import_ui.fields[0]);
                        p.strip_prefix(root)
                            .map(|r| format!("…/{}", r.display()))
                            .unwrap_or_else(|_| p.display().to_string())
                    },
                )
            },
        );
        panel = panel.child(
            div()
                .id("ingest-example")
                .flex()
                .flex_col()
                .flex_shrink_0()
                .mx(px(space::SM))
                .p(px(space::SM))
                .gap(px(space::XS))
                .bg(self.c("elevated_surface.background", "#1b1b1b"))
                .child(self.ingest_label("Example"))
                .child(
                    div()
                        .min_w(px(0.))
                        .text_size(px(11.))
                        .whitespace_normal()
                        .child(example.clone()),
                )
                .tooltip(self.tip(&example, "")),
        );
        // The token reference stays folded unless a template is being edited.
        let tokens_open =
            self.import_ui.tokens_open || matches!(self.import_ui.active_field, Some(1 | 2));
        let hover = self.text();
        panel = panel.child(
            div()
                .id("ingest-tokens-toggle")
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(space::XS))
                .px(px(space::SM))
                .cursor_pointer()
                .text_size(px(ui::text::LABEL))
                .text_color(self.muted())
                .hover(move |s| s.text_color(hover))
                .child(icon(
                    if tokens_open { "down" } else { "chevron" },
                    self.icon_muted(),
                ))
                .child(format!("Naming tokens ({})", ingest::naming::TOKENS.len()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ingest_action("tokens", cx);
                })),
        );
        if tokens_open {
            panel = panel.child(self.ingest_tokens(cx));
        }
        let selection = SELECTIONS
            .iter()
            .find(|(value, _)| *value == options.selection)
            .map_or(options.selection.as_str(), |(_, label)| label);
        panel = panel
            .child(self.ingest_heading("Options", false))
            .child(self.ingest_dropdown(IngestMenu::Selection, "Import", selection, inner, cx));
        if options.selection == "rated" {
            let mut stars = div().flex().flex_shrink_0();
            for rating in 1..=5u8 {
                stars = stars.child(self.star(
                    ("ingest-rating", rating as usize),
                    rating <= options.min_rating,
                    Cmd::Ingest(format!("rating:{rating}")),
                    cx,
                ));
            }
            panel = panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .px(px(space::SM))
                    .child(self.ingest_label("Minimum rating"))
                    .child(stars),
            );
        }
        panel = panel
            .child(self.ingest_choice_row(
                "Videos",
                "ingest-videos",
                [
                    (
                        "Include".into(),
                        Cmd::Ingest("videos:include".into()),
                        options.include_videos,
                    ),
                    (
                        "Skip".into(),
                        Cmd::Ingest("videos:skip".into()),
                        !options.include_videos,
                    ),
                ],
                cx,
            ))
            .child(self.ingest_choice_row(
                "Name already taken",
                "ingest-collision",
                [
                    (
                        "Add suffix".into(),
                        Cmd::Ingest("collision:suffix".into()),
                        options.collision == "suffix",
                    ),
                    (
                        "Skip".into(),
                        Cmd::Ingest("collision:skip".into()),
                        options.collision == "skip",
                    ),
                ],
                cx,
            ))
            .child(self.ingest_heading("More", false));
        let mut more = div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .gap(px(space::XS))
            .px(px(space::SM));
        for (action, label, tip) in [
            (
                "save",
                "Save as defaults",
                "Remember the destination, templates and options for future imports",
            ),
            (
                "cull",
                "Cull on card first",
                "Open the card in place (nothing is copied) to pick and rate, then import Picks or Rated photos",
            ),
            ("eject", "Eject card", "Safely eject the source volume"),
        ] {
            more = more.child(
                self.button(
                    SharedString::from(format!("ingest-{action}")),
                    label,
                    "",
                    Cmd::Ingest(action.into()),
                    false,
                    cx,
                )
                .border_color(self.border())
                .tooltip(self.tip(tip, "")),
            );
        }
        panel = panel.child(more);
        // Mark as imported records photos without copying them: it asks first,
        // with the count and what it means for the next scan of this card.
        let count = self.import_ui.selected.len();
        let marking = self.import_ui.confirming(IngestConfirm::Mark);
        let mut mark = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(space::XS))
            .px(px(space::SM));
        if marking {
            mark = mark
                .child(div().text_size(px(ui::text::LABEL)).whitespace_normal().child(format!(
                    "Mark {} as already imported? Nothing is copied, and they appear dimmed and unchecked next time this card is scanned.",
                    plural(count, "photo", "photos")
                )))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(space::XS))
                        .child(self.confirm_button(
                            self.action_button(
                                "ingest-mark-keep",
                                "Cancel",
                                Cmd::Ingest("mark:keep".into()),
                                ButtonKind::Secondary,
                                false,
                                cx,
                            ),
                            true,
                        ))
                        .child(self.confirm_button(
                            self.action_button(
                                "ingest-mark-confirm",
                                &format!("Mark {} as imported", plural(count, "photo", "photos")),
                                Cmd::Ingest("mark".into()),
                                ButtonKind::DestructiveQuiet,
                                false,
                                cx,
                            ),
                            false,
                        )),
                );
        } else {
            mark = mark.child(
                self.ingest_link(
                    "ingest-mark",
                    "Mark selected as imported…",
                    cx,
                    |this, cx| this.ingest_action("mark", cx),
                )
                .tooltip(self.tip(
                    "Record the selected photos as imported without copying them",
                    "",
                )),
            );
        }
        panel.child(mark).into_any_element()
    }
    fn ingest_selection_controls(&self, cx: &gpui::Context<Self>) -> AnyElement {
        // What is shown (all or new only, with counts), then what to check.
        let all = self.import_ui.items.len();
        let new = self.import_ui.new_count;
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(space::XS))
            .child(
                self.segment(
                    "ingest-show-all",
                    &format!("All {all}"),
                    Cmd::Ingest("show:all".into()),
                    !self.import_ui.new_only,
                    cx,
                )
                .tooltip(self.tip("Show every photo on the source", "")),
            )
            .child(
                self.segment(
                    "ingest-new",
                    &format!("New {new}"),
                    Cmd::Ingest("show:new".into()),
                    self.import_ui.new_only,
                    cx,
                )
                .tooltip(self.tip("Hide photos already in the library", "")),
            )
            .child(self.control_divider())
            .child(
                self.button(
                    "ingest-all",
                    "Check all",
                    "",
                    Cmd::Ingest("all".into()),
                    false,
                    cx,
                )
                .tooltip(self.tip(
                    "Include every shown photo; hidden photos are unaffected",
                    "⌘A",
                )),
            )
            .child(
                self.button(
                    "ingest-none",
                    "Uncheck all",
                    "",
                    Cmd::Ingest("none".into()),
                    false,
                    cx,
                )
                .tooltip(self.tip("Leave out every photo", "")),
            )
            .into_any_element()
    }
    pub(super) fn ingest_view(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        if self.import_ui.show_history {
            let history = self.import_ui.history.clone();
            let loaded = history["jobs"].is_array();
            let jobs_len = history["jobs"].as_array().map_or(0, Vec::len);
            let empty = (jobs_len == 0).then(|| {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(space::MD))
                    .text_center()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(if loaded {
                                "No imports yet"
                            } else {
                                "Loading history…"
                            }),
                    )
                    .when(loaded, |s| {
                        s.child(div().text_color(self.muted()).child(
                            "Imports you run from a card or folder appear here with their verification result.",
                        ))
                        .child(
                            self.button(
                                "ingest-history-import",
                                "Import from Card…",
                                "import",
                                Cmd::Import,
                                false,
                                cx,
                            )
                            .border_1()
                            .border_color(self.c("border.variant", "#333333")),
                        )
                    })
            });
            if let Some(empty) = empty {
                return div()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .p(px(ui::PANEL_INSET))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child("Import History")
                            .child(self.button(
                                "ingest-history-close",
                                "Close",
                                "close",
                                Cmd::Ingest("close".into()),
                                false,
                                cx,
                            )),
                    )
                    .child(empty)
                    .into_any_element();
            }
            return div()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .p(px(ui::PANEL_INSET))
                .gap(px(space::MD))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child("Import History")
                        .child(self.button(
                            "ingest-history-close",
                            "Close",
                            "close",
                            Cmd::Ingest("close".into()),
                            false,
                            cx,
                        )),
                )
                .child(
                    // At most 100 jobs, and unfolded rows grow: a plain
                    // scrolling column rather than a fixed-height list.
                    div()
                        .id("ingest-history")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(
                            history["jobs"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .enumerate()
                                .map(|(n, job)| self.ingest_history_row(n, job, cx)),
                        ),
                )
                .into_any_element();
        }
        let layout = self.ingest_layout();
        let narrow = layout.destination == 0.;
        let focus_timeline = layout.timeline == 0. && self.import_ui.panel == ImportPanel::Timeline;
        let focus_destination = narrow && self.import_ui.panel == ImportPanel::Destination;
        let timeline = uniform_list(
            "ingest-timeline",
            self.import_ui.timeline.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let group = &this.import_ui.timeline[index];
                        // Day level, then the session indented under it; each
                        // level's checkbox sits at one left edge.
                        // The year only when the card spans more than one.
                        let timeline = &this.import_ui.timeline;
                        let one_year =
                            timeline.first().zip(timeline.last()).is_some_and(|(a, b)| {
                                a.day.get(..4).is_some() && a.day.get(..4) == b.day.get(..4)
                            });
                        let date = chrono::NaiveDate::parse_from_str(&group.day, "%Y-%m-%d")
                            .map_or_else(
                                |_| group.day.clone(),
                                |d| {
                                    d.format(if one_year {
                                        "%a %-d %b"
                                    } else {
                                        "%a %-d %b %Y"
                                    })
                                    .to_string()
                                },
                            );
                        let (day_selected, day_total) = this
                            .import_ui
                            .day_counts
                            .get(&group.day)
                            .copied()
                            .unwrap_or((0, 0));
                        let first_of_day =
                            index == 0 || this.import_ui.timeline[index - 1].day != group.day;
                        div()
                            .id(("ingest-time", index))
                            .h(px(66.))
                            .px(px(ui::PANEL_INSET))
                            .py(px(space::SM))
                            .flex()
                            .flex_col()
                            .gap(px(space::XS))
                            .overflow_hidden()
                            .border_b_1()
                            .border_color(this.border())
                            .cursor_pointer()
                            .child(
                                div()
                                    .id(("ingest-day", index))
                                    .flex()
                                    .items_center()
                                    .gap(px(space::XS))
                                    .min_w(px(0.))
                                    .when(!first_of_day, |s| s.text_color(this.muted()))
                                    .child(icon(
                                        selection_icon(day_selected, day_total),
                                        this.icon_muted(),
                                    ))
                                    // The label yields; the count never clips.
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(date),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(ui::text::LABEL))
                                            .text_color(this.muted())
                                            .child(copy::count(day_total)),
                                    )
                                    .tooltip(this.tip(
                                        &format!(
                                            "{} · check or uncheck the whole day",
                                            plural(day_total, "photo", "photos")
                                        ),
                                        "",
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let day = this.import_ui.timeline[index].day.clone();
                                        let ids: Vec<_> = this
                                            .import_ui
                                            .timeline
                                            .iter()
                                            .filter(|g| g.day == day)
                                            .flat_map(|g| g.ids.iter().copied())
                                            .collect();
                                        let all = ids
                                            .iter()
                                            .all(|id| this.import_ui.selected.contains(id));
                                        for &id in &ids {
                                            if this.import_ui.selected.contains(&id) == all {
                                                this.import_ui.toggle(id);
                                            }
                                        }
                                        this.ingest_scroll_to_group(ids.first().copied());
                                        cx.stop_propagation();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .pl(px(space::LG))
                                    .min_w(px(0.))
                                    .text_size(px(11.))
                                    .text_color(this.muted())
                                    .flex()
                                    .items_center()
                                    .gap(px(space::XS))
                                    .child(icon(
                                        selection_icon(
                                            this.import_ui
                                                .timeline_selected
                                                .get(index)
                                                .copied()
                                                .unwrap_or(0),
                                            group.ids.len(),
                                        ),
                                        this.icon_muted(),
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(group.label.clone()),
                                    )
                                    .child(
                                        div().flex_shrink_0().child(copy::count(group.ids.len())),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let ids = this.import_ui.timeline[index].ids.clone();
                                let all = ids.iter().all(|id| this.import_ui.selected.contains(id));
                                for &id in &ids {
                                    if this.import_ui.selected.contains(&id) == all {
                                        this.import_ui.toggle(id);
                                    }
                                }
                                this.ingest_scroll_to_group(ids.first().copied());
                                cx.notify();
                            }))
                    })
                    .collect()
            }),
        )
        .track_scroll(self.import_ui.timeline_scroll.clone())
        .size_full()
        .flex_1()
        .min_h(px(0.));
        let timeline = self.scrollable_list(
            "ingest-timeline-scrollbar",
            &self.import_ui.timeline_scroll,
            timeline.into_any_element(),
            cx,
        );
        let label = self.import_ui.plan.as_ref().map_or_else(
            || {
                copy::import_label(
                    self.import_ui.photos,
                    self.import_ui.videos,
                    self.import_ui.bytes,
                )
            },
            |s| copy::import_label(s.plan.photos, s.plan.videos, s.plan.bytes),
        );
        let destination = self.ingest_destination(
            if focus_destination {
                self.center_size.0.max(0.)
            } else {
                layout.destination
            },
            cx,
        );
        let title = self
            .import_ui
            .source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let short = self.center_size.1 < 360.;
        let source_label = format!(
            "Import from {title}{}",
            if self.import_ui.scanning() {
                " · scanning…"
            } else {
                ""
            }
        );
        let timeline_panel = div()
            .w(px(if focus_timeline {
                self.center_size.0.max(0.)
            } else {
                layout.timeline
            }))
            .h_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_r_1()
            .border_color(self.border())
            .child(
                div()
                    .h(px(TOOLBAR_HEIGHT))
                    .flex_shrink_0()
                    .p(px(ui::BAR_INSET))
                    .border_b_1()
                    .border_color(self.border())
                    .child(
                        self.ingest_dropdown(
                            IngestMenu::Gap,
                            "",
                            &format!("Gap · {} min", self.settings.ingest_time_group_minutes),
                            220.,
                            cx,
                        )
                        .px(px(0.)),
                    ),
            )
            .child(timeline);
        let mut body = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden();
        if focus_timeline {
            body = body.child(timeline_panel);
        } else if focus_destination {
            body = body.child(destination);
        } else {
            if layout.timeline > 0. {
                body = body
                    .child(timeline_panel)
                    .child(self.ingest_panel_handle(ImportPanel::Timeline, cx));
            }
            body = body.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .h_full()
                    .overflow_hidden()
                    .child(self.ingest_grid(cx)),
            );
            if layout.destination > 0. {
                body = body
                    .child(self.ingest_panel_handle(ImportPanel::Destination, cx))
                    .child(destination);
            }
        }
        let mut controls = div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .items_center()
            .min_w(px(0.))
            .gap(px(space::SM))
            .p(px(ui::BAR_INSET))
            .border_b_1()
            .border_color(self.border());
        if narrow || short {
            controls = controls.child(self.ingest_selection_controls(cx));
        }
        if layout.timeline == 0. {
            let mut tabs = div()
                .flex()
                .flex_shrink_0()
                .gap(px(space::XS))
                .child(self.button(
                    "ingest-panel-photos",
                    "Photos",
                    "",
                    Cmd::Ingest("panel:photos".into()),
                    !focus_timeline && !focus_destination,
                    cx,
                ))
                .child(self.button(
                    "ingest-panel-timeline",
                    "Timeline",
                    "",
                    Cmd::Ingest("panel:timeline".into()),
                    focus_timeline,
                    cx,
                ));
            if narrow {
                tabs = tabs.child(self.button(
                    "ingest-panel-destination",
                    "Destination",
                    "",
                    Cmd::Ingest("panel:destination".into()),
                    focus_destination,
                    cx,
                ));
            }
            controls = controls.child(tabs);
        }
        let footer = self.ingest_footer(narrow, &label, cx);
        div()
            .id("ingest-root")
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(!short, |s| {
                s.child(
                    div()
                        .h(px(TOOLBAR_HEIGHT))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .pl(px(ui::PANEL_INSET))
                        .pr(px(ui::BAR_INSET))
                        .py(px(ui::BAR_INSET))
                        .border_b_1()
                        .border_color(self.border())
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_ellipsis()
                                .whitespace_normal()
                                .line_clamp(1)
                                .child(source_label.clone()),
                        )
                        .when(!narrow, |s| s.child(self.ingest_selection_controls(cx)))
                        .child(
                            self.button(
                                "ingest-history-open",
                                "History",
                                "files",
                                Cmd::Ingest("history".into()),
                                false,
                                cx,
                            )
                            .tooltip(self.tip("Past imports and their verification", "")),
                        ),
                )
            })
            .when(narrow || short || layout.timeline == 0., |s| {
                s.child(controls)
            })
            .child(body)
            .child(footer)
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.move_ingest_resize(event, cx)
                }),
            )
            .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, _, _| {
                if event.button == MouseButton::Left {
                    this.finish_ingest_resize();
                }
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.finish_ingest_resize()),
            )
            .into_any_element()
    }
    /// One Import History job: when, from which card, how much, where to, and
    /// whether every copy verified. A click unfolds its destinations and
    /// errors with Reveal and Open manifest.
    fn ingest_history_row(&self, n: usize, job: &Value, cx: &gpui::Context<Self>) -> AnyElement {
        let id = job["id"].as_str().unwrap_or_default().to_owned();
        let open = self.import_ui.history_open.contains(&id);
        let counts = &job["counts"];
        let count = |key: &str| counts[key].as_u64().unwrap_or(0) as usize;
        let started = job["started"]
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| time.with_timezone(&chrono::Local));
        let this_year = chrono::Local::now().format("%Y").to_string();
        let date = started.map_or_else(String::new, |time| {
            let year = time.format("%Y").to_string();
            time.format(if year == this_year {
                "%a %-d %b · %H:%M"
            } else {
                "%a %-d %b %Y · %H:%M"
            })
            .to_string()
        });
        let source = job["source"].as_str().unwrap_or_default();
        let volume = std::path::Path::new(source)
            .file_name()
            .map_or_else(|| source.to_owned(), |n| n.to_string_lossy().into_owned());
        let photos = count("photos") + count("videos");
        let mut amount = plural(photos.max(count("completed")), "photo", "photos");
        if let Some(bytes) = job["bytes"].as_u64().filter(|b| *b > 0) {
            amount.push_str(&format!(" · {}", copy::fmt_bytes(bytes)));
        }
        let library = job["options"]["library_root"]
            .as_str()
            .or_else(|| counts["library_root"].as_str())
            .unwrap_or_default()
            .to_owned();
        let backups: Vec<String> = job["options"]["backup_roots"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_owned))
            .collect();
        let mut destination = format!(
            "→ {}",
            std::path::Path::new(&library)
                .file_name()
                .map_or_else(|| library.clone(), |n| n.to_string_lossy().into_owned())
        );
        if !backups.is_empty() {
            destination.push_str(&format!(
                " + {}",
                plural(backups.len(), "backup", "backups")
            ));
        }
        let errors: Vec<String> = job["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect();
        let success = self.c("success", "#7fbf7f");
        let error = self.c("error", "#d46a6a");
        let (chip, chip_color) = match job["verification"].as_str().unwrap_or_default() {
            "completed" => ("Verified".to_owned(), success),
            "failed" if count("failed") > 0 => (format!("Failed · {}", count("failed")), error),
            "failed" => ("Failed".to_owned(), error),
            "cancelled" => ("Cancelled".to_owned(), self.muted()),
            _ => ("In progress".to_owned(), self.muted()),
        };
        let skipped = count("skipped");
        let manifest = counts["manifest"].as_str().map(PathBuf::from);
        let toggle = format!("history_row:{id}");
        let hover = self.hover_bg();
        let mut row = div()
            .id(("ingest-history-row", n))
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w(px(0.))
            .border_b_1()
            .border_color(self.border())
            .child(
                div()
                    .id(("ingest-history-summary", n))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .min_h(px(44.))
                    .px(px(space::SM))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(
                        icon(if open { "down" } else { "chevron" }, self.icon_muted())
                            .size(px(12.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(format!("{date} · {volume} · {amount}")),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(ui::text::LABEL))
                                    .text_color(self.muted())
                                    .child(if skipped > 0 {
                                        format!(
                                            "{destination} · {} already in library",
                                            copy::count(skipped)
                                        )
                                    } else {
                                        destination
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(space::XS))
                            .px(px(space::SM))
                            .h(px(ui::COMPACT_CONTROL_HEIGHT))
                            .border_1()
                            .border_color(chip_color)
                            .text_size(px(ui::text::LABEL))
                            .text_color(chip_color)
                            .when(chip == "Verified", |s| {
                                s.child(icon("check", chip_color).size(px(12.)))
                            })
                            .child(chip),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ingest_action(&toggle, cx);
                        cx.notify();
                    })),
            );
        if open {
            let paths = std::iter::once(("Library", library.clone()))
                .chain(backups.iter().map(|b| ("Backup", b.clone())))
                .filter(|(_, p)| !p.is_empty());
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .pl(px(space::LG))
                    .pr(px(space::SM))
                    .pb(px(space::SM))
                    .text_size(px(ui::text::LABEL))
                    .children(paths.map(|(label, path)| {
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(self.muted())
                            .child(format!("{label} · {}", copy::middle_path(&path, 72)))
                    }))
                    .child(
                        div()
                            .text_color(self.muted())
                            .child(format!("From {}", copy::middle_path(source, 72))),
                    )
                    .when(!errors.is_empty(), |s| {
                        s.child(
                            div()
                                .id(("ingest-history-errors", n))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .p(px(space::XS))
                                .border_1()
                                .border_color(self.border())
                                .children(errors.iter().map(|e| {
                                    div().whitespace_normal().text_color(error).child(e.clone())
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(space::XS))
                            .when(!library.is_empty(), |s| {
                                s.child(
                                    self.action_button(
                                        ("ingest-history-reveal", n),
                                        "Reveal",
                                        Cmd::RevealPath(PathBuf::from(&library)),
                                        ButtonKind::Secondary,
                                        false,
                                        cx,
                                    )
                                    .tooltip(self.tip("Show the library folder", "")),
                                )
                            })
                            .when_some(manifest, |s, manifest| {
                                s.child(
                                    self.action_button(
                                        ("ingest-history-manifest", n),
                                        "Open manifest",
                                        Cmd::RevealPath(manifest),
                                        ButtonKind::Secondary,
                                        false,
                                        cx,
                                    )
                                    .tooltip(self.tip(
                                        "Show the job's manifest: exact paths and BLAKE3 digests",
                                        "",
                                    )),
                                )
                            }),
                    ),
            );
        }
        row.into_any_element()
    }
    /// Why Import cannot start yet, or None when it can.
    fn ingest_blocker(&self) -> Option<&'static str> {
        if self.import_ui.scanning() {
            Some("Wait for the scan to finish")
        } else if self.import_ui.active() && self.import_ui.queued(&self.import_ui.source) {
            Some("Already queued: it starts when the current import finishes")
        } else if self.import_ui.selected.is_empty() {
            Some("Check the photos to import")
        } else if !self.unsaved.is_empty() || self.awaiting > 0 || !self.outbox.is_empty() {
            Some("Wait for cull decisions to finish saving")
        } else if self.import_ui.error.is_some() {
            Some("Fix the problem shown above Import")
        } else {
            match &self.import_ui.plan {
                None => Some("Checking the destination…"),
                Some(plan) if !plan.ready => Some("The destination needs attention"),
                Some(plan) if plan.plan.groups.is_empty() => Some("Nothing to import"),
                Some(_) => None,
            }
        }
    }
    /// The one filled button in the Import view: what it copies and how much,
    /// with its own icon and the shortcut that starts it.
    fn ingest_primary(
        &self,
        label: &str,
        disabled: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        let accent = self.accent();
        let on = self.text_on(accent);
        div()
            .id("ingest-start")
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(space::SM))
            .h(px(ui::CONTROL_HEIGHT + 4.))
            .px(px(space::MD))
            .bg(accent)
            .text_color(on)
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .occlude()
            .child(icon("import", on))
            .child(label.to_owned())
            // The shortcut in full ink (a faded hint fell under 4.5:1 on the
            // dark accent), set apart by a thin rule and regular weight.
            .child(div().w(px(1.)).h(px(14.)).bg(on).opacity(0.5))
            .child(
                div()
                    .text_size(px(ui::text::LABEL))
                    .font_weight(FontWeight::NORMAL)
                    .child(if cfg!(target_os = "macos") {
                        "⌘↵"
                    } else {
                        "Ctrl+Enter"
                    }),
            )
            .when(disabled, |s| s.opacity(0.45))
            .when(!disabled, |s| {
                s.cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(|this, _, _, cx| this.ingest_action("start", cx)))
            })
    }
    /// A button of an inline confirmation: the one Enter activates wears the
    /// focus ring, always (as in the delete dialog), with the keys taught.
    fn confirm_button(&self, button: Stateful<Div>, keep: bool) -> Stateful<Div> {
        let focused = self.import_ui.confirm_keep == keep;
        button
            .relative()
            .when(focused, |s| s.child(self.focus_ring_overlay()))
            .tooltip(self.tip(
                if keep {
                    "Keep things as they are · Tab or arrows switch"
                } else {
                    "Confirm · Tab or arrows switch"
                },
                if keep { "Esc" } else { "Enter" },
            ))
    }
    /// Footer of the Import view: the last job's outcome, the plan problem,
    /// the guarantees, then either the running job's progress or the summary
    /// and the primary Import button.
    fn ingest_footer(&self, narrow: bool, label: &str, cx: &gpui::Context<Self>) -> AnyElement {
        let ui_state = &self.import_ui;
        // A job copying another card leaves this card's footer to its own
        // review and Queue button, with that job summarized above it.
        let active = ui_state.job_here();
        let error = self.c("error", "#d46a6a");
        let warning = self.c("warning", "#e0b341");
        let mut footer = div()
            .id("ingest-footer")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .min_w(px(0.))
            .gap(px(space::XS))
            .p(px(ui::BAR_INSET))
            .border_t_1()
            .border_color(self.border());
        if let Some(outcome) = ui_state.outcome.as_ref().filter(|_| !active) {
            let (glyph, color) = match outcome.tone {
                Tone::Success => ("check", self.c("success", "#7fbf7f")),
                Tone::Warning => ("warning", warning),
                Tone::Info => ("check", self.icon_muted()),
            };
            footer = footer.child(
                div()
                    .id("ingest-outcome")
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::SM))
                    .min_h(px(ui::CONTROL_HEIGHT))
                    .bg(self.c("elevated_surface.background", "#1b1b1b"))
                    .child(icon(glyph, color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .whitespace_normal()
                            .line_clamp(2)
                            .child(outcome.text.clone()),
                    )
                    .when_some(outcome.library.clone(), |s, root| {
                        s.child(
                            self.action_button(
                                "ingest-outcome-reveal",
                                "Reveal",
                                Cmd::RevealPath(root),
                                ButtonKind::Secondary,
                                false,
                                cx,
                            )
                            .tooltip(self.tip("Show the library folder in the file manager", "")),
                        )
                        .child(self.action_button(
                            "ingest-outcome-cull",
                            "Cull imported photos",
                            Cmd::Ingest("close".into()),
                            ButtonKind::Secondary,
                            false,
                            cx,
                        ))
                    })
                    .when(outcome.remaining, |s| {
                        s.child(self.action_button(
                            "ingest-remaining",
                            "Import remaining",
                            Cmd::Ingest("remaining".into()),
                            ButtonKind::Secondary,
                            self.ingest_blocker().is_some(),
                            cx,
                        ))
                    })
                    .when(!outcome.errors.is_empty(), |s| {
                        s.child(
                            self.action_button(
                                "ingest-outcome-details",
                                if ui_state.details_open {
                                    "Hide details"
                                } else {
                                    "Details"
                                },
                                Cmd::Ingest("details".into()),
                                ButtonKind::Secondary,
                                false,
                                cx,
                            )
                            .tooltip(self.tip("List every problem with its file", "")),
                        )
                    })
                    .when(!outcome.failed_ids.is_empty(), |s| {
                        s.child(
                            self.action_button(
                                "ingest-retry-failed",
                                "Retry failed",
                                Cmd::Ingest("retry_failed".into()),
                                ButtonKind::Secondary,
                                ui_state.active() || ui_state.scanning(),
                                cx,
                            )
                            .tooltip(self.tip(
                                "Import only the photos that failed; verified photos are not copied again",
                                "",
                            )),
                        )
                    })
                    .when_some(
                        outcome.manifest.clone().filter(|_| outcome.tone != Tone::Success),
                        |s, manifest| {
                            s.child(
                                self.action_button(
                                    "ingest-open-manifest",
                                    "Open manifest",
                                    Cmd::RevealPath(manifest),
                                    ButtonKind::Secondary,
                                    false,
                                    cx,
                                )
                                .tooltip(self.tip(
                                    "Show the job's manifest: exact paths and BLAKE3 digests",
                                    "",
                                )),
                            )
                        },
                    )
                    .child(
                        self.compact_button(
                            "ingest-outcome-dismiss",
                            "",
                            "close",
                            Cmd::Ingest("dismiss_outcome".into()),
                            false,
                            cx,
                        )
                        .tooltip(self.tip("Dismiss", "")),
                    ),
            );
        }
        // The failed job's problems, one per line, folded until asked for.
        if let Some(outcome) = ui_state
            .outcome
            .as_ref()
            .filter(|o| !active && ui_state.details_open && !o.errors.is_empty())
        {
            footer = footer.child(
                div()
                    .id("ingest-outcome-errors")
                    .flex()
                    .flex_col()
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .px(px(space::SM))
                    .py(px(space::XS))
                    .gap(px(2.))
                    .text_size(px(ui::text::LABEL))
                    .border_1()
                    .border_color(self.border())
                    .children(outcome.errors.iter().enumerate().map(|(n, problem)| {
                        div()
                            .id(("ingest-outcome-error", n))
                            .min_w(px(0.))
                            .whitespace_normal()
                            .child(problem.clone())
                    })),
            );
        }
        // A job copying another card: what it does, and that this one waits.
        if ui_state.active() && !active {
            let progress = &ui_state.progress;
            let missing = progress.source_missing;
            let line = if missing {
                copy::import_progress(progress)
            } else {
                format!(
                    "{} · {}",
                    copy::source_name(progress),
                    copy::import_progress(progress)
                )
            };
            footer = footer.child(
                div()
                    .id("ingest-other-job")
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::SM))
                    .text_size(px(ui::text::LABEL))
                    .text_color(if missing { warning } else { self.muted() })
                    .child(icon(
                        if missing { "warning" } else { "import" },
                        self.icon_muted(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(line.clone()),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .h(px(4.))
                            .flex_shrink_0()
                            .bg(self.border())
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(copy::import_fraction(progress)))
                                    .bg(self.accent()),
                            ),
                    )
                    .tooltip(self.tip(&line, "")),
            );
        }
        if let Some(problem) = ui_state.error.as_ref().filter(|_| !active) {
            footer = footer.child(
                div()
                    .id("ingest-error")
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(space::SM))
                    .text_color(error)
                    .child(icon("warning", error))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .whitespace_normal()
                            .line_clamp(2)
                            .child(problem.clone()),
                    )
                    .tooltip(self.tip(problem, "")),
            );
        }
        // What the engine guarantees, said once where the decision is made.
        footer = footer.child(
            div()
                .id("ingest-trust")
                .flex()
                .items_center()
                .gap(px(space::XS))
                .px(px(space::SM))
                .text_size(px(ui::text::LABEL))
                .text_color(self.muted())
                .child(icon("check", self.icon_muted()).size(px(12.)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .whitespace_normal()
                        .line_clamp(2)
                        .child("Copies are read back and checked · nothing on the card is changed · existing files are never overwritten"),
                )
                .tooltip(self.tip(
                    "Every copy is verified with BLAKE3 after writing, on the library and each backup. The source is opened read-only, and names that are taken get a suffix (or are skipped).",
                    "",
                )),
        );
        let mut row = div()
            .flex()
            .when(narrow, |s| s.flex_col())
            .min_w(px(0.))
            .gap(px(space::SM))
            .items_center();
        let mut actions = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(space::XS))
            .when(narrow, |s| s.w_full().justify_end());
        if active {
            let progress = &ui_state.progress;
            let (skipped, failed) = copy::import_exceptions(progress);
            let line = copy::import_progress(progress);
            row = row.child(
                div()
                    .id("ingest-progress")
                    .when(!narrow, |s| s.flex_1())
                    .when(narrow, |s| s.w_full())
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .px(px(space::SM))
                    .child(
                        div()
                            .flex()
                            .min_w(px(0.))
                            .gap(px(space::XS))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .when(progress.source_missing, |s| s.text_color(warning))
                                    .child(line.clone()),
                            )
                            // Identical files already in the library are the
                            // engine's safe deduplication, not a problem.
                            .when_some(skipped, |s, text| {
                                s.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(self.muted())
                                        .child(format!("· {text}")),
                                )
                            })
                            .when_some(failed, |s, text| {
                                s.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(error)
                                        .child(format!("· {text}")),
                                )
                            }),
                    )
                    .child(
                        div().h(px(4.)).w_full().bg(self.border()).child(
                            div()
                                .h_full()
                                .w(relative(copy::import_fraction(progress)))
                                .bg(self.accent()),
                        ),
                    )
                    .tooltip(self.tip(&line, "")),
            );
            if ui_state.confirming(IngestConfirm::Cancel) {
                actions = actions
                    .child(
                        div()
                            .text_size(px(ui::text::LABEL))
                            .text_color(self.muted())
                            .child(if ui_state.queue.is_empty() {
                                "Verified photos stay imported."
                            } else {
                                "Verified photos stay imported; queued cards are not started."
                            }),
                    )
                    .child(self.confirm_button(
                        self.action_button(
                            "ingest-cancel-keep",
                            "Keep importing",
                            Cmd::Ingest("cancel:keep".into()),
                            ButtonKind::Secondary,
                            false,
                            cx,
                        ),
                        true,
                    ))
                    .child(self.confirm_button(
                        self.action_button(
                            "ingest-cancel",
                            "Cancel import",
                            Cmd::Ingest("cancel".into()),
                            ButtonKind::DestructiveQuiet,
                            false,
                            cx,
                        ),
                        false,
                    ));
            } else {
                actions = actions
                    // While the card is out the job resumes by itself on
                    // reinsertion; Resume would have nothing to read.
                    .when(!progress.source_missing, |s| {
                        s.child(self.action_button(
                            "ingest-pause",
                            if progress.phase == Phase::Paused {
                                "Resume"
                            } else {
                                "Pause"
                            },
                            Cmd::Ingest("pause".into()),
                            ButtonKind::Secondary,
                            false,
                            cx,
                        ))
                    })
                    .child(
                        self.action_button(
                            "ingest-cancel",
                            "Cancel…",
                            Cmd::Ingest("cancel".into()),
                            ButtonKind::Secondary,
                            false,
                            cx,
                        )
                        .tooltip(self.tip(
                            "Stop after the current photo; verified photos stay imported",
                            "",
                        )),
                    );
            }
        } else {
            // Where the photos go, in place of engine words like "Plan ready".
            let library = std::path::Path::new(ui_state.fields[0].trim());
            let root = library.file_name().map_or_else(
                || library.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let target = ui_state
                .plan
                .as_ref()
                .and_then(|plan| plan.plan.example.as_ref())
                .and_then(|example| example.parent()?.strip_prefix(library).ok())
                .filter(|sub| !sub.as_os_str().is_empty())
                .map_or_else(|| root.clone(), |sub| format!("{root}/{}", sub.display()));
            let backups = ui_state.fields[4]
                .split(';')
                .filter(|p| !p.trim().is_empty())
                .count();
            let mut summary = format!(
                "{} of {} checked · → {target}",
                ui_state.selected.len(),
                ui_state.items.len()
            );
            if backups > 0 {
                summary.push_str(&format!(" · +{}", plural(backups, "backup", "backups")));
            }
            if ui_state.scanning() {
                summary = format!("Scanning · {} found so far", ui_state.items.len());
            }
            let blocker = self.ingest_blocker();
            row = row.child(
                div()
                    .id("ingest-summary")
                    .when(!narrow, |s| s.flex_1())
                    .when(narrow, |s| s.w_full())
                    .min_w(px(0.))
                    .h(px(ui::CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .px(px(space::SM))
                    .text_color(self.muted())
                    .overflow_hidden()
                    .tooltip(self.tip(&format!("{summary}\n{}", library.display()), ""))
                    .child(
                        div()
                            .min_w(px(0.))
                            .text_ellipsis()
                            .whitespace_normal()
                            .line_clamp(1)
                            .child(summary),
                    ),
            );
            // While another card copies, the same button queues this one.
            let label = if ui_state.active() {
                if ui_state.queued(&ui_state.source) {
                    format!("Queued · after {}", copy::source_name(&ui_state.progress))
                } else {
                    format!("Queue {}", label.strip_prefix("Import ").unwrap_or(label))
                }
            } else {
                label.to_owned()
            };
            let tip = match blocker {
                Some(blocker) => blocker.to_owned(),
                None if ui_state.active() => format!(
                    "{label} · starts by itself when {} finishes",
                    copy::source_name(&ui_state.progress)
                ),
                None => label.clone(),
            };
            if ui_state.active() && ui_state.queued(&ui_state.source) {
                // Queued: a statement in full ink, not a dimmed button.
                actions = actions.child(
                    div()
                        .id("ingest-queued")
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(space::SM))
                        .h(px(ui::CONTROL_HEIGHT + 4.))
                        .px(px(space::MD))
                        .border_1()
                        .border_color(self.border())
                        .whitespace_nowrap()
                        .child(icon("check", self.icon_muted()))
                        .child(format!(
                            "Queued · starts after {}",
                            copy::source_name(&ui_state.progress)
                        ))
                        .tooltip(self.tip(
                            "Cancel import stops the running import and drops the queue",
                            "",
                        )),
                );
            } else {
                actions = actions.child(
                    self.ingest_primary(&label, blocker.is_some(), cx)
                        .tooltip(self.tip(&tip, "⌘Enter")),
                );
            }
        }
        actions = actions.child(
            self.button(
                "ingest-close",
                "",
                "close",
                Cmd::Ingest("close".into()),
                false,
                cx,
            )
            .tooltip(self.tip("Close Import", "Esc")),
        );
        footer.child(row.child(actions)).into_any_element()
    }
    fn ingest_picker(&mut self, field: usize, cx: &mut gpui::Context<Self>) {
        if cfg!(target_os = "linux") {
            let initial = if field == 4 {
                self.import_ui.fields[field].split(';').next().unwrap_or("")
            } else {
                &self.import_ui.fields[field]
            };
            self.show_path_picker(
                super::folder_picker::Purpose::Ingest(field),
                PathBuf::from(initial),
                cx,
            );
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                if field == 0 {
                    "Choose photo library"
                } else {
                    "Choose verified backup destination"
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await {
                if let Some(path) = paths.first() {
                    let _ = this.update(cx, |this, cx| {
                        if field == 4 && !this.import_ui.fields[4].is_empty() {
                            this.import_ui.fields[4].push(';');
                            this.import_ui.fields[4].push_str(&path.to_string_lossy());
                        } else {
                            this.import_ui.fields[field] = path.to_string_lossy().into_owned();
                        }
                        this.import_ui.active_field = Some(field);
                        this.refocus_ingest_field();
                        this.import_ui.changed();
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
    pub(super) fn apply_ingest_path(&mut self, field: usize, path: &std::path::Path) {
        if field >= self.import_ui.fields.len() {
            return;
        }
        if field == 4 && !self.import_ui.fields[4].is_empty() {
            self.import_ui.fields[4].push(';');
            self.import_ui.fields[4].push_str(&path.to_string_lossy());
        } else {
            self.import_ui.fields[field] = path.to_string_lossy().into_owned();
        }
        self.import_ui.active_field = Some(field);
        self.refocus_ingest_field();
        self.import_ui.changed();
    }
    pub(super) fn refocus_ingest_field(&mut self) {
        if let Some(field) = self.import_ui.active_field {
            self.text_edit.focus(&self.import_ui.fields[field]);
        }
    }
    pub(super) fn ingest_preferences(
        &self,
        window: &Window,
        cx: &gpui::Context<Self>,
    ) -> AnyElement {
        let settings = self.preference_settings();
        // Keyboard focus (Preferences tab order: body controls are index 2).
        let focusable = |element: Stateful<Div>, key: &str, cmd: Cmd| {
            self.focusable(element, key, 2, true, FocusTarget::button(cmd), window, cx)
                .into_any_element()
        };
        const INSERT: [(&str, &str, &str); 4] = [
            ("ask", "Ask", "Open Import for the card and wait for you."),
            (
                "auto",
                "Import automatically",
                "Start importing new photos to the library as soon as a card is inserted.",
            ),
            (
                "index_in_place",
                "Cull in place",
                "Index the card where it is so you can pick and rate before importing.",
            ),
            ("ignore", "Ignore", "Do nothing when a card is inserted."),
        ];
        let mode = INSERT
            .iter()
            .find(|(value, _, _)| *value == settings.ingest_on_insert)
            .unwrap_or(&INSERT[0]);
        let insert = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(space::XS))
            .child(self.pref_choices(
                "pref-ingest-insert",
                INSERT.iter().map(|(value, label, _)| {
                    (
                        (*label).to_owned(),
                        Cmd::Ingest(format!("insert:{value}")),
                        settings.ingest_on_insert == *value,
                    )
                }),
                false,
                window,
                cx,
            ))
            .child(
                div()
                    .text_size(px(ui::text::LABEL))
                    .text_color(self.muted())
                    .child(mode.2),
            );
        // Any hour in one click: two rows of twelve.
        let hours = |id: &str, range: std::ops::Range<u8>| {
            self.pref_choices(
                id,
                range.map(|hour| {
                    (
                        format!("{hour:02}"),
                        Cmd::Ingest(format!("day:{hour}")),
                        settings.ingest_day_start_hour == hour,
                    )
                }),
                false,
                window,
                cx,
            )
        };
        let day = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(space::XS))
            .child(hours("pref-ingest-day-am", 0..12))
            .child(hours("pref-ingest-day-pm", 12..24));
        let mut body = div()
            .flex()
            .flex_col()
            .child(self.preference_row(
                "On card insertion",
                "What happens when a memory card or camera is connected",
                insert.into_any_element(),
            ))
            .child(self.preference_row(
                "Day begins",
                &format!(
                    "Photos taken before {:02}:00 count as the previous day in folder names and the timeline",
                    settings.ingest_day_start_hour
                ),
                day.into_any_element(),
            ))
            .child(self.preference_row(
                "Eject after import",
                "Safely eject the card once every copy is verified",
                self.pref_choices(
                    "pref-ingest-eject",
                    [
                        (
                            "Off".to_owned(),
                            Cmd::Ingest("auto_eject:off".into()),
                            !settings.eject_after_ingest,
                        ),
                        (
                            "On".to_owned(),
                            Cmd::Ingest("auto_eject:on".into()),
                            settings.eject_after_ingest,
                        ),
                    ],
                    false,
                    window,
                    cx,
                ),
            ))
            .child(self.preference_row(
                "Notify when done",
                "A desktop notification when an import or export finishes while Shadowroom is in the background",
                self.pref_choices(
                    "pref-ingest-notify",
                    [
                        (
                            "Off".to_owned(),
                            Cmd::Ingest("notify:off".into()),
                            !settings.ingest_notify,
                        ),
                        (
                            "On".to_owned(),
                            Cmd::Ingest("notify:on".into()),
                            settings.ingest_notify,
                        ),
                    ],
                    false,
                    window,
                    cx,
                ),
            ));
        // The scanned card's rule, named, instead of an anonymous toggle.
        if let Some(scan) = &self.import_ui.scan {
            let denied = settings
                .ingest_volumes
                .get(&scan.volume)
                .is_some_and(|rule| rule == "deny");
            let name = scan
                .volume
                .strip_prefix("label:")
                .unwrap_or(&scan.volume)
                .to_owned();
            body = body.child(self.preference_row(
                &format!("Card “{name}”"),
                "Whether automatic drive discovery offers this card",
                self.pref_choices(
                    "pref-ingest-remember",
                    [
                        (
                            "Offer".to_owned(),
                            Cmd::Ingest(if denied { "remember" } else { "" }.into()),
                            !denied,
                        ),
                        (
                            "Ignore".to_owned(),
                            Cmd::Ingest(if denied { "" } else { "remember" }.into()),
                            denied,
                        ),
                    ],
                    false,
                    window,
                    cx,
                ),
            ));
        }
        let library = settings.ingest_library_root.to_string_lossy();
        body = body
            .child(
                self.preference_row(
                    "Library",
                    &copy::middle_path(&library, 56),
                    focusable(
                        self.dialog_button(
                            "pref-ingest-configure",
                            "Change in Import…",
                            ButtonKind::Secondary,
                            false,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.execute(Cmd::Ingest("show".into()), window, cx)
                        })),
                        "pref-ingest-configure",
                        Cmd::Ingest("show".into()),
                    ),
                ),
            )
            .child(
                self.preference_row(
                    "Naming",
                    &preset_detail(
                        &settings.ingest_folder_template,
                        &settings.ingest_file_template,
                    ),
                    div()
                        .text_size(px(ui::text::LABEL))
                        .text_color(self.muted())
                        .child("Set in Import → Naming")
                        .into_any_element(),
                ),
            );
        body.into_any_element()
    }
}

pub(super) struct MenuOption {
    label: String,
    detail: String,
    action: String,
    active: bool,
}

/// Posts a notification through the desktop's own service: `notify-send`
/// (freedesktop notifications) on Linux, Notification Center through
/// `osascript` on macOS. The helper runs detached and is reaped off the UI
/// thread.
fn desktop_notification(summary: &str, body: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
        let mut command = std::process::Command::new("osascript");
        command.arg("-e").arg(format!(
            "display notification \"{}\" with title \"Shadowroom\" subtitle \"{}\"",
            quote(body),
            quote(summary)
        ));
        command
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = {
        let mut command = std::process::Command::new("notify-send");
        command
            .arg("--app-name=Shadowroom")
            .arg("--icon=app.shadowroom.Shadowroom")
            .arg("--")
            .arg(summary)
            .arg(body);
        command
    };
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("shadowroom-notify".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

fn preset_detail(folder: &str, file: &str) -> String {
    if folder.is_empty() {
        file.to_owned()
    } else {
        format!("{folder}/{file}")
    }
}

fn selection_icon(selected: usize, total: usize) -> &'static str {
    if selected == 0 {
        "select"
    } else if selected == total {
        "selected"
    } else {
        "partial"
    }
}

/// Tab order of the destination fields: Job code (the per-shoot field)
/// first, then library, backups and the two naming templates.
const FIELD_ORDER: [usize; 5] = [3, 0, 4, 1, 2];

fn next_field(field: usize, back: bool) -> usize {
    let at = FIELD_ORDER.iter().position(|&f| f == field).unwrap_or(0);
    let next = (if back {
        at + FIELD_ORDER.len() - 1
    } else {
        at + 1
    }) % FIELD_ORDER.len();
    FIELD_ORDER[next]
}

#[cfg(test)]
mod responsive_tests {
    use super::{
        GridPick, IMPORT_RESIZE_HANDLE, ImportLayout, ImportUi, ingest, selection_icon, space, ui,
    };

    fn import_fixture() -> ImportUi {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photos");
        std::fs::create_dir(&source).unwrap();
        for index in 0..12 {
            image::RgbImage::new(4, 4)
                .save(source.join(format!("{index:02}.png")))
                .unwrap();
        }
        let catalog = shadowroom::catalog::Catalog::open(&temp.path().join("catalog.db")).unwrap();
        let scan = ingest::scan(&source, &catalog, |_| {}, &ingest::Control::default()).unwrap();
        let mut state = ImportUi::new(&shadowroom::settings::Settings::default());
        for group in &scan.groups {
            state.append(group.clone());
        }
        state.timeline = ingest::timeline(&scan, 60);
        state.timeline_counts();
        state
    }

    #[test]
    fn scan_requests_always_leave_scanning_and_never_rescan_the_copying_card() {
        use super::{Phase, ScanState};
        use std::path::PathBuf;
        let mut state = ImportUi::new(&shadowroom::settings::Settings::default());
        assert!(state.request_scan(PathBuf::from("/card")));
        assert!(state.scanning());
        // Queue full / worker error: the view must not stay "scanning" forever.
        state.scan_failed("queue full".into());
        assert!(!state.scanning());
        assert_eq!(state.scan_state, ScanState::Failed);
        assert!(state.request_scan(PathBuf::from("/card")));
        assert!(state.scanning());
        state.scan_state = ScanState::Ready;
        state.progress.phase = Phase::Copying;
        state.progress.source = Some(PathBuf::from("/card"));
        // The card being copied is never scanned again: the view stays.
        assert!(!state.request_scan(PathBuf::from("/card")));
        assert_eq!(state.source, PathBuf::from("/card"));
        assert!(!state.scanning());
        assert!(state.job_here());
        // An unrelated worker error after a finished scan keeps the scan usable.
        state.scan_failed("eject failed".into());
        assert_eq!(state.scan_state, ScanState::Ready);
        // A second card scans while the first copies, to be reviewed and queued.
        assert!(state.request_scan(PathBuf::from("/card-b")));
        assert!(state.scanning());
        assert_eq!(state.source, PathBuf::from("/card-b"));
        assert!(state.active() && !state.job_here());
    }

    #[test]
    fn drive_rows_show_copying_queued_and_done_states() {
        use super::Phase;
        use shadowroom::ingest::worker::QueuedJob;
        use std::path::{Path, PathBuf};
        let mut state = ImportUi::new(&shadowroom::settings::Settings::default());
        state.progress.phase = Phase::Copying;
        state.progress.source = Some(PathBuf::from("/media/A"));
        state.progress.transferred = 40;
        state.progress.total_bytes = 100;
        state.queue = vec![QueuedJob {
            source: PathBuf::from("/media/B"),
            volume: "label:B".into(),
            photos: 3,
            videos: 0,
            bytes: 9,
        }];
        assert_eq!(
            state.drive_state(Path::new("/media/A")).as_deref(),
            Some("Copying 40%")
        );
        assert_eq!(
            state.drive_state(Path::new("/media/B")).as_deref(),
            Some("Queued")
        );
        assert_eq!(state.drive_state(Path::new("/media/C")), None);
        state.progress.phase = Phase::Paused;
        state.progress.source_missing = true;
        assert_eq!(
            state.drive_state(Path::new("/media/A")).as_deref(),
            Some("Paused 40%")
        );
        state.progress.phase = Phase::Completed;
        state.done_source = Some(PathBuf::from("/media/A"));
        assert_eq!(
            state.drive_state(Path::new("/media/A")).as_deref(),
            Some("Done · safe to eject")
        );
    }

    /// Leaves nothing checked, as Uncheck all does.
    fn uncheck_all(state: &mut ImportUi) {
        let ids: Vec<_> = state.selected.iter().copied().collect();
        for id in ids {
            state.toggle(id);
        }
    }

    #[test]
    fn plain_clicks_and_arrows_move_the_current_tile_without_changing_the_import() {
        let mut state = import_fixture();
        let all = state.selected.clone();
        assert_eq!(all.len(), 12);
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        state.select_position(3, GridPick::Focus);
        assert_eq!(state.focused, Some(ids[3]));
        assert_eq!(state.navigate("right", 3, false), Some(4));
        assert_eq!(state.navigate("right", 3, false), Some(5));
        assert_eq!(state.navigate("down", 3, false), Some(8));
        assert_eq!(state.selected, all, "looking never shrinks the import");
        assert_eq!(state.focused, Some(ids[8]));
        // Space (and the checkbox / Ctrl-click) toggles only the current tile.
        assert_eq!(state.toggle_focused(), Some(8));
        assert_eq!(state.selected.len(), 11);
        assert!(!state.selected.contains(&ids[8]));
        assert_eq!(state.photos, 11);
        state.select_position(8, GridPick::Toggle);
        assert_eq!(state.selected, all);
        assert_eq!(state.timeline_selected.iter().sum::<usize>(), 12);
    }

    #[test]
    fn range_selection_counts_shrink_and_survive_filtered_positions() {
        let mut state = import_fixture();
        uncheck_all(&mut state);
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        state.select_position(2, GridPick::Toggle);
        state.select_position(6, GridPick::Range);
        assert_eq!(state.selected, ids[2..=6].iter().copied().collect());
        assert_eq!(state.photos, 5);
        assert!(state.timeline_selected.iter().any(|&n| n > 0));
        assert!(
            state
                .day_counts
                .values()
                .any(|&(n, total)| selection_icon(n, total) == "partial")
        );
        // Shrinking the range restores the photos that left it.
        state.select_position(2, GridPick::Range);
        assert_eq!(state.selected, [ids[2]].into_iter().collect());
        assert_eq!(state.photos, 1);
        state.visible = vec![2, 4, 6, 8, 10];
        state.select_position(2, GridPick::Range);
        assert_eq!(
            state.selected,
            [ids[2], ids[4], ids[6]].into_iter().collect()
        );
        assert_eq!(state.photos, 3);
        assert_eq!(state.timeline_selected.iter().sum::<usize>(), 3);
    }

    #[test]
    fn a_range_from_an_unchecked_anchor_leaves_photos_out() {
        let mut state = import_fixture();
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        state.select_position(4, GridPick::Toggle);
        assert!(!state.selected.contains(&ids[4]));
        state.select_position(7, GridPick::Range);
        assert_eq!(state.selected.len(), 8);
        assert!(ids[4..=7].iter().all(|id| !state.selected.contains(id)));
        assert_eq!(state.photos, 8);
    }

    #[test]
    fn first_shift_navigation_establishes_an_anchor_and_range_reverses() {
        let mut state = import_fixture();
        uncheck_all(&mut state);
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        // The first Shift-arrow anchors on the first photo with its state.
        state.select_position(0, GridPick::Toggle);
        assert_eq!(state.navigate("down", 3, true), Some(3));
        assert_eq!(state.anchor, Some(ids[0]));
        assert_eq!(state.selected, ids[..=3].iter().copied().collect());
        assert_eq!(state.navigate("down", 3, true), Some(6));
        assert_eq!(state.selected, ids[..=6].iter().copied().collect());
        assert_eq!(state.navigate("up", 3, true), Some(3));
        assert_eq!(state.selected, ids[..=3].iter().copied().collect());
        assert_eq!(state.photos, 4);
        assert_eq!(state.timeline_selected.iter().sum::<usize>(), 4);
        assert_eq!(state.navigate("end", 3, true), Some(11));
        assert!(state.day_counts.values().all(|&(n, total)| n == total));
        // A plain arrow ends the range without touching the import.
        assert_eq!(state.navigate("left", 3, false), Some(10));
        assert_eq!(state.photos, 12);
        assert_eq!(state.anchor, Some(ids[10]));
    }

    #[test]
    fn navigation_starts_at_first_visible_photo_and_ignores_hidden_anchors() {
        let mut state = import_fixture();
        uncheck_all(&mut state);
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        assert_eq!(state.navigate("down", 3, false), Some(0));
        assert_eq!(state.focused, Some(ids[0]));
        assert!(state.selected.is_empty());
        state.select_position(2, GridPick::Toggle);
        state.select_position(4, GridPick::Toggle);
        assert_eq!(state.selected, [ids[2], ids[4]].into_iter().collect());
        state.anchor = Some(ids[0]);
        state.visible = vec![2, 4, 6, 8, 10];
        state.navigate("down", 2, true);
        assert_eq!(state.anchor, Some(ids[4]));
        assert_eq!(
            state.selected,
            [ids[2], ids[4], ids[6], ids[8]].into_iter().collect()
        );
        state.visible.clear();
        assert_eq!(state.navigate("end", 3, true), None);
        assert_eq!(state.toggle_focused(), None);
    }

    #[test]
    fn tab_order_starts_at_the_job_code() {
        use super::next_field;
        assert_eq!(next_field(3, false), 0);
        assert_eq!(next_field(2, false), 3);
        assert_eq!(next_field(3, true), 2);
    }

    /// Data-side cost of one Import grid frame (36 visible tiles) over a 5k plan:
    /// the former per-tile `IngestGroup`/`Photo` clone plus text formatting
    /// against the cached cell model. Run with
    /// `cargo test --release --bins grid_cell_frame_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn grid_cell_frame_cost() {
        use std::{hint::black_box, time::Instant};
        let base = import_fixture();
        let mut state = ImportUi::new(&shadowroom::settings::Settings::default());
        for n in 0..5000 {
            let mut group = base.items[n % base.items.len()].clone();
            group.id = n;
            group.primary.path = format!("/card/DCIM/100CANON/IMG_{n:05}.CR3").into();
            group.photo.filename = format!("IMG_{n:05}.CR3");
            state.append(group);
        }
        let frames = 2000;
        let visible: Vec<usize> = (2400..2436).collect();
        let started = Instant::now();
        for _ in 0..frames {
            for &index in &visible {
                // The removed per-frame work, verbatim in substance.
                let group = state.items[index].clone();
                let photo = state.previews[index].clone();
                let tip = group.seen.as_ref().map_or_else(
                    || {
                        group
                            .warning
                            .clone()
                            .unwrap_or_else(|| group.primary.path.display().to_string())
                    },
                    |s| s.job_id.clone(),
                );
                let detail = format!("{:.1} MB", group.bytes() as f64 / 1_000_000.);
                let name: gpui::SharedString = group.photo.filename.clone().into();
                let tip: gpui::SharedString = tip.as_str().to_owned().into();
                black_box((&group, &photo, tip, detail, name));
            }
        }
        let before = started.elapsed();
        let started = Instant::now();
        for _ in 0..frames {
            for &index in &visible {
                let cell = state.cells[index].clone();
                black_box((&state.previews[index], cell));
            }
        }
        let after = started.elapsed();
        println!(
            "grid cell data per frame (36 tiles, 5k plan): before {:?}, after {:?}",
            before / frames,
            after / frames
        );
    }

    #[test]
    fn marking_groups_keeps_other_choices_and_refreshes_only_marked_tiles() {
        let mut state = import_fixture();
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        // The user deselects everything, then checks three tiles.
        for &id in &ids {
            if state.selected.contains(&id) {
                state.toggle(id);
            }
        }
        for &id in &ids[3..6] {
            state.toggle(id);
        }
        state.new_only = true;
        let seen = ingest::store::SeenRecord {
            record_id: 1,
            imported_at: 0,
            destination: None,
            job_id: String::new(),
            matched_by: String::new(),
        };
        let new_before = state.new_count;
        state.committed(vec![(ids[4], seen)]);
        assert_eq!(state.selected, [ids[3], ids[5]].into_iter().collect());
        assert_eq!(state.photos, 2);
        assert_eq!(state.timeline_selected.iter().sum::<usize>(), 2);
        assert_eq!(state.new_count, new_before - 1);
        assert!(state.cells[4].seen && state.cells[4].seen_label.is_some());
        assert!(!state.cells[3].seen);
        assert!(!state.visible.contains(&4));
        assert!(state.dirty_at.is_some(), "a mark must trigger a replan");
    }

    #[test]
    fn completing_a_scan_preserves_choices_from_batches_and_defaults_unseen_groups() {
        let fixture = import_fixture();
        let mut groups = fixture.items;
        groups[0].selected = false;
        let mut state = ImportUi::new(&shadowroom::settings::Settings::default());
        state.append(groups[0].clone());
        state.append(groups[1].clone());
        state.toggle(groups[0].id); // explicitly select a default-unselected group
        state.toggle(groups[1].id); // explicitly deselect a default-selected group
        state.scanned(
            std::sync::Arc::new(ingest::Scan {
                groups: groups.clone(),
                ..Default::default()
            }),
            60,
        );
        let expected: std::collections::HashSet<_> = groups
            .iter()
            .filter(|g| g.id == groups[0].id || (g.id != groups[1].id && g.selected))
            .map(|g| g.id)
            .collect();
        assert_eq!(state.selected, expected);
        assert_eq!(state.photos, expected.len());
        assert_eq!(
            state.timeline_selected.iter().sum::<usize>(),
            expected.len()
        );
        assert_eq!(state.cells.len(), state.items.len());
        assert!(!state.scanning());
        assert!(state.dirty_at.is_some());
    }

    #[test]
    fn hidden_import_panels_do_not_consume_grid_width() {
        for width in [0., 320., 599., 600., 879., 880., 1280.] {
            let layout = ImportLayout::new(width, 160., 304.);
            assert_eq!(layout.timeline > 0., width >= 880.);
            assert_eq!(layout.destination > 0., width >= 600.);
            let handles = if layout.timeline > 0. {
                IMPORT_RESIZE_HANDLE
            } else {
                0.
            } + if layout.destination > 0. {
                IMPORT_RESIZE_HANDLE
            } else {
                0.
            };
            assert_eq!(
                layout.timeline + layout.destination + layout.grid + handles,
                width
            );
        }
    }

    #[test]
    fn resized_import_panels_leave_room_for_grid_cells_and_gaps() {
        for width in [320., 599., 600., 879., 880., 1024., 1920.] {
            for timeline in [0., 160., 1000.] {
                for destination in [0., 304., 1000.] {
                    let layout = ImportLayout::new(width, timeline, destination);
                    let columns = layout.columns() as f32;
                    let cells = columns * 180. + (columns - 1.) * space::SM;
                    assert!(cells + 2. * ui::BAR_INSET <= layout.grid);
                    assert!((1..=6).contains(&layout.columns()));
                }
            }
        }
    }
}
