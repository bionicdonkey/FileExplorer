#!/bin/sh
#
# Builds the app universal (arm64 + x86_64) and runs the unit tests, on the Mac. UI tests are
# opt-in (UI=1) because they need Developer mode and Automation mode enabled once on the machine.
# Exits non-zero if a build fails, a test fails, or the test process crashes or times out.
#
#   sh scripts/xcode-test.sh
set -eu
cd "$(dirname "$0")/.."
export SPEC_ROOT="${SPEC_ROOT:-$(cd ../spec && pwd)}"
derived="${DERIVED_DATA:-.dd}"
arch="$(uname -m)"
logs="$derived/logs"
mkdir -p "$logs"

# Runs a command with its output in a log, keeps its own exit status, and shows the lines that matter.
# A failure stops the script, so nothing after it can quietly use a stale build.
run() {
    name="$1"; pattern="$2"; shift 2
    log="$logs/$name.log"
    status=0
    "$@" > "$log" 2>&1 || status=$?
    grep -E "$pattern" "$log" | sort -u | head -60 || true
    if [ "$status" -ne 0 ]; then
        echo "xcode-test: $name failed (exit $status); full log: $log" >&2
        tail -15 "$log" >&2
        exit "$status"
    fi
}

echo "== universal build (arm64 + x86_64)"
run build "error:|BUILD (SUCCEEDED|FAILED)" \
    xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -configuration Debug \
        -derivedDataPath "$derived" ONLY_ACTIVE_ARCH=NO build
app="$derived/Build/Products/Debug/Morin Explorer.app"
for binary in "$app/Contents/MacOS/Morin Explorer" "$app/Contents/MacOS/MorinExplorerHelper"; do
    archs="$(lipo -archs "$binary")"
    echo "$(basename "$binary"): $archs"
    case "$archs" in *arm64*x86_64*|*x86_64*arm64*) ;; *) echo "xcode-test: $binary is not universal" >&2; exit 1 ;; esac
done

# Unit tests run the .xctest bundle directly. `xcodebuild test` needs testmanagerd, which refuses to
# start sessions while Developer mode is off; the direct run needs neither and tests the same code.
echo "== unit tests ($arch)"
run build-for-testing "error:|TEST BUILD (SUCCEEDED|FAILED)" \
    xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -destination "platform=macOS,arch=$arch" \
        -derivedDataPath "$derived" build-for-testing
contents="$PWD/$derived/Build/Products/Debug/Morin Explorer.app/Contents"
export DYLD_FRAMEWORK_PATH="$contents/MacOS:$contents/Frameworks" DYLD_LIBRARY_PATH="$contents/MacOS"
run unit-tests "error:|budget |Executed [0-9]+ tests?, with .* in [0-9.]+ \([0-9.]+\) seconds$" \
    perl -e 'alarm 1200; exec @ARGV' xcrun xctest "$contents/PlugIns/MorinExplorerUnitTests.xctest"
# A run that printed no summary at all did not run the tests.
grep -Eq "Executed [1-9][0-9]* tests?, with 0 failures" "$logs/unit-tests.log" || { echo "xcode-test: no passing test summary" >&2; exit 1; }
unset DYLD_FRAMEWORK_PATH DYLD_LIBRARY_PATH

if [ "${UI:-0}" = "1" ]; then
    echo "== UI tests ($arch)"
    run ui-tests "error:|Executed .* tests|TEST (SUCCEEDED|FAILED)" \
        xcodebuild -project MorinExplorer.xcodeproj -scheme MorinExplorer -destination "platform=macOS,arch=$arch" \
            -derivedDataPath "$derived" test -only-testing:MorinExplorerUITests
fi
echo "xcode-test: ok"
