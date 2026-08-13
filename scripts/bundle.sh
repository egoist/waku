#!/bin/sh
set -eu

profile="${1:-debug}"
cargo_target_dir="${CARGO_TARGET_DIR:-target}"
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
    app_name="Waku Debug"
    helper_name="Waku Debug Computer Use"
    bundle_identifier="sh.waku.dev"
    icon_file="AppIconDev.icns"
    ;;
  release)
    app_name="Waku"
    helper_name="Waku Computer Use"
    bundle_identifier="sh.waku"
    icon_file="AppIcon.icns"
    ;;
  *)
    echo "usage: scripts/bundle.sh [debug|release]" >&2
    exit 2
    ;;
esac
if [ "${WAKU_SKIP_CARGO_BUILD:-0}" != "1" ]; then
  if [ "$profile" = "release" ]; then
    cargo build --release -vv --bin waku --bin waku_js_repl
  else
    cargo build -vv --bin waku --bin waku_js_repl
  fi
fi

bundle="$cargo_target_dir/$profile/$app_name.app"
contents="$bundle/Contents"
helper_bundle="$contents/Helpers/$helper_name.app"
repl_executable="$contents/Resources/waku_js_repl"
swift_module_cache="$cargo_target_dir/$profile/swift-module-cache"
helper_source="resources/computer-use/WakuComputerUse.swift"
menu_bar_cursor_resource="resources/computer-use/menubar-cursor.png"
overlay_cursor_resource="resources/computer-use/overlay-cursor.svg"
helper_fingerprint="$({
  shasum -a 256 \
    "$helper_source" \
    resources/computer-use/Info.plist \
    "$menu_bar_cursor_resource" \
    "$overlay_cursor_resource"
  printf '%s\n' "standalone-service-v2" "$helper_name" "$bundle_identifier.computer-use" "$codesign_identity" "$(uname -m)-apple-macos13.0"
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
  elif [ "$profile" = "release" ]; then
    codesign --force --options runtime --timestamp --sign "$codesign_identity" "$cached_helper_staging"
  else
    codesign --force --options runtime --sign "$codesign_identity" "$cached_helper_staging"
  fi
  mkdir -p "$helper_cache_root"
  mv "$helper_cache_staging" "$helper_cache_entry"
fi

# Sparkle powers in-app updates. The framework is embedded in the bundle and
# the same distribution's bin/ tools (generate_appcast, sign_update) sign
# releases, so both come from one pinned archive cached outside target/ where
# `cargo clean` cannot evict it. Bump the version and checksum together.
sparkle_version="2.9.4"
sparkle_sha256="ce89daf967db1e1893ed3ebd67575ed82d3902563e3191ca92aaec9164fbdef9"
sparkle_cache_root=".waku-cache/sparkle"
sparkle_cache_entry="$sparkle_cache_root/$sparkle_version"
sparkle_framework_source="$sparkle_cache_entry/Sparkle.framework"

if [ ! -d "$sparkle_framework_source" ]; then
  sparkle_staging="$sparkle_cache_root/.staging-$sparkle_version-$$"
  rm -rf "$sparkle_staging"
  mkdir -p "$sparkle_staging"
  sparkle_archive="$sparkle_staging/Sparkle-$sparkle_version.tar.xz"
  curl -fsSL --retry 3 -o "$sparkle_archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle_version/Sparkle-$sparkle_version.tar.xz"
  echo "$sparkle_sha256  $sparkle_archive" | shasum -a 256 -c - >/dev/null
  tar -xJf "$sparkle_archive" -C "$sparkle_staging" ./Sparkle.framework ./bin
  rm "$sparkle_archive"
  mv "$sparkle_staging" "$sparkle_cache_entry"
fi

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources/computer-use" "$contents/Resources/skills/waku-computer-use" "$contents/Helpers"
cp "$cargo_target_dir/$profile/waku" "$contents/MacOS/$app_name"
cp "$cargo_target_dir/$profile/waku_js_repl" "$repl_executable"
chmod 755 "$repl_executable"
cp resources/Info.plist "$contents/Info.plist"
cp "resources/$icon_file" "$contents/Resources/AppIcon.icns"
cp resources/computer-use/pi-extension.ts "$contents/Resources/computer-use/pi-extension.ts"
cp resources/computer-use/SKILL.md "$contents/Resources/skills/waku-computer-use/SKILL.md"
frameworks_directory="$contents/Frameworks"
sparkle_framework="$frameworks_directory/Sparkle.framework"
mkdir -p "$frameworks_directory"
cp -R "$sparkle_framework_source" "$sparkle_framework"
# Waku is not sandboxed, so Sparkle's XPC services never run; drop them along
# with the header and module folders so the shipped framework carries no dev
# artifacts and no unsigned nested code.
for sparkle_extra in XPCServices Headers PrivateHeaders Modules; do
  rm -rf "$sparkle_framework/$sparkle_extra" \
    "$sparkle_framework/Versions/B/$sparkle_extra"
done
plutil -replace CFBundleDisplayName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleExecutable -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$contents/Info.plist"
cp -R "$cached_helper_bundle" "$helper_bundle"
# Finder info and resource forks on copied resources make codesign reject the
# bundle as "detritus"; strip extended attributes before signing.
xattr -cr "$bundle"
# Sparkle's nested executables sign first, then the framework, then the app.
# The app's hardened runtime enforces library validation, so the framework must
# carry the same identity as the app or dlopen rejects it at launch.
if [ "$codesign_identity" = "-" ]; then
  codesign --force --sign - "$sparkle_framework/Versions/B/Autoupdate"
  codesign --force --sign - "$sparkle_framework/Versions/B/Updater.app"
  codesign --force --sign - "$sparkle_framework"
  codesign --force --identifier "$bundle_identifier.js-repl" --sign - "$repl_executable"
  codesign --force --sign - "$bundle"
elif [ "$profile" = "release" ]; then
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework/Versions/B/Autoupdate"
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework/Versions/B/Updater.app"
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework"
  codesign --force --options runtime --timestamp --identifier "$bundle_identifier.js-repl" --sign "$codesign_identity" "$repl_executable"
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$bundle"
else
  codesign --force --options runtime --sign "$codesign_identity" "$sparkle_framework/Versions/B/Autoupdate"
  codesign --force --options runtime --sign "$codesign_identity" "$sparkle_framework/Versions/B/Updater.app"
  codesign --force --options runtime --sign "$codesign_identity" "$sparkle_framework"
  codesign --force --options runtime --identifier "$bundle_identifier.js-repl" --sign "$codesign_identity" "$repl_executable"
  codesign --force --options runtime --sign "$codesign_identity" "$bundle"
fi
if [ "$profile" = "release" ]; then
  codesign --verify --strict --verbose=2 "$repl_executable"
  codesign --verify --deep --strict --verbose=2 "$bundle"
fi

echo "$bundle"
