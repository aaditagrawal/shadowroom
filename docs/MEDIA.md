# Media pipeline

`MediaPipeline` keeps source reads, EXIF parsing, ICC conversion, decoding, resampling,
histograms, and persistent-cache work off the GPUI thread. Its `request`, `get`,
`error`, and `stats` methods only access memory with `try_lock`; a contended call
returns immediately. Repeat requests while a result is absent. A request may be
deferred on contention or a full queue, and existing work is deduplicated.

```rust
let media = MediaPipeline::new(cache_dir, 512 * 1024 * 1024);
media.set_disk_budget(8 * 1024 * 1024 * 1024);
media.request_photo(&photo, PreviewKind::Fit);
let shown = media.best_photo(&photo);
// On an explicit 100% zoom:
media.request_photo(&photo, PreviewKind::Full);
```

Use the `*_with_size` methods with catalog metadata. Their cache identity includes
the native path bytes, catalog modification time, byte size, preview kind, and
pipeline version. Version 9 uses paired/embedded camera previews for full-size
inspection and retains the associated-color alpha resampling from version 8. It
persists `rendered_raw`, `embedded_raw`, and `rendering_note` provenance, including
on derived previews. Older entries are regenerated. No path canonicalization or
filesystem stat occurs on the UI thread. Paths supplied by the catalog should be absolute. The smaller contract
methods `request(path, modified, kind)` and `get(path, modified, kind)` are also
available, with size zero denoting unspecified size; applications needing exact
size invalidation should supply it. Do not mix the two identity forms for a photo.

Triangle resampling of nonopaque RGBA and gray-alpha images filters color
multiplied by alpha, then divides by the filtered alpha once. Fully transparent
resized pixels have zero color. This prevents hidden color in transparent pixels
from contaminating visible boundaries. Eight-bit opaque RGBA retains the previous
integer resizer. Native-depth gray-alpha/RGBA TIFF and JPEG XL paths share the same
alpha rule. Filtering remains in the existing encoded color space; this change
does not introduce linear-light resampling or HDR output.

Two workers consume a bounded queue of 160 requests. Fit views have first priority,
full views second, and thumbnails third. New foreground work can displace an older
thumbnail when the queue fills; an equal-priority request replaces the oldest queued
request. Within a priority, the newest request wins. The
384-pixel thumbnail, 2560-pixel fit view, and original-resolution full view form the
progressive levels. A fit decode also creates a thumbnail; a full decode also
creates fit and thumbnail views. A retained Full also satisfies subsequent Fit or
Thumbnail requests, avoiding repeated decoding when lower levels were evicted.
The returned `Preview.kind` describes its actual tier. A newly published requested
preview is protected until its first consumer or 300 ms, so another worker cannot
evict it before the UI sees it; cancellation removes that temporary protection.
Compressed sources share the memory LRU, so progressive decoding usually reads an
external original once. Work for the same source is serialized to prevent duplicate
reads from concurrent thumbnail/fit requests. Other originals decode concurrently.
JPEG fit/thumbnail decoding uses reduced DCT dimensions at 1/2, 1/4, or 1/8 scale
when possible, then resizes to the requested edge. PNG, TIFF, and WebP need a normal
decode before resampling.

One byte counter charges retained RGBA/histogram/source buffers, evicted buffers
still held by external `Arc`s and source-read allocations. It also reserves
conservative decoder, orientation, ICC, resampling and persistence workspace. Eviction uses actual
buffer byte cost, and a `Weak` retirement record keeps an evicted allocation charged
until its last consumer releases it. Both decoder workers use the same weighted
admission gate. Work waits for another active operation to finish or rejects an
impossible footprint before allocating pixels. It never waits for growth while
holding its own RAW prefix reservation: impossible growth errors immediately,
avoiding self-deadlock and two-worker hold-and-wait. RAW prefix reads/compaction
are serialized, then decodes can proceed concurrently.

