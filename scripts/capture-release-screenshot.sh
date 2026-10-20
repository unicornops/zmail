#!/usr/bin/env bash
# Captures Linux screenshots of Zmail under an X server (normally Xvfb).
#
#   scripts/capture-release-screenshot.sh OUTPUT_PATH
#       The release screenshot: starts the live mail harness (Docker),
#       syncs its IMAP account and captures the main window.
#
#   scripts/capture-release-screenshot.sh --matrix OUTPUT_DIR [STATE...]
#       The UI screenshot matrix: one PNG per UI state, named after the
#       state, with no harness or network. Each state launches
#       `zmail-app launch-ui-fixture VIEW THEME`, which opens an in-memory
#       store seeded with synthetic mail in that view. Pass state names to
#       capture only those; `--list` prints them.
#
# Both modes expect a running X server in DISPLAY with room for a
# 1920x1080 window in matrix mode, for example:
#
#   cargo build -p zmail-app --release --locked
#   xvfb-run -a --server-args="-screen 0 2560x1440x24" \
#     scripts/capture-release-screenshot.sh --matrix artifacts/ui-screenshots
#
# Requires openbox, xdotool and ImageMagick's `import`. ZMAIL_APP_BINARY
# overrides the binary (default target/release/zmail-app) and
# ZMAIL_CAPTURE_LOG_DIR the directory for the app and openbox logs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_BINARY="${ZMAIL_APP_BINARY:-${REPO_ROOT}/target/release/zmail-app}"
WINDOW_TITLE="Zmail"
LOG_DIR="${ZMAIL_CAPTURE_LOG_DIR:-${TMPDIR:-/tmp}}"
APP_LOG="${LOG_DIR}/zmail-release-screenshot-app.log"
OPENBOX_LOG="${LOG_DIR}/zmail-release-screenshot-openbox.log"
APP_PID=""
OPENBOX_PID=""
HARNESS_STARTED=0

# The matrix: NAME VIEW THEME WIDTH HEIGHT [POINTER_X POINTER_Y]. 1440x920
# is the app's default window size. With a pointer position (in window
# coordinates) the mouse rests there before the capture, to show a hover
# state such as a message row's quick actions. The inbox and reader are also captured at each layout
# breakpoint: 720 (one pane at a time), 900 (sidebar as an overlay) and
# 1920 (wide); `sidebar` is the overlay open at 900. `accounts` is three
# accounts with nested folders on the Unified Inbox, and `command-palette`
# is the palette open over it. The compose views are
# captured from their compose window (see window_title_for), at its default
# 760x680.
MATRIX=(
  "empty-dark empty dark 1440 920"
  "empty-light empty light 1440 920"
  "inbox-dark inbox dark 1440 920"
  "inbox-light inbox light 1440 920"
  "inbox-compact-dark inbox-compact dark 1440 920"
  "inbox-compact-light inbox-compact light 1440 920"
  "inbox-hover-dark inbox dark 1440 920 470 240"
  "inbox-hover-light inbox light 1440 920 470 240"
  "inbox-empty-dark inbox-empty dark 1440 920"
  "inbox-empty-light inbox-empty light 1440 920"
  "syncing-dark syncing dark 1440 920"
  "syncing-light syncing light 1440 920"
  "outbox-dark outbox dark 1440 920"
  "outbox-light outbox light 1440 920"
  "reader-dark reader dark 1440 920"
  "reader-light reader light 1440 920"
  "reader-expanded-dark reader-expanded dark 1440 920"
  "reader-expanded-light reader-expanded light 1440 920"
  "reader-long-dark reader-long dark 1440 920"
  "reader-long-light reader-long light 1440 920"
  "reader-selection-dark reader-selection dark 1440 920"
  "reader-selection-light reader-selection light 1440 920"
  "reader-link-warning-dark reader-link-warning dark 1440 920"
  "reader-link-warning-light reader-link-warning light 1440 920"
  "reader-reply-dark reader-reply dark 1440 920"
  "reader-reply-light reader-reply light 1440 920"
  "compose-dark compose dark 760 680"
  "compose-light compose light 760 680"
  "compose-invalid-dark compose-invalid dark 760 680"
  "compose-invalid-light compose-invalid light 760 680"
  "undo-send-dark undo-send dark 1440 920"
  "undo-send-light undo-send light 1440 920"
  "account-setup-dark account-setup dark 640 780"
  "account-setup-light account-setup light 640 780"
  "onboarding-provider-dark onboarding-provider dark 1440 920"
  "onboarding-provider-light onboarding-provider light 1440 920"
  "onboarding-credentials-dark onboarding-credentials dark 1440 920"
  "onboarding-credentials-light onboarding-credentials light 1440 920"
  "onboarding-discovering-dark onboarding-discovering dark 1440 920"
  "onboarding-consent-dark onboarding-consent dark 1440 920"
  "onboarding-consent-light onboarding-consent light 1440 920"
  "onboarding-result-dark onboarding-result dark 1440 920"
  "onboarding-manual-dark onboarding-manual dark 1440 920"
  "onboarding-manual-light onboarding-manual light 1440 920"
  "onboarding-manual-cleartext-dark onboarding-manual-cleartext dark 1440 920"
  "onboarding-colour-dark onboarding-colour dark 1440 920"
  "onboarding-colour-light onboarding-colour light 1440 920"
  "onboarding-syncing-dark onboarding-syncing dark 1440 920"
  "onboarding-manual-dark-720 onboarding-manual dark 720 720"
  "inbox-dark-720 inbox dark 720 720"
  "inbox-light-720 inbox light 720 720"
  "reader-dark-720 reader dark 720 720"
  "reader-light-720 reader light 720 720"
  "inbox-dark-900 inbox dark 900 720"
  "inbox-light-900 inbox light 900 720"
  "reader-dark-900 reader dark 900 720"
  "reader-light-900 reader light 900 720"
  "sidebar-dark-900 sidebar dark 900 720"
  "sidebar-light-900 sidebar light 900 720"
  "accounts-dark accounts dark 1440 920"
  "accounts-light accounts light 1440 920"
  "multi-select-dark multi-select dark 1440 920"
  "multi-select-light multi-select light 1440 920"
  "search-dark search dark 1440 920"
  "search-light search light 1440 920"
  "search-filters-dark search-filters dark 1440 920"
  "search-filters-light search-filters light 1440 920"
  "command-palette-dark command-palette dark 1440 920"
  "command-palette-light command-palette light 1440 920"
  "inbox-dark-1920 inbox dark 1920 1080"
  "inbox-light-1920 inbox light 1920 1080"
  "reader-dark-1920 reader dark 1920 1080"
  "reader-light-1920 reader light 1920 1080"
  "reader-safety-info-dark reader-safety-info dark 1440 920"
  "reader-safety-info-light reader-safety-info light 1440 920"
  "reader-safety-caution-dark reader-safety-caution dark 1440 920"
  "reader-safety-caution-light reader-safety-caution light 1440 920"
  "reader-safety-danger-dark reader-safety-danger dark 1440 920"
  "reader-safety-danger-light reader-safety-danger light 1440 920"
  "reader-attachments-dark reader-attachments dark 1440 920"
  "reader-attachments-light reader-attachments light 1440 920"
  "reader-attachments-dark-720 reader-attachments dark 720 720"
)

