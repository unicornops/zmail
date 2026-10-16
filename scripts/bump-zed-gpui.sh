#!/usr/bin/env bash
set -euo pipefail

# bump-zed-gpui.sh - Moves the GPUI git pin to a newer Zed stable release.
#
# Usage: scripts/bump-zed-gpui.sh [TAG]
#
# With no TAG the latest Zed stable release is used (GitHub's
# releases/latest, which never returns drafts or pre-releases). When that
# release is not newer than the tag pinned in Cargo.toml the script is a
# no-op. Otherwise it:
#
#   1. points every `tag = "v..."` on the gpui/gpui_platform lines of the
#      workspace Cargo.toml at the new tag,
#   2. moves rust-toolchain.toml (and the workspace rust-version) to the
#      toolchain Zed pins at that tag,
#   3. re-syncs the mirrored [patch.crates-io] revisions (async-task,
#      calloop) from Zed's root Cargo.toml,
#   4. refreshes Cargo.lock: `cargo update -p gpui -p gpui_platform` first,
#      a full `cargo update` only when the targeted update cannot resolve,
#   5. rewrites the pin table in docs/upstream/zed.md.
#
# It writes a pull request body to $PR_BODY_FILE (default: a temp file)
# and, when $GITHUB_OUTPUT is set, the outputs updated, tag, previous_tag,
# commit and body_file. See docs/upstream/zed.md.
#
# Requires: gh (authenticated; read access to public repos is enough),
# cargo, python3 >= 3.11, sed.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ZED_REPO="zed-industries/zed"
ZED_URL="https://github.com/${ZED_REPO}"
DOC="docs/upstream/zed.md"
BODY_FILE="${PR_BODY_FILE:-$(mktemp)}"

cd "$REPO_ROOT"

output() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s=%s\n' "$1" "$2" >> "$GITHUB_OUTPUT"
  fi
}

zed_raw() {
  # zed_raw PATH REF - prints a file from the Zed repository at REF.
  gh api -H "Accept: application/vnd.github.raw" "repos/${ZED_REPO}/contents/$1?ref=$2"
}

current_tag="$(sed -n 's|^gpui = { git = "https://github.com/zed-industries/zed", tag = "\(v[^"]*\)".*|\1|p' Cargo.toml)"
if [[ -z "$current_tag" ]]; then
  echo "error: could not find the gpui tag pin in Cargo.toml" >&2
  exit 1
fi

target_tag="${1:-}"
if [[ -z "$target_tag" ]]; then
  target_tag="$(gh api "repos/${ZED_REPO}/releases/latest" \
    --jq 'select(.prerelease == false and .draft == false) | .tag_name')"
