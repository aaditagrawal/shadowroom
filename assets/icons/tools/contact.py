"""Build the icon review contact sheet (SVG; rasterize with magick at -density 96/192).

Usage: python3 -I contact.py ICON_DIR OUT.svg

Glyphs are inlined at integer offsets so a 96-dpi render shows true 1x pixels.
Colors are the app's theme fallbacks plus the accent seen in the Linux screenshot.
"""
import html, os, re, sys

BASE, OUT = sys.argv[1], sys.argv[2]
BG, PANEL, BORDER = "#0f0f0f", "#141414", "#262626"
ICON, MUTED, TEXT, LABEL = "#b8b8b8", "#8a8a8a", "#d7dce2", "#6f6f6f"
HOVER, SELECTED, ACCENT = "#1b1b1b", "#232323", "#5b9bd5"
PICK, REJECT, RATING, CLOSE_HOVER = "#7fbf7f", "#d46a6a", "#e0b341", "#d46a6a33"
MONO = "DM Mono, monospace"
SANS = "DM Sans, sans-serif"

def inner(file):
    src = open(os.path.join(BASE, file)).read()
    vb = re.search(r'viewBox="([^"]+)"', src).group(1)
    return vb, re.sub(r"^.*?<svg[^>]*>|</svg>\s*$", "", src, flags=re.S)

def glyph(file, x, y, color, size=16, opacity=1.0):
    vb, body = inner(file)
    op = f' opacity="{opacity}"' if opacity < 1 else ""
    return f'<svg x="{x}" y="{y}" width="{size}" height="{size}" viewBox="{vb}" fill="{color}" color="{color}"{op}>{body}</svg>'

def text(x, y, s, color=LABEL, size=10, font=MONO, anchor="start", weight=400):
    return (f'<text x="{x}" y="{y}" fill="{color}" font-family="{font}" font-size="{size}" '
            f'font-weight="{weight}" text-anchor="{anchor}">{html.escape(s)}</text>')

def rect(x, y, w, h, fill, stroke=None):
    s = f' stroke="{stroke}"' if stroke else ""
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{fill}"{s}/>'

# key, file, role/tooltip, semantic color for the "on" state (None = accent)
ROLES = [
    ("Window", [
        ("win-min", "shadowroom/window-minimize.svg", "Minimize", None),
        ("win-max", "shadowroom/window-maximize.svg", "Maximize", None),
        ("win-restore", "shadowroom/window-restore.svg", "Restore", None),
        ("win-close", "shadowroom/window-close.svg", "Close", None),
    ]),
    ("Views", [
        ("grid", "shadowroom/view-grid.svg", "Grid view · G", None),
        ("loupe", "shadowroom/view-loupe.svg", "Loupe view · L", None),
        ("compare", "shadowroom/view-compare.svg", "Compare view · C", None),
        ("zoom", "zoom-in.svg", "View at 100% · Z", None),
        ("focus", "screen-full.svg", "Focus mode · Tab", None),
    ]),
    ("Review", [
        ("pick", "pass.svg", "Pick · P", PICK),
        ("pick-on", "pass-filled.svg", "Picked", PICK),
        ("reject", "circle-slash.svg", "Reject · X", REJECT),
        ("reject-on", "shadowroom/reject-filled.svg", "Rejected", REJECT),
        ("unflagged", "circle-large.svg", "Unreviewed / Clear flag · U", None),
        ("star", "star-empty.svg", "Rate · 1–5", RATING),
        ("star-full", "star-full.svg", "Rated", RATING),
        ("label", "tag.svg", "Color label", None),
    ]),
    ("Navigate", [
        ("previous", "arrow-left.svg", "Previous", None),
        ("next", "arrow-right.svg", "Next", None),
        ("auto-advance", "shadowroom/auto-advance.svg", "Auto-advance after decision · ⇧A", None),
        ("undo", "discard.svg", "Undo · Ctrl+Z", None),
        ("redo", "redo.svg", "Redo · Ctrl+Shift+Z", None),
        ("pin", "pinned.svg", "Pin as compare reference · B", None),
        ("sync-pan", "link.svg", "Synced pan", None),
    ]),
    ("Selection", [
        ("select", "shadowroom/checkbox.svg", "Select", None),
        ("selected", "shadowroom/checkbox-checked.svg", "Selected", None),
    ]),
    ("Library", [
        ("library", "library.svg", "All photos", None),
        ("folder", "folder.svg", "Folder", None),
        ("folder-opened", "folder-opened.svg", "Open folder…", None),
        ("drive", "shadowroom/drive.svg", "Drive", None),
        ("add", "add.svg", "Add folder", None),
        ("group", "layers.svg", "Burst / similar stack", None),
        ("duplicate", "shadowroom/duplicate.svg", "Exact duplicates", None),
        ("trash", "trash.svg", "Move to Trash / Trash", None),
        ("restore", "shadowroom/trash-restore.svg", "Restore from Trash", None),
    ]),
    ("Output", [
        ("export", "export.svg", "Export picks…", None),
        ("xmp", "file-code.svg", "Export XMP sidecars…", None),
        ("save", "save.svg", "Retry unsaved decisions", None),
        ("external", "link-external.svg", "Show in Files", None),
        ("refresh", "refresh.svg", "Refresh catalog", None),
        ("stop", "stop-circle.svg", "Stop indexing", None),
        ("warning", "warning.svg", "Not saved / error", None),
    ]),
    ("Panels", [
        ("panel-left", "shadowroom/panel-left.svg", "Toggle library", None),
        ("panel-right", "shadowroom/panel-right.svg", "Toggle inspector", None),
        ("histogram", "shadowroom/histogram.svg", "Toggle histogram · H", None),
        ("settings", "gear.svg", "Preferences · Ctrl+,", None),
        ("search", "search.svg", "Command palette · Ctrl+K", None),
        ("filter", "list-filter.svg", "Minimum rating", None),
    ]),
]

