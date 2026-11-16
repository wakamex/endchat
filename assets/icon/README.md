# endchat icon

`make_icon.py` is the source: a line standing on a wall, the chat running down to its end, where every turn works. It writes the shape as `endchat-icon.svg` and as `wakterm-poly.rs` from the same two rounded rectangles. It is one colour so a tab bar can tint it, and stays legible at 14 and 16 pixels on light and dark backgrounds. It was chosen from a sheet of variations on a line meeting a wall (an arrow, falling dots, a taper, bricks and others) for being the clearest at tab size; earlier candidates were a speech bubble, which read as a messaging app, and a spiral, which said "never ends" but not where a turn works.

`update.sh` runs `make_icon.py` and makes PNGs from the SVG with [ImageMagick](https://imagemagick.org/):

- `endchat-icon-256.png`, `-128.png` and `-32.png`, transparent, for anything that takes a picture.
- `wakterm-poly.rs`, the same shape as a harness icon for [Wakterm](https://github.com/wakamex/wakterm)'s tab bar: a `&[Poly]` for `wakterm-gui/src/customglyph.rs` on its 120-unit grid, in whole units because its coordinates are fractions with a one-byte denominator.
