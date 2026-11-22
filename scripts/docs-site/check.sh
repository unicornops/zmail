#!/usr/bin/env bash
set -euo pipefail

# check.sh - Checks a built documentation site (docs/site.md).
#
#   scripts/docs-site/check.sh SITE_DIR              links inside the site
#   scripts/docs-site/check.sh --external SITE_DIR   links to other sites
#
# Without --external, offline: every link between pages, to an image or to
# a heading (#anchor) must resolve inside SITE_DIR, which is checked as the
# root of the site (so /dev/ is SITE_DIR/dev/), and no page may load
# anything (a script, stylesheet, image or font) from another origin.
# This is the check pull requests get.
#
# With --external, online: every http(s) link, including those to source
# files on GitHub, must answer. Outside sites come and go, so this runs on
# a schedule rather than on pull requests. Set GITHUB_TOKEN so GitHub does
# not rate-limit the requests.
#
# LYCHEE names the lychee binary (default: lychee on PATH;
# install-tools.sh fetches the pinned version).

external=false
if [[ "${1:-}" == "--external" ]]; then
  external=true
  shift
fi
if [[ $# -ne 1 || ! -f "${1}/index.html" ]]; then
  echo "usage: $0 [--external] SITE_DIR (a built site, with index.html at its top)" >&2
  exit 2
fi
site="$(cd "$1" && pwd)"
lychee="${LYCHEE:-lychee}"

if [[ "${external}" == "true" ]]; then
  # The site's own address (each page's canonical link, from build-site.sh)
  # is not another site, and is not live before the first deploy.
  own=()
  origin="$(sed -n 's|.*<link rel="canonical" href="\(https://[^/"]*/\).*|\1|p' "${site}/index.html" | head -n 1)"
  if [[ -n "${origin}" ]]; then
    own=(--exclude "^${origin//./\\.}")
  fi
  # Fragments are not checked online: GitHub draws line anchors (#L10)
  # with JavaScript, so they never appear in the HTML lychee reads.
  exec "${lychee}" --no-progress --format detailed \
    --scheme https --scheme http --exclude-loopback "${own[@]}" \
    --max-concurrency 8 --max-retries 3 \
    --root-dir "${site}" "${site}/**/*.html"
fi

# Third-party resources: anything a page loads from an absolute or
# protocol-relative URL. Links to other sites (<a href>, and the canonical
# <link> head.py adds) are fine.
third_party="$(
  grep -rnoE --include='*.html' \
    '<(script|link|img|iframe|source|video|audio|embed|object)\b[^>]*\b(src|href|data|srcset)="(https?:)?//[^"]*"' \
    "${site}" | grep -v '<link rel="canonical" ' || true
  grep -rnoE --include='*.css' '(url\(|@import)[^)]*(https?:)?//[^)]*' "${site}" || true
)"
# A Rust playground block makes mdBook's script call play.rust-lang.org.
playground="$(grep -rlE --include='*.html' 'class="playground"' "${site}" || true)"
if [[ -n "${third_party}" || -n "${playground}" ]]; then
  echo "check.sh: these pages load something from another origin; the site must not:" >&2
  [[ -z "${third_party}" ]] || echo "${third_party}" >&2
  [[ -z "${playground}" ]] || echo "Rust playground blocks in: ${playground}" >&2
  exit 1
fi

# --index-files: a link to a folder must reach that folder's index.html,
# not just an existing folder.
"${lychee}" --offline --include-fragments --index-files index.html \
  --no-progress --format detailed \
  --root-dir "${site}" "${site}/**/*.html"
