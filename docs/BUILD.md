# Build and install

Shadowroom is an early release for Linux and macOS. Build with a current
stable Rust toolchain. The desktop uses GPUI; the remote agent also runs headless.

## Linux

An X11 or Wayland session, Vulkan drivers, and a working desktop portal are needed
to open the app. On Ubuntu 24.04, install the development prerequisites:

```sh
sudo apt-get update
sudo apt-get install build-essential clang libclang-dev pkg-config cmake \
  libfontconfig1-dev libfreetype-dev libwayland-dev libx11-xcb-dev \
  libxcb1-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libssl-dev libzstd-dev \
  libvulkan-dev libasound2-dev libglib2.0-0t64
LCMS2_STATIC=1 cargo build --locked --release --bins
./target/release/shadowroom /path/to/photos
```

Install your distribution's Vulkan runtime/driver and an `xdg-desktop-portal`
backend for folder dialogs. LibRaw is required for sensor RAW rendering. HEIF on
Linux requires libheif and an HEVC decoder such as libde265. These codecs are
loaded at runtime; see [format support](MEDIA.md).

OS Trash on Linux uses GIO (`libgio-2.0.so.0`, provided by GLib). Install your
distribution's GLib runtime; Fedora's RPM requires `glib2`. If the library or a
source volume's Trash is unavailable, deletion reports an error and never falls
back to permanent deletion. AppImages use the host's GIO service.

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
  --no-build --heif-runtime .local/ci-linux/codecs
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
