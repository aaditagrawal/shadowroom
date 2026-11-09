//! Incremental catalog snapshots shared by the catalog actor and the UI.
//!
//! The actor keeps the photo list it last handed to the UI and publishes only
//! the rows that differ from it: full photos for additions and changes, IDs
//! for removals. Both sides apply a delta with [`apply_delta`], so the UI's
//! list always equals the actor's baseline, which in turn equals a full
//! catalog reload sorted by [`snapshot_order`].
use crate::model::Photo;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

/// Total order of the photo list: capture time (modification time when the
/// capture time is unknown), then path, then ID. IDs are unique, so every
/// sorting algorithm produces the same sequence.
pub fn snapshot_order(a: &Photo, b: &Photo) -> Ordering {
    a.captured_at
        .unwrap_or(a.modified_at)
        .cmp(&b.captured_at.unwrap_or(b.modified_at))
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.id.cmp(&b.id))
}

fn same_position(a: &Photo, b: &Photo) -> bool {
    a.captured_at.unwrap_or(a.modified_at) == b.captured_at.unwrap_or(b.modified_at)
        && a.path == b.path
}

/// Changes since the previous snapshot. Each ID appears at most once across
/// both lists.
#[derive(Debug, Default)]
pub struct PhotoDelta {
    /// Complete current rows (including decisions) for new or changed photos.
    pub upserts: Vec<Photo>,
    pub removed: Vec<i64>,
}

impl PhotoDelta {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removed.is_empty()
    }
    pub fn ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.upserts
            .iter()
            .map(|photo| photo.id)
            .chain(self.removed.iter().copied())
    }
    /// Combine with a later delta, as if both had been applied in order.
    pub fn merge(&mut self, newer: PhotoDelta) {
        if newer.is_empty() {
            return;
        }
        let replaced: HashSet<i64> = newer.ids().collect();
        self.upserts.retain(|photo| !replaced.contains(&photo.id));
        self.removed.retain(|id| !replaced.contains(id));
        self.upserts.extend(newer.upserts);
        self.removed.extend(newer.removed);
    }
}

/// Apply `delta` to a list sorted by [`snapshot_order`]; `index` maps each ID
/// to its position in `photos` before the call. Returns whether the list
/// changed, in which case `index` is stale and must be rebuilt. Upserts equal
/// to the current row (e.g. a decision the UI already shows) change nothing.
pub fn apply_delta(
    photos: &mut Vec<Photo>,
    index: &HashMap<i64, usize>,
    delta: PhotoDelta,
) -> bool {
    let mut changed = false;
    let mut resort = false;
    for photo in delta.upserts {
        match index.get(&photo.id) {
            Some(&position) => {
                let current = &mut photos[position];
                if *current != photo {
                    resort |= !same_position(current, &photo);
                    *current = photo;
                    changed = true;
                }
            }
            None => {
                photos.push(photo);
                changed = true;
                resort = true;
            }
        }
    }
    let removed: HashSet<i64> = delta
        .removed
        .into_iter()
        .filter(|id| index.contains_key(id))
        .collect();
    if !removed.is_empty() {
        // Removal keeps the remaining order; appended IDs are never removed.
        photos.retain(|photo| !removed.contains(&photo.id));
        changed = true;
    }
    if resort {
        // Stable and adaptive: a sorted list with a few changed or appended
        // rows costs close to one linear pass.
        photos.sort_by(snapshot_order);
    }
    changed
}

/// ID to position for a snapshot list.
pub fn positions(photos: &[Photo]) -> HashMap<i64, usize> {
    photos
        .iter()
        .enumerate()
        .map(|(position, photo)| (photo.id, position))
        .collect()
}

/// The grouping input differs: membership, order, or a field the grouping
/// comparator or partitioner reads.
fn grouping_changed(old: &Photo, new: &Photo) -> bool {
    old.trashed != new.trashed
        || !same_position(old, new)
        || old.source != new.source
        || old.captured_at != new.captured_at
        || old.capture_time_kind != new.capture_time_kind
        || old.modified_at != new.modified_at
        || old.folder != new.folder
        || old.camera != new.camera
        || old.dhash != new.dhash
}

/// Result of one [`SnapshotBuilder::refresh`].
#[derive(Debug, Default)]
pub struct Refresh {
    /// What the UI must apply to match the catalog.
    pub delta: PhotoDelta,
    /// The grouping input changed and [`SnapshotBuilder::samples`] must be
    /// regrouped; groups computed for the previous input remain valid otherwise.
    pub regroup: bool,
    /// Publish even an empty delta on startup or after withdrawing a hand-off.
    pub required: bool,
}

