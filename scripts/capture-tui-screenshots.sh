#!/usr/bin/env bash
set -euo pipefail

# capture-tui-screenshots.sh - Renders the documentation's screenshots of
# the terminal UI: screens of the terminal UI's snapshot tests (synthetic
# mail, a fixed clock) with Zmail's version from crates/zmail-app/Cargo.toml
# in the title bar, drawn cell by cell into PNGs. No terminal, X server or
# network beyond fetching the fonts. docs/site.md explains the flow.
#
#   scripts/capture-tui-screenshots.sh OUT_DIR [STATE...]
#   scripts/capture-tui-screenshots.sh --list
#
# Writes OUT_DIR/STATE.png for each state (all of them by default), for
# example OUT_DIR/tui-message-index.png. --list prints the states, which
# are the names docs/images/tui-*.png must have.
#
# The images depend only on the code, Cargo.lock and the fonts below, so
# the same commit gives the same bytes. The fonts (SIL Open Font Licence,
# used only to draw the images, never shipped) are fetched from
# google/fonts at a pinned commit into ZMAIL_TUI_FONT_DIR (default
# target/tui-screenshot-fonts) and checked against their SHA-256; a copy
# already there is used when it matches.
#
# The renderer and the screens are in crates/zmail-tui/src/tests/
# (screenshot.rs, screenshots.rs); it runs as an ignored test.

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "${here}/.." && pwd)"

# The states, one per line; screenshots.rs's test checks they are its
# screenshots, in the same order.
STATES="$(cat <<'EOF'
# tui-states: begin
tui-message-index
tui-message-text
tui-compose-reply
# tui-states: end
EOF
)"
STATES="$(grep -v '^#' <<< "${STATES}")"

# google/fonts at this commit: NAME PATH SHA256. Bump the commit and the
# checksums together, then look at the images.
FONTS_COMMIT=06bfe0d62a98375cb09c942fa18e68bd32cff057
FONTS=(
  "JetBrainsMono[wght].ttf ofl/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf 48715a42ec242c21e9f02692891e147d022299a52e48d5e413e1a942193ffeda"
  "NotoSansJP[wght].ttf ofl/notosansjp/NotoSansJP%5Bwght%5D.ttf c2f3b4d463500a2ddcd3849cded1fceeb9fd6d1c32e6cbecd568453ba50fc68f"
  "NotoEmoji[wght].ttf ofl/notoemoji/NotoEmoji%5Bwght%5D.ttf de6c18832938afc99caf132b39d6a30a19bac7f2e812e28db2535b4608d27551"
)

if [[ $# -eq 1 && "$1" == "--list" ]]; then
  printf '%s\n' "${STATES}"
  exit 0
fi
if [[ $# -lt 1 || "$1" == -* ]]; then
  echo "usage: $0 OUT_DIR [STATE...] | --list" >&2
  exit 2
fi
out="$1"
shift
if [[ $# -gt 0 ]]; then
  wanted=("$@")
else
  mapfile -t wanted <<< "${STATES}"
fi
for state in "${wanted[@]}"; do
  if ! grep -qxF -- "${state}" <<< "${STATES}"; then
    echo "capture-tui-screenshots.sh: no state ${state} (--list prints them)" >&2
    exit 2
  fi
done

sha256() {
  sha256sum "$1" | cut -d' ' -f1
}

font_dir="${ZMAIL_TUI_FONT_DIR:-${repo}/target/tui-screenshot-fonts}"
mkdir -p "${font_dir}"
for font in "${FONTS[@]}"; do
  read -r name path sum <<< "${font}"
  file="${font_dir}/${name}"
  if [[ -f "${file}" && "$(sha256 "${file}")" == "${sum}" ]]; then
    continue
  fi
  echo "capture-tui-screenshots.sh: fetching ${name}" >&2
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --max-time 300 \
    -o "${file}.part" "https://raw.githubusercontent.com/google/fonts/${FONTS_COMMIT}/${path}"
  if [[ "$(sha256 "${file}.part")" != "${sum}" ]]; then
    rm -f "${file}.part"
    echo "capture-tui-screenshots.sh: ${name} does not match its SHA-256" >&2
    exit 1
  fi
  mv "${file}.part" "${file}"
done

mkdir -p "${out}"
out="$(cd "${out}" && pwd)"
font_dir="$(cd "${font_dir}" && pwd)"
cd "${repo}"
ZMAIL_TUI_SCREENSHOT_DIR="${out}" \
  ZMAIL_TUI_SCREENSHOT_FONTS="${font_dir}" \
  ZMAIL_TUI_SCREENSHOTS="${wanted[*]}" \
  cargo test -p zmail-tui --lib --locked -- --ignored --exact \
  tests::screenshots::capture_documentation_screenshots

missing=0
for state in "${wanted[@]}"; do
  if [[ ! -s "${out}/${state}.png" ]]; then
    echo "capture-tui-screenshots.sh: ${state}.png was not written" >&2
    missing=1
  fi
done
exit "${missing}"
