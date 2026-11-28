//! Keeping the store key and mail out of crash dumps (ADR 0005): a core
//! file of a running Zmail holds the store key, decrypted pages of the
//! store, passwords and tokens in plaintext.
//!
//! - Unix: the core file size limit (`RLIMIT_CORE`) is set to 0, soft and
//!   hard, so a crash writes no core file, and systemd-coredump and
//!   apport, which honour the limit, store nothing. Child processes (the
//!   web renderer's engine processes, the terminal UI's external editor)
//!   inherit the limit and otherwise run as before.
//! - Linux, for the commands that open no window: the process is marked
//!   not dumpable (`PR_SET_DUMPABLE` 0), which also stops a core pattern
//!   pipe that ignores the limit and other processes of the same user
//!   attaching to it. A child is dumpable again from its `exec`. The
//!   desktop app is left dumpable: xdg-desktop-portal identifies its
//!   caller through `/proc/PID/root`, which a process that isn't dumpable
//!   doesn't let it open, so file dialogs and other portal calls would
//!   fail.
//! - Windows: the executable is put on Windows Error Reporting's
//!   exclusion list for the user (`ExcludedApplications`, the value
//!   `WerAddExcludedApplication` writes), so WER queues no report of a
//!   crash: no minidump for Microsoft, and no fault dialog. WER's
//!   local dumps (`LocalDumps`, off unless an administrator or a program
//!   turned them on for the machine) ignore that list and can only be
//!   turned off for an executable by an administrator: the MSI does it
//!   for `zmail-app.exe`, and puts it on the exclusion list for every
//!   user too (`wix/main.wxs`). A portable copy on a machine with local
//!   dumps on still leaves one. The desktop app and `zmail-app tui` are
//!   the same executable. WebView2's processes keep their crash dumps in
//!   Zmail's own user data folder (`zmail-render-web`). CI crashes both
//!   on purpose (`scripts/check-windows-crash-dumps.ps1`).
//!
//! Files the CLI writes with mail or crash reports in them (the mbox
//! export, the crash log) are created readable by their owner only.

/// Turns core dumps off for this process and its children (Unix), or
/// keeps Windows Error Reporting from queueing a report with a dump
/// (Windows). A failure is reported on stderr and the command goes on.
pub(crate) fn forbid_core_dumps() {
    #[cfg(windows)]
    if let Err(error) = exclude_from_error_reporting() {
        eprintln!(
            "warning: couldn't keep Windows Error Reporting from saving crash dumps: {error}"
        );
    }
    #[cfg(unix)]
    if let Err(error) = rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    ) {
        eprintln!("warning: couldn't turn core dumps off: {error}");
    }
}

/// Windows Error Reporting's per-user exclusion list, under
/// `HKEY_CURRENT_USER`: a DWORD 1 named after an executable's file name
/// (`zmail-app.exe`) keeps WER from reporting its crashes. It is what
/// `WerAddExcludedApplication(path, FALSE)` writes (tried on a Windows
/// runner: the file name only, whatever the path), written here through
/// the safe registry API, as the workspace forbids `unsafe` outside the
/// web renderer's platform code.
#[cfg_attr(not(windows), allow(dead_code))]
const WER_EXCLUDED_APPLICATIONS: &str =
    r"Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications";

/// The name WER's exclusion list knows `exe` by: its file name.
#[cfg_attr(not(windows), allow(dead_code))]
fn error_reporting_name(exe: &std::path::Path) -> Option<&str> {
    exe.file_name()?.to_str()
}

/// Puts this executable on the user's WER exclusion list, unless it is
/// there already. WER reads the list when a process crashes.
#[cfg(windows)]
fn exclude_from_error_reporting() -> Result<(), String> {
    exclude_from_error_reporting_in(windows_registry::CURRENT_USER, WER_EXCLUDED_APPLICATIONS)
}

