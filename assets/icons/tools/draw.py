"""Generate Shadowroom's original 16px icons as fill-only SVG paths.

Usage: python3 -I draw.py OUT_DIR   (normally assets/icons/shadowroom)

Conventions (match Codicons 2024+): 16x16 grid, 1px line weight, round caps,
axis-aligned edges on whole pixels so 1x and 2x render crisp. Unions share
clockwise winding; knockouts/holes are counter-clockwise (nonzero fill).
"""
import math, os, sys

def f(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s

def area(pts):
    return sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1])) / 2

def poly(pts, cw=True):
    if (area(pts) > 0) != cw:  # y-down: positive shoelace area == clockwise on screen
        pts = pts[::-1]
    return "M" + "L".join(f"{f(x)} {f(y)}" for x, y in pts) + "Z"

def rrect(x0, y0, x1, y1, r=0.0, cw=True):
    """Rounded rectangle; r=0 gives square corners."""
    if r <= 0:
        return poly([(x0, y0), (x1, y0), (x1, y1), (x0, y1)], cw)
    s = 1 if cw else 0
    if cw:
        return (f"M{f(x0+r)} {f(y0)}H{f(x1-r)}A{f(r)} {f(r)} 0 0 {s} {f(x1)} {f(y0+r)}"
                f"V{f(y1-r)}A{f(r)} {f(r)} 0 0 {s} {f(x1-r)} {f(y1)}H{f(x0+r)}"
                f"A{f(r)} {f(r)} 0 0 {s} {f(x0)} {f(y1-r)}V{f(y0+r)}A{f(r)} {f(r)} 0 0 {s} {f(x0+r)} {f(y0)}Z")
    return (f"M{f(x0+r)} {f(y0)}A{f(r)} {f(r)} 0 0 {s} {f(x0)} {f(y0+r)}V{f(y1-r)}"
            f"A{f(r)} {f(r)} 0 0 {s} {f(x0+r)} {f(y1)}H{f(x1-r)}A{f(r)} {f(r)} 0 0 {s} {f(x1)} {f(y1-r)}"
            f"V{f(y0+r)}A{f(r)} {f(r)} 0 0 {s} {f(x1-r)} {f(y0)}Z")

def frame(x0, y0, x1, y1, r=0.0, w=1.0):
    """1px outline whose outer edge lies on (x0,y0)-(x1,y1)."""
    return rrect(x0, y0, x1, y1, r) + rrect(x0 + w, y0 + w, x1 - w, y1 - w, max(r - w, 0), cw=False)

def circle(cx, cy, r, cw=True):
    s = 1 if cw else 0
    return (f"M{f(cx-r)} {f(cy)}A{f(r)} {f(r)} 0 1 {s} {f(cx+r)} {f(cy)}"
            f"A{f(r)} {f(r)} 0 1 {s} {f(cx-r)} {f(cy)}Z")

def stroke(pts, w=1.0, cw=True):
    """Outline an open polyline: miter joins, round caps, as one closed path."""
    h = w / 2
    def normal(a, b):
        dx, dy = b[0] - a[0], b[1] - a[1]
        l = math.hypot(dx, dy)
        return (-dy / l * h, dx / l * h)
    segs = list(zip(pts, pts[1:]))
    ns = [normal(a, b) for a, b in segs]
    def side(sign):
        out = [(pts[0][0] + sign * ns[0][0], pts[0][1] + sign * ns[0][1])]
        for i in range(1, len(pts) - 1):
            n0, n1 = ns[i - 1], ns[i]
            mx, my = n0[0] + n1[0], n0[1] + n1[1]
            k = (h * h) / (mx * n0[0] + my * n0[1])  # miter scale
            out.append((pts[i][0] + sign * mx * k, pts[i][1] + sign * my * k))
        out.append((pts[-1][0] + sign * ns[-1][0], pts[-1][1] + sign * ns[-1][1]))
        return out
    left, right = side(1), side(-1)
    ring = left + right[::-1]
    # orientation of the ribbon decides which arc sweep bulges outward
    if (area(ring) > 0) != cw:
        left, right = right, left
    s = 1 if cw else 0
    d = "M" + "L".join(f"{f(x)} {f(y)}" for x, y in left)
    d += f"A{f(h)} {f(h)} 0 0 {s} {f(right[-1][0])} {f(right[-1][1])}"
    d += "L" + "L".join(f"{f(x)} {f(y)}" for x, y in right[::-1][1:])
    d += f"A{f(h)} {f(h)} 0 0 {s} {f(left[0][0])} {f(left[0][1])}Z"
    return d

