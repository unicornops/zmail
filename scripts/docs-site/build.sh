#!/usr/bin/env bash
set -euo pipefail

# build.sh - Builds one copy of the documentation site with mdBook from the
# repository this script is in (its working tree as it is, uncommitted
# changes included). docs/site.md explains the site.
#
#   scripts/docs-site/build.sh [OPTIONS] OUT_DIR
#
#   --ref REF          git ref that links to repository files point at on
#                      GitHub (default: main)
#   --site-url PATH    where this copy is served, such as / or /dev/
#                      (default: /); only the 404 page uses it
#   --title TITLE      the book's title (default: book.toml's, Zmail)
#   --banner MARKDOWN  markdown put at the top of every page
#   --canonical-url URL
#                      the copy's public address, such as
#                      https://docs.example/dev/: each page gets a canonical
#                      link to its page there (head.py)
#   --noindex          ask search engines not to index this copy
#
# OUT_DIR is replaced. Links are not checked here: build-site.sh checks the
# whole site, and check.sh checks any built folder.
#
# build-site.sh runs the build.sh of each release it publishes, so keep these
# options working (add new ones, do not change these).
#
# MDBOOK names the mdbook binary (default: mdbook on PATH; install-tools.sh
# fetches the pinned version).

usage() {
  sed -n '4,/^$/s/^# \{0,1\}//p' "${BASH_SOURCE[0]}" >&2
  exit 2
}

ref=main
site_url=/
title=
banner=
canonical_url=
noindex=false
out=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref) ref="${2:?--ref needs a value}"; shift 2 ;;
    --site-url) site_url="${2:?--site-url needs a value}"; shift 2 ;;
    --title) title="${2:?--title needs a value}"; shift 2 ;;
    --banner) banner="${2:?--banner needs a value}"; shift 2 ;;
    --canonical-url) canonical_url="${2:?--canonical-url needs a value}"; shift 2 ;;
    --noindex) noindex=true; shift ;;
    -h|--help) usage ;;
    -*) echo "build.sh: unknown option $1" >&2; usage ;;
    *)
      [[ -z "${out}" ]] || usage
      out="$1"; shift ;;
  esac
done
[[ -n "${out}" ]] || usage
[[ "${site_url}" == /* && "${site_url}" == */ ]] || {
  echo "build.sh: --site-url must start and end with / (got ${site_url})" >&2
  exit 2
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/../.." && pwd)"
out="$(realpath -m "${out}")"
case "${repo}/" in
  "${out}/"*)
    echo "build.sh: refusing to replace ${out}, which holds the repository" >&2
    exit 2 ;;
esac

stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT

stage_args=("${repo}" "${stage}/book" --ref "${ref}")
[[ -z "${banner}" ]] || stage_args+=(--banner "${banner}")
python3 -I "${here}/stage.py" "${stage_args[@]}"
cp "${here}/book.toml" "${stage}/book/"

rm -rf "${out}"
export MDBOOK_OUTPUT__HTML__SITE_URL="${site_url}"
[[ -z "${title}" ]] || export MDBOOK_BOOK__TITLE="${title}"
# mdBook prints warnings (a bare HTML tag in prose, say) and carries on;
# a warning fails the build here.
log="${stage}/mdbook.log"
"${MDBOOK:-mdbook}" build "${stage}/book" -d "${out}" 2>&1 | tee "${log}"
if grep -q ' WARN ' "${log}"; then
  echo "build.sh: mdBook printed warnings (above); fix them" >&2
  exit 1
fi
# The book's source folder is the staged tree, so mdBook copies its own
# config into the output.
rm -f "${out}/book.toml"
head_args=("${out}")
[[ -z "${canonical_url}" ]] || head_args+=(--canonical-url "${canonical_url}")
[[ "${noindex}" == "false" ]] || head_args+=(--noindex)
python3 -I "${here}/head.py" "${head_args[@]}"
echo "built ${out} (ref ${ref}, served at ${site_url})" >&2
