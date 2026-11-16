"""Print endchat-icon.svg's shape as a Wakterm harness icon: a `&[Poly]` on the 120-unit grid of
`wakterm-gui/src/customglyph.rs`. The bubble's outline goes clockwise and each dot is a 16-sided
polygon going the other way, so a nonzero fill leaves the dots as holes. Keep the numbers in step
with the SVG."""
import math

def p(x, y):
    return f"BlockCoord::SquareFrac({round(x)}, 120), BlockCoord::SquareFrac({round(y)}, 120)"

cmds = [f"PolyCommand::MoveTo({p(26, 12)})"]
# Outline, clockwise: straight edges, rounded corners as quadratic curves, the tail at lower left.
for kind, *pts in [
    ("L", 94, 12), ("Q", 112, 12, 112, 30), ("L", 112, 70), ("Q", 112, 88, 94, 88),
    ("L", 50, 88), ("L", 22, 108), ("L", 30, 88), ("L", 26, 88), ("Q", 8, 88, 8, 70),
    ("L", 8, 30), ("Q", 8, 12, 26, 12),
]:
    if kind == "L":
        cmds.append(f"PolyCommand::LineTo({p(*pts)})")
    else:
        cmds.append(f"PolyCommand::QuadTo {{ control: ({p(pts[0], pts[1])}), to: ({p(pts[2], pts[3])}) }}")
cmds.append("PolyCommand::Close")
# Dots, counter-clockwise.
for cx in (36, 60, 84):
    for k in range(16):
        a = -2 * math.pi * k / 16
        x, y = cx + 9 * math.cos(a), 50 + 9 * math.sin(a)
        cmds.append(f"PolyCommand::{'MoveTo' if k == 0 else 'LineTo'}({p(x, y)})")
    cmds.append("PolyCommand::Close")

print("/// endchat's mark: a speech bubble whose three dots say the conversation goes on, from")
print("/// endchat's assets/icon/endchat-icon.svg (made by its wakterm_poly.py). The dots are holes")
print("/// wound against the outline.")
print("pub const HARNESS_ICON_ENDCHAT_POLY: &[Poly] = &[Poly {")
print("    path: &[")
for c in cmds:
    print(f"        {c},")
print("    ],")
print("    intensity: BlockAlpha::Full,")
print("    style: PolyStyle::Fill,")
print("}];")
