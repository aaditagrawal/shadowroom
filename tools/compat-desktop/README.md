# CPU/X11 compatibility prototype

A separate, read-only frontend that reuses Shadowroom's catalog, indexer, and
bounded asynchronous media pipeline. It draws pixels on the CPU and sends them
to an X11 server through `x11rb::RustConnection`. It does not create an OpenGL or
Vulkan context, load libxcb, or use GPUI.

```sh
cargo run --manifest-path tools/compat-desktop/Cargo.toml --locked -- /path/to/photos
cargo run --manifest-path tools/compat-desktop/Cargo.toml --locked -- /path/to/photos --mode loupe
cargo run --manifest-path tools/compat-desktop/Cargo.toml --locked -- /path/to/photos --offscreen /tmp/grid.png
```

Run with `--help` for controls and headless options. Originals are read-only.
The default catalog is disposable and separate from the main application; an
explicit `--state-dir` must be outside the photo folder. Image decoding uses two
workers and a 256 MiB memory budget. Disk preview persistence is disabled.

`--offscreen` runs without `DISPLAY` and uses the same renderer as the window.
`--exit-after 3` closes a window automatically for private Xvfb smoke tests.
Output files must be new and outside the photo folder.

```sh
cargo test --manifest-path tools/compat-desktop/Cargo.toml --locked
```

This is a feasibility prototype, not a port of the desktop application. It has
grid/loupe selection, fit and actual-pixel rendering, keyboard panning, and basic
bitmap labels. There are no editing, ingestion, deletion, or export actions.
It assumes an sRGB monitor, uses nearest-neighbor image sampling, and requires
an X11 server with a 16/24/32-bit TrueColor visual. Wayland requires Xwayland.
Keyboard shortcuts use the server's first keysym; full keyboard layout, input
method, clipboard, and accessibility integration are not implemented.

The root dependency enables Rust color and Rust HEIF decoding and disables its
default desktop and native-codec features. Bundled SQLite still uses compiled C.
A GNU build still depends on its build environment's glibc. Build the static
musl variant with `scripts/build-compat-musl.sh` after installing the
`x86_64-unknown-linux-musl` Rust target. The helper uses pinned Zig through uvx
to compile bundled C and Rust's self-contained linker. The resulting executable
has no host glibc or shared-library requirements. It has been run in AlmaLinux 8
and Debian 11 userspaces and on X11; those containers share the host kernel and
do not establish an older-kernel compatibility baseline.
