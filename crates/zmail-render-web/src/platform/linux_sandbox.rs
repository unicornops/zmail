//! WebKitGTK's sandbox, checked before and after Zmail uses the engine.
//!
//! WebKitGTK runs each web process inside bubblewrap (`bwrap`) when the
//! sandbox is on, which Zmail always asks for. But the engine quietly runs
//! web processes *without* a sandbox inside a snap, inside a Flatpak whose
//! `flatpak-spawn` cannot sandbox, or when an environment variable says
//! so; and when bubblewrap cannot create its namespaces (Ubuntu 24.04's
//! AppArmor restriction of unprivileged user namespaces, a container
//! without them) the engine aborts the whole process. Zmail never shows
//! mail in an unsandboxed engine, and never reaches that abort, so before
//! the engine is started at all:
//!
//! 1. [`environment_refusal`]: refuse where the engine would drop the
//!    sandbox by itself;
//! 2. [`probe_bubblewrap`]: run bubblewrap the way WebKit's own container
//!    check does, and refuse if it fails.
//!
//! And after the engine starts its first web process (a blank page, before
//! any mail is loaded), [`check_web_processes`] confirms that every web
//! process of ours runs in its own user and PID namespaces. A refusal is the
//! text view, with the reason shown to the user.
//!
//! Safe code only: environment variables, `/proc` and a child process.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// The bubblewrap WebKitGTK runs web processes in. Distributions build
/// WebKitGTK with this path compiled in (`BWRAP_EXECUTABLE`).
pub(crate) const BUBBLEWRAP: &str = "/usr/bin/bwrap";

/// The D-Bus proxy WebKitGTK starts for each sandboxed web process
/// (`DBUS_PROXY_EXECUTABLE`).
pub(crate) const DBUS_PROXY: &str = "/usr/bin/xdg-dbus-proxy";

/// Environment variables that make WebKitGTK weaken or drop its sandbox.
const SANDBOX_OVERRIDES: [&str; 2] = [
    "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS",
    "WEBKIT_ENABLE_DEBUG_PERMISSIONS_IN_SANDBOX",
];

/// How long bubblewrap may take to start and run `true`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The name the kernel gives WebKitGTK's web processes (`comm` is cut at
/// 15 bytes: `WebKitWebProcess`).
const WEB_PROCESS_NAME: &str = "WebKitWebProces";

/// Why the engine would run without its sandbox here, if it would:
/// `var` reads an environment variable and `exists` checks a path.
pub(crate) fn environment_refusal(
    var: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> Option<String> {
    if let Some(name) = SANDBOX_OVERRIDES.iter().find(|name| var(name).is_some()) {
        return Some(format!(
            "{name} is set, which turns off the web engine's sandbox"
        ));
    }
    // WebKit's own test for a snap: all three variables.
    if ["SNAP", "SNAP_NAME", "SNAP_REVISION"]
        .iter()
        .all(|name| var(name).is_some())
    {
        return Some(
            "Zmail is running as a snap, where the web engine runs without its sandbox".to_owned(),
        );
    }
    if exists(Path::new("/.flatpak-info")) {
        return Some(
            "Zmail is running in Flatpak, where the web engine's sandbox depends on \
             flatpak-spawn and is not checked"
                .to_owned(),
        );
    }
    for (path, package) in [(BUBBLEWRAP, "bubblewrap"), (DBUS_PROXY, "xdg-dbus-proxy")] {
        if !exists(Path::new(path)) {
            return Some(format!(
                "{path} is missing; install {package} for the web engine's sandbox"
            ));
        }
    }
    None
}

/// [`environment_refusal`] for this process.
pub(crate) fn this_environment_refusal() -> Option<String> {
    environment_refusal(|name| std::env::var_os(name), Path::exists)
}

/// Runs bubblewrap as WebKit's container check does (every namespace
/// unshared, `true` inside), and says why it failed if it did.
pub(crate) fn probe_bubblewrap() -> Result<(), String> {
    let started = Instant::now();
    let mut child = Command::new(BUBBLEWRAP)
        .args([
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--unshare-all",
            "true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the web engine's sandbox (bubblewrap) did not run: {error}"))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < PROBE_TIMEOUT => {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the web engine's sandbox (bubblewrap) did not start in time".into());
            }
            Err(error) => {
                return Err(format!(
                    "the web engine's sandbox (bubblewrap) did not run: {error}"
                ));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let restricted = fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|value| value.trim() == "1");
    Err(probe_failure(stderr.trim(), restricted))
}

/// The reason given when the bubblewrap probe fails with `stderr`;
/// `apparmor_restricted` is whether AppArmor restricts unprivileged user
/// namespaces (Ubuntu 23.10 and later).
pub(crate) fn probe_failure(stderr: &str, apparmor_restricted: bool) -> String {
    let detail = stderr
        .lines()
        .next()
        .map(|line| {
            // bubblewrap's own advice ("See <https://...>") is for its
            // packagers, not for Zmail's users.
            let line = line.trim_start_matches("bwrap: ");
            line.split(" See <")
                .next()
                .unwrap_or(line)
                .trim()
                .trim_end_matches('.')
        })
        .filter(|line| !line.is_empty())
        .unwrap_or("it exited with an error");
    let mut reason = format!("the web engine's sandbox (bubblewrap) cannot start: {detail}");
    if apparmor_restricted {
        reason.push_str(
            ". AppArmor restricts unprivileged user namespaces on this system \
             (Ubuntu 24.04 and later), and no AppArmor profile allows them for Zmail",
        );
    } else if detail.contains("namespace") && !detail.contains("user namespaces") {
        reason.push_str(". The system does not allow unprivileged user namespaces");
    }
    reason
}

/// A process seen in `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessInfo {
    /// Its parent's PID.
    pub parent: u32,
    /// Its name (`comm`).
    pub name: String,
}

