#!/usr/bin/env bash
set -euo pipefail

# test-check-linux-glibc.sh - Runs scripts/check-linux-glibc.sh against
# made-up binaries and .debs and checks what it reports. An `objdump` stub
# on PATH prints each fake binary's dynamic symbol table (lines in the real
# `objdump -T` format), so no compiler or particular glibc is needed; the
# .debs are real packages built with dpkg-deb.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="${SCRIPT_DIR}/check-linux-glibc.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

mkdir -p "${WORK}/bin"
cat > "${WORK}/bin/objdump" <<'STUB'
#!/usr/bin/env bash
# objdump -T FILE: a fake binary's first line is FAKE-ELF, the rest its table.
[[ "$1" == "-T" && -f "$2" ]] || exit 2
if [[ "$(head -n 1 "$2")" != "FAKE-ELF" ]]; then
  echo "objdump: $2: file format not recognized" >&2
  exit 1
fi
printf '\n%s:     file format elf64-x86-64\n\nDYNAMIC SYMBOL TABLE:\n' "$2"
tail -n +2 "$2"
STUB
chmod +x "${WORK}/bin/objdump"

# fake NAME VERSION... - a fake binary needing one symbol per GLIBC VERSION.
fake() {
  local name="$1" version i=0
  shift
  {
    echo "FAKE-ELF"
    printf '0000000000000000  w   D  *UND*\t0000000000000000              __gmon_start__\n'
    for version in "$@"; do
      i=$((i + 1))
      printf '0000000000000000      DF *UND*\t0000000000000000 (GLIBC_%s) symbol_%d\n' "${version}" "${i}"
    done
  } > "${WORK}/${name}"
}

# deb NAME BINARY DEPENDS - a .deb with BINARY as /usr/bin/zmail-app (none
# when BINARY is empty).
deb() {
  local name="$1" binary="$2" depends="$3" root="${WORK}/${1}.root"
  mkdir -p "${root}/DEBIAN" "${root}/usr/bin"
  printf 'Package: zmail-app\nVersion: 1.0.0\nArchitecture: amd64\nMaintainer: test <test@example.invalid>\nDepends: %s\nDescription: test\n' \
    "${depends}" > "${root}/DEBIAN/control"
  if [[ -n "${binary}" ]]; then
    cp "${WORK}/${binary}" "${root}/usr/bin/zmail-app"
  fi
  dpkg-deb --root-owner-group --build "${root}" "${WORK}/${name}.deb" > /dev/null
}

# run NAME FILE... - runs the check; leaves its output in $WORK/NAME.log and
# the exit status in $status.
run() {
  local name="$1"
  shift
  status=0
  PATH="${WORK}/bin:${PATH}" "${CHECK}" "$@" > "${WORK}/${name}.log" 2>&1 || status=$?
}

expect_status() {
  [[ "${status}" == "$2" ]] || fail "$1: exit status ${status}, expected $2; output:
$(cat "${WORK}/$1.log")"
}

expect_in() {
  grep -qF -- "$2" "${WORK}/$1.log" || fail "$1: output should contain: $2
$(cat "${WORK}/$1.log")"
}

fake ubuntu-2404 2.2.5 2.3.4 2.14 2.34 2.39
fake ubuntu-2204 2.2.5 2.4 2.34
fake ubuntu-2604 2.2.5 2.34 2.39 2.43
fake no-glibc
printf 'not a binary\n' > "${WORK}/text"
good_depends="libc6 (>= 2.39), libdbus-1-3, libxkbcommon0"

# Built on Ubuntu 24.04: exactly the minimum.
run at-minimum "${WORK}/ubuntu-2404"
expect_status at-minimum 0
expect_in at-minimum "needs glibc 2.39 (minimum 2.39): OK"

# Older is fine, and 2.4 sorts below 2.39 (versions, not strings).
run older "${WORK}/ubuntu-2204"
expect_status older 0
expect_in older "needs glibc 2.34 (minimum 2.39): OK"

# Built on Ubuntu 26.04 (#483): fails and names the symbols.
run newer "${WORK}/ubuntu-2604"
expect_status newer 1
expect_in newer "needs glibc 2.43, newer than the supported minimum 2.39"
expect_in newer "  symbol_4"

# A binary with no glibc versions, or something objdump cannot read, is
# not silently passed.
run no-glibc "${WORK}/no-glibc"
expect_status no-glibc 1
expect_in no-glibc "no GLIBC symbol versions found"
run not-elf "${WORK}/text"
expect_status not-elf 1
expect_in not-elf "objdump could not read its dynamic symbols"
run missing "${WORK}/missing"
expect_status missing 1
expect_in missing "no such file"

# Every file is checked, and one failure fails the run.
run several "${WORK}/ubuntu-2604" "${WORK}/ubuntu-2404"
expect_status several 1
expect_in several "ubuntu-2404: needs glibc 2.39 (minimum 2.39): OK"

# The .deb: its zmail-app, and a Depends that asks for the minimum libc6.
deb good ubuntu-2404 "${good_depends}"
run deb-good "${WORK}/good.deb"
expect_status deb-good 0
expect_in deb-good "good.deb:/usr/bin/zmail-app: needs glibc 2.39 (minimum 2.39): OK"

deb newer ubuntu-2604 "${good_depends}"
run deb-newer "${WORK}/newer.deb"
expect_status deb-newer 1
expect_in deb-newer "newer.deb:/usr/bin/zmail-app needs glibc 2.43"

deb no-libc6 ubuntu-2404 "libdbus-1-3, libxkbcommon0"
run deb-no-libc6 "${WORK}/no-libc6.deb"
expect_status deb-no-libc6 1
expect_in deb-no-libc6 "Depends does not ask for libc6 (>= 2.39)"

deb old-libc6 ubuntu-2404 "libc6 (>= 2.34), libdbus-1-3"
run deb-old-libc6 "${WORK}/old-libc6.deb"
expect_status deb-old-libc6 1
expect_in deb-old-libc6 "Depends does not ask for libc6 (>= 2.39)"

deb no-binary "" "${good_depends}"
run deb-no-binary "${WORK}/no-binary.deb"
expect_status deb-no-binary 1
expect_in deb-no-binary "no /usr/bin/zmail-app"

run no-args
expect_status no-args 2

if [[ "${failures}" -gt 0 ]]; then
  echo "${failures} check(s) failed" >&2
  exit 1
fi
echo "check-linux-glibc.sh self-test passed"
