# endchat icon

`endchat-icon.svg` is the source: a speech bubble whose three dots say the conversation goes on, in one colour on a 120-unit grid so a terminal's tab bar can tint it. It was drawn by hand as SVG, chosen over a bubble with a looping arrow because the dots stay legible at 14 and 16 pixels and the arrow did not.

`update.sh` makes everything else from it with [ImageMagick](https://imagemagick.org/):

- `endchat-icon-256.png`, `-128.png` and `-32.png`, transparent, for anything that takes a picture.
- `wakterm-poly.rs`, the same shape as a harness icon for [Wakterm](https://github.com/wakamex/wakterm)'s tab bar: a `&[Poly]` for `wakterm-gui/src/customglyph.rs`, written by `wakterm_poly.py`. The dots are 16-sided polygons wound against the outline, which Wakterm's winding fill leaves as holes. `wakterm_poly.py` repeats the SVG's numbers, so a change to the drawing goes in both.
