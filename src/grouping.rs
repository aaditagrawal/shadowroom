//! Bounded temporal-neighbor grouping: O(n log n + n * max_neighbors), never all-pairs.
use crate::model::Photo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupingMode {
    #[default]
    Off,
    Time,
    Visual,
    Burst,
}
#[derive(Clone, Copy, Debug)]
pub struct GroupingOptions {
    pub mode: GroupingMode,
    pub gap_ms: u64,
    pub hamming_distance: u32,
    /// Hard cap on comparisons per image, including unusually dense timestamps.
    pub max_neighbors: usize,
}
impl Default for GroupingOptions {
    fn default() -> Self {
        Self {
            mode: GroupingMode::Off,
            gap_ms: 1500,
            hamming_distance: 10,
            max_neighbors: 32,
        }
    }
}
/// Returns all IDs exactly once; groups and members follow input order.
/// Time uses capture time (or file mtime) and camera/folder boundaries. Visual requires
/// two hashes; Burst uses time until hashes arrive, then also checks visual similarity.
/// Visual groups retain a representative to prevent similarity drift and unknown
/// previews bridging two visibly different scenes. Burst additionally bounds the
/// total span and size, so a continuous shoot cannot become one enormous stack.
pub fn group_photos(photos: &[Photo], options: GroupingOptions) -> Vec<Vec<i64>> {
    if options.mode == GroupingMode::Off {
        return photos.iter().map(|p| vec![p.id]).collect();
    }
    let nodes = photo_nodes(photos);
    let streams = sorted_streams(photos, &nodes);
    group_nodes(&nodes, &streams, options)
        .into_iter()
        .map(|(_, ids)| ids)
        .collect()
}