/// [`exclude_from_error_reporting`] with the root and key given, for tests.
#[cfg(windows)]
fn exclude_from_error_reporting_in(root: &windows_registry::Key, key: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("no executable path: {error}"))?;
    let name =
        error_reporting_name(&exe).ok_or_else(|| format!("no file name in {}", exe.display()))?;
    let key = root
        .create(key)
        .map_err(|error| format!("opening {key}: {error}"))?;
    if key.get_u32(name).ok() == Some(1) {
        return Ok(());
    }
    key.set_u32(name, 1)
        .map_err(|error| format!("setting {name}: {error}"))
}

/// Marks the process not dumpable (Linux), for the commands that open no
/// window. A failure is reported on stderr and the command goes on.
pub(crate) fn make_undumpable() {
    #[cfg(target_os = "linux")]
    if let Err(error) =
        rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
    {
        eprintln!("warning: couldn't mark the process not dumpable: {error}");
    }
}

/// Debug builds only: crash the app on purpose, so CI can check that a
/// crash leaves no dump (`scripts/check-windows-crash-dumps.ps1`).
/// `abort` panics (the crash log records it) and then aborts, as a panic
/// that cannot unwind does; `stack-overflow` overflows a thread's stack, an
/// exception nothing handles. `:MS` waits that many milliseconds first, on
/// a thread of its own, so the window or the terminal UI is up by then.
/// Release builds have neither the variable nor the code: a user's
/// environment or a message can't reach it there (CI's build job checks
/// the release binary for the name).
#[cfg(debug_assertions)]
pub(crate) const FORCE_CRASH_ENV: &str = "ZMAIL_DEBUG_FORCE_CRASH";

/// A crash [`FORCE_CRASH_ENV`] asks for.
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForcedCrash {
    Abort,
    StackOverflow,
}

/// Reads a [`FORCE_CRASH_ENV`] value: the crash and how long to wait.
#[cfg(debug_assertions)]
pub(crate) fn parse_forced_crash(value: &str) -> Option<(ForcedCrash, std::time::Duration)> {
    let (kind, delay) = match value.split_once(':') {
        Some((kind, delay)) => (kind, delay.parse().ok()?),
        None => (value, 0),
    };
    let kind = match kind {
        "abort" => ForcedCrash::Abort,
        "stack-overflow" => ForcedCrash::StackOverflow,
        _ => return None,
    };
    Some((kind, std::time::Duration::from_millis(delay)))
}

/// Crashes the process if [`FORCE_CRASH_ENV`] asks for it (debug builds;
/// nothing in release builds). Called once the crash log is in place.
pub(crate) fn crash_if_asked() {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var(FORCE_CRASH_ENV) {
        let Some((kind, delay)) = parse_forced_crash(&value) else {
            eprintln!("warning: {FORCE_CRASH_ENV}={value} isn't abort or stack-overflow[:MS]");
            return;
        };
        eprintln!("{FORCE_CRASH_ENV}: crashing ({kind:?}) in {delay:?}");
        let crash = move || {
            std::thread::sleep(delay);
            crash(kind);
        };
        if let Err(error) = std::thread::Builder::new()
            .name("forced-crash".to_owned())
            .spawn(crash)
        {
            eprintln!("warning: {FORCE_CRASH_ENV}: no thread to crash on: {error}");
        }
    }
}

#[cfg(debug_assertions)]
fn crash(kind: ForcedCrash) {
    match kind {
        ForcedCrash::Abort => {
            let _ = std::panic::catch_unwind(|| panic!("forced crash ({FORCE_CRASH_ENV})"));
            std::process::abort();
        }
        ForcedCrash::StackOverflow => {
            std::hint::black_box(overflow(0));
        }
    }
}

/// Recurses until the thread's stack runs out.
#[cfg(debug_assertions)]
fn overflow(depth: u64) -> u64 {
    if depth == u64::MAX {
        return 0;
    }
    let frame = std::hint::black_box([depth; 4096]);
    overflow(depth + 1).wrapping_add(frame[0])
}

