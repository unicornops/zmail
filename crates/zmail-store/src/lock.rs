//! Advisory locks beside the database.
//!
//! - `<database>.lock`, so work that rewrites the store's files (encrypting
//!   a plaintext store, rotating its key) never runs while another process
//!   has the store open. Every open store holds a shared lock on it for as
//!   long as it lives; that work takes the lock exclusively, and fails at
//!   once rather than waiting when it can't.
//! - `<database>.sync.lock` ([`SyncLock`], ADR 0007): only the process
//!   holding it exclusively syncs, so a GUI and a terminal UI open on the
//!   same store don't both hold IDLE and push connections, poll twice or
//!   write the same queued changes back twice.
//!
//! The OS releases a lock when the process ends, so a crash never leaves
//! one held.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};

use crate::encryption::suffixed;

/// A held lock on a store; released when dropped.
#[derive(Debug)]
pub(crate) struct StoreLock {
    _file: fs::File,
}

impl StoreLock {
    /// A shared lock, as every open store holds.
    pub(crate) fn shared(database: &Path) -> Result<Self> {
        let file = open(&path(database))?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Self { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(anyhow!(
                "another Zmail is encrypting this mail store or changing its key; try again \
                 when it has finished"
            )),
            Err(fs::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("failed to lock {}", path(database).display()))
            }
        }
    }

    /// The exclusive lock for rewriting the store's files, or `None` when
    /// another store has it open (in this process or another).
    pub(crate) fn exclusive(database: &Path) -> Result<Option<Self>> {
        Ok(try_lock_exclusive(&path(database))?.map(|file| Self { _file: file }))
    }
}

/// The lock that lets one process sync the store (ADR 0007): an exclusive
/// lock on `<database>.sync.lock`, held for as long as this value lives and
/// released by the OS if the process ends first.
///
/// Every frontend that syncs in the background (the GUI, the terminal UI)
/// takes it before starting sync. A frontend that can't get it leaves
/// sync to the holder: it reads the store as usual, and its changes wait
/// in the store as queued work (pending operations, outbox jobs) that the
/// holder's sync picks up.
#[derive(Debug)]
pub struct SyncLock {
    _file: fs::File,
}

impl SyncLock {
    /// Takes the sync lock for the store at `database`, or `None` when
    /// another process (or another handle in this one) holds it. Never
    /// waits.
    pub fn try_acquire(database: &Path) -> Result<Option<Self>> {
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(try_lock_exclusive(&sync_path(database))?.map(|file| Self { _file: file }))
    }
}

/// `<database>.lock`.
fn path(database: &Path) -> PathBuf {
    suffixed(database, ".lock")
}

/// `<database>.sync.lock`.
fn sync_path(database: &Path) -> PathBuf {
    suffixed(database, ".sync.lock")
}

/// The lock file at `path`, locked exclusively, or `None` when someone
/// else holds a lock on it.
fn try_lock_exclusive(path: &Path) -> Result<Option<fs::File>> {
    let file = open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("failed to lock {}", path.display()))
        }
    }
}

fn open(path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{StoreLock, SyncLock};

    fn database(name: &str) -> (PathBuf, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("zmail-lock-{name}-{nanos}-{}", std::process::id()));
        (directory.join("store.sqlite3"), directory)
    }

    #[test]
    fn only_one_handle_holds_the_sync_lock_until_it_is_dropped() {
        let (path, directory) = database("sync");
        let first = SyncLock::try_acquire(&path)
            .expect("the lock file opens")
            .expect("the first holder gets the lock");
        // A second handle, as a second process would have, is refused at
        // once rather than waiting.
        assert!(
            SyncLock::try_acquire(&path)
                .expect("the lock file opens")
                .is_none(),
            "a second holder is refused"
        );
        drop(first);
        let again = SyncLock::try_acquire(&path)
            .expect("the lock file opens")
            .expect("the lock is free once its holder is gone");
        assert!(path.with_extension("sqlite3.sync.lock").exists());
        drop(again);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// Set in the child process `a_second_process_is_refused_the_sync_lock`
    /// starts: the database whose sync lock the child tries.
    const CHILD_DATABASE: &str = "ZMAIL_SYNC_LOCK_CHILD_DATABASE";

    #[test]
    fn a_second_process_is_refused_the_sync_lock() {
        let (path, directory) = database("process");
        let held = SyncLock::try_acquire(&path)
            .expect("the lock file opens")
            .expect("the lock is free");
        let child = |path: &std::path::Path| {
            std::process::Command::new(std::env::current_exe().expect("the test binary"))
                .args(["--exact", "lock::tests::sync_lock_child", "--nocapture"])
                .env(CHILD_DATABASE, path)
                .output()
                .expect("the child runs")
        };
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
        match SyncLock::try_acquire(std::path::Path::new(&path)).expect("the lock file opens") {
            Some(_) => println!("sync lock: acquired"),
            None => println!("sync lock: refused"),
        }
    }

    #[test]
    fn the_sync_lock_is_independent_of_the_store_lock() {
        let (path, directory) = database("independent");
        let sync = SyncLock::try_acquire(&path)
            .expect("the lock file opens")
            .expect("the sync lock is free");
        // Open stores (shared store locks) don't stop the sync lock, and the
        // sync lock doesn't stop a store opening or the exclusive store lock.
        let shared = StoreLock::shared(&path).expect("a store opens beside the sync lock");
        drop(shared);
        let exclusive = StoreLock::exclusive(&path)
            .expect("the lock file opens")
            .expect("no store is open");
        drop(exclusive);
        drop(sync);
        let _ = std::fs::remove_dir_all(directory);
    }
}
