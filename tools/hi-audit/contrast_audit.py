#!/usr/bin/env python3
"""Shadowroom contrast and colour-vision audit.

Reads each audited commit's palettes with `git show <sha>:src/theme.rs`
(read-only; the working tree is never touched), mirrors Theme::builtin (aliases,
status alphas and, from r1 on, the PHOTO_OVERLAY roles), and measures:

  * CORE: the same 71 pair ids per theme in every round, so the category score
    stays comparable. Where a refinement moved a render site to another token,
    the pair is re-pointed at what the code draws at that commit (`site_note`).
  * EXTENDED: pairs that exist only after a refinement (focus ring, field
    outlines, label chips, cull ledger, image menu, clipping overlay, decision
    stamp, ...). Reported separately; they never change the comparable score.
  * Colour vision: label palettes and decision colours under protan / deutan /
    tritan simulation (Machado 2009, severity 1.0), CIEDE2000 >= 12 to pass.

Method: WCAG 2.2 relative luminance; translucent layers are composited over
their surface; on-photo plates are measured over white, black and mid-grey
photos and the worst case is kept. Text needs 4.5:1, graphics and state cues
3:1; `info` rows are recorded but not scored (decorative or redundant cues).

Scoring variants per run:
  * core                  -- the baseline rule: a choice-group outline is a 3:1
                             control boundary (unselected segments have no fill).
  * core_lenient_outline  -- choice-group outlines treated as decorative (the
                             docs/THEMES.md decision: text identifies segments,
                             the selected bar and the focus ring carry 3:1).
  * core_strict_state     -- selected fills must also reach 3:1 on their own.

Usage:
    uv run --no-project python -I contrast.py REPO OUT_DIR

Writes OUT_DIR/contrast-<label>.json for every row of VERSIONS and
HIGH_CONTRAST, and OUT_DIR/contrast-summary.json. Standard library only.

To audit a new revision, add a (label, sha) row to VERSIONS (and to
HIGH_CONTRAST for the high-contrast palettes). A new round inherits the latest
round's render sites; add a gated block in `extended()` / `SITES` when the code
starts drawing a pair with a different token.
"""
from __future__ import annotations

import json
import math
import re
import subprocess
import sys
from pathlib import Path

# Audited commits, oldest first. The round number (list position) gates the
# render-site re-pointing below.
VERSIONS = [
    ("before", "c0760d5"),
    ("r1", "3a5567b"),
    ("r2", "e355fe4"),
    ("r3", "05c6fc1"),
    ("r4", "837512e"),
    ("r5", "72b936e"),
]
# High-contrast palettes (Theme::high_contrast_dark/light), measured with the
# render sites of the round they belong to.
HIGH_CONTRAST = [
    ("r4-hc", "837512e", "r4"),
    ("r5-hc", "72b936e", "r5"),
]
THEME_FNS = {"dark": "Shadowroom Dark", "light": "Shadowroom Light",
             "high_contrast_dark": "Shadowroom High Contrast Dark",
             "high_contrast_light": "Shadowroom High Contrast Light"}

REPO = Path(".")
ROUND = {label: n for n, (label, _) in enumerate(VERSIONS)}
ROUND.update({label: ROUND[base] for label, _, base in HIGH_CONTRAST})


