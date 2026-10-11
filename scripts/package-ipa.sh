#!/usr/bin/env bash
# Package a built .app into an .ipa (Payload/<Name>.app, zipped).
#
#   scripts/package-ipa.sh build/dd/Build/Products/Release-iphoneos/MapMe.app build/MapMe-unsigned.ipa
#
# Any existing code signature and provisioning profile are removed, so the
# result is an unsigned IPA for Sideloadly / AltStore, which sign it with the
# user's Apple ID. Symlinks are preserved.
set -euo pipefail

APP="${1:?usage: package-ipa.sh <path/to/App.app> <out.ipa>}"
OUT="${2:?usage: package-ipa.sh <path/to/App.app> <out.ipa>}"

die() { echo "package-ipa: error: $*" >&2; exit 1; }

APP="${APP%/}"
[[ -d "$APP" && "$APP" == *.app ]] || die "not an .app bundle: $APP"
[[ -f "$APP/Info.plist" ]] || die "missing $APP/Info.plist"

plist_get() {
  local key="$1" plist="$2"
  if command -v plutil >/dev/null 2>&1 && plutil -extract "$key" raw -o - "$plist" 2>/dev/null; then
    return 0
  fi
  python3 -c 'import plistlib, sys; print(plistlib.load(open(sys.argv[2], "rb")).get(sys.argv[1], ""))' "$key" "$plist"
}

EXE="$(plist_get CFBundleExecutable "$APP/Info.plist")"
BUNDLE_ID="$(plist_get CFBundleIdentifier "$APP/Info.plist")"
VERSION="$(plist_get CFBundleShortVersionString "$APP/Info.plist")"
BUILD="$(plist_get CFBundleVersion "$APP/Info.plist")"
[[ -n "$EXE" ]] || die "CFBundleExecutable missing from Info.plist"
[[ -f "$APP/$EXE" ]] || die "executable $APP/$EXE not found"

mkdir -p "$(dirname "$OUT")"
OUT_ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/package-ipa.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/Payload"
NAME="$(basename "$APP")"
if command -v ditto >/dev/null 2>&1; then
  ditto "$APP" "$STAGE/Payload/$NAME"
else
  cp -a "$APP" "$STAGE/Payload/$NAME"
fi

find "$STAGE/Payload" -name _CodeSignature -type d -prune -exec rm -rf {} +
find "$STAGE/Payload" \( -name embedded.mobileprovision -o -name .DS_Store \) -type f -delete

rm -f "$OUT_ABS"
if command -v zip >/dev/null 2>&1; then
  (cd "$STAGE" && zip -qry -X "$OUT_ABS" Payload)
else
  (cd "$STAGE" && ditto -c -k --sequesterRsrc --keepParent Payload "$OUT_ABS")
fi

ARCHS=""
if command -v lipo >/dev/null 2>&1; then
  ARCHS="$(lipo -archs "$APP/$EXE" 2>/dev/null || true)"
fi
SIZE="$(du -h "$OUT_ABS" | cut -f1)"
echo "package-ipa: $OUT ($SIZE)"
echo "  bundle id: $BUNDLE_ID  version: $VERSION ($BUILD)  executable: $EXE${ARCHS:+ [$ARCHS]}"
if command -v shasum >/dev/null 2>&1; then
  echo "  sha256: $(shasum -a 256 "$OUT_ABS" | cut -d' ' -f1)"
elif command -v sha256sum >/dev/null 2>&1; then
  echo "  sha256: $(sha256sum "$OUT_ABS" | cut -d' ' -f1)"
fi
