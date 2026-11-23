# Human-interface audit tools

These tools produced the measurements in
[docs/HUMAN_INTERFACE.md](../../docs/HUMAN_INTERFACE.md). They run against a
built binary and never touch your own Shadowroom profile or photos.

## Screenshot harness

`capture_states.py` drives the native window through about 120 interface states
(empty library, catalog, review, compare, survey, focus mode, every overlay and
dialog, Import, export pickers, save failures). It runs dark and light themes at
1440 × 900, a dark pass at 1024 × 680, and a persistence-failure pass. For each
state it records how the state was reached, a `workspace_state` check, and the
transition timings. Standard library only.

```sh
SHADOWROOM_FILE_CHOOSER=builtin \
DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent \
uv run --no-project python -I tools/hi-audit/capture_states.py \
  --binary target/release/shadowroom \
  --photos /path/to/demo-photos \
  --out /tmp/shadowroom-hi/after \
  --compare /tmp/shadowroom-hi/before/manifest.json
```

- It starts a private rootful Xwayland server, so its synthetic keys and clicks
  never reach your own windows. A small window titled with that display opens
  while it runs. Linux only; it needs Xwayland, xdotool and ImageMagick.
- The app gets its own `HOME` and `XDG_*` folders under `--session` (default: a
  fixed folder in the system temp directory). **That folder is deleted at the
  start of every run**, so never point `--session` at anything you want to keep.
  Concurrent runs need distinct `--session` folders.
- The demo photos are copied first; the originals are never modified. The
  harness checks this, and that your real profile was untouched, before it exits.
- The two environment variables keep the run from opening your desktop's real
  file-chooser portal; the built-in picker is captured instead.
- `--compare` labels each capture `identical`, `noise` (GPU jitter of a few
  colour levels) or `changed`, with a bounding box. `--only` and `--passes`
  restrict the run.
- Timings (`mcp_rtt_ms`, `frame_change_ms`, `settle_ms`) are coarse and sensitive
  to host load. Use `scripts/native-responsiveness.py` with
  `SHADOWROOM_FRAME_TRACE` for latency measurements
  ([PERFORMANCE.md](../../docs/PERFORMANCE.md#human-interface-responsiveness-2026-11-16)).

## Contrast and colour-vision audit

`contrast_audit.py` reads each audited commit's palettes with
`git show <sha>:src/theme.rs` (read-only) and measures the same text, component
and state pairs for every built-in theme, plus pairs that exist only in later
rounds. It reports WCAG 2.2 ratios (translucent layers composited over their
surface, photo badges measured over white, black and grey) and a colour-vision
simulation of the label palette (Machado 2009, CIEDE2000 ≥ 12).

```sh
uv run --no-project python -I tools/hi-audit/contrast_audit.py . /tmp/shadowroom-hi/contrast
```

The commits it measures are listed near the top of the script. Add a
`(label, sha)` row to audit a new revision; render-site notes explain where a
pair was re-pointed because the code started drawing a different token.
