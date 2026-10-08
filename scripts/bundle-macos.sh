#!/usr/bin/env bash
# Build the GUI and CLI in release mode and wrap the GUI in <out>/lanlink.app (ad-hoc
# signed). The CLI binary is copied next to it as <out>/lanlink.
#
#   scripts/bundle-macos.sh [--target <triple>] [--out <dir>] [--locked]
#
# --target  e.g. aarch64-apple-darwin or x86_64-apple-darwin. Default: the host.
# --out     output directory. Default: dist.
# --locked  passed to cargo (CI uses it).
#
# The bundle version comes from LANLINK_VERSION (set by the release workflow, also baked
# into the binary), else from Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")/.."

target=""
out=dist
cargo_flags=()
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --locked) cargo_flags+=(--locked); shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

bin_dir=target/release
if [ -n "$target" ]; then
  cargo_flags+=(--target "$target")
  bin_dir="target/$target/release"
fi

# The ${a[@]+...} form keeps bash 3.2 (macOS /bin/bash) happy with an empty array under set -u.
cargo build ${cargo_flags[@]+"${cargo_flags[@]}"} --release -p lanlink-app -p lanlink-cli

version="${LANLINK_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
# CFBundleShortVersionString must be numeric ("0.2.0"); CFBundleVersion keeps the full
# version so nightlies stay distinguishable in Finder's Get Info.
short_version="${version%%-*}"

app="$out/lanlink.app"
rm -rf "$app" "$out/lanlink"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin_dir/lanlink-app" "$app/Contents/MacOS/lanlink"
cp "$bin_dir/lanlink" "$out/lanlink"
cp assets/icon.icns "$app/Contents/Resources/lanlink.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>lanlink</string>
  <key>CFBundleDisplayName</key><string>lanlink</string>
  <key>CFBundleIdentifier</key><string>dev.lanlink.app</string>
  <key>CFBundleExecutable</key><string>lanlink</string>
  <key>CFBundleIconFile</key><string>lanlink</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${short_version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep -s - "$app"
codesign --force -s - "$out/lanlink"
echo "built $app and $out/lanlink ($version)"
