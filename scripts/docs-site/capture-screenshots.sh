#!/usr/bin/env bash
set -euo pipefail

# capture-screenshots.sh - Captures the documentation's screenshots for a
# release: one state per image in docs/images, named after it, saved as
# OUT_DIR/zmail-docs-<image> (screenshots.py has the naming). An image
# named tui-* is the terminal UI, rendered by
# scripts/capture-tui-screenshots.sh (docs/images/tui-message-index.png is
# its state tui-message-index); any other is a state of the desktop app's
# UI screenshot matrix, scripts/capture-release-screenshot.sh
# (docs/images/reader-light.png is the state reader-light). The release
# workflow publishes them as release assets and the site build uses them
# (docs/site.md).
#
#   scripts/docs-site/capture-screenshots.sh OUT_DIR
#   scripts/docs-site/capture-screenshots.sh --check
#
# Fails, before capturing anything, when an image in docs/images is not a
# state of its kind (either script's --list). --check does only that (no
# X server, no build): the docs workflow runs it on every pull request, so
# an image the release cannot capture is caught there.
#
# Capturing the desktop states needs what the matrix needs: a release build
# of zmail-app and an X server with room for the states' windows. The
# terminal UI's need cargo and the network (they build zmail-tui's tests and
# fetch pinned fonts). For example
#
#   xvfb-run -a --server-args="-screen 0 2560x1440x24" \
#     scripts/docs-site/capture-screenshots.sh artifacts/docs-screenshots
#
# ZMAIL_APP_BINARY and ZMAIL_CAPTURE_LOG_DIR are passed on to the matrix,
# ZMAIL_TUI_FONT_DIR to the terminal UI's capture.

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUT_DIR | --check" >&2
  exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
capture="${repo}/scripts/capture-release-screenshot.sh"
capture_tui="${repo}/scripts/capture-tui-screenshots.sh"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

bash "${capture}" --list > "${work}/states"
bash "${capture_tui}" --list > "${work}/tui-states"
plan="$(python3 -I "${here}/screenshots.py" plan "${repo}/docs/images" "${work}/states" "${work}/tui-states")"
states=()
assets=()
matrix_states=()
tui_states=()
while read -r state asset; do
  states+=("${state}")
  assets+=("${asset}")
  if [[ "${state}" == tui-* ]]; then
    tui_states+=("${state}")
  else
    matrix_states+=("${state}")
  fi
done <<< "${plan}"
if [[ "$1" == "--check" ]]; then
  echo "capture-screenshots.sh: ${#states[@]} documentation screenshot(s), each a state it can capture: ${states[*]}" >&2
  exit 0
fi

out="$1"
if [[ ${#matrix_states[@]} -gt 0 ]]; then
  bash "${capture}" --matrix "${work}/captured" "${matrix_states[@]}"
fi
if [[ ${#tui_states[@]} -gt 0 ]]; then
  bash "${capture_tui}" "${work}/captured" "${tui_states[@]}"
fi
mkdir -p "${out}"
for i in "${!states[@]}"; do
  if [[ ! -s "${work}/captured/${states[i]}.png" ]]; then
    echo "capture-screenshots.sh: ${states[i]} was not captured" >&2
    exit 1
  fi
  mv "${work}/captured/${states[i]}.png" "${out}/${assets[i]}"
done
echo "capture-screenshots.sh: captured ${#states[@]} documentation screenshot(s) into ${out}" >&2
