#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/build-driver.sh
xcrun clang -std=c11 -O1 -g -fsanitize=address,undefined -fblocks \
    -framework CoreAudio -framework CoreFoundation Driver/DriverTests.c -o build/driver-tests
ASAN_OPTIONS=detect_leaks=0 build/driver-tests "$PWD/build/AudioBridge.driver"
