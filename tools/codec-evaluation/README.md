# Rust HEIF evaluation

This isolated tool evaluates the pinned `heifer 0.2.0` decoder without linking
Shadowroom or loading libheif. It writes a full precision RGBA16 PNG and prints
JSON with container features, dimensions, source depth, alpha, and elapsed decode
time. An optional reference PNG produces full-image RGB and alpha error metrics.

```sh
cargo run --manifest-path tools/codec-evaluation/Cargo.toml --locked --release -- \
  input.heic /tmp/decoded.png /tmp/reference.png
cargo test --manifest-path tools/codec-evaluation/Cargo.toml --locked
```

The reference should represent the same source RGB encoding. This tool deliberately
does not apply ICC or NCLX color management, while Shadowroom's integrated backend
does. JSON labels this distinction. Use real native references for meaningful
comparison; comparing a saved image against itself does not establish correctness.
Input is capped at 512 MiB and decoding at 160 megapixels, using one thread.
These are evaluation limits, not an allocator sandbox. This tool can exercise
features that the conservative app adapter intentionally sends to native fallback.

The app backend is opt-in on Linux:

```sh
cargo build --features rust-heif
cargo test --no-default-features --features rust-heif --lib heif::
SHADOWROOM_HEIF_FIXTURE=/path/to/ordinary.heic \
  cargo test --no-default-features --features rust-heif,native-codecs --lib \
  rust_fixture_matches_native -- --ignored --nocapture
SHADOWROOM_HEIF_FALLBACK_FIXTURE=/path/to/alpha.heic \
  cargo test --no-default-features --features rust-heif,native-codecs --lib \
  unsupported_fixture_retains_native -- --ignored --nocapture
```

`native-codecs` is enabled by default and controls loading LibRaw and Linux
libheif. To omit dynamic codec loading entirely, build with
`--no-default-features --features rust-color,rust-heif`. Supported HEVC images
use Rust; unsupported HEIF features and full sensor RAW rendering report that
the codec is unavailable in this build. Embedded camera previews remain supported.
On macOS, ImageIO remains an operating-system backend independently of this flag.
To verify Rust decoding without a native fallback:

```sh
SHADOWROOM_HEIF_FIXTURE=/path/to/ordinary.heic \
  cargo test --no-default-features --features rust-color,rust-heif --lib \
  rust_fixture_decodes_without_loading_native -- --ignored --nocapture
```

The two fixture tests compare against libheif, which must have its HEVC decoder
available. For our existing locally built codec bundle, set `LD_LIBRARY_PATH` to
its `lib` directory. Fixtures remain outside the repository. The feature does not
remove native codecs from packages: alpha, non-HEVC data, overlays, unknown
properties and other unsupported coding tools retain the native path. Header,
HDR and memory validation happen before selecting either backend.

Sources: [heifer](https://github.com/HarpeLm/Heifer),
[heic-rs](https://github.com/tbraun96/heic-rs),
[Rawler](https://github.com/dnglab/dnglab/tree/main/rawler).
`heic-rs 0.1.4` was inspected but not selected: its output/container pixel limit is
not passed through `decode_coded` into the HEVC reconstruction allocator. Heifer
checks bitstream dimensions against `Options::max_pixels` before reconstruction.
Rawler 0.8.0 includes demosaicing, camera white balance, calibration and sRGB
development, but a production replacement additionally needs cancellation,
admission validation, orientation and camera/color parity work. Neither alternative
changes Shadowroom's default RAW decoder in this change.
