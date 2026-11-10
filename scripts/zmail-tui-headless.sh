#!/usr/bin/env bash
# zmail-tui-headless.sh: run Zmail's terminal UI (`zmail-app tui`) on a Linux
# machine with no desktop session, such as one you reach over SSH.
#
# Zmail keeps its store key and account secrets in the OS keychain (the Secret
# Service on Linux) and has no passphrase or plaintext fallback. Without a
# desktop login there is no Secret Service, so `zmail-app tui` stops with an
# error. This script starts (or reuses) a private D-Bus session bus and
# gnome-keyring's Secret Service on it, asks for the keyring password when the
# keyring is locked, then runs `zmail-app tui` with that bus.
#
#   zmail-tui-headless.sh            start the TUI (arguments go to `zmail-app tui`)
#   zmail-tui-headless.sh --status   show the bus and keyring state
#   zmail-tui-headless.sh --lock     stop the keyring and the bus (secrets locked
#                                    again; the next start asks for the password)
#
# Needs: bash, dbus-daemon, busctl (systemd), gnome-keyring, flock, and
# `zmail-app` on PATH (or set ZMAIL_APP to its path). On Debian and Ubuntu:
# `sudo apt install dbus-daemon gnome-keyring`.
#
# The bus is private to Zmail: it is never exported to your shell, so other
# D-Bus users are unaffected. If your environment already has a session bus
# with a Secret Service on it (a desktop login), that one is used instead.
#
# The keyring lives in ${XDG_DATA_HOME:-~/.local/share}/keyrings/login.keyring,
# encrypted with the password you choose on first run. The password is read
# from the terminal and never stored anywhere.
#
# Security trade-off: once unlocked, the keyring stays unlocked for the life of
# the private bus and keyring daemon, that is until you run `--lock` or the
# machine restarts, not just while the TUI is open. In that time any process
# running as your user can ask that Secret Service for Zmail's secrets, which
# include the key that unlocks your mail store. Run `--lock` when you are done
# on a shared machine. A passphrase-protected store key would avoid this but
# does not exist yet (ADR 0007, "Risks").
set -euo pipefail

zmail_app=${ZMAIL_APP:-zmail-app}
uid=$(id -u)
if [[ -n ${XDG_RUNTIME_DIR:-} && -d ${XDG_RUNTIME_DIR} ]]; then
    rundir=${XDG_RUNTIME_DIR}/zmail-tui-headless
else
    rundir=/tmp/zmail-tui-headless-${uid}
fi
bus_addr=unix:path=${rundir}/bus
keyring_file=${XDG_DATA_HOME:-${HOME}/.local/share}/keyrings/login.keyring
secrets=org.freedesktop.secrets

die() { printf 'zmail-tui-headless: %s\n' "$*" >&2; exit 1; }
say() { printf 'zmail-tui-headless: %s\n' "$*" >&2; }

bus() { busctl --address="${bus_addr}" --auto-start=no --timeout=5 "$@"; }

bus_alive() {
    [[ -S ${rundir}/bus ]] &&
        bus call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus GetId >/dev/null 2>&1
}

# PID of whoever owns org.freedesktop.secrets on our bus, or nothing.
keyring_pid() {
    bus call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus \
        GetConnectionUnixProcessID s "${secrets}" 2>/dev/null | awk '{print $2}' || true
}

# "unlocked", "locked" or "missing" (no Secret Service or no default collection).
keyring_state() {
    local out
    out=$(bus get-property "${secrets}" /org/freedesktop/secrets/aliases/default \
        org.freedesktop.Secret.Collection Locked 2>/dev/null) || { echo missing; return; }
    case ${out} in
        "b false") echo unlocked ;;
        "b true") echo locked ;;
        *) echo missing ;;
    esac
}

prepare_rundir() {
    if [[ ! -e ${rundir} ]]; then
        mkdir -m 700 -- "${rundir}"
    fi
    [[ -d ${rundir} && ! -L ${rundir} && -O ${rundir} ]] ||
        die "${rundir} is not a directory owned by you; remove it and try again"
    [[ $(stat -c %a -- "${rundir}") == 700 ]] || chmod 700 -- "${rundir}"
}

start_bus() {
    local pid
    rm -f -- "${rundir}/bus"
    # No service directories: nothing is auto-started on this bus, so a locked
    # keyring can never be activated behind zmail's back.
    cat >"${rundir}/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>${bus_addr}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
    pid=$(dbus-daemon --config-file="${rundir}/bus.conf" --fork --print-pid --nopidfile 9>&- </dev/null) ||
        die "could not start dbus-daemon"
    echo "${pid}" >"${rundir}/dbus.pid"
    for _ in $(seq 50); do bus_alive && return 0; sleep 0.1; done
    die "dbus-daemon started but its bus at ${rundir}/bus does not answer"
}

stop_keyring() {
    local pid
    pid=$(keyring_pid)
    [[ -n ${pid} ]] || return 0
    kill "${pid}" 2>/dev/null || true
    for _ in $(seq 50); do [[ -z $(keyring_pid) ]] && return 0; sleep 0.1; done
    kill -9 "${pid}" 2>/dev/null || true
}

