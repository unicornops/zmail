#!/usr/bin/env bash
set -euo pipefail

# generate-app-icon.sh - Builds Zmail.icns, and optionally a compiled asset
# catalog (Assets.car), from assets/icons/app-icon.svg.
#
# The SVG is rasterized with rsvg-convert (brew install librsvg) or, when
# librsvg is unavailable, macOS's own qlmanage; sips resizes the 1024px
# source into the Apple iconset sizes and iconutil assembles the icns.
# This mirrors the Shuffleboard icon pipeline (unicornops/shuffleboard
# generate-appicon.sh), adapted because Zmail is not an Xcode project.
#
# With a second argument, the same images also go into an asset catalog
# with an "AppIcon" set (Shuffleboard's AppIcon.appiconset layout), which
# Xcode's actool compiles to Assets.car. The bundle names it with
# CFBundleIconName: recent macOS takes the app icon from there, notably for
# notifications, and keeps CFBundleIconFile (the icns) as the fallback.
#
# Usage: scripts/generate-app-icon.sh [output.icns] [output-Assets.car]
# The icns defaults to target/Zmail.icns; no asset catalog by default.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SOURCE_SVG="${REPO_ROOT}/assets/icons/app-icon.svg"
OUTPUT="${1:-${REPO_ROOT}/target/Zmail.icns}"
ASSETS_CAR="${2:-}"

if [ ! -f "$SOURCE_SVG" ]; then
  echo "Error: icon source not found at $SOURCE_SVG" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"

WORK_DIR="$(mktemp -d)"
ICONSET="${WORK_DIR}/AppIcon.iconset"
mkdir -p "$ICONSET"
trap 'rm -rf "$WORK_DIR"' EXIT

# Obtain the 1024x1024 source PNG.
SOURCE_PNG="${WORK_DIR}/icon-1024.png"
if command -v rsvg-convert >/dev/null 2>&1; then
  rsvg-convert -w 1024 -h 1024 "$SOURCE_SVG" -o "$SOURCE_PNG"
  echo "Converted the icon SVG with rsvg-convert"
else
  # qlmanage names its thumbnail <file>.svg.png inside the output directory.
  qlmanage -t -s 1024 -o "$WORK_DIR" "$SOURCE_SVG" >/dev/null 2>&1 || true
  for candidate in "$WORK_DIR"/*.png; do
    if [ -f "$candidate" ]; then
      mv "$candidate" "$SOURCE_PNG"
      break
    fi
  done
  echo "Converted the icon SVG with qlmanage"
fi

if [ ! -f "$SOURCE_PNG" ]; then
  echo "Error: could not rasterize $SOURCE_SVG" >&2
  echo "Install librsvg: brew install librsvg" >&2
  exit 1
fi

if ! sips -g pixelWidth "$SOURCE_PNG" 2>/dev/null | grep -q 1024; then
  sips -z 1024 1024 "$SOURCE_PNG" --out "$SOURCE_PNG" >/dev/null
fi

# Apple iconset sizes (pt x scale = px): 16, 32, 128, 256, 512 at 1x and 2x.
for SIZE in 16 32 128 256 512; do
  sips -z "$SIZE" "$SIZE" "$SOURCE_PNG" --out "$ICONSET/icon_${SIZE}x${SIZE}.png" >/dev/null
  DOUBLE=$((SIZE * 2))
  sips -z "$DOUBLE" "$DOUBLE" "$SOURCE_PNG" --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png" >/dev/null
done

iconutil -c icns -o "$OUTPUT" "$ICONSET"
echo "Built $OUTPUT"

if [ -z "$ASSETS_CAR" ]; then
  exit 0
fi

# The asset catalog: the iconset's PNGs as AppIcon.appiconset.
CATALOG="${WORK_DIR}/Assets.xcassets"
APPICONSET="${CATALOG}/AppIcon.appiconset"
mkdir -p "$APPICONSET"
cp "$ICONSET"/*.png "$APPICONSET/"
cat > "${CATALOG}/Contents.json" <<'JSON'
{
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON
{
  echo '{'
  echo '  "images" : ['
  first=1
  for SIZE in 16 32 128 256 512; do
    for SCALE in 1 2; do
      if [ "$SCALE" = 1 ]; then FILE="icon_${SIZE}x${SIZE}.png"; else FILE="icon_${SIZE}x${SIZE}@2x.png"; fi
      [ "$first" = 1 ] || echo ','
      first=0
      printf '    { "filename" : "%s", "idiom" : "mac", "scale" : "%sx", "size" : "%sx%s" }' \
        "$FILE" "$SCALE" "$SIZE" "$SIZE"
    done
  done
  echo
  echo '  ],'
  echo '  "info" : { "author" : "xcode", "version" : 1 }'
  echo '}'
} > "${APPICONSET}/Contents.json"

COMPILED="${WORK_DIR}/compiled"
mkdir -p "$COMPILED"
xcrun actool "$CATALOG" \
  --compile "$COMPILED" \
  --platform macosx \
  --minimum-deployment-target 12.0 \
  --app-icon AppIcon \
  --output-partial-info-plist "${WORK_DIR}/partial-info.plist" \
  --output-format human-readable-text \
  --notices --warnings --errors

if [ ! -f "${COMPILED}/Assets.car" ]; then
  echo "Error: actool did not produce Assets.car" >&2
  exit 1
fi
mkdir -p "$(dirname "$ASSETS_CAR")"
cp "${COMPILED}/Assets.car" "$ASSETS_CAR"
echo "Built $ASSETS_CAR"
