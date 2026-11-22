#!/usr/bin/env bash
set -euo pipefail

# refresh-images.sh - Replaces the committed screenshots in docs/images
# (the ones GitHub shows in the README and the docs, and /dev/ uses) with a
# release's own, checked exactly as the site build checks them
# (release-screenshots.sh, then screenshots.py). Commit the result in a
# pull request; nothing refreshes them automatically. docs/site.md
# explains the flow.
#
#   scripts/docs-site/refresh-images.sh [TAG]
#
# TAG defaults to the latest release. An image the release has no usable
# screenshot for is left as it is. Needs the GitHub CLI (GH names it) and
# the network.

if [[ $# -gt 1 ]]; then
  echo "usage: $0 [TAG]" >&2
  exit 2
fi
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
tag="${1:-}"
if [[ -z "${tag}" ]]; then
  tag="$("${GH:-gh}" release view --repo unicornops/zmail --json tagName --jq .tagName)"
fi

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
"${here}/release-screenshots.sh" "${tag}" "${work}/assets"
used="$(python3 -I "${here}/screenshots.py" apply "${repo}/docs/images" "${work}/assets")"
echo "refresh-images.sh: ${used} image(s) in docs/images now from ${tag}; review them with git diff --stat docs/images" >&2
