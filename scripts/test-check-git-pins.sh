#!/usr/bin/env bash
set -euo pipefail

# test-check-git-pins.sh - Runs scripts/check-git-pins.py against the
# offline fixtures in scripts/fixtures/git-pins and checks what it reports,
# then checks the real Cargo.toml against docs/upstream/git-pins.toml
# (--validate, also offline). No network and no GitHub token needed: issue
# updates go to a stub `gh` that records its arguments.
#
# The fixtures pin gpui/gpui_platform (aaaa...), pgp (bbbb...) and
# proton-srp (cccc...), like the root Cargo.toml, and answer the upstream
# calls in two states:
#   current/  every tracked pull request open with its head on the pin, no
#             newer crates.io release, one RustSec advisory that does not
#             affect the pinned pgp version: nothing to act on
#   moved/    Zed #62379 closed unmerged with a new head (dddd...), rpgp #573
#             merged (eeee...) and in pgp 0.22.0, proton-srp 0.9.0 off
#             crypto-bigint 0.5, an advisory affecting pgp 0.21.0 and a
#             withdrawn one for proton-srp

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="${SCRIPT_DIR}/check-git-pins.py"
FIXTURES="${SCRIPT_DIR}/fixtures/git-pins"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

A="$(printf 'a%.0s' {1..40})"
B="$(printf 'b%.0s' {1..40})"
D="$(printf 'd%.0s' {1..40})"
E="$(printf 'e%.0s' {1..40})"

# A stub gh for --update-issue: records each call in $WORK/gh.log and
# answers `gh issue list` with $WORK/issues.json.
mkdir -p "${WORK}/bin"
cat > "${WORK}/bin/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_STUB_LOG}"
if [[ "$1 $2" == "issue list" ]]; then
  cat "${GH_STUB_ISSUES}"
fi
STUB
chmod +x "${WORK}/bin/gh"

# run NAME STATE CARGO_TOML MANIFEST [ARGS...] - runs the check; leaves its
# stdout in $WORK/NAME.md, stderr in $WORK/NAME.log, the outputs in
# $WORK/NAME.out and the exit status in $status.
run() {
  local name="$1" state="$2" cargo_toml="$3" manifest="$4"
  shift 4
  # Cargo-X.toml goes with Cargo-X.lock when there is one.
  local lock="${FIXTURES}/${cargo_toml%.toml}.lock"
  [[ -f "${lock}" ]] || lock="${FIXTURES}/Cargo.lock"
  : > "${WORK}/${name}.out"
  : > "${WORK}/gh.log"
  status=0
  PATH="${WORK}/bin:${PATH}" \
    GH_STUB_LOG="${WORK}/gh.log" \
    GH_STUB_ISSUES="${WORK}/issues.json" \
    GIT_PINS_FIXTURES="${FIXTURES}/${state}" \
    GIT_PINS_CARGO_TOML="${FIXTURES}/${cargo_toml}" \
    GIT_PINS_CARGO_LOCK="${lock}" \
    GIT_PINS_MANIFEST="${FIXTURES}/${manifest}" \
    ISSUE_BODY_FILE="${WORK}/${name}.body" \
    GITHUB_OUTPUT="${WORK}/${name}.out" \
    "${CHECK}" "$@" > "${WORK}/${name}.md" 2> "${WORK}/${name}.log" || status=$?
}

expect_status() {
  [[ "${status}" == "$2" ]] || fail "$1: exit status ${status}, expected $2; stderr:
$(cat "${WORK}/$1.log")"
}

expect_output() {
  grep -qxF "$2=$3" "${WORK}/$1.out" || fail "$1: expected output $2=$3, got:
$(cat "${WORK}/$1.out")"
}

expect_in() {
  grep -qF -- "$3" "${WORK}/$1.$2" || fail "$1: $1.$2 should contain: $3"
}

expect_not_in() {
  if grep -qF -- "$3" "${WORK}/$1.$2"; then
    fail "$1: $1.$2 should not contain: $3"
  fi
}

expect_gh() {
  grep -qF -- "$2" "${WORK}/gh.log" || fail "$1: gh should have been called with: $2; calls:
$(cat "${WORK}/gh.log")"
}

expect_no_gh() {
  if grep -qF -- "$2" "${WORK}/gh.log"; then
    fail "$1: gh should not have been called with: $2"
  fi
}

