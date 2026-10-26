# Photograph grouping

A fresh app profile defaults to **Burst**, a 2,000 ms neighbor gap, and dHash Hamming distance 8. Existing saved preferences remain authoritative. `GroupingMode::default()` and `GroupingOptions::default()` are deliberately Off at the library level; the catalog service explicitly supplies the app's current preferences.

## Modes

| Mode | Membership rule |
| --- | --- |
| Off | No stacks in the app. The pure `group_photos` function returns each input ID as a singleton. |
| Time | Connect adjacent timestamps within the gap, regardless of hash. A continuous temporal chain can form a long group. |
| Visual | Require known hashes, a pair within the time gap, and similarity to the preceding group's representative. Unknown hashes remain singletons. |
| Burst | Use temporal neighbors provisionally while hashes are missing; apply pair and representative similarity when available. Bound the whole group's duration and size. |

All modes partition by source device/root and the exact `(folder, camera)` strings before comparing photographs. They additionally separate four timestamp domains: normalized UTC capture times, camera-local capture times, legacy capture times with unknown provenance, and file-modification fallbacks. Equal millisecond integers from different domains cannot establish temporal proximity. Different original EXIF offsets can still group when their UTC instants agree. Within each partition, order is timestamp then input position for ties. The catalog worker preserves both source identity and timestamp provenance. Mixed metadata can conservatively split a real burst; changed camera clocks, copied file modification times, and missing camera metadata can still reduce temporal accuracy.

## Representatives and bounds

Time mode needs only the immediately preceding photograph. Visual and Burst scan backward through at most `max_neighbors` preceding entries, stopping when the time gap is exceeded. The service uses 32 neighbors; the pure function clamps this option to 1–256. A compatible photograph attaches to one preceding group rather than joining several groups through one ambiguous frame.

Each visual-aware group retains a known-hash representative. When a new photograph has a hash, both its proposed neighboring pair and the two groups' representatives must pass the Hamming threshold. The established preceding group's representative is retained; an all-unknown provisional group gets its first representative when a known hash joins. Thus gradual pairwise similarity cannot drift arbitrarily away from that representative, and an unknown preview cannot bridge two known incompatible scenes. Members need not all be mutually similar: this is a representative test, not an all-pairs clique.

Burst additionally permits at most **256 photographs** and a total timestamp span of:

```
clamp(gap_ms * 8, 2,000 ms, 30,000 ms)
```

With the app's default gap, the maximum span is 16 seconds. These are caps on the entire group, not just each neighboring pair. Equal timestamps still obey the 256-photo cap. Visual and Time modes do not impose these Burst-specific total-span/size caps.

Groups and their members are returned in input order, with every input photograph retained, including singletons. Similarity is conservative when there are more temporal neighbors than the comparison cap: a suitable older candidate outside the bounded window can be missed.

## Work and progressive results

The pure grouping function borrows the photo array and allocates indices, union-find state, and ID groups. It does not clone image pixels or load files. Sorting costs O(n log n); the comparison stage examines at most n × k candidate neighbors, with near-constant amortized union-find operations. Additional memory is O(n). There is no whole-catalog quadratic image comparison.

The service runs grouping on a dedicated worker, using only ID, timestamps, folder, camera, and hash fields. Replaceable snapshots and generation checks prevent obsolete results from overwriting newer work. Notifications coalesce for at most 350 ms from the first wake; continuous hash notifications do not extend that deadline. Each pass still recomputes groups for the current compact snapshot: this is bounded candidate work per pass, not an incremental sorted-tree algorithm. An already-running pass finishes before its obsolete result is discarded. The coalescing bound is not a guarantee of total completion latency under CPU contention or repeated snapshot replacement. Hashes arrive from the preview pipeline rather than from an extra full-image decode in the indexer. Provisional Burst groups can split as visual evidence arrives; Visual groups appear as hashes become available. This is an approximate culling aid: dHash can confuse similar coarse patterns and does not establish that photographs are identical.

Hashes are also persisted as rebuildable metadata in the local SQLite catalog. The actor coalesces updates by photo ID and commits batches when 32 hashes are pending or 300 ms has elapsed; durability barriers flush pending hashes as well. Hash-only writes preserve rating, flag, and color-label columns. A reopened unchanged catalog can therefore group using its existing hashes without decoding those photographs again. New or refreshed source metadata may require new preview hashes. Originals receive no hash/cache writes.

## Verification

Run the grouping regressions with:

```sh
cargo test --locked --no-default-features --lib grouping
```

Existing grouping tests cover temporal chains and input order, camera separation, missing hashes, threshold behavior, representative drift, unknown-hash bridging, long-shoot span limits, equal-timestamp size limits, and completeness for 20,000 dense records. Service tests cover replacing obsolete grouping work and applying hashes off the UI thread. After the bounded-coalescing change, the targeted `--lib group` run passed 10/10 tests (98 total tests discovered); its new continuous-wake regression injects a monotonic clock and an always-ready receiver, verifying shrinking waits and termination without scheduler-dependent elapsed-time assertions. These are core correctness tests; native library timing and memory measurements are documented separately in `BENCHMARK.md`.
