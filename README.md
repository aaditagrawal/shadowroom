# Shadowroom

**Find the photographs worth keeping. Make every decision deliberate.**

A native photo culling workspace for Linux and macOS, built in Rust with
[GPUI](https://www.gpui.rs/). Review a shoot, compare frames, flag picks, and carry
your decisions into the next editor.

![Shadowroom reviewing a lakeside photograph, with histogram, five-star rating, pick flag, and filmstrip](docs/images/review.png)

**Early release · 0.1.2 · Apache-2.0**

[Get started](#get-started) · [Performance](#performance) · [Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md)

## A workspace for the first pass

- **Review at your pace.** Grid, loupe, pinned comparison, and Survey views;
  keyboard decisions, optional automatic advance, stars, and color labels.
- **Keep a shoot together.** Pair RAW+JPEG/HEIF companions and group nearby frames
  by time or visual similarity. Burst grouping refines as previews arrive.
- **Import with a plan.** Preview names and folders, copy to a library and backup
  destinations, verify copies, and resume interrupted work.
- **Delete deliberately.** Confirm the photo/file counts and source locations
  before moving files to OS Trash. Optional in-app Trash leaves files in place;
  exact-duplicate scans return matches for review.
- **Take your decisions onward.** Copy picks with their companions or export XMP
  sidecars to a separate destination. Existing destination files are preserved.
- **Make it yours.** Resizable sidebars, three bundled fonts, light/dark themes,
  and Zed theme imports. Explore preferences, then Apply or Cancel your changes.
  Control the desktop through MCP or review remote sources with the SSH agent.

<table>
  <tr>
    <td><img src="docs/images/compare.png" alt="Pinned comparison of two mountain photographs" width="680"></td>
    <td><img src="docs/images/catalog.png" alt="Photo catalog with picks, ratings, and labels" width="680"></td>
  </tr>
  <tr>
    <td><strong>Compare</strong> — keep a reference beside the next candidate.</td>
    <td><strong>Catalog</strong> — see the shoot and its decisions together.</td>
  </tr>
</table>

<details>
<summary>See the light theme</summary>

![The same native review workspace in Shadowroom Light](docs/images/light.png)

</details>

## Get started

[Download v0.1.2](https://github.com/aaditagrawal/shadowroom/releases/tag/v0.1.2)
for the Linux x86_64 AppImage/archive, macOS arm64 archive, Fedora 44 RPM, and source release.
The portable Linux AppImage and archive target glibc 2.28 or newer and include
the documented RAW/HEIF codecs; a desktop session and Vulkan driver remain
required. The macOS build is signed ad hoc and is not notarized. Each asset
includes a SHA-256 checksum.

Fedora 44 x86_64 packages also use the
[Shadowroom COPR repository](https://copr.fedorainfracloud.org/coprs/aaditagrawal/shadowroom/).
After its release build succeeds, enable it with `sudo dnf copr enable
aaditagrawal/shadowroom`, then `sudo dnf install shadowroom`; subsequent versions
arrive through DNF updates. See [Fedora packaging and publication](docs/FEDORA.md)
for current build availability and the system-codec differences.

Install a current stable Rust toolchain and the
[native prerequisites for your platform](docs/BUILD.md), then build both binaries:

```sh
LCMS2_STATIC=1 cargo build --locked --release --bins
./target/release/shadowroom /path/to/photos
```

On macOS, the default build needs Xcode's Metal tools. Command Line Tools users
can add `--features runtime-shaders`. Linux needs a desktop session and Vulkan
drivers. Native package and AppImage instructions are in the [build guide](docs/BUILD.md).

Open a folder with **Cmd/Ctrl+O** or **Add photos** to plan a copy import. Passing a
folder on the command line indexes it in place. Pick or rate frames as you review,
then use **Export** to copy picks. Export XMP sidecars and other commands are in
the command palette (**Cmd/Ctrl+K**).

| Key | Action |
| --- | --- |
| `←` / `→` | Previous / next photograph |
| `P` / `X` / `U` | Pick / reject / clear flag |
| `0`–`5` | Star rating; zero clears |
| `R` | Reset stars and pick/reject without advancing; keep color label |
| `6` / `7` / `8` / `9` | Red / yellow / green / blue label |
| `G` / `L` / `C` | Grid / loupe / compare |
| Enter or `L` in Catalog | Review the folder; two or more selected photos limit Review to that selection |
| `B` | Pin comparison reference |
| Space or `Z` | Fit / 100% |
| `+` / `−` | Increase / decrease zoom |
| `F` | Toggle focus peaking |
| `F1` | Getting Started guide |
| Shift+`↓` / Shift+`↑` | Extend / shrink selection by a grid row |
| `[` / `]` | Previous / next group |
| Cmd/Ctrl+Z | Undo decision |
| Delete / Backspace | Confirm moving selected photos to the configured Trash |
| Shift+Delete | Confirm permanent deletion, bypassing Trash |

See [keyboard and Creator Micro controls](docs/CONTROLS.md) for the complete
controller mapping and calibration guide. Physical controller validation remains
a hardware test.

## Performance

These application measurements describe the 0.1.0 release. Build and cache tuning
for 0.1.1 is documented separately in [CI performance](docs/CI-PERFORMANCE.md).

Measured on an **Intel Core i5-10300H, Fedora Linux 45, native X11/Xwayland**, using
the optimized release build. Three independent runs each indexed **10,000
synthetic photo paths** into a fresh catalog.

<!-- measured-results:start -->
| Measurement | Result |
| --- | ---: |
| Index 10,000 synthetic paths, median | 0.278 s |
| Photo selection, median / p95 | 0.58 / 6.41 ms |
| Star-rating acceptance, median / p95 | 0.22 / 1.97 ms |
| Peak process RSS across all three runs | 204 MiB |
<!-- measured-results:end -->

The fixture uses 16 hardlinked 128×80 BMPs with warm OS page caches. Command
timings include MCP transport; a rating response accepts the edit before its
durable save. These measurements cover catalog/command behavior, not RAW decode,
camera-card throughput, or frame presentation. [Methodology, raw samples, and reproduction commands](docs/PERFORMANCE.md) are included.

## Formats and limits

| Media | Display path |
| --- | --- |
| JPEG, PNG, WebP, BMP | Native preview pipeline |
| TIFF / BigTIFF | First page; supported integer/float samples and ICC conversion |
| JPEG XL | Integer SDR, including 8/16-bit input and straight alpha; first animation frame |
| HEIC / HEIF / HIF | ImageIO on macOS; libheif with a compatible decoder on Linux |
| Camera RAW | Paired JPEG/HEIF or embedded JPEG for Fit and zoom; LibRaw fallback when no preview exists |

Zoom inspection prefers the camera JPEG over RAW sensor development.
Nearby frames are read ahead for comparison, including while zoomed.
Automatic preview memory uses a quarter of physical RAM, bounded to 512 MiB–4 GiB;
manual limits remain available in Preferences. The budget covers admitted media
work, not total process RSS. RAW fallback support depends on LibRaw.

Display output is **8-bit sRGB**. HDR/wide-gamut output, soft proofing, lens
corrections, video playback, and AVIF are outside this release. See
[media and color behavior](docs/MEDIA.md) for exact format restrictions.

## Your catalog, your files

Catalog, settings, and caches use the operating system's local application-data
directories. A disconnected drive does not remove its catalog entries; cached
previews can remain available offline. The footer distinguishes pending, failed,
and saved changes. Normal quit waits for saves and accepted exports; failed writes
remain visible for retry. See [recovery and durability](docs/RECOVERY.md).

Ratings, flags, and labels do not edit source photographs. Decisions remain in
the local catalog until you explicitly export XMP to a chosen destination; there
is no automatic synchronization with another editor. Back up the catalog to
preserve staged decisions independently of your image backups.

**Delete** defaults to the operating system's Trash. The confirmation freezes the
selected photo groups and shows photo/file counts, source folders, and the
destination. RAWs, paired companions, and recognized sidecars are included;
restore them through the OS file manager. **Preferences → Culling → Trash
destination → In-app** keeps files in place and supports Restore and decision
Undo instead. **Shift+Delete** requests permanent deletion with its own explicit
confirmation. Cancel is selected by default; Enter cancels, and Tab then Enter
confirms. Unavailable Trash never silently falls back to permanent deletion.
Physical deletion is local-only; partial failures remain visible. See
[Trash and duplicates](docs/DUPLICATES_TRASH.md).

The startup **Getting Started** guide explains the workflow, shortcuts, color
labels, time groups and caching. Reopen it with **F1** or **?**; Preferences → Help
controls whether it appears at startup. In Catalog, double-click a photo to review
the full folder at that image. **Enter/L** also opens the full folder for zero or
one selected photo, using the hovered photo when available; select multiple
photos first to review only that set. **Show full folder** leaves a subset, and
**G** returns to Catalog. Right-click a photo for Show in folder, Show in review,
Survey burst, Compare, decisions, and deletion actions. Preferences **Apply**
commits and stays open; **Apply & Close** also closes the pane. Cancel/Escape
discards only changes made since the last Apply.

## Documentation and development

[Import](docs/INGEST.md) · [Export](docs/EXPORT.md) · [Themes](docs/THEMES.md) ·
[Remote access](docs/NETWORK.md) · [MCP](docs/MCP.md) · [All guides](docs/README.md)

```sh
cargo test --locked --no-default-features
cargo test --locked --bin shadowroom
cargo fmt --all -- --check
```

[CI](.github/workflows/ci.yml) checks core/integration behavior and builds native
Linux/macOS packages. [Contributing](CONTRIBUTING.md) covers local checks;
[releasing](docs/RELEASING.md) covers packaging and platform validation. Windows
packages are not provided. macOS distribution signing/notarization is separate
from the default ad-hoc package signature.

## License and credits

Shadowroom is licensed under [Apache-2.0](LICENSE). Bundled fonts, Codicons, and
dependencies retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).
Screenshots show the native application with demo photographs from Unsplash.
[Image credits and provenance](docs/images/README.md).