#[derive(Clone, Copy)]
struct GroupNode {
    id: i64,
    time: i64,
    hash: Option<u64>,
    order: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ClockDomain {
    UtcCapture,
    CameraLocal,
    UnknownCapture,
    FileModified,
}

fn clock_domain(photo: &Photo) -> ClockDomain {
    // A camera wall clock is encoded as neutral milliseconds by the indexer;
    // it is not a UTC instant. File mtime measures a different event entirely.
    // Keep legacy capture times separate rather than inventing an offset.
    if photo.captured_at.is_none() {
        ClockDomain::FileModified
    } else {
        match photo.capture_time_kind.as_str() {
            "offset" => ClockDomain::UtcCapture,
            "camera_local" => ClockDomain::CameraLocal,
            _ => ClockDomain::UnknownCapture,
        }
    }
}

fn photo_nodes(photos: &[Photo]) -> Vec<GroupNode> {
    photos
        .iter()
        .enumerate()
        .map(|(order, p)| GroupNode {
            id: p.id,
            time: timestamp(p),
            hash: p.dhash,
            order,
        })
        .collect()
}
fn sorted_streams(photos: &[Photo], nodes: &[GroupNode]) -> Vec<Vec<usize>> {
    // Intern stream identities once. The hot sort/union paths touch compact
    // integer indices and nodes, never Photo's large strings/metadata.
    let mut identities = HashMap::<_, usize>::new();
    let mut streams: Vec<Vec<usize>> = Vec::new();
    let mut previous = None;
    for (i, p) in photos.iter().enumerate() {
        let key = (p.grouping_key(), clock_domain(p));
        let stream = match previous {
            Some((previous_key, stream)) if previous_key == key => stream,
            _ => *identities.entry(key).or_insert_with(|| {
                streams.push(Vec::new());
                streams.len() - 1
            }),
        };
        streams[stream].push(i);
        previous = Some((key, stream));
    }
    for stream in &mut streams {
        stream.sort_unstable_by_key(|&i| (nodes[i].time, i));
    }
    streams
}
fn group_nodes(
    nodes: &[GroupNode],
    streams: &[Vec<usize>],
    options: GroupingOptions,
) -> Vec<(usize, Vec<i64>)> {
    if options.mode == GroupingMode::Time {
        // Temporal connectivity is a sorted adjacent-gap scan, with no union
        // state, representative hashes or per-component span bookkeeping.
        let mut roots = vec![0; nodes.len()];
        let mut sizes = vec![0; nodes.len()];
        for stream in streams {
            let Some(&first) = stream.first() else {
                continue;
            };
            let mut root = first;
            let mut previous = first;
            for &i in stream {
                if nodes[i].time.abs_diff(nodes[previous].time) > options.gap_ms {
                    root = i;
                }
                roots[i] = root;
                sizes[root] += 1;
                previous = i;
            }
        }
        return collect_groups(nodes, &roots, &sizes);
    }
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    let mut sizes = vec![1usize; nodes.len()];
    let mut anchor: Vec<Option<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(i, p)| p.hash.map(|_| i))
        .collect();
    let mut first: Vec<i64> = nodes.iter().map(|p| p.time).collect();
    let mut last = first.clone();
    let burst_span = options.gap_ms.saturating_mul(8).clamp(2_000, 30_000);
    let limit = if options.mode == GroupingMode::Time {
        1
    } else {
        options.max_neighbors.clamp(1, 256)
    };
    let threshold = options.hamming_distance.min(64);
    for stream in streams {
        for pos in 1..stream.len() {
            let i = stream[pos];
            for prev in (pos.saturating_sub(limit)..pos).rev() {
                let j = stream[prev];
                if nodes[i].time.abs_diff(nodes[j].time) > options.gap_ms {
                    break;
                }
                let similar = match options.mode {
                    GroupingMode::Time => true,
                    GroupingMode::Visual => {
                        matches!((nodes[i].hash,nodes[j].hash),(Some(a),Some(b)) if (a^b).count_ones()<=threshold)
                    }
                    GroupingMode::Burst => match (nodes[i].hash, nodes[j].hash) {
                        (Some(a), Some(b)) => (a ^ b).count_ones() <= threshold,
                        _ => true,
                    },
                    GroupingMode::Off => false,
                };
                if similar {
                    let a = find(&mut parent, i);
                    let b = find(&mut parent, j);
                    if a != b {
                        let start = first[a].min(first[b]);
                        let end = last[a].max(last[b]);
                        if options.mode == GroupingMode::Burst
                            && (end.abs_diff(start) > burst_span || sizes[a] + sizes[b] > 256)
                        {
                            continue;
                        }
                        if matches!(options.mode, GroupingMode::Visual | GroupingMode::Burst) {
                            if let (Some(left), Some(right)) = (anchor[a], anchor[b]) {
                                if (nodes[left].hash.unwrap() ^ nodes[right].hash.unwrap())
                                    .count_ones()
                                    > threshold
                                {
                                    continue;
                                }
                            }
                        }
                        let representative = anchor[b].or(anchor[a]);
                        let root;
                        if sizes[a] < sizes[b] {
                            parent[a] = b;
                            sizes[b] += sizes[a];
                            root = b;
                        } else {
                            parent[b] = a;
                            sizes[a] += sizes[b];
                            root = a;
                        }
                        anchor[root] = representative;
                        first[root] = start;
                        last[root] = end;
                    }
                    // Attach to one compatible preceding group. Joining several
                    // old groups through a single ambiguous frame creates drift.
                    break;
                }
            }
        }
    }
    for i in 0..parent.len() {
        parent[i] = find(&mut parent, i);
    }
    collect_groups(nodes, &parent, &sizes)
}
fn collect_groups(nodes: &[GroupNode], roots: &[usize], sizes: &[usize]) -> Vec<(usize, Vec<i64>)> {
    // Roots are dense indices: a vector avoids an extra hash table/probe per photo.
    let mut indices = vec![usize::MAX; nodes.len()];
    let mut groups: Vec<(usize, Vec<i64>)> = Vec::new();
    for (i, p) in nodes.iter().enumerate() {
        let root = roots[i];
        let index = if indices[root] == usize::MAX {
            indices[root] = groups.len();
            groups.push((p.order, Vec::with_capacity(sizes[root])));
            groups.len() - 1
        } else {
            indices[root]
        };
        groups[index].1.push(p.id);
    }
    groups
}