/// The PIDs of the web processes descended from `ancestor` in `processes`.
pub(crate) fn web_processes_of(ancestor: u32, processes: &HashMap<u32, ProcessInfo>) -> Vec<u32> {
    let mut found: Vec<u32> = processes
        .iter()
        .filter(|(_, info)| info.name == WEB_PROCESS_NAME)
        .filter(|(pid, _)| {
            let mut current = **pid;
            // Bounded, in case a PID was reused into a loop between reads.
            for _ in 0..64 {
                let Some(info) = processes.get(&current) else {
                    return false;
                };
                if info.parent == ancestor {
                    return true;
                }
                current = info.parent;
            }
            false
        })
        .map(|(pid, _)| *pid)
        .collect();
    found.sort_unstable();
    found
}

/// Parses `/proc/<pid>/stat` into the parent PID and the name.
pub(crate) fn parse_stat(stat: &str) -> Option<ProcessInfo> {
    // `pid (comm) state ppid ...`, where comm may itself hold spaces and
    // parentheses, so it ends at the last ')'.
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let parent = rest.next()?.parse().ok()?;
    Some(ProcessInfo { parent, name })
}

fn processes() -> HashMap<u32, ProcessInfo> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            Some((pid, parse_stat(&stat)?))
        })
        .collect()
}

fn namespace(pid: &str, kind: &str) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/ns/{kind}")).ok()
}