Generic scratch admission conservatively estimates 20 bytes per decoded pixel,
plus metadata workspace, including intermediate RGBA32F resampling, progressive
outputs, and a pending display copy. JPEG estimates use reduced DCT dimensions;
RAW, JXL, TIFF and HEIF use format-specific estimates described below. Whole-source
decodes are capped at 512 MiB and decoded canvases at 160 megapixels; embedded RAW
previews can come from larger files through the bounded prefix scan. Generic
metadata decoders use a 4 MiB allocation limit, and non-TIFF EXIF scanning is limited to the first 4 MiB.
Large Full views can therefore require a higher configured budget than their final
RGBA buffer alone. The error states the estimated requirement. These are allocation
estimates, not a hard native allocator cap or an exact process RSS ceiling: codec
internals, thread stacks, allocator arenas, driver allocations, and Rust/container bookkeeping remain outside
precise accounting. RSS can stay high after freed buffers return to an allocator.

`reserve_display_bytes(bytes) -> Option<DisplayReservation>` lets the desktop charge
its external CPU BGRA copies to the same pool before allocating them. Keep the
Send + Sync token with the conversion job and then the retained `RenderImage`.
Its drop only updates an atomic counter; the next stats/admission pass settles the
release, so dropping on GPUI never waits for the media mutex. A failed acquisition
means defer conversion until budget usage changes or show a budget message. Keep
the token until all corresponding CPU buffer owners release it. GPU texture bytes
and atlas fragmentation require a separate estimate and explicit GPUI image
retirement by the desktop; this module cannot observe GPU ownership.

`set_memory_budget(bytes) -> bool` applies runtime budget changes without waiting
for the mutex; false means retry on a later poll. Lowering evicts reclaimable
entries and queued persistence. Already active/pinned buffers remain accurately
charged, so usage may temporarily exceed a newly lowered budget until their owners
release them. New admissions respect the new limit. Increasing the limit clears
cached memory failures and wakes waiting work.

Thumbnail and fit previews are persisted losslessly on the local disk as versioned
`.srp` files containing metadata and PNG pixels. Writes use a temporary file and
rename. The default disk budget is 2 GiB; `set_disk_budget` changes it. Quota is
enforced after the next write, pruning to a 90% low-water mark. An incrementally
maintained ordered index makes each eviction O(log N), with no full-map sort per
write. Fits above two-thirds of the quota are evicted first to preserve space for
thumbnails. The directory is scanned once per process. Existing unchanged preview
files are not re-encoded or rewritten.

A separate low-priority persistence worker consumes a bounded queue of preview
Arcs (at most 64 MiB or a quarter of the RAM budget). Those Arcs remain charged to
the live allocation pool even after LRU eviction; PNG compression scratch also
requires admission. Decoders publish and immediately resume foreground work.
Persistence waits until foreground decode work is idle, drops superseded jobs, and
skips unchanged cached levels. Foreground admission drops queued persistence Arcs
when those pins would otherwise prevent a useful decode. Cache JSON dimensions are checked against the PNG
IHDR before any pixel decode; forged dimensions cannot bypass admission. Full frames are memory-only to avoid filling local
storage with hundreds of megabytes per original. A cached fit/thumbnail can be
displayed with its external drive disconnected because cache lookup precedes
original access and uses the catalog identity. An uncached full view still needs
the original. Damaged or obsolete cache entries are ignored and regenerated.

`cancel_pending()` latches cancellation without waiting on GPUI, then atomically
advances the generation and clears queued/pending/persistence work under the same
state lock. If contended, the next worker or request applies that latch. Queued
old work is dropped, and active old requests check the generation before decoding,
publication, and persistence. A codec already executing cannot be interrupted
safely; its result is discarded after it returns. An already started PNG write
can finish its bounded operation; queued obsolete writes are dropped. Pipeline drop signals workers
to stop and deliberately does not join a codec on the rendering thread.

