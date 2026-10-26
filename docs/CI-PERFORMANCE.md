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

## Measured repeat

Two successful attempts of commit `0d6c94148cf5d878771e40eeabc3984d6f9535de`
ran the same checks and packaging work on October 26, 2026 UTC:
[first verified run](https://github.com/aaditagrawal/shadowroom/actions/runs/40386475051/attempts/1)
and [repeat](https://github.com/aaditagrawal/shadowroom/actions/runs/40386475051/attempts/2).

| Measurement | First verified | Repeat |
| --- | ---: | ---: |
| Elapsed runner span | 20m39s | 9m26s |
| Summed job time | 45m56s | 22m11s |
| Linux package job | 18m50s | 5m08s |
| macOS package job | 10m27s | 6m11s |
| Fedora RPM job | 13m18s | 7m45s |
| Linux display-package setup | 5m21s | 15s |

Elapsed runner span decreased by **54.32%**, or 11m13s. This measures the combined
effect of cache reuse and hosted-runner variation, not an isolated CPU speedup.
The first attempt had empty native Cargo, patched-xattr, codec and DEB caches;
core caches and container-image layers were already warm. It was not a fully
cold environment. Both attempts passed every required check, including the
actual AppImage native preview and clean Fedora installation.

The repeat logs confirm Cargo, xattr, codec and DEB cache hits. APT reported
`0 B/17.6 MB` to download. Linux and macOS no longer compiled `xattr` or GPUI;
application binaries and desktop-test binaries still compiled. The Fedora RPM's
separate build tree still compiled its patched xattr/GPUI chain. These results
do not measure application startup, photo rendering or user-interface latency.

## Cross-commit Fedora correction

The [published v0.1.1 run](https://github.com/aaditagrawal/shadowroom/actions/runs/40391316246)
exposed a separate issue after the identical-commit comparison. Commit `549085d`
changed only documentation, with the same application and lockfile bytes, and
restored the same Fedora target cache. Nevertheless, its release compile took
7m51s and compiled 303 crates, versus 3m18s and five crates in the prior repeat.

The source-RPM preparer assigned every vendored registry file the new application
commit's timestamp. A small build-script probe reproduced Cargo marking an
unchanged dependency dirty through `rerun-if-changed`; the rebuild propagated to
its dependents. The preparer now fixes only locked registry inputs to January 1,
1980. Source0 application files and patched path dependencies keep their source
commit timestamps, and changed application source still rebuilds. Generated
notices and provenance retain the selected source commit.

The Fedora cache namespace also includes `Cargo.lock` and the preparation/build
scripts, preventing fallback across changed dependency or build-input identities.
A complete SRPM validation preserved all 37,541 registry files' bytes, modes and
membership and verified 747 package checksums and 36,794 file checksums. The
54.32% table above predates this correction; it is not a hosted measurement of
this additional fix.

## Build tuning

- Pin the Rust toolchain and release inputs so cache reuse has a stable basis.
- Cache Cargo dependencies separately for Ubuntu core, macOS core, EL8 Linux
  packages, and macOS packages. Mount the actual cached Linux target directory
  inside the container.
- Include container/specification and vendor hashes in `shared-key`: the pinned
  [Rust cache implementation](https://github.com/Swatinem/rust-cache/blob/e9d7b9f1e587218970671bac74e5511ed9506e6a/src/config.ts)
  ignores its separate `key` input when `shared-key` is set.
- Preserve only patched `xattr` dependency artifacts in a separate exact-key
  cache, saved before the Rust cache removes local path dependencies. Normalize
  only tracked `vendor/xattr` file mtimes; the key includes all vendor bytes,
  manifests, toolchain, platform and build configuration, with no fallback keys.
  Application sources retain normal rebuild behavior. A restored-cache Linux run
  still rebuilt `xattr` and GPUI in its 194-second release build; measure a
  successful warm run before attributing savings to this targeted cache.
- Disable incremental compilation and test/development debug information in CI
  to reduce cache size and linking work.
- Run desktop unit tests in the release profile already used for native packages,
  avoiding a second GPUI dependency build in the debug profile. The core matrix
  retains debug coverage and the desktop test assertions remain enabled.
- Disable unused DWARF generation in the main-only Fedora RPM while preserving
  the remaining Fedora Rust hardening flags and executable build-ID checks.
- Cache the EL8 build-image layers and verified codec outputs separately from
  application binaries. Changed application source must still rebuild.
- Check installed Ubuntu display/runtime tools before running APT. The first
  hosted package-setup step took 315 seconds; installed tools can be reused.
  Cache downloaded DEB archives for packages absent from the runner; APT still
  verifies packages and installs their declared dependencies.
- Upload already-compressed archives and RPMs without another compression pass.
- Bound compilation/test concurrency and cancel superseded branch runs while
  allowing an in-progress version-tag release to finish.
- Publish only after all required checks, native packages and source bundles
  succeed. Verify asset checksums before making the release public.

The [raw timing summary](benchmarks/actions.json) contains public run identifiers,
commit hashes and job/step durations, without runner logs or private files.

## Reproduce the timing comparison

Run a full successful workflow, then repeat the same revision after its caches
have been saved with `gh run rerun RUN_ID --repo aaditagrawal/shadowroom`.
Record cache hits from the workflow logs. Export both timelines:

```sh
gh run view RUN_ID --repo aaditagrawal/shadowroom --attempt 1 \
  --json attempt,databaseId,url,headSha,status,conclusion,createdAt,jobs > first.json
gh run view RUN_ID --repo aaditagrawal/shadowroom --attempt 2 \
  --json attempt,databaseId,url,headSha,status,conclusion,createdAt,jobs > warm.json
uv run --no-project python scripts/summarize-actions.py \
  first=first.json warm=warm.json --output actions.json
```

Runner elapsed time spans first job start through final job completion and
includes dependency waits. Summed job time counts parallel jobs separately.
Both are wall-clock measurements, including network and cache transfer; neither
is CPU execution time. Hosted-runner variation prevents attributing every second
of a rerun difference to caching alone. Failed runs are excluded from speed
comparisons.