struct CachedComponent {
    nodes: Vec<GroupNode>,
    stream: Vec<usize>,
    groups: Vec<(usize, Vec<i64>)>,
    dirty: bool,
}
/// Persistent grouping for a stable photo snapshot and fixed options. Hash
/// batches only recompute affected time-connected components; no re-sort or
/// scan of unrelated photos. Recreate after snapshot/order/options changes.
/// Photo IDs must be unique, as in the catalog/service grouping worker.
/// Worst case for one continuous time stream is still O(n * max_neighbors).
/// Materializing all returned IDs necessarily costs O(n).
pub struct GroupingCache {
    options: GroupingOptions,
    components: Vec<CachedComponent>,
    positions: HashMap<i64, (usize, usize)>,
    heads: Vec<Option<(usize, usize)>>,
    dirty_components: Vec<usize>,
}
impl GroupingCache {
    pub fn new(photos: &[Photo], options: GroupingOptions) -> Self {
        let nodes = photo_nodes(photos);
        let streams = sorted_streams(photos, &nodes);
        let mut cache = Self {
            options,
            components: Vec::new(),
            positions: HashMap::with_capacity(photos.len()),
            heads: vec![None; photos.len()],
            dirty_components: Vec::new(),
        };
        // No compatible edge can cross a gap, including representative and span
        // checks. Such components can safely be recomputed independently.
        for stream in streams {
            let mut start = 0;
            for end in 1..=stream.len() {
                if end == stream.len()
                    || nodes[stream[end]]
                        .time
                        .abs_diff(nodes[stream[end - 1]].time)
                        > options.gap_ms
                {
                    let mut local: Vec<_> = stream[start..end].iter().map(|&i| nodes[i]).collect();
                    local.sort_unstable_by_key(|p| p.order);
                    let mut order: Vec<_> = (0..local.len()).collect();
                    order.sort_unstable_by_key(|&i| (local[i].time, local[i].order));
                    let component = cache.components.len();
                    for (i, node) in local.iter().enumerate() {
                        cache.positions.insert(node.id, (component, i));
                    }
                    cache.components.push(CachedComponent {
                        nodes: local,
                        stream: order,
                        groups: Vec::new(),
                        dirty: false,
                    });
                    cache.recompute(component);
                    start = end;
                }
            }
        }
        cache
    }
    fn recompute(&mut self, index: usize) {
        let component = &mut self.components[index];
        for (head, _) in &component.groups {
            self.heads[*head] = None;
        }
        component.groups = if self.options.mode == GroupingMode::Off {
            component
                .nodes
                .iter()
                .map(|p| (p.order, vec![p.id]))
                .collect()
        } else {
            group_nodes(
                &component.nodes,
                std::slice::from_ref(&component.stream),
                self.options,
            )
        };
        for (group, (head, _)) in component.groups.iter().enumerate() {
            self.heads[*head] = Some((index, group));
        }
    }
    /// Returns whether any known photo's hash changed. Repeated/unknown updates
    /// do no grouping work; multiple changes in a component are coalesced.
    pub fn update_hashes(&mut self, hashes: impl IntoIterator<Item = (i64, u64)>) -> bool {
        let mut changed = false;
        for (id, hash) in hashes {
            if let Some(&(component, position)) = self.positions.get(&id) {
                let node = &mut self.components[component].nodes[position];
                if node.hash != Some(hash) {
                    node.hash = Some(hash);
                    changed = true;
                    if matches!(
                        self.options.mode,
                        GroupingMode::Visual | GroupingMode::Burst
                    ) {
                        if !self.components[component].dirty {
                            self.components[component].dirty = true;
                            self.dirty_components.push(component);
                        }
                    }
                }
            }
        }
        while let Some(component) = self.dirty_components.pop() {
            self.components[component].dirty = false;
            self.recompute(component);
        }
        changed
    }
    pub fn groups(&self) -> Vec<Vec<i64>> {
        self.heads
            .iter()
            .flatten()
            .map(|&(component, group)| self.components[component].groups[group].1.clone())
            .collect()
    }
}

