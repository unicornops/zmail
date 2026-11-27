//! Stores opened on several threads at once, as a process's first use of
//! SQLite, all open (#514).
//!
//! SQLite marks itself initialised, and lets other threads through, before
//! it runs SQLCipher's own set-up (`SQLITE_EXTRA_INIT`, which libsqlite3-sys
//! sets to `sqlcipher_extra_init`). A thread that opens and keys a
//! connection in that window gets SQLCipher's catch-all refusal, "PRAGMA key
//! requires a key of one or more characters", although its key is a full
//! 32-byte raw key: SQLCipher answers every failed `PRAGMA key` with that
//! message. The window exists once per process, so each round runs in a
//! fresh child process (this test binary, run again on just this test), and
//! other threads open plain SQLite connections meanwhile, as other tests or
//! threads of the app may, to hold up the thread that sets SQLCipher up.
//!
//! The window is short and depends on scheduling: this test caught it now
//! and then on a loaded machine before the store waited for SQLCipher's
//! set-up, never every time. It keeps the scenario covered.

use std::{
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use zmail_store::LocalStore;

const TEST_NAME: &str = "stores_opened_at_once_as_the_first_use_of_sqlite_all_open";

/// Set in the child processes: open the stores instead of spawning.
const CHILD: &str = "ZMAIL_FIRST_OPEN_RACE_CHILD";

/// Child processes, each one chance at the window.
const ROUNDS: usize = 12;

/// Threads in each child that open a store at the same moment.
const STORES: usize = 8;

/// Threads in each child that open plain SQLite connections meanwhile.
const CONTENDERS: usize = 8;

/// How long the contenders keep opening connections.
const CONTENTION: Duration = Duration::from_millis(50);

#[test]
fn stores_opened_at_once_as_the_first_use_of_sqlite_all_open() {
    if std::env::var_os(CHILD).is_some() {
        open_at_once();
        return;
    }
    let exe = std::env::current_exe().expect("the test binary's path");
    let failures: Vec<String> = (0..ROUNDS)
        .filter_map(|round| {
            let output = Command::new(&exe)
                .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .expect("the child test runs");
            (!output.status.success()).then(|| {
                format!(
                    "round {round}:\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {ROUNDS} rounds failed to open every store:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Opens [`STORES`] stores at once, the process's first SQLite use, while
/// [`CONTENDERS`] threads open plain in-memory connections.
fn open_at_once() {
    let start = Arc::new(Barrier::new(STORES + CONTENDERS));
    let contenders: Vec<_> = (0..CONTENDERS)
        .map(|_| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                let until = Instant::now() + CONTENTION;
                while Instant::now() < until {
                    drop(rusqlite::Connection::open_in_memory().expect("SQLite opens"));
                }
            })
        })
        .collect();
    let stores: Vec<_> = (0..STORES)
        .map(|_| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                LocalStore::open_ephemeral().map(drop)
            })
        })
        .collect();
    for contender in contenders {
        contender.join().expect("the contender finishes");
    }
    let errors: Vec<String> = stores
        .into_iter()
        .filter_map(|store| store.join().expect("the opener finishes").err())
        .map(|error| format!("{error:#}"))
        .collect();
    assert!(errors.is_empty(), "stores failed to open: {errors:#?}");
}
