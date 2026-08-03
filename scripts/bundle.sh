#!/bin/sh
set -eu

profile="${1:-debug}"
if [ -n "${WAKU_CODESIGN_IDENTITY:-}" ]; then
  codesign_identity="$WAKU_CODESIGN_IDENTITY"
else
  if [ "$profile" = "debug" ]; then
    preferred_identity="Apple Development:"
    fallback_identity="Developer ID Application:"
  else
    preferred_identity="Developer ID Application:"
    fallback_identity="Apple Development:"
  fi
  codesign_identity=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk -v identity="$preferred_identity" 'index($0, "\"" identity) { print $2; exit }')
  if [ -z "$codesign_identity" ]; then
    codesign_identity=$(security find-identity -v -p codesigning 2>/dev/null \
      | awk -v identity="$fallback_identity" 'index($0, "\"" identity) { print $2; exit }')
  fi
  if [ -z "$codesign_identity" ]; then
    codesign_identity="-"
  fi
fi
case "$profile" in
  debug)
    cargo build
    app_name="Waku Debug"
    helper_name="Waku Debug Computer Use"
    bundle_identifier="codes.waku.dev"
    ;;
  release)
    cargo build --release
    app_name="Waku"
    helper_name="Waku Computer Use"
    bundle_identifier="codes.waku"
    ;;
  *)
    echo "usage: scripts/bundle.sh [debug|release]" >&2
    exit 2
    ;;
esac

bundle="target/$profile/$app_name.app"
contents="$bundle/Contents"
helper_bundle="$contents/Helpers/$helper_name.app"
swift_module_cache="target/$profile/swift-module-cache"
helper_source="resources/computer-use/WakuComputerUse.swift"
menu_bar_cursor_resource="resources/computer-use/menubar-cursor.png"
overlay_cursor_resource="resources/computer-use/overlay-cursor.svg"
helper_fingerprint="$({
  shasum -a 256 \
    "$helper_source" \
    resources/computer-use/Info.plist \
    "$menu_bar_cursor_resource" \
    "$overlay_cursor_resource"
  printf '%s\n' "standalone-service-v1" "$helper_name" "$bundle_identifier.computer-use" "$codesign_identity" "$(uname -m)-apple-macos13.0"
  xcrun swiftc -version
} | shasum -a 256 | awk '{ print $1 }')"
helper_cache_root=".waku-cache/computer-use/$profile"
helper_cache_entry="$helper_cache_root/$helper_fingerprint"
cached_helper_bundle="$helper_cache_entry/$helper_name.app"

# Keep compiled helpers outside target so `cargo clean` does not force an
# unnecessary Swift rebuild. The fingerprint includes the signing identity so
# switching certificates can never reuse a helper signed as different code.
# The cached app is copied into Waku's standard Helpers directory as the
# canonical packaged service. Waku refreshes a stable standalone runtime copy
# from it so Screen Recording is attributed to the helper rather than Waku.

if [ ! -d "$cached_helper_bundle" ]; then
  helper_cache_staging="$helper_cache_root/.staging-$helper_fingerprint-$$"
  rm -rf "$helper_cache_staging"
  cached_helper_staging="$helper_cache_staging/$helper_name.app"
  cached_helper_contents="$cached_helper_staging/Contents"
  mkdir -p "$cached_helper_contents/MacOS" "$cached_helper_contents/Resources" "$swift_module_cache"
  cp resources/computer-use/Info.plist "$cached_helper_contents/Info.plist"
  cp "$menu_bar_cursor_resource" "$overlay_cursor_resource" "$cached_helper_contents/Resources/"
  printf '%s\n' "$helper_fingerprint" > "$cached_helper_contents/Resources/.waku-helper-fingerprint"
  plutil -replace CFBundleDisplayName -string "$helper_name" "$cached_helper_contents/Info.plist"
  plutil -replace CFBundleExecutable -string "$helper_name" "$cached_helper_contents/Info.plist"
  plutil -replace CFBundleIdentifier -string "$bundle_identifier.computer-use" "$cached_helper_contents/Info.plist"
  plutil -replace CFBundleName -string "$helper_name" "$cached_helper_contents/Info.plist"
  xcrun swiftc \
    -O \
    -parse-as-library \
    -module-cache-path "$swift_module_cache" \
    -target "$(uname -m)-apple-macos13.0" \
    "$helper_source" \
    -o "$cached_helper_contents/MacOS/$helper_name"
  if [ "$codesign_identity" = "-" ]; then
    codesign --force --sign - "$cached_helper_staging"
  else
    codesign --force --options runtime --sign "$codesign_identity" "$cached_helper_staging"
  fi
  mkdir -p "$helper_cache_root"
  mv "$helper_cache_staging" "$helper_cache_entry"
fi

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources/skills/waku-computer-use" "$contents/Helpers"
cp "target/$profile/waku" "$contents/MacOS/$app_name"
cp resources/Info.plist "$contents/Info.plist"
cp resources/computer-use/SKILL.md "$contents/Resources/skills/waku-computer-use/SKILL.md"
cp resources/computer-use/waku-computer-use-client.mjs "$contents/Resources/Waku Computer Use Client.mjs"
plutil -replace CFBundleDisplayName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleExecutable -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$contents/Info.plist"
cp -R "$cached_helper_bundle" "$helper_bundle"
if [ "$codesign_identity" = "-" ]; then
  codesign --force --sign - "$bundle"
else
  codesign --force --options runtime --sign "$codesign_identity" "$bundle"
fi

echo "$bundle"
