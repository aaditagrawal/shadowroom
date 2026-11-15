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
