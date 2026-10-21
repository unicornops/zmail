//! An advisory lock beside the database, so work that rewrites the store's
//! files (encrypting a plaintext store, rotating its key) never runs while
//! another process has the store open.
//!
//! Every open store holds a shared lock on `<database>.lock` for as long as
//! it lives; that work takes the lock exclusively, and fails at once
//! rather than waiting when it can't. The OS releases the lock when the
//! process ends, so a crash never leaves it held.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};

/// A held lock on a store; released when dropped.
#[derive(Debug)]
pub(crate) struct StoreLock {
    _file: fs::File,
}

impl StoreLock {
    /// A shared lock, as every open store holds.
    pub(crate) fn shared(database: &Path) -> Result<Self> {
        let file = open(database)?;
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
        let file = open(database)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("failed to lock {}", path(database).display()))
            }
        }
    }
}

/// `<database>.lock`.
fn path(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn open(database: &Path) -> Result<fs::File> {
    let path = path(database);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))
}
