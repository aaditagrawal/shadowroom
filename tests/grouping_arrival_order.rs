//! Burst grouping must not depend on the order perceptual hashes arrive in
//! (local preview decodes and remote agent analysis deliver them in any order).
use shadowroom::{grouping::*, model::Photo};

fn photos(hashes: [Option<u64>; 4]) -> Vec<Photo> {
    let t = [
        1792209800000i64,
        1792209800300,
        1792209800600,
        1792209800900,
    ];
    (0..4)
        .map(|i| Photo {
            id: 200 + i as i64,
            folder: "f".into(),
            modified_at: t[i],
            dhash: hashes[i],
            ..Default::default()
        })
        .collect()
}

#[test]
fn incremental_grouping_matches_full_recompute_for_every_arrival_order() {
    // Real hashes from a 4-frame panned burst (adjacent Hamming distances 10, 5, 4).
    let h = [
        6373803845650095152u64,
        1735274277326691344,
        1735054409360940048,
        2023284235756870680,
    ];
    let options = GroupingOptions {
        mode: GroupingMode::Burst,
        gap_ms: 2000,
        hamming_distance: 8,
        ..Default::default()
    };
    let normalize = |mut g: Vec<Vec<i64>>| {
        g.iter_mut().for_each(|x| x.sort());
        g.sort();
        g
    };
    let expected = normalize(group_photos(&photos(h.map(Some)), options));
    let mut order = [0usize, 1, 2, 3];
    // Heap's algorithm over all 24 permutations.
    let mut c = [0usize; 4];
    let check = |order: &[usize; 4]| {
        let mut cache = GroupingCache::new(&photos([None; 4]), options);
        for &i in order {
            cache.update_hashes([(200 + i as i64, h[i])]);
        }
        assert_eq!(
            normalize(cache.groups()),
            expected,
            "arrival order {order:?}"
        );
    };
    check(&order);
    let mut i = 0;
    while i < 4 {
        if c[i] < i {
            if i % 2 == 0 {
                order.swap(0, i)
            } else {
                order.swap(c[i], i)
            }
            check(&order);
            c[i] += 1;
            i = 0;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
}