def spline(points, steps=10):
    """Catmull-Rom through points, sampled to a polyline."""
    p = [points[0]] + points + [points[-1]]
    out = []
    for i in range(1, len(p) - 2):
        p0, p1, p2, p3 = p[i - 1], p[i], p[i + 1], p[i + 2]
        for k in range(steps):
            t = k / steps
            out.append(tuple(0.5 * ((2 * p1[j]) + (-p0[j] + p2[j]) * t
                                    + (2 * p0[j] - 5 * p1[j] + 4 * p2[j] - p3[j]) * t * t
                                    + (-p0[j] + 3 * p1[j] - 3 * p2[j] + p3[j]) * t ** 3) for j in (0, 1)))
    out.append(points[-1])
    return out

ICONS = {}
def icon(name, title, size=16):
    def deco(fn):
        ICONS[name] = (title, fn, size)
        return fn
    return deco

# --- Window controls: square corners, 10px optical box (3..13), Zed-like ---
@icon("window-minimize", "Minimize window")
def _(): return [rrect(3, 8, 13, 9)]

@icon("window-maximize", "Maximize window")
def _(): return [frame(3, 3, 13, 13)]

@icon("window-restore", "Restore window")
def _():
    return [frame(3, 5, 11, 13),
            # back window: only the parts not hidden by the front one
            rrect(5, 3, 13, 4), rrect(12, 4, 13, 11), rrect(5, 4, 6, 5), rrect(11, 10, 12, 11)]

@icon("window-close", "Close window")
def _():
    a, b = 3.5, 12.5
    return [stroke([(a, a), (b, b)]), stroke([(b, a), (a, b)])]

# --- Dock toggles (status bar) ---
@icon("panel-left", "Toggle library")
def _(): return [frame(2, 3, 14, 13, 1.5), rrect(6, 4, 7, 12)]

@icon("panel-right", "Toggle inspector")
def _(): return [frame(2, 3, 14, 13, 1.5), rrect(9, 4, 10, 12)]

# --- View modes: one family, tiles inside a 12px box ---
@icon("view-grid", "Grid view")
def _():
    return [frame(x, y, x + 5, y + 5, 1) for x in (2, 9) for y in (2, 9)]

@icon("view-loupe", "Loupe view")
def _():
    # one photograph: frame, a single ridge and the sun; few strokes so 1x stays clean
    return [frame(2, 3, 14, 13, 1.5),
            stroke([(4.5, 10.5), (7.5, 7.5), (10.5, 10.5)]),
            circle(10.5, 6, 1)]

@icon("view-compare", "Compare view")
def _(): return [frame(2, 3, 7, 13, 1), frame(9, 3, 14, 13, 1)]

# --- Review decisions ---
@icon("reject-filled", "Rejected")
def _():
    # same disc as Codicons pass-filled; slash angle matches circle-slash
    return [circle(8, 8, 7), stroke([(4.6, 11.4), (11.4, 4.6)], cw=False)]

@icon("auto-advance", "Auto-advance after a decision")
def _():
    # decide (check) then move (arrow)
    return [stroke([(1.5, 8.5), (3.5, 10.5), (7, 7)]),
            stroke([(8.5, 8.5), (14, 8.5)]),
            stroke([(11.5, 6), (14, 8.5), (11.5, 11)])]

