#!/usr/bin/env bash
# Build the Rust core as a static library for iOS (device + Apple-silicon
# simulator, optionally Intel simulator) and wrap it in build/MapMeCore.xcframework.
#
#   scripts/build-core-ios.sh            # release, device + arm64 simulator
#   INCLUDE_X86_SIM=1 scripts/build-core-ios.sh
#   PROFILE=debug scripts/build-core-ios.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORE="$ROOT/core"
OUT="$ROOT/build"
PROFILE="${PROFILE:-release}"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim)
if [[ "${INCLUDE_X86_SIM:-0}" == "1" ]]; then
  TARGETS+=(x86_64-apple-ios)
fi

rustup target add "${TARGETS[@]}" >/dev/null

CARGO_FLAGS=()
[[ "$PROFILE" == "release" ]] && CARGO_FLAGS+=(--release)

for t in "${TARGETS[@]}"; do
  echo "==> cargo build $PROFILE for $t"
  (cd "$CORE" && cargo build "${CARGO_FLAGS[@]}" --lib --target "$t")
done

LIB=libmapme_core.a
TD="$CORE/target"
rm -rf "$OUT/MapMeCore.xcframework" "$OUT/sim-universal" "$OUT/headers"
mkdir -p "$OUT/headers" "$OUT/sim-universal"
cp "$CORE/include/mapme_core.h" "$CORE/include/module.modulemap" "$OUT/headers/"

SIM_LIBS=("$TD/aarch64-apple-ios-sim/$PROFILE/$LIB")
if [[ "${INCLUDE_X86_SIM:-0}" == "1" ]]; then
  SIM_LIBS+=("$TD/x86_64-apple-ios/$PROFILE/$LIB")
fi
lipo -create "${SIM_LIBS[@]}" -output "$OUT/sim-universal/$LIB"

xcodebuild -create-xcframework \
  -library "$TD/aarch64-apple-ios/$PROFILE/$LIB" -headers "$OUT/headers" \
  -library "$OUT/sim-universal/$LIB" -headers "$OUT/headers" \
  -output "$OUT/MapMeCore.xcframework"

echo "==> $OUT/MapMeCore.xcframework"
du -sh "$OUT/MapMeCore.xcframework"