# Everything upstream where the pins are: all four crates listed, nothing
# to act on, and the dry run touches no issue.
run current current Cargo.toml git-pins.toml --dry-run
expect_status current 0
expect_output current findings 0
expect_output current title "Git pins: nothing to act on"
expect_in current md "Git pins: nothing to act on"
expect_in current md "<!-- git-pins-check -->"
expect_in current md "### gpui, gpui_platform"
expect_in current md "### pgp"
expect_in current md "### proton-srp"
expect_in current md "Nothing to act on."
expect_in current md "| Pinned | [\`aaaaaaaaaaaa\`](https://github.com/huacnlee/zed/commit/${A}) on [\`huacnlee/zed\`](https://github.com/huacnlee/zed) (gpui 0.2.2, gpui_platform 0.1.0) |"
expect_in current md "[zed-industries/zed#62379](https://github.com/zed-industries/zed/pull/62379) \"gpui: Add native window composition support\": open"
expect_in current md "| Pinned | [\`bbbbbbbbbbbb\`](https://github.com/lazzurs/rpgp/commit/${B}) on [\`lazzurs/rpgp\`](https://github.com/lazzurs/rpgp) (pgp 0.21.0) |"
expect_in current md "the pinned commit"
expect_in current md "[\`v1.30.2\`](https://github.com/zed-industries/zed/releases/tag/v1.30.2) (2026-11-25)"
expect_in current md "[pgp 0.21.0](https://crates.io/crates/pgp/0.21.0)"
expect_in current md "| \`crypto-bigint\` in that release | \`^0.5.5\` |"
expect_in current md "[RUSTSEC-2024-0447](https://rustsec.org/advisories/RUSTSEC-2024-0447) (Panics on Malformed Untrusted Input): does not affect 0.21.0"
expect_in current md "None for \`gpui\`, \`gpui_platform\`."
expect_in current md "no pull request"
expect_in current md "A crates.io pgp release contains rpgp/rpgp#573"
expect_not_in current md "- **"
expect_not_in current md "RUSTSEC-2099"
[[ ! -s "${WORK}/gh.log" ]] || fail "current: the dry run called gh: $(cat "${WORK}/gh.log")"
cmp -s <(sed '1,/^$/d' "${WORK}/current.md") "${WORK}/current.body" \
  || fail "current: the printed body differs from the body file"

# Upstream moved: one finding per change.
run moved moved Cargo.toml git-pins.toml
expect_status moved 0
expect_output moved findings 5
expect_output moved title "Git pins: 5 findings"
expect_in moved md "- **gpui, gpui_platform**: [zed-industries/zed#62379](https://github.com/zed-industries/zed/pull/62379) was closed without being merged (2026-11-20)."
expect_in moved md "- **gpui, gpui_platform**: the head of [zed-industries/zed#62379](https://github.com/zed-industries/zed/pull/62379) moved to [\`dddddddddddd\`](https://github.com/huacnlee/zed/commit/${D}); the pin is [\`aaaaaaaaaaaa\`](https://github.com/huacnlee/zed/commit/${A}) ([changes](https://github.com/huacnlee/zed/compare/${A}...${D}))."
expect_in moved md "- **pgp**: [rpgp/rpgp#573](https://github.com/rpgp/rpgp/pull/573) was merged (2026-11-24) as [\`eeeeeeeeeeee\`](https://github.com/rpgp/rpgp/commit/${E}), and pgp 0.22.0 contains it: the fork can be dropped"
expect_in moved md "- **pgp**: [RUSTSEC-2099-0001](https://rustsec.org/advisories/RUSTSEC-2099-0001) (Signature forgery in a fixture) affects pgp 0.21.0, the version the fork declares; patched: \`>= 0.21.2\`."
expect_in moved md "- **proton-srp**: proton-srp 0.9.0 is out on crates.io (the fork declares 0.8.4); its \`crypto-bigint\` requirement is \`^0.7.6\`."
expect_in moved md "| \`crypto-bigint\` in that release | \`^0.7.6\` |"
expect_in moved md "closed without being merged"
expect_in moved md "moved since the pin"
expect_in moved md "[pgp 0.22.0](https://crates.io/crates/pgp/0.22.0)"
expect_in moved md "RUSTSEC-2099-0002 (Withdrawn fixture advisory): withdrawn"
expect_in moved md "does not affect 0.21.0"
# The yanked 0.22.1 and the pre-release 0.23.0-alpha.1 are not releases to move to.
expect_not_in moved md "0.22.1"
expect_not_in moved md "0.23.0"
# The release that contains the merged pull request is one finding, not two.
expect_not_in moved md "pgp 0.22.0 is out on crates.io"
expect_not_in moved md "calloop"

