#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/build-driver.sh
stage="$PWD/build/driver-package"
mkdir -p "$stage/root/Library/Audio/Plug-Ins/HAL" "$stage/scripts"
ditto build/AudioBridge.driver "$stage/root/Library/Audio/Plug-Ins/HAL/AudioBridge.driver"
cp scripts/driver-preinstall "$stage/scripts/preinstall"
cp scripts/driver-postinstall "$stage/scripts/postinstall"
chmod 755 "$stage/scripts/"* 
pkgbuild --root "$stage/root" --scripts "$stage/scripts" --ownership recommended \
    --identifier se.lbsa71.AudioBridge.driver --version 1.0.0 --install-location / \
    "$PWD/build/AudioBridge-1.0.0.pkg"
