#!/bin/sh
# Make the PNGs and Wakterm's outline from endchat-icon.svg.
set -eu
cd "$(dirname "$0")"
command -v magick >/dev/null || { echo "update.sh needs ImageMagick (magick)" >&2; exit 1; }
for s in 256 128 32; do
    magick -background none -density 384 endchat-icon.svg -resize ${s}x${s} -strip endchat-icon-$s.png
done
python3 wakterm_poly.py > wakterm-poly.rs