`error[_with_size]` reports an actionable source/decode/budget error per identity.
Failures are cached in a bounded 128-entry table to prevent disconnected-drive
retry storms. `retry(path, modified, size, kind)` clears a failure after a reconnect.
`recent_errors() -> Option<Vec<RecentMediaError>>` supplies up to 32 historical
worker failures, oldest first, with timestamp, path, source revision, requested
tier, and an error message capped at 2048 UTF-8 bytes. Admission failures are
included; a later retry or successful decode does not erase the history. `None`
means mutex contention, so a diagnostic client can retry its nonblocking snapshot.
`stats()` exposes queued/active jobs, memory usage, memory/disk hits, physical source
reads, decodes, dropped requests, errors, local-cache bytes, compression writes,
persistence drops, externally held retired bytes, display-token bytes, in-flight
reservations, and the highest combined live+reserved estimate. A contended
stats read returns an empty snapshot; smooth counters in the UI if desired.

## Image and color behavior

JPEG, PNG, WebP and BMP use Rust image decoders. TIFF and JPEG XL have dedicated
Rust paths; RAW sensor rendering uses dynamically loaded LibRaw. HEIC, HEIF and
HIF use the platform paths below. AVIF is unsupported. Recognized RAW extensions,
including KDC, DCR and MRW, are indexed; actual camera/variant support depends on
the installed LibRaw build.

Every display buffer is straight-alpha **8-bit sRGB RGBA**. Supported source
precision is retained through color conversion, then quantized for display.
Orientation is applied once; source dimensions describe display geometry separately
from preview dimensions. ICC conversion uses Little CMS with relative colorimetric
rendering, except system HEIF color conversion on macOS. For generic image formats,
missing profiles assume sRGB; invalid/incompatible profiles preserve decoded pixels
and report `ColorStatus::InvalidProfile`. The dedicated paths below apply stricter
profile checks where needed. `AssumedSrgb` and `ConvertedToSrgb` expose the other
color outcomes.

Resampling uses display-encoded channels. Monitor presentation depends on GPUI and
the OS; the pipeline does not discover monitor profiles or promise calibrated
wide-gamut output. There is no HDR display, soft proofing, lens correction or
highlight recovery.

### RAW previews and sensor rendering

All preview tiers prefer a same-folder, case-insensitive same-stem JPEG companion,
then HEIC/HEIF/HIF. Independently cataloged files keep their live IDs. Companion
size/mtime participate in invalidation and export; RAW capture metadata remains
authoritative, with a companion filling only missing fields.

Without a companion, the bounded embedded-JPEG scan selects the largest supported
preview in the first 128 MiB, with at most 64 candidates and 32 MiB per candidate.
It skips JPEG APP/EXIF segments correctly. The retained source is compacted to the
JPEG plus orientation metadata before pixel decoding. If no supported embedded
preview is found, the pipeline falls back to sensor rendering, then resizes for
Fit/Thumbnail.

**Full uses the paired JPEG/HEIF or embedded JPEG at that preview's native
resolution**. Zooming does not trigger RAW development when a camera preview is
available. With neither kind of preview, `src/raw_render.rs` uses the public LibRaw C API; the prepared
runtime is LibRaw 0.22.2. Its prepared build excludes JPEG-lossy DNG and optional
SDK-dependent variants. Rendering demosaics with camera white balance, falling
back to daylight defaults if as-shot multipliers are unusable. LibRaw applies
orientation and converts camera color to sRGB; the pipeline must not apply EXIF or
source ICC again. No converter executable is launched. Missing libraries,
unsupported cameras and insufficient admission produce explicit errors; a camera
preview is never relabeled as a sensor Full.

`Preview.rendered_raw`, `embedded_raw`, and `rendering_note` identify sensor versus
camera-preview pixels and record LibRaw version/white-balance choice. Derived
smaller tiers retain this provenance. The desktop can keep showing an available
Fit when Full fails. MCP `photo_get` returns the current `preview` geometry, tier,
color/provenance fields and a separate `full_error`.

