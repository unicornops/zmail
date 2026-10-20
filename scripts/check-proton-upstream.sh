#!/usr/bin/env bash
set -euo pipefail

# check-proton-upstream.sh - Reports upstream changes to the Proton code
# zmail ports.
#
# Usage: scripts/check-proton-upstream.sh [BRIDGE_TAG]
#
# zmail's Proton support mirrors Proton Mail Bridge and go-proton-api at the
# commits pinned in docs/upstream/proton.md (between the proton-pin
# markers); docs/upstream/proton-ports.toml lists which upstream files were
# ported, and to where. With no BRIDGE_TAG the latest Bridge release on
# GitHub is used (releases/latest never returns drafts or pre-releases).
# The script:
#
#   1. reads the pins and the ported-file manifest,
#   2. resolves Bridge's release commit and, from Bridge's go.mod at that
#      release, the commit of every other pinned repository (go-proton-api
#      and any further ProtonMail module pinned later),
#   3. compares each pin with its new commit
#      (gh api repos/ProtonMail/{repo}/compare/{pin}...{new}) and intersects
#      the changed files with the manifest,
#   4. diffs Bridge's SPKI pins (internal/dialer/dialer_pinning.go) between
#      the two commits, and against the local port of that file when the
#      manifest lists one.
#
# It never changes the repository and never opens pull requests. It writes
# an issue body to $ISSUE_BODY_FILE (default: a temp file) and, when
# $GITHUB_OUTPUT is set, the outputs updated (true when any pinned
# repository has moved, or the local SPKI pins differ from Bridge's), touched (number of changed ported files),
# spki_changed, bridge_tag, title and body_file. The weekly
# proton-upstream workflow turns those into one issue.
#
# Environment:
#   PROTON_UPSTREAM_DOC       pin document (default docs/upstream/proton.md)
#   PROTON_UPSTREAM_MANIFEST  manifest (default docs/upstream/proton-ports.toml)
#   PROTON_UPSTREAM_LOCAL_ROOT  root for the manifest's local paths
#                             (default: the repository root)
#   PROTON_UPSTREAM_FIXTURES  answer GitHub API calls from this directory
#                             instead of the network (see
#                             scripts/test-check-proton-upstream.sh)
#
# Requires: gh (authenticated; read access to public repos is enough), jq,
# python3 >= 3.11, git (only when a comparison lists 300 or more files).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OWNER="ProtonMail"
BRIDGE="proton-bridge"
PINNING_FILE="internal/dialer/dialer_pinning.go"
DOC="${PROTON_UPSTREAM_DOC:-${REPO_ROOT}/docs/upstream/proton.md}"
MANIFEST="${PROTON_UPSTREAM_MANIFEST:-${REPO_ROOT}/docs/upstream/proton-ports.toml}"
LOCAL_ROOT="${PROTON_UPSTREAM_LOCAL_ROOT:-${REPO_ROOT}}"
FIXTURES="${PROTON_UPSTREAM_FIXTURES:-}"
BODY_FILE="${ISSUE_BODY_FILE:-$(mktemp)}"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

output() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s=%s\n' "$1" "$2" >> "$GITHUB_OUTPUT"
  fi
}

die() {
  echo "error: $*" >&2
  exit 1
}

# api ENDPOINT - prints a GitHub REST API response.
api() {
  if [[ -n "${FIXTURES}" ]]; then
    local file="${FIXTURES}/api/${1//\//__}.json"
    [[ -f "${file}" ]] || die "no fixture for ${1} (${file})"
    cat "${file}"
  else
    gh api "$1"
  fi
}

# raw REPO PATH REF - prints a file from ProtonMail/REPO at REF.
raw() {
  if [[ -n "${FIXTURES}" ]]; then
    local file="${FIXTURES}/raw/$1/$3/$2"
    [[ -f "${file}" ]] || die "no fixture for $1:$2 at $3 (${file})"
    cat "${file}"
  else
    gh api -H "Accept: application/vnd.github.raw" "repos/${OWNER}/$1/contents/$2?ref=$3"
  fi
}

# commit_of REPO REF - prints the full commit SHA REF names in ProtonMail/REPO.
commit_of() {
  local sha
  sha="$(api "repos/${OWNER}/$1/commits/$2" | jq -r '.sha // empty')"
  [[ "${sha}" =~ ^[0-9a-f]{40}$ ]] || die "could not resolve ${OWNER}/$1@$2 to a commit"
  printf '%s' "${sha}"
}

