#!/bin/sh
#
# Builds the release: a universal (arm64 + x86_64) archive, signed with a Developer ID, notarized,
# stapled and packed into a .dmg. Runs on the Mac.
#
#   sh scripts/release.sh [--version X.Y.Z]
#
# Signing and notarization read their inputs from the environment, so no secret ever sits in the
# repository. With none of them set, the script still builds, packs and checks the universal
# archive, but the result is ad-hoc signed and says so (it will not pass Gatekeeper):
#
#   MORIN_SIGNING_IDENTITY   "Developer ID Application: Name (TEAMID)", as `security find-identity` shows it
#   MORIN_TEAM_ID            the ten-character team identifier; pins the helper and the app to each other
#   MORIN_NOTARY_PROFILE     a `notarytool store-credentials` profile name, or instead all three of:
#   MORIN_NOTARY_KEY_PATH    the App Store Connect API key (.p8)
#   MORIN_NOTARY_KEY_ID      its key id
#   MORIN_NOTARY_ISSUER      its issuer id
#
# The version is read from MARKETING_VERSION (macos/tools/gen-xcodeproj.py). --version asserts that
# it matches, which is how the tag build checks that `macos-vX.Y.Z` names what it builds.
set -eu

cd "$(dirname "$0")/.."
project="MorinExplorer.xcodeproj"
app_name="Morin Explorer"
out="${RELEASE_DIR:-build/release}"

expected=""
while [ $# -gt 0 ]; do
    case "$1" in
        --version) expected="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

version="$(xcodebuild -project "$project" -scheme MorinExplorer -showBuildSettings 2>/dev/null | awk '/ MARKETING_VERSION = /{print $3; exit}')"
[ -n "$version" ] || { echo "release: cannot read MARKETING_VERSION" >&2; exit 1; }
if [ -n "$expected" ] && [ "$expected" != "$version" ]; then
    echo "release: the tag says $expected but the project builds $version" >&2
    exit 1
fi

identity="${MORIN_SIGNING_IDENTITY:-}"
signed=yes
[ -n "$identity" ] || { identity="-"; signed=no; echo "release: no MORIN_SIGNING_IDENTITY; building ad-hoc signed (not for distribution)"; }

rm -rf "$out"
mkdir -p "$out"

echo "== archive $version (arm64 + x86_64)"
xcodebuild -project "$project" -scheme MorinExplorer -configuration Release -archivePath "$out/MorinExplorer.xcarchive" \
    ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO MORIN_TEAM_ID="${MORIN_TEAM_ID:-}" \
    CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY="$identity" ENABLE_HARDENED_RUNTIME=YES OTHER_CODE_SIGN_FLAGS="--timestamp" \
    archive 2>&1 | grep -E "error:|ARCHIVE (SUCCEEDED|FAILED)" || true
app="$out/MorinExplorer.xcarchive/Products/Applications/$app_name.app"
[ -d "$app" ] || { echo "release: the archive has no app" >&2; exit 1; }

echo "== architectures"
for binary in "$app/Contents/MacOS/$app_name" "$app/Contents/MacOS/MorinExplorerHelper"; do
    archs="$(lipo -archs "$binary")"
    echo "$(basename "$binary"): $archs"
    case "$archs" in *arm64*x86_64*|*x86_64*arm64*) ;; *) echo "release: $binary is not universal" >&2; exit 1 ;; esac
done
[ -f "$app/Contents/Library/LaunchDaemons/casa.alera.MorinExplorer.helper.plist" ] || { echo "release: the helper's launchd plist is missing" >&2; exit 1; }

echo "== sign (helper first, then the app that contains it)"
if [ "$signed" = yes ]; then
    codesign --force --options runtime --timestamp --sign "$identity" --identifier casa.alera.MorinExplorer.helper "$app/Contents/MacOS/MorinExplorerHelper"
    codesign --force --options runtime --timestamp --sign "$identity" "$app"
else
    codesign --force --sign - --identifier casa.alera.MorinExplorer.helper "$app/Contents/MacOS/MorinExplorerHelper"
    codesign --force --sign - "$app"
fi
codesign --verify --strict --verbose=2 "$app"

notarize() {
    if [ -n "${MORIN_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$1" --keychain-profile "$MORIN_NOTARY_PROFILE" --wait
    else
        xcrun notarytool submit "$1" --key "$MORIN_NOTARY_KEY_PATH" --key-id "$MORIN_NOTARY_KEY_ID" --issuer "$MORIN_NOTARY_ISSUER" --wait
    fi
}
can_notarize=no
if [ "$signed" = yes ] && { [ -n "${MORIN_NOTARY_PROFILE:-}" ] || [ -n "${MORIN_NOTARY_KEY_PATH:-}" ]; }; then can_notarize=yes; fi

if [ "$can_notarize" = yes ]; then
    echo "== notarize the app"
    ditto -c -k --keepParent "$app" "$out/notarize.zip"
    notarize "$out/notarize.zip"
    xcrun stapler staple "$app"
    spctl --assess --type execute --verbose=2 "$app"
else
    echo "release: not notarizing (needs a signing identity and notary credentials)"
fi

echo "== disk image"
stage="$out/stage"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg="$out/Morin-Explorer-$version.dmg"
hdiutil create -volname "$app_name $version" -srcfolder "$stage" -ov -format UDZO "$dmg" > /dev/null
if [ "$signed" = yes ]; then codesign --force --timestamp --sign "$identity" "$dmg"; fi
if [ "$can_notarize" = yes ]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
fi
shasum -a 256 "$dmg" | tee "$dmg.sha256"
echo "release: $dmg ($( [ "$signed" = yes ] && echo signed || echo 'ad-hoc signed, NOT for distribution' ), $( [ "$can_notarize" = yes ] && echo notarized || echo 'not notarized' ))"