Sensor admission includes 32 bytes per sensor pixel, 16 per output pixel, two
source lengths for metadata, and 16 MiB fixed workspace; compressed source storage
is charged separately. A common 24 MP RAW fallback needs about 1.2 GiB. The desktop
defaults to automatic memory sizing: a quarter of physical RAM, clamped to
512 MiB–4 GiB. Existing explicit limits remain honored, and Preferences offers
Auto as well as manual values. This is conservative admission, **not a hard
cap on LibRaw's native allocator**. Inputs are limited to 512 MiB, 160 MP and
65,535 pixels per axis. Cancellation is cooperative at LibRaw progress callbacks
and stage boundaries.

### JPEG XL

`.jxl` uses the portable Rust `jxl-oxide` decoder with Little CMS; no native libjxl
runtime is needed. Raw codestreams and `jxlc`/multipart `jxlp` containers are
accepted. Integer SDR input, including 8/16-bit, gray/RGB, embedded ICC and straight
alpha is converted to sRGB before eight-bit quantization. Codestream orientation
is applied once. Animated files show the **first displayed frame only**.
HDR (including PQ/HLG), floating-point samples and associated/premultiplied alpha
are rejected explicitly.

Only codestream pieces are fed to the decoder. Auxiliary Exif/XMP/JUMBF,
JPEG-reconstruction and Brotli boxes are skipped, without decompression.
`inspect_file` seeks over their payloads, reads at most 64 KiB of codestream and
checks up to 4096 box headers/part indices (under 145 KiB total reads). Header
inspection does not certify compressed pixels. Limits include 512 MiB source,
160 MP, 100,000 pixels per axis, eight extra channels, 8 MiB initialization input
and 4 MiB expanded ICC. A stricter encoded-ICC expansion bound can reject some
large valid profiles.

Admission includes full-resolution grids even for thumbnails. The allocation
tracker limits **pixel grids**, while entropy tables, frame bookkeeping, allocator
overhead and Little CMS have conservative allowances rather than allocator caps.
A single float output row avoids an untracked full-frame float copy; grids are
dropped before resizing. The media counter therefore remains an estimate of total
working memory, not a process RSS limit.

### TIFF and BigTIFF

Classic TIFF and BigTIFF (`.tif`, `.tiff`, `.btf`, `.btiff`, `.tf8`) use the dedicated
Rust TIFF path, without libtiff. It supports unsigned 8/16-bit RGB/RGBA/gray,
float32 RGB/RGBA/gray, and 8/16-bit CMYK with a matching embedded ICC. Chunky and
planar storage, strips/tiles, all eight orientations, and uncompressed, LZW,
Deflate and PackBits compression are supported. Only the **first page/IFD** is
shown. Gray alpha requires an explicit alpha tag and BlackIsZero; straight alpha
is preserved and associated alpha is unpremultiplied at source precision.

Little CMS receives native 8/16-bit or float samples before display quantization.
Unprofiled integer RGB/gray assumes encoded sRGB. Unprofiled float32 explicitly
assumes **linear sRGB**, clips to SDR and applies the sRGB transfer function;
this assumption appears in the rendering note. Alpha stays outside the transform.
Missing CMYK ICC, invalid/device-link profiles, source/profile color-space
mismatches and unprofiled alternative TIFF color-characterization tags are errors.

Palette, YCbCr, signed/uint32/f16/f64 samples, mixed sample layouts, extra bands,
non-CMYK separated inks and other compression are unsupported. JPEG/WebP TIFF
compression is rejected before decode because its internal codec dimensions lack
a separately bounded inspection path.

`inspect_file` seeks to bounded first-IFD/tag metadata without reading pixel
strips/tiles or ICC payloads. Limits are 512 MiB source, 160 MP, 100,000 pixels per
axis, 4096 first-IFD entries, 4 MiB metadata/header reads, 4 MiB ICC and 200,000
strips/tiles. Offsets, arrays, ranges, tile dimensions and uncompressed byte counts
are checked. Admission charges a complete native-precision decode even for a
thumbnail, plus codec, color, orientation and resampling buffers.

### HEIC, HEIF and HIF

