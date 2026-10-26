#!/usr/bin/env bash
set -euo pipefail

# capture-linux-web-screenshots.sh - Screenshots of the Linux web renderer.
#
# Usage: scripts/capture-linux-web-screenshots.sh [--wayland] OUTPUT_DIR
#
# Launches `zmail-app launch-ui-fixture` once per state below (seeded
# synthetic mail, no network) under an X server and captures the screen,
# so CI's Linux web renderer job can show what a message in a WebKitGTK
# view looks like on X11: the page itself, GPUI's menus and dialogs drawn
# above it, and the page cut off by the conversation list when scrolled
# (ADR 0006). Then three interactions with xdotool: the wheel over the page,
# a link click, and a selection copied with Ctrl-C (the clipboard is saved
# as reader-html-copy.txt). Each launch's stderr, with ZMAIL_WEB_DEBUG on,
# is saved next to its PNG.
#
# With --wayland the app runs on Wayland instead, under weston's kiosk shell
# in a weston window on the X server (so xdotool's events still reach it as
# Wayland input), and the pages are pictures (ADR 0006); the PNGs are named
# with a -wayland suffix, and the clipboard is not read.
#
# Run it inside an X server with WebKitGTK 4.1 installed, for example:
#
#   cargo build -p zmail-app --locked
#   xvfb-run -a --server-args="-screen 0 1440x920x24" \
#     scripts/capture-linux-web-screenshots.sh artifacts/linux-web-screenshots
#
# Requires openbox (or weston, for --wayland), xdotool, xclip and
# ImageMagick's `import`.
# ZMAIL_APP_BINARY overrides the binary (default target/debug/zmail-app).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_BINARY="${ZMAIL_APP_BINARY:-${REPO_ROOT}/target/debug/zmail-app}"
WAYLAND=0
if [[ "${1:-}" == "--wayland" ]]; then
  WAYLAND=1
  shift
fi
OUTPUT_DIR="${1:?usage: $0 [--wayland] OUTPUT_DIR}"
SUFFIX=""
[[ "$WAYLAND" == 1 ]] && SUFFIX="-wayland"
WIDTH=1440
HEIGHT=920

# name view theme
STATES=(
  "reader-html-light reader-html light"
  "reader-html-dark reader-html dark"
  "reader-html-menu-light reader-html-menu light"
  "reader-html-scrolled-light reader-html-scrolled light"
  "reader-link-warning-light reader-link-warning light"
  "reader-link-warning-dark reader-link-warning dark"
)

if [[ -z "${DISPLAY:-}" ]]; then
  echo "error: no X server (DISPLAY is unset); run under xvfb-run" >&2
  exit 1
fi
if [[ ! -x "$APP_BINARY" ]]; then
  echo "error: no app binary at ${APP_BINARY}; build it first" >&2
  exit 1
fi
mkdir -p "$OUTPUT_DIR"

export NO_AT_BRIDGE=1
export LIBGL_ALWAYS_SOFTWARE=1
if [[ "$WAYLAND" == 1 ]]; then
  # weston's window on the X server, at 0,0 with the app filling it.
  export XDG_RUNTIME_DIR
  XDG_RUNTIME_DIR="$(mktemp -d)"
  export XDG_SESSION_TYPE=wayland
  weston --backend=x11 --renderer=pixman --shell=kiosk-shell.so \
    --socket=wayland-zmail --width="$WIDTH" --height="$HEIGHT" \
    > "${OUTPUT_DIR}/weston.log" 2>&1 &
  session_pid=$!
  for _ in $(seq 1 100); do
    [[ -S "${XDG_RUNTIME_DIR}/wayland-zmail" ]] && break
    sleep 0.2
  done
  export WAYLAND_DISPLAY=wayland-zmail
else
  export XDG_SESSION_TYPE=x11
  unset WAYLAND_DISPLAY
  openbox > "${OUTPUT_DIR}/openbox.log" 2>&1 &
  session_pid=$!
fi
app_pid=""
cleanup() {
  if [[ -n "$app_pid" ]]; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
}
trap 'cleanup; kill "$session_pid" 2>/dev/null || true' EXIT

