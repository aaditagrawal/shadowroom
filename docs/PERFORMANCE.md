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
GPU memory and frame times were not measured.

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