# --update-issue: creates the issue when none carries the marker...
printf '[{"number": 3, "title": "Unrelated", "body": "no marker here"}]' > "${WORK}/issues.json"
run create moved Cargo.toml git-pins.toml --update-issue
expect_status create 0
expect_gh create "issue list --state open --limit 200 --json number,title,body"
expect_gh create "issue create --title Git pins: 5 findings --body-file ${WORK}/create.body"
expect_no_gh create "issue edit"

# ...edits the marked issue when its title or body changed...
printf '[{"number": 7, "title": "Git pins: nothing to act on", "body": "<!-- git-pins-check -->\\nold"}]' > "${WORK}/issues.json"
run edit moved Cargo.toml git-pins.toml --update-issue
expect_status edit 0
expect_gh edit "issue edit 7 --title Git pins: 5 findings --body-file ${WORK}/edit.body"
expect_no_gh edit "issue create"

# ...and leaves an unchanged issue alone, so a weekly re-run is silent.
jq -n --rawfile body "${WORK}/edit.body" \
  '[{number: 7, title: "Git pins: 5 findings", body: ($body | rtrimstr("\n") | gsub("\n"; "\r\n"))}]' \
  > "${WORK}/issues.json"
run unchanged moved Cargo.toml git-pins.toml --update-issue
expect_status unchanged 0
expect_no_gh unchanged "issue edit"
expect_no_gh unchanged "issue create"
expect_in unchanged log "already up to date"

# Cargo.toml and the manifest must agree.
run unlisted current Cargo-unlisted.toml git-pins.toml
expect_status unlisted 1
expect_in unlisted log "extra is a git dependency in scripts/fixtures/git-pins/Cargo-unlisted.toml but no [[pin]] in scripts/fixtures/git-pins/git-pins.toml lists it"

run split current Cargo-split.toml git-pins.toml
expect_status split 1
expect_in split log "gpui and gpui_platform share one [[pin]] but not one git source and commit"

run stale current Cargo.toml git-pins-stale.toml
expect_status stale 1
expect_in stale log "lists gone, which is not a git dependency in"

# The RustSec version ranges, read as Cargo reads requirements.
if ! python3 -I -B - "${CHECK}" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("check_git_pins", sys.argv[1])
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)
cases = [
    (">= 0.21.2", "0.21.1", False), (">= 0.21.2", "0.21.2", True), (">= 0.21.2", "1.0.0", True),
    ("< 0.10.0", "0.9.9", True), ("< 0.10.0", "0.10.0", False),
    (">= 0.9.3, < 0.10.0", "0.9.5", True), (">= 0.9.3, < 0.10.0", "0.10.1", False),
    ("^0.8.5", "0.8.9", True), ("^0.8.5", "0.9.0", False), ("^1.2", "1.9.0", True), ("^1.2", "2.0.0", False),
    ("^0.0.3", "0.0.3", True), ("^0.0.3", "0.0.4", False), ("0.4.2", "0.4.7", True),
    ("~1.2.3", "1.2.9", True), ("~1.2.3", "1.3.0", False), ("=1.2", "1.2.5", True), ("=1.2.3", "1.2.4", False),
    ("> 1.2", "1.2.9", False), ("> 1.2", "1.3.0", True), ("<= 1.2", "1.2.9", True), ("<= 1.2", "1.3.0", False),
    (">= 1.0.0", "1.0.0-rc.1", False), ("*", "3.1.4", True),
]
bad = [c for c in cases if check.requirement_matches(c[0], c[1]) != c[2]]
for requirement, version, expected in bad:
    print(f"'{requirement}' on {version}: expected {expected}", file=sys.stderr)
assert check.version_key("1.0.0-alpha.2") < check.version_key("1.0.0-alpha.10") < check.version_key("1.0.0")
sys.exit(1 if bad else 0)
PY
then
  fail "requirement_matches reads a RustSec range wrongly"
fi

# The real Cargo.toml, Cargo.lock and docs/upstream/git-pins.toml agree.
status=0
"${CHECK}" --validate > "${WORK}/validate.log" 2>&1 || status=$?
[[ "${status}" == 0 ]] || fail "validate: the repository's pins and docs/upstream/git-pins.toml disagree:
$(cat "${WORK}/validate.log")"

if ((failures > 0)); then
  echo "${failures} check(s) failed" >&2
  exit 1
fi
echo "check-git-pins.py: all fixture checks passed"
