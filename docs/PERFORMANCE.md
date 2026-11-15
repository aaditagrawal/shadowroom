# Performance

Measured on 2026-10-26 with Shadowroom 0.1.0's optimized release build, Rust
1.99.1, an Intel(R) Core(TM) i5-10300H CPU @ 2.50GHz, 8 logical CPUs,
and Fedora Linux 45 (Workstation Edition). The desktop ran through native GPUI on X11/Xwayland.
No builds or other Shadowroom instances ran alongside the benchmark. Ordinary
desktop applications remained running; the host was not CPU-isolated.

## Native catalog and command workload

Each of three independent runs started with a new private catalog, a 512 MiB
preview budget, a 128 MiB disk cache, grouping off, and drive discovery and
auto-advance disabled. The fixture contains 10,000 paths in 40 folders, hardlinked
to 16 distinct 128 × 80, 24-bit BMP files. Freshly created source data was warm in
the operating system's page cache.

Indexing completed in a median **0.278 s**. This fixture
exercises catalog/path processing with tiny images. It does not measure a real
camera card, 10,000 distinct full-size photographs, RAW decoding, network access,
or uncached storage throughput.

| Command | Median | p95 |
| --- | ---: | ---: |
| Photo selection | 0.58 ms | 6.41 ms |
| Star-rating acceptance | 0.22 ms | 1.97 ms |
| Selection with SQLite writer blocked | 0.52 ms | 0.73 ms |
| Warm loupe selection | 0.89 ms | 1.00 ms |

Each grid/decision command has 90 samples pooled across three runs; warm loupe
selection has 36. Median uses all samples and p95 uses nearest rank. Individual
runs and every latency sample are retained in [release.json](benchmarks/release.json).

Timings run from the harness's MCP request to its response, including Python,
JSON-RPC, bridge transport, and desktop dispatch. They are not frame presentation
or end-to-end preview latency. A star-rating response acknowledges acceptance;
its FULL-synchronized SQLite save happens asynchronously. The suite separately
waits for durable decisions. The blocked-writer case deliberately holds a SQLite
writer transaction while selections and decisions continue.

## Startup and memory

| Run | Endpoint + MCP readiness | Index 10k paths | Peak process RSS | Warm reverse source reads |
| --- | ---: | ---: | ---: | ---: |
| 1 | 0.322 s | 0.278 s | 197.0 MiB | 0 |
| 2 | 0.296 s | 0.283 s | 201.1 MiB | 0 |
| 3 | 0.288 s | 0.277 s | 203.6 MiB | 0 |

Startup measures process launch through private endpoint readiness and the MCP
handshake, including polling/transport. It does not identify the first presented
frame. The median is **0.296 s** on this warmed host.

