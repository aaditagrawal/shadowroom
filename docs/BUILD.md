# Build and install

Shadowroom is an early release for Linux and macOS. Build with a current
stable Rust toolchain. The desktop uses GPUI; the remote agent also runs headless.

## Local development

After installing the native prerequisites below, use `scripts/dev.sh`. It selects
only the desktop binary for UI work and uses the same native-library environment
for checks, builds and tests:

| Work | Command | Default profile |
| --- | --- | --- |
| Desktop diagnostics | `scripts/dev.sh check` | `dev-fast` |
| Desktop executable | `scripts/dev.sh build` | `dev-fast` |
| Open a photo folder | `scripts/dev.sh run -- /path/to/photos` | `dev-fast` |
| Core and integration tests, without GPUI | `scripts/dev.sh test-core` | `dev-test` |
| Desktop unit tests | `scripts/dev.sh test-desktop` | `dev-fast` |
| Both production binaries | `scripts/dev.sh release` | `release` |

Options go before the task: `--runner cargo|mbx|blaze`, `--profile NAME`, and
`--linker system|wild|mold`. Cargo and the system linker are the defaults.
Extra Cargo arguments follow the task; test/app arguments follow `--`:

```sh
scripts/dev.sh --profile dev-perf run -- /path/to/photos
scripts/dev.sh test-core grouping -- --nocapture
scripts/dev.sh --runner mbx build --offline --timings
```

`dev-fast` keeps the app unoptimized for quick UI edits, enables incremental
compilation, omits dependency debug information and retains source-line app
backtraces. `dev-perf` adds optimization level 2 for `image`, `moxcms`,
`zune-jpeg` and `jpeg-decoder`, making image-heavy development runs more useful.
`dev-test` also optimizes Shadowroom at level 2 so fixture-heavy regressions
finish sooner; development assertions and overflow checks remain enabled.
It is the default for `test-core`, which retains native color and codecs.

Each profile populates separate `target/<profile>` artifacts. Its first build,
first check and initial incremental training can cost substantially more than
later edits. Keep `target/`, features, toolchain, native-library paths and
`LCMS2_STATIC` consistent between runs. Full variable/type inspection uses the
ordinary `dev` profile. Production release keeps ThinLTO and one codegen unit;
use it for published performance results and packages.

The wrapper sets `LCMS2_STATIC=1` unless already specified and forwards through
`scripts/check-linux-local.sh`, including Linux sysroot/pkg-config paths. Plain
Cargo remains available; no global Cargo shim or compiler flags are installed:

```sh
LCMS2_STATIC=1 cargo check --locked --profile dev-fast --bin shadowroom
LCMS2_STATIC=1 cargo run --locked --profile dev-perf --bin shadowroom -- /path/to/photos
LCMS2_STATIC=1 cargo test --locked --profile dev-test --no-default-features \
  --features native-color,native-codecs
```

Use the Linux wrapper instead of `cargo` when extracted development packages
are needed. On macOS with Command Line Tools, add `--features runtime-shaders`.
`--offline` works only after dependencies are cached. `cargo check` skips final
code generation/linking, so final verification still needs a runnable build.

### Optional MBX and linkers

MBX 1.30.0 is installed in `~/.local/bin` on the measured host. Select it per
command with `--runner mbx`; a selected runner must succeed itself. The project
[`.mbx.toml`](../.mbx.toml) enables eager incremental seeding and test scheduling.
Use eager incrementals for development profiles with LTO off; release retains
ThinLTO. Keep MBX's shared cache on the checkout's filesystem for efficient reuse;
the measured checkout and shared cache both use XFS. Cache restoration and
learning have costs, so measure edited builds as well as unchanged invocations.

On Linux x86_64, optionally install the checksum-pinned Wild 0.11.0 linker in a
user-owned tools directory, then select it for desktop build/run:

```sh
scripts/dev.sh --linker wild setup-linker
scripts/dev.sh --runner mbx --linker wild build
scripts/dev.sh --runner mbx --linker wild run -- /path/to/photos
```

Setup downloads once into `${XDG_DATA_HOME:-$HOME/.local/share}/shadowroom-build/tools`;
`SHADOWROOM_BUILD_TOOLS` overrides that location. Cargo also works with
`--linker wild`. Linker selection affects only the final desktop build/run,
preserving dependency compiler flags and caches. `mold` is available through the
same API; neither alternative applies to tests or the `release` task.

