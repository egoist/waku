#!/usr/bin/env bash
# Package the Linux bundle into a .deb and an .rpm.
#
# Usage:
#   ./scripts/package-linux.sh <waku-<version>-<triple>.tar.gz>
#
# The tarball ships an install-prefix layout (`bin/`, `share/`) under one
# versioned directory. A system package needs a rooted one, so the binaries
# move to `/usr/lib/waku/bin`, the resources stay beside them in
# `/usr/lib/waku/share`, and `/usr/bin/waku` is a symlink — which Waku
# resolves back to the real path, as `docs/linux.md` already documents.
#
# The graphics stack, the portals and the desktop entry are runtime
# concerns rather than link-time ones: the binary needs only libc and libgcc
# from the system and loads the rest itself, so the package declares only
# those two and the rest stays documented rather than forced.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

tarball="${1:-}"
if [ -z "$tarball" ]; then
  echo "usage: $0 <waku-<version>-<triple>.tar.gz>" >&2
  exit 1
fi
if [ ! -f "$tarball" ]; then
  echo "no such tarball: $tarball" >&2
  exit 1
fi

target_dir="${CARGO_TARGET_DIR:-target}"
version="$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"name":"waku","version":"\([^"]*\)".*/\1/p')"
if [ -z "$version" ]; then
  echo "could not read the waku version from cargo metadata" >&2
  exit 1
fi
name="$(basename "$tarball" .tar.gz)"
case "$name" in
  *x86_64*)
    deb_arch=amd64
    rpm_arch=x86_64
    ;;
  *aarch64*)
    deb_arch=arm64
    rpm_arch=aarch64
    ;;
  *)
    echo "unknown architecture in $name" >&2
    exit 1
    ;;
esac

staging="$(mktemp -d)"
package="$(mktemp -d)"
trap 'rm -rf -- "$staging" "$package"' EXIT

tar -xzf "$tarball" -C "$staging"
bundle="$(find "$staging" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
if [ -z "$bundle" ] || [ ! -d "$bundle/bin" ]; then
  echo "$tarball does not hold a waku bundle" >&2
  exit 1
fi

# The payload root both packages are built from.
payload="$package/root"
mkdir -p "$payload/usr/lib/waku/bin" "$payload/usr/lib/waku/share" "$payload/usr/bin"
cp -a "$bundle/bin/." "$payload/usr/lib/waku/bin/"
cp -a "$bundle/share/." "$payload/usr/lib/waku/share/"
ln -sf ../lib/waku/bin/waku "$payload/usr/bin/waku"
mkdir -p "$payload/usr/share/applications" \
  "$payload/usr/share/icons/hicolor/256x256/apps" \
  "$payload/usr/share/doc/waku"
cp "$bundle/share/applications/sh.waku.desktop" \
  "$payload/usr/share/applications/sh.waku.desktop"
cp "$bundle/share/icons/hicolor/256x256/apps/sh.waku.png" \
  "$payload/usr/share/icons/hicolor/256x256/apps/sh.waku.png"
cp LICENSE "$payload/usr/share/doc/waku/copyright"

# Debian: `dpkg-deb --root-owner-group` writes the payload as root without
# needing fakeroot, which the release runners do not have.
debian_root="$package/debian"
deb="$target_dir/release/waku_${version}_${deb_arch}.deb"
mkdir -p "$debian_root/DEBIAN"
cp -a "$payload/usr" "$debian_root/usr"
cat > "$debian_root/DEBIAN/control" <<EOF
Package: waku
Version: ${version}
Section: devel
Priority: optional
Architecture: ${deb_arch}
Maintainer: Waku contributors <noreply@waku.sh>
Depends: libc6, libgcc-s1
Description: A native control plane for local coding agents
 Waku drives local coding agents from a native desktop client: it runs the
 provider CLIs, streams their work into one transcript, and keeps every task,
 project and checkpoint in one place.
Homepage: https://waku.sh
EOF
mkdir -p "$(dirname "$deb")"
chmod -R u+w,go=rX "$debian_root"
dpkg-deb --root-owner-group --build "$debian_root" "$deb"
printf 'Created %s\n' "$deb"

# RPM: the payload is copied into the build root by `%install`, because
# rpmbuild empties the build root before it runs. Auto-requires read the
# binaries, so the dependencies are the ones this architecture's own package
# manager names.
spec="$package/waku.spec"
rpm="$target_dir/release/waku-${version}-${rpm_arch}.rpm"
cp -a "$payload" "$package/payload"
cat > "$spec" <<EOF
Name:           waku
Version:        ${version}
Release:        1
Summary:        A native control plane for local coding agents
License:        GPL-3.0-or-later
BuildArch:      ${rpm_arch}
URL:            https://waku.sh

%description
Waku drives local coding agents from a native desktop client: it runs the
provider CLIs, streams their work into one transcript, and keeps every task,
project and checkpoint in one place.

%install
cp -a "%{payload}/." "%{buildroot}/"

%files
/usr/bin/waku
/usr/lib/waku
/usr/share/applications/sh.waku.desktop
/usr/share/icons/hicolor/256x256/apps/sh.waku.png
/usr/share/doc/waku/copyright
EOF
rpmbuild -bb \
  --define "_topdir $package/rpmbuild" \
  --define "payload $package/payload" \
  "$spec" >/dev/null
find "$package/rpmbuild/RPMS" -name 'waku-*.rpm' -exec mv {} "$rpm" \;
printf 'Created %s\n' "$rpm"
