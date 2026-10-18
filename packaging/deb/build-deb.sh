#!/usr/bin/env bash
#
# Builds zorin-explorer_<version>_<arch>.deb from the current checkout.
#
#   packaging/deb/build-deb.sh [output-dir]      (default: dist/)
#
# Needs cargo, dpkg-deb and the GTK 4 / libadwaita / libarchive development
# packages. Run it on the oldest distribution the package should install on:
# the Depends line is computed from the libraries the binary actually links, so
# a build on Ubuntu 24.04 (Zorin OS 18) installs there and on anything newer.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="${1:-$root/dist}"
cd "$root"

version="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
arch="$(dpkg --print-architecture)"
name="zorin-explorer"
app_id="com.zorin.Explorer"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
pkg="$stage/pkg"

cargo build --release --locked
binary="target/release/$name"

install -Dm755 "$binary" "$pkg/usr/bin/$name"
strip --strip-unneeded "$pkg/usr/bin/$name"
install -Dm644 "resources/$app_id.desktop" "$pkg/usr/share/applications/$app_id.desktop"
install -Dm644 "resources/icons/hicolor/scalable/apps/$app_id.svg" \
    "$pkg/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
install -Dm644 "packaging/$app_id.metainfo.xml" "$pkg/usr/share/metainfo/$app_id.metainfo.xml"

install -d "$pkg/usr/share/doc/$name"
copyright="$pkg/usr/share/doc/$name/copyright"
cat > "$copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: Zorin Explorer
Source: https://github.com/bionicdonkey/FileExplorer

Files: *
Copyright: Josh Benoit
License: GPL-3.0-or-later
 On Debian systems, the full text of the GNU General Public License version 3
 can be found in /usr/share/common-licenses/GPL-3.
EOF
chmod 644 "$copyright"

# Link-time dependencies come from the binary itself, so the versions track
# whatever the build host provides rather than a hand-kept list. The tool wants
# a debian/control to run against; an empty one is enough.
shlibs="$(
    cd "$stage"
    mkdir -p debian
    printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$name" "$name" > debian/control
    dpkg-shlibdeps -O -e"$root/$binary" 2>/dev/null | sed -n 's/^shlibs:Depends=//p'
)"
if [[ -z "$shlibs" ]]; then
    echo "dpkg-shlibdeps found no dependencies for $binary" >&2
    exit 1
fi

# pkexec runs the elevated retry; gvfs provides trash, mounts and network
# locations; shared-mime-info names the file types the context menu offers
# applications for.
depends="$shlibs, pkexec | policykit-1, gvfs, shared-mime-info"
# gvfs-backends carries the archive and remote backends. Extraction and
# compression themselves run in-process, so no archiver program is needed.
recommends="gvfs-backends"

install -d "$pkg/DEBIAN"
cat > "$pkg/DEBIAN/control" <<EOF
Package: $name
Version: $version
Architecture: $arch
Maintainer: Josh Benoit <18510219+bionicdonkey@users.noreply.github.com>
Installed-Size: $(du -sk "$pkg/usr" | cut -f1)
Depends: $depends
Recommends: $recommends
Section: utils
Priority: optional
Homepage: https://github.com/bionicdonkey/FileExplorer
Description: Explorer-style file manager for Linux
 Zorin Explorer is a native Explorer-style file manager for Linux, designed
 for Zorin OS and GNOME. It has tabs, a sidebar with quick access, archive
 browsing, search and a file operations queue.
EOF

# The icon cache and desktop database are refreshed by dpkg triggers on
# /usr/share/icons and /usr/share/applications, so no maintainer scripts.

find "$pkg" -type d -exec chmod 755 {} +
mkdir -p "$out"
deb="$out/${name}_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$pkg" "$deb" > /dev/null
echo "$deb"
