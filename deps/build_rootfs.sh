#!/usr/bin/env bash
# Build the guest root filesystem: Alpine Linux minirootfs (aarch64),
# converted to iSH's fakefs layout and zipped for the app bundle.
#
#   deps/build_rootfs.sh
#
# Output: deps/out/rootfs/alpine-rootfs.zip
# The download is pinned by version and checked against its SHA-256, so a
# clean clone builds the same file.
#
# Requires: curl, meson, ninja, libarchive (brew install libarchive).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISH="$HERE/ish"
VERSION=3.21.0
SERIES=v3.21
ARCH=aarch64
SHA256=f31202c4070c4ef7de9e157e1bd01cb4da3a2150035d74ea5372c5e86f1efac1
TARBALL="alpine-minirootfs-$VERSION-$ARCH.tar.gz"
URL="https://dl-cdn.alpinelinux.org/alpine/$SERIES/releases/$ARCH/$TARBALL"

WORK="$HERE/work"
OUT="$HERE/out/rootfs"
mkdir -p "$WORK" "$OUT"

if [ ! -f "$WORK/$TARBALL" ]; then
  curl -fL --retry 3 -o "$WORK/$TARBALL.part" "$URL"
  mv "$WORK/$TARBALL.part" "$WORK/$TARBALL"
fi
echo "$SHA256  $WORK/$TARBALL" | shasum -a 256 -c -

# fakefsify is a host tool from the iSH tree.
NATIVE="$ISH/build-native"
if [ ! -x "$NATIVE/tools/fakefsify" ]; then
  LIBARCHIVE="$(brew --prefix libarchive 2>/dev/null || true)"
  export PKG_CONFIG_PATH="${LIBARCHIVE:+$LIBARCHIVE/lib/pkgconfig:}${PKG_CONFIG_PATH:-}"
  (cd "$ISH" && { [ -f "$NATIVE/build.ninja" ] || meson setup "$NATIVE" --buildtype=release \
      -Dlog="" -Dkernel=ish -Dengine=asbestos -Dguest_arch=arm64; } && ninja -C "$NATIVE" tools/fakefsify)
fi

STAGE="$WORK/alpine-rootfs"
rm -rf "$STAGE"
"$NATIVE/tools/fakefsify" "$WORK/$TARBALL" "$STAGE"
[ -f "$STAGE/meta.db" ] && [ -d "$STAGE/data" ] || { echo "fakefsify produced no rootfs" >&2; exit 1; }

rm -f "$OUT/alpine-rootfs.zip"
(cd "$WORK" && zip -qry "$OUT/alpine-rootfs.zip" alpine-rootfs)
echo "rootfs (Alpine $VERSION) -> $OUT/alpine-rootfs.zip ($(du -h "$OUT/alpine-rootfs.zip" | cut -f1))"
