#!/bin/bash
# pixels.sh OUT.png name=path ...  — real-size rasters (12px badge, 16px 1x, 32px 2x),
# #b8b8b8 on #0f0f0f, enlarged with nearest-neighbour so AA/blur is visible.
set -e
out=$1; shift
work=$(mktemp -d)
i=0
for item in "$@"; do
  name=${item%%=*}; path=${item#*=}
  sed 's/currentColor/#b8b8b8/g' "$path" > "$work/src.svg"
  tiles=()
  for px in 12 16 32; do
    sed -E "s/<svg([^>]*) width=\"[0-9.]+\" height=\"[0-9.]+\"/<svg\1 width=\"$px\" height=\"$px\"/; s/<svg width=\"[0-9.]+\" height=\"[0-9.]+\"/<svg width=\"$px\" height=\"$px\"/" "$work/src.svg" > "$work/s$px.svg"
    magick -background '#0f0f0f' -density 96 "$work/s$px.svg" -flatten -extent ${px}x${px} "$work/r$px.png"
    scale=$(( px == 32 ? 300 : (px == 12 ? 800 : 600) ))
    magick "$work/r$px.png" -filter point -resize ${scale}% -background '#0f0f0f' -gravity center -extent 100x100 "$work/t$px.png"
    tiles+=("$work/t$px.png")
  done
  magick "${tiles[@]}" +append -background '#0f0f0f' -fill '#8a8a8a' -font "$(dirname "$0")/../../fonts/dmmono/DMMono-Regular.ttf" -pointsize 12 label:"$name" -gravity west -append "$work/row$(printf %03d $i).png"
  i=$((i+1))
done
magick montage "$work"/row*.png -tile 4x -geometry +10+8 -background '#0a0a0a' "$out"
rm -rf "$work"
