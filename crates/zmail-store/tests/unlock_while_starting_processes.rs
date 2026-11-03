//! A store unlocks while another thread starts processes (#345).
//!
//! Starting a process copies every descriptor the parent has open into the
//! child until it runs its program, lock files included, and a lock
//! belongs to the open file, so the copy holds it too. `LocalStore::unlock`
//! takes the store's lock exclusively, then holds it shared while the store
//! is open; if it dropped the exclusive lock and locked the file again, a
//! process started in between could keep the exclusive lock for a moment
//! and the store would refuse to open as if another Zmail were encrypting
//! it. A frontend starts processes from other threads (a browser for a
//! link, a settings probe), so the unlock has to hold up. This lives in a
//! test binary of its own, as the processes it starts would disturb other
//! tests' locks.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use zmail_security::InMemorySecretStore;
use zmail_store::LocalStore;

/// Unlocks in the test; each takes and hands over the exclusive lock.
const UNLOCKS: usize = 200;

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "zmail-unlock-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn a_store_unlocks_while_another_thread_starts_processes() {
    let directory = temp_dir("spawning");
    let path = directory.join("zmail.sqlite3");
    let keychain = InMemorySecretStore::default();
    drop(LocalStore::unlock(&path, &keychain, &|_| {}).expect("a new store unlocks"));

    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // This test binary again, running no tests.
            let program = std::env::current_exe().expect("the test binary");
            while !done.load(Ordering::Relaxed) {
                Command::new(&program)
                    .args(["--exact", "no test has this name"])
                    .stdout(Stdio::null())
                    .status()
                    .expect("the child runs");
            }
        });
        let result = (0..UNLOCKS).try_for_each(|_| {
            LocalStore::unlock(&path, &keychain, &|_| {}).map(|(store, _)| drop(store))
        });
        done.store(true, Ordering::Relaxed);
        if let Err(error) = result {
            panic!("the store unlocks: {error:#}");
        }
    });
    let _ = std::fs::remove_dir_all(directory);
}