W = 1180
out, y = [], 0

def header(title, sub=""):
    global y
    y += 34
    out.append(text(24, y, title, TEXT, 14, SANS, weight=500))
    if sub:
        out.append(text(24, y + 16, sub, LABEL, 10))
        y += 16
    y += 14

# --- title bar strip, true size --------------------------------------------
out.append(text(24, 30, "Shadowroom icon set — review sheet", TEXT, 18, SANS, weight=500))
out.append(text(24, 48, "16px glyphs at 1x (96 dpi render). Codicons CC BY 4.0, unmodified, except the shadowroom/ originals; trash-restore adapts Codicons trash.", LABEL, 10))
y = 56
header("Title bar · Linux client decorations", "34px bar, 36px buttons, icon #b8b8b8; hover on maximize, close hover tinted red. Second row: maximized state.")
for state, maxkey in ((0, "shadowroom/window-maximize.svg"), (1, "shadowroom/window-restore.svg")):
    by = y
    out.append(rect(24, by, W - 48, 34, PANEL))
    out.append(rect(24, by + 34, W - 48, 1, BORDER))
    out.append(text(40, by + 21, "shadowroom-test-shoot", TEXT, 13, SANS))
    for i, (lab, on) in enumerate((("Grid", 0), ("Loupe", 1), ("Compare", 0))):
        tx = 210 + i * 64
        if on:
            out.append(rect(tx - 8, by + 5, 56, 24, SELECTED))
        out.append(text(tx, by + 21, lab, TEXT if on else MUTED, 13, SANS))
    sx = W - 24 - 3 * 36 - 120
    out.append(glyph("search.svg", sx, by + 9, MUTED))
    out.append(text(sx + 24, by + 21, "Ctrl+K", MUTED, 12, SANS))
    for j, f in enumerate(("shadowroom/window-minimize.svg", maxkey, "shadowroom/window-close.svg")):
        bx = W - 24 - (3 - j) * 36
        if j == 1 and state == 0:
            out.append(rect(bx, by, 36, 34, HOVER))
        if j == 2 and state == 1:
            out.append(f'<rect x="{bx}" y="{by}" width="36" height="34" fill="#d46a6a" fill-opacity="0.2"/>')
        out.append(glyph(f, bx + 10, by + 9, ICON))
    y += 42
y += 4