# --- Selection: square family, distinct from circular pick/reject ---
@icon("checkbox", "Not selected")
def _(): return [frame(2, 2, 14, 14, 2)]

@icon("checkbox-checked", "Selected")
def _(): return [rrect(2, 2, 14, 14, 2), stroke([(4.75, 8.25), (7, 10.5), (11.25, 6)], w=1.1, cw=False)]

# --- Library management ---
@icon("duplicate", "Exact duplicate")
def _():
    return [frame(2, 5, 11, 14, 1.5),
            # back copy: visible top and right edges only
            rrect(5, 2, 12.5, 3), rrect(13, 3.5, 14, 11),
            "M12.5 2A1.5 1.5 0 0 1 14 3.5H13A0.5 0.5 0 0 0 12.5 3Z",  # rounded corner (clockwise)
            rrect(5, 3, 6, 5), rrect(11, 10, 13, 11),
            # identical-content mark
            rrect(4.5, 8, 8.5, 9), rrect(4.5, 10, 8.5, 11)]

@icon("trash-restore", "Restore from Trash")
def _():
    # ADAPTED from Codicons `trash` (CC BY 4.0): lid, handle and bin outline are
    # upstream geometry; the two slats are replaced by an up arrow.
    return [stroke([(2, 2.5), (14, 2.5)]),
            "M6 2C6 0.897 6.897 0 8 0C9.103 0 10 0.897 10 2H9C9 1.449 8.551 1 8 1C7.449 1 7 1.449 7 2Z",
            "M2.54 3H3.544L4.346 12.625C4.41 13.396 5.067 14 5.841 14H10.16C10.934 14 11.591 13.396 11.655 12.625"
            "L12.458 3H13.459L12.65 12.708C12.543 13.993 11.449 15 10.159 15H5.84C4.55 15 3.456 13.994 3.349 12.708Z",
            stroke([(8, 11.5), (8, 5.5)]),
            stroke([(6, 7.5), (8, 5.5), (10, 7.5)])]

@icon("drive", "Drive")
def _():
    return [frame(1, 4, 15, 12, 1.5), rrect(3, 8, 7, 9), circle(12, 8.5, 0.75)]

@icon("histogram", "Histogram")
def _():
    # a real photo histogram: dominant peak, a shoulder, falling to the baseline
    curve = spline([(2, 12.5), (4.5, 10), (6.5, 4), (8.5, 9.5), (10.5, 7.5), (14, 12.5)], steps=8)
    return [rrect(1, 13, 15, 14), stroke(curve)]

# --- Application mark, symbolic (monochrome) 16px form of shadowroom-app.svg ---
@icon("shadowroom-symbolic", "Shadowroom")
def _(): return ["M4 4H12V6H6V8H4ZM12 12H4V10H10V8H12ZM7 7H9V9H7Z"]

# --- 12px badges (grid/filmstrip overlays), drawn on a 12-unit grid ---
@icon("reject-filled-compact", "Rejected (badge)", 12)
def _():
    # pairs with Codicons pass-filled-compact: same 12px disc, slash knocked out
    return [circle(6, 6, 6), stroke([(3.4, 8.6), (8.6, 3.4)], w=1.15, cw=False)]

@icon("layers-compact", "Burst stack (badge)", 12)
def _():
    # two stacked frames with a ledge: a stack, not the overlapping "=" pair of duplicate
    return [frame(1, 4, 11, 11, 1), rrect(2.5, 2, 9.5, 3), rrect(4, 0, 8, 1)]

def svg(name, paths, n=16):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{n}" height="{n}" viewBox="0 0 {n} {n}" '
            'fill="currentColor"><path d="' + "".join(paths) + '"/></svg>\n')

if __name__ == "__main__":
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for name, (_, fn, size) in ICONS.items():
        with open(os.path.join(out, name + ".svg"), "w") as fh:
            fh.write(svg(name, fn(), size))
    print("\n".join(ICONS))
