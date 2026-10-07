#!/usr/bin/env bash
# Regenerate assets/icon.{png,ico,icns} from assets/icon.svg.
# Needs: rsvg-convert (brew install librsvg), python3 (Pillow is installed into a temp venv), iconutil (macOS).
set -euo pipefail
cd "$(dirname "$0")/.."
command -v rsvg-convert >/dev/null || { echo "rsvg-convert missing: brew install librsvg" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for s in 16 32 48 64 128 256 512 1024; do
  rsvg-convert -w "$s" -h "$s" assets/icon.svg -o "$tmp/$s.png"
done
cp "$tmp/512.png" assets/icon.png

python3 -m venv "$tmp/venv"
"$tmp/venv/bin/pip" -q install pillow
"$tmp/venv/bin/python" -I - "$tmp" assets/icon.ico <<'PY'
import sys
from PIL import Image
src, out = sys.argv[1], sys.argv[2]
sizes = [16, 32, 48, 256]
imgs = [Image.open(f"{src}/{s}.png").convert("RGBA") for s in sizes]
imgs[-1].save(out, format="ICO", sizes=[(s, s) for s in sizes], append_images=imgs[:-1])
PY

if command -v iconutil >/dev/null; then
  set_dir="$tmp/icon.iconset"
  mkdir "$set_dir"
  for s in 16 32 128 256 512; do
    cp "$tmp/$s.png" "$set_dir/icon_${s}x${s}.png"
    cp "$tmp/$((s * 2)).png" "$set_dir/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$set_dir" -o assets/icon.icns
else
  echo "iconutil not found; skipping icon.icns" >&2
fi
echo "wrote assets/icon.png assets/icon.ico assets/icon.icns"
