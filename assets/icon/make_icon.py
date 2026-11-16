"""Write endchat's icon, a line standing on a wall: the chat runs down to its end, where every turn
works. Writes `endchat-icon.svg` and `wakterm-poly.rs`, a harness icon for Wakterm's tab bar
(`&[Poly]` on the 120-unit grid of `wakterm-gui/src/customglyph.rs`), from the same rectangles, so
the picture and the tab icon are the same shape."""

# Rounded rectangles (x, y, width, height, corner radius) on a 120-unit square: the line, then the
# wall. They overlap where the line meets the wall; both go clockwise, so a winding fill joins them.
RECTS = [(51, 8, 18, 90, 4), (10, 94, 100, 18, 4)]


def outline(x, y, w, h, r):
    """The rectangle clockwise from its top edge: ('L', x, y) lines and ('Q', cx, cy, x, y) corners."""
    return [
        ("M", x + r, y), ("L", x + w - r, y), ("Q", x + w, y, x + w, y + r),
        ("L", x + w, y + h - r), ("Q", x + w, y + h, x + w - r, y + h),
        ("L", x + r, y + h), ("Q", x, y + h, x, y + h - r),
        ("L", x, y + r), ("Q", x, y, x + r, y),
    ]


shapes = [outline(*r) for r in RECTS]

d = " ".join(" ".join(f"{c[0]} " + " ".join(str(n) for n in c[1:]) for c in s) + " Z" for s in shapes)
with open("endchat-icon.svg", "w") as f:
    f.write('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120">\n')
    f.write("  <!-- endchat: a line standing on a wall, the chat running down to its end. Written by make_icon.py; one colour so a tab bar can tint it. -->\n")
    f.write(f'  <path fill="#000" d="{d}"/>\n</svg>\n')

c = lambda x, y: f"BlockCoord::SquareFrac({x}, 120), BlockCoord::SquareFrac({y}, 120)"
with open("wakterm-poly.rs", "w") as f:
    f.write("/// endchat's mark, a line standing on a wall: the chat running down to its end, where every\n")
    f.write("/// turn works. Written by endchat's assets/icon/make_icon.py from the same rectangles as its SVG;\n")
    f.write("/// the two overlap and go the same way, so the winding fill joins them.\n")
    f.write("pub const HARNESS_ICON_ENDCHAT_POLY: &[Poly] = &[Poly {\n    path: &[\n")
    for s in shapes:
        for cmd in s:
            if cmd[0] == "M":
                f.write(f"        PolyCommand::MoveTo({c(*cmd[1:])}),\n")
            elif cmd[0] == "L":
                f.write(f"        PolyCommand::LineTo({c(*cmd[1:])}),\n")
            else:
                f.write(f"        PolyCommand::QuadTo {{ control: ({c(*cmd[1:3])}), to: ({c(*cmd[3:])}) }},\n")
        f.write("        PolyCommand::Close,\n")
    f.write("    ],\n    intensity: BlockAlpha::Full,\n    style: PolyStyle::Fill,\n}];\n")