/// Options that create a file readable and writable by its owner only
/// (mode 0600 on Unix); an existing file keeps its mode.
pub(crate) fn private_file() -> std::fs::OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Creates `path` and its missing parents, each new one readable by its
/// owner only (mode 0700 on Unix).
pub(crate) fn create_private_dir_all(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Makes an existing file readable and writable by its owner only (Unix),
/// as files created before [`private_file`] may not be. Nothing there is
/// fine.
pub(crate) fn make_private(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Set in the child processes of
    /// `core_dumps_are_off_and_children_still_run`: which half to play.
    const ROLE: &str = "ZMAIL_APP_HARDENING_ROLE";

    #[test]
    fn core_dumps_are_off_and_children_still_run() {
        // This test process stays as it is; a child plays the app.
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "hardening::tests::hardening_child",
                "--nocapture",
            ])
            .env(ROLE, "app")
            .output()
            .expect("the child runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "child failed: {stdout}");
        assert!(
            stdout.contains("app: core limit 0/0, not dumpable"),
            "{stdout}"
        );
        // Its own child, as an editor or engine process would be: the
        // limit carries over, and `exec` makes it dumpable again.
        assert!(
            stdout.contains("spawned: core limit 0/0, dumpable"),
            "{stdout}"
        );
    }

    /// The child halves of `core_dumps_are_off_and_children_still_run`;
    /// does nothing in a normal test run.
    #[test]
    fn hardening_child() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        if role == "app" {
            forbid_core_dumps();
            make_undumpable();
        }
        let limit = rustix::process::getrlimit(rustix::process::Resource::Core);
        let dumpable = match rustix::process::dumpable_behavior() {
            Ok(rustix::process::DumpableBehavior::NotDumpable) => "not dumpable",
            Ok(_) => "dumpable",
            Err(_) => "unknown",
        };
        println!(
            "{role}: core limit {}/{}, {dumpable}",
            limit
                .current
                .map_or("unlimited".to_owned(), |v| v.to_string()),
            limit
                .maximum
                .map_or("unlimited".to_owned(), |v| v.to_string()),
        );
        if role == "app" {
            let output =
                std::process::Command::new(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        "hardening::tests::hardening_child",
                        "--nocapture",
                    ])
                    .env(ROLE, "spawned")
                    .output()
                    .expect("the spawned process runs");
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
}

#[cfg(test)]
mod crash_dump_tests {
    use super::*;

    #[test]
    fn wer_knows_the_executable_by_its_file_name() {
        let exe = std::path::Path::new("C:/Program Files/Zmail/zmail-app.exe");
        assert_eq!(error_reporting_name(exe), Some("zmail-app.exe"));
        assert_eq!(error_reporting_name(std::path::Path::new("/")), None);
        assert!(
            WER_EXCLUDED_APPLICATIONS.ends_with(r"Windows Error Reporting\ExcludedApplications")
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn forced_crashes_are_read_strictly() {
        use std::time::Duration;
        assert_eq!(
            parse_forced_crash("abort"),
            Some((ForcedCrash::Abort, Duration::ZERO))
        );
        assert_eq!(
            parse_forced_crash("stack-overflow:2500"),
            Some((ForcedCrash::StackOverflow, Duration::from_millis(2500)))
        );
        for value in ["", "ABORT", "abort:", "abort:soon", "segfault", "abort:-1"] {
            assert_eq!(parse_forced_crash(value), None, "{value:?}");
        }
    }

    /// The exclusion is written once, as a DWORD 1 named after the test
    /// binary, here under a scratch key instead of WER's.
    #[cfg(windows)]
    #[test]
    fn the_executable_goes_on_the_exclusion_list() {
        let root = windows_registry::CURRENT_USER;
        let key = format!(r"Software\Zmail\tests\wer-{}", std::process::id());
        let name = std::env::current_exe().expect("the test binary");
        let name = error_reporting_name(&name).expect("a file name").to_owned();
        exclude_from_error_reporting_in(root, &key).expect("the first write");
        exclude_from_error_reporting_in(root, &key).expect("already there");
        let value = root.open(&key).and_then(|key| key.get_u32(&name));
        let _ = root.remove_tree(&key);
        assert_eq!(value.ok(), Some(1));
    }
}