usage() {
  echo "usage: scripts/capture-release-screenshot.sh OUTPUT_PATH" >&2
  echo "       scripts/capture-release-screenshot.sh --matrix OUTPUT_DIR [STATE...]" >&2
  echo "       scripts/capture-release-screenshot.sh --list" >&2
  exit 1
}

stop_app() {
  if [[ -n "${APP_PID}" ]] && kill -0 "${APP_PID}" 2>/dev/null; then
    kill "${APP_PID}" 2>/dev/null || true
  fi
  if [[ -n "${APP_PID}" ]]; then
    wait "${APP_PID}" 2>/dev/null || true
  fi
  APP_PID=""
}

cleanup() {
  local exit_code=$?

  stop_app

  if [[ -n "${OPENBOX_PID}" ]] && kill -0 "${OPENBOX_PID}" 2>/dev/null; then
    kill "${OPENBOX_PID}" 2>/dev/null || true
  fi
  if [[ -n "${OPENBOX_PID}" ]]; then
    wait "${OPENBOX_PID}" 2>/dev/null || true
  fi

  if [[ "${HARNESS_STARTED}" -eq 1 ]]; then
    "${REPO_ROOT}/scripts/mail-harness.sh" down >/dev/null 2>&1 || true
  fi

  exit "${exit_code}"
}

# The title of the window a fixture view is captured from: the main
# window, the compose window a compose view's draft opens in (titled with
# the draft's subject), or the Add Account window.
window_title_for() {
  case "$1" in
    compose) echo "Re: Q4 roadmap review" ;;
    compose-invalid) echo "Offline search demo" ;;
    account-setup) echo "Add Account" ;;
    *) echo "${WINDOW_TITLE}" ;;
  esac
}

# Prints the id of the app's visible window titled $1 (default: the main
# window) once it is mapped, waiting up to 90 seconds; fails if the app
# exits first.
wait_for_window() {
  local title="${1:-${WINDOW_TITLE}}"
  local window_id=""
  for _ in $(seq 1 90); do
    if ! kill -0 "${APP_PID}" 2>/dev/null; then
      echo "zmail-app exited before its window appeared" >&2
      cat "${APP_LOG}" >&2
      return 1
    fi

    window_id="$(xdotool search --onlyvisible --name "^${title}\$" 2>/dev/null | head -n 1 || true)"
    if [[ -n "${window_id}" ]]; then
      echo "${window_id}"
      return 0
    fi

    sleep 1
  done

  echo "timed out waiting for the ${title} window" >&2
  cat "${APP_LOG}" >&2
  return 1
}

