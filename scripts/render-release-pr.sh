#!/usr/bin/env bash
set -euo pipefail

# render-release-pr.sh - Renders the zmail release PR from the full commit
# log, closing the release-please workspace gap (issue #67).
#
# Release Please's cargo-workspace plugin counts only commits that touch
# the released package path (crates/zmail-app), so conventional commits
# that live entirely in sub-crates, scripts/, or .github/ never open a
# release PR and never reach the changelog. This script bridges the gap:
#
#   1. When a release PR is open on the release-please branch, the pending
#      changelog section is regenerated from every conventional feat/fix
#      commit in the release window - app-crate and sub-crate alike - and
#      pushed to the PR branch.
#   2. When no release PR is open but the window holds unreleased
#      conventional commits, the release PR is bootstrapped exactly as
#      release-please would: version bump in Cargo.toml/Cargo.lock and the
#      manifest, changelog section, PR titled chore(main): release X.Y.Z
#      with the autorelease: pending label.
#   3. Otherwise the script is a no-op.
#
# Merging a bootstrapped PR is recognized by release-please through the
# autorelease: pending label, which takes over tagging, release notes,
# and the asset builds.
#
# Requires: a full clone with tags, git, gh in PATH, GH_TOKEN set.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

MANIFEST=".github/.release-please-manifest.json"
PACKAGE_PATH="crates/zmail-app"
CHANGELOG="crates/zmail-app/CHANGELOG.md"
BRANCH="release-please--branches--main--components--zmail"
LABEL="autorelease: pending"

cd "$REPO_ROOT"

# -----------------------------------------------------------------------------
# Repository URL for changelog links, derived from the origin remote.
# -----------------------------------------------------------------------------
repo_url() {
  local url
  url="$(git remote get-url origin)"
  url="${url%.git}"
  case "$url" in
    git@github.com:*) url="https://github.com/${url#git@github.com:}" ;;
  esac
  printf '%s' "$url"
}
REPO_URL="$(repo_url)"

# The last released version, read from the manifest on main.
manifest_version() {
  sed -n 's|.*"'"${PACKAGE_PATH}"'": "\([^"]*\)".*|\1|p' "$MANIFEST"
}

# -----------------------------------------------------------------------------
# Collect the release window: every conventional feat/fix commit since the
# last release tag. git log emits records separated by US (0x1e), each
# holding "sha<TAB>subject" then the commit body.
# -----------------------------------------------------------------------------
PREV_VERSION="$(manifest_version)"
PREV_TAG="v${PREV_VERSION}"
if ! git rev-parse -q --verify "refs/tags/${PREV_TAG}" >/dev/null; then
  echo "Error: manifest says ${PREV_VERSION} but tag ${PREV_TAG} does not exist" >&2
  exit 1
fi

declare -a FEATURE_ENTRIES=() FIX_ENTRIES=() BREAKING_ENTRIES=()
HAS_FEATURE=0
HAS_FIX=0
HAS_BREAKING=0

# Conventional subject: type(scope)!: description
CONVENTIONAL_RX='^(feat|fix)(\(([^)]+)\))?(!)?: (.+)$'

add_entry() {
  local sha="$1" subject="$2" scope="$3" closes_issue="$4" kind="$5"
  local pr_number="" entry=""

  # Squash-merge subjects end in " (#N)"; split it off for the issue link.
  if [[ "$subject" =~ \ \(#([0-9]+)\)$ ]]; then
    pr_number="${BASH_REMATCH[1]}"
    subject="${subject% \(*}"
  fi

  if [ -n "$scope" ]; then
    entry="* **${scope}:** ${subject}"
  else
    entry="* ${subject}"
  fi
  if [ -n "$pr_number" ]; then
    entry+=" ([#${pr_number}](${REPO_URL}/issues/${pr_number}))"
  fi
  entry+=" ([${sha:0:7}](${REPO_URL}/commit/${sha}))"
  if [ -n "$closes_issue" ]; then
    entry+=", closes [#${closes_issue}](${REPO_URL}/issues/${closes_issue})"
  fi

  case "$kind" in
    breaking) BREAKING_ENTRIES+=("$entry") ;;
    feature) FEATURE_ENTRIES+=("$entry") ;;
    fix) FIX_ENTRIES+=("$entry") ;;
  esac
}

