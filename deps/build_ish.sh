#!/usr/bin/env bash
# Build the iSH kernel (deps/ish) as static libraries for one iOS SDK.
#
#   deps/build_ish.sh [--sdk iphoneos|iphonesimulator] [--debug]
#
# Output: deps/out/ios-<sdk>/lib/{libish,libish_emu,libfakefs}.a
# The kernel's headers are used from the source tree and the build
# directory; nothing is copied.
#
# Requires: Xcode, meson, ninja.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISH="$HERE/ish"
SDK=iphoneos
BUILDTYPE=release
NDEBUG=true
MIN_IOS=18.0

while [ $# -gt 0 ]; do
  case "$1" in
    --sdk) SDK="$2"; shift 2 ;;
    --debug) BUILDTYPE=debug; NDEBUG=false; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$SDK" in
  iphoneos) MIN_FLAG="-miphoneos-version-min=$MIN_IOS" ;;
  iphonesimulator) MIN_FLAG="-mios-simulator-version-min=$MIN_IOS" ;;
  *) echo "--sdk must be iphoneos or iphonesimulator" >&2; exit 2 ;;
esac

# ld.lld links the guest vDSO (brew formula: lld).
for tool in meson ninja ld.lld; do
  command -v "$tool" >/dev/null || { echo "missing $tool (brew install ${tool#ld.})" >&2; exit 1; }
done
[ -f "$ISH/meson.build" ] || { echo "deps/ish is empty: git submodule update --init deps/ish" >&2; exit 1; }

SDK_PATH="$(xcrun --sdk "$SDK" --show-sdk-path)"
BUILD="$ISH/build-$SDK"
OUT="$HERE/out/ios-$SDK"
mkdir -p "$BUILD"

cat > "$BUILD/cross.txt" <<EOF
[binaries]
c = ['clang', '-arch', 'arm64', '-isysroot', '$SDK_PATH', '$MIN_FLAG']
ar = 'ar'
strip = 'strip'
pkg-config = 'false'

[host_machine]
system = 'darwin'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'

[built-in options]
c_link_args = ['-L$SDK_PATH/usr/lib']

[properties]
needs_exe_wrapper = true
sys_root = '$SDK_PATH'
EOF

cd "$ISH"
# The kernel's own vendored libraries (libarchive for fakefs tooling, etc.).
git submodule update --init --depth 1 deps/libarchive >/dev/null 2>&1 || true

if [ ! -f "$BUILD/build.ninja" ]; then
  meson setup "$BUILD" --cross-file "$BUILD/cross.txt" \
    --buildtype="$BUILDTYPE" -Db_ndebug="$NDEBUG" \
    -Dlog="" -Dlog_handler=nslog -Dkernel=ish -Dengine=asbestos -Dguest_arch=arm64
else
  meson configure "$BUILD" --buildtype="$BUILDTYPE" -Db_ndebug="$NDEBUG" >/dev/null
fi
ninja -C "$BUILD" libish.a libish_emu.a libfakefs.a

mkdir -p "$OUT/lib"
cp "$BUILD"/libish.a "$BUILD"/libish_emu.a "$BUILD"/libfakefs.a "$OUT/lib/"
echo "iSH ($SDK, $BUILDTYPE) -> $OUT/lib"