start_session() {
  mkdir -p "${LOG_DIR}"
  : > "${APP_LOG}"
  : > "${OPENBOX_LOG}"

  export NO_AT_BRIDGE=1
  export LIBGL_ALWAYS_SOFTWARE=1
  export WINIT_UNIX_BACKEND=x11
  export XDG_SESSION_TYPE=x11
  unset WAYLAND_DISPLAY

  openbox >"${OPENBOX_LOG}" 2>&1 &
  OPENBOX_PID=$!
}

capture_release() {
  local output_path="$1"
  mkdir -p "$(dirname "${output_path}")"
  start_session

  "${REPO_ROOT}/scripts/mail-harness.sh" up
  HARNESS_STARTED=1

  "${APP_BINARY}" launch-imap-fixture \
    "imap://127.0.0.1:3143" \
    "phase2" \
    "phase2-password" \
    "phase2@localhost" \
    >"${APP_LOG}" 2>&1 &
  APP_PID=$!

  local window_id
  window_id="$(wait_for_window)"
  xdotool windowactivate "${window_id}" >/dev/null 2>&1 || true
  sleep 3
  import -window "${window_id}" "${output_path}"
}

# Captures one matrix state into OUTPUT_DIR/NAME.png.
capture_state() {
  local output_dir="$1" name="$2" view="$3" theme="$4" width="$5" height="$6"
  local pointer_x="${7:-}" pointer_y="${8:-}"
  local output_path="${output_dir}/${name}.png"

  echo "capturing ${name} (${view}, ${theme}, ${width}x${height})"
  echo "=== ${name}" >> "${APP_LOG}"
  "${APP_BINARY}" launch-ui-fixture "${view}" "${theme}" >>"${APP_LOG}" 2>&1 &
  APP_PID=$!

  local window_id
  window_id="$(wait_for_window "$(window_title_for "${view}")")"
  xdotool windowactivate "${window_id}" >/dev/null 2>&1 || true
  xdotool windowmove "${window_id}" 0 0 >/dev/null 2>&1 || true
  xdotool windowsize "${window_id}" "${width}" "${height}"

  # Wait for the window manager to apply the size, then for GPUI to draw
  # the new frame.
  local actual=""
  for _ in $(seq 1 20); do
    actual="$(xdotool getwindowgeometry "${window_id}" | sed -n 's/^ *Geometry: //p')"
    [[ "${actual}" == "${width}x${height}" ]] && break
    sleep 0.5
  done
  if [[ "${actual}" != "${width}x${height}" ]]; then
    echo "the ${name} window is ${actual}, not ${width}x${height}" >&2
    return 1
  fi
  if [[ -n "${pointer_x}" ]]; then
    xdotool mousemove --window "${window_id}" "${pointer_x}" "${pointer_y}"
  fi
  sleep 3

  import -window "${window_id}" "${output_path}"
  if [[ -n "${pointer_x}" ]]; then
    # Park the pointer so it does not hover the next state.
    xdotool mousemove 2500 1400
  fi
  stop_app
  # Let the window unmap so the next state cannot find this one.
  sleep 1
}

capture_matrix() {
  local output_dir="$1"
  shift
  mkdir -p "${output_dir}"
  start_session

  local entry name selected
  local captured=0
  for entry in "${MATRIX[@]}"; do
    read -r name _ <<< "${entry}"
    if [[ $# -gt 0 ]]; then
      selected=0
      for wanted in "$@"; do
        [[ "${wanted}" == "${name}" ]] && selected=1
      done
      [[ "${selected}" -eq 1 ]] || continue
    fi
    # shellcheck disable=SC2086 # Split the entry into its fields.
    capture_state "${output_dir}" ${entry}
    captured=$((captured + 1))
  done

  if [[ "${captured}" -eq 0 ]]; then
    echo "no matrix state matched: $*" >&2
    return 1
  fi
  echo "captured ${captured} screenshot(s) in ${output_dir}"
}

case "${1:-}" in
  --list)
    for entry in "${MATRIX[@]}"; do
      echo "${entry%% *}"
    done
    exit 0
    ;;
  --matrix)
    [[ $# -ge 2 ]] || usage
    ;;
  "" | -*)
    usage
    ;;
  *)
    [[ $# -eq 1 ]] || usage
    ;;
esac

trap cleanup EXIT

if [[ ! -x "${APP_BINARY}" ]]; then
  echo "expected a zmail-app binary at ${APP_BINARY}" >&2
  exit 1
fi

if [[ "$1" == "--matrix" ]]; then
  shift
  capture_matrix "$@"
else
  capture_release "$1"
fi
