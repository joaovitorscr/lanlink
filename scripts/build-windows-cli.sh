#!/usr/bin/env bash
# Cross-compile the CLI for Windows from macOS (needs: brew install mingw-w64).
# The GUI cannot be built this way (gpui needs fxc.exe); CI builds it on Windows.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
target=x86_64-pc-windows-gnu

rustup +stable target add "$target" >/dev/null
cargo +stable build --release -p lanlink-cli --target "$target"
mkdir -p dist
cp "target/$target/release/lanlink.exe" dist/lanlink-cli.exe
echo "built dist/lanlink-cli.exe"