def git_show(sha: str, path: str, required: bool = True) -> str:
    proc = subprocess.run(["git", "-C", str(REPO), "show", f"{sha}:{path}"],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        if required:
            raise SystemExit(f"git show {sha}:{path} failed: {proc.stderr.strip()}")
        return ""
    return proc.stdout


# ----------------------------------------------------------------- parsing
def parse_hex(s: str):
    s = s.strip().lstrip("#")
    if len(s) in (3, 4):
        s = "".join(c * 2 for c in s)
    if len(s) == 6:
        s += "ff"
    r, g, b, a = (int(s[i:i + 2], 16) for i in range(0, 8, 2))
    return r / 255, g / 255, b / 255, a / 255


def to_hex(c) -> str:
    return "#" + "".join(f"{round(v * 255):02x}" for v in c[:3])


def theme_block(src: str, fn: str) -> dict[str, str]:
    m = re.search(rf"pub fn {fn}\(\) -> Self \{{(.*?)\n    \}}", src, re.S)
    if not m:
        raise SystemExit(f"Theme::{fn} not found")
    pairs = dict(re.findall(r'\("([a-z_.]+)",\s*"(#[0-9a-fA-F]+)"\)', m.group(1)))
    alias_block = re.search(r"for \(key, source\) in \[(.*?)\]\s*\{", src, re.S).group(1)
    for key, source in re.findall(r'\("([a-z_.]+)",\s*"([a-z_.]+)"\)', alias_block):
        if source in pairs:
            pairs[key] = pairs[source]
    ov = re.search(r"pub const PHOTO_OVERLAY: \[\(&str, &str\); \d+\] = \[(.*?)\];", src, re.S)
    if ov:  # r1+: identical on-photo roles in every palette
        pairs.update(dict(re.findall(r'\("([a-z_.]+)",\s*"(#[0-9a-fA-F]+)"\)', ov.group(1))))
    for status in ("success", "error", "warning", "info"):
        if status in pairs:
            base = pairs[status][:7]
            pairs[f"{status}.background"] = base + "1a"
            pairs[f"{status}.border"] = base + "80"
    return pairs


def code_constants(sha: str) -> dict[str, str]:
    """Colours the code draws outside the theme (r5+): clipping inks and the
    decision-stamp plate opacity. Missing constants simply leave rows n/a."""
    out = {}
    peaking = git_show(sha, "src/peaking.rs", required=False)
    for name in ("HIGHLIGHT_INK", "SHADOW_INK"):
        m = re.search(rf"pub const {name}: \[u8; 3\] = \[(\d+),\s*(\d+),\s*(\d+)\]", peaking)
        if m:
            out[name] = "#" + "".join(f"{int(v):02x}" for v in m.groups())
    aids = git_show(sha, "src/desktop/loupe_aids.rs", required=False)
    m = re.search(r"const STAMP_PLATE_ALPHA: f32 = ([0-9.]+);", aids)
    if m:
        out["STAMP_PLATE_ALPHA"] = m.group(1)
    return out


# ----------------------------------------------------------------- colour math
def over(fg, bg):
    a = fg[3]
    return tuple(fg[i] * a + bg[i] * (1 - a) for i in range(3)) + (1.0,)


def lin(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def lum(c):
    r, g, b = (lin(v) for v in c[:3])
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def ratio(a, b):
    la, lb = lum(a), lum(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def text_on(fill):
    """dialog.rs text_on(): white or #0b0b0b, whichever contrasts more."""
    l = lum(fill)
    return parse_hex("#ffffff") if 1.05 / (l + 0.05) >= (l + 0.05) / 0.0534 else parse_hex("#0b0b0b")


# ----------------------------------------------------------------- core pairs
# (id, category, fg, bg, kind, size, where). fg/bg are token names, "#hex",
# ("over", top, base), ("alpha", tok, a), "TEXT_ON:<tok>", "PLATE" (the badge
# scrim over a photo) or "PLATE:<#rrggbbaa>" (another plate). Per-round
# overrides live in SITES.
CORE = [
    ("body.background", "body", "text", "background", "text", 12, "root text on window bg"),
    ("body.panel", "body", "text", "panel.background", "text", 12, "sidebar / inspector rows"),
    ("body.toolbar", "body", "text", "toolbar.background", "text", 12, "toolbar, decision bar"),
    ("body.title", "body", "text", "title_bar.background", "text", 12, "title bar, view tabs"),
    ("body.status", "body", "text", "status_bar.background", "text", 12, "status bar"),
    ("body.elevated", "body", "text", "elevated_surface.background", "text", 12, "dialogs, menus, tooltips"),
    ("body.canvas", "body", "text", "shadowroom.canvas", "text", 15, "empty-state headline on canvas"),
    ("body.element", "body", "text", "element.background", "text", 12, "filled controls"),
    ("body.selected_row", "selected", "text", ("over", "ghost_element.selected", "panel.background"), "text", 12, "selected sidebar row / segment"),
    ("body.selected_elevated", "selected", "text", ("over", "ghost_element.selected", "elevated_surface.background"), "text", 12, "selected segment in Preferences / palette row"),
    ("body.hover_elevated", "body", "text", "element.hover", "text", 12, "hovered button"),
    ("muted.panel", "muted", "text.muted", "panel.background", "text", 11, "section headers, counts"),
    ("muted.toolbar", "muted", "text.muted", "toolbar.background", "text", 11, "decision-bar keyboard hints"),
    ("muted.title", "muted", "text.muted", "title_bar.background", "text", 12, "inactive view tabs"),
    ("muted.status", "muted", "text.muted", "status_bar.background", "text", 12, "status-bar counts"),
    ("muted.elevated", "muted", "text.muted", "elevated_surface.background", "text", 10, "dialog captions"),
    ("muted.canvas", "muted", "text.muted", "shadowroom.canvas", "text", 11, "empty-state hints, grid filenames"),
    ("muted.keycap", "keycap", "text.muted", "shadowroom.canvas", "text", 10, "keycap glyphs (10px on canvas_bg)"),
    ("muted.selected_row", "selected", "text.muted", ("over", "ghost_element.selected", "panel.background"), "text", 11, "count in selected sidebar row"),
    ("muted.hover_row", "muted", "text.muted", ("over", "ghost_element.hover", "panel.background"), "text", 11, "count in hovered sidebar row"),
    ("disabled.panel", "disabled", "text.disabled", "panel.background", "info", 12, "disabled text (exempt)"),
    ("disabled.elevated", "disabled", "text.disabled", "elevated_surface.background", "info", 12, "disabled text in dialogs"),
    ("accent.toolbar", "accent", "text.accent", "toolbar.background", "text", 10, "'N selected'"),
    ("accent.panel", "accent", "text.accent", "panel.background", "text", 12, "accent text in panels"),
    ("accent.elevated", "accent", "text.accent", "elevated_surface.background", "text", 12, "accent in dialogs"),
    ("accent.selected_row", "accent", "text.accent", ("over", "ghost_element.selected", "panel.background"), "ui", None, "accent bar/icon in selected row"),
    ("error.status", "status", "error", "status_bar.background", "text", 12, "'Not saved · Retry'"),
    ("error.elevated", "status", "error", "elevated_surface.background", "text", 12, "form error / DestructiveQuiet text"),
    ("warning.toolbar", "status", "warning", "toolbar.background", "text", 10, "warning text (10px)"),
    ("success.panel", "status", "success", "panel.background", "ui", None, "host-connected dot (6px)"),
    ("warning.panel", "status", "warning", "panel.background", "ui", None, "host-connecting dot (6px)"),
    ("error.panel", "status", "error", "panel.background", "ui", None, "host-error dot (6px)"),
    ("error.toast_border", "status", "error", "elevated_surface.background", "ui", None, "error toast stripe/icon"),
    ("icon.panel", "icons", "icon", "panel.background", "ui", None, "active icons"),
    ("icon.title", "icons", "icon", "title_bar.background", "ui", None, "window controls"),
    ("icon_muted.toolbar", "icons", "icon.muted", "toolbar.background", "ui", None, "inactive toolbar icons"),
    ("icon_muted.panel", "icons", "icon.muted", "panel.background", "ui", None, "sidebar icons"),
    ("icon_muted.elevated", "icons", "icon.muted", "elevated_surface.background", "ui", None, "dialog icons"),
    ("icon_muted.vs_rating", "icons", "icon.muted", "toolbar.background", "ui", None, "empty star outline"),
    ("border.panel", "borders", "border", "panel.background", "ui", None, "segmented-choice outline on panel"),
    ("border.elevated", "borders", "border", "elevated_surface.background", "ui", None, "choice-group outline in Preferences"),
    ("border.background", "borders", "border", "background", "info", None, "panel dividers (decorative)"),
    ("border_variant.elevated", "borders", "border.variant", "elevated_surface.background", "info", None, "dialog / menu outline"),
    ("border_variant.background", "borders", "border.variant", "background", "info", None, "dialog edge vs window"),
    ("focus.elevated", "focus", "border.focused", "elevated_surface.background", "ui", None, "keyboard focus outline in dialogs"),
    ("focus.selected_bg", "focus", "border.focused", ("over", "ghost_element.selected", "elevated_surface.background"), "ui", None, "focus outline vs selected fill"),
    ("focus.toolbar", "focus", "border.focused", "toolbar.background", "ui", None, "active toolbar control outline"),
    ("select.grid", "focus", "text.accent", "shadowroom.canvas", "ui", None, "selected grid cell 1px border"),
    ("select.film", "focus", "text.accent", "panel.background", "ui", None, "selected filmstrip 2px border"),
    ("select.multi", "focus", ("alpha", "text.accent", 0.45), "panel.background", "ui", None, "multi-select filmstrip border"),
    ("select.tab", "focus", "text.accent", "title_bar.background", "ui", None, "active view-tab underline"),
    ("select.label_swatch_ring", "focus", "text", "panel.background", "ui", None, "selected label swatch ring"),
    ("state.selected_vs_panel", "selected", ("over", "ghost_element.selected", "panel.background"), "panel.background", "state", None, "selected row fill vs unselected"),
    ("state.selected_vs_elevated", "selected", ("over", "ghost_element.selected", "elevated_surface.background"), "elevated_surface.background", "state", None, "selected Preferences segment fill vs unselected"),
    ("state.hover_vs_panel", "selected", ("over", "ghost_element.hover", "panel.background"), "panel.background", "info", None, "hover fill"),
    ("state.hover_vs_selected", "selected", "element.hover", ("over", "ghost_element.selected", "elevated_surface.background"), "info", None, "hover vs selected fill on dialogs"),
    ("rating.toolbar", "decisions", "shadowroom.rating", "toolbar.background", "ui", None, "filled star in decision bar"),
    ("rating.canvas_text", "decisions", "shadowroom.rating", "shadowroom.canvas", "text", 11, "grid star text"),
    ("pick.toolbar_text", "decisions", "shadowroom.pick", ("over", ("alpha", "shadowroom.pick", 0.15), "toolbar.background"), "text", 12, "'Pick' label on its tint"),
    ("reject.toolbar_text", "decisions", "shadowroom.reject", ("over", ("alpha", "shadowroom.reject", 0.15), "toolbar.background"), "text", 12, "'Reject' label on its tint"),
    ("pick.vs_reject", "decisions", "shadowroom.pick", "shadowroom.reject", "info", None, "pick vs reject hue pair"),
    # thumbnail badges on the plate (worst photo): fg only, bg = plate
    ("badge.rating", "thumbnails", "shadowroom.rating", "PLATE", "text", 9, "filmstrip star text on badge plate"),
    ("badge.pick", "thumbnails", "shadowroom.pick", "PLATE", "ui", None, "pick glyph on badge plate"),
    ("badge.reject", "thumbnails", "shadowroom.reject", "PLATE", "ui", None, "reject glyph on badge plate"),
    ("badge.multi", "thumbnails", "text.accent", "PLATE", "ui", None, "multi-select check on badge plate"),
    ("badge.count_text", "thumbnails", "#e6e6e6", "PLATE", "text", 9, "group-count text"),
    ("hist.label_panel", "histogram", "#6e6e6e", "panel.background", "text", 9, "histogram axis/footer labels"),
    ("hist.label_strong_panel", "histogram", "#a0a0a0", "panel.background", "text", 9, "histogram clip readout"),
    ("hist.border_panel", "histogram", "#262626", "panel.background", "info", None, "histogram plot outline"),
    ("caption.compare", "thumbnails", "#9a9a9a", ("over", "#0f0f0fd0", "#ffffff"), "text", 11, "Compare caption over white photo"),
]
LABELS = ["Red", "Yellow", "Green", "Blue", "Purple"]

# ----------------------------------------------------------------- render sites
# Per-round re-pointing of CORE pairs: {pair id: field overrides}.
R1_SITES = {
    "border.panel": dict(fg="shadowroom.control_border", site_note="choices() outline -> control_border (desktop.rs choices; r2 desktop.rs:10518)"),
    "border.elevated": dict(fg="shadowroom.control_border", site_note="Preferences choice groups -> control_border"),
    "focus.elevated": dict(fg="shadowroom.focus_ring", site_note="focus_ring_overlay() dialog.rs:303"),
    "focus.selected_bg": dict(fg="shadowroom.focus_ring", site_note="ring on a focused selected segment"),
    "select.multi": dict(fg=("alpha", "text.accent", 0.8), site_note="multi-select border opacity 0.8 (r2 desktop.rs:8552)"),
    "pick.toolbar_text": dict(bg=("over", ("alpha", "shadowroom.pick", 0.10), "toolbar.background"), site_note="tint 0.10 (r2 desktop.rs:8755)"),
    "reject.toolbar_text": dict(bg=("over", ("alpha", "shadowroom.reject", 0.10), "toolbar.background"), site_note="tint 0.10"),
    "state.selected_vs_panel": dict(kind="info", site_note="no longer the sole cue: rows add a 2px accent bar; see ext state.segment_bar_*"),
    "state.selected_vs_elevated": dict(kind="info", site_note="no longer the sole cue: segment() adds a 2px text-colour bar (r2 desktop.rs:5876-5912)"),
    "badge.rating": dict(fg="shadowroom.scrim.rating", size=11, site_note="scrim_rating()"),
    "badge.pick": dict(fg="shadowroom.scrim.pick", site_note="scrim_flag()"),
    "badge.reject": dict(fg="shadowroom.scrim.reject", site_note="scrim_flag()"),
    "badge.multi": dict(fg="shadowroom.scrim.accent", site_note="scrim_accent()"),
    "badge.count_text": dict(fg="shadowroom.scrim.text", site_note="scrim_text()"),
    "hist.label_panel": dict(fg="text.muted", site_note="HistogramOptions.label = theme muted"),
    "hist.label_strong_panel": dict(fg="text", site_note="HistogramOptions.label_strong = self.text() (r2 desktop.rs:9291)"),
    "caption.compare": dict(fg="shadowroom.scrim.muted", bg="PLATE", site_note="compare caption on scrim plate"),
}
# r3 (b5363eb): choices()/grid-size/LIN-LOG outlines and keycaps go back to the
# border.variant hairline; r3 (7e39a5e): thumbnail label badge becomes a filled
# chip with a near-black initial.
R3_SITES = dict(R1_SITES)
R3_SITES.update({
    "border.panel": dict(fg="border.variant", site_note="r3 b5363eb: choices()/LIN-LOG outline = group_border() = border.variant (r4 desktop.rs:9897, 11231)"),
    "border.elevated": dict(fg="border.variant", site_note="r3 b5363eb: Preferences choice groups = group_border() = border.variant (r4 desktop.rs:11231)"),
    "hist.border_panel": dict(fg="border", site_note="plot_border = self.border() (r4 desktop.rs:9970)"),
    "badge.rating": dict(fg="shadowroom.scrim.rating", size=10, site_note="rating_badge scrim_rating, MICRO"),
})
R4_SITES = dict(R3_SITES)
R4_SITES.update({
    "badge.rating": dict(fg="shadowroom.scrim.rating", size=11, site_note="rating_badge scrim_rating, MICRO 11 (r4 desktop.rs:8888)"),
})
# r5: no CORE site moves. The hairline choice-group outline is now a recorded
# decision (docs/THEMES.md "Choice groups"); the strict score keeps the baseline
# rule and the lenient variant reflects the decision.
R5_SITES = dict(R4_SITES)
R5_SITES.update({
    "border.panel": dict(fg="border.variant", site_note="group_border() = border.variant; decorative by decision (docs/THEMES.md Choice groups, r5 d0aa71e)"),
    "border.elevated": dict(fg="border.variant", site_note="group_border() = border.variant; decorative by decision (docs/THEMES.md Choice groups, r5 d0aa71e)"),
})
SITES_BY_ROUND = [{}, R1_SITES, R1_SITES, R3_SITES, R4_SITES, R5_SITES]


def sites(label: str) -> dict:
    return SITES_BY_ROUND[min(ROUND[label], len(SITES_BY_ROUND) - 1)]


# Choice-group outlines: counted as 3:1 control boundaries in `core`, and as
# decorative in `core_lenient_outline`.
LENIENT_INFO = {"border.panel", "border.elevated"}


# ----------------------------------------------------------------- extended pairs
def extended_r2(post: bool):
    """Pairs added by rounds 1-2 (for `before`, the equivalent drawing)."""
    fr = "shadowroom.focus_ring" if post else "border.focused"
    cb = "shadowroom.control_border" if post else "border"
    rows = [
        ("ext.control_border.canvas", "controls", cb, "shadowroom.canvas", "ui", None, "keycap outline vs its canvas fill"),
        ("ext.control_border.toolbar", "controls", cb, "toolbar.background", "ui", None, "keycap / grid-size outline on toolbar"),
        ("ext.field.outline_elevated", "controls", "border", "elevated_surface.background", "ui", None, "inactive text-field outline in dialogs (add-host, picker): still `border`"),
        ("ext.field.outline_panel", "controls", "border", "panel.background", "ui", None, "inactive Import text-field outline: still `border`"),
        ("ext.field.fill_elevated", "controls", "background", "elevated_surface.background", "info", None, "text-field fill vs dialog (fill is the only other extent cue)"),
        ("ext.focus.panel", "focus", fr, "panel.background", "ui", None, "focus ring on panel surfaces"),
        ("ext.focus.toolbar", "focus", fr, "toolbar.background", "ui", None, "focus ring on bars"),
        ("ext.state.segment_bar_elevated", "selected", "text" if post else None, "elevated_surface.background", "ui", None, "2px selected-segment bar (text colour) in Preferences"),
        ("ext.state.segment_bar_panel", "selected", "text" if post else None, "panel.background", "ui", None, "2px selected-segment bar on panels (LIN/LOG)"),
        ("ext.palette.accent_bar", "selected", "text.accent" if post else None, ("over", "ghost_element.selected", "elevated_surface.background"), "ui", None, "palette selected-row accent bar"),
        ("ext.palette.muted_selected", "selected", "text.muted", ("over", "ghost_element.selected", "elevated_surface.background"), "text", 11, "palette category/keys on selected row"),
        ("ext.palette.recent_selected", "selected", "text.accent", ("over", "ghost_element.selected", "elevated_surface.background"), "text", 11, "palette 'recent' accent text on selected row"),
        ("ext.button.primary_text", "dialogs", "TEXT_ON:text.accent" if post else None, "text.accent", "text", 12, "Primary (accent-filled) button label"),
        ("ext.button.destructive_text", "dialogs", "TEXT_ON:error" if post else None, "error", "text", 12, "Destructive (error-filled) button label"),
        ("ext.button.primary_fill", "dialogs", "text.accent" if post else None, "elevated_surface.background", "info", None, "Primary fill vs dialog"),
        ("ext.button.secondary_outline", "dialogs", "border.variant" if post else None, "elevated_surface.background", "info", None, "Secondary button 1px outline (label identifies the control)"),
        ("ext.notice.success_icon", "notices", "success", "status_bar.background", "ui", None, "success glyph in status-bar notice"),
        ("ext.notice.warning_icon", "notices", "warning", "status_bar.background", "ui", None, "warning glyph in status-bar notice"),
        ("ext.notice.text", "notices", "text", "status_bar.background", "text", 12, "notice text"),
        ("ext.import.current_tile", "import", "text.accent", "background", "ui", None, "Import current-tile accent outline"),
        ("ext.import.included_tile", "import", cb, "background", "ui" if post else "info", None, "Import included-tile outline (r2: control_border)"),
        ("ext.well.vs_canvas", "thumbnails", "shadowroom.well" if post else None, "shadowroom.canvas", "info", None, "thumbnail well vs grid canvas"),
        ("ext.modal.surface_vs_scrimmed_panel", "modal", "elevated_surface.background",
         ("over", "shadowroom.scrim.modal" if post else "#00000060", "panel.background"), "info", None, "dialog surface vs scrimmed panel behind it"),
        ("ext.modal.surface_vs_scrimmed_canvas", "modal", "elevated_surface.background",
         ("over", "shadowroom.scrim.modal" if post else "#00000060", "shadowroom.canvas"), "info", None, "dialog surface vs scrimmed canvas/photo grid"),
    ]
    for name in LABELS:  # label chips in chrome: initial (label.text) on the swatch
        rows.append((f"ext.label_chip.{name.lower()}", "labels", "shadowroom.label.text" if post else None,
                     f"shadowroom.label.{name.lower()}" if post else None, "text", 10, "label initial on chrome swatch (10px)"))
    return rows


def extended(label: str):
    """Extended rows as drawn at this round: r1/r2 rows re-pointed, plus the
    pairs each later round added."""
    n = ROUND[label]
    post = n >= 1
    fr = "shadowroom.focus_ring" if post else "border.focused"
    sel = ("over", "ghost_element.selected", "elevated_surface.background")
    rows = []
    for r in extended_r2(post):
        pid = r[0]
        r = list(r)
        if n >= 3 and pid == "ext.control_border.canvas":
            r[2] = "border.variant"; r[6] = "keycap outline vs its canvas fill: r3 group_border (border.variant); keycaps are labels, not controls"
            r[4] = "info"
        if n >= 3 and pid == "ext.control_border.toolbar":
            r[2] = "border.variant"; r[6] = "grid-size S/M/L outline on the decision bar: r3 group_border (border.variant)"
        if n >= 3 and pid in ("ext.field.outline_elevated", "ext.field.outline_panel"):
            r[2] = "shadowroom.control_border"; r[6] = r[6].replace("still `border`", "r3 fc885ef: control_border")
        if n >= 3 and pid == "ext.palette.recent_selected":
            r[2] = "text"; r[6] = "palette 'Recent' tag on the active row: r3 text (inactive rows text.muted)"
        if n >= 3 and pid == "ext.import.included_tile":
            r[2] = "border"; r[6] = "Import tile outline: r3 b5363eb hairline `border` either way (checkbox + text carry 'included')"
        if n >= 3 and pid.startswith("ext.label_chip."):
            r[5] = 11 if n >= 4 else 10
            r[6] = "inspector swatch key digit (6-9; Purple none) in label.text on chrome label fill"
            if pid.endswith("purple"):
                if n >= 5:
                    r[6] = "inspector Purple swatch: its initial P in label.text (r5 label_glyph)"
                else:
                    r[4] = "info"; r[6] += " -- Purple swatch draws no text"
        rows.append(tuple(r))
    if n >= 3:
        reviewed = "shadowroom.ledger.reviewed" if n >= 5 else "icon.muted"
        rail = "shadowroom.ledger.rail" if n >= 5 else "border"
        rows += [
            ("ext.field.outline_vs_fill", "controls", "shadowroom.control_border", "background", "ui", None, "text-field outline vs its own fill (bg)"),
            ("ext.ledger.pick", "ledger", "shadowroom.pick", "panel.background", "ui", None, "cull ledger pick tick (4px) on panel"),
            ("ext.ledger.reject", "ledger", "shadowroom.reject", "panel.background", "ui", None, "cull ledger reject tick on panel"),
            ("ext.ledger.reviewed", "ledger", reviewed, "panel.background", "ui", None, "cull ledger reviewed tick on panel"),
            ("ext.ledger.rail", "ledger", rail, "panel.background", "info", None,
             "unreviewed rail: r5 its own token, a dim hairline (theme test asks 1.5:1)" if n >= 5 else "unreviewed rail (bare track)"),
            ("ext.ledger.playhead", "ledger", "text.accent", "panel.background", "ui", None, "ledger playhead (accent, 1px knockout)"),
            ("ext.ledger.reviewed_vs_rail", "ledger", reviewed, rail, "ui", None, "reviewed tick vs adjacent unreviewed rail"),
            ("ext.ledger.pick_vs_reviewed", "ledger", "shadowroom.pick", reviewed, "info", None,
             "pick vs reviewed: r5 shape-coded (pick 4px bar above the rail, reviewed 3px on it)" if n >= 5 else "pick vs reviewed tick (hue/lightness only)"),
            ("ext.hist.plot_vs_panel", "histogram", "shadowroom.histogram.background", "panel.background", "info", None, "themed histogram plate vs panel"),
            ("ext.hist.clip_warning", "histogram", "warning", "panel.background", "text", 11, "clip readout in warning colour (>0.5%)"),
        ]
        for name in LABELS:
            l = name.lower()
            rows.append((f"ext.label_strip.{l}.well", "labels", f"shadowroom.label.{l}", "shadowroom.well", "ui", None, "grid label strip (3px) on the image well"))
            if n >= 5:
                rows.append((f"ext.label_strip.{l}.canvas", "labels", f"shadowroom.scrim.label.{l}", "PLATE", "ui", None,
                             "r5 filmstrip strip sits on the photo: photo-overlay ink over a 1 px scrim line (label_strip on_photo)"))
            else:
                rows.append((f"ext.label_strip.{l}.canvas", "labels", f"shadowroom.label.{l}", "shadowroom.canvas", "ui", None, "filmstrip label strip (2px) on the canvas slot"))
            rows.append((f"ext.label_chip_fill.{l}", "labels", f"shadowroom.scrim.label.{l}", "PLATE", "info", None, "filled thumbnail chip vs its plate"))
    if n >= 4:
        rows += [
            ("ext.unsaved.glyph", "thumbnails", "shadowroom.scrim.reject", "PLATE", "ui", None, "unsaved-decision warning glyph on the badge plate (r4 desktop.rs:8865)"),
            ("ext.menu.title", "image_menu", "text.muted", "elevated_surface.background", "text", 12, "image menu header (filename)"),
            ("ext.menu.row_selected", "image_menu", "text", sel, "text", 12, "menu row label on the selected row"),
            ("ext.menu.keys_selected", "image_menu", "text.muted", sel, "text", 11, "keycap text on the selected row"),
            ("ext.menu.icon_selected", "image_menu", "icon.muted", sel, "ui", None, "row icon on the selected row"),
            ("ext.menu.destructive", "image_menu", "error", "elevated_surface.background", "text", 12, "'Delete Permanently…' label"),
            ("ext.menu.destructive_selected", "image_menu", "text" if n >= 5 else "error", sel, "text", 12,
             "'Delete Permanently…' on the selected row: r5 label turns to text (review_actions.rs:562-565)" if n >= 5 else "'Delete Permanently…' on the selected row"),
            ("ext.menu.chip_ring", "image_menu", "text", "elevated_surface.background", "ui", None, "current rating/label chip outline"),
            ("ext.menu.empty_label_chip", "image_menu", "shadowroom.control_border", "elevated_surface.background", "ui", None, "'No label' chip outline"),
            ("ext.menu.rating_digit", "image_menu", "text.muted", "elevated_surface.background", "text", 11, "rating chip digits"),
            ("ext.menu.selected_fill", "image_menu", sel, "elevated_surface.background", "info", None, "selected row fill (keycap+icon+row also change)"),
            ("ext.menu.separator", "image_menu", "border.variant", "elevated_surface.background", "info", None, "separator rule"),
            ("ext.focus.pane_panel", "focus", fr, "panel.background", "ui", None, "F6 pane ring (2px) on library/inspector"),
            ("ext.focus.pane_canvas", "focus", fr, "shadowroom.canvas", "ui", None, "F6 pane ring over the photo area"),
            ("ext.focus.library_cursor_selected", "focus", fr, ("over", "ghost_element.selected", "panel.background"), "ui", None, "library row cursor ring on the current (selected) row"),
            ("ext.notice.focus_plate_text", "notices", "text.muted", "status_bar.background", "text", 11, "Focus-mode notice plate text"),
            ("ext.prefs.description", "dialogs", "text.muted", "elevated_surface.background", "text", 11, "Preferences row descriptions (Text size etc.)"),
        ]
        for name in LABELS:
            l = name.lower()
            rows.append((f"ext.menu.label_chip.{l}", "image_menu", f"shadowroom.label.{l}", "elevated_surface.background", "ui", None,
                         "image-menu label chip (16px fill, initial, tooltip)" if n >= 5 else "image-menu label chip (12px square, colour only + tooltip)"))
    if n >= 5:
        rows += extended_r5()
    return rows


def extended_r5():
    """Pairs round 5 (72b936e) added. Inks that the code draws outside the
    theme come from code_constants() as "CONST:<name>" specs."""
    sel = ("over", "ghost_element.selected", "elevated_surface.background")
    rows = [
        # cull ledger shapes (ledger.rs: rail 1px, reviewed 3px on it, pick 4px above, reject 4px below)
        ("ext.ledger.bracket", "ledger", "text.muted", "panel.background", "ui", None, "filmstrip-window bracket (1px text.muted hairline with serifs)"),
        ("ext.ledger.reject_vs_pick_shape", "ledger", "shadowroom.reject", "shadowroom.pick", "info", None, "reject vs pick: r5 shape-coded (above vs below the rail)"),
        # clipping overlay and readout (peaking.rs inks; histogram.rs swatches and markers)
        ("ext.clip.stripe_blown", "clipping", "CONST:HIGHLIGHT_INK", "#ffffff", "ui", None, "magenta 45° stripes over blown (all channels >= 254) pixels"),
        ("ext.clip.shadow_fill", "clipping", "CONST:SHADOW_INK", "#000000", "ui", None, "solid blue over crushed (all channels <= 2) pixels, next to near-black"),
        ("ext.clip.stripe_channel_worst", "clipping", "CONST:HIGHLIGHT_INK", "#ff00ff", "info", None, "thin stripes over a colour-clipped region (worst: saturated magenta)"),
        ("ext.clip.readout_shadow_swatch", "clipping", "CONST:SHADOW_INK", "panel.background", "ui", None, "6px 'Shadows' readout swatch on the inspector panel"),
        ("ext.clip.readout_highlight_swatch", "clipping", "CONST:HIGHLIGHT_INK", "panel.background", "ui", None, "6px 'Highlights' readout swatch on the inspector panel"),
        ("ext.clip.marker_shadow_plot", "clipping", "CONST:SHADOW_INK", "shadowroom.histogram.background", "ui", None, "7px shadow clip marker on the histogram plate"),
        ("ext.clip.marker_highlight_plot", "clipping", "CONST:HIGHLIGHT_INK", "shadowroom.histogram.background", "ui", None, "7px highlight clip marker on the histogram plate"),
        # new warning colour where it is drawn as text and glyphs
        ("ext.warning.background_text", "status", "warning", "background", "text", 11, "Import bands (pulled card, queue) in warning on the window background"),
        ("ext.warning.status_text", "status", "warning", "status_bar.background", "text", 11, "warning-tone text on the status bar"),
        ("ext.warning.elevated_text", "status", "warning", "elevated_surface.background", "text", 11, "warning text in dialogs (Add host, History)"),
        # decision stamp (lights-out): 13px MEDIUM on the scrim at STAMP_PLATE_ALPHA, worst photo
        ("ext.stamp.reject", "stamp", "shadowroom.scrim.reject", "STAMP", "text", 13, "'REJECT' stamp"),
        ("ext.stamp.pick", "stamp", "shadowroom.scrim.pick", "STAMP", "text", 13, "'PICK' stamp"),
        ("ext.stamp.rating", "stamp", "shadowroom.scrim.rating", "STAMP", "text", 13, "'★★★★' stamp"),
        ("ext.stamp.text", "stamp", "shadowroom.scrim.text", "STAMP", "text", 13, "'RESET' / 'UNFLAGGED' / 'NO LABEL' stamp"),
        # floating menu surface (dialog.rs menu_surface): elevated + border.variant + shadow
        ("ext.menu.surface_vs_canvas", "image_menu", "elevated_surface.background", "shadowroom.canvas", "info", None, "menu surface vs the grid canvas (shadow adds separation)"),
        ("ext.menu.surface_vs_panel", "image_menu", "elevated_surface.background", "panel.background", "info", None, "menu surface vs a panel"),
        ("ext.menu.outline_vs_canvas", "image_menu", "border.variant", "shadowroom.canvas", "info", None, "menu hairline vs the grid canvas"),
        ("ext.menu.destructive_icon_selected", "image_menu", "error", sel, "ui", None, "warning icon of the highlighted 'Delete Permanently…' row (keeps error)"),
        # palette match highlight (MEDIUM + text-coloured underline)
        ("ext.palette.match_underline", "selected", "text", "elevated_surface.background", "ui", None, "1px text-coloured underline under matched letters"),
        ("ext.palette.match_underline_selected", "selected", "text", sel, "ui", None, "match underline on the active row"),
        ("ext.palette.description_selected", "selected", "text.muted", sel, "text", 11, "inline description on the active row"),
        # Import per-card results
        ("ext.import.failed_chip_text", "import", "shadowroom.scrim.reject", "PLATE", "text", 11, "'Failed' tile chip: 11px scrim.reject text on the scrim plate"),
        ("ext.import.verified_icon", "import", "success", "PLATE", "info", None, "'Verified' chip check icon in the theme success colour on the photo scrim (text carries it)"),
        # status bar theme icon
        ("ext.status.theme_icon", "icons", "icon.muted", "status_bar.background", "ui", None, "theme icon (the theme's only status-bar presence; name in tooltip)"),
    ]
    for name in LABELS:
        l = name.lower()
        rows.append((f"ext.stamp.label.{l}", "stamp", f"shadowroom.scrim.label.{l}", "STAMP", "text", 13, f"'{name.upper()}' stamp"))
        rows.append((f"ext.menu.label_initial.{l}", "image_menu", "shadowroom.label.text", f"shadowroom.label.{l}", "text", 11, "image-menu label chip initial (r5)"))
    return rows


# ----------------------------------------------------------------- measurement
def resolve(spec, pal, consts):
    if isinstance(spec, tuple):
        if spec[0] == "over":
            return over(resolve(spec[1], pal, consts), resolve(spec[2], pal, consts))
        if spec[0] == "alpha":
            c = resolve(spec[1], pal, consts)
            return c[:3] + (spec[2],)
    if spec.startswith("TEXT_ON:"):
        return text_on(resolve(spec[8:], pal, consts))
    if spec.startswith("CONST:"):
        return parse_hex(consts[spec[6:]])
    if spec.startswith("#"):
        return parse_hex(spec)
    return parse_hex(pal[spec])


def opaque(spec, pal, consts):
    c = resolve(spec, pal, consts)
    return over(c, parse_hex(pal["background"])) if c[3] < 1 else c


def minimum(kind):
    return {"text": 4.5, "ui": 3.0, "state": 3.0}.get(kind)


def lab(spec):
    if spec is None:
        return "-"
    if isinstance(spec, tuple):
        if spec[0] == "over":
            return f"{lab(spec[1])} over {lab(spec[2])}"
        return f"{lab(spec[1])}@{int(spec[2] * 100)}%"
    return spec


def missing_const(spec, consts):
    specs = spec if isinstance(spec, tuple) else (spec,)
    return any(isinstance(s, str) and s.startswith("CONST:") and s[6:] not in consts for s in specs)


def measure(pid, cat, fg, bg, kind, size, where, pal, consts, note=None, theme=""):
    base = dict(theme=theme, id=pid, category=cat, fg=lab(fg), bg=lab(bg), kind=kind, size_px=size, where=where)
    if note:
        base["site_note"] = note
    if fg is None or bg is None or missing_const(fg, consts) or missing_const(bg, consts):
        return dict(base, ratio=None, required=minimum(kind), result="n/a", reason="not drawn at this commit")
    need = minimum(kind)
    if bg in ("PLATE", "STAMP"):
        plate = parse_hex(pal.get("shadowroom.scrim", "#0f0f0fd0"))
        if bg == "STAMP":
            plate = plate[:3] + (float(consts.get("STAMP_PLATE_ALPHA", plate[3])),)
        worst = None
        for photo in ("#ffffff", "#000000", "#808080"):
            b = over(plate, parse_hex(photo))
            f = resolve(fg, pal, consts)
            if f[3] < 1:
                f = over(f, b)
            r = ratio(f, b)
            if worst is None or r < worst[0]:
                worst = (r, photo, b, f)
        r, photo, b, f = worst
        plate_name = (f"stamp plate {to_hex(plate)}@{plate[3]:.2f}" if bg == "STAMP"
                      else f'plate {pal.get("shadowroom.scrim", "#0f0f0fd0")}')
        return dict(base, bg=f"{plate_name} over {photo} photo",
                    fg_hex=to_hex(f), bg_hex=to_hex(b), ratio=round(r, 2), required=need,
                    result=("n/a" if need is None else ("PASS" if r >= need else "FAIL")))
    b = opaque(bg, pal, consts)
    f = resolve(fg, pal, consts)
    if f[3] < 1:
        f = over(f, b)
    r = ratio(f, b)
    return dict(base, fg_hex=to_hex(f), bg_hex=to_hex(b), ratio=round(r, 2), required=need,
                result=("n/a" if need is None else ("PASS" if r >= need else "FAIL")))


# ----------------------------------------------------------------- colour vision (Machado 2009)
MACHADO = {
    "protan": [[0.152286, 1.052583, -0.204868], [0.114503, 0.786281, 0.099216], [-0.003882, -0.048116, 1.051998]],
    "deutan": [[0.367322, 0.860646, -0.227968], [0.280085, 0.672501, 0.047413], [-0.011820, 0.042940, 0.968881]],
    "tritan": [[1.255528, -0.076749, -0.178779], [-0.078411, 0.930809, 0.147602], [0.004733, 0.691367, 0.303900]],
}
CVD_MIN = 12.0
VISIONS = ("normal", "protan", "deutan", "tritan")


def unlin(v):
    v = min(max(v, 0.0), 1.0)
    return 12.92 * v if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055


def simulate(c, kind):
    if kind == "normal":
        return c[:3]
    l = [lin(v) for v in c[:3]]
    m = MACHADO[kind]
    return tuple(unlin(sum(m[i][j] * l[j] for j in range(3))) for i in range(3))


def to_lab(c):
    r, g, b = (lin(v) for v in c[:3])
    x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047
    y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883
    f = lambda t: t ** (1 / 3) if t > 216 / 24389 else (24389 / 27 * t + 16) / 116
    fx, fy, fz = f(x), f(y), f(z)
    return 116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)


def de2000(c1, c2):
    L1, a1, b1 = to_lab(c1)
    L2, a2, b2 = to_lab(c2)
    C1, C2 = math.hypot(a1, b1), math.hypot(a2, b2)
    Cm = (C1 + C2) / 2
    G = 0.5 * (1 - math.sqrt(Cm ** 7 / (Cm ** 7 + 25 ** 7)))
    a1p, a2p = (1 + G) * a1, (1 + G) * a2
    C1p, C2p = math.hypot(a1p, b1), math.hypot(a2p, b2)
    h1p = math.degrees(math.atan2(b1, a1p)) % 360
    h2p = math.degrees(math.atan2(b2, a2p)) % 360
    dLp, dCp = L2 - L1, C2p - C1p
    dh = h2p - h1p
    if C1p * C2p == 0:
        dh = 0
    elif dh > 180:
        dh -= 360
    elif dh < -180:
        dh += 360
    dHp = 2 * math.sqrt(C1p * C2p) * math.sin(math.radians(dh / 2))
    Lpm, Cpm = (L1 + L2) / 2, (C1p + C2p) / 2
    if C1p * C2p == 0:
        hpm = h1p + h2p
    elif abs(h1p - h2p) <= 180:
        hpm = (h1p + h2p) / 2
    else:
        hpm = (h1p + h2p + 360) / 2 if h1p + h2p < 360 else (h1p + h2p - 360) / 2
    T = (1 - 0.17 * math.cos(math.radians(hpm - 30)) + 0.24 * math.cos(math.radians(2 * hpm))
         + 0.32 * math.cos(math.radians(3 * hpm + 6)) - 0.20 * math.cos(math.radians(4 * hpm - 63)))
    dtheta = 30 * math.exp(-(((hpm - 275) / 25) ** 2))
    Rc = 2 * math.sqrt(Cpm ** 7 / (Cpm ** 7 + 25 ** 7))
    Sl = 1 + 0.015 * (Lpm - 50) ** 2 / math.sqrt(20 + (Lpm - 50) ** 2)
    Sc, Sh = 1 + 0.045 * Cpm, 1 + 0.015 * Cpm * T
    Rt = -math.sin(math.radians(2 * dtheta)) * Rc
    return math.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2 + Rt * (dCp / Sc) * (dHp / Sh))


def cvd_row(set_name, kind, a_name, b_name, a_hex, b_hex):
    de = de2000(simulate(parse_hex(a_hex), kind), simulate(parse_hex(b_hex), kind))
    return dict(set=set_name, vision=kind, a=a_name, b=b_name, delta_e2000=round(de, 1),
                result="PASS" if de >= CVD_MIN else "FAIL")


def colour_vision(label, themes, desk_src):
    """Label palettes drawn at this commit, plus decision / ledger / hue pairs."""
    n = ROUND[label]
    sets = {}
    if n == 0:
        sets["labels (hard-coded, both themes)"] = {
            name: "#" + h for name, h in re.findall(r"ColorLabel::(\w+) => Some\(0x([0-9a-fA-F]{6})\)", desk_src)}
    else:
        for tname, pal in themes.items():
            sets[f"labels chrome {tname.split()[-1]}"] = {name: pal[f"shadowroom.label.{name.lower()}"] for name in LABELS}
        pal = next(iter(themes.values()))
        sets["labels on-photo scrim"] = {name: pal[f"shadowroom.scrim.label.{name.lower()}"] for name in LABELS}
    out = []
    for kind in VISIONS:
        for sname, d in sets.items():
            names = list(d)
            for i in range(len(names)):
                for j in range(i + 1, len(names)):
                    out.append(cvd_row(sname, kind, names[i], names[j], d[names[i]], d[names[j]]))
        for tname, pal in themes.items():
            for a, b in (("shadowroom.pick", "shadowroom.reject"), ("shadowroom.rating", "shadowroom.pick"), ("shadowroom.rating", "shadowroom.reject")):
                out.append(cvd_row(tname, kind, a.split(".")[1], b.split(".")[1], pal[a], pal[b]))
    if n >= 3:
        reviewed = "shadowroom.ledger.reviewed" if n >= 5 else "icon.muted"
        for kind in VISIONS:
            for tname, pal in themes.items():
                for a, b in (("shadowroom.pick", "shadowroom.reject"), ("shadowroom.pick", reviewed), ("shadowroom.reject", reviewed)):
                    out.append(cvd_row(f"ledger {tname}", kind, a.split(".")[-1], b.split(".")[-1], pal[a], pal[b]))
    return out


def hue_pairs(label, themes):
    """r5+: colours that must not be mistaken for each other (informational,
    not part of the colour-vision pass count)."""
    if ROUND[label] < 5:
        return []
    out = []
    for kind in VISIONS:
        for tname, pal in themes.items():
            for a, b in (("warning", "shadowroom.rating"), ("shadowroom.label.blue", "text.accent"),
                         ("shadowroom.label.blue", "shadowroom.label.purple")):
                out.append(cvd_row(f"hue {tname}", kind, a, b, pal[a], pal[b]))
    return out


# ----------------------------------------------------------------- scoring and runs
def score(rs):
    app = [r for r in rs if r["result"] in ("PASS", "FAIL")]
    p = sum(r["result"] == "PASS" for r in app)
    return dict(passed=p, applicable=len(app), score_pct=round(100 * p / len(app), 1) if app else None)


def lenient(rs):
    return score([dict(r, result="n/a") if r["id"] in LENIENT_INFO else r for r in rs])


def run_one(label, sha, themes, desk_src, consts):
    hard_labels = {name: "#" + h for name, h in re.findall(r"ColorLabel::(\w+) => Some\(0x([0-9a-fA-F]{6})\)", desk_src)}
    rows, ext = [], []
    n = ROUND[label]
    site = sites(label)
    for tname, pal in themes.items():
        for pid, cat, fg, bg, kind, size, where in CORE:
            o = site.get(pid, {})
            rows.append(measure(pid, cat, o.get("fg", fg), o.get("bg", bg), o.get("kind", kind), o.get("size", size),
                                where, pal, consts, o.get("site_note"), tname))
        for name in LABELS:
            l = name.lower()
            if n == 0:
                sw = hard_labels[name]
                rows.append(measure(f"label.{l}.panel", "labels", sw, "panel.background", "ui", None,
                                    "inspector swatch / sidebar chip", pal, consts, None, tname))
                rows.append(measure(f"label.{l}.shadowroom", "labels", sw, "shadowroom.canvas", "ui", None,
                                    "thumbnail label bar (3-4px) on canvas", pal, consts, None, tname))
                continue
            rows.append(measure(f"label.{l}.panel", "labels", f"shadowroom.label.{l}", "panel.background",
                                "ui", None, "inspector swatch / sidebar chip", pal, consts, "theme label token", tname))
            if n >= 3:
                rows.append(measure(f"label.{l}.shadowroom", "labels", "shadowroom.scrim.label.text", f"shadowroom.scrim.label.{l}",
                                    "text", 11 if n >= 4 else 10, "thumbnail label chip: near-black initial on the filled chip", pal, consts,
                                    "r3 7e39a5e filled chip (label_badge r4 desktop.rs:8807)", tname))
            else:
                rows.append(measure(f"label.{l}.shadowroom", "labels", f"shadowroom.scrim.label.{l}", "PLATE",
                                    "text", 10, "thumbnail label badge: initial on scrim plate", pal, consts,
                                    "bar replaced by 18px badge with initial (label_badge desktop.rs:8364)", tname))
        for pid, cat, fg, bg, kind, size, where in extended(label):
            ext.append(measure(pid, cat, fg, bg, kind, size, where, pal, consts, None, tname))
    return rows, ext


def short(r):
    return f'{r["theme"].replace("Shadowroom ", "")}:{r["id"]} {r["ratio"]}<{r["required"]}'


def main(argv):
    global REPO
    REPO = Path(argv[1]) if len(argv) > 1 else Path(".")
    out_dir = Path(argv[2]) if len(argv) > 2 else Path("contrast-out")
    out_dir.mkdir(parents=True, exist_ok=True)
    runs = [(label, sha, ("dark", "light")) for label, sha in VERSIONS]
    runs += [(label, sha, ("high_contrast_dark", "high_contrast_light")) for label, sha, _ in HIGH_CONTRAST]
    summary = {}
    for label, sha, fns in runs:
        theme_src = git_show(sha, "src/theme.rs")
        desk_src = git_show(sha, "src/desktop.rs")
        consts = code_constants(sha) if ROUND[label] >= 5 else {}
        themes = {THEME_FNS[f]: theme_block(theme_src, f) for f in fns}
        rows, ext = run_one(label, sha, themes, desk_src, consts)
        cvd = colour_vision(label, themes, desk_src)
        hues = hue_pairs(label, themes)
        per_theme = {t: score([r for r in rows if r["theme"] == t]) for t in themes}
        per_theme_lenient = {t: lenient([r for r in rows if r["theme"] == t]) for t in themes}
        per_theme_ext = {t: score([r for r in ext if r["theme"] == t]) for t in themes}
        cv_fail = [c for c in cvd if c["result"] == "FAIL"]
        strict_rows = [dict(r, result=("PASS" if r["ratio"] is not None and r["ratio"] >= 3.0 else "FAIL"))
                       if r["id"].startswith("state.selected_vs") else r for r in rows]
        doc = dict(commit=sha, label=label,
                   method="WCAG 2.2 luminance; translucent layers composited over their surface; "
                          "photo plates measured over white/black/grey and worst case kept; CVD Machado 2009 sev 1.0, CIEDE2000 >= 12",
                   palettes=themes, code_constants=consts, core=score(rows), core_lenient_outline=lenient(rows),
                   core_strict_state=score(strict_rows), core_per_theme=per_theme, core_lenient_per_theme=per_theme_lenient,
                   extended=score(ext), extended_per_theme=per_theme_ext,
                   pairs=rows, extended_pairs=ext, colour_vision=cvd,
                   colour_vision_summary=dict(total=len(cvd), fail=len(cv_fail)), hue_separation=hues)
        (out_dir / f"contrast-{label}.json").write_text(json.dumps(doc, indent=2))
        summary[label] = dict(commit=sha, core=doc["core"], core_lenient_outline=doc["core_lenient_outline"],
                              core_strict_state=doc["core_strict_state"], core_per_theme=per_theme,
                              extended=doc["extended"], extended_per_theme=per_theme_ext, cvd=doc["colour_vision_summary"],
                              core_fails=[short(r) for r in rows if r["result"] == "FAIL"],
                              ext_fails=[short(r) for r in ext if r["result"] == "FAIL"],
                              cvd_fails=[f'{c["set"]} {c["vision"]} {c["a"]}/{c["b"]} dE={c["delta_e2000"]}' for c in cv_fail])
    (out_dir / "contrast-summary.json").write_text(json.dumps(summary, indent=2))
    for label, s in summary.items():
        print(f"\n=== {label} ({s['commit']}) core {s['core']} lenient {s['core_lenient_outline']['score_pct']} "
              f"strict {s['core_strict_state']['score_pct']} ext {s['extended']} cvd {s['cvd']}")
        print("  core fails:", *s["core_fails"], sep="\n    ")
        print("  ext fails:", *s["ext_fails"], sep="\n    ")
        print("  cvd fails:", *s["cvd_fails"], sep="\n    ")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