Blaze is exposed for experiments but is currently unsupported for this graph:
its checker could not load the `const-random-macro` proc macro. Unstable parallel
frontend flags were tested but are not adopted. This build has no GPU compilation
path; GPUI uses the GPU when the app runs.

### Local build measurements

November 13, 2026 experiments used an i5-10300H (4 cores/8 threads, 16 GB RAM,
NVMe), Rust 1.99.1 and cached registry inputs. These are whole-command wall times:

| Work | Before | After |
| --- | ---: | ---: |
| Desktop edit to executable, LLD → Wild | 2.245 s | 1.239 s |
| Core edit through an exact codec regression, `dev-fast` → `dev-test` | 20.251 s | 5.830 s |
| Same regression without a source edit, `dev-fast` → `dev-test` | 18.272 s | 2.721 s |
| Fresh MBX target, cache population → cache restoration | 123.797 s | 26.897 s |

Edit and regression figures use three samples; edit warmups are excluded. Other
builds caused varying contention; the optimized core measurements overlapped
unrelated compilation. MBX population/restoration each have one sample, different
contention and cache/training states, and no matched clean-Cargo baseline. These
results do not isolate individual settings or imply every workload improves by
the same amount. The [experiment data](benchmarks/build-experiments.json) records
commands, samples and limitations; the [earlier profile comparison](benchmarks/build-local.json)
retains the initial `dev`/`dev-fast` observations.

With the final configuration and cached compilation, the complete core and
integration suite passed 354 tests in a median 8.222 seconds (three runs);
32 fixture/workload tests remained ignored. Desktop tests passed 39 with three
ignored, and the Wild-linked app passed all 11 native smoke checks. These are
validation results, with no paired full-suite baseline.

Both production binaries also built successfully with the existing release
settings and dependency cache. Recompiling Shadowroom took 162.915 seconds;
unchanged release invocations took a median 0.602 seconds. This is a release
verification, with no release-compilation speedup claim.

Reproduce steady desktop edits with:

```sh
uv run --no-project python scripts/benchmark-builds.py \
  --label mbx-wild --output .local/build-bench --scenario desktop-edit \
  --warmup-edit --repeat 3 -- \
  scripts/dev.sh --runner mbx --linker wild build --offline

uv run --no-project python scripts/benchmark-builds.py \
  --label core-regression --output .local/build-bench --scenario core-edit \
  --warmup-edit --repeat 3 -- \
  scripts/dev.sh --runner mbx test-core --offline --lib \
  media::tests::scaled_jpeg_quality_preserves_late_icc_grayscale_and_all_orientations \
  -- --exact
```

The second command includes the measured codec regression after catalog edits;
compare it with `--profile dev-fast` before `test-core`. The script edits a small error string,
warms an edit, restores the original source in `finally` when it still owns the
change, and records concurrent compilers. Do not edit the probe file concurrently.
Rebuild after restoration; warmups and that restoration build are outside the
reported edit medians. Compare warmed configurations in alternating order, and
record first-profile/cache population separately.

## Linux

The GPUI desktop needs an X11 or Wayland session and compatible Vulkan drivers.
Folder and theme choosers use the XDG desktop portal's file chooser when one is
running and fall back to a built-in picker otherwise, so portals are optional. On Ubuntu 24.04, install the development prerequisites:

```sh
sudo apt-get update
sudo apt-get install build-essential clang libclang-dev pkg-config cmake \
  libfontconfig1-dev libfreetype-dev libwayland-dev libx11-xcb-dev \
  libxcb1-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libssl-dev libzstd-dev \
  libvulkan-dev libasound2-dev
LCMS2_STATIC=1 cargo build --locked --release --bins
./target/release/shadowroom /path/to/photos
```

Install your distribution's Vulkan runtime/driver. LibRaw is required for sensor
RAW rendering. HEIF on
Linux requires libheif and an HEVC decoder such as libde265. These codecs are
loaded at runtime; see [format support](MEDIA.md).

OS Trash on Linux implements FreeDesktop Trash directly, including per-volume
restore metadata, without loading GLib/GIO. It requires a safe writable Trash
directory and atomic no-replace rename support from the kernel/filesystem; an
unsupported operation reports an error and never falls back to permanent deletion.
Optional drive ejection still invokes `gio mount -e` and needs a desktop volume
manager. See [portability work](PORTABILITY.md) for lean builds and prototypes.

After building, run the core checks in [Contributing](../CONTRIBUTING.md). On an
X11/Xwayland desktop, check the feedback workflows against the native app with
isolated settings and photo fixtures:

