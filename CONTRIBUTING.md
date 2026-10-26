# Contributing

Start with the [build guide](docs/BUILD.md) and [documentation index](docs/README.md).
Keep changes focused and explain the behavior being changed, how it was checked,
and any platform limitations.

## Local checks

```sh
cargo fmt --all -- --check
cargo test --locked --no-default-features
LCMS2_STATIC=1 cargo build --locked --release --bins
cargo test --locked --bin shadowroom
bash -n scripts/*.sh
uv run --no-project python scripts/check-docs.py
```

The core command includes integration tests for import, remote protocol,
round trips, grouping, image analysis, and persistence. Tests marked `ignored`
need explicit fixtures or measure workloads; read their requirements before
running them. The desktop tests need native development dependencies but do not
substitute for opening the app on a real desktop.

Linux native checks use private XDG directories and only control their own
application process. Run them on an X11/Xwayland desktop with `DISPLAY` set:

```sh
uv run --no-project python scripts/native-smoke.py --binary target/release/shadowroom
uv run --no-project python scripts/native-extended.py \
  --binary target/release/shadowroom --agent target/release/shadowroom-remote
uv run --no-project python scripts/native-feedback.py \
  --binary target/release/shadowroom
uv run --no-project python scripts/native-reset.py \
  --binary target/release/shadowroom
```

Other `native-*.py` scripts cover specific media, ingest, packaging, and recovery
paths; use `--help` for required fixtures. Prefer `uv run --no-project python`
for Python tooling. It is not an application runtime dependency.

`native-interactions.py` checks Apply/Cancel/Escape, restart persistence, and
Shift+Up/Down selection with real XTEST events. It additionally needs xdotool,
ImageMagick, and Tesseract; an authenticated Xvfb with a software Vulkan driver
provides repeatable input delivery when a compositor restricts injection.

`native-feedback.py` uses the same X11/XTEST, ImageMagick and Tesseract setup to
exercise the startup guide, selected Review subset, draggable Catalog/import
scrollbars, partial time-group selection, caret blinking and editing, JPEG-first
zoom, neighboring-frame read-ahead and focus peaking. It creates private fixtures
and records evidence under a fresh temporary directory
(or a new `--output` directory). Run it against the newly built desktop binary.

`native-reset.py` checks R, the Reset button, the command palette and automation;
atomic Undo/Redo; label preservation; no automatic advance; Catalog/Review/Compare
selection scope; text-input isolation; and durable restart. It uses the same
X11/XTEST and OCR prerequisites and creates its own disposable sources.

## Source layout

| Path | Responsibility |
| --- | --- |
| `src/desktop.rs`, `src/desktop/` | Native workspace and import UI |
| `src/media.rs`, `src/raw_render.rs`, format modules | Decoding, color, previews, and budgets |
| `src/catalog.rs`, `src/service.rs`, `src/workspace.rs` | Catalog, background work, and review state |
| `src/ingest/`, `src/ingest.rs` | Copy planning, verification, and import history |
| `src/remote/`, `src/network.rs`, `src/bin/` | Headless agent, protocol, and desktop connections |
| `tests/` | Integration and regression coverage |
| `scripts/`, `packaging/` | Native acceptance and release tooling |
| `assets/`, `vendor/` | Original assets, licensed resources, and the xattr patch |

Keep catalog, cache, and generated evidence outside source photo folders.
Preserve read-only originals, report failed saves, and keep slow I/O off the UI
thread. Add regression coverage when changing these contracts. Keep all required
license notices when changing dependencies or assets.

Generated build output, local evidence, catalogs, and release archives are ignored.
Documentation screenshots and published benchmark data belong under `docs/`.
See [release checks](docs/RELEASING.md) before preparing an artifact.
