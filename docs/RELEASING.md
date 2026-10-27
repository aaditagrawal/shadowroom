# Release process

Shadowroom uses the version in `Cargo.toml` for native package names. The
[Native checks workflow](../.github/workflows/ci.yml) checks main-branch pushes and
pull requests, retains native artifacts, and publishes a preview GitHub Release after
a matching `v<VERSION>` tag passes every required job. Ordinary branch pushes and
pull requests never publish releases.

## Validate the source

Run the checks in [CONTRIBUTING.md](../CONTRIBUTING.md), then the applicable native
smoke scripts. Test both the desktop and `shadowroom-remote` from the final package.
Verify import/copy and decision recovery against disposable sources. Check that
original SHA-256 hashes and modification times remain unchanged.

Inspect native formats with known reference fixtures and required codecs. A passing
core suite does not establish native codec fidelity, Wayland behavior, hardware
controller input, or macOS notarization. Record the systems actually tested.

## Prepare artifacts

1. Build packages using [BUILD.md](BUILD.md), retaining dependency notices and any
   optional codec sources/licenses/provenance.
2. Run the packaged binary's `--help` and the native smoke harness with `--binary`
   pointing at its launcher. For an AppImage, also run `native-appimage-smoke.py`.
3. Regenerate [performance data](PERFORMANCE.md) from the final release binary and
   capture current screenshots using `scripts/capture-screenshots.py` with demo
   photos cleared for redistribution. Update [image credits](images/README.md).
4. Generate SHA-256 checksums for the archives. Inspect archive contents for local
   profiles, machine addresses, private photographs, build logs, and experiments.
   `uv run --no-project python scripts/package-source.py` creates a source tarball
   and checksum from tracked files under an explicit public-file list, excluding
   untracked screenshots and build/local artifacts.
5. On macOS, apply the intended distribution identity and notarization process
   before describing an artifact as signed/notarized for distribution.

## Automated artifacts and build inputs

The workflow produces:

- Linux x86_64 directory archive and AppImage, compiled in the public, digest-pinned
  [AlmaLinux 8 builder](../packaging/linux/Containerfile), with a glibc 2.28 baseline.
  Native desktop and agent checks run inside that environment. An Xvfb/software
  Vulkan smoke check opens the actual AppImage without FUSE on the Ubuntu runner.
- macOS arm64 `.app` archive containing both the desktop and remote agent.
  `runtime-shaders` avoids requiring the hosted runner's offline Metal compiler.
  The bundle is signed ad hoc and verified, with no notarization claim. CI package
  checks are not interactive macOS GUI or hardware validation.
- Fedora 44 x86_64 RPM plus its self-contained, vendored source RPM. Compilation
  runs without container networking and DNF installs the resulting RPM in a
  disposable Fedora environment. This package uses system codecs and shared
  libraries, separate from the portable Linux payload. After GitHub publication,
  the separate [Fedora COPR workflow](../.github/workflows/copr.yml) submits that
  verified source RPM to the configured repository; see [Fedora distribution](FEDORA.md).
- Public source archive, individual `.sha256` files, and a combined `SHA256SUMS`.

Rust 1.99.1, uv 0.13.5 and action commit IDs are pinned in the workflow. Public
codec archives and the dated AppImage runtime are SHA-256 pinned in
[release-inputs.json](../packaging/release-inputs.json). The fetch helper verifies
existing cached inputs as well as fresh downloads. The Linux runtime includes
LibRaw 0.22.2, libheif 1.23.5 and libde265 1.0.18, their original source archives,
licenses, exact build script, container definition, installed RPM inventory and
payload checksums. It disables external HEIF plugins/encoders and optional RAW SDKs.
JPEG-lossy DNG support is excluded. macOS HEIF remains system-native; sensor RAW
fallback requires a compatible separately installed LibRaw.

The AppImage carries the pinned runtime's notices and source/build instructions.
Its runtime is deliberately a dated release rather than mutable `continuous`.
`SOURCE_DATE_EPOCH` fixes the squashfs timestamps in CI. These are reproducible
build instructions and versioned inputs, not a claim that different OS package
mirrors or toolchains always produce byte-identical native executables.

Core caches separate Ubuntu and macOS targets; release caches separate EL8 and
macOS targets. The EL8 target directory restored on the host is the same directory
mounted into the container, and the host Cargo registry is shared with the build.
Dependency caches are keyed by toolchain/lock/configuration rather than every app
source edit. Container layers and the independently keyed codec payload also have
caches. Incremental compilation and dev/test debug info are disabled to reduce
cache size; test assertions and coverage remain enabled. Measure completed job and
step timings before attributing a speed change to a cache or build setting.
Desktop unit tests use the same optimized release profile as the native binaries,
so their GPUI dependencies share build outputs. Core tests retain the default
debug profile; test assertions remain active in both profiles.

## Publish a version

Commit only the source tree, documentation, benchmark JSON, and documentation
images. Keep `target/`, `.local/`, `dist/`, catalogs, and codec build work out of Git.
Retain `Cargo.lock`, `vendor/xattr`, and all attribution files.

Update `Cargo.toml` and `Cargo.lock`, and write `docs/releases/v<VERSION>.md` with
the actual platform support, changes and known limitations. Push the reviewed
source and check the branch run before tagging that revision. The tag must exactly
match `v` plus the Cargo package version; mismatches fail before packaging.

Only the publish job receives `contents: write`. GitHub's per-job `GITHUB_TOKEN`
supplies release access; no personal publishing token is required. After all
platform jobs pass, it downloads their artifacts, verifies their checksums,
creates a draft preview release, uploads the archives, then publishes it. A failed
upload leaves a draft which a rerun can resume. Already published release assets
are refused rather than silently replaced; use a new version for changed bytes.
Existing historical tags and releases remain unchanged.

## Fedora COPR publication

Native checks calls the reusable Fedora COPR workflow only after its tag publish
job succeeds and `COPR_PROJECT` names `aaditagrawal/shadowroom`. This explicit call
is necessary because releases created with `GITHUB_TOKEN` do not trigger a second
release-event workflow. Owner-published release events and default-branch manual
runs also support an already-published tag.
[GitHub event behavior](https://docs.github.com/en/actions/concepts/security/github_token#when-github_token-triggers-workflow-runs).

COPR receives only its specific API secret and read-only GitHub permissions.
Its publisher runs from the default branch, verifies the exact release tag/SHA,
required native job results, asset checksums, and SRPM source/lock provenance,
then uploads the verified local SRPM for Fedora 44 x86_64 without build networking.
The reusable call validates successful predecessor jobs instead of requiring its
own still-running workflow to have a final conclusion. External/manual runs
require a completed Native checks run whose seven source/package/publication
jobs succeeded; a previous COPR-monitor failure does not block resuming it.

Submissions are serialized. The publisher reuses a running/successful build for
the same EVR and checks a reused source package against the release. It never
automatically retries a POST or creates a second failed-EVR build. A 300-minute
monitor timeout leaves the server build running; manually rerun Fedora COPR with
the published tag to resume inspection. Inspect uncertain submission responses
before retrying. Sanitized receipts include source/asset hashes, build ID, and
state and are uploaded even when monitoring fails. See [Fedora](FEDORA.md) for
credentials, package availability, DNF commands, and codec distinctions.