while IFS= read -r -d $'\x1e' header; do
  IFS= read -r -d $'\x1e' body
  # git log separates entries with a newline, which lands at the head of
  # the next record; strip it before splitting sha and subject.
  header="${header#$'\n'}"
  sha="${header%%$'\t'*}"
  subject="${header#*$'\t'}"

  # Conventional subject: type(scope)!: description
  [[ "$subject" =~ $CONVENTIONAL_RX ]] || continue
  type="${BASH_REMATCH[1]}"
  scope="${BASH_REMATCH[3]}"
  breaking="${BASH_REMATCH[4]}"
  subject="${BASH_REMATCH[5]}"

  # Conventional "Closes #N" footer from the commit body becomes a link.
  closes_issue=""
  while IFS= read -r body_line; do
    if [ -z "$closes_issue" ] && [[ "$body_line" =~ ^[Cc]loses\ \#([0-9]+)$ ]]; then
      closes_issue="${BASH_REMATCH[1]}"
    fi
  done <<<"$body"

  if [ -n "$breaking" ] || grep -qi '^BREAKING CHANGE:' <<<"$body"; then
    HAS_BREAKING=1
    add_entry "$sha" "$subject" "$scope" "$closes_issue" breaking
  elif [ "$type" == "feat" ]; then
    HAS_FEATURE=1
    add_entry "$sha" "$subject" "$scope" "$closes_issue" feature
  else
    HAS_FIX=1
    add_entry "$sha" "$subject" "$scope" "$closes_issue" fix
  fi
done < <(git log --no-merges --format='%H%x09%s%x1e%b%x1e' "${PREV_TAG}..HEAD")

if [ "$HAS_BREAKING" -eq 0 ] && [ "$HAS_FEATURE" -eq 0 ] && [ "$HAS_FIX" -eq 0 ]; then
  echo "No unreleased conventional commits since ${PREV_TAG}; nothing to do."
  exit 0
fi

# -----------------------------------------------------------------------------
# Next version: breaking bumps major, feat bumps minor, fix bumps patch.
# -----------------------------------------------------------------------------
IFS='.' read -r MAJOR MINOR PATCH <<<"$PREV_VERSION"
if [ "$HAS_BREAKING" -eq 1 ]; then
  NEXT_VERSION="$((MAJOR + 1)).0.0"
elif [ "$HAS_FEATURE" -eq 1 ]; then
  NEXT_VERSION="${MAJOR}.$((MINOR + 1)).0"
else
  NEXT_VERSION="${MAJOR}.${MINOR}.$((PATCH + 1))"
fi
NEXT_TAG="v${NEXT_VERSION}"
TODAY="$(date +%F)"
echo "Release window: ${PREV_TAG}..HEAD -> ${NEXT_TAG}"

# -----------------------------------------------------------------------------
# Render the pending changelog section in release-please's format.
# -----------------------------------------------------------------------------
SECTION_FILE="$(mktemp)"
{
  echo "## [${NEXT_VERSION}](${REPO_URL}/compare/${PREV_TAG}...${NEXT_TAG}) (${TODAY})"
  echo ""
  echo ""
  if [ "$HAS_BREAKING" -eq 1 ]; then
    echo "### BREAKING CHANGES"
    echo ""
    printf '%s\n' "${BREAKING_ENTRIES[@]}"
    echo ""
  fi
  if [ "${#FEATURE_ENTRIES[@]}" -gt 0 ]; then
    echo "### Features"
    echo ""
    printf '%s\n' "${FEATURE_ENTRIES[@]}"
    echo ""
  fi
  if [ "${#FIX_ENTRIES[@]}" -gt 0 ]; then
    echo "### Bug Fixes"
    echo ""
    printf '%s\n' "${FIX_ENTRIES[@]}"
    echo ""
  fi
} > "$SECTION_FILE"

# Rebuild the changelog: title line, blank line, the new section, then the
# previous releases. In complete mode the pending section release-please
# generated (app-crate commits only) is the first section and is replaced;
# in bootstrap mode the previous release header is the first section.
rewrite_changelog() {
  local header_index="$1"
  {
    head -n 1 "$CHANGELOG"
    echo ""
    cat "$SECTION_FILE"
    tail -n +"$header_index" "$CHANGELOG"
  } > "${CHANGELOG}.new"
  mv "${CHANGELOG}.new" "$CHANGELOG"
}

# -----------------------------------------------------------------------------
# Mode selection: an open release PR completes its changelog; otherwise
# bootstrap the release PR from main. Both modes commit, so identity is
# configured for the checkout first; CI runners have no global git user.
# -----------------------------------------------------------------------------
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
OPEN_PR_NUMBER="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number // ""')"

if [ -n "$OPEN_PR_NUMBER" ]; then
  echo "Completing the changelog of release PR #${OPEN_PR_NUMBER}"
  git fetch origin "$BRANCH"
  git checkout FETCH_HEAD
  PR_VERSION="$(manifest_version)"
  if [ "$PR_VERSION" != "$NEXT_VERSION" ]; then
    echo "Error: open PR #${OPEN_PR_NUMBER} targets ${PR_VERSION} but the window demands ${NEXT_VERSION}" >&2
    exit 1
  fi
  header_index="$(grep -n '^## \[' "$CHANGELOG" | sed -n '2p' | cut -d: -f1)"
  if [ -z "$header_index" ]; then
    echo "Error: could not locate a previous release header under the pending section" >&2
    exit 1
  fi
  rewrite_changelog "$header_index"
  if git diff --quiet; then
    echo "The pending changelog section is already complete."
    exit 0
  fi
  git add "$CHANGELOG"
  git commit -m "chore: complete the ${NEXT_VERSION} changelog with workspace-wide changes"
  git push origin "HEAD:${BRANCH}"
  echo "Completed the changelog of release PR #${OPEN_PR_NUMBER}"
  exit 0
fi

echo "Bootstrapping the ${NEXT_VERSION} release PR"
git checkout -B "$BRANCH" origin/main

sed -i "s/^version = \"${PREV_VERSION}\"/version = \"${NEXT_VERSION}\"/" "${PACKAGE_PATH}/Cargo.toml"
LOCK_LINE="$(grep -n 'name = "zmail-app"' Cargo.lock | head -n 1 | cut -d: -f1)"
sed -i "$((LOCK_LINE + 1))s/version = \"${PREV_VERSION}\"/version = \"${NEXT_VERSION}\"/" Cargo.lock
sed -i "s|\"${PACKAGE_PATH}\": \"${PREV_VERSION}\"|\"${PACKAGE_PATH}\": \"${NEXT_VERSION}\"|" "$MANIFEST"

header_index="$(grep -n '^## \[' "$CHANGELOG" | head -n 1 | cut -d: -f1)"
rewrite_changelog "$header_index"

git add "$CHANGELOG" "$MANIFEST" Cargo.lock "${PACKAGE_PATH}/Cargo.toml"
git commit -m "chore(main): release ${NEXT_VERSION}"
# The release-please branch is automation-owned and is recreated from main
# each release cycle; a stale branch from a closed PR is replaced.
if ! git push origin "HEAD:${BRANCH}"; then
  echo "Push was not fast-forward; recreating the release branch from main"
  git push --force origin "HEAD:${BRANCH}"
fi

PR_BODY="$(mktemp)"
{
  echo ":robot: I have created a release *beep* *boop*"
  echo "---"
  echo ""
  cat "$SECTION_FILE"
  echo "The changelog was rendered from the full release window by"
  echo "scripts/render-release-pr.sh, which covers workspace changes outside"
  echo "crates/zmail-app that release-please does not count (issue #67)."
  echo ""
  echo "---"
  echo "This PR was generated with [Release Please](https://github.com/googleapis/release-please). See [documentation](https://github.com/googleapis/release-please#release-please)."
} > "$PR_BODY"

gh pr create --base main --head "$BRANCH" \
  --title "chore(main): release ${NEXT_VERSION}" \
  --body-file "$PR_BODY" \
  --label "$LABEL"
echo "Bootstrapped the ${NEXT_VERSION} release PR"