fn timestamp(p: &Photo) -> i64 {
    p.captured_at.unwrap_or(p.modified_at)
}
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(id: i64, time: i64, hash: Option<u64>) -> Photo {
        Photo {
            id,
            captured_at: Some(time),
            dhash: hash,
            ..Photo::default()
        }
    }
    fn reference_group_photos(photos: &[Photo], options: GroupingOptions) -> Vec<Vec<i64>> {
        if options.mode == GroupingMode::Off {
            return photos.iter().map(|p| vec![p.id]).collect();
        }
        let mut parent: Vec<usize> = (0..photos.len()).collect();
        let mut sizes = vec![1usize; photos.len()];
        let mut anchor: Vec<Option<usize>> = photos
            .iter()
            .enumerate()
            .map(|(i, p)| p.dhash.map(|_| i))
            .collect();
        let mut first: Vec<i64> = photos.iter().map(timestamp).collect();
        let mut last = first.clone();
        let burst_span = options.gap_ms.saturating_mul(8).clamp(2_000, 30_000);
        let mut streams: HashMap<_, Vec<usize>> = HashMap::new();
        for (i, p) in photos.iter().enumerate() {
            streams
                .entry((
                    &p.folder,
                    &p.camera,
                    p.captured_at.is_none(),
                    p.capture_time_kind.as_str(),
                ))
                .or_default()
                .push(i);
        }
        for stream in streams.values_mut() {
            stream.sort_unstable_by_key(|&i| (timestamp(&photos[i]), i));
            for pos in 1..stream.len() {
                let i = stream[pos];
                // Adjacent links suffice for time groups, avoiding work on large bursts.
                let limit = if options.mode == GroupingMode::Time {
                    1
                } else {
                    options.max_neighbors.clamp(1, 256)
                };
                for prev in (pos.saturating_sub(limit)..pos).rev() {
                    let j = stream[prev];
                    if timestamp(&photos[i]).abs_diff(timestamp(&photos[j])) > options.gap_ms {
                        break;
                    }
                    let similar = match options.mode {
                        GroupingMode::Time => true,
                        GroupingMode::Visual => {
                            matches!((photos[i].dhash,photos[j].dhash),(Some(a),Some(b)) if (a^b).count_ones()<=options.hamming_distance.min(64))
                        }
                        GroupingMode::Burst => match (photos[i].dhash, photos[j].dhash) {
                            (Some(a), Some(b)) => {
                                (a ^ b).count_ones() <= options.hamming_distance.min(64)
                            }
                            _ => true,
                        },
                        GroupingMode::Off => false,
                    };
                    if similar {
                        let a = find(&mut parent, i);
                        let b = find(&mut parent, j);
                        if a != b {
                            let start = first[a].min(first[b]);
                            let end = last[a].max(last[b]);
                            if options.mode == GroupingMode::Burst
                                && (end.abs_diff(start) > burst_span || sizes[a] + sizes[b] > 256)
                            {
                                continue;
                            }
                            if matches!(options.mode, GroupingMode::Visual | GroupingMode::Burst) {
                                if let (Some(left), Some(right)) = (anchor[a], anchor[b]) {
                                    if (photos[left].dhash.unwrap() ^ photos[right].dhash.unwrap())
                                        .count_ones()
                                        > options.hamming_distance.min(64)
                                    {
                                        continue;
                                    }
                                }
                            }
                            let representative = anchor[b].or(anchor[a]);
                            let root;
                            if sizes[a] < sizes[b] {
                                parent[a] = b;
                                sizes[b] += sizes[a];
                                root = b;
                            } else {
                                parent[b] = a;
                                sizes[a] += sizes[b];
                                root = a;
                            }
                            anchor[root] = representative;
                            first[root] = start;
                            last[root] = end;
                        }
                        // Attach to one compatible preceding group. Joining several
                        // old groups through a single ambiguous frame creates drift.
                        break;
                    }
                }
            }
        }
        let mut indices = HashMap::new();
        let mut groups: Vec<Vec<i64>> = Vec::new();
        for (i, p) in photos.iter().enumerate() {
            let root = find(&mut parent, i);
            let index = *indices.entry(root).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[index].push(p.id);
        }
        groups
    }
    #[test]
    fn randomized_grouping_matches_original_and_incremental_hashes() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x534841444f57);
        for _ in 0..160 {
            let mut photos: Vec<_> = (0..rng.random_range(0..100))
                .map(|i| {
                    let mut photo = p(
                        i,
                        rng.random_range(-10_000..10_000),
                        if rng.random_bool(0.5) {
                            Some(rng.random())
                        } else {
                            None
                        },
                    );
                    photo.folder = format!("folder{}", rng.random_range(0..3));
                    photo.camera = format!("camera{}", rng.random_range(0..2));
                    photo.capture_time_kind =
                        ["", "offset", "camera_local"][rng.random_range(0..3)].into();
                    if rng.random_bool(0.1) {
                        photo.captured_at = None;
                        photo.capture_time_kind.clear();
                        photo.modified_at = rng.random();
                    }
                    photo
                })
                .collect();
            for mode in [
                GroupingMode::Off,
                GroupingMode::Time,
                GroupingMode::Visual,
                GroupingMode::Burst,
            ] {
                let options = GroupingOptions {
                    mode,
                    gap_ms: if rng.random_bool(0.1) {
                        u64::MAX
                    } else {
                        rng.random_range(0..5000)
                    },
                    hamming_distance: rng.random_range(0..80),
                    max_neighbors: rng.random_range(0..300),
                };
                assert_eq!(
                    group_photos(&photos, options),
                    reference_group_photos(&photos, options)
                );
                let mut cache = GroupingCache::new(&photos, options);
                assert_eq!(cache.groups(), reference_group_photos(&photos, options));
                for _ in 0..5 {
                    let mut changes = Vec::new();
                    for _ in 0..3 {
                        if !photos.is_empty() {
                            let i = rng.random_range(0..photos.len());
                            let hash = rng.random();
                            photos[i].dhash = Some(hash);
                            changes.push((photos[i].id, hash));
                        }
                    }
                    cache.update_hashes(changes.iter().copied());
                    assert_eq!(cache.groups(), reference_group_photos(&photos, options));
                    assert!(!cache.update_hashes(changes.into_iter().rev().take(1)));
                }
            }
        }
    }
    #[test]
    #[ignore]
    fn bench_incremental_grouping_100k() {
        let photos: Vec<_> = (0..100_000)
            .map(|i| p(i, (i / 8) * 15_000 + (i % 8) * 120, None))
            .collect();
        let options = GroupingOptions {
            mode: GroupingMode::Burst,
            ..Default::default()
        };
        let began = std::time::Instant::now();
        let mut cache = GroupingCache::new(&photos, options);
        eprintln!("incremental setup {:?}", began.elapsed());
        let began = std::time::Instant::now();
        for id in 0..1000 {
            std::hint::black_box(cache.update_hashes([(id, 0)]));
        }
        eprintln!(
            "incremental one-hash update mean {:?}",
            began.elapsed() / 1000
        );
        let began = std::time::Instant::now();
        for _ in 0..100 {
            std::hint::black_box(cache.groups());
        }
        eprintln!(
            "incremental materialize 100k mean {:?}",
            began.elapsed() / 100
        );
    }
    #[test]
    #[ignore]
    fn bench_grouping_100k() {
        let photos: Vec<_> = (0..100_000)
            .map(|i| {
                let mut photo = p(i, (i / 8) * 15_000 + (i % 8) * 120, Some((i / 8) as u64));
                photo.folder = format!("/photos/long/folder/{:03}", i / 10_000);
                photo.camera = "Camera model serial number".into();
                photo
            })
            .collect();
        for mode in [
            GroupingMode::Time,
            GroupingMode::Burst,
            GroupingMode::Visual,
        ] {
            let options = GroupingOptions {
                mode,
                ..Default::default()
            };
            let began = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(reference_group_photos(
                    std::hint::black_box(&photos),
                    options,
                ));
            }
            eprintln!(
                "reference grouping {mode:?} 100k mean {:?}",
                began.elapsed() / 20
            );
            let began = std::time::Instant::now();
            for _ in 0..20 {
                std::hint::black_box(group_photos(std::hint::black_box(&photos), options));
            }
            eprintln!("grouping {mode:?} 100k mean {:?}", began.elapsed() / 20);
        }
    }
    #[test]
    fn temporal_chain_and_input_order() {
        let photos = vec![
            p(3, 2200, None),
            p(1, 0, None),
            p(2, 1100, None),
            p(4, 10_000, None),
        ];
        let g = group_photos(
            &photos,
            GroupingOptions {
                mode: GroupingMode::Time,
                ..Default::default()
            },
        );
        assert_eq!(g, vec![vec![3, 1, 2], vec![4]]);
    }
    #[test]
    fn visual_requires_hash_and_respects_threshold() {
        let photos = vec![
            p(1, 0, Some(0)),
            p(2, 1, Some(3)),
            p(3, 2, Some(u64::MAX)),
            p(4, 3, None),
        ];
        let g = group_photos(
            &photos,
            GroupingOptions {
                mode: GroupingMode::Visual,
                hamming_distance: 2,
                ..Default::default()
            },
        );
        assert_eq!(g, vec![vec![1, 2], vec![3], vec![4]]);
    }
    #[test]
    fn folders_and_cameras_are_separate() {
        let mut photos = vec![p(1, 0, None), p(2, 1, None)];
        photos[1].camera = "other".into();
        assert_eq!(
            group_photos(
                &photos,
                GroupingOptions {
                    mode: GroupingMode::Time,
                    ..Default::default()
                }
            )
            .len(),
            2
        );
    }
    #[test]
    fn dense_catalog_is_bounded_and_complete() {
        let photos: Vec<_> = (0..20_000).map(|i| p(i, 0, Some(i as u64))).collect();
        let groups = group_photos(
            &photos,
            GroupingOptions {
                mode: GroupingMode::Visual,
                max_neighbors: 8,
                ..Default::default()
            },
        );
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), photos.len());
    }
    #[test]
    fn missing_preview_cannot_bridge_incompatible_scenes() {
        let photos = vec![p(1, 0, Some(0)), p(2, 100, None), p(3, 200, Some(u64::MAX))];
        let options = GroupingOptions {
            mode: GroupingMode::Burst,
            ..Default::default()
        };
        assert_eq!(group_photos(&photos, options), vec![vec![1, 2], vec![3]]);
    }
    #[test]
    fn visual_representative_prevents_gradual_similarity_drift() {
        let photos = vec![p(1, 0, Some(0)), p(2, 100, Some(3)), p(3, 200, Some(15))];
        let options = GroupingOptions {
            mode: GroupingMode::Visual,
            hamming_distance: 2,
            ..Default::default()
        };
        assert_eq!(group_photos(&photos, options), vec![vec![1, 2], vec![3]]);
    }
    #[test]
    fn long_continuous_shoot_and_equal_mtimes_have_bounded_bursts() {
        let options = GroupingOptions {
            mode: GroupingMode::Burst,
            gap_ms: 1000,
            ..Default::default()
        };
        let photos: Vec<_> = (0..100).map(|i| p(i, i * 500, None)).collect();
        let groups = group_photos(&photos, options);
        assert!(groups.len() > 1);
        assert!(groups.iter().all(|g| g.len() <= 17));
        let same_time: Vec<_> = (0..1000).map(|i| p(i, 0, None)).collect();
        let groups = group_photos(&same_time, options);
        assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 1000);
        assert!(groups.iter().all(|g| g.len() <= 256));
    }
}
