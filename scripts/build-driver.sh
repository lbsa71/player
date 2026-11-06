#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
bundle="$PWD/build/AudioBridge.driver"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
xcrun clang -std=c11 -O2 -Wall -Wextra -Wno-unused-parameter -Werror -fblocks \
    -arch arm64 -arch x86_64 -mmacosx-version-min=14.4 -bundle \
    -framework CoreAudio -framework CoreFoundation Driver/AudioBridge.c \
    -o "$bundle/Contents/MacOS/AudioBridge"
cp Driver/Info.plist "$bundle/Contents/Info.plist"
cp Driver/APPLE-SAMPLE-LICENSE.txt "$bundle/Contents/Resources/APPLE-SAMPLE-LICENSE.txt"
codesign --force --sign "${PLAYER_SIGNING_IDENTITY:--}" "$bundle"
codesign --verify --strict "$bundle"
printf 'Built %s\n' "$bundle"