stop_bus() {
    local pid
    if [[ -f ${rundir}/dbus.pid ]]; then
        pid=$(<"${rundir}/dbus.pid")
        # Only kill it if it is still our dbus-daemon.
        if [[ ${pid} =~ ^[0-9]+$ ]] && tr '\0' ' ' <"/proc/${pid}/cmdline" 2>/dev/null |
            grep -q "dbus-daemon --config-file=${rundir}/bus.conf"; then
            kill "${pid}" 2>/dev/null || true
        fi
        rm -f -- "${rundir}/dbus.pid"
    fi
    rm -f -- "${rundir}/bus"
}

# Start gnome-keyring on our bus, unlocking (or creating) the login keyring
# with the password on stdin.
start_keyring() {
    DBUS_SESSION_BUS_ADDRESS=${bus_addr} gnome-keyring-daemon --unlock --daemonize \
        --components=secrets --control-directory="${rundir}/keyring" >/dev/null 9>&- ||
        return 1
    for _ in $(seq 50); do [[ -n $(keyring_pid) ]] && return 0; sleep 0.1; done
    return 1
}

read_password() { # prompt -> variable name
    local __pw
    IFS= read -rs -p "$1" __pw </dev/tty || { printf '\n' >/dev/tty; die "no password read"; }
    printf '\n' >/dev/tty
    printf -v "$2" '%s' "${__pw}"
}

unlock_keyring() {
    local pw pw2 tries
    { : </dev/tty; } 2>/dev/null || die "the keyring is locked and there is no terminal to ask for its password"
    if [[ ! -e ${keyring_file} ]]; then
        say "first run: creating a keyring for Zmail's secrets in ${keyring_file}"
        say "choose its password; you will type it whenever the keyring has been locked (after --lock or a restart of this machine)"
        while :; do
            read_password "New keyring password: " pw
            if [[ -z ${pw} ]]; then
                say "an empty password would store the keyring unencrypted; choose one"
                continue
            fi
            read_password "Repeat it: " pw2
            [[ ${pw} == "${pw2}" ]] && break
            say "the passwords differ; try again"
        done
        pw2=
        stop_keyring
        printf '%s' "${pw}" | start_keyring || { pw=; die "gnome-keyring-daemon did not start"; }
        pw=
        [[ $(keyring_state) == unlocked ]] || die "the new keyring did not come up unlocked"
        return 0
    fi
    for tries in 1 2 3; do
        read_password "Keyring password (zmail): " pw
        stop_keyring
        # printf is a builtin, so the password never appears in a process list.
        printf '%s' "${pw}" | start_keyring || { pw=; die "gnome-keyring-daemon did not start"; }
        pw=
        [[ $(keyring_state) == unlocked ]] && return 0
        say "wrong password (attempt ${tries} of 3)"
    done
    stop_keyring
    die "the keyring is still locked"
}

status() {
    echo "runtime dir:  ${rundir}"
    echo "keyring file: ${keyring_file} ($([[ -e ${keyring_file} ]] && echo present || echo 'not created yet'))"
    if bus_alive; then
        echo "bus:          running (${bus_addr}, pid $(cat "${rundir}/dbus.pid" 2>/dev/null || echo '?'))"
        local pid
        pid=$(keyring_pid)
        echo "keyring:      ${pid:+running (pid ${pid}), }$(keyring_state)"
    else
        echo "bus:          not running"
        echo "keyring:      not running"
    fi
}

case ${1:-} in
    --status) status; exit 0 ;;
    --lock)
        prepare_rundir
        exec 9>"${rundir}/lock"; flock 9
        if pgrep -u "${uid}" -x zmail-app >/dev/null; then
            say "note: zmail-app is still running; quit it first, or it loses access to its secrets"
        fi
        bus_alive && stop_keyring
        stop_bus
        say "keyring locked and bus stopped"
        exit 0
        ;;
esac

command -v "${zmail_app}" >/dev/null || die "${zmail_app} is not on PATH"

# A desktop session that already has a Secret Service: use it as it is.
if [[ -n ${DBUS_SESSION_BUS_ADDRESS:-} ]] &&
    busctl --user --auto-start=no --timeout=5 call org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus NameHasOwner s "${secrets}" 2>/dev/null | grep -q '^b true'; then
    exec "${zmail_app}" tui "$@"
fi

command -v dbus-daemon >/dev/null || die "dbus-daemon is not installed (Debian and Ubuntu: apt install dbus-daemon)"
command -v gnome-keyring-daemon >/dev/null || die "gnome-keyring-daemon is not installed (Debian and Ubuntu: apt install gnome-keyring)"

prepare_rundir
exec 9>"${rundir}/lock"
flock 9
bus_alive || { stop_bus; start_bus; }
[[ $(keyring_state) == unlocked ]] || unlock_keyring
exec 9>&-

export DBUS_SESSION_BUS_ADDRESS=${bus_addr}
exec "${zmail_app}" tui "$@"
