use super::*;
use shadowroom::ingest::{
    self, IngestGroup, IngestOptions, Progress, Scan,
    worker::{Event, PlanSummary},
};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportPanel {
    Photos,
    Timeline,
    Destination,
}

const IMPORT_RESIZE_HANDLE: f32 = space::SM;

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

pub(super) struct ImportUi {
    pub open: bool,
    pub scanning: bool,
    pub scan: Option<Arc<Scan>>,
    pub items: Vec<IngestGroup>,
    pub selected: HashSet<usize>,
    focused: Option<usize>,
    anchor: Option<usize>,
    pub lookup: HashMap<usize, usize>,
    pub photo_lookup: HashMap<i64, usize>,
    pub previews: Vec<Photo>,
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
    pub preset: usize,
    pub preset_open: bool,
    pub expected_plan: Option<IngestOptions>,
    panel: ImportPanel,
    timeline_width: f32,
    destination_width: f32,
    resizing: Option<(ImportPanel, f32, f32, f32)>,
    resize_moved: bool,
    suppress_resize_click: bool,
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
            scanning: false,
            scan: None,
            items: Vec::new(),
            selected: HashSet::new(),
            focused: None,
            anchor: None,
            lookup: HashMap::new(),
            photo_lookup: HashMap::new(),
            previews: Vec::new(),
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
            preset: 1,
            preset_open: false,
            expected_plan: None,
            panel: ImportPanel::Photos,
            timeline_width: ui::INGEST_TIMELINE_WIDTH,
            destination_width: ui::INGEST_DESTINATION_WIDTH,
            resizing: None,
            resize_moved: false,
            suppress_resize_click: false,
        }
    }
    fn append(&mut self, group: IngestGroup) {
        let index = self.items.len();
        if group.seen.is_none() {
            self.new_count += 1;
        }
        if group.selected {
            self.select_count(&group, true);
            self.selected.insert(group.id);
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
        self.previews.push(photo);
        self.lookup.insert(group.id, index);
        if !self.new_only || group.seen.is_none() {
            self.visible.push(index);
        }
        self.items.push(group);
    }
    fn clear_items(&mut self) {
        self.items.clear();
        self.selected.clear();
        self.focused = None;
        self.anchor = None;
        self.lookup.clear();
        self.photo_lookup.clear();
        self.previews.clear();
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
    fn select_count(&mut self, group: &IngestGroup, add: bool) {
        if add {
            if group.kind == "video" {
                self.videos += 1;
            } else {
                self.photos += 1;
            }
            self.bytes += group.bytes();
        } else {
            if group.kind == "video" {
                self.videos -= 1;
            } else {
                self.photos -= 1;
            }
            self.bytes -= group.bytes();
        }
    }
    fn toggle(&mut self, id: usize) {
        if let Some(&index) = self.lookup.get(&id) {
            let group = self.items[index].clone();
            let add = if self.selected.remove(&id) {
                false
            } else {
                self.selected.insert(id);
                true
            };
            self.select_count(&group, add);
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
    fn select_position(&mut self, position: usize, extend: bool, range: bool) {
        let Some(&index) = self.visible.get(position) else {
            return;
        };
        let id = self.items[index].id;
        let visible_position = |id| self.visible.iter().position(|&i| self.items[i].id == id);
        let anchor = self
            .anchor
            .and_then(visible_position)
            .or_else(|| self.focused.and_then(visible_position))
            .unwrap_or(0);
        if range {
            // A first Shift-click/arrow starts at the first visible photo.
            // Keep that anchor when the range subsequently grows or shrinks.
            self.anchor = Some(self.items[self.visible[anchor]].id);
        }
        if range || !extend {
            let mut wanted: HashSet<_> = if range {
                self.visible[anchor.min(position)..=anchor.max(position)]
                    .iter()
                    .map(|&i| self.items[i].id)
                    .collect()
            } else {
                HashSet::from([id])
            };
            if extend {
                wanted.extend(&self.selected);
            }
            let changes: Vec<_> = self
                .selected
                .symmetric_difference(&wanted)
                .copied()
                .collect();
            for id in changes {
                self.toggle(id);
            }
        } else {
            self.toggle(id);
        }
        if !range {
            self.anchor = Some(id);
        }
        self.focused = Some(id);
    }
    fn navigate(&mut self, key: &str, columns: usize, range: bool) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let focused = self
            .focused
            .and_then(|id| self.visible.iter().position(|&i| self.items[i].id == id));
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
        self.select_position(target, false, range);
        Some(target)
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
        ["queued", "copying", "verifying", "paused", "indexing"]
            .contains(&self.progress.phase.as_str())
    }
}
impl Workspace {
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

    pub(super) fn open_ingest(&mut self, path: PathBuf) {
        self.import_ui.open = true;
        self.import_ui.panel = ImportPanel::Photos;
        self.import_ui.active_field = None;
        self.import_ui.resizing = None;
        self.import_ui.show_history = false;
        self.import_ui.error = None;
        self.import_ui.scanning = true;
        self.import_ui.source = path.clone();
        self.import_ui.plan = None;
        self.enqueue(
            if self
                .drives
                .iter()
                .any(|drive| drive.removable && drive.path == path)
            {
                IoCommand::IngestScanCard(path)
            } else {
                IoCommand::IngestScan(path)
            },
        );
        self.palette_revision = self.palette_revision.wrapping_add(1);
    }
    pub(super) fn ingest_event(&mut self, event: Event) {
        match event {
            Event::Scanning(path) => {
                self.import_ui.clear_items();
                self.import_ui.scan = None;
                self.import_ui.plan = None;
                self.import_ui.source = path;
                self.import_ui.scanning = true;
                self.import_ui.error = None;
            }
            Event::Batch(groups) => {
                for group in groups {
                    self.import_ui.append(group);
                }
            }
            Event::Scanned(scan) => {
                self.import_ui.clear_items();
                for group in &scan.groups {
                    self.import_ui.append(group.clone());
                }
                self.import_ui.timeline =
                    ingest::timeline(&scan, self.settings.ingest_time_group_minutes);
                self.import_ui.timeline_counts();
                self.import_ui.source = scan.source.clone();
                self.import_ui.scan = Some(scan);
                self.import_ui.scanning = false;
                self.import_ui.changed();
            }
            Event::Plan(plan) => {
                if self.import_ui.expected_plan.as_ref() == Some(&plan.plan.options)
                    || (self.settings.ingest_on_insert == "auto" && self.import_ui.active())
                {
                    self.import_ui.error = plan.error.clone();
                    self.import_ui.plan = Some(plan);
                }
            }
            Event::Progress(mut progress) => {
                if progress.job_id.is_empty() {
                    self.import_ui.progress.phase = progress.phase;
                } else {
                    if self.import_ui.progress.phase == "paused"
                        && ["copying", "verifying"].contains(&progress.phase.as_str())
                    {
                        progress.phase = "paused".into();
                    }
                    self.import_ui.progress = progress;
                }
                let p = &self.import_ui.progress;
                if p.phase == "failed" {
                    let error = if p.errors.is_empty() {
                        "Import failed; inspect import history for committed groups".to_string()
                    } else {
                        p.errors.join("; ")
                    };
                    self.import_ui.error = Some(error.clone());
                    self.error = Some(format!("Import: {error}"));
                }
                self.status = if p.phase == "completed" {
                    format!(
                        "Imported {}{} · all destinations verified",
                        plural(p.photos, "photo", "photos"),
                        if p.videos > 0 {
                            format!(", {}", plural(p.videos, "video", "videos"))
                        } else {
                            String::new()
                        }
                    )
                } else {
                    format!(
                        "Import {} · {}/{} · {:.0} MB/s · {} s left",
                        p.phase,
                        p.completed,
                        p.total,
                        p.mb_per_second,
                        p.eta_seconds.unwrap_or(0)
                    )
                };
                if p.phase == "completed" {
                    if self.settings.eject_after_ingest {
                        self.enqueue(IoCommand::Eject(self.import_ui.source.clone()));
                    }
                    let p = &self.import_ui.progress;
                    if let Some(root) = &p.library_root {
                        self.import_ui.open = false;
                        self.scope = None;
                        self.mode = ViewMode::Grid;
                        self.pending_ingest_scope = Some(root.clone());
                    }
                    self.ingest_scope_arrived();
                }
            }
            Event::History(history) => self.import_ui.history = Arc::new(history),
            Event::Committed(committed) => {
                for (id, seen) in committed {
                    if let Some(&index) = self.import_ui.lookup.get(&id) {
                        if self.import_ui.items[index].seen.is_none() {
                            self.import_ui.new_count = self.import_ui.new_count.saturating_sub(1);
                        }
                        if self.import_ui.selected.remove(&id) {
                            let group = self.import_ui.items[index].clone();
                            self.import_ui.select_count(&group, false);
                        }
                        self.import_ui.items[index].seen = Some(seen);
                    }
                }
                self.import_ui.timeline_counts();
                if self.import_ui.new_only {
                    self.import_ui.visible = self
                        .import_ui
                        .items
                        .iter()
                        .enumerate()
                        .filter(|(_, g)| g.seen.is_none())
                        .map(|(i, _)| i)
                        .collect();
                }
                self.import_ui.plan = None;
            }
            Event::Marked(count) => {
                self.status = format!("Marked {count} groups as imported; source unchanged")
            }
            Event::Ejected(path) => self.status = format!("Ejected {}", path.display()),
            Event::Error(error) => {
                self.import_ui.scanning = false;
                if self.import_ui.progress.phase == "queued" {
                    self.import_ui.progress.phase = "failed".into();
                }
                self.import_ui.error = Some(error.clone());
                self.error = Some(format!("Import: {error}"));
            }
        }
        self.palette_revision = self.palette_revision.wrapping_add(1);
    }
    pub(super) fn ingest_tick(&mut self) {
        if self.outbox.is_empty()
            && self.pending_saves == 0
            && self.unsaved.is_empty()
            && !self.import_ui.scanning
            && !self.import_ui.active()
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
                if self.import_ui.active() {
                    return Err("An import is running".into());
                }
                self.open_ingest(PathBuf::from(
                    params["path"].as_str().ok_or("path required")?,
                ));
                self.import_ui.scanning = true;
            }
            "ingest_plan" => {
                if !self.unsaved.is_empty() || self.pending_saves > 0 || !self.outbox.is_empty() {
                    return Err("Wait for cull decisions to finish saving before planning".into());
                }
                if self.import_ui.scanning {
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
                if !self.unsaved.is_empty() || self.pending_saves > 0 || !self.outbox.is_empty() {
                    return Err("Wait for cull decisions to finish saving before importing".into());
                }
                if self.import_ui.active() {
                    return Err("An import is already running".into());
                }
                if !self
                    .import_ui
                    .plan
                    .as_ref()
                    .is_some_and(|p| p.ready && !p.plan.groups.is_empty())
                {
                    return Err("A valid nonempty import plan is required".into());
                }
                self.import_ui.progress = Progress {
                    phase: "queued".into(),
                    ..Default::default()
                };
                if !self.enqueue(IoCommand::IngestStart) {
                    self.import_ui.progress.phase = "failed".into();
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
                    json!({"scanning":self.import_ui.scanning,"source":self.import_ui.source,"total":self.import_ui.items.len(),"items":items,"plan":plan,"progress":self.import_ui.progress,"error":self.import_ui.error,"history":self.import_ui.history.as_ref(),"warnings":self.import_ui.scan.as_ref().map(|s| &s.warnings),"clock":self.import_ui.scan.as_ref().map(|s| s.clock)}),
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
                if !self.import_ui.active() {
                    self.import_ui.changed();
                }
            }
            "history" => {
                self.import_ui.show_history = true;
                self.import_ui.open = true;
                self.enqueue(IoCommand::IngestHistory(100));
            }
            "close" => {
                self.import_ui.open = false;
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
                if !self.import_ui.active() {
                    let options = self.import_ui.options();
                    if let Err(error) = self.ingest_api("ingest_plan", &json!({"options": options}))
                    {
                        self.import_ui.error = Some(error);
                    }
                }
            }
            "all" | "none" => {
                self.import_ui.selected.clear();
                self.import_ui.photos = 0;
                self.import_ui.videos = 0;
                self.import_ui.bytes = 0;
                if action == "all" {
                    for &index in &self.import_ui.visible {
                        let group = &self.import_ui.items[index];
                        self.import_ui.selected.insert(group.id);
                        if group.kind == "video" {
                            self.import_ui.videos += 1;
                        } else {
                            self.import_ui.photos += 1;
                        }
                        self.import_ui.bytes += group.bytes();
                    }
                }
                self.import_ui.timeline_counts();
                self.import_ui.changed();
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
            "preset" => self.import_ui.preset_open = !self.import_ui.preset_open,
            "save_preset" => {
                let name = if self.import_ui.fields[3].trim().is_empty() {
                    "Custom"
                } else {
                    self.import_ui.fields[3].trim()
                }
                .to_string();
                self.settings.ingest_presets.insert(
                    name,
                    (
                        self.import_ui.fields[1].clone(),
                        self.import_ui.fields[2].clone(),
                    ),
                );
                self.save_settings();
            }
            "videos" => {
                self.import_ui.options.include_videos = !self.import_ui.options.include_videos;
                self.import_ui.changed();
            }
            "collision" => {
                self.import_ui.options.collision =
                    if self.import_ui.options.collision == "suffix" {
                        "skip"
                    } else {
                        "suffix"
                    }
                    .into();
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
                self.status = "Cull in place, then Show Import Panel and choose Picks / Except rejects / Rated".into();
            }
            "picks" => {
                self.import_ui.open = true;
                self.import_ui.options.selection = "picks".into();
                self.import_ui.changed();
            }
            "selection" => {
                self.import_ui.options.selection =
                    match self.import_ui.options.selection.as_str() {
                        "all" => "picks",
                        "picks" => "except_rejects",
                        "except_rejects" => "rated",
                        _ => "all",
                    }
                    .into();
                self.import_ui.changed();
            }
            "rating" => {
                self.import_ui.options.min_rating = self.import_ui.options.min_rating % 5 + 1;
                self.import_ui.changed();
            }
            "mark" => {
                self.enqueue(IoCommand::IngestMarkImported(
                    self.import_ui.selected.iter().copied().collect(),
                ));
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
                self.status = "Import defaults saved".into();
            }
            "start" => {
                if let Err(error) = self.ingest_api("ingest_start", &json!({})) {
                    self.error = Some(error);
                }
            }
            "pause" => {
                self.enqueue(if self.import_ui.progress.phase == "paused" {
                    IoCommand::IngestResume
                } else {
                    IoCommand::IngestPause
                });
            }
            "cancel" => {
                self.enqueue(IoCommand::IngestCancel);
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
                        self.import_ui.preset = index;
                        self.import_ui.fields[1] = folder.to_string();
                        self.import_ui.fields[2] = file.to_string();
                    }
                    self.import_ui.preset_open = false;
                    self.refocus_ingest_field();
                    self.import_ui.changed();
                }
            }
            action if action.starts_with("custom:") => {
                if let Some((folder, file)) = self.settings.ingest_presets.get(&action[7..]) {
                    self.import_ui.fields[1] = folder.clone();
                    self.import_ui.fields[2] = file.clone();
                    self.import_ui.preset_open = false;
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
            if key == "tab" {
                let next = (field + if modifiers.shift { 4 } else { 1 }) % 5;
                self.import_ui.active_field = Some(next);
                self.text_edit.focus(&self.import_ui.fields[next]);
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
            if primary && key == "a" {
                self.ingest_action("all", cx);
            } else if !primary && !m.alt && !self.import_ui.visible.is_empty() {
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
                range
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
                                let group = this.import_ui.items[index].clone();
                                let photo = this.import_ui.previews[index].clone();
                                let selected = this.import_ui.selected.contains(&group.id);
                                let has_preview =
                                    group.kind == "photo" || photo.path != group.primary.path;
                                let texture = if has_preview {
                                    this.request_preview(&photo, PreviewKind::Thumbnail);
                                    this.texture_photo(&photo, PreviewKind::Thumbnail, cx)
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
                                // Badge only what differs from a plain still.
                                let badge = if group.kind == "video" {
                                    Some("VIDEO")
                                } else if group.companion.is_some() {
                                    Some("RAW+JPEG")
                                } else {
                                    None
                                };
                                if let Some(badge) = badge {
                                    image = image.child(
                                        div()
                                            .absolute()
                                            .bottom(px(3.))
                                            .right(px(4.))
                                            .px(px(space::XS))
                                            .bg(this.panel())
                                            .text_size(px(10.))
                                            .child(badge),
                                    );
                                }
                                let id = group.id;
                                let tip = group
                                    .seen
                                    .as_ref()
                                    .map(|s| {
                                        format!(
                                            "Imported {} → {}",
                                            chrono::DateTime::from_timestamp_millis(s.imported_at)
                                                .map_or_else(
                                                    || "previously".into(),
                                                    |t| t.format("%Y-%m-%d").to_string()
                                                ),
                                            s.destination.as_ref().map_or_else(
                                                || "marked by user".into(),
                                                |p| p.display().to_string()
                                            )
                                        )
                                    })
                                    .unwrap_or_else(|| {
                                        group.warning.clone().unwrap_or_else(|| {
                                            group.primary.path.display().to_string()
                                        })
                                    });
                                line = line.child(
                                    div()
                                        .id(("ingest-item", index))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(if selected {
                                            this.accent()
                                        } else {
                                            this.border()
                                        })
                                        .when(group.seen.is_some(), |s| s.opacity(0.5))
                                        .cursor_pointer()
                                        .tooltip(this.tip(&tip, ""))
                                        .child(image)
                                        .child(
                                            div()
                                                .px(px(space::SM))
                                                .py(px(space::XS))
                                                .flex()
                                                .gap(px(space::XS))
                                                .child(
                                                    icon(
                                                        if selected {
                                                            "selected"
                                                        } else {
                                                            "select"
                                                        },
                                                        this.icon_muted(),
                                                    )
                                                    .id(("ingest-checkbox", index))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.import_ui.toggle(id);
                                                        this.import_ui.focused = Some(id);
                                                        this.import_ui.anchor = Some(id);
                                                        this.import_ui.active_field = None;
                                                        cx.stop_propagation();
                                                        cx.notify();
                                                    })),
                                                )
                                                .child(
                                                    div()
                                                        .min_w(px(0.))
                                                        .text_ellipsis()
                                                        .whitespace_normal()
                                                        .line_clamp(1)
                                                        .overflow_hidden()
                                                        .child(group.photo.filename.clone()),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .px(px(space::SM))
                                                .text_size(px(10.))
                                                .text_color(this.muted())
                                                .child(format!(
                                                    "{:.1} MB{}",
                                                    group.bytes() as f64 / 1_000_000.,
                                                    if group.seen.is_some() {
                                                        " · Imported"
                                                    } else {
                                                        ""
                                                    }
                                                )),
                                        )
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, _, cx| {
                                                this.import_ui.active_field = None;
                                                let m = event.modifiers();
                                                this.import_ui.select_position(
                                                    row * columns + col,
                                                    m.control || m.platform,
                                                    m.shift,
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
                    .collect()
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
    fn ingest_option_button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        action: String,
        active: bool,
        cx: &gpui::Context<Self>,
    ) -> Stateful<Div> {
        self.button(id, label, "", Cmd::Ingest(action), active, cx)
            .w_full()
            .min_w(px(0.))
            .justify_start()
            .overflow_hidden()
            .whitespace_nowrap()
            .tooltip(self.tip(label, ""))
    }
    fn ingest_selection_controls(&self, cx: &gpui::Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_shrink_0()
            .gap(px(space::XS))
            .child(self.button(
                "ingest-all",
                "All",
                "",
                Cmd::Ingest("all".into()),
                false,
                cx,
            ))
            .child(self.button(
                "ingest-none",
                "None",
                "",
                Cmd::Ingest("none".into()),
                false,
                cx,
            ))
            .child(self.button(
                "ingest-new",
                if self.import_ui.new_only {
                    "New only"
                } else {
                    "Show all"
                },
                "",
                Cmd::Ingest("new".into()),
                self.import_ui.new_only,
                cx,
            ))
            .into_any_element()
    }
    pub(super) fn ingest_view(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        if self.import_ui.show_history {
            let history = self.import_ui.history.clone();
            let jobs_len = history["jobs"].as_array().map_or(0, Vec::len);
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
                    uniform_list(
                        "ingest-history",
                        jobs_len,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, _| {
                            let jobs = history["jobs"].as_array().expect("history jobs");
                            range
                                .map(|i| {
                                    div()
                                        .h(px(54.))
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .border_b_1()
                                        .border_color(this.border())
                                        .child(format!(
                                            "{} · {} · {} groups",
                                            jobs[i]["id"].as_str().unwrap_or(""),
                                            jobs[i]["verification"].as_str().unwrap_or(""),
                                            jobs[i]["counts"]["completed"].as_u64().unwrap_or(0)
                                        ))
                                        .child(
                                            div()
                                                .text_ellipsis()
                                                .whitespace_normal()
                                                .line_clamp(1)
                                                .text_color(this.muted())
                                                .child(
                                                    jobs[i]["source"]
                                                        .as_str()
                                                        .unwrap_or("")
                                                        .to_string(),
                                                ),
                                        )
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .min_h(px(0.)),
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

                        div()
                            .id(("ingest-time", index))
                            .h(px(66.))
                            .px(px(ui::PANEL_INSET))
                            .py(px(space::SM))
                            .overflow_hidden()
                            .border_b_1()
                            .border_color(this.border())
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(group.month.clone())
                                    .child(
                                        div()
                                            .id(("ingest-day", index))
                                            .flex()
                                            .items_center()
                                            .gap(px(space::XS))
                                            .child(group.day[8..].to_string())
                                            .child(icon(
                                                this.import_ui.day_counts.get(&group.day).map_or(
                                                    "select",
                                                    |&(selected, total)| {
                                                        selection_icon(selected, total)
                                                    },
                                                ),
                                                this.icon_muted(),
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                let day =
                                                    this.import_ui.timeline[index].day.clone();
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
                                                for id in ids {
                                                    if this.import_ui.selected.contains(&id) == all
                                                    {
                                                        this.import_ui.toggle(id);
                                                    }
                                                }
                                                cx.stop_propagation();
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
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
                                    .child(format!("{} · {} files", group.label, group.ids.len())),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let ids = this.import_ui.timeline[index].ids.clone();
                                let all = ids.iter().all(|id| this.import_ui.selected.contains(id));
                                for &id in &ids {
                                    if this.import_ui.selected.contains(&id) == all {
                                        this.import_ui.toggle(id);
                                    }
                                }
                                if let Some(id) =
                                    ids.first().and_then(|id| this.import_ui.lookup.get(id))
                                {
                                    if let Some(position) =
                                        this.import_ui.visible.iter().position(|i| i == id)
                                    {
                                        this.import_ui.scroll.scroll_to_item(
                                            position / this.ingest_columns(),
                                            ScrollStrategy::Top,
                                        );
                                    }
                                }
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
        let mut destination = div()
            .id("ingest-destination")
            .w(px(if focus_destination {
                self.center_size.0.max(0.)
            } else {
                layout.destination
            }))
            .min_w(px(0.))
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .overflow_y_scroll()
            .p(px(ui::BAR_INSET))
            .border_color(self.border())
            .flex()
            .flex_col()
            .gap(px(space::LG))
            .child(
                div()
                    .px(px(space::SM))
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Destination"),
            );
        for (n, label) in [
            "Library root",
            "Subfolder template",
            "Filename template",
            "Job code",
            "Backup roots (semicolon separated)",
        ]
        .into_iter()
        .enumerate()
        {
            let mut field = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .min_w(px(0.))
                .px(px(space::SM))
                .gap(px(space::XS))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(self.muted())
                        .child(label),
                )
                .child(
                    div()
                        .id(("ingest-field", n))
                        .h(px(ui::FIELD_HEIGHT))
                        .flex_shrink_0()
                        .min_w(px(0.))
                        .p(px(ui::FIELD_INSET))
                        .border_1()
                        .border_color(if self.import_ui.active_field == Some(n) {
                            self.accent()
                        } else {
                            self.border()
                        })
                        .bg(self.bg())
                        .overflow_hidden()
                        .cursor(CursorStyle::IBeam)
                        .child(self.input_text(
                            &if (n == 0 || n == 4) && self.import_ui.active_field != Some(n) {
                                tail_ellipsis(&self.import_ui.fields[n], 34)
                            } else {
                                self.import_ui.fields[n].clone()
                            },
                            "",
                            self.import_ui.active_field == Some(n),
                        ))
                        .tooltip(self.tip(&self.import_ui.fields[n], ""))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.import_ui.active_field = Some(n);
                            this.text_edit.focus(&this.import_ui.fields[n]);
                            cx.notify();
                        })),
                );
            if n == 0 || n == 4 {
                field = field.child(
                    div()
                        .id(("ingest-pick", n))
                        .cursor_pointer()
                        .text_color(self.muted())
                        .child(if n == 0 {
                            "Choose library…"
                        } else {
                            "+ Add backup…"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.ingest_picker(n, cx))),
                );
            }
            destination = destination.child(field);
        }
        if let Some(plan) = &self.import_ui.plan {
            destination = destination.child(
                div()
                    .px(px(space::SM))
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .child(format!(
                        "Example: {}",
                        plan.plan.example.as_ref().map_or_else(
                            || "No selection".into(),
                            // Relative to the library root: the folder/file pattern is what matters.
                            |p| {
                                let root = std::path::Path::new(&self.import_ui.fields[0]);
                                p.strip_prefix(root)
                                    .map(|r| format!("…/{}", r.display()))
                                    .unwrap_or_else(|_| p.display().to_string())
                            }
                        )
                    )),
            );
            for capacity in &plan.space {
                let remaining = capacity.remaining as f64 / 1_000_000_000.;
                destination = destination
                    .child(
                        div()
                            .px(px(space::SM))
                            .flex_shrink_0()
                            .text_size(px(10.))
                            .text_color(if capacity.sufficient {
                                self.muted()
                            } else {
                                self.c("error", "#d46a6a")
                            })
                            .child(format!(
                                "{} · {:.2} GB after import",
                                capacity.root.display(),
                                remaining
                            )),
                    )
                    .child(
                        div()
                            .h(px(3.))
                            .flex_shrink_0()
                            .w_full()
                            .bg(self.border())
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(
                                        (capacity.remaining as f64
                                            / capacity.available.max(1) as f64)
                                            .clamp(0., 1.)
                                            as f32,
                                    ))
                                    .bg(self.accent()),
                            ),
                    );
            }
        }
        if let Some(error) = &self.import_ui.error {
            destination = destination.child(
                div()
                    .px(px(space::SM))
                    .flex_shrink_0()
                    .text_color(self.c("error", "#d46a6a"))
                    .child(error.clone()),
            );
        }
        if self.import_ui.preset_open {
            for (index, (label, _, _)) in ingest::naming::PRESETS.iter().enumerate() {
                destination = destination.child(self.ingest_option_button(
                    SharedString::from(format!("ingest-preset-{index}")),
                    label,
                    format!("preset:{index}"),
                    self.import_ui.preset == index,
                    cx,
                ));
            }
            for (index, (label, _)) in self.settings.ingest_presets.iter().enumerate() {
                destination = destination.child(self.ingest_option_button(
                    SharedString::from(format!("ingest-custom-{index}")),
                    label,
                    format!("custom:{label}"),
                    false,
                    cx,
                ));
            }
            destination = destination.child(self.ingest_option_button(
                "ingest-save-preset",
                "Save custom preset (job code / Custom)",
                "save_preset".into(),
                false,
                cx,
            ));
        }
        if self.import_ui.active_field == Some(3) {
            for (index, code) in self.settings.ingest_job_codes.iter().take(5).enumerate() {
                destination = destination.child(self.ingest_option_button(
                    SharedString::from(format!("ingest-job-{index}")),
                    code,
                    format!("job:{code}"),
                    false,
                    cx,
                ));
            }
        }
        let buttons = [
            (
                "preset",
                format!(
                    "Preset: {} ▾",
                    ingest::naming::PRESETS[self.import_ui.preset].0
                ),
            ),
            (
                "videos",
                format!("Videos: {}", self.import_ui.options.include_videos),
            ),
            (
                "collision",
                format!("Collisions: {}", self.import_ui.options.collision),
            ),
            (
                "selection",
                format!("Import: {} ▾", self.import_ui.options.selection),
            ),
            (
                "rating",
                format!("Rated ≥ {}", self.import_ui.options.min_rating),
            ),
            ("save", "Save as defaults".into()),
            ("cull", "Cull first".into()),
            ("mark", "Mark as imported".into()),
        ];
        for (action, label) in buttons {
            destination = destination.child(self.ingest_option_button(
                SharedString::from(format!("ingest-{action}")),
                &label,
                action.into(),
                false,
                cx,
            ));
        }
        let label = self.import_ui.plan.as_ref().map_or_else(
            || {
                format!(
                    "Import {} photos · {} videos ({:.2} GB)",
                    self.import_ui.photos,
                    self.import_ui.videos,
                    self.import_ui.bytes as f64 / 1e9
                )
            },
            |s| {
                format!(
                    "Import {} photos · {} videos ({:.2} GB)",
                    s.plan.photos,
                    s.plan.videos,
                    s.plan.bytes as f64 / 1e9
                )
            },
        );
        destination = destination
            .child(self.ingest_option_button(
                "ingest-eject",
                "Eject card",
                "eject".into(),
                false,
                cx,
            ))
            .child(self.ingest_option_button(
                "ingest-hide",
                "Close panel",
                "close".into(),
                false,
                cx,
            ));
        let title = self
            .import_ui
            .source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let short = self.center_size.1 < 360.;
        let source_label = format!(
            "Import from {title}{}",
            if self.import_ui.scanning {
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
                    .child(self.button(
                        "ingest-gap",
                        &format!("Gap: {} min", self.settings.ingest_time_group_minutes),
                        "",
                        Cmd::Ingest("gap".into()),
                        false,
                        cx,
                    ).tooltip(self.tip("Time grouping: consecutive photos stay together when the gap between captures is at most this value. Click to change it.", ""))),
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
        let active = self.import_ui.active();
        let plan_status = if active {
            format!(
                "{} · {}/{}",
                self.import_ui.progress.phase,
                self.import_ui.progress.completed,
                self.import_ui.progress.total
            )
        } else if self.import_ui.scanning {
            "Scanning…".into()
        } else if self.import_ui.error.is_some() {
            "Import needs attention".into()
        } else if let Some(plan) = &self.import_ui.plan {
            if plan.ready {
                format!("Plan ready · {} files", plan.plan.files)
            } else {
                "Plan needs attention".into()
            }
        } else {
            "Planning…".into()
        };
        let summary = format!(
            "{} selected · {}/{} shown · {plan_status}",
            self.import_ui.selected.len(),
            self.import_ui.visible.len(),
            self.import_ui.items.len()
        );
        let mut actions = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(space::XS))
            .when(narrow, |s| s.w_full().justify_between());
        if active {
            actions = actions
                .child(self.button(
                    "ingest-pause",
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
                .child(self.button(
                    "ingest-cancel",
                    "Cancel",
                    "",
                    Cmd::Ingest("cancel".into()),
                    false,
                    cx,
                ));
        } else {
            actions = actions
                .child(self.button(
                    "ingest-plan",
                    "Plan",
                    "",
                    Cmd::Ingest("plan".into()),
                    false,
                    cx,
                ))
                .child(
                    self.button(
                        "ingest-start",
                        "Import",
                        "export",
                        Cmd::Ingest("start".into()),
                        true,
                        cx,
                    )
                    .tooltip(self.tip(&label, "")),
                );
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
            .tooltip(self.tip("Close import panel", "")),
        );
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
                        .when(!narrow, |s| s.child(self.ingest_selection_controls(cx))),
                )
            })
            .when(narrow || short || layout.timeline == 0., |s| {
                s.child(controls)
            })
            .child(body)
            .child(
                div()
                    .flex()
                    .when(narrow, |s| s.flex_col())
                    .flex_shrink_0()
                    .min_w(px(0.))
                    .gap(px(space::SM))
                    .p(px(ui::BAR_INSET))
                    .border_t_1()
                    .border_color(self.border())
                    .child(
                        div()
                            .id("ingest-summary")
                            .when(!narrow, |s| s.flex_1())
                            .min_w(px(0.))
                            .h(px(ui::CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .px(px(space::SM))
                            .text_color(self.muted())
                            .overflow_hidden()
                            .tooltip(self.tip(&format!("{source_label}\n{summary}\n{label}"), ""))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_ellipsis()
                                    .whitespace_normal()
                                    .line_clamp(1)
                                    .child(summary),
                            ),
                    )
                    .child(actions),
            )
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
    pub(super) fn ingest_preferences(&self, cx: &gpui::Context<Self>) -> AnyElement {
        let settings = self.preference_settings();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .pt(px(space::LG))
                    .pb(px(space::SM))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Import"),
            )
            .child(
                self.preference_row(
                    "On card insertion",
                    "ingest_on_insert",
                    self.button(
                        "pref-ingest-insert",
                        &settings.ingest_on_insert,
                        "",
                        Cmd::Ingest("insert".into()),
                        false,
                        cx,
                    )
                    .into_any_element(),
                ),
            )
            .child(
                self.preference_row(
                    "Day begins",
                    "ingest_day_start_hour",
                    self.button(
                        "pref-ingest-day",
                        &format!("{:02}:00", settings.ingest_day_start_hour),
                        "",
                        Cmd::Ingest("day".into()),
                        false,
                        cx,
                    )
                    .into_any_element(),
                ),
            )
            .child(
                self.preference_row(
                    "Eject after import",
                    "eject_after_ingest (explicit opt-in)",
                    self.button(
                        "pref-ingest-eject",
                        &settings.eject_after_ingest.to_string(),
                        "",
                        Cmd::Ingest("auto_eject".into()),
                        settings.eject_after_ingest,
                        cx,
                    )
                    .into_any_element(),
                ),
            )
            .child(
                self.preference_row(
                    "Current volume",
                    "Remember allow / deny for auto discovery",
                    self.button(
                        "pref-ingest-remember",
                        "Toggle allow / deny",
                        "",
                        Cmd::Ingest("remember".into()),
                        false,
                        cx,
                    )
                    .into_any_element(),
                ),
            )
            .child(div().child(format!(
                "Library: {}",
                settings.ingest_library_root.display()
            )))
            .child(div().child(format!(
                "Folder: {} · File: {}",
                settings.ingest_folder_template, settings.ingest_file_template
            )))
            .child(self.button(
                "pref-ingest-configure",
                "Configure destinations and templates…",
                "folder-opened",
                Cmd::Ingest("show".into()),
                false,
                cx,
            ))
            .into_any_element()
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

/// "…" + the last `max` characters, on a char boundary.
fn tail_ellipsis(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - max).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod responsive_tests {
    use super::{IMPORT_RESIZE_HANDLE, ImportLayout, ImportUi, ingest, selection_icon, space, ui};

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
    fn range_selection_counts_shrink_and_survive_filtered_positions() {
        let mut state = import_fixture();
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        state.select_position(2, false, false);
        state.select_position(6, false, true);
        assert_eq!(state.selected, ids[2..=6].iter().copied().collect());
        assert_eq!(state.photos, 5);
        assert!(state.timeline_selected.iter().any(|&n| n > 0));
        assert!(
            state
                .day_counts
                .values()
                .any(|&(n, total)| selection_icon(n, total) == "partial")
        );
        state.select_position(2, false, true);
        assert_eq!(state.selected, [ids[2]].into_iter().collect());
        assert_eq!(state.photos, 1);
        state.visible = vec![2, 4, 6, 8, 10];
        state.select_position(2, false, true);
        assert_eq!(
            state.selected,
            [ids[2], ids[4], ids[6]].into_iter().collect()
        );
        assert_eq!(state.photos, 3);
        assert_eq!(state.timeline_selected.iter().sum::<usize>(), 3);
    }

    #[test]
    fn first_shift_navigation_establishes_an_anchor_and_range_reverses() {
        let mut state = import_fixture();
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
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
        state.select_position(11, true, false);
        assert_eq!(state.anchor, Some(ids[11]));
        assert_eq!(state.photos, 11);
        state.navigate("left", 3, true);
        assert_eq!(state.selected, ids[10..].iter().copied().collect());
        assert_eq!(state.photos, 2);
    }

    #[test]
    fn navigation_starts_at_first_visible_photo_and_ignores_hidden_anchors() {
        let mut state = import_fixture();
        let ids: Vec<_> = state.items.iter().map(|g| g.id).collect();
        assert_eq!(state.navigate("down", 3, false), Some(0));
        assert_eq!(state.selected, [ids[0]].into_iter().collect());
        state.select_position(2, true, false);
        assert_eq!(state.selected, [ids[0], ids[2]].into_iter().collect());
        state.select_position(4, true, false);
        assert_eq!(
            state.selected,
            [ids[0], ids[2], ids[4]].into_iter().collect()
        );
        state.anchor = Some(ids[0]);
        state.visible = vec![2, 4, 6, 8, 10];
        state.navigate("down", 2, true);
        assert_eq!(state.anchor, Some(ids[4]));
        assert_eq!(
            state.selected,
            [ids[4], ids[6], ids[8]].into_iter().collect()
        );
        state.visible.clear();
        assert_eq!(state.navigate("end", 3, true), None);
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
