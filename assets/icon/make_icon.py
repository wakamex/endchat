"""Write endchat's icon, a spiral for a conversation that never ends, as `endchat-icon.svg` and as
`wakterm-poly.rs`, a harness icon for Wakterm's tab bar (`&[Poly]` on the 120-unit grid of
`wakterm-gui/src/customglyph.rs`). Both come from one outline: a band of width WIDTH around the
spiral r = R0 + B * theta, with round ends, so the picture and the tab icon are the same shape."""
import math

CENTRE, R0, B, WIDTH = (60, 60), 6.0, 4.1, 15.0
START, END, STEP = 1.3, 2 * math.pi * 1.62, 0.12


def point(t, side):
    """The spiral at angle t, moved side * WIDTH / 2 along its outward normal."""
    r = R0 + B * t
    x, y = CENTRE[0] + r * math.cos(t), CENTRE[1] + r * math.sin(t)
    # Tangent of (r cos t, r sin t); the outward normal is it turned a quarter.
    dx, dy = B * math.cos(t) - r * math.sin(t), B * math.sin(t) + r * math.cos(t)
    n = math.hypot(dx, dy)
    return x + side * WIDTH / 2 * dy / n, y - side * WIDTH / 2 * dx / n


def cap(t, flip, ahead):
    """A round end at angle t: half a circle from the band's edge on side flip to the other edge,
    bulging forward along the spiral (ahead = 1) or backward (ahead = -1)."""
    r = R0 + B * t
    cx, cy = CENTRE[0] + r * math.cos(t), CENTRE[1] + r * math.sin(t)
    tx, ty = B * math.cos(t) - r * math.sin(t), B * math.sin(t) + r * math.cos(t)
    a0 = math.atan2(point(t, flip)[1] - cy, point(t, flip)[0] - cx)
    # Of the two half circles, take the one whose middle lies in the bulge's direction.
    turn = max((math.pi, -math.pi), key=lambda s: ahead * (math.cos(a0 + s / 2) * tx + math.sin(a0 + s / 2) * ty))
    return [(cx + WIDTH / 2 * math.cos(a0 + turn * k / 8), cy + WIDTH / 2 * math.sin(a0 + turn * k / 8)) for k in range(1, 8)]


ts = [START + STEP * k for k in range(int((END - START) / STEP) + 1)] + [END]
outline = [point(t, 1) for t in ts] + cap(END, 1, 1) + [point(t, -1) for t in reversed(ts)] + cap(START, -1, -1)
# Wakterm's outline is in whole units, since its coordinates are fractions with a one-byte
# denominator; the SVG keeps a tenth of a unit, so the two differ by at most half a unit.
pts = [(round(x), round(y)) for x, y in outline]
pts = [p for k, p in enumerate(pts) if p != pts[k - 1]]

d = "M " + " L ".join(f"{x:.1f} {y:.1f}" for x, y in outline) + " Z"
with open("endchat-icon.svg", "w") as f:
    f.write('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120">\n')
    f.write("  <!-- endchat: a spiral for a conversation that never ends. Written by make_icon.py; one colour so a tab bar can tint it. -->\n")
    f.write(f'  <path fill="#000" d="{d}"/>\n</svg>\n')

c = lambda x, y: f"BlockCoord::SquareFrac({x}, 120), BlockCoord::SquareFrac({y}, 120)"
with open("wakterm-poly.rs", "w") as f:
    f.write("/// endchat's mark, a spiral for a conversation that never ends: a band around the spiral with\n")
    f.write("/// round ends, written by endchat's assets/icon/make_icon.py with the same outline as its SVG.\n")
    f.write("pub const HARNESS_ICON_ENDCHAT_POLY: &[Poly] = &[Poly {\n    path: &[\n")
    for k, (x, y) in enumerate(pts):
        f.write(f"        PolyCommand::{'MoveTo' if k == 0 else 'LineTo'}({c(x, y)}),\n")
    f.write("        PolyCommand::Close,\n    ],\n    intensity: BlockAlpha::Full,\n    style: PolyStyle::Fill,\n}];\n")
