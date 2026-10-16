#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/capture-release-screenshot.sh OUTPUT_PATH" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_BINARY="${REPO_ROOT}/target/release/zmail-app"
OUTPUT_PATH="$1"
WINDOW_TITLE="Zmail"
APP_LOG="${TMPDIR:-/tmp}/zmail-release-screenshot-app.log"
OPENBOX_LOG="${TMPDIR:-/tmp}/zmail-release-screenshot-openbox.log"
APP_PID=""
OPENBOX_PID=""
HARNESS_STARTED=0

cleanup() {
  local exit_code=$?

  if [[ -n "${APP_PID}" ]] && kill -0 "${APP_PID}" 2>/dev/null; then
    kill "${APP_PID}" 2>/dev/null || true
  fi
  if [[ -n "${APP_PID}" ]]; then
    wait "${APP_PID}" 2>/dev/null || true
  fi

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

trap cleanup EXIT

if [[ ! -x "${APP_BINARY}" ]]; then
  echo "expected a release binary at ${APP_BINARY}" >&2
  exit 1
fi

mkdir -p "$(dirname "${OUTPUT_PATH}")"
: > "${APP_LOG}"
: > "${OPENBOX_LOG}"

export NO_AT_BRIDGE=1
export LIBGL_ALWAYS_SOFTWARE=1
export WINIT_UNIX_BACKEND=x11
export XDG_SESSION_TYPE=x11
unset WAYLAND_DISPLAY

"${REPO_ROOT}/scripts/mail-harness.sh" up
HARNESS_STARTED=1

openbox >"${OPENBOX_LOG}" 2>&1 &
OPENBOX_PID=$!

"${APP_BINARY}" launch-imap-fixture \
  "imap://127.0.0.1:3143" \
  "phase2" \
  "phase2-password" \
  "phase2@localhost" \
  >"${APP_LOG}" 2>&1 &
APP_PID=$!

WINDOW_ID=""
for _ in $(seq 1 90); do
  if ! kill -0 "${APP_PID}" 2>/dev/null; then
    echo "zmail-app exited before the screenshot was captured" >&2
    cat "${APP_LOG}" >&2
    exit 1
  fi

  WINDOW_ID="$(xdotool search --onlyvisible --name "${WINDOW_TITLE}" 2>/dev/null | head -n 1 || true)"
  if [[ -n "${WINDOW_ID}" ]]; then
    break
  fi

  sleep 1
done

if [[ -z "${WINDOW_ID}" ]]; then
  echo "timed out waiting for the ${WINDOW_TITLE} window" >&2
  cat "${APP_LOG}" >&2
  exit 1
fi

xdotool windowactivate "${WINDOW_ID}" >/dev/null 2>&1 || true
sleep 3
import -window "${WINDOW_ID}" "${OUTPUT_PATH}"
