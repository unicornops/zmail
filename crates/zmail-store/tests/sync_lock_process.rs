//! A second process is refused the sync lock (ADR 0007).
//!
//! The test starts this test binary again as the second process. It lives
//! in a test binary of its own because starting a process copies every
//! descriptor the parent has open into the child until it runs its
//! program, lock files included: in the library's tests it briefly kept
//! other tests' dropped store locks alive, and they failed at random
//! (#345). Nothing else here takes a lock.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use zmail_store::SyncLock;

/// Set in the child process `a_second_process_is_refused_the_sync_lock`
/// starts: the database whose sync lock the child tries.
const CHILD_DATABASE: &str = "ZMAIL_SYNC_LOCK_CHILD_DATABASE";

fn database(name: &str) -> (PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("zmail-lock-{name}-{nanos}-{}", std::process::id()));
    (directory.join("store.sqlite3"), directory)
}

/// Runs [`sync_lock_child`] in a new process on `path`.
fn child(path: &Path) -> Output {
    Command::new(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "sync_lock_child", "--nocapture"])
        .env(CHILD_DATABASE, path)
        .output()
        .expect("the child runs")
}

#[test]
fn a_second_process_is_refused_the_sync_lock() {
    let (path, directory) = database("process");
    let held = SyncLock::try_acquire(&path)
        .expect("the lock file opens")
        .expect("the lock is free");
    let output = child(&path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "child failed: {stdout}");
    assert!(
        stdout.contains("sync lock: refused"),
        "child output: {stdout}"
    );
    drop(held);
    let output = child(&path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "child failed: {stdout}");
    assert!(
        stdout.contains("sync lock: acquired"),
        "child output: {stdout}"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// The child half of `a_second_process_is_refused_the_sync_lock`; does
/// nothing in a normal test run.
#[test]
fn sync_lock_child() {
    let Some(path) = std::env::var_os(CHILD_DATABASE) else {
        return;
    };
    match SyncLock::try_acquire(Path::new(&path)).expect("the lock file opens") {
        Some(_) => println!("sync lock: acquired"),
        None => println!("sync lock: refused"),
    }
}
