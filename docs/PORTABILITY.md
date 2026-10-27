# Linux portability work

This describes the source checkout, not a new compatibility promise for published
release artifacts. The full GPUI desktop retains its glibc 2.28 release target,
Vulkan requirements, native color management and bundled codecs. A separate
CPU/X11 frontend is a read-only feasibility prototype.

## Included features and optional native code

The catalog, ingest, decisions, grouping, exports and remote protocol are Rust.
JPEG/PNG/WebP/BMP, dedicated TIFF and JPEG XL decoding are also Rust. SQLite is
compiled into the executable; rewriting it would not remove a runtime package.

Linux folder, ingest-destination, export and theme pickers run inside the app,
with background directory reads and bounded listings. Ctrl+L edits the path, Tab
switches to the entry list, Enter opens an entry, Ctrl+Enter confirms, and Escape cancels. macOS keeps
native pickers. The picker never creates destination folders or changes photos.

Linux OS Trash writes FreeDesktop Trash storage directly without loading GIO.
It reserves restore metadata before an atomic no-replace rename, keeps per-volume
relative paths, handles non-UTF8 names and collisions, and refuses symlinked or
insecure Trash storage. Existing home/data-home aliases are supported. Unsupported
atomic rename operations fail; there is no copy/unlink or permanent-delete
fallback. Optional Linux drive ejection still uses `gio mount -e`.

Ingest invokes the kernel's renameat2 operation through the old syscall entry
point, so it no longer imports glibc's 2.28 renameat2 wrapper. Its existing
no-replace staging fallback remains. Other imports and libraries can still set
the executable's actual minimum glibc version.

## Build features

The default features are `desktop`, `native-color` and `native-codecs`.

- `native-color` keeps LittleCMS for established ICC output. Portable releases
  statically link it; Fedora packages use the system library.
- `rust-color` selects the experimental moxcms paths with defaults disabled.
  If `native-color` is also enabled, LittleCMS wins.
- `native-codecs` enables LibRaw and Linux libheif loading. Disable it to remove
  libloading from the shipped graph. macOS ImageIO is an OS backend independently.
- `rust-heif` enables a conservative Linux HEVC still-image decoder using heifer.
  Unsupported properties/alpha/non-HEVC inputs use the native fallback when
  enabled, or report that the build lacks the necessary decoder.

```sh
# Rust-only normal/build dependency graph; reference tests still use LittleCMS.
cargo tree --locked --edges normal,build --no-default-features --features rust-color,rust-heif

# Headless agent without GPUI, LittleCMS or runtime-loaded codec libraries.
cargo build --locked --no-default-features --features rust-color,rust-heif --bin shadowroom-remote

# Experimental Rust color/HEIC in GPUI, retaining native codec fallbacks.
cargo run --locked --no-default-features --features desktop,rust-color,rust-heif,native-codecs --bin shadowroom
```

RGB8 engine comparisons allow at most one final display code value. Native
precision, grayscale ramps, floating-point conversion and unchanged alpha have
dedicated regression coverage. CMYK CLUT interpolation is **not equivalent**:
one existing fixture produces LCMS `[225,194,152]` versus Rust `[225,177,138]`.
Production keeps LittleCMS until this gap is resolved. Changing engines must
not reuse previews from the previous rendering configuration. Local preview
filenames include the rendering namespace within one shared version-10 directory,
so app/backend changes share the existing disk quota instead of accumulating a
separate unpruned cache for each configuration. Remote caches advertise and
persist the server namespace; see [remote cache identity](NETWORK.md).

The Rust HEVC route has real 8-bit fixture comparisons; higher-depth HEVC still
needs real fixtures. Alpha and uncompressed HEIF retain native coverage. LibRaw
remains for full sensor rendering: a Rawler migration still needs bounded
admission, cancellation, camera/color/orientation parity and error-path work.
RAW files with usable companion or embedded JPEG previews remain viewable in
lean builds. No sensor-rendering replacement has been claimed.

## Software frontend

[tools/compat-desktop](../tools/compat-desktop/README.md) shares the catalog,
indexer and bounded asynchronous media pipeline, but sends CPU-rendered pixels
through x11rb's Rust connection. It uses no GPUI, Vulkan or libxcb.

```sh
cargo run --locked --manifest-path tools/compat-desktop/Cargo.toml -- /path/to/photos
cargo run --locked --manifest-path tools/compat-desktop/Cargo.toml -- /path/to/photos --offscreen /tmp/grid.png
rustup target add x86_64-unknown-linux-musl
scripts/build-compat-musl.sh
```

The musl helper uses pinned Zig 0.15.2 through uvx for bundled C compilation and
Rust's self-contained CRT/linker. It does not install or replace system glibc.
An executable with no ELF interpreter or shared-library requirements does not
need the host glibc. It still needs compatible kernel operations and an X11
server (or Xwayland). Static linking is not proof of an older kernel baseline.

The prototype supports grid/loupe, selection, fit/100%, keyboard pan and isolated
state. It does not implement full culling, editing, ingest, deletion or exports.
Keyboard layouts, input methods, accessibility, clipboard, text rendering and
resampling need further frontend work before it can replace GPUI.

## Enforced artifact baseline

```sh
uv run --no-project python scripts/check-linux-abi.py /path/to/package --max-glibc 2.28 --report /tmp/linux-abi.json
```

The inspector checks ELF version-needs records in every packaged executable and
library, forces a stable readelf locale, rejects private/unrecognized glibc ABI
references and escaping symlinks, and records external shared libraries. It can
also inspect a single executable. `--require-bundled SONAME` asserts that a known
runtime-loaded codec is present. Tests distinguish mandatory version needs from
weak imports and exported definitions.

Release CI passes `--glibc-baseline 2.28` to the Linux packager, which writes
`share/doc/shadowroom/linux-abi.json` and refuses to archive incompatible output.
This supplements actual launch tests; it does not inspect unbundled libraries
loaded later by name or establish kernel/CPU/driver compatibility. Full older
desktop support requires older-distribution and kernel VM/hardware coverage.

## Verified in this checkout

The 2026-10-27 validation built the full desktop in AlmaLinux 8 and inspected
all five packaged ELF objects against glibc 2.28. A portable tarball and actual
AppImage were assembled. Native X11 acceptance checks passed for the built-in
picker, decisions and persistence, restart and close barriers, preview/export
workflows, confirmed OS Trash, and AppImage launch without FUSE.

The x86_64 musl CPU/X11 prototype is about 9.9 MiB. ELF inspection found no
interpreter, shared-library requirements or glibc version requirements. The
same binary produced grids in AlmaLinux 8 and Debian 11 containers and opened
grid/loupe windows on a private X11 server. Original bytes and timestamps were
checked by the native acceptance scripts. Containers shared the host kernel;
older kernels, physical GPUs and a complete older desktop matrix remain untested.

Core tests passed with lean, native-codec/color, and experimental Rust-codec/color
configurations, along with desktop, prototype, ABI inspector and documentation
checks. The main app keeps its established color/codec defaults; these checks do
not resolve the experimental CMYK and higher-depth HEVC gaps described above.
