#!/usr/bin/env bash
set -euo pipefail

# capture-screenshots.sh - Captures the documentation's screenshots for a
# release: one UI screenshot matrix state per image in docs/images, named
# after it (docs/images/reader-light.png is the state reader-light), saved
# as OUT_DIR/zmail-docs-<image> (screenshots.py has the naming). The
# release workflow publishes them as release assets and the site build
# uses them (docs/site.md).
#
#   scripts/docs-site/capture-screenshots.sh OUT_DIR
#   scripts/docs-site/capture-screenshots.sh --check
#
# Fails, before capturing anything, when an image in docs/images is not a
# state of scripts/capture-release-screenshot.sh's matrix. --check does
# only that (no X server, no binary): the docs workflow runs it on every
# pull request, so an image the release cannot capture is caught there.
#
# Capturing needs what the matrix needs: a release build of zmail-app and
# an X server with room for the states' windows, for example
#
#   xvfb-run -a --server-args="-screen 0 2560x1440x24" \
#     scripts/docs-site/capture-screenshots.sh artifacts/docs-screenshots
#
# ZMAIL_APP_BINARY and ZMAIL_CAPTURE_LOG_DIR are passed on to the matrix.

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUT_DIR | --check" >&2
  exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
capture="${repo}/scripts/capture-release-screenshot.sh"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

bash "${capture}" --list > "${work}/states"
plan="$(python3 -I "${here}/screenshots.py" plan "${repo}/docs/images" "${work}/states")"
states=()
assets=()
while read -r state asset; do
  states+=("${state}")
  assets+=("${asset}")
done <<< "${plan}"
if [[ "$1" == "--check" ]]; then
  echo "capture-screenshots.sh: ${#states[@]} documentation screenshot(s), each a matrix state: ${states[*]}" >&2
  exit 0
fi

out="$1"
bash "${capture}" --matrix "${work}/captured" "${states[@]}"
mkdir -p "${out}"
for i in "${!states[@]}"; do
  if [[ ! -s "${work}/captured/${states[i]}.png" ]]; then
    echo "capture-screenshots.sh: the matrix did not capture ${states[i]}" >&2
    exit 1
  fi
  mv "${work}/captured/${states[i]}.png" "${out}/${assets[i]}"
done
echo "capture-screenshots.sh: captured ${#states[@]} documentation screenshot(s) into ${out}" >&2
