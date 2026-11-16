# endchat icon

`make_icon.py` is the source: it computes a spiral, for a conversation that never ends, as a band of even width with round ends, and writes it as `endchat-icon.svg` and as `wakterm-poly.rs`. It is one colour so a tab bar can tint it, and was chosen over a speech bubble, which read as a messaging app, and a prompt chevron with dots, which read as a plain terminal (`>_`). It stays legible at 14 and 16 pixels on light and dark backgrounds.

`update.sh` runs `make_icon.py` and makes PNGs from the SVG with [ImageMagick](https://imagemagick.org/):

- `endchat-icon-256.png`, `-128.png` and `-32.png`, transparent, for anything that takes a picture.
- `wakterm-poly.rs`, the same shape as a harness icon for [Wakterm](https://github.com/wakamex/wakterm)'s tab bar: a `&[Poly]` for `wakterm-gui/src/customglyph.rs` on its 120-unit grid, in whole units because its coordinates are fractions with a one-byte denominator.
