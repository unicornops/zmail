#!/usr/bin/env bash
set -euo pipefail

# create-dmg.sh - Creates a distributable DMG for the Zmail app bundle.
#
# The image is created read-write, mounted, and styled with AppleScript so
# Finder opens it as a large-icon view with the app and the Applications
# alias side by side, then converted to a compressed read-only image. This
# is the same layout pipeline Shuffleboard releases use
# (unicornops/shuffleboard create-dmg.sh).
#
# Usage: scripts/create-dmg.sh <app_path> <dmg_name> <volume_name>
# Example: scripts/create-dmg.sh Zmail.app Zmail-0.8.0.dmg 'Zmail 0.8.0'

APP_PATH="$1"
DMG_NAME="$2"
VOLUME_NAME="$3"

if [ -z "$APP_PATH" ] || [ -z "$DMG_NAME" ] || [ -z "$VOLUME_NAME" ]; then
  echo "Usage: $0 <app_path> <dmg_name> <volume_name>" >&2
  echo "Example: $0 Zmail.app Zmail-0.8.0.dmg 'Zmail 0.8.0'" >&2
  exit 1
fi

if [ ! -d "$APP_PATH" ]; then
  echo "Error: app not found at $APP_PATH" >&2
  exit 1
fi

APP_NAME="$(basename "$APP_PATH")"
STAGING="$(mktemp -d)"
TEMP_DMG="temp_$DMG_NAME"
trap 'rm -rf "$STAGING"; rm -f "$TEMP_DMG"' EXIT

echo "Creating $DMG_NAME for $APP_PATH (volume: $VOLUME_NAME)"

cp -R "$APP_PATH" "$STAGING/"
ln -s /Applications "$STAGING/Applications"

# App size plus headroom for the volume metadata.
APP_SIZE="$(du -sm "$APP_PATH" | cut -f1)"
DMG_SIZE=$((APP_SIZE + 10))
echo "App size: ${APP_SIZE}MB, DMG size: ${DMG_SIZE}MB"

hdiutil create -srcfolder "$STAGING" \
  -volname "$VOLUME_NAME" \
  -fs HFS+ \
  -fsargs "-c c=64,a=16,e=16" \
  -format UDRW \
  -size "${DMG_SIZE}m" \
  "$TEMP_DMG" >/dev/null

MOUNT_DIR="$(hdiutil attach -readwrite -noverify -noautoopen "$TEMP_DMG" | grep '/Volumes/' | sed 's/.*\t//')"
if [ -z "$MOUNT_DIR" ]; then
  echo "Error: failed to mount the temporary DMG" >&2
  exit 1
fi
echo "Mounted at $MOUNT_DIR"

# Large icon view with the app and the Applications alias side by side.
cat > /tmp/zmail-dmg-setup.applescript << EOF
on run argv
  set volumeName to item 1 of argv
  set appName to item 2 of argv
  tell application "Finder"
    tell disk volumeName
      open
      set current view of container window to icon view
      set toolbar visible of container window to false
      set statusbar visible of container window to false
      set the bounds of container window to {100, 100, 600, 400}
      set viewOptions to the icon view options of container window
      set arrangement of viewOptions to not arranged
      set icon size of viewOptions to 72
      set position of item appName of container window to {150, 150}
      set position of item "Applications" of container window to {350, 150}
      close
      open
      update without registering applications
      delay 2
    end tell
  end tell
end run
EOF

osascript /tmp/zmail-dmg-setup.applescript "$VOLUME_NAME" "$APP_NAME" 2>/dev/null \
  || echo "::warning::Could not set the DMG appearance (non-critical)"
rm -f /tmp/zmail-dmg-setup.applescript

sync
hdiutil detach "$MOUNT_DIR" -quiet

hdiutil convert "$TEMP_DMG" \
  -format UDZO \
  -imagekey zlib-level=9 \
  -o "$DMG_NAME" >/dev/null

echo "Created $DMG_NAME"
ls -lh "$DMG_NAME"
