#!/usr/bin/env bash
# Builds everything the app links against, for the platform Xcode is building
# (PLATFORM_NAME), and regenerates the Swift bindings. Run by Xcode before
# compiling; safe to run by hand.
#
#   ios/build-core.sh            # simulator
#   PLATFORM_NAME=iphoneos ios/build-core.sh
set -euo pipefail

IOS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$IOS/.." && pwd)"
PLATFORM="${PLATFORM_NAME:-iphonesimulator}"
case "$PLATFORM" in
  iphonesimulator) TARGET=aarch64-apple-ios-sim ;;
  iphoneos) TARGET=aarch64-apple-ios ;;
  *) echo "unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac

# Xcode's environment is for building iOS code; cargo's host builds (build
# scripts, the binding generator) must not inherit it.
HOSTENV=(env -i "HOME=$HOME" "PATH=$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin" "USER=${USER:-}")

# Commits and pushes in both repos go through tools/namecheck.
if command -v git >/dev/null; then
  git -C "$ROOT" config core.hooksPath tools/hooks || true
  [ -e "$ROOT/deps/ish/.git" ] && git -C "$ROOT/deps/ish" config core.hooksPath "$ROOT/tools/hooks" || true
  cmp -s "$ROOT/tools/namecheck" "$ROOT/deps/ish/.github/namecheck" \
    || echo "warning: deps/ish/.github/namecheck differs from tools/namecheck" >&2
fi

[ -f "$ROOT/deps/out/ios-$PLATFORM/lib/libish.a" ] || "${HOSTENV[@]}" "$ROOT/deps/build_ish.sh" --sdk "$PLATFORM"
[ -f "$ROOT/deps/out/rootfs/alpine-rootfs.zip" ] || "${HOSTENV[@]}" "$ROOT/deps/build_rootfs.sh"

cd "$ROOT/core"
# The guest CLI first: the core carries it (crates/core/build.rs).
"${HOSTENV[@]}" cargo build --release -p solos-guest --target aarch64-unknown-linux-musl
"${HOSTENV[@]}" IPHONEOS_DEPLOYMENT_TARGET=18.0 cargo build --release -p solos-ffi --target "$TARGET"

# Bindings come from the host build of the same crate: identical metadata,
# and nothing iOS-specific is needed to read it.
"${HOSTENV[@]}" cargo build --release -p solos-ffi
OUT="$IOS/Generated"
TMP="$(mktemp -d)"
"${HOSTENV[@]}" cargo run --release -q -p uniffi-bindgen -- generate \
  --library target/release/libsolos_ffi.dylib --language swift --out-dir "$TMP"
mkdir -p "$OUT"
# Only replace files that changed, so Xcode does not recompile needlessly.
for f in "$TMP"/*; do
  cmp -s "$f" "$OUT/$(basename "$f")" || cp "$f" "$OUT/"
done
rm -rf "$TMP"
