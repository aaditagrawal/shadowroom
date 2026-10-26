# GitHub Actions build and release performance

This work measures the CI and release pipeline. Application startup and photo
review measurements remain in [Performance](PERFORMANCE.md).

## Original failure

The [original workflow run](https://github.com/aaditagrawal/shadowroom/actions/runs/40357926435)
failed during core tests on both operating systems. Ubuntu finished its failed
job in 98 seconds; macOS in 76 seconds. Neither native packaging job ran.
These failed jobs are diagnosis evidence, not a successful performance baseline.

The ingest tests depended on private JPEG files, and macOS exposed a lexical
`/var` versus canonical `/private/var` path comparison. The tests now generate
their JPEG/EXIF data and use canonical temporary roots. Core behavior and source
integrity assertions remain enabled.

## Build tuning

- Pin the Rust toolchain and release inputs so cache reuse has a stable basis.
- Cache Cargo dependencies separately for Ubuntu core, macOS core, EL8 Linux
  packages, and macOS packages. Mount the actual cached Linux target directory
  inside the container.
- Disable incremental compilation and test/development debug information in CI
  to reduce cache size and linking work.
- Run desktop unit tests in the release profile already used for native packages,
  avoiding a second GPUI dependency build in the debug profile. The core matrix
  retains debug coverage and the desktop test assertions remain enabled.
- Disable unused DWARF generation in the main-only Fedora RPM while preserving
  the remaining Fedora Rust hardening flags and executable build-ID checks.
- Cache the EL8 build-image layers and verified codec outputs separately from
  application binaries. Changed application source must still rebuild.
- Bound compilation/test concurrency and cancel superseded branch runs while
  allowing an in-progress version-tag release to finish.
- Publish only after all required checks, native packages and source bundles
  succeed. Verify asset checksums before making the release public.

The [raw timing summary](benchmarks/actions.json) contains public run identifiers,
commit hashes and job/step durations, without runner logs or private files.

## Reproduce the timing comparison

Run a full successful workflow, then repeat the same revision after its caches
have been saved. Record cache hits from the workflow logs. Export both timelines:

```sh
gh run view RUN_ID --repo aaditagrawal/shadowroom --attempt 1 \
  --json attempt,databaseId,url,headSha,status,conclusion,createdAt,jobs > cold.json
gh run view RUN_ID --repo aaditagrawal/shadowroom --attempt 2 \
  --json attempt,databaseId,url,headSha,status,conclusion,createdAt,jobs > warm.json
uv run --no-project python scripts/summarize-actions.py \
  cold=cold.json warm=warm.json --output actions.json
```

Runner elapsed time spans first job start through final job completion and
includes dependency waits. Summed job time counts parallel jobs separately.
Both are wall-clock measurements, including network and cache transfer; neither
is CPU execution time. Hosted-runner variation prevents attributing every second
of a rerun difference to caching alone. Failed runs are excluded from speed
comparisons.
