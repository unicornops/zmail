#!/usr/bin/env bash
set -euo pipefail

# test-check-proton-upstream.sh - Runs scripts/check-proton-upstream.sh
# against the offline fixtures in scripts/fixtures/proton-upstream and
# checks what it reports. No network and no GitHub token needed.
#
# The fixtures pin five repositories:
#   proton-bridge     1111... → v3.28.0 (2222...): events, a renamed IMAP
#                     file and the SPKI pin file changed, plus unported files
#   go-proton-api     3333... → 4444... (a go.mod pseudo-version): auth.go
#                     changed (ported), message.go and server/auth.go not
#   go-message        5555..., ahead of Bridge's replace target 6666...
#   gopenpgp          7777..., equal to the v2.10.0-proton tag
#   not-a-dependency  absent from Bridge's go.mod

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="${SCRIPT_DIR}/check-proton-upstream.sh"
FIXTURES="${SCRIPT_DIR}/fixtures/proton-upstream"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

# run NAME DOC LOCAL_ROOT [ARGS...] - runs the check; leaves its stdout and
# stderr in $WORK/NAME.log, the body in $WORK/NAME.md, the outputs in
# $WORK/NAME.out and the exit status in $status.
run() {
  local name="$1" doc="$2" local_root="$3"
  shift 3
  : > "${WORK}/${name}.out"
  status=0
  PROTON_UPSTREAM_FIXTURES="${FIXTURES}" \
    PROTON_UPSTREAM_DOC="${FIXTURES}/${doc}" \
    PROTON_UPSTREAM_MANIFEST="${FIXTURES}/proton-ports.toml" \
    PROTON_UPSTREAM_LOCAL_ROOT="${FIXTURES}/${local_root}" \
    ISSUE_BODY_FILE="${WORK}/${name}.md" \
    GITHUB_OUTPUT="${WORK}/${name}.out" \
    "${CHECK}" "$@" > "${WORK}/${name}.log" 2>&1 || status=$?
}

expect_status() {
  [[ "${status}" == "$2" ]] || fail "$1: exit status ${status}, expected $2; output:
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

anchor() {
  printf '%s' "$1" | sha256sum | cut -d' ' -f1
}

bridge_compare="https://github.com/ProtonMail/proton-bridge/compare/$(printf '1%.0s' {1..40})...$(printf '2%.0s' {1..40})"
api_compare="https://github.com/ProtonMail/go-proton-api/compare/$(printf '3%.0s' {1..40})...$(printf '4%.0s' {1..40})"

# Pins behind the latest release, with stale local SPKI pins.
run moved proton-moved.md local-stale
expect_status moved 0
expect_output moved updated true
expect_output moved touched 5
expect_output moved spki_changed true
expect_output moved bridge_tag v3.28.0
expect_output moved title "Proton upstream: Bridge v3.28.0, 5 ported file(s) changed, SPKI pins changed"
expect_in moved md "<!-- proton-upstream-check -->"
expect_in moved md "[\`internal/events/user.go\`](${bridge_compare}#diff-$(anchor internal/events/user.go)) | modified | \`crates/zmail-proton/src/events.rs\` |"
expect_in moved md "[\`internal/events/user_test.go\`](${bridge_compare}#diff-$(anchor internal/events/user_test.go)) | added |"
expect_in moved md "[\`internal/services/imapservice/connector.go\`](${bridge_compare}#diff-$(anchor internal/services/imapservice/connector.go)) (from \`internal/services/imapservice/imap_connector.go\`) | renamed | \`crates/zmail-proton/src/mailbox.rs\` |"
expect_in moved md "[\`internal/dialer/dialer_pinning.go\`](${bridge_compare}#diff-$(anchor internal/dialer/dialer_pinning.go))"
expect_in moved md "[\`auth.go\`](${api_compare}#diff-$(anchor auth.go)) | modified | \`crates/zmail-proton-api/src/auth.rs\` |"
expect_in moved md "6 files changed, [full diff](${bridge_compare})"
expect_not_in moved md "README.md"
expect_not_in moved md "internal/services/smtp/smtp.go"
expect_not_in moved md "message.go"
expect_not_in moved md "server/auth.go"
expect_in moved md "### go-message"
expect_in moved md "Pinned ahead of Bridge v3.28.0 go.mod"
expect_in moved md "### gopenpgp"
expect_in moved md "Up to date at [\`777777777777\`]"
expect_in moved md "- added \`Dddddddddddddddddddddddddddddddddddddddddd0=\`"
expect_in moved md "- removed \`AfMENBVvOS8MnISprtvyPsjKlPooqh8nMB/pvCrpJpw=\`"
expect_in moved md "\`crates/zmail-proton-api/src/pinning.rs\` lacks 1 of Bridge v3.28.0's pins: \`Dddddddddddddddddddddddddddddddddddddddddd0=\`"
expect_in moved md "has 1 pin(s) Bridge v3.28.0 no longer lists: \`AfMENBVvOS8MnISprtvyPsjKlPooqh8nMB/pvCrpJpw=\`"
expect_in moved log "ProtonMail/not-a-dependency is pinned but Bridge v3.28.0's go.mod does not require it; skipped."

# An explicit tag gives the same result as releases/latest.
run tagged proton-moved.md local-stale v3.28.0
expect_status tagged 0
expect_output tagged touched 5

# Every pin current and the local SPKI pins matching: nothing to report.
run current proton-current.md local-current
expect_status current 0
expect_output current updated false
expect_output current touched 0
expect_output current spki_changed false
expect_in current log "Nothing to report."

# Pins current but the local SPKI pins stale: still worth an issue.
run stale-pins proton-current.md local-stale
expect_status stale-pins 0
expect_output stale-pins updated true
expect_output stale-pins touched 0
expect_in stale-pins md "are current, but zmail's SPKI pins differ from Bridge's"

# Malformed input fails loudly.
run bad-row proton-bad-row.md local-current
expect_status bad-row 1
expect_in bad-row log "pin row needs one ProtonMail/<repo> and one full commit SHA"

run no-bridge proton-no-bridge.md local-current
expect_status no-bridge 1
expect_in no-bridge log "the proton-pin table has no ProtonMail/proton-bridge row"

run bad-tag proton-moved.md local-current 3.28
expect_status bad-tag 1
expect_in bad-tag log "'3.28' is not a Bridge release tag"

# No pin document yet: a warning, not a failure.
run no-doc does-not-exist.md local-current
expect_status no-doc 0
expect_output no-doc updated false
expect_in no-doc log "does not exist yet"

if ((failures > 0)); then
  echo "${failures} check(s) failed" >&2
  exit 1
fi
echo "check-proton-upstream.sh: all fixture checks passed"
