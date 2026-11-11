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
//! - Windows: nothing here yet.
//!
//! Files the CLI writes with mail or crash reports in them (the mbox
//! export, the crash log) are created readable by their owner only.

/// Turns core dumps off for this process and its children (Unix). A
/// failure is reported on stderr and the command goes on.
pub(crate) fn forbid_core_dumps() {
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