These extensions are indexed even without a usable codec. Linux dynamically loads
`libheif.so.1`; its runtime needs an HEVC decoder such as libde265. macOS uses
system ImageIO/CoreGraphics and ColorSync, including native thumbnails. Other
platforms report decoding unavailable. No per-file converter process is launched.

Before native pixel allocation, a bounded ISO-BMFF parser checks primary property
associations, dimensions, clean aperture, rotation, bit depth and NCLX/ICC metadata.
Limits are 4096 boxes per inspected container, 1024 image properties, 64 primary
property associations, 8 MiB metadata, 4 MiB ICC and 160 MP per image/tile. File
inspection seeks over compressed payloads and validates trailing box headers.
Duplicate critical boxes, invalid sizes, unsupported fractional crops, depths
above 16 and PQ/HLG transfer functions are rejected. Native dimensions are checked
before decode and before copying; libheif receives an admitted maximum image edge
and uses no parallel tile workers. Linux also rejects premultiplied-alpha inputs.

For Linux SDR above eight bits, libheif is asked for RGBA16. ICC conversion uses
16-bit samples, and supported NCLX BT.709, Display-P3 and BT.2020 transforms retain
16-bit precision before the final eight-bit sRGB quantization. Unspecified color
assumes sRGB; unsupported NCLX without ICC fails. macOS uses the system-native
ImageIO/ColorSync conversion into an explicit eight-bit sRGB bitmap and returns
straight alpha. Native crop/rotation/mirroring is applied exactly once; EXIF
orientation is not reapplied. PQ/HLG is rejected rather than tone-mapped.

Linux decodes the native full image before resizing. Admission charges 40 bytes
per largest declared coded pixel, two compressed-source lengths and 13 MiB
metadata/profile overhead; retained source bytes are charged separately. macOS
charges 20 bytes per source pixel plus 20 per requested thumbnail pixel and the
same overhead. A 24 MP Linux HEIF can exceed a manually selected 512 MiB even for Fit.
These are conservative estimates, not hard foreign-library allocation bounds.

A prepared EL8/glibc 2.28 codec payload includes LibRaw 0.22.2 and libheif 1.23.5
with libde265 1.0.18, source archives/RPMs, build scripts and license notices.
LibRaw is dual LGPL-2.1/CDDL-1.0; libheif/libde265 retain their LGPL notices and
replacement rights. The launcher scopes runtime library paths to the app process.
Final package assembly and application/platform validation of the expanded format
paths are pending; older package test results do not validate these additions.

### Histograms and similarity

Histograms contain 256 bins each for red, green, blue, and weighted luminance.
They count actual display-preview pixels, with at most 512 samples per axis.
Larger previews use deterministic jittered samples in a bounded grid, avoiding
the clipping loss from counting an averaged thumbnail and the periodic aliasing
of a fixed stride. Fully transparent pixels are excluded; partial alpha counts
once without assuming a background. The explicit sample count is the total in
each channel. Large-preview counts are estimates and may miss small clipped
regions; clipping lost during an earlier preview resize cannot be recovered.
These are display previews, not RAW exposure statistics. Existing cache pixels
are analyzed again on load, so this change needs no cache migration.
`Preview.dhash` and
`difference_hash(&DynamicImage)` provide a 64-bit horizontal difference hash.
Hamming distance is `(left ^ right).count_ones()`. Compute it on oriented display
pixels, and persist hashes from decoded previews instead of rereading originals
during indexing. Hashes from different decode levels can differ slightly near
the grouping threshold.

## Verification

Core regressions can be run with `cargo test --locked --no-default-features --lib`.
Native fixture tests are opt-in and require compatible runtimes and independently
known reference pixels; header inspection alone does not verify color or decoding.
For example:

```sh
SHADOWROOM_HEIF_FIXTURE=/path/to/fixture.heic cargo test --locked --no-default-features --lib heif::tests::actual_native_heif_fixture -- --ignored --nocapture
# Optionally supply SHADOWROOM_HEIF_REFERENCE=/path/to/reference.bmp.
```

See [performance measurements](PERFORMANCE.md) for the published workload and [release checks](RELEASING.md) for package validation.
