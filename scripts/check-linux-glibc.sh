#!/usr/bin/env bash
set -euo pipefail

# check-linux-glibc.sh FILE... - Fails when a Linux build needs a newer glibc
# than Zmail supports (#483).
#
# Each FILE is a dynamically linked binary (zmail-app) or the .deb. The
# check reads the GLIBC_2.x symbol versions the binary asks for with
# `objdump -T` and fails when the highest is above MIN_GLIBC. For a .deb it
# checks the zmail-app inside, and that the package's Depends asks for
# `libc6 (>= MIN_GLIBC)`, so apt refuses it where it cannot run.
#
# The release binaries are built on Ubuntu 24.04 (release-please.yml); a
# binary built on a newer image links against that image's glibc and does
# not start on older systems. Raise MIN_GLIBC only together with the build
# image, the libc6 dependency in crates/zmail-app/Cargo.toml and the
# minimum in docs/install.md.
MIN_GLIBC="2.39"

usage() {
  echo "usage: $0 FILE..." >&2
  echo "  FILE: a Linux zmail-app binary or zmail .deb; fails when it needs glibc newer than ${MIN_GLIBC}" >&2
  exit 2
}

# newest_glibc FILE - prints the highest GLIBC_2.x version FILE needs, or
# nothing when it needs none.
newest_glibc() {
  local symbols
  symbols="$(objdump -T "$1")" || return 1
  { grep -o 'GLIBC_2\.[0-9][0-9.]*' <<< "${symbols}" || true; } | sed 's/^GLIBC_//' | sort -uV | tail -n 1
}

# check_binary FILE [LABEL]
check_binary() {
  local file="$1" label="${2:-$1}" newest
  if ! newest="$(newest_glibc "${file}")"; then
    echo "::error::${label}: objdump could not read its dynamic symbols" >&2
    return 1
  fi
  if [[ -z "${newest}" ]]; then
    echo "::error::${label}: no GLIBC symbol versions found; is it a dynamically linked Linux binary?" >&2
    return 1
  fi
  # sort -V puts the larger version last.
  if [[ "$(printf '%s\n%s\n' "${MIN_GLIBC}" "${newest}" | sort -V | tail -n 1)" != "${MIN_GLIBC}" ]]; then
    echo "::error::${label} needs glibc ${newest}, newer than the supported minimum ${MIN_GLIBC}; was it built on a newer image than Ubuntu 24.04?" >&2
    objdump -T "${file}" | grep -F "GLIBC_${newest}" | awk '{print "  " $NF}' | sort -u >&2 || true
    return 1
  fi
  echo "${label}: needs glibc ${newest} (minimum ${MIN_GLIBC}): OK"
}

# check_deb FILE
check_deb() {
  local deb="$1" depends work status=0
  depends="$(dpkg-deb --field "${deb}" Depends)"
  if ! grep -qE "(^|, )libc6 \(>= ${MIN_GLIBC//./\\.}\)(,|$)" <<< "${depends}"; then
    echo "::error::${deb}: Depends does not ask for libc6 (>= ${MIN_GLIBC}): ${depends}" >&2
    status=1
  fi
  work="$(mktemp -d)"
  dpkg-deb --extract "${deb}" "${work}"
  if [[ -f "${work}/usr/bin/zmail-app" ]]; then
    check_binary "${work}/usr/bin/zmail-app" "${deb}:/usr/bin/zmail-app" || status=1
  else
    echo "::error::${deb}: no /usr/bin/zmail-app" >&2
    status=1
  fi
  rm -rf "${work}"
  return "${status}"
}

[[ $# -gt 0 ]] || usage

failed=0
for file in "$@"; do
  if [[ ! -f "${file}" ]]; then
    echo "::error::${file}: no such file" >&2
    failed=1
  elif [[ "${file}" == *.deb ]]; then
    check_deb "${file}" || failed=1
  else
    check_binary "${file}" || failed=1
  fi
done
exit "${failed}"