fi
if [[ ! "$target_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: '${target_tag}' is not a Zed stable release tag (vX.Y.Z)" >&2
  exit 1
fi

newest="$(printf '%s\n%s\n' "$current_tag" "$target_tag" | sort -V | tail -n 1)"
if [[ "$target_tag" == "$current_tag" || "$newest" != "$target_tag" ]]; then
  echo "GPUI is pinned to ${current_tag}; ${target_tag} is not newer. Nothing to do."
  output updated false
  exit 0
fi

echo "Bumping GPUI from Zed ${current_tag} to ${target_tag}"

release_json="$(gh api "repos/${ZED_REPO}/releases/tags/${target_tag}")"
if [[ "$(jq -r '.prerelease' <<<"$release_json")" != "false" ]]; then
  echo "error: ${target_tag} is a pre-release; only stable releases are pinned" >&2
  exit 1
fi
released_on="$(jq -r '.published_at[0:10]' <<<"$release_json")"
commit="$(gh api "repos/${ZED_REPO}/commits/${target_tag}" --jq '.sha')"

# 1. The tag on the gpui lines.
sed -i -E "/^gpui(_platform)? = \{ git = \"https:\/\/github.com\/zed-industries\/zed\"/s/tag = \"v[^\"]*\"/tag = \"${target_tag}\"/" Cargo.toml

# 2. The Rust toolchain Zed builds that tag with.
zed_toolchain="$(zed_raw rust-toolchain.toml "$target_tag" | sed -n 's/^channel = "\(.*\)"/\1/p')"
current_toolchain="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
if [[ "$zed_toolchain" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ && "$zed_toolchain" != "$current_toolchain" ]]; then
  sed -i "s/^channel = \".*\"/channel = \"${zed_toolchain}\"/" rust-toolchain.toml
  rust_version="$(cut -d. -f1-2 <<<"$zed_toolchain")"
  sed -i "s/^rust-version = \".*\"/rust-version = \"${rust_version}\"/" Cargo.toml
fi

# 3. The mirrored [patch.crates-io] entries.
zed_cargo="$(mktemp)"
zed_raw Cargo.toml "$target_tag" > "$zed_cargo"
patch_notes="$(python3 - "$zed_cargo" Cargo.toml <<'PY'
import re, sys, tomllib

zed_path, ours_path = sys.argv[1], sys.argv[2]
with open(zed_path, "rb") as f:
    zed_patches = tomllib.load(f).get("patch", {}).get("crates-io", {})
with open(ours_path, "rb") as f:
    our_patches = tomllib.load(f).get("patch", {}).get("crates-io", {})
text = open(ours_path, encoding="utf-8").read()
notes = []
for name, ours in our_patches.items():
    theirs = zed_patches.get(name)
    if theirs is None:
        notes.append(f"- Zed no longer patches `{name}`; review the mirrored patch in Cargo.toml.")
        continue
    if theirs.get("git") != ours.get("git") or theirs.get("rev") != ours.get("rev"):
        line = f'{name} = {{ git = "{theirs["git"]}", rev = "{theirs["rev"]}" }}'
        text = re.sub(rf"^{re.escape(name)} = \{{.*\}}$", line, text, count=1, flags=re.M)
        notes.append(f"- Re-synced the `{name}` patch to `{theirs['git']}` at `{theirs['rev'][:12]}`.")
others = sorted(set(zed_patches) - set(our_patches))
if others:
    notes.append("- Zed also patches " + ", ".join(f"`{n}`" for n in others)
                 + "; check that none of them is now in GPUI's dependency graph.")
open(ours_path, "w", encoding="utf-8").write(text)
print("\n".join(notes))
PY
)"
rm -f "$zed_cargo"

# 4. The lockfile.
lock_note="Cargo.lock was refreshed with \`cargo update -p gpui -p gpui_platform\`."
if ! cargo update -p gpui -p gpui_platform; then
  echo "Targeted update could not resolve; falling back to a full cargo update"
  cargo update
  lock_note="The targeted \`cargo update -p gpui -p gpui_platform\` could not resolve, so Cargo.lock was refreshed with a full \`cargo update\`; review the non-GPUI version changes."
fi
cargo metadata --locked --format-version 1 > /dev/null

# 5. The pin table in docs/upstream/zed.md.
toolchain_now="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
python3 - "$DOC" "$target_tag" "$commit" "$released_on" "$toolchain_now" <<'PY'
import re, sys
doc, tag, commit, released, toolchain = sys.argv[1:]
url = "https://github.com/zed-industries/zed"
table = "\n".join([
    "<!-- zed-pin:start -->",
    "| | |",
    "| --- | --- |",
    f"| Zed release tag | [`{tag}`]({url}/releases/tag/{tag}) |",
    f"| Commit | [`{commit}`]({url}/commit/{commit}) |",
    f"| Released | {released} |",
    f"| Rust toolchain (Zed's `rust-toolchain.toml`) | `{toolchain}` |",
    "<!-- zed-pin:end -->",
])
text = open(doc, encoding="utf-8").read()
new, count = re.subn(r"<!-- zed-pin:start -->.*?<!-- zed-pin:end -->", table, text, flags=re.S)
if count != 1:
    sys.exit(f"error: {doc} has no zed-pin marker block")
open(doc, "w", encoding="utf-8").write(new)
PY

# Release notes for every stable release in (current, target].
release_links="$(gh api "repos/${ZED_REPO}/releases?per_page=100" \
  --jq '.[] | select(.prerelease == false and .draft == false) | .tag_name' \
  | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V \
  | awk -v lo="$current_tag" -v hi="$target_tag" '
      function cmp(a, b,   x, y, i) {
        sub(/^v/, "", a); sub(/^v/, "", b)
        split(a, x, "."); split(b, y, ".")
        for (i = 1; i <= 3; i++) if (x[i] + 0 != y[i] + 0) return (x[i] + 0 < y[i] + 0) ? -1 : 1
        return 0
      }
      cmp($0, lo) > 0 && cmp($0, hi) <= 0' \
  | while read -r tag; do
      printf -- '- [%s release notes](%s/releases/tag/%s)\n' "$tag" "$ZED_URL" "$tag"
    done)"

toolchain_note="The Rust toolchain stays at \`${current_toolchain}\`."
if [[ "$toolchain_now" != "$current_toolchain" ]]; then
  toolchain_note="Rust toolchain: \`${current_toolchain}\` → \`${toolchain_now}\` (Zed's \`rust-toolchain.toml\` at ${target_tag}); CI reads it from \`rust-toolchain.toml\`."
fi

cat > "$BODY_FILE" <<EOF
Moves GPUI from Zed [\`${current_tag}\`](${ZED_URL}/releases/tag/${current_tag}) to the latest stable release, [\`${target_tag}\`](${ZED_URL}/releases/tag/${target_tag}) (released ${released_on}, commit [\`${commit:0:12}\`](${ZED_URL}/commit/${commit})).

Opened by the weekly \`zed-upstream\` workflow (\`scripts/bump-zed-gpui.sh\`); see \`docs/upstream/zed.md\`.

## Release notes

${release_links:-- [${target_tag} release notes](${ZED_URL}/releases/tag/${target_tag})}
- [Full diff ${current_tag}...${target_tag}](${ZED_URL}/compare/${current_tag}...${target_tag}) (GPUI lives in \`crates/gpui*\`)

## Changes

- \`Cargo.toml\`: \`gpui\` and \`gpui_platform\` now use \`tag = "${target_tag}"\`.
- ${toolchain_note}
- ${lock_note}
${patch_notes}
- \`docs/upstream/zed.md\`: pin table updated.

## Before merging

- [ ] CI is green on Linux, macOS and Windows. If this PR was opened with the default \`GITHUB_TOKEN\` (no \`RELEASE_PLEASE_TOKEN\` secret), CI did not start on its own: close and reopen the PR, or push to the branch, to run it.
- [ ] Any GPUI API breakage is fixed on this branch.
- [ ] \`cargo deny check sources\` passes; a new git dependency needs a narrow \`allow-git\` entry in \`deny.toml\`.
- [ ] A local GUI launch (or the release screenshot script) still renders.
EOF

echo "PR body written to ${BODY_FILE}"
output updated true
output tag "$target_tag"
output previous_tag "$current_tag"
output commit "$commit"
output body_file "$BODY_FILE"