#[derive(Default, PartialEq, Eq)]
enum Publication {
    #[default]
    Initial,
    Current,
    Withdrawn,
}

/// The actor's copy of the photo list the UI holds after applying every
/// published delta, plus each row's metadata revision so unchanged rows are
/// never reparsed.
#[derive(Default)]
pub struct SnapshotBuilder {
    photos: Vec<Photo>,
    index: HashMap<i64, usize>,
    /// Catalog revision and stored dhash (before pending-hash overlays) of
    /// every photo in `photos`.
    stored: HashMap<i64, (i64, Option<u64>)>,
    /// IDs whose published change never reached the UI.
    dirty: HashSet<i64>,
    publication: Publication,
}

impl SnapshotBuilder {
    /// Sorted by [`snapshot_order`]; equals a full catalog reload.
    pub fn photos(&self) -> &[Photo] {
        &self.photos
    }
    /// A published delta was taken back before the UI applied it: the UI's
    /// rows for its IDs are unknown, so the next refresh resends them.
    pub fn withdraw(&mut self, delta: &PhotoDelta) {
        self.dirty.extend(delta.ids());
        self.publication = Publication::Withdrawn;
    }
    /// Compare the catalog (with unsaved `hashes` overlaid) to the baseline,
    /// advance the baseline, and return the difference.
    pub fn refresh(
        &mut self,
        catalog: &crate::catalog::Catalog,
        hashes: &HashMap<i64, u64>,
    ) -> anyhow::Result<Refresh> {
        let stored = &self.stored;
        let rows = catalog.photo_revisions(|id, revision| {
            stored.get(&id).is_none_or(|&(known, _)| known != revision)
        })?;
        let mut delta = PhotoDelta::default();
        let required = self.publication != Publication::Current;
        let mut regroup = self.publication == Publication::Initial;
        let mut seen = HashSet::with_capacity(rows.len());
        for row in rows {
            let id = row.decision.id;
            seen.insert(id);
            let forced = self.dirty.contains(&id);
            let current = self.index.get(&id).map(|&position| &self.photos[position]);
            let next = match row.photo {
                Some(mut photo) => {
                    self.stored.insert(id, (row.revision, photo.dhash));
                    if let Some(&hash) = hashes.get(&id) {
                        photo.dhash = Some(hash);
                    }
                    (forced || current != Some(&photo)).then_some(photo)
                }
                None => {
                    let current = current.expect("every known revision has a baseline row");
                    let decision = row.decision;
                    let dhash = hashes.get(&id).copied().or(self.stored[&id].1);
                    let changed = current.rating != decision.rating
                        || current.flag != decision.flag
                        || current.label != decision.label
                        || current.trashed != decision.trashed
                        || current.dhash != dhash;
                    (forced || changed).then(|| Photo {
                        rating: decision.rating,
                        flag: decision.flag,
                        label: decision.label,
                        trashed: decision.trashed,
                        dhash,
                        ..current.clone()
                    })
                }
            };
            if let Some(photo) = next {
                regroup |= current.is_none_or(|current| grouping_changed(current, &photo));
                delta.upserts.push(photo);
            }
        }
        self.stored.retain(|id, _| {
            let present = seen.contains(id);
            if !present {
                delta.removed.push(*id);
            }
            present
        });
        // A withdrawn removal: the UI may still show a row the baseline lacks.
        for &id in &self.dirty {
            if !seen.contains(&id) && !self.index.contains_key(&id) {
                delta.removed.push(id);
            }
        }
        self.dirty.clear();
        regroup |= !delta.removed.is_empty();
        let baseline = PhotoDelta {
            upserts: delta.upserts.clone(),
            removed: delta.removed.clone(),
        };
        if apply_delta(&mut self.photos, &self.index, baseline) {
            self.index = positions(&self.photos);
        }
        self.publication = Publication::Current;
        Ok(Refresh {
            delta,
            regroup,
            required,
        })
    }
    /// Grouping input: the visible photos, in order, with only the fields
    /// the grouping comparator and partitioner read.
    pub fn samples(&self) -> Vec<Photo> {
        self.photos
            .iter()
            .filter(|p| !p.trashed)
            .map(|p| Photo {
                source: p.source.clone(),
                id: p.id,
                captured_at: p.captured_at,
                capture_time_kind: p.capture_time_kind.clone(),
                modified_at: p.modified_at,
                folder: p.folder.clone(),
                camera: p.camera.clone(),
                dhash: p.dhash,
                ..Photo::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn photo(id: i64, time: i64) -> Photo {
        Photo {
            id,
            captured_at: Some(time),
            path: format!("/p/{id}").into(),
            ..Photo::default()
        }
    }

    #[test]
    fn merged_deltas_equal_sequential_application() {
        let base: Vec<_> = (0..6).map(|id| photo(id, id * 10)).collect();
        let first = || PhotoDelta {
            upserts: vec![photo(1, 99), photo(7, 5)],
            removed: vec![2, 3],
        };
        let second = || PhotoDelta {
            upserts: vec![photo(2, 1), photo(7, 70)],
            removed: vec![1, 4],
        };
        let mut sequential = base.clone();
        apply_delta(&mut sequential, &positions(&base), first());
        let index = positions(&sequential);
        apply_delta(&mut sequential, &index, second());

        let mut merged = first();
        merged.merge(second());
        let mut combined = base.clone();
        assert!(apply_delta(&mut combined, &positions(&base), merged));
        let ids = |photos: &[Photo]| photos.iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(&combined), ids(&sequential));
        assert_eq!(ids(&combined), vec![0, 2, 5, 7]);
    }

    /// The full rebuild the incremental path replaces: reload every row,
    /// overlay unsaved hashes, sort.
    fn full_rebuild(catalog: &Catalog, hashes: &HashMap<i64, u64>) -> Vec<Photo> {
        let mut photos = catalog.photos().unwrap();
        for photo in &mut photos {
            if let Some(&hash) = hashes.get(&photo.id) {
                photo.dhash = Some(hash);
            }
        }
        photos.sort_unstable_by(snapshot_order);
        photos
    }

    fn random_photo(rng: &mut impl rand::Rng, path: String) -> Photo {
        // Few distinct times and folders: ties exercise the path/ID order.
        let time = rng.random_range(0..12i64);
        Photo {
            folder: format!("/f{}", rng.random_range(0..3)),
            filename: path.clone(),
            path: path.into(),
            captured_at: rng.random_bool(0.7).then_some(time * 1000),
            modified_at: rng.random_range(0..12i64) * 1000,
            camera: ["A", "B"][rng.random_range(0..2)].into(),
            capture_time_kind: ["offset", "camera_local"][rng.random_range(0..2)].into(),
            width: rng.random_range(1..4),
            dhash: rng.random_bool(0.3).then(|| rng.random_range(0..4)),
            ..Photo::default()
        }
    }

    /// Random catalog edits, unconsumed and withdrawn deltas, and unsaved
    /// hashes: the actor baseline and the UI's merged list always equal a
    /// full rebuild, and a skipped regroup always means unchanged samples.
    #[test]
    fn incremental_snapshots_match_full_rebuilds_over_random_edits() {
        use crate::model::{ColorLabel, Flag, PhotoDecision};
        use rand::{Rng, SeedableRng};
        for seed in 0..24 {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(&dir.path().join("catalog.sqlite3")).unwrap();
            let mut builder = SnapshotBuilder::default();
            let mut view: Vec<Photo> = Vec::new();
            let mut pending: Option<PhotoDelta> = None;
            let mut grouped: Option<Vec<Photo>> = None;
            let mut hashes = HashMap::new();
            let mut next_path = 0;
            for step in 0..60 {
                for _ in 0..rng.random_range(0..4) {
                    let ids: Vec<i64> = catalog.photos().unwrap().iter().map(|p| p.id).collect();
                    let pick = |rng: &mut rand::rngs::StdRng| ids[rng.random_range(0..ids.len())];
                    match rng.random_range(0..7) {
                        0 | 1 => {
                            let batch: Vec<_> = (0..rng.random_range(1..5))
                                .map(|_| {
                                    next_path += 1;
                                    random_photo(&mut rng, format!("/p/{next_path:03}"))
                                })
                                .collect();
                            catalog.upsert_photos(&batch).unwrap();
                        }
                        2 if !ids.is_empty() => {
                            // Metadata refresh of an existing path (same ID).
                            let id = pick(&mut rng);
                            let path = catalog.photos().unwrap().into_iter().find(|p| p.id == id);
                            if let Some(old) = path {
                                let path = old.path.to_string_lossy().into_owned();
                                catalog
                                    .upsert_photos(&[random_photo(&mut rng, path)])
                                    .unwrap();
                            }
                        }
                        3 if !ids.is_empty() => {
                            let decisions: Vec<_> = (0..rng.random_range(1..4))
                                .map(|_| PhotoDecision {
                                    id: pick(&mut rng),
                                    rating: rng.random_range(0..6),
                                    flag: [Flag::None, Flag::Pick, Flag::Reject]
                                        [rng.random_range(0..3)],
                                    label: [ColorLabel::None, ColorLabel::Red]
                                        [rng.random_range(0..2)],
                                    trashed: rng.random_bool(0.2),
                                })
                                .collect();
                            catalog.set_decisions(&decisions).unwrap();
                        }
                        4 if !ids.is_empty() => {
                            let id = pick(&mut rng);
                            catalog
                                .set_dhashes(&[(id, rng.random_range(0..4))])
                                .unwrap();
                            hashes.remove(&id);
                        }
                        5 if !ids.is_empty() => {
                            // Deleting the highest ID lets SQLite reuse it.
                            let id = if rng.random_bool(0.5) {
                                *ids.iter().max().unwrap()
                            } else {
                                pick(&mut rng)
                            };
                            catalog
                                .connection
                                .lock()
                                .unwrap()
                                .execute("DELETE FROM photos WHERE id=?1", [id])
                                .unwrap();
                        }
                        6 => {
                            let id = if ids.is_empty() || rng.random_bool(0.2) {
                                9999
                            } else {
                                pick(&mut rng)
                            };
                            hashes.insert(id, rng.random_range(0..4));
                        }
                        _ => {}
                    }
                }
                let refresh = builder.refresh(&catalog, &hashes).unwrap();
                let expected = full_rebuild(&catalog, &hashes);
                assert_eq!(builder.photos(), expected, "seed {seed} step {step}");
                let samples = builder.samples();
                if refresh.regroup {
                    grouped = Some(samples);
                } else {
                    assert_eq!(grouped.as_ref(), Some(&samples), "seed {seed} step {step}");
                }
                // Same hand-off as publish_with_hashes.
                let mut delta = refresh.delta;
                if let Some(mut older) = pending.take() {
                    older.merge(delta);
                    delta = older;
                }
                if refresh.required || !delta.is_empty() {
                    pending = Some(delta);
                }
                match rng.random_range(0..4) {
                    0 => {} // the UI has not polled yet
                    1 => {
                        // A decision commit withdrew the unconsumed delta.
                        if let Some(delta) = pending.take() {
                            builder.withdraw(&delta);
                        }
                    }
                    _ => {
                        if let Some(delta) = pending.take() {
                            let index = positions(&view);
                            apply_delta(&mut view, &index, delta);
                        }
                        assert_eq!(view, expected, "seed {seed} step {step}");
                    }
                }
            }
            let mut delta = builder.refresh(&catalog, &hashes).unwrap().delta;
            let again = builder.refresh(&catalog, &hashes).unwrap();
            assert!(
                again.delta.is_empty() && !again.regroup,
                "unchanged catalog"
            );
            if let Some(mut older) = pending.take() {
                older.merge(delta);
                delta = older;
            }
            let index = positions(&view);
            apply_delta(&mut view, &index, delta);
            assert_eq!(view, full_rebuild(&catalog, &hashes), "seed {seed} final");
        }
    }

    #[test]
    fn equal_upserts_and_unknown_removals_are_no_ops() {
        let base: Vec<_> = (0..3).map(|id| photo(id, id)).collect();
        let mut photos = base.clone();
        let delta = PhotoDelta {
            upserts: vec![photo(1, 1)],
            removed: vec![42],
        };
        assert!(!apply_delta(&mut photos, &positions(&base), delta));
    }

    #[test]
    fn withdrawing_an_empty_initial_snapshot_republishes_the_hand_off() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(&directory.path().join("catalog.sqlite3")).unwrap();
        let mut builder = SnapshotBuilder::default();
        let initial = builder.refresh(&catalog, &HashMap::new()).unwrap();
        assert!(initial.required && initial.regroup && initial.delta.is_empty());
        builder.withdraw(&initial.delta);
        let retry = builder.refresh(&catalog, &HashMap::new()).unwrap();
        assert!(retry.required && !retry.regroup && retry.delta.is_empty());
        let unchanged = builder.refresh(&catalog, &HashMap::new()).unwrap();
        assert!(!unchanged.required && !unchanged.regroup && unchanged.delta.is_empty());
    }
}
