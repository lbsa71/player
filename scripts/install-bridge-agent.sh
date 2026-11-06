#!/bin/bash
# User-space update only: never modifies or reinstalls the HAL driver.
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/build-bridge.sh
destination="$HOME/Applications/AudioBridge.app"
label="se.lbsa71.AudioBridge.agent"
agent="$HOME/Library/LaunchAgents/$label.plist"
if [ -e "$destination" ]; then
    installed_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$destination/Contents/Info.plist")
    [ "$installed_id" = "$label" ] || { echo 'Refusing to replace another app.' >&2; exit 1; }
fi
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
# Give the agent time to finalize its WAV before replacing executable files.
for attempt in {1..50}; do
    pgrep -x AudioBridge >/dev/null || break
    sleep 0.1
done
if pgrep -x AudioBridge >/dev/null; then
    echo 'AudioBridge is still running. Stop it before updating.' >&2; exit 1
fi
mkdir -p "$HOME/Applications" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/AudioBridge"
ditto build/AudioBridge.app "$destination"
codesign --verify --strict "$destination"
python3 - "$destination" "$agent" <<'PY'
import pathlib, plistlib, sys
app, target = sys.argv[1:]
logs = pathlib.Path.home() / 'Library/Logs/AudioBridge'
with open(target, 'wb') as f:
    plistlib.dump(dict(Label='se.lbsa71.AudioBridge.agent',
        ProgramArguments=[app+'/Contents/MacOS/AudioBridge'],
        RunAtLoad=True, KeepAlive=True, ThrottleInterval=30,
        LimitLoadToSessionType='Aqua',
        StandardOutPath=str(logs/'stdout.log'), StandardErrorPath=str(logs/'stderr.log')), f)
PY
chmod 644 "$agent"
launchctl bootstrap "gui/$(id -u)" "$agent"
printf 'Installed independent user agent: %s\n' "$destination"
