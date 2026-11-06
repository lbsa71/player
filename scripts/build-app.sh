#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
swift build -c release
binary_dir="$(swift build -c release --show-bin-path)"
app_path="$PWD/build/Player.app"
mkdir -p "$app_path/Contents/MacOS" "$app_path/Contents/Resources"
cp "$binary_dir/Player" "$app_path/Contents/MacOS/Player"
cp "$binary_dir/PlayerMCP" "$app_path/Contents/MacOS/PlayerMCP"
cp Resources/Info.plist "$app_path/Contents/Info.plist"
codesign --force --sign "${PLAYER_SIGNING_IDENTITY:--}" --options runtime "$app_path/Contents/MacOS/PlayerMCP"
codesign --force --sign "${PLAYER_SIGNING_IDENTITY:--}" --options runtime "$app_path"
codesign --verify --strict "$app_path"
printf 'Built %s\n' "$app_path"
