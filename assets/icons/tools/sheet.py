"""Build a review sheet SVG from 16px icon SVG files. Usage: sheet.py OUT.svg cols label=path ..."""
import sys, re, html
out, cols, items = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
CELL_W, CELL_H, PAD = 150, 92, 16
rows = (len(items) + cols - 1) // cols
W, H = PAD * 2 + cols * CELL_W, PAD * 2 + rows * CELL_H
parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">',
         f'<rect width="{W}" height="{H}" fill="#0f0f0f"/>']
for i, item in enumerate(items):
    label, path = item.split('=', 1)
    src = open(path).read()
    inner = re.sub(r'^.*?<svg[^>]*>|</svg>\s*$', '', src, flags=re.S)
    x, y = PAD + (i % cols) * CELL_W, PAD + (i // cols) * CELL_H
    # 1x, 2x and 3x renders on a 16px pixel grid; 1x on integer coordinates
    parts.append(f'<g transform="translate({x},{y})">')
    parts.append(f'<rect x="-0.5" y="-0.5" width="{CELL_W-8}" height="{CELL_H-8}" fill="#161616" stroke="#262626"/>')
    for s, ox, col in ((1, 8, '#b8b8b8'), (2, 32, '#b8b8b8'), (3, 72, '#e6e6e6')):
        parts.append(f'<svg x="{ox}" y="8" width="{16*s}" height="{16*s}" viewBox="0 0 16 16" fill="{col}" color="{col}">{inner}</svg>')
    parts.append(f'<text x="8" y="{CELL_H-16}" fill="#8a8a8a" font-family="DM Mono, monospace" font-size="10">{html.escape(label)}</text>')
    parts.append('</g>')
parts.append('</svg>')
open(out, 'w').write('\n'.join(parts))
