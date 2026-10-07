#!/usr/bin/env bash
# Build the GUI in release mode and wrap it in dist/lanlink.app (ad-hoc signed).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p lanlink-app

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=dist/lanlink.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/lanlink-app "$app/Contents/MacOS/lanlink"
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
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep -s - "$app"
echo "built $app"