# changed_files REPO FROM TO - writes the files changed between two commits
# as a JSON array of {filename, previous_filename, status} to stdout, and
# the comparison's status (ahead, behind, diverged) to $WORK/REPO.status.
changed_files() {
  local repo="$1" from="$2" to="$3" compare count status
  compare="$(api "repos/${OWNER}/${repo}/compare/${from}...${to}")"
  status="$(jq -r '.status' <<<"${compare}")"
  printf '%s' "${status}" > "${WORK}/${repo}.status"
  if [[ "${status}" == "behind" ]]; then
    # The pin is ahead of the new commit; nothing upstream to review.
    echo "[]"
    return
  fi
  count="$(jq '.files | length' <<<"${compare}")"
  if ((count < 300)); then
    jq '[.files[] | {filename, previous_filename, status}]' <<<"${compare}"
    return
  fi
  # The compare API lists at most 300 files; ask git for the full list.
  [[ -z "${FIXTURES}" ]] || die "fixture comparison for ${repo} lists ${count} files"
  echo "${repo}: the comparison lists ${count} files (the API's limit); diffing with git" >&2
  local clone="${WORK}/${repo}.git"
  git clone --quiet --bare --filter=blob:none "https://github.com/${OWNER}/${repo}.git" "${clone}"
  git -C "${clone}" diff --name-status -M "${from}" "${to}" | python3 -c '
import json, sys
files = []
for line in sys.stdin:
    parts = line.rstrip("\n").split("\t")
    kind = parts[0][0]
    status = {"A": "added", "D": "removed", "M": "modified", "R": "renamed", "C": "copied"}.get(kind, "changed")
    entry = {"filename": parts[-1], "previous_filename": parts[1] if kind in "RC" else None, "status": status}
    files.append(entry)
json.dump(files, sys.stdout)
'
}

[[ -f "${DOC}" ]] || {
  echo "::warning::${DOC} does not exist yet; nothing to check."
  output updated false
  exit 0
}
[[ -f "${MANIFEST}" ]] || die "${MANIFEST} is missing"

# 1. Pins: one "repo commit release" line per row of the proton-pin table.
pins="$(python3 - "${DOC}" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
blocks = re.findall(r"<!-- proton-pin:start -->(.*?)<!-- proton-pin:end -->", text, flags=re.S)
if len(blocks) != 1:
    sys.exit(f"error: {sys.argv[1]} must have exactly one proton-pin marker block")
seen = set()
for line in blocks[0].splitlines():
    if not line.strip().startswith("|"):
        continue
    repos = set(re.findall(r"ProtonMail/([A-Za-z0-9._-]+)", line))
    commits = re.findall(r"`([0-9a-f]{40})`", line)
    if not repos and not commits:
        continue  # header or separator row
    if len(repos) != 1 or len(commits) != 1:
        sys.exit(f"error: pin row needs one ProtonMail/<repo> and one full commit SHA: {line.strip()}")
    repo = repos.pop()
    if repo in seen:
        sys.exit(f"error: {repo} is pinned twice")
    seen.add(repo)
    cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
    release = cells[1].strip("`") if len(cells) > 1 else ""
    print(f"{repo}\t{commits[0]}\t{release}")
if "proton-bridge" not in seen:
    sys.exit("error: the proton-pin table has no ProtonMail/proton-bridge row")
PY
)"

# 2. Bridge's new release and the commits its go.mod pins.
bridge_tag="${1:-}"
if [[ -z "${bridge_tag}" ]]; then
  bridge_tag="$(api "repos/${OWNER}/${BRIDGE}/releases/latest" \
    | jq -r 'select(.prerelease == false and .draft == false) | .tag_name')"
