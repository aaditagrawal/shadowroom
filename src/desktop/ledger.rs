//! Cull ledger: one thin track for every frame in the view, not just the
//! filmstrip's window, read like an account book. Unreviewed frames are a
//! hairline rail; reviewed frames without a flag a short grey bar on it; picks
//! stand above the line and rejects hang below it, so the two read apart by
//! shape as well as colour. A short accent playhead marks the current frame
//! (the accent's only use here). When the track is not laid out over the
//! filmstrip's cells, a hairline bracket shows which frames the strip holds.
//! Clicking or dragging along the track jumps to that frame.
//!
//! Cost is bounded by the track's width, not the catalog: frames fold into at
//! most one cell per [`BUCKET`] pixels (majority vote, ties to the decided
//! state; a cell still holding an unreviewed frame keeps a 1 px notch), the
//! cells are cached per decision epoch, and one canvas paints them as
//! run-length quads, so 50k photos add no layout nodes.

use super::*;

/// Narrowest cell, in logical pixels, once frames share cells.
pub(super) const BUCKET: f32 = 2.;
/// A frame keeps its own cell, separated by a 1 px gap, while each frame gets
/// at least this many pixels.
pub(super) const FRAME_MIN: f32 = 4.;
/// The track's full extent: a pick's 4 px bar above the rail line through a
/// reject's 4 px bar below it (they share the rail's row).
pub(super) const TRACK: f32 = 7.;
/// Rows from the track's top edge to the rail line.
const RAIL: f32 = 3.;
/// The playhead's overshoot above and below the track.
const OVERSHOOT: f32 = 2.;
/// Height of the ledger's hit area: track plus breathing room. The Catalog
/// draws it as its own band under the decision bar; the filmstrip hosts it in
/// its top padding.
pub(super) const BAND: f32 = 16.;
/// Track's top edge inside the band.
pub(super) const TRACK_TOP: f32 = 4.;
/// Lights-out: the decision bar and filmstrip fold into this band along the
/// window's bottom edge.
pub(super) const LIGHTS_OUT_BAND: f32 = 8.;
/// A free-standing track keeps 1 px gaps between frames only while cells are
/// narrower than this; wider cells run together, so they never read as labels
/// for the thumbnails below (R4-review-01).
const GAP_BELOW: f32 = 12.;
/// The Catalog's band under the decision bar, hairline included.
pub(super) const ROW_HEIGHT: f32 = ui::HAIRLINE + BAND;

