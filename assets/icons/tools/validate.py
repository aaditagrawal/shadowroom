"""Validate Shadowroom icon SVGs.

Usage: python3 -I validate.py ICON_DIR [CODICONS_UPSTREAM_ICON_DIR]

Glyphs (everything except shadowroom-app.svg) must be: well-formed XML; root
<svg> with width=16 height=16 viewBox="0 0 16 16"
(12/12 for *-compact badge glyphs); only <svg>/<path>; fill-only
(no stroke); no color other than currentColor; no scripts, styles, hrefs, url(),
images, text or foreignObject. Top-level Codicons must be byte-identical to
upstream when an upstream directory is given. The app mark is checked for
safety (no scripts/refs) and its 64-unit grid, but may use its fixed palette.
"""
import os, re, sys, hashlib
import xml.etree.ElementTree as ET

NS = "{http://www.w3.org/2000/svg}"
APP = "shadowroom-app.svg"
# Still loaded by current desktop.rs keys; replaced by gear.svg / file-code.svg in
# the docs/ICONS.md map. Reported as WARN until the keys are rewired, then delete.
LEGACY_24_GRID = set()  # empty since the 2026-10-18 rewiring
BANNED_TAGS = {"script", "style", "image", "foreignObject", "text", "use", "a", "iframe", "animate", "set"}
COLOR = re.compile(r"#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(")

def check(path, rel, upstream):
    errs = []
    raw = open(path, "rb").read()
    text = raw.decode("utf-8-sig")
    try:
        root = ET.fromstring(text)
    except ET.ParseError as e:
        return [f"not well-formed: {e}"]
    if root.tag != NS + "svg":
        errs.append("root is not <svg> in the SVG namespace")
    low = text.lower()
    for needle in ("href", "url(", "<!entity", "<!doctype", "javascript:", "data:", "@import"):
        if needle in low:
            errs.append(f"forbidden content: {needle}")
    for el in root.iter():
        tag = el.tag.replace(NS, "")
        if tag in BANNED_TAGS:
            errs.append(f"forbidden element <{tag}>")
        for k in el.attrib:
            if k.startswith("on"):
                errs.append(f"event handler attribute {k}")
    name = os.path.basename(path)
    if name == APP:
        if root.get("viewBox") != "0 0 1024 1024":
            errs.append("app mark viewBox must be 0 0 1024 1024")
        for n in re.findall(r"-?\d+(?:\.\d+)?", " ".join(
                (el.get("d") or "") + " " + " ".join(el.get(a, "") for a in ("x", "y", "width", "height"))
                for el in root.iter())):
            if float(n) % 32:
                errs.append(f"app mark coordinate {n} is off the 32-unit grid")
                break
        return errs
    n = "12" if name.endswith("-compact.svg") else "16"  # *-compact: 12px badge grid
    if (root.get("width"), root.get("height"), root.get("viewBox")) != (n, n, f"0 0 {n} {n}"):
        errs.append(f"glyph must be width={n} height={n} viewBox='0 0 {n} {n}'")
    tags = {el.tag.replace(NS, "") for el in root.iter()}
    if tags - {"svg", "path"}:
        errs.append(f"unexpected elements {sorted(tags - {'svg', 'path'})}")
    if "stroke" in low:
        errs.append("stroke found: glyphs are fill-only alpha masks")
    if COLOR.search(text):
        errs.append("literal color found: glyphs must be monochrome currentColor")
    for el in root.iter():
        fill = el.get("fill")
        if fill not in (None, "currentColor"):
            errs.append(f"fill={fill!r}; only currentColor allowed")
    if os.sep not in rel and upstream:
        up = os.path.join(upstream, name)
        if not os.path.exists(up):
            errs.append("top-level glyph is not an upstream Codicon (originals belong in shadowroom/)")
        elif open(up, "rb").read() != raw:
            errs.append("differs from upstream Codicon (top-level Codicons must be unmodified)")
    if rel.startswith("shadowroom" + os.sep) and len(root.findall(NS + "path")) != 1:
        errs.append("Shadowroom glyphs use exactly one <path> so nonzero holes/knockouts combine")
    return errs

def main():
    base = sys.argv[1]
    upstream = sys.argv[2] if len(sys.argv) > 2 else None
    failures, count = 0, 0
    for dirpath, dirs, files in os.walk(base):
        dirs[:] = [d for d in dirs if d != "tools"]  # review artifacts, not glyphs
        for f in sorted(files):
            if not f.endswith(".svg"):
                continue
            path = os.path.join(dirpath, f)
            rel = os.path.relpath(path, base)
            count += 1
            errs = check(path, rel, upstream)
            digest = hashlib.sha256(open(path, "rb").read()).hexdigest()[:12]
            legacy = rel in LEGACY_24_GRID and all("width=" in e for e in errs)
            status = "WARN" if errs and legacy else "FAIL" if errs else "ok  "
            print(f"{status} {rel:40} {digest}")
            for e in errs:
                print(f"     - {e}" + (" (legacy 24-grid upstream file; retire after rewiring)" if legacy else ""))
            failures += bool(errs) and not legacy
    print(f"\n{count} SVGs checked, {failures} failed")
    sys.exit(1 if failures else 0)

if __name__ == "__main__":
    main()