```sh
uv run --no-project python scripts/native-feedback.py --binary target/release/shadowroom
```

This also needs xdotool, ImageMagick and Tesseract; the contributing guide
describes repeatable XTEST setup.

For development without root, `scripts/check-linux-local.sh` accepts a
`SHADOWROOM_SYSROOT` containing extracted development packages under `usr/`.
It adds include, pkg-config, and linker paths without installing system packages.
The default is `~/.local/share/shadowroom-build`. The
[vendored xattr patch](../vendor/xattr/SHADOWROOM-PATCH.md) fixes a compatibility
issue in GPUI's dependency tree and is required by this source release.

## macOS

The default build needs Xcode with its macOS SDK and Metal compiler:

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  LCMS2_STATIC=1 cargo build --locked --release --bins
./target/release/shadowroom /path/to/photos
```

With Command Line Tools and its SDK, use runtime shader compilation instead:

```sh
LCMS2_STATIC=1 cargo build --locked --release --features runtime-shaders --bins
```

This feature compiles GPUI's embedded shaders through Metal at startup, which can
add initial latency. HEIF uses system ImageIO/ColorSync. Sensor RAW rendering
still needs a compatible LibRaw installation.

## Native packages

Packaging requires [uv](https://docs.astral.sh/uv/) for its Python helpers.

```sh
# Linux directory package, tar.gz, desktop entry, icon, and user-local installer.
scripts/package-linux.sh

# Optional prepared codecs, with sources, licenses, and provenance.
scripts/package-linux.sh --heif-runtime /path/to/prepared/runtime

# macOS app bundle and zip, built on the native architecture.
scripts/package-macos.sh
# Command Line Tools alternative:
scripts/package-macos.sh --runtime-shaders
```

Artifacts go to `dist/`. Existing artifact names are refused; select a fresh
`--output DIR` for another build. `--no-build` packages existing binaries;
`--debug` selects the debug profile. Both scripts respect `CARGO_TARGET_DIR`.

On Linux, extract the tarball and run `./install.sh` for `~/.local`, or
`./install.sh /chosen/prefix`. The generated installer needs Python 3. The package
records its shared-library dependencies and requires compatible system runtimes.
Optional codec paths are scoped to the application launcher.

To assemble an AppImage from an existing Linux package, supply an independently
verified AppImage runtime and install `mksquashfs`. The public release inputs can
be fetched with their committed checksums:

```sh
uv run --no-project python scripts/fetch-release-inputs.py --kind appimage --output .local/release-inputs
scripts/package-appimage.sh --package dist/shadowroom-0.1.1-linux-x86_64 \
  --runtime .local/release-inputs/runtime-x86_64 \
  --runtime-sha256 2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d \
  --runtime-metadata .local/release-inputs --output dist/Shadowroom-0.1.1-x86_64.AppImage
```

The [tag workflow](../.github/workflows/ci.yml) builds Linux on AlmaLinux 8 for a
glibc 2.28 baseline, rebuilding the bundled RAW/HEIF codecs from verified public
sources. To reproduce that path with Docker or Podman:

```sh
docker build -f packaging/linux/Containerfile -t shadowroom-release-el8:ci packaging/linux
uv run --no-project python scripts/fetch-release-inputs.py --output .local/release-inputs
scripts/ci-linux-build.sh
CARGO_TARGET_DIR="$PWD/.local/ci-linux/target" scripts/package-linux.sh \
  --no-build --heif-runtime .local/ci-linux/codecs --glibc-baseline 2.28
```

For Podman set `CONTAINER_ENGINE=podman` and build/tag the same image. The build
script mounts the selected `rustc --print sysroot`, host Cargo registry, and the
same persistent EL8 target directory used by CI. Use Rust 1.99.1 to match CI.
Ordinary native builds on newer distributions retain their own system-library
requirements; placing such a binary in an AppImage does not lower its glibc floor.
See [Releasing](RELEASING.md) for artifact names, notices, cache layout and tag publication.

On macOS, move `Shadowroom.app` from the zip into Applications. The bundle declares
macOS 12+ and contains the native build architecture. Packaging signs ad hoc by
default. `SHADOWROOM_CODESIGN_IDENTITY` selects an installed signing identity;
Apple notarization is a separate release step. Windows packages are not provided.
Both desktop and remote agent are included in the app bundle. macOS HEIF uses
system frameworks; sensor RAW fallback still requires a compatible LibRaw
installation. Camera JPEG/embedded-preview inspection works without LibRaw.