/// (decision epoch, frames, cells) a cached ledger was built for.
pub(super) type CacheKey = (u64, usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Mark {
    Unreviewed = 0,
    /// Rated or labelled, no flag.
    Reviewed = 1,
    Pick = 2,
    Reject = 3,
}

impl Mark {
    /// Rows of the track this mark fills: (top offset, height).
    fn rows(self) -> (f32, f32) {
        match self {
            Self::Unreviewed => (RAIL, 1.),
            Self::Reviewed => (RAIL - 1., 3.),
            Self::Pick => (0., RAIL + 1.),
            Self::Reject => (RAIL, TRACK - RAIL),
        }
    }
    pub(super) fn of(photo: &Photo) -> Self {
        match photo.flag {
            Flag::Reject => Self::Reject,
            Flag::Pick => Self::Pick,
            Flag::None if Filter::Unrated.accepts(photo) => Self::Unreviewed,
            Flag::None => Self::Reviewed,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Ledger {
    pub frames: usize,
    /// One mark per cell; `cells.len() == frames` while frames keep their own cell.
    pub cells: Vec<Mark>,
    /// Bucketed cells only: how many frames of each mark a cell holds, so a
    /// decision updates its cell without another pass over the view.
    counts: Vec<[u32; 4]>,
    /// Per-frame cells only: a burst or duplicate set starts or ends before this cell.
    pub breaks: Vec<bool>,
    pub picks: usize,
    pub rejects: usize,
    pub reviewed: usize,
}

impl Ledger {
    /// One frame's mark changed in place (same view, same order).
    pub(super) fn update(&mut self, frame: usize, before: Mark, after: Mark) {
        if before == after || frame >= self.frames || self.cells.is_empty() {
            return;
        }
        for (mark, delta) in [(before, -1isize), (after, 1)] {
            match mark {
                Mark::Pick => self.picks = self.picks.saturating_add_signed(delta),
                Mark::Reject => self.rejects = self.rejects.saturating_add_signed(delta),
                _ => {}
            }
            if mark != Mark::Unreviewed {
                self.reviewed = self.reviewed.saturating_add_signed(delta);
            }
        }
        if self.counts.is_empty() {
            self.cells[frame] = after;
            return;
        }
        let cell = cell_of(frame, self.frames, self.cells.len());
        let counts = &mut self.counts[cell];
        counts[before as usize] = counts[before as usize].saturating_sub(1);
        counts[after as usize] += 1;
        self.cells[cell] = majority(*counts);
    }
}

/// Cells for `frames` frames across `width` pixels.
pub(super) fn cell_count(frames: usize, width: f32) -> usize {
    if frames == 0 || width < 1. {
        return 0;
    }
    if frames as f32 * FRAME_MIN <= width {
        frames
    } else {
        ((width / BUCKET).floor() as usize).clamp(1, frames)
    }
}

/// First frame of cell `cell` (cells split frames as evenly as integers allow).
fn cell_start(cell: usize, frames: usize, cells: usize) -> usize {
    (cell as u128 * frames as u128 / cells.max(1) as u128) as usize
}

/// Cell holding `frame`.
fn cell_of(frame: usize, frames: usize, cells: usize) -> usize {
    if frames == 0 || cells == 0 {
        return 0;
    }
    // Inverse of cell_start: the last cell whose start is <= frame.
    let guess = ((frame as u128 * cells as u128) / frames as u128) as usize;
    let mut cell = guess.min(cells - 1);
    while cell + 1 < cells && cell_start(cell + 1, frames, cells) <= frame {
        cell += 1;
    }
    while cell > 0 && cell_start(cell, frames, cells) > frame {
        cell -= 1;
    }
    cell
}

/// Horizontal layout of a track: its width and the gap between per-frame
/// cells (1 px on a free-standing track; the filmstrip's gap when the track
/// sits exactly over the strip's frames). Bucketed tracks have no gaps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Track {
    pub width: f32,
    pub gap: f32,
}

impl Track {
    /// A track that is not laid out over the filmstrip's cells.
    pub(super) fn free(width: f32, frames: usize) -> Self {
        let pitch = width / frames.max(1) as f32;
        Self {
            width,
            gap: if pitch < GAP_BELOW { 1. } else { 0. },
        }
    }
    /// x-range of frames `start..end` on this track, for the window bracket.
    pub(super) fn span(self, start: usize, end: usize, frames: usize, cells: usize) -> (f32, f32) {
        if self.per_frame(frames, cells) {
            let pitch = self.pitch(frames);
            (
                (start as f32 * pitch).round(),
                (end as f32 * pitch - self.gap).round(),
            )
        } else {
            let edge = |frame: usize| (self.width * frame as f32 / frames.max(1) as f32).round();
            (edge(start), edge(end))
        }
    }
    /// Whether `frames` frames keep their own cells (and gaps) at this width.
    fn per_frame(self, frames: usize, cells: usize) -> bool {
        cells == frames && frames > 0 && self.width / frames as f32 >= FRAME_MIN
    }
    /// Distance between per-frame cell starts.
    fn pitch(self, frames: usize) -> f32 {
        (self.width + self.gap) / frames.max(1) as f32
    }
    /// Frame under `x` pixels from the track's left edge (clamped to the ends).
    pub(super) fn frame_at(self, x: f32, frames: usize) -> usize {
        if frames == 0 || self.width <= 0. {
            return 0;
        }
        let pitch = self.pitch(frames);
        // A click in a gap belongs to the nearer frame.
        (((x + self.gap / 2.) / pitch).floor().max(0.) as usize).min(frames - 1)
    }
    /// Playhead for `frame`: (x, width), centred on the frame's own cell.
    pub(super) fn playhead(self, frame: usize, frames: usize) -> (f32, f32) {
        let pitch = self.pitch(frames);
        let centre = frame as f32 * pitch + (pitch - self.gap) / 2.;
        let w = 2.;
        (
            (centre - w / 2.)
                .round()
                .clamp(0., (self.width - w).max(0.)),
            w,
        )
    }
}

/// Folds `frames` marks into `cells` cells. `group` is called only while frames
/// keep their own cell, so bucketed catalogs never look up burst membership.
pub(super) fn build(
    frames: usize,
    cells: usize,
    mark: impl Fn(usize) -> Mark,
    group: impl Fn(usize) -> Option<usize>,
) -> Ledger {
    let mut ledger = Ledger {
        frames,
        cells: Vec::with_capacity(cells),
        ..Ledger::default()
    };
    if frames == 0 || cells == 0 {
        return ledger;
    }
    let per_frame = cells == frames;
    let mut counts = [0usize; 4];
    let mut cell = 0;
    let mut end = cell_start(1, frames, cells);
    let mut previous_group = None;
    for frame in 0..frames {
        let m = mark(frame);
        match m {
            Mark::Pick => ledger.picks += 1,
            Mark::Reject => ledger.rejects += 1,
            _ => {}
        }
        ledger.reviewed += usize::from(m != Mark::Unreviewed);
        if per_frame {
            let g = group(frame);
            ledger.breaks.push(frame > 0 && g != previous_group);
            previous_group = g;
            ledger.cells.push(m);
            continue;
        }
        counts[m as usize] += 1;
        if frame + 1 == end {
            ledger.cells.push(majority(counts));
            ledger.counts.push(counts.map(|n| n as u32));
            counts = [0; 4];
            cell += 1;
            end = cell_start(cell + 1, frames, cells);
        }
    }
    ledger
}

/// The most common mark; ties go to the more decided state.
fn majority<T: PartialOrd + Copy>(counts: [T; 4]) -> Mark {
    let mut best = Mark::Unreviewed;
    for (i, mark) in [Mark::Reviewed, Mark::Pick, Mark::Reject]
        .into_iter()
        .enumerate()
    {
        if counts[i + 1] >= counts[best as usize] {
            best = mark;
        }
    }
    best
}

/// x of a 1 px notch for every bucketed cell that still holds an unreviewed
/// frame under a decided majority (R4-review-05): one skipped frame in a run
/// of picks never disappears from the rail. Per-frame cells need none.
pub(super) fn notches(ledger: &Ledger, track: Track) -> Vec<f32> {
    let cells = ledger.cells.len();
    if ledger.counts.is_empty() || cells == 0 || track.width < 1. {
        return Vec::new();
    }
    ledger
        .cells
        .iter()
        .zip(&ledger.counts)
        .enumerate()
        .filter(|(_, (mark, counts))| **mark != Mark::Unreviewed && counts[0] > 0)
        .map(|(i, _)| (track.width * i as f32 / cells as f32).round())
        .collect()
}

/// Painted rectangles as (x, width, mark) from the track's left edge.
/// Per-frame cells keep their gaps (3 px where a burst starts or ends on a
/// free-standing track); bucketed cells merge into edge-to-edge runs with
/// integral edges, so neighbours never overlap or leave sub-pixel seams.
pub(super) fn quads(ledger: &Ledger, track: Track) -> Vec<(f32, f32, Mark)> {
    let cells = ledger.cells.len();
    let mut out: Vec<(f32, f32, Mark)> = Vec::new();
    if cells == 0 || track.width < 1. {
        return out;
    }
    if track.per_frame(ledger.frames, cells) {
        let pitch = track.pitch(cells);
        for (i, &mark) in ledger.cells.iter().enumerate() {
            let mut x0 = (i as f32 * pitch).round();
            let x1 = (i as f32 * pitch + pitch - track.gap).round();
            if track.gap < 2. && ledger.breaks.get(i).copied().unwrap_or(false) && x1 - x0 >= 8. {
                x0 += 2.;
            }
            if x1 > x0 {
                out.push((x0, x1 - x0, mark));
            }
        }
        return out;
    }
    let edge = |c: usize| (track.width * c as f32 / cells as f32).round();
    for (i, &mark) in ledger.cells.iter().enumerate() {
        let (x0, x1) = (edge(i), edge(i + 1));
        if x1 <= x0 {
            continue;
        }
        match out.last_mut() {
            Some((x, w, m)) if *m == mark && (*x + *w - x0).abs() < 0.5 => *w = x1 - *x,
            _ => out.push((x0, x1 - x0, mark)),
        }
    }
    out
}

impl Workspace {
    /// The ledger for the current view at `cells` cells, cached until a
    /// decision, filter or catalog change bumps the decision epoch.
    fn ledger_data(&self, cells: usize) -> Rc<Ledger> {
        let key = (self.decision_epoch, self.filtered.len(), cells);
        if let Some((cached, ledger)) = &*self.ledger_cache.borrow()
            && *cached == key
        {
            return ledger.clone();
        }
        let ledger = Rc::new(build(
            self.filtered.len(),
            cells,
            |frame| Mark::of(&self.photos[self.filtered[frame]]),
            |frame| {
                self.burst_slot(self.photos[self.filtered[frame]].id)
                    .map(|(group, ..)| group)
            },
        ));
        *self.ledger_cache.borrow_mut() = Some((key, ledger.clone()));
        ledger
    }

    /// Jump to the frame under the pointer (click or drag along the track).
    fn ledger_jump(&mut self, x: f32, cx: &mut Context<Self>) {
        let (left, track) = self.ledger_track.get();
        let frame = track.frame_at(x - left, self.filtered.len());
        if frame == self.selected && self.multi.len() < 2 {
            return;
        }
        self.select(frame, false, false, cx);
        if self.mode == ViewMode::Grid {
            self.reveal_grid_row(frame / self.grid_cols().max(1), true);
        }
    }

    /// Decisions are what the ledger shows, so the Trash (which has none to
    /// make) goes without it.
    pub(super) fn ledger_visible(&self) -> bool {
        !self.filtered.is_empty() && self.filter != Filter::Trash
    }

    /// Track width for the centre pane: the filmstrip's and Catalog band's
    /// content box.
    pub(super) fn ledger_width(&self) -> f32 {
        (self.center_size.0 - 2. * ui::PANEL_INSET).floor().max(0.)
    }

    /// Catalog band: the ledger on the panel under the decision bar, where the
    /// filmstrip's top edge carries it in the other views.
    pub(super) fn ledger_band(&self, cx: &mut Context<Self>) -> AnyElement {
        let width = self.ledger_width();
        div()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .px(px(ui::PANEL_INSET))
            .border_t_1()
            .border_color(self.border())
            .bg(self.panel())
            .child(self.ledger(
                Track::free(width, self.filtered.len()),
                None,
                TRACK_TOP,
                BAND,
                cx,
            ))
            .into_any_element()
    }

    /// Lights-out (a second Focus press): the decision bar and filmstrip fold
    /// into an 8 px ledger along the window's bottom edge.
    pub(super) fn lights_out_band(&self, cx: &mut Context<Self>) -> AnyElement {
        let width = (self.center_size.0 - 2. * ui::PANEL_INSET).floor().max(0.);
        div()
            .id("lights-out-ledger")
            .h(px(LIGHTS_OUT_BAND))
            .flex_shrink_0()
            .px(px(ui::PANEL_INSET))
            .bg(self.panel())
            .child(self.ledger(
                Track::free(width, self.filtered.len()),
                None,
                ((LIGHTS_OUT_BAND - TRACK) / 2.).floor(),
                LIGHTS_OUT_BAND,
                cx,
            ))
            .into_any_element()
    }

    /// The track, `height` tall and transparent so it sits on whatever band
    /// hosts it (always a panel), with its extent `top` px down. `window`
    /// (first frame, end) brackets the frames the filmstrip shows when the
    /// track is not laid out over them.
    pub(super) fn ledger(
        &self,
        track: Track,
        window: Option<(usize, usize)>,
        top: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frames = self.filtered.len();
        let width = track.width;
        let ledger = self.ledger_data(cell_count(frames, width));
        // Bucketed tracks have no gaps, for painting and for hit-testing alike.
        let track = if track.per_frame(frames, ledger.cells.len()) {
            track
        } else {
            Track { width, gap: 0. }
        };
        let current = self.selected.min(frames.saturating_sub(1));
        let colors = [
            self.c("shadowroom.ledger.rail", "#3a3a3a"),
            self.c("shadowroom.ledger.reviewed", "#8a8a8a"),
            self.pick_color(),
            self.reject_color(),
        ];
        let accent = self.accent();
        let bracket_color = self.muted();
        // Filmstrip, Catalog band and lights-out band are all panels.
        let surface = self.panel();
        let geometry = self.ledger_track.clone();
        let painted = ledger.clone();
        let bracket = window.map(|(start, end)| track.span(start, end, frames, ledger.cells.len()));
        let track = canvas(
            move |bounds, _, _| {
                geometry.set((f32::from(bounds.left()), track));
            },
            move |bounds, (), window, _| {
                let left = f32::from(bounds.left()).round();
                let top = (f32::from(bounds.top()) + top).round();
                let quad = |window: &mut Window, x: f32, y: f32, w: f32, h: f32, color| {
                    window.paint_quad(fill(
                        Bounds::new(point(px(left + x), px(top + y)), size(px(w), px(h))),
                        color,
                    ));
                };
                // The rail runs the whole view, so progress reads as the line
                // filling up; decided frames stand on it or hang from it.
                quad(window, 0., RAIL, width, 1., colors[0]);
                for (x, w, mark) in quads(&painted, track) {
                    let (y, h) = mark.rows();
                    quad(window, x, y, w, h, colors[mark as usize]);
                }
                for x in notches(&painted, track) {
                    quad(window, x, 0., 1., TRACK, surface);
                    quad(window, x, RAIL, 1., 1., colors[0]);
                }
                // The strip's frames on a minimap: a hairline bracket.
                if let Some((x0, x1)) = bracket
                    && x1 - x0 >= 3.
                {
                    let (y, h) = (-2., TRACK + 4.);
                    for x in [x0, x1 - 1.] {
                        quad(window, x, y, 1., h, bracket_color);
                    }
                    for (x, serif) in [(x0 + 1., 2.), (x1 - 3., 2.)] {
                        quad(window, x, y, serif, 1., bracket_color);
                        quad(window, x, y + h - 1., serif, 1., bracket_color);
                    }
                }
                if painted.frames > 0 {
                    let (x, w) = track.playhead(current, painted.frames);
                    // A 1 px knockout keeps the playhead distinct from the
                    // tick under it (accent on pick green is ~1:1 in light).
                    quad(window, x - 1., 0., w + 2., TRACK, surface);
                    quad(window, x, -OVERSHOOT, w, TRACK + 2. * OVERSHOOT, accent);
                }
            },
        )
        .size_full();
        let tip = format!(
            "Cull ledger · {} of {} reviewed · {} · {} · click to jump",
            copy::count(ledger.reviewed),
            copy::count(frames),
            plural(ledger.picks, "pick", "picks"),
            plural(ledger.rejects, "reject", "rejects"),
        );
        div()
            .id("cull-ledger")
            .w(px(width))
            .h(px(height))
            .flex_shrink_0()
            .cursor_pointer()
            .child(track)
            .tooltip(self.tip(&tip, ""))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.ledger_jump(f32::from(event.position.x), cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.ledger_jump(f32::from(event.position.x), cx);
                }
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that glob brings in gpui's `test` attribute.
    use super::{Mark, Track, build, cell_count, cell_of, cell_start, majority, notches, quads};
    use shadowroom::model::{Flag, Photo};
    use std::time::Instant;

    fn marks(pattern: &[Mark]) -> impl Fn(usize) -> Mark + '_ {
        move |i| pattern[i % pattern.len()]
    }

    #[test]
    fn small_views_keep_one_cell_per_frame_with_burst_breaks() {
        let pattern = [Mark::Pick, Mark::Reject, Mark::Unreviewed, Mark::Reviewed];
        let ledger = build(7, cell_count(7, 900.), marks(&pattern), |i| {
            (2..5).contains(&i).then_some(0)
        });
        assert_eq!(ledger.cells.len(), 7);
        assert_eq!(ledger.cells[0], Mark::Pick);
        assert_eq!(ledger.cells[2], Mark::Unreviewed);
        assert_eq!(
            ledger.breaks,
            [false, false, true, false, false, true, false]
        );
        assert_eq!((ledger.picks, ledger.rejects, ledger.reviewed), (2, 2, 5));
        // Separate quads with gaps, never overlapping, inside the track.
        let track = Track {
            width: 900.,
            gap: 1.,
        };
        let quads = quads(&ledger, track);
        assert_eq!(quads.len(), 7);
        for pair in quads.windows(2) {
            assert!(pair[0].0 + pair[0].1 < pair[1].0);
        }
        let last = quads.last().unwrap();
        assert_eq!(last.0 + last.1, 900.);
    }

    #[test]
    fn a_track_over_the_filmstrip_puts_each_cell_over_its_frame() {
        // Seven 96 px frames 8 px apart, as the filmstrip lays them out.
        let ledger = build(7, cell_count(7, 720.), |_| Mark::Pick, |_| Some(0));
        let track = Track {
            width: 7. * 104. - 8.,
            gap: 8.,
        };
        for (i, (x, w, _)) in quads(&ledger, track).into_iter().enumerate() {
            assert_eq!((x, w), (i as f32 * 104., 96.));
        }
        // Clicks land on the frame below, gaps going to the nearer frame.
        assert_eq!(track.frame_at(0., 7), 0);
        assert_eq!(track.frame_at(99., 7), 0);
        assert_eq!(track.frame_at(101., 7), 1);
        assert_eq!(track.frame_at(700., 7), 6);
        assert_eq!(track.playhead(1, 7), (151., 2.));
    }

    #[test]
    fn large_catalogs_fold_into_width_bounded_cells() {
        let frames = 50_000;
        let cells = cell_count(frames, 1_000.);
        assert_eq!(cells, 500);
        // First fifth picked, last fifth rejected, the rest unreviewed.
        let ledger = build(
            frames,
            cells,
            |i| match i * 5 / frames {
                0 => Mark::Pick,
                4 => Mark::Reject,
                _ => Mark::Unreviewed,
            },
            |_| unreachable!("bucketed ledgers never look up groups"),
        );
        assert_eq!(ledger.cells.len(), cells);
        assert!(ledger.breaks.is_empty());
        assert_eq!(ledger.cells[0], Mark::Pick);
        assert_eq!(ledger.cells[250], Mark::Unreviewed);
        assert_eq!(ledger.cells[cells - 1], Mark::Reject);
        assert_eq!(ledger.picks, 10_000);
        // Run-length quads: three runs, edge to edge across the whole width.
        let quads = quads(
            &ledger,
            Track {
                width: 1_000.,
                gap: 0.,
            },
        );
        assert_eq!(quads.len(), 3);
        assert_eq!(quads[0].0, 0.);
        assert_eq!(quads[0].0 + quads[0].1, quads[1].0);
        assert_eq!(quads[2].0 + quads[2].1, 1_000.);
    }

    #[test]
    fn decisions_update_the_cached_ledger_like_a_rebuild() {
        for (frames, width) in [(7usize, 900.), (50_000, 1_000.)] {
            let cells = cell_count(frames, width);
            let mut marks = vec![Mark::Unreviewed; frames];
            let mut ledger = build(frames, cells, |i| marks[i], |_| None);
            for (step, frame) in (0..frames).step_by((frames / 13).max(1)).enumerate() {
                let after = [Mark::Pick, Mark::Reject, Mark::Reviewed][step % 3];
                // A run of decisions inside one cell can flip its majority.
                for f in frame..(frame + 120).min(frames) {
                    ledger.update(f, marks[f], after);
                    marks[f] = after;
                }
                assert_eq!(ledger, build(frames, cells, |i| marks[i], |_| None));
            }
        }
    }

    #[test]
    fn a_skipped_frame_inside_a_bucket_leaves_a_notch() {
        let (frames, cells) = (2_000, 450);
        let mark = |i: usize| {
            if i == 1_234 {
                Mark::Unreviewed
            } else {
                Mark::Pick
            }
        };
        let ledger = build(frames, cells, mark, |_| None);
        let track = Track {
            width: 900.,
            gap: 0.,
        };
        // The majority stays Pick, so the run is one quad...
        assert_eq!(quads(&ledger, track).len(), 1);
        // ...but the cell holding frame 1,234 keeps a notch, and clicking it
        // lands within 4 frames of the skipped one.
        let notches = notches(&ledger, track);
        assert_eq!(notches.len(), 1);
        let frame = track.frame_at(notches[0] + 0.5, frames);
        assert!(frame.abs_diff(1_234) <= 4, "{frame}");
        // Reviewing it removes the notch; per-frame ledgers never notch.
        let mut ledger = ledger;
        ledger.update(1_234, Mark::Unreviewed, Mark::Pick);
        assert!(super::notches(&ledger, track).is_empty());
        let small = build(7, 7, |i| [Mark::Pick, Mark::Unreviewed][i % 2], |_| None);
        assert!(
            super::notches(
                &small,
                Track {
                    width: 900.,
                    gap: 1.
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn picks_stand_above_the_rail_and_rejects_hang_below() {
        let (pick_top, pick_h) = Mark::Pick.rows();
        let (reject_top, reject_h) = Mark::Reject.rows();
        let (rail, rail_h) = Mark::Unreviewed.rows();
        let (reviewed, reviewed_h) = Mark::Reviewed.rows();
        // Shape, not only hue: 3 px apart vertically, all on the rail row.
        assert_eq!(reject_top - pick_top, 3.);
        assert_eq!((pick_h, reject_h, rail_h), (4., 4., 1.));
        assert!(pick_top + pick_h > rail && reject_top <= rail);
        assert!(reviewed < rail && reviewed + reviewed_h > rail + rail_h);
        assert_eq!(reject_top + reject_h, super::TRACK);
    }

    #[test]
    fn free_tracks_drop_gaps_once_cells_are_wide_and_bracket_the_window() {
        // 7 frames on a 704 px free-standing track: 100 px cells, no gaps.
        assert_eq!(Track::free(704., 7).gap, 0.);
        assert_eq!(Track::free(704., 200).gap, 1.);
        let track = Track::free(700., 7);
        assert_eq!(track.span(1, 6, 7, 7), (100., 600.));
        // Bucketed: the bracket covers the window's share of the width.
        let wide = Track {
            width: 1_000.,
            gap: 0.,
        };
        assert_eq!(wide.span(0, 10, 50_000, 500), (0., 0.));
        assert_eq!(wide.span(25_000, 30_000, 50_000, 500), (500., 600.));
    }

    #[test]
    fn ties_go_to_the_decided_state() {
        assert_eq!(majority([2, 2, 0, 0]), Mark::Reviewed);
        assert_eq!(majority([1, 0, 1, 1]), Mark::Reject);
        assert_eq!(majority([3, 0, 1, 0]), Mark::Unreviewed);
        assert_eq!(majority([0, 0, 0, 0]), Mark::Reject);
    }

    #[test]
    fn positions_round_trip_between_frames_cells_and_pixels() {
        for (frames, width) in [(7usize, 900.), (1_000, 333.), (50_000, 1_187.)] {
            let cells = cell_count(frames, width);
            let track = Track { width, gap: 0. };
            let mut previous = 0;
            for frame in (0..frames).step_by((frames / 97).max(1)) {
                let cell = cell_of(frame, frames, cells);
                assert!(cell >= previous && cell < cells);
                assert!(cell_start(cell, frames, cells) <= frame);
                previous = cell;
                let x = width * (frame as f32 + 0.5) / frames as f32;
                assert_eq!(track.frame_at(x, frames), frame);
                let (head, w) = track.playhead(frame, frames);
                assert!(head >= 0. && head + w <= width);
            }
            assert_eq!(track.frame_at(-5., frames), 0);
            assert_eq!(track.frame_at(width + 5., frames), frames - 1);
        }
    }

    /// Run with `--ignored --nocapture bench_ledger` for the 50k rebuild cost.
    #[test]
    #[ignore]
    fn bench_ledger() {
        use std::hint::black_box;
        let photos: Vec<Photo> = (0..50_000i64)
            .map(|id| Photo {
                id,
                filename: format!("frame{id:06}.nef"),
                rating: (id % 7) as u8 % 6,
                flag: match id % 11 {
                    0 => Flag::Pick,
                    1 => Flag::Reject,
                    _ => Flag::None,
                },
                ..Photo::default()
            })
            .collect();
        let filtered: Vec<usize> = (0..photos.len()).collect();
        let track = Track {
            width: 1_120.,
            gap: 0.,
        };
        let best = (0..9)
            .map(|_| {
                let started = Instant::now();
                let cells = cell_count(filtered.len(), track.width);
                let ledger = build(
                    filtered.len(),
                    cells,
                    |f| super::Mark::of(&photos[filtered[f]]),
                    |_| None,
                );
                black_box(quads(&ledger, track).len());
                started.elapsed()
            })
            .min()
            .unwrap();
        println!("BENCH ledger 50k photos, 1120 px: rebuild + quads {best:?}");
        let mut ledger = build(
            filtered.len(),
            cell_count(filtered.len(), track.width),
            |f| super::Mark::of(&photos[filtered[f]]),
            |_| None,
        );
        let started = Instant::now();
        for frame in (0..50_000).step_by(50) {
            ledger.update(frame, super::Mark::of(&photos[frame]), Mark::Pick);
        }
        println!(
            "BENCH ledger 50k: one decision {:?}",
            started.elapsed() / 1_000
        );
        let started = Instant::now();
        for _ in 0..100 {
            black_box(quads(&ledger, track).len());
        }
        println!(
            "BENCH ledger 50k: quads per paint {:?}",
            started.elapsed() / 100
        );
    }
}
