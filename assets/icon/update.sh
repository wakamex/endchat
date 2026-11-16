#!/bin/sh
# Make the SVG, Wakterm's outline and the PNGs from make_icon.py.
set -eu
cd "$(dirname "$0")"
command -v magick >/dev/null || { echo "update.sh needs ImageMagick (magick)" >&2; exit 1; }
python3 make_icon.py
for s in 256 128 32; do
    magick -background none -density 384 endchat-icon.svg -resize ${s}x${s} -strip endchat-icon-$s.png
done