The raw data includes process resident memory sampled every 100 ms,
starting after readiness. Peak RSS in the table comes from Linux's kernel
high-water counter. It includes allocations outside the media pipeline. The
512 MiB preview budget is not a cap on total RSS, native codecs, or GPU memory.
GPU memory was not measured; frame times and input latency were measured later,
in [Human interface responsiveness](#human-interface-responsiveness-2026-11-16).

All runs exited normally, preserved source hashes and mtimes, and completed the
12-photo reverse loupe pass without additional source reads or decodes. Benchmark
wall duration includes intentional settle waits and writer contention; it is not
CPU execution time.

## Reproduce

Build once, then run measurements with the compiler and other instances idle.
A working X11/Xwayland `DISPLAY` is required. Each output directory must be new.

```sh
LCMS2_STATIC=1 cargo build --locked --release --bins
uv run --no-project python scripts/native-benchmark.py \
  --binary target/release/shadowroom --count 10000 --samples 30 \
  --output /tmp/shadowroom-bench-1
# Repeat with fresh output directories: /tmp/shadowroom-bench-2 and -3.
uv run --no-project python scripts/summarize-benchmarks.py \
  /tmp/shadowroom-bench-1/report.json /tmp/shadowroom-bench-2/report.json \
  /tmp/shadowroom-bench-3/report.json
```

The raw native reports also retain transcripts, process samples, private catalog,
fixture details, cache counters, load averages, and normal-exit/source-integrity
checks. The portable published JSON excludes private session paths while retaining
all latency samples and RSS traces. It identifies executable SHA-256
`df443580aa94f5dc3c89d137fb23987eedb2d20927eb52eb797da68e5c031802`.

## State-machine review, 2026-11-09

The separate [round-4 results](benchmarks/state-machines-round4.json) compare
`7e03044` with `491561e` using optimized builds with default features. The host
was an Intel Core i5-10300H, Rust 1.99.1, and Linux; exact toolchain/platform,
binary hashes and all outputs are retained in the JSON. No compiler or other
Shadowroom test process ran during measurement. Ordinary desktop processes
remained running and the host was not CPU-isolated.

The catalog fixture starts with 50,000 synthetic photos containing realistic
metadata. Each warm result is the minimum of five refreshes; the table reports
the median of three independent process runs. Cold refresh and initial writes
have one sample per process. Catalogs used the persistent filesystem containing
the checkout, with a warm page cache. The five indexing samples add 500 photos
each, so subsequent decision/hash cases include those additions. Refresh costs
include actor-side reading, sorting/group input and publication, excluding UI
layout, presentation and source decoding.

| Catalog operation | Before | After |
| --- | ---: | ---: |
| Unchanged refresh | 99.94 ms | 38.61 ms |
| Refresh after 500 indexed photos | 104.83 ms | 63.53 ms |
| Refresh after 100 decisions | 114.36 ms | 44.46 ms |
| Refresh after 32 hashes | 110.12 ms | 59.22 ms |
| First refresh | 110.41 ms | 175.07 ms |
| Initial 50k metadata writes | 679.69 ms | 1069.34 ms |

The parsed snapshot baseline avoids repeated JSON parsing and sends only
changed rows to the UI. Equal rows, such as already-applied decisions, skip UI
rebuilds. This has costs: the first refresh creates the baseline, metadata
revision triggers add write work, and the baseline remains in memory. Median
peak RSS for the entire catalog benchmark process increased from 263.1 MiB to
278.4 MiB. These peaks include fixture vectors and grouping workers; they do
not establish steady-state application memory use. First-load and write costs
remain candidates for a later optimization pass.

The import fixture contains 128 distinct generated 200,000-byte files and one
backup, all on the same persistent filesystem. Three fresh runs per revision
measure copy, BLAKE3 verification, syncs, catalog commits and manifest
finalization; source creation and planning are outside the execution timer.
Median execution fell from **2013.37 ms to 210.75 ms**. This small-file workload
exposes commit/sync overhead; it does not measure RAW decoding, a camera card,
or backups on separate physical devices. Every output still verifies and syncs
before its batch's catalog transaction commits.

The Import grid microbenchmark emulates the former per-cell clones/formatting
beside the shared-cell model: 36 visible cells over a 5k plan, 2,000 frames per
path, with three process runs. Median data preparation per frame fell from
**9.76 µs to 1.31 µs**. This excludes GPUI layout, texture upload and presentation
and does not establish a frame-rate improvement.

Reproduce the final paths after one release build, keeping builds and other
Shadowroom instances idle during measurement:

```sh
LCMS2_STATIC=1 scripts/check-linux-local.sh test --locked --release \
  --lib --bins --test ingest --no-run
TMPDIR=/absolute/persistent/benchmark-directory LCMS2_STATIC=1 \
  scripts/check-linux-local.sh test --locked --release --lib \
  bench_snapshot_refresh -- --ignored --nocapture
SHADOWROOM_INGEST_BENCH_DIR=/absolute/persistent/benchmark-directory \
  SHADOWROOM_INGEST_BENCH=128x200000 LCMS2_STATIC=1 \
  scripts/check-linux-local.sh test --locked --release --test ingest \
  ingest_throughput_benchmark -- --ignored --nocapture
LCMS2_STATIC=1 scripts/check-linux-local.sh test --locked --release --bins \
  grid_cell_frame_cost -- --ignored --nocapture
```

## Human interface responsiveness, 2026-11-16

These measurements answer how quickly the desktop shows the result of a key or
pointer action, what each frame costs, and whether the window stays quiet when
nothing changes. [Raw data](benchmarks/hi-responsiveness.json) keeps every
run's environment, binary hash, scenario tables, idle samples and per-input
latencies.

### How GPUI 0.2.2 turns input into a frame

- **One view, whole-tree frames.** `Workspace` is the only GPUI view. Any
  `cx.notify()` marks the window dirty, and the next frame calls
  `Workspace::render`, rebuilds every element, runs Taffy layout, prepaints
  (hitboxes, dispatch tree) and paints the whole tree into a new scene. Between
  frames GPUI keeps shaped text lines from the previous frame, rasterized glyph,
  SVG and image tiles in its GPU atlas, and per-element state (scroll offsets,
  hover, running animations). It does not keep layout or tessellated paths:
  `paint_path` runs Lyon tessellation every frame.
- **Scheduling.** On X11 a calloop timer fires at the RandR refresh interval
  (here 59.87 Hz) and draws only a dirty window, so a frame waits up to one
  interval for the next tick; Wayland draws on compositor frame callbacks and
  macOS on `CVDisplayLink`. For one second after any input GPUI re-presents the
  last scene every tick to keep the display from down-clocking. A keystroke that
  arrives while the window is dirty is preceded by a full draw (without present)
  so key bindings match the latest tree.
- **Present.** `Window::present` encodes and submits the scene through Blade
  (Vulkan, with GPUI's low-latency `DisplaySync::Recent` preference) and waits
  for the previous frame's GPU work. The trace's "present" is that submission
  returning, not photons; the X server sees the frame (XDamage) a median
  ~7.6 ms later on this host.

### Instrumentation and method

`SHADOWROOM_FRAME_TRACE=/path/trace.jsonl` (off by default; one `Option`
check per hook when unset) records, in `CLOCK_MONOTONIC`: keystrokes from a GPUI
keystroke interceptor (before any handler), pointer presses and drags at the
root, the end of dispatch, `Workspace::render` start and end, an empty probe
element painted last (splitting layout + prepaint from paint), and the instant
the frame's draw and present returned (an effect GPUI flushes right after
`present`). Each frame also records main-thread CPU time
(`CLOCK_THREAD_CPUTIME_ID`, which excludes time the thread was preempted), CPU
per region (title bar, sidebar, filters, center, rating bar, filmstrip,
inspector, status bar), whether it was presented or drawn for key dispatch, and
which preview of the current photo it showed.

[`scripts/native-responsiveness.py`](../scripts/native-responsiveness.py) runs
the release binary on a private rootful Xwayland server with private
HOME/XDG directories, 48 photos (eight copies of the six 2000 × 1333 demo JPEGs,
copied; sources are verified unchanged), a 1440 × 900 window, automatic advance
on and grouping off. It injects input with XTEST from the same process,
stamping each event before sending it, and listens for XDamage on the app
window from a second X connection. Interactions, 300 ms apart unless noted:
next/previous photo in Review (40 each), held arrow keys (X autorepeat, 2 s),
ratings 1–5 and Pick/Reject with automatic advance (20 each), ratings without
automatic advance, Z (fit/100 %), pointer pans at 100 % (40 motion events per
drag, 16 ms apart), Compare next/previous (24 each), G/L/C view switches, Catalog
arrows and a held arrow, Ctrl+K/Escape, typing and deleting “export” in the
palette, and Ctrl+,/Escape. Idle periods measure 8 s after a 1.5 s pause
(outlasting GPUI's one-second post-input presentation).

Input → present runs from the XTEST send to the next presented frame; for held
keys it starts at the app's receipt of each autorepeat. Percentiles pool every
input of every run of a configuration (nearest rank). Frame CPU is the
main-thread CPU time from `render` start to the end of paint of the first
frame after the input.

**Host.** Intel Core i5-10300H (8 logical CPUs, `performance` governor), Intel
UHD Graphics (CometLake-H GT2) through Mesa's Vulkan driver, Fedora Linux 45,
kernel 7.2.11, GNOME on Wayland; built-in 2560 × 1440 panel at 59.96 Hz; the
private Xwayland server reports 59.87 Hz. Other builds and screenshot
runs shared the machine during the first rounds (1-minute load average 9–16);
later rounds ran with loads of 0.3–3. Configurations were interleaved round by
round, so each saw similar conditions, but wall-clock latency still moves with
load. Main-thread CPU time and the preview counts are the load-robust numbers.

### Results

The table pools three quiet-host runs per configuration (1-minute load
0.4–3): **before** is `ux/hi-refinements` with only the instrumentation,
**after** adds fixes 1, 2 and 4 below with Reduce motion on, and **after,
motion on** is the same build with the fades. Fix 3 came last and was measured
separately (below); it changes only Catalog/filmstrip thumbnail fallbacks.
Latency cells are p50 / p95 / max; frame CPU is the next frame's main-thread
CPU (render, layout, prepaint and paint) p50 / p95 / max. "Sharp first frame"
counts inputs whose first presented frame already showed the current photo's
fit preview rather than its thumbnail. `Workspace::render` itself (building
the element tree) took 0.29 / 0.76 / 4.9 ms before and 0.29 / 0.73 / 3.7 ms
after across all inputs, so nearly all frame CPU is GPUI layout, prepaint and
paint.

| Interaction | Inputs | Input → present ms, before | after | after, motion on | Frame CPU ms, before | after | Sharp first frame, before → after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Review: next photo | 120 | 16.4 / 24.4 / 33 | 14.7 / 22.8 / 30 | 15.2 / 23.7 / 28 | 5.0 / 8.2 / 9 | 3.7 / 7.4 / 9 | 0/120 → 120/120 |
| Review: previous photo | 120 | 15.8 / 26.1 / 33 | 14.8 / 26.7 / 32 | 14.7 / 31.8 / 66 | 3.8 / 6.2 / 7 | 2.6 / 5.2 / 7 | 33/120 → 120/120 |
| Review: held arrow (autorepeat) | 210 | 13.6 / 22.1 / 25 | 13.2 / 20.3 / 23 | 12.6 / 20.6 / 22 | 3.5 / 6.5 / 10 | 2.6 / 5.2 / 8 | 78/210 → 159/210 |
| Rating 1–5, auto-advance | 60 | 16.6 / 31.1 / 32 | 15.8 / 25.3 / 32 | 15.4 / 28.8 / 31 | 4.6 / 6.2 / 7 | 3.6 / 5.5 / 7 | 26/60 → 60/60 |
| Pick/Reject, auto-advance | 60 | 15.2 / 23.2 / 32 | 16.2 / 28.2 / 49 | 16.0 / 29.1 / 31 | 4.7 / 6.5 / 7 | 3.9 / 6.9 / 13 | 19/60 → 60/60 |
| Rating without auto-advance | 60 | 14.8 / 26.1 / 32 | 14.7 / 26.4 / 29 | 13.7 / 18.9 / 24 | 3.0 / 4.0 / 4 | 2.3 / 3.4 / 4 | 60/60 → 60/60 |
| Z: fit ↔ 100 % | 33 | 15.7 / 24.0 / 25 | 14.5 / 22.4 / 29 | 14.1 / 20.4 / 21 | 3.1 / 4.4 / 5 | 2.3 / 3.5 / 4 | 33/33 → 33/33 |
| Pan drag at 100 % (per motion event) | 480 | 15.8 / 30.9 / 36 | 15.6 / 31.5 / 35 | 15.2 / 31.1 / 34 | 2.7 / 3.7 / 5 | 2.0 / 2.9 / 4 | – |
| Compare: next | 72 | 16.1 / 27.9 / 34 | 15.0 / 26.8 / 31 | 14.8 / 27.5 / 31 | 4.5 / 6.9 / 9 | 3.5 / 5.3 / 8 | 40/72 → 72/72 |
| Compare: previous | 72 | 15.5 / 26.4 / 32 | 14.7 / 25.1 / 30 | 15.2 / 28.3 / 31 | 3.7 / 5.4 / 8 | 2.9 / 4.3 / 5 | 65/72 → 72/72 |
| G / L / C view switch | 75 | 14.9 / 23.9 / 29 | 14.5 / 27.2 / 32 | 14.9 / 27.9 / 33 | 3.8 / 5.5 / 6 | 2.7 / 4.8 / 6 | – |
| Catalog arrows | 96 | 16.0 / 23.9 / 27 | 15.7 / 26.7 / 32 | 16.6 / 26.8 / 32 | 4.3 / 8.2 / 10 | 3.8 / 10.1 / 14 | – |
| Catalog held arrow | 105 | 18.3 / 24.5 / 25 | 13.7 / 22.0 / 25 | 15.7 / 24.1 / 27 | 5.1 / 7.1 / 8 | 4.4 / 6.0 / 7 | – |
| Ctrl+K / Escape | 60 | 15.7 / 26.4 / 32 | 14.7 / 22.8 / 31 | 15.8 / 26.4 / 29 | 4.8 / 6.0 / 6 | 4.1 / 5.4 / 6 | – |
| Typing in the palette | 174 | 16.4 / 29.0 / 34 | 16.3 / 30.2 / 34 | 14.9 / 30.2 / 32 | 4.5 / 5.9 / 7 | 3.8 / 5.5 / 6 | – |
| Ctrl+, / Escape | 66 | 16.2 / 26.1 / 32 | 16.1 / 27.6 / 34 | 17.2 / 29.1 / 35 | 5.2 / 6.9 / 8 | 5.0 / 7.5 / 9 | – |

| Idle state (8 s) | CPU %, before → after | Main-thread wakeups/s | Frames/s, before → after |
| --- | ---: | ---: | ---: |
| Review | 0.33 → 0.37 | 63.9 → 63.9 | 0.0 → 0.0 |
| Compare | 0.37 → 0.33 | 63.8 → 63.9 | 0.0 → 0.0 |
| Catalog | 0.33 → 0.33 | 63.9 → 63.9 | 0.0 → 0.0 |
| Palette open, caret blinking | 1.42 → 1.33 | 67.0 → 66.4 | 2.0 → 2.0 |
| Palette open, 10 s after the last key | 1.46 → 0.33 | 66.9 → 63.8 | 2.0 → 0.0 |
| Preferences open | 0.29 → 0.41 | 63.9 → 63.7 | 0.0 → 0.0 |

The sharp preview now arrives with the first frame: input → sharp preview fell
from 33.7 / 50.1 ms (p50 / p95) to 14.7 / 22.8 ms for the next photo, from
21.4 / 47.2 to 15.8 / 25.3 ms for a rating with auto-advance, and from
18.2 / 42.9 to 15.0 / 26.8 ms for Compare. Held arrow keys still outrun
decoding: autorepeat at 25 Hz asks for a new 2000 × 1333 decode every 40 ms,
so 159 of 210 repeats (78 before) showed the sharp preview at once.

Frames longer than one display interval (16.7 ms of wall time) were rare on
the quiet host: 12 of 3,559 presented frames before and 37 of 3,618 after,
21 of the 37 during Catalog arrows, in the thumbnail bursts that fix 3
removes. In the earlier rounds, when other builds kept the 1-minute load at 9–16, input →
present p95 rose to 30–60 ms in every configuration; those runs are kept out of
the table but showed the same CPU and preview-count changes.

### Fixes, before and after

Per-region CPU and stack samples (`eu-stack` on the release binary) located the
costs; each fix was measured against the build without it.

1. **Histogram paths reused.** The inspector histogram tessellated seven band
   fills and three or four outlines of 256 bins with Lyon on every frame, for
   any repaint anywhere in the window. The tessellated paths are now cached
   until the plot's geometry or data changes. Mean inspector CPU per frame fell
   from 1.12 ms to 0.44 ms across interactions, about two thirds of the
   next-frame CPU drop in the table.
2. **Neighbouring textures prepared before navigation.** Read-ahead decoded the
   next and previous photos, but turning a decoded preview into a GPUI texture
   took a background conversion and an atlas upload in `paint`, both started
   only when the photo was first drawn. Every advance therefore showed the
   thumbnail first and the sharp preview one or more frames later, with the
   upload landing in that later frame: this was the remaining "second repaint".
   Review and Compare now convert the neighbours' fit previews and paint them
   invisibly (1 px, zero opacity) so the atlas holds them when the key arrives.
   Zoomed views skip this; full-resolution textures are too large to keep
   speculatively.
3. **Thumbnail fallbacks scaled down.** When a grid or filmstrip cell's
   thumbnail had left the preview cache, the cell converted and uploaded the
   whole fit preview (10.6 MB here) instead. Display textures reserve preview
   memory, so after long Review passes at the default 512 MB budget the first
   Catalog scroll could draw ~600 ms of 10–25 ms frames. The fallback is now
   scaled to the 384 px thumbnail edge in the background conversion.
   In three runs with this change, no Catalog frame spent more than 1.9 ms
   painting the grid, against up to 24 ms (16–19 frames above 4 ms per run) in
   4 of 6 quiet runs without it; a 2048 MB budget also avoided the burst.
   Those three runs met heavier host load (1-minute load 0.7–41), so their
   latencies are kept in the raw data rather than the table.
4. **Caret blink timeout.** Each caret blink repaints the whole window (one
   view), so an open palette or text field drew two frames a second forever.
   Like GTK's default, the caret now stops blinking and stays visible 10 s after
   the last key in the field.

### Motion

Menus, dialogs and the palette fade in over 120 ms from 30 % opacity; notices,
the error toast and the trailing decision note over 150 ms; the decision chip
settles in from 60 % when it first appears on the photo on screen (see
[Themes and preferences](THEMES.md#motion)). Every entrance is readable on the
frame that answers the key, a notice replacing a visible one does not fade
again, and navigation, photo changes, zoom and pan never animate. GPUI 0.2.2
has no layout-independent transform for elements, so there is no scale.

Each animation frame is a full workspace frame. Opening the palette or
Preferences drew a median 9 frames in the following 250 ms with motion on
instead of 1; decisions drew the same number of frames either way, because
repeated notices and chip changes do not animate. Input → present was unchanged
within the spread between runs: all inputs pooled, p50 15.1 → 15.3 ms and p95
27.2 → 28.3 ms; Ctrl+K / Escape p50 14.7 → 15.8 ms. A keystroke that arrives
during a fade waits at most for the frame in progress (4–7 ms of CPU) and the
next tick, as it would for any repaint.
**Reduce motion** (Preferences → Appearance, or the system setting) removes the
wrappers and their frames entirely.

### Not changed

- **Scheduling dominates latency.** On a quiet host the next frame's CPU is
  2–5 ms, but input → present is ~15 ms: a median ~6 ms wait for GPUI's X11
  refresh tick (uniform over one interval, not phase-locked to input) and, for
  about a third of keystrokes, up to one more interval before GPUI handles the X
  event at all (p95 ~12 ms; the rest arrive in under 1 ms). The X server then
  sees the frame a median ~7.6 ms after `present` returns. These are GPUI and
  X11 behaviour, not application work.
- **Background repaints after decodes.** Each Review step is followed by about
  three poll-driven frames (~3 ms CPU each) as read-ahead decodes finish. The
  neighbour prefetch relies on them, so they stay.
- **Whole-tree frames.** The Catalog grid is the largest remaining region
  (2–4 ms per frame). Moving the sidebar, inspector and grid into separately
  cached GPUI views (`AnyView::cached`) would let input that changes only the
  centre skip their layout and paint; in GPUI 0.2.2 notifying a child view also
  invalidates its ancestors, so that needs state ownership restructured rather
  than wrappers.
- **Idle.** No idle state draws frames once the caret rests. The main thread
  wakes ~64 times a second: GPUI's 60 Hz X11 refresh timer runs whenever the
  window is visible, plus the app's 4 Hz idle poll; idle CPU is ~0.3 %.
- **Pre-dispatch draws.** A few keystrokes per run arrived while the window was
  dirty and paid GPUI's extra draw before dispatch.

### Reproduce

Build a release binary, keep compilers and other Shadowroom instances idle, and
use a new output directory per run. A Wayland session with Xwayland is required
for the private X server.

```sh
LCMS2_STATIC=1 cargo build --locked --release --bin shadowroom
uv run --no-project python scripts/native-responsiveness.py \
  --binary target/release/shadowroom --photos /path/to/photos \
  --output /tmp/shadowroom-hi-1 --label after
# Repeat with new output directories; --reduce-motion 1 or 0 sets
# SHADOWROOM_REDUCE_MOTION. Then pool runs by label:
uv run --no-project python scripts/summarize-responsiveness.py \
  --output /tmp/hi-responsiveness.json /tmp/shadowroom-hi-*/report.json
```

## CPU histogram workload

The separate example isolates histogram computation on synthetic, memory-resident
RGB8 pixels, excluding decoding, storage, and GUI work:

```sh
cargo run --locked --release --no-default-features --example histogram_benchmark
```

It alternates the current sampled-pixel implementation with a retained averaged
thumbnail baseline, warms both paths, then measures 21 iterations per method.
[Raw results](benchmarks/histogram.json) include clipping estimates as well as
latencies. These algorithm measurements do not establish end-to-end viewing speed.
