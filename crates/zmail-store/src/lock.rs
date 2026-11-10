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
use zmail_security::vault::private;

use crate::encryption::suffixed;

/// A held lock on a store; released when dropped.
#[derive(Debug)]
pub(crate) struct StoreLock {
    file: fs::File,
}

impl StoreLock {
    /// A shared lock, as every open store holds.
    pub(crate) fn shared(database: &Path) -> Result<Self> {
        let file = open(&path(database))?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => Err(rewriting()),
            Err(fs::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("failed to lock {}", path(database).display()))
            }
        }
    }

    /// The exclusive lock for rewriting the store's files, or `None` when
    /// another store has it open (in this process or another).
    pub(crate) fn exclusive(database: &Path) -> Result<Option<Self>> {
        Ok(try_lock_exclusive(&path(database))?.map(|file| Self { file }))
    }

    /// Turns an exclusive lock into the shared lock an open store holds,
    /// on the same open file rather than by dropping it and locking the
    /// file again.
    ///
    /// A lock belongs to the open file, and a process this one starts (a
    /// browser opening a link, any subprocess) gets copies of all its
    /// descriptors until it runs its program. Dropping the lock while
    /// another thread is starting one leaves that copy holding it for a
    /// moment, and the shared lock taken next would be refused as if
    /// another Zmail were rewriting the store. Unlocking here releases the
    /// lock however many copies of the descriptor there are.
    ///
    /// Like dropping and locking again, this is not atomic: another
    /// process can take the exclusive lock in between, and then the store
    /// does not open.
    pub(crate) fn downgrade(&self) -> Result<()> {
        self.file
            .unlock()
            .context("failed to release the store's exclusive lock")?;
        match self.file.try_lock_shared() {
            Ok(()) => Ok(()),
            Err(fs::TryLockError::WouldBlock) => Err(rewriting()),
            Err(fs::TryLockError::Error(error)) => {
                Err(error).context("failed to share the store's lock")
            }
        }
    }
}

/// Why a shared lock was refused: another process holds it exclusively.
fn rewriting() -> anyhow::Error {
    anyhow!(
        "another Zmail is encrypting this mail store or changing its key; try again when it \
         has finished"
    )
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
            private::create_private_dir_all(parent)
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

/// Opens the lock file at `path`, creating it for its owner alone: anyone
/// who can open a lock file can hold its lock, and keep Zmail from opening
/// the store or from syncing.
fn open(path: &Path) -> Result<fs::File> {
    let file = private::private_file_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    // One made before lock files were private.
    let _ = private::make_file_private(&file);
    Ok(file)
}

// No test in this crate's library may start a process: the child gets
// copies of every descriptor until it runs its program, so it keeps locks
// other tests have just dropped (#345). Tests that need a second process
// have a test binary of their own (`tests/sync_lock_process.rs`).
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

    #[test]
    fn a_downgraded_lock_lets_other_stores_open_but_not_rewrite() {
        let (path, directory) = database("downgrade");
        std::fs::create_dir_all(&directory).expect("temp dir");
        let lock = StoreLock::exclusive(&path)
            .expect("the lock file opens")
            .expect("no store is open");
        assert!(
            StoreLock::shared(&path).is_err(),
            "no store opens while the lock is exclusive"
        );
        lock.downgrade().expect("the lock becomes shared");
        let other = StoreLock::shared(&path).expect("another store opens beside it");
        assert!(
            StoreLock::exclusive(&path)
                .expect("the lock file opens")
                .is_none(),
            "nothing rewrites the store while it is open"
        );
        drop(other);
        drop(lock);
        assert!(
            StoreLock::exclusive(&path)
                .expect("the lock file opens")
                .is_some(),
            "the lock is free once both are gone"
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
