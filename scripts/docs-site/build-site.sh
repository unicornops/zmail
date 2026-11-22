#!/usr/bin/env bash
set -euo pipefail

# build-site.sh - Builds the whole documentation site as it is deployed,
# then checks it (check.sh). docs/site.md explains the layout:
#
#   /      the latest release: the newest vX.Y.Z tag that has this site
#          (scripts/docs-site/build.sh), built by that tag's own build.sh,
#          its links to repository files pointing at the tag
#   /dev/  this working tree (main, when CI deploys), titled
#          "Zmail (development)", with a note on every page that it
#          describes unreleased code
#
# Until a release has the site (releases made before it existed cannot be
# built), / is this working tree too, with a note saying no release has
# the site yet.
#
# Screenshots: / shows the release's own screenshots of the UI, the
# zmail-docs-*.png assets the release workflow captured and published
# (screenshots.py), each used only once its provenance attestation,
# checksum and PNG structure check out. An image without a usable asset
# keeps the committed copy from docs/images, and the log says why: 0.34.4
# and older have no such assets, and a failed download or check never
# fails the build. /dev/ always shows the committed copies, like the
# working tree it is built from.
#
#   scripts/docs-site/build-site.sh [OPTIONS] OUT_DIR
#
#   --dev-ref REF    git ref that /dev/'s links to repository files point
#                    at (default: main)
#   --release TAG    build / from TAG instead of the newest release with the
#                    site; "none" builds / from this working tree
#   --screenshots MODE
#                    where /'s screenshots come from: "download" (default:
#                    the release's assets, through release-screenshots.sh,
#                    which needs gh and the network; without them, the
#                    committed copies), "none" (the committed copies), or
#                    a folder holding already downloaded assets and their
#                    SHA256SUMS-Screenshots (checked as above except for
#                    the attestation)
#   --no-check       skip check.sh
#
# Needs the release tags (git fetch --tags). OUT_DIR is replaced. MDBOOK
# and LYCHEE name the binaries, as in build.sh and check.sh, and GH the
# GitHub CLI, as in release-screenshots.sh.

# The site's public address, used for each page's canonical link. Change
# it here (docs/site.md, One-time setup, has the hosting side).
SITE_ORIGIN=https://docs.zmail.unicornops.ie

usage() {
  sed -n '4,/^$/s/^# \{0,1\}//p' "${BASH_SOURCE[0]}" >&2
  exit 2
}

dev_ref=main
release=
screenshots=download
check=true
out=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dev-ref) dev_ref="${2:?--dev-ref needs a value}"; shift 2 ;;
    --release) release="${2:?--release needs a value}"; shift 2 ;;
    --screenshots) screenshots="${2:?--screenshots needs a value}"; shift 2 ;;
    --no-check) check=false; shift ;;
    -h|--help) usage ;;
    -*) echo "build-site.sh: unknown option $1" >&2; usage ;;
    *)
      [[ -z "${out}" ]] || usage
      out="$1"; shift ;;
  esac
done
[[ -n "${out}" ]] || usage
case "${screenshots}" in
  download|none) ;;
  *)
    [[ -d "${screenshots}" ]] || {
      echo "build-site.sh: --screenshots takes download, none or a folder (got ${screenshots})" >&2
      exit 2
    }
    screenshots="$(cd "${screenshots}" && pwd)" ;;
esac

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
out="$(realpath -m "${out}")"
site_marker=scripts/docs-site/build.sh

# The newest release tag whose tree has the site, or nothing.
latest_site_release() {
  local tag
  while IFS= read -r tag; do
    [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
    if git -C "${repo}" cat-file -e "${tag}^{commit}:${site_marker}" 2>/dev/null; then
      echo "${tag}"
      return 0
    fi
  done < <(git -C "${repo}" tag --list 'v*' --sort=-version:refname)
}

if [[ -z "${release}" ]]; then
  release="$(latest_site_release)"
  [[ -n "${release}" ]] || release=none
elif [[ "${release}" != "none" ]] &&
  ! git -C "${repo}" cat-file -e "${release}^{commit}:${site_marker}" 2>/dev/null; then
  echo "build-site.sh: ${release} is not a ref with ${site_marker}" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

if [[ "${release}" == "none" ]]; then
  root_from="this working tree (no release has the site yet)"
  # shellcheck disable=SC2016 # the backticks are markdown
  root_note='> [!NOTE]
> No Zmail release includes this documentation site yet, so these pages describe the `main` branch, which may be ahead of [the latest release](https://github.com/unicornops/zmail/releases/latest).'
  "${here}/build.sh" --ref "${dev_ref}" --site-url / --banner "${root_note}" \
    --canonical-url "${SITE_ORIGIN}/" "${out}"
  dev_note="${root_note}"
else
  root_from="${release}"
  # The tag's own scripts and config build it, as they were released.
  mkdir "${work}/release"
  git -C "${repo}" archive --format=tar "${release}^{commit}" | tar -xf - -C "${work}/release"
  bash "${work}/release/${site_marker}" --ref "${release}" --site-url / \
    --canonical-url "${SITE_ORIGIN}/" "${out}"
  # shellcheck disable=SC2016 # the backticks are markdown
  dev_note='> [!WARNING]
> These pages describe the development version of Zmail, the unreleased `main` branch, so some of what they say may not be in a release yet. See [the documentation for the latest release](/).'

  # The release's own screenshots over the committed copies in its build.
  assets="${work}/screenshots"
  case "${screenshots}" in
    none) shots_why="not asked for" ;;
    download)
      if "${here}/release-screenshots.sh" "${release}" "${assets}"; then
        shots_why=""
      else
        shots_why="none downloaded"
      fi ;;
    *)
      assets="${screenshots}"
      shots_why="" ;;
  esac
  if [[ -n "${shots_why}" ]]; then
    assets="${work}/no-screenshots"
  fi
  if [[ -d "${out}/docs/images" ]]; then
    shots_used="$(python3 -I "${here}/screenshots.py" apply "${out}/docs/images" "${assets}")"
  else
    shots_used="0 of 0"
  fi
  root_from="${root_from} (screenshots: ${shots_used} from the release's assets${shots_why:+, ${shots_why}}; any others are the committed copies)"
fi

if [[ -e "${out}/dev" ]]; then
  echo "build-site.sh: the build of / has its own dev/, which /dev/ would replace" >&2
  exit 1
fi
"${here}/build.sh" --ref "${dev_ref}" --site-url /dev/ \
  --title "Zmail (development)" --banner "${dev_note}" \
  --canonical-url "${SITE_ORIGIN}/dev/" --noindex "${out}/dev"

summary="Documentation site in ${out}: / from ${root_from}, /dev/ from this working tree (links to ${dev_ref})."
echo "${summary}" >&2
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  echo "${summary}" >> "${GITHUB_STEP_SUMMARY}"
fi

if [[ "${check}" == "true" ]]; then
  "${here}/check.sh" "${out}"
fi