# --- role table --------------------------------------------------------------
header("Roles and states", "key → file · default #8a8a8a · hover · active (accent on selected) · disabled 40% · semantic on-state · 2x")
COLS = [("default", 300), ("hover", 340), ("active", 380), ("disabled", 420), ("on", 460), ("2x", 500)]
colw = (W - 48) // 2
for group, items in ROLES:
    out.append(text(24, y + 12, group.upper(), "#9a9a9a", 10, MONO, weight=500))
    y += 20
    for idx, (key, f, tip, sem) in enumerate(items):
        col, row = idx % 2, idx // 2
        x0, ry = 24 + col * colw, y + row * 30
        out.append(text(x0, ry + 13, key, TEXT, 11))
        out.append(text(x0 + 100, ry + 13, f.replace("shadowroom/", "sr/"), LABEL, 9))
        out.append(text(x0 + 100, ry + 24, tip, "#5a5a5a", 9))
        for name, cx in COLS:
            gx = x0 + cx
            if name == "hover":
                out.append(rect(gx - 4, ry - 1, 24, 24, HOVER))
                out.append(glyph(f, gx, ry + 3, ICON))
            elif name == "active":
                out.append(rect(gx - 4, ry - 1, 24, 24, SELECTED))
                out.append(glyph(f, gx, ry + 3, ACCENT))
            elif name == "disabled":
                out.append(glyph(f, gx, ry + 3, MUTED, opacity=0.4))
            elif name == "on":
                out.append(glyph(f, gx, ry + 3, sem or ACCENT))
            elif name == "2x":
                out.append(glyph(f, gx, ry - 5, ICON, 32))
            else:
                out.append(glyph(f, gx, ry + 3, MUTED))
    y += ((len(items) + 1) // 2) * 30 + 10

# --- 12px badges on a thumbnail ---------------------------------------------
header("Thumbnail badges · 12px glyphs on a 12-unit grid", "filmstrip/grid overlays on a photo; badge chip #0f0f0f at 80%")
tx = 24
for f, color, lab in (("pass-filled-compact.svg", PICK, "pick"), ("reject-filled-compact.svg", REJECT, "reject"),
                      ("shadowroom/layers-compact.svg", ICON, "stack"), ("circle-slash-compact.svg", REJECT, "reject outline")):
    path = f if os.path.exists(os.path.join(BASE, f)) else "shadowroom/" + f
    out.append(f'<rect x="{tx}" y="{y}" width="112" height="72" fill="#3b4a3f"/>')
    out.append(f'<rect x="{tx}" y="{y + 40}" width="112" height="32" fill="#56634f"/>')
    out.append(f'<rect x="{tx + 4}" y="{y + 4}" width="18" height="18" fill="#0f0f0f" fill-opacity="0.8"/>')
    out.append(glyph(path, tx + 7, y + 7, color, 12))
    out.append(text(tx, y + 86, lab, LABEL, 9))
    tx += 128
y += 96

# --- app mark ------------------------------------------------------------------
header("Application mark · assets/icons/shadowroom-app.svg (+ shadowroom/shadowroom-symbolic.svg)",
       "64-unit grid: crisp at 16 / 32 / 48 / 64 / 128; shown on light and dark docks")
_, app = inner("shadowroom-app.svg")
ax = 24
for dock in ("#2a2a2a", "#e8e8e8"):
    out.append(rect(ax, y, 420, 140, dock))
    px = ax + 12
    for s in (16, 32, 48, 64, 128):
        out.append(f'<svg x="{px}" y="{y + 6 + (128 - s)}" width="{s}" height="{s}" viewBox="0 0 1024 1024">{app}</svg>')
        px += s + 14
    ax += 436
out.append(glyph("shadowroom/shadowroom-symbolic.svg", ax + 8, y + 8, ICON))
out.append(glyph("shadowroom/shadowroom-symbolic.svg", ax + 36, y + 8, ICON, 32))
out.append(text(ax + 8, y + 64, "symbolic", LABEL, 9))
y += 150

# --- before / after ---------------------------------------------------------------
header("Role fixes · current → proposed")
FIXES = [("undo", "history.svg", "discard.svg"), ("redo", "history.svg", "redo.svg"),
         ("auto-advance", "arrow-right.svg", "shadowroom/auto-advance.svg"),
         ("prev unreviewed", "arrow-right.svg", "arrow-left.svg"),
         ("grid", "layout.svg", "shadowroom/view-grid.svg"), ("loupe", "list-selection.svg", "shadowroom/view-loupe.svg"),
         ("compare", "split-horizontal.svg", "shadowroom/view-compare.svg"), ("burst", "group-by-ref-type.svg", "layers.svg"),
         ("pin", "pin.svg", "pinned.svg"), ("unreviewed", "files.svg", "circle-large.svg"),
         ("drive", "files.svg", "shadowroom/drive.svg"), ("export", "save.svg", "export.svg"),
         ("stop", "close.svg", "stop-circle.svg"), ("clear flag", "close.svg", "circle-large.svg"),
         ("settings", "settings-gear.svg", "gear.svg"), ("xmp", "files.svg", "file-code.svg"),
         ("histogram", "graph.svg", "shadowroom/histogram.svg"), ("win-max", None, "shadowroom/window-maximize.svg")]
for i, (lab, old, new) in enumerate(FIXES):
    x0, ry = 24 + (i % 4) * 284, y + (i // 4) * 34
    out.append(text(x0, ry + 13, lab, TEXT, 10))
    if old and os.path.exists(os.path.join(BASE, old)):
        out.append(glyph(old, x0 + 120, ry + 1, MUTED))
    elif old:
        out.append(text(x0 + 100, ry + 13, "retired", "#5a5a5a", 9))  # removed from the bundle
    else:
        # the former inline WIN_MAX constant: 1px edges on half-pixels
        out.append('<svg x="%d" y="%d" width="16" height="16" viewBox="0 0 16 16" fill="%s">'
                   '<path fill-rule="evenodd" d="M3.5 3.5h9v9h-9zm1 1v7h7v-7z"/></svg>' % (x0 + 120, ry + 1, MUTED))
    out.append(text(x0 + 146, ry + 13, "→", LABEL, 11))
    out.append(glyph(new, x0 + 166, ry + 1, TEXT))
y += ((len(FIXES) + 3) // 4) * 34 + 16

svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{y}" viewBox="0 0 {W} {y}">'
       f'<rect width="{W}" height="{y}" fill="{BG}"/>' + "\n".join(out) + "</svg>\n")
open(OUT, "w").write(svg)
print(f"{OUT}: {W}x{y}")
