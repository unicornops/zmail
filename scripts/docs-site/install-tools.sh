#!/usr/bin/env bash
set -euo pipefail

# install-tools.sh - Downloads the pinned mdBook and lychee release
# binaries (x86-64 Linux), checks each archive against its SHA-256 below,
# and installs them into BIN_DIR.
#
#   scripts/docs-site/install-tools.sh BIN_DIR
#
# The docs workflow runs it; it works the same on a local x86-64 Linux
# machine (docs/site.md). Dependabot does not see these pins: bump the
# version and checksum together by hand, taking the checksum from the
# release page (GitHub lists each asset's SHA-256 digest), and build the
# site with the new version before merging.

MDBOOK_VERSION=0.5.4
MDBOOK_SHA256=5222beabd3e37dc5be0d18ff99b79058469354db5c220153a1b92db5ba12be89
LYCHEE_VERSION=0.24.2
LYCHEE_SHA256=73657a111819a30c47c08352896796f23d64e4eb2b3ed39b6d32149241566fc5

if [[ $# -ne 1 ]]; then
  echo "usage: $0 BIN_DIR" >&2
  exit 2
fi
if [[ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]]; then
  echo "install-tools.sh: only x86-64 Linux binaries are pinned; install mdBook ${MDBOOK_VERSION} and lychee ${LYCHEE_VERSION} another way (docs/site.md)" >&2
  exit 1
fi

bin="$1"
mkdir -p "${bin}"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# fetch NAME URL SHA256 MEMBER - downloads URL, checks it, and installs the
# archive member MEMBER as BIN_DIR/NAME.
fetch() {
  local name="$1" url="$2" sha256="$3" member="$4"
  local archive="${work}/${name}.tar.gz"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --retry 3 --output "${archive}" "${url}"
  if ! echo "${sha256}  ${archive}" | sha256sum --check --quiet; then
    echo "install-tools.sh: ${url} does not match its pinned SHA-256" >&2
    exit 1
  fi
  mkdir "${work}/${name}"
  tar -xzf "${archive}" -C "${work}/${name}" "${member}"
  install -m 0755 "${work}/${name}/${member}" "${bin}/${name}"
}

fetch mdbook \
  "https://github.com/rust-lang/mdBook/releases/download/v${MDBOOK_VERSION}/mdbook-v${MDBOOK_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
  "${MDBOOK_SHA256}" mdbook
fetch lychee \
  "https://github.com/lycheeverse/lychee/releases/download/lychee-v${LYCHEE_VERSION}/lychee-x86_64-unknown-linux-musl.tar.gz" \
  "${LYCHEE_SHA256}" lychee-x86_64-unknown-linux-musl/lychee

"${bin}/mdbook" --version
"${bin}/lychee" --version
