#!/usr/bin/env bash
set -euo pipefail

# release-screenshots.sh - Downloads a release's documentation screenshots
# (zmail-docs-*.png and SHA256SUMS-Screenshots, see screenshots.py) into a
# new folder and checks each file's build provenance attestation: it must
# have been made by this repository's release workflow on a GitHub-hosted
# runner. A file that fails the check is deleted, with a warning.
# docs/site.md explains the flow.
#
#   scripts/docs-site/release-screenshots.sh TAG DEST_DIR
#
# Exits 0 with the verified files in DEST_DIR (which must not exist yet),
# or non-zero saying why there are none: no gh, a release from before the
# release published screenshots, or a failed download. The caller then
# keeps the committed images. The files are still untrusted after this:
# screenshots.py checks them against SHA256SUMS-Screenshots and as PNGs
# before using any.
#
# Uses the GitHub CLI (GH names it; default gh on PATH) with its usual
# credentials (GH_TOKEN in CI; contents: read and attestations: read are
# enough).

REPO=unicornops/zmail
SIGNER_WORKFLOW="${REPO}/.github/workflows/release-please.yml"
SUMS=SHA256SUMS-Screenshots

if [[ $# -ne 2 ]]; then
  echo "usage: $0 TAG DEST_DIR" >&2
  exit 2
fi
tag="$1"
dest="$2"
gh="${GH:-gh}"

if ! command -v "${gh}" >/dev/null 2>&1; then
  echo "release-screenshots.sh: no GitHub CLI (${gh})" >&2
  exit 1
fi
if [[ -e "${dest}" ]]; then
  echo "release-screenshots.sh: ${dest} already exists" >&2
  exit 2
fi

# Network calls get a deadline, so a hung request cannot stall the build.
run() {
  timeout 120 "${gh}" "$@"
}

if ! names="$(run release view "${tag}" --repo "${REPO}" --json assets --jq '.assets[].name')"; then
  echo "release-screenshots.sh: could not read release ${tag}" >&2
  exit 1
fi
if ! grep -qx "${SUMS}" <<< "${names}"; then
  echo "release-screenshots.sh: release ${tag} has no ${SUMS} (it was made before releases published documentation screenshots)" >&2
  exit 1
fi

mkdir -p "${dest}"
if ! run release download "${tag}" --repo "${REPO}" --dir "${dest}" \
  --pattern "${SUMS}" --pattern 'zmail-docs-*.png'; then
  echo "release-screenshots.sh: downloading ${tag}'s screenshots failed" >&2
  rm -rf "${dest}"
  exit 1
fi

verified=0
for file in "${dest}"/*; do
  name="${file##*/}"
  if run attestation verify "${file}" --repo "${REPO}" \
    --signer-workflow "${SIGNER_WORKFLOW}" --deny-self-hosted-runners >/dev/null; then
    verified=$((verified + 1))
  else
    echo "::warning title=Release screenshot not verified::${tag}'s ${name} has no valid build provenance attestation from ${SIGNER_WORKFLOW}; not using it"
    rm -f "${file}"
  fi
done
if [[ ! -f "${dest}/${SUMS}" ]]; then
  echo "release-screenshots.sh: ${tag}'s ${SUMS} could not be verified" >&2
  rm -rf "${dest}"
  exit 1
fi
echo "release-screenshots.sh: ${verified} file(s) from ${tag} verified" >&2
