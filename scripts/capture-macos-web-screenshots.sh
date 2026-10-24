#!/usr/bin/env bash
set -euo pipefail

# capture-macos-web-screenshots.sh - Screenshots of the macOS web renderer.
#
# Usage: scripts/capture-macos-web-screenshots.sh OUTPUT_DIR
#
# Launches `zmail-app launch-ui-fixture` once per state below (seeded
# synthetic mail, no network) and captures the screen with `screencapture`,
# so CI's macOS job can show what a message in a web view looks like: the
# page itself, GPUI's menus and dialogs drawn above it, and the page cut
# off by the conversation list when scrolled (ADR 0006). Each launch's
# stderr, with ZMAIL_WEB_DEBUG on, is saved next to its PNG.
#
# Requires macOS and a built app (ZMAIL_APP_BINARY, default
# target/debug/zmail-app).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_BINARY="${ZMAIL_APP_BINARY:-${REPO_ROOT}/target/debug/zmail-app}"
OUTPUT_DIR="${1:?usage: $0 OUTPUT_DIR}"

# name view theme seconds-to-wait
STATES=(
  "reader-html-light reader-html light 8"
  "reader-html-dark reader-html dark 8"
  "reader-html-menu-light reader-html-menu light 8"
  "reader-html-scrolled-light reader-html-scrolled light 10"
  "reader-link-warning-light reader-link-warning light 8"
  "reader-link-warning-dark reader-link-warning dark 8"
)

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: this script only runs on macOS" >&2
  exit 1
fi
if [[ ! -x "$APP_BINARY" ]]; then
  echo "error: no app binary at ${APP_BINARY}; build it first" >&2
  exit 1
fi
mkdir -p "$OUTPUT_DIR"

app_pid=""
cleanup() {
  if [[ -n "$app_pid" ]]; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

for entry in "${STATES[@]}"; do
  read -r name view theme wait_seconds <<<"$entry"
  echo "Capturing ${name}"
  ZMAIL_WEB_DEBUG=1 "$APP_BINARY" launch-ui-fixture "$view" "$theme" \
    > "${OUTPUT_DIR}/${name}.log" 2>&1 &
  app_pid=$!
  sleep "$wait_seconds"
  if ! kill -0 "$app_pid" 2>/dev/null; then
    echo "::warning::zmail-app exited before ${name} was captured; see ${name}.log"
    app_pid=""
    continue
  fi
  screencapture -x "${OUTPUT_DIR}/${name}.png" \
    || echo "::warning::screencapture failed for ${name}"
  cleanup
  app_pid=""
done

# Interactions, driven with posted mouse and keyboard events. Each starts
# the reader-html state again; positions are screen points on the runner's
# 1024x768 screen with the window where the app opens it.
EVENTS="${TMPDIR:-/tmp}/macos-ui-events"
if swiftc -O "${SCRIPT_DIR}/macos-ui-events.swift" -o "$EVENTS"; then
  interact() {
    # interact NAME then EVENT ARGS... [-- EVENT ARGS...]
    local name="$1"
    shift
    echo "Capturing ${name}"
    ZMAIL_WEB_DEBUG=1 "$APP_BINARY" launch-ui-fixture reader-html light \
      > "${OUTPUT_DIR}/${name}.log" 2>&1 &
    app_pid=$!
    sleep 8
    local step=()
    for argument in "$@" --; do
      if [[ "$argument" == "--" ]]; then
        "$EVENTS" "${step[@]}" || echo "::warning::${step[*]} failed"
        sleep 1
        step=()
      else
        step+=("$argument")
      fi
    done
    sleep 2
    screencapture -x "${OUTPUT_DIR}/${name}.png" \
      || echo "::warning::screencapture failed for ${name}"
    cleanup
    app_pid=""
  }
  # The wheel over the page scrolls the conversation.
  interact reader-html-wheel scroll 724 550 300
  # A link in the page opens Zmail's open-link dialog.
  interact reader-html-link-click click 724 681
  # Text selected in the page is copied by Command-C, through Zmail.
  pbcopy < /dev/null
  interact reader-html-copy drag 496 485 930 506 -- key-copy
  pbpaste > "${OUTPUT_DIR}/reader-html-copy.txt" || true
  echo "Copied: $(cat "${OUTPUT_DIR}/reader-html-copy.txt")"
else
  echo "::warning::could not build macos-ui-events; skipping the interaction captures"
fi

ls -l "$OUTPUT_DIR"