fi
[[ "${bridge_tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "'${bridge_tag}' is not a Bridge release tag (vX.Y.Z)"
raw "${BRIDGE}" go.mod "${bridge_tag}" > "${WORK}/go.mod"

: > "${WORK}/repos.tsv"
while IFS=$'\t' read -r repo pin release; do
  if [[ "${repo}" == "${BRIDGE}" ]]; then
    new="$(commit_of "${BRIDGE}" "${bridge_tag}")"
    new_label="${bridge_tag}"
  else
    # The version Bridge's go.mod requires, after any replace directive.
    version="$(python3 - "${WORK}/go.mod" "${repo}" <<'PY'
import re, sys
gomod, repo = sys.argv[1], sys.argv[2]
module = re.compile(rf"github\.com/ProtonMail/{re.escape(repo)}(/v\d+)?")
text = open(gomod, encoding="utf-8").read()
version = None
for match in re.finditer(r"^\s*(?:require\s+)?(\S+)\s+(v\S+)", text, flags=re.M):
    if module.fullmatch(match.group(1)):
        version = match.group(2)
for match in re.finditer(r"^\s*(?:replace\s+)?\S+(?:\s+v\S+)?\s+=>\s+(\S+)\s+(v\S+)", text, flags=re.M):
    if module.fullmatch(match.group(1)):
        version = match.group(2)
if version is None:
    sys.exit(0)
# A pseudo-version (vX.Y.Z-[pre.]yyyymmddhhmmss-abcdefabcdef) names a commit;
# anything else is a tag.
pseudo = re.search(r"\d{14}-([0-9a-f]{12})$", version)
print(pseudo.group(1) if pseudo else version.removesuffix("+incompatible"))
PY
)"
    if [[ -z "${version}" ]]; then
      echo "::warning::${OWNER}/${repo} is pinned but Bridge ${bridge_tag}'s go.mod does not require it; skipped."
      continue
    fi
    new="$(commit_of "${repo}" "${version}")"
    new_label="Bridge ${bridge_tag} go.mod"
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "${repo}" "${pin}" "${release}" "${new}" "${new_label}" >> "${WORK}/repos.tsv"
  # 3. What changed upstream since the pin.
  if [[ "${new}" == "${pin}" ]]; then
    printf '[]' > "${WORK}/${repo}.files.json"
    printf 'identical' > "${WORK}/${repo}.status"
  else
    changed_files "${repo}" "${pin}" "${new}" > "${WORK}/${repo}.files.json"
  fi
done <<<"${pins}"

# 4. SPKI pins at the pinned and the new Bridge commit.
bridge_pin="$(awk -F'\t' -v r="${BRIDGE}" '$1 == r { print $2 }' "${WORK}/repos.tsv")"
bridge_new="$(awk -F'\t' -v r="${BRIDGE}" '$1 == r { print $4 }' "${WORK}/repos.tsv")"
raw "${BRIDGE}" "${PINNING_FILE}" "${bridge_pin}" > "${WORK}/spki-old.go"
if [[ "${bridge_new}" == "${bridge_pin}" ]]; then
  cp "${WORK}/spki-old.go" "${WORK}/spki-new.go"
else
  raw "${BRIDGE}" "${PINNING_FILE}" "${bridge_new}" > "${WORK}/spki-new.go"
fi

# 5. Intersect with the manifest and write the issue body.
python3 - "${WORK}" "${MANIFEST}" "${LOCAL_ROOT}" "${BODY_FILE}" "${bridge_tag}" "${PINNING_FILE}" <<'PY' > "${WORK}/summary"
import hashlib, json, os, re, sys, tomllib

work, manifest_path, local_root, body_file, bridge_tag, pinning_file = sys.argv[1:]
gh = "https://github.com/ProtonMail"

with open(manifest_path, "rb") as f:
    ports = tomllib.load(f).get("port", [])
for port in ports:
    for key in ("repo", "upstream", "local"):
        if not isinstance(port.get(key), str) or not port[key]:
            sys.exit(f"error: every [[port]] in {manifest_path} needs a string {key!r}: {port}")

def matches(upstream, path):
    if path is None:
        return False
    return path.startswith(upstream) if upstream.endswith("/") else path == upstream

repos = []
with open(os.path.join(work, "repos.tsv"), encoding="utf-8") as f:
    for line in f:
        repo, pin, release, new, new_label = line.rstrip("\n").split("\t")
        with open(os.path.join(work, f"{repo}.files.json"), encoding="utf-8") as g:
            files = json.load(g)
        with open(os.path.join(work, f"{repo}.status"), encoding="utf-8") as g:
            status = g.read().strip()
        repos.append((repo, pin, release, new, new_label, status, files))

def spki(path):
    with open(path, encoding="utf-8") as f:
        return re.findall(r'pin-sha256="([A-Za-z0-9+/]{43}=)"', f.read())

old_pins, new_pins = spki(os.path.join(work, "spki-old.go")), spki(os.path.join(work, "spki-new.go"))
added = [p for p in new_pins if p not in old_pins]
removed = [p for p in old_pins if p not in new_pins]

# The local port of the pinning file, if the manifest names one.
local_pins_note = []
local_mismatch = False
for port in ports:
    if port["repo"] == "proton-bridge" and port["upstream"] == pinning_file:
        local = os.path.join(local_root, port["local"])
        if not os.path.isfile(local):
            local_pins_note.append(f"- `{port['local']}` (the manifest's port of `{pinning_file}`) does not exist.")
            continue
        text = open(local, encoding="utf-8").read()
        ours = set(re.findall(r"[A-Za-z0-9+/]{43}=", text))
        missing = [p for p in new_pins if p not in ours]
        extra = sorted(ours - set(new_pins))
        local_mismatch = local_mismatch or bool(missing or extra)
        if missing:
            local_pins_note.append(f"- `{port['local']}` lacks {len(missing)} of Bridge {bridge_tag}'s pins: "
                                   + ", ".join(f"`{p}`" for p in missing))
        if extra:
            local_pins_note.append(f"- `{port['local']}` has {len(extra)} pin(s) Bridge {bridge_tag} no longer lists: "
                                   + ", ".join(f"`{p}`" for p in extra))
        if not missing and not extra:
            local_pins_note.append(f"- `{port['local']}` matches Bridge {bridge_tag}'s pins.")

moved = [r for r in repos if r[5] not in ("identical", "behind")]
touched_total = 0
sections = []
for repo, pin, release, new, new_label, status, files in repos:
    if status == "identical":
        sections.append(f"### {repo}\n\nUp to date at [`{pin[:12]}`]({gh}/{repo}/commit/{pin}) ({new_label}).")
        continue
    if status == "behind":
        sections.append(f"### {repo}\n\nPinned ahead of {new_label}: [`{pin[:12]}`]({gh}/{repo}/commit/{pin}) "
                        f"already contains [`{new[:12]}`]({gh}/{repo}/commit/{new}).")
        continue
    compare = f"{gh}/{repo}/compare/{pin}...{new}"
    rows = []
    for file in files:
        for port in ports:
            if port["repo"] != repo:
                continue
            path = file["filename"]
            if not (matches(port["upstream"], path) or matches(port["upstream"], file.get("previous_filename"))):
                continue
            anchor = hashlib.sha256(path.encode()).hexdigest()
            renamed = f" (from `{file['previous_filename']}`)" if file.get("previous_filename") else ""
            rows.append(f"| [`{path}`]({compare}#diff-{anchor}){renamed} | {file['status']} | `{port['local']}` |")
    touched_total += len(rows)
    lines = [
        f"### {repo}",
        "",
        f"[`{pin[:12]}`]({gh}/{repo}/commit/{pin}) ({release or 'pinned'}) → "
        f"[`{new[:12]}`]({gh}/{repo}/commit/{new}) ({new_label}): "
        f"{len(files)} files changed, [full diff]({compare}).",
        "",
    ]
    if rows:
        lines += ["| Upstream file | Change | zmail port |", "| --- | --- | --- |", *rows]
    else:
        lines.append("No ported file changed.")
    sections.append("\n".join(lines))

spki_lines = []
if added or removed:
    spki_lines.append(f"`{pinning_file}` changed its pins between the pinned Bridge commit and {bridge_tag}:")
    spki_lines += [f"- added `{p}`" for p in added]
    spki_lines += [f"- removed `{p}`" for p in removed]
else:
    spki_lines.append(f"No change to the pins in `{pinning_file}` ({len(new_pins)} pins).")
spki_lines += local_pins_note

if moved:
    intro = "Proton upstream has moved past the pins in `docs/upstream/proton.md`."
else:
    intro = "The pins in `docs/upstream/proton.md` are current, but zmail's SPKI pins differ from Bridge's."
body = "\n".join([
    "<!-- proton-upstream-check -->",
    f"{intro} Latest Bridge release: [`{bridge_tag}`]({gh}/proton-bridge/releases/tag/{bridge_tag}).",
    "",
    "Opened by the weekly `proton-upstream` workflow (`scripts/check-proton-upstream.sh`), which compares each pin "
    "with the new commit and lists the changed files that `docs/upstream/proton-ports.toml` says zmail ports. "
    "It never opens pull requests.",
    "",
    "## Ported files",
    "",
    "\n\n".join(sections),
    "",
    "## SPKI pins",
    "",
    "\n".join(spki_lines),
    "",
    "## To do",
    "",
    "- [ ] Review each listed diff and carry the relevant changes into the zmail port (keep the upstream copyright notices and add a dated modification note).",
    "- [ ] Update the SPKI pins if they changed.",
    "- [ ] Move the pins in `docs/upstream/proton.md` (and `docker/proton-harness` if go-proton-api moved), then close this issue.",
    "",
])
with open(body_file, "w", encoding="utf-8") as f:
    f.write(body)

print(f"updated={'true' if moved or local_mismatch else 'false'}")
print(f"touched={touched_total}")
print(f"spki_changed={'true' if added or removed else 'false'}")
PY

updated="$(sed -n 's/^updated=//p' "${WORK}/summary")"
touched="$(sed -n 's/^touched=//p' "${WORK}/summary")"
spki_changed="$(sed -n 's/^spki_changed=//p' "${WORK}/summary")"
title="Proton upstream: Bridge ${bridge_tag}, ${touched} ported file(s) changed"
[[ "${spki_changed}" == "true" ]] && title="${title}, SPKI pins changed"

if [[ "${updated}" == "true" ]]; then
  echo "${title}"
else
  echo "Every pinned Proton repository is at its latest commit (Bridge ${bridge_tag}) and the SPKI pins agree. Nothing to report."
fi
echo "Issue body written to ${BODY_FILE}"
output updated "${updated}"
output touched "${touched}"
output spki_changed "${spki_changed}"
output bridge_tag "${bridge_tag}"
output title "${title}"
output body_file "${BODY_FILE}"