# launch NAME VIEW THEME: starts the app on a fixture state and puts its
# window at 0,0 in WIDTH x HEIGHT; fails if no window appears.
launch() {
  local name="$1" view="$2" theme="$3"
  ZMAIL_WEB_DEBUG=1 "$APP_BINARY" launch-ui-fixture "$view" "$theme" \
    > "${OUTPUT_DIR}/${name}.log" 2>&1 &
  app_pid=$!
  if [[ "$WAYLAND" == 1 ]]; then
    # The kiosk shell shows the window full size; there is no X window
    # to wait for.
    sleep 15
    if ! kill -0 "$app_pid" 2>/dev/null; then
      echo "::warning::zmail-app exited before ${name}; see ${name}.log"
      app_pid=""
      return 1
    fi
    return 0
  fi
  local window_id=""
  for _ in $(seq 1 90); do
    if ! kill -0 "$app_pid" 2>/dev/null; then
      echo "::warning::zmail-app exited before ${name}; see ${name}.log"
      app_pid=""
      return 1
    fi
    window_id="$(xdotool search --onlyvisible --name '^Zmail$' 2>/dev/null | head -n 1 || true)"
    [[ -n "$window_id" ]] && break
    sleep 1
  done
  if [[ -z "$window_id" ]]; then
    echo "::warning::no window for ${name}"
    return 1
  fi
  xdotool windowactivate "$window_id" > /dev/null 2>&1 || true
  xdotool windowmove "$window_id" 0 0 > /dev/null 2>&1 || true
  xdotool windowsize "$window_id" "$WIDTH" "$HEIGHT" > /dev/null 2>&1 || true
  # The page loads, is measured and appears after the window does.
  sleep 8
}

# The whole screen, so the WebKitGTK windows inside GPUI's are included.
capture() {
  import -window root -crop "${WIDTH}x${HEIGHT}+0+0" "${OUTPUT_DIR}/$1${SUFFIX}.png" \
    || echo "::warning::import failed for $1"
}

for entry in "${STATES[@]}"; do
  read -r name view theme <<< "$entry"
  echo "Capturing ${name}"
  if launch "$name" "$view" "$theme"; then
    capture "$name"
  fi
  cleanup
  app_pid=""
  sleep 1
done

# Interactions on the reader-html state; positions are window (= screen)
# pixels in the 1440x920 window, over the newsletter's page.
interact() {
  # interact NAME XDOTOOL-COMMAND...
  local name="$1"
  shift
  echo "Capturing ${name}"
  if launch "$name" reader-html light; then
    xdotool "$@" || echo "::warning::xdotool $* failed"
    sleep 2
    capture "$name"
    # The app owns the clipboard: read it while the app runs (X11 only).
    if [[ "$WAYLAND" == 0 ]]; then
      xclip -o -selection clipboard > "${OUTPUT_DIR}/${name}-clipboard.txt" 2> /dev/null || true
    fi
  fi
  cleanup
  app_pid=""
  sleep 1
}
# The wheel over the page scrolls the conversation.
interact reader-html-wheel mousemove 1000 600 click --repeat 5 --delay 100 5
# A link in the page ("Read the full issue") opens Zmail's open-link dialog.
interact reader-html-link-click mousemove 1075 733 click 1
# Text selected in the page (a drag across the first paragraph, in steps
# as a hand would) is copied by Ctrl-C, through Zmail.
printf '' | xclip -selection clipboard || true
interact reader-html-copy mousemove 823 487 mousedown 1 sleep 0.2 \
  mousemove 900 495 sleep 0.1 mousemove 1100 515 sleep 0.1 mousemove 1300 535 sleep 0.2 \
  mouseup 1 sleep 0.5 key ctrl+c
mv "${OUTPUT_DIR}/reader-html-copy-clipboard.txt" "${OUTPUT_DIR}/reader-html-copy.txt" 2> /dev/null || true
rm -f "${OUTPUT_DIR}"/*-clipboard.txt
echo "Copied: $(cat "${OUTPUT_DIR}/reader-html-copy.txt" 2> /dev/null)"

ls -l "$OUTPUT_DIR"
