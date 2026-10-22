#!/bin/sh
#
# Builds the app universal (arm64 + x86_64) and runs the unit tests, on the Mac. UI tests are
# opt-in (UI=1) because they need Developer mode and Automation mode enabled once on the machine.
#
#   sh scripts/xcode-test.sh
set -eu
cd "$(dirname "$0")/.."
export SPEC_ROOT="${SPEC_ROOT:-$(cd ../spec && pwd)}"
derived="${DERIVED_DATA:-.dd}"
arch="$(uname -m)"

echo "== universal build (arm64 + x86_64)"
xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -configuration Debug \
    -derivedDataPath "$derived" ONLY_ACTIVE_ARCH=NO build 2>&1 | grep -E "error:|warning: .*(unre|deprec)|BUILD (SUCCEEDED|FAILED)" || true
app="$derived/Build/Products/Debug/Morin Explorer.app"
lipo -info "$app/Contents/MacOS/Morin Explorer"

echo "== unit tests ($arch)"
xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -destination "platform=macOS,arch=$arch" \
    -derivedDataPath "$derived" test -only-testing:MorinExplorerUnitTests 2>&1 \
    | grep -E "error:|Executed .* tests|TEST (SUCCEEDED|FAILED)"

if [ "${UI:-0}" = "1" ]; then
    echo "== UI tests ($arch)"
    xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -destination "platform=macOS,arch=$arch" \
        -derivedDataPath "$derived" test -only-testing:MorinExplorerUITests 2>&1 \
        | grep -E "error:|Executed .* tests|TEST (SUCCEEDED|FAILED)"
fi
