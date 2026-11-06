#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
swift build -c release
binary_dir="$(swift build -c release --show-bin-path)"
bundle="$PWD/build/AudioBridge.app"
mkdir -p "$bundle/Contents/MacOS"
cp "$binary_dir/AudioBridge" "$bundle/Contents/MacOS/AudioBridge"
cp Resources/AudioBridge-Info.plist "$bundle/Contents/Info.plist"
codesign --force --sign "${PLAYER_SIGNING_IDENTITY:--}" --options runtime --entitlements Resources/AudioBridge.entitlements "$bundle"
codesign --verify --strict "$bundle"
printf 'Built %s\n' "$bundle"