/// Confirms that each of this process's web processes runs in a user and
/// PID namespace of its own; returns how many there are. Fails when there
/// are none, when a namespace cannot be read, or when one is shared with
/// Zmail (an unsandboxed web process).
pub(crate) fn check_web_processes() -> Result<usize, String> {
    let ours = std::process::id();
    let (Some(our_user), Some(our_pid)) = (namespace("self", "user"), namespace("self", "pid"))
    else {
        return Err("the web engine's sandbox could not be checked: /proc is unreadable".into());
    };
    let found = web_processes_of(ours, &processes());
    if found.is_empty() {
        return Err("the web engine's sandbox could not be checked: no web process found".into());
    }
    for pid in &found {
        let pid = pid.to_string();
        let (Some(user), Some(pid_ns)) = (namespace(&pid, "user"), namespace(&pid, "pid")) else {
            return Err(format!(
                "the web engine's sandbox could not be checked: web process {pid} is unreadable"
            ));
        };
        if user == our_user || pid_ns == our_pid {
            return Err(format!(
                "the web engine started web process {pid} without its sandbox"
            ));
        }
    }
    Ok(found.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[&str]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<String> = vars.iter().map(|name| (*name).to_owned()).collect();
        move |name| {
            vars.iter()
                .any(|var| var == name)
                .then(|| OsString::from("1"))
        }
    }

    fn paths(present: &[&str]) -> impl Fn(&Path) -> bool {
        let present: Vec<PathBuf> = present.iter().map(PathBuf::from).collect();
        move |path| present.iter().any(|candidate| candidate == path)
    }

    const TOOLS: [&str; 2] = [BUBBLEWRAP, DBUS_PROXY];

    #[test]
    fn a_plain_system_with_the_tools_is_not_refused() {
        assert_eq!(environment_refusal(env(&[]), paths(&TOOLS)), None);
    }

    #[test]
    fn the_sandbox_overrides_are_refused() {
        for name in SANDBOX_OVERRIDES {
            let reason = environment_refusal(env(&[name]), paths(&TOOLS)).expect("refused");
            assert!(reason.contains(name), "{reason}");
        }
    }

    #[test]
    fn a_snap_needs_all_three_variables() {
        assert_eq!(environment_refusal(env(&["SNAP"]), paths(&TOOLS)), None);
        let reason =
            environment_refusal(env(&["SNAP", "SNAP_NAME", "SNAP_REVISION"]), paths(&TOOLS))
                .expect("refused");
        assert!(reason.contains("snap"), "{reason}");
    }

    #[test]
    fn flatpak_and_missing_tools_are_refused() {
        let flatpak = [BUBBLEWRAP, DBUS_PROXY, "/.flatpak-info"];
        assert!(
            environment_refusal(env(&[]), paths(&flatpak))
                .expect("refused")
                .contains("Flatpak")
        );
        assert!(
            environment_refusal(env(&[]), paths(&[DBUS_PROXY]))
                .expect("refused")
                .contains("bubblewrap")
        );
        assert!(
            environment_refusal(env(&[]), paths(&[BUBBLEWRAP]))
                .expect("refused")
                .contains("xdg-dbus-proxy")
        );
    }

    #[test]
    fn probe_failures_name_the_cause() {
        let ubuntu = probe_failure("bwrap: setting up uid map: Permission denied\n", true);
        assert!(
            ubuntu.contains("setting up uid map: Permission denied"),
            "{ubuntu}"
        );
        assert!(ubuntu.contains("AppArmor"), "{ubuntu}");
        let container = probe_failure(
            "bwrap: No permissions to create new namespace, likely because the kernel does \
             not allow non-privileged user namespaces. See <https://deb.li/bubblewrap>.",
            false,
        );
        assert!(container.contains("user namespaces"), "{container}");
        assert!(
            !container.contains("deb.li") && !container.contains(".."),
            "{container}"
        );
        assert!(probe_failure("", false).contains("exited with an error"));
    }

    #[test]
    fn stat_lines_parse_even_with_odd_names() {
        assert_eq!(
            parse_stat("4242 (WebKitWebProces) S 4200 4242 4242 0 -1"),
            Some(ProcessInfo {
                parent: 4200,
                name: WEB_PROCESS_NAME.to_owned()
            })
        );
        assert_eq!(
            parse_stat("7 (a) b (c)) R 1 7 7"),
            Some(ProcessInfo {
                parent: 1,
                name: "a) b (c)".to_owned()
            })
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn only_our_descendant_web_processes_count() {
        let process = |parent, name: &str| ProcessInfo {
            parent,
            name: name.to_owned(),
        };
        let table: HashMap<u32, ProcessInfo> = [
            (100, process(1, "zmail")),
            (110, process(100, "bwrap")),
            (111, process(110, "bwrap")),
            (112, process(111, WEB_PROCESS_NAME)),
            (120, process(100, "WebKitNetworkPr")),
            // Another app's web process.
            (200, process(1, "epiphany")),
            (201, process(200, WEB_PROCESS_NAME)),
            // A loop that must not hang.
            (300, process(301, WEB_PROCESS_NAME)),
            (301, process(300, "x")),
        ]
        .into_iter()
        .collect();
        assert_eq!(web_processes_of(100, &table), vec![112]);
        assert!(web_processes_of(200, &table).contains(&201));
    }
}
