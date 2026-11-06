#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/build-app.sh
destination="${PLAYER_INSTALL_DIR:-$HOME/Applications}/Player.app"
if pgrep -x Player >/dev/null; then
    printf 'Quit Player before installing an update.\n' >&2
    exit 1
fi
if [ -e "$destination" ]; then
    installed_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$destination/Contents/Info.plist")
    if [ "$installed_id" != "se.lbsa71.player" ]; then
        printf 'Refusing to replace a different app at %s\n' "$destination" >&2
        exit 1
    fi
fi
mkdir -p "$(dirname "$destination")"
ditto build/Player.app "$destination"
codesign --verify --strict "$destination"
open "$destination"
printf 'Installed %s\n' "$destination"
