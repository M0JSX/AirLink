#!/usr/bin/env bash
# Builds dist/AirLink.app (universal arm64 + x86_64) and dist/AirLink-<ver>-macos-universal.dmg
#
# Optional env:
#   ARCHS="aarch64 x86_64"        architectures to build (default both)
#   MACOS_SIGN_IDENTITY="Developer ID Application: …"   sign with hardened runtime (else ad-hoc)
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD           notarise + staple the DMG
set -euo pipefail
cd "$(dirname "$0")/../.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCHS=${ARCHS:-"aarch64 x86_64"}
APP=dist/AirLink.app
DMG=dist/AirLink-$VERSION-macos-universal.dmg

# Build libopus from source and link it statically, so the app doesn't depend on Homebrew.
export LIBOPUS_STATIC=1 LIBOPUS_NO_PKG=1 OPUS_STATIC=1 OPUS_NO_PKG=1 PKG_CONFIG=false
export MACOSX_DEPLOYMENT_TARGET=11.0
export CMAKE_POLICY_VERSION_MINIMUM=3.5   # libopus's CMakeLists predates CMake 4

BINS=()
for a in $ARCHS; do
  t="$a-apple-darwin"
  rustup target add "$t" >/dev/null
  cargo build --release --target "$t"
  BINS+=("target/$t/release/airlink")
done

rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/airlink" "${BINS[@]}"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

# Fail if anything links outside the system.
if otool -L "$APP/Contents/MacOS/airlink" | grep -E '/opt/homebrew|/usr/local' ; then
  echo "error: binary links against Homebrew libraries" >&2; exit 1
fi

ICONSET=dist/AirLink.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s packaging/icons/airlink.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) packaging/icons/airlink.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AirLink.icns"
rm -rf "$ICONSET"

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp --options runtime \
    --entitlements packaging/macos/entitlements.plist -s "$MACOS_SIGN_IDENTITY" "$APP"
else
  codesign --force -s - "$APP"   # ad-hoc: runs locally, Gatekeeper will warn elsewhere
fi

STAGE=dist/dmg
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "AirLink $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

if [ -n "${APPLE_ID:-}" ] && [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp -s "$MACOS_SIGN_IDENTITY" "$DMG"
  xcrun notarytool submit "$DMG" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait
  xcrun stapler staple "$DMG"
fi

echo "Built $DMG"
