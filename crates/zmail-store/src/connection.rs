//! Opening keyed SQLCipher connections, and keeping them per thread.
//!
//! Every store method takes a connection for its own work. Opening one
//! costs key setup and an empty page cache, so every page the next query
//! reads is decrypted and its MAC checked again (ADR 0005 §5). Each thread
//! therefore keeps the connections it has finished with, per store, and
//! the next call on that thread reuses one with its page cache still warm.
//! A connection is only kept when it is back in autocommit mode, so no
//! transaction outlives the call that began it. Each connection's page
//! cache is 16 MiB rather than SQLite's 2 MiB, so walking a 100k-thread
//! mailbox's indexes stays in decrypted pages.

use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    path::Path,
    sync::{Arc, Weak},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, ErrorCode};
use zmail_security::vault::StoreKey;

/// Connections a thread keeps per store: the UI thread nests at most one
/// read inside another, so two covers it.
const KEPT_PER_STORE: usize = 2;

/// Identifies one opened store across its clones. The cache holds it
/// weakly, so connections of a dropped store are closed on that thread's
/// next use of the cache.
pub(crate) type StoreToken = Arc<()>;

thread_local! {
    static KEPT: RefCell<Vec<(Weak<()>, Connection)>> = const { RefCell::new(Vec::new()) };
}

/// A connection to a store, returned to its thread's cache when dropped.
pub(crate) struct StoreConnection {
    conn: Option<Connection>,
    owner: Weak<()>,
}

impl StoreConnection {
    /// A kept connection for the store `token` identifies, or a new one
    /// opened at `path` with `key`.
    pub(crate) fn get(token: &StoreToken, path: &Path, key: &StoreKey) -> Result<Self> {
        let owner = Arc::downgrade(token);
        let kept = KEPT
            .try_with(|kept| {
                let mut kept = kept.borrow_mut();
                kept.retain(|(store, _)| store.strong_count() > 0);
                let index = kept
                    .iter()
                    .position(|(store, _)| Weak::ptr_eq(store, &owner))?;
                Some(kept.swap_remove(index).1)
            })
            .ok()
            .flatten();
        let conn = match kept {
            Some(conn) => conn,
            None => open_keyed(path, key)?,
        };
        Ok(Self {
            conn: Some(conn),
            owner,
        })
    }
}

impl Deref for StoreConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("the connection is present until drop")
    }
}

impl DerefMut for StoreConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn
            .as_mut()
            .expect("the connection is present until drop")
    }
}

impl Drop for StoreConnection {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if self.owner.strong_count() == 0 || !conn.is_autocommit() {
            return;
        }
        let owner = self.owner.clone();
        // During thread teardown the cache may be gone; the connection then
        // just closes.
        let _ = KEPT.try_with(move |kept| {
            let Ok(mut kept) = kept.try_borrow_mut() else {
                return;
            };
            let count = kept
                .iter()
                .filter(|(store, _)| Weak::ptr_eq(store, &owner))
                .count();
            if count < KEPT_PER_STORE {
                kept.push((owner, conn));
            }
        });
    }
}

/// Closes this thread's kept connections of stores that have been dropped,
/// so their files can be deleted (Windows refuses while one is open).
pub(crate) fn close_dropped_store_connections() {
    let _ = KEPT.try_with(|kept| {
        if let Ok(mut kept) = kept.try_borrow_mut() {
            kept.retain(|(store, _)| store.strong_count() > 0);
        }
    });
}

/// Opens a connection to the encrypted database at `path` with `key`, and
/// configures it. A wrong key, or a database that isn't encrypted, fails
/// here rather than at the first query.
pub(crate) fn open_keyed(path: &Path, key: &StoreKey) -> Result<Connection> {
    let conn =
        Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    apply_key(&conn, key)?;
    configure_connection(&conn).map_err(|error| {
        if is_not_a_database(&error) {
            error.context(format!(
                "{} can't be decrypted with the store key",
                path.display()
            ))
        } else {
            error
        }
    })?;
    Ok(conn)
}

/// Keys a connection: SQLCipher needs this before anything reads the file.
pub(crate) fn apply_key(conn: &Connection, key: &StoreKey) -> Result<()> {
    let statement = zeroize::Zeroizing::new(format!("PRAGMA key = {};", *key.sqlcipher_key()));
    conn.execute_batch(&statement)
        .context("failed to key the store connection")
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("failed to configure SQLite busy timeout")?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -16384;
        "#,
    )
    .context("failed to configure SQLite pragmas")
}

/// Whether `error` is SQLite's "file is not a database": a wrong key, or a
/// file that isn't encrypted (or isn't SQLite at all).
pub(crate) fn is_not_a_database(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code)
            == Some(ErrorCode::NotADatabase)
    })
}

#[cfg(test)]
mod tests {
    use crate::LocalStore;

    /// Whether this connection has the marker temp table: temp tables live
    /// in one connection only, so they show which connection a call got.
    fn has_marker(conn: &rusqlite::Connection) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM temp.sqlite_master WHERE name = 'marker'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("temp schema reads")
            == 1
    }

    #[test]
    fn a_thread_reuses_its_connection() {
        let store = LocalStore::open_ephemeral().expect("store opens");
        {
            let conn = store.connection().expect("connection");
            conn.execute_batch("CREATE TEMP TABLE marker (x)")
                .expect("marker");
        }
        let conn = store.connection().expect("connection");
        assert!(has_marker(&conn), "the kept connection comes back");
        // A nested call gets a second connection, not the one in use.
        let nested = store.connection().expect("connection");
        assert!(!has_marker(&nested));
    }

    #[test]
    fn a_connection_inside_a_transaction_is_not_kept() {
        let store = LocalStore::open_ephemeral().expect("store opens");
        {
            let conn = store.connection().expect("connection");
            conn.execute_batch("CREATE TEMP TABLE marker (x); BEGIN;")
                .expect("marker and an open transaction");
        }
        let conn = store.connection().expect("connection");
        assert!(!has_marker(&conn), "a connection mid-transaction is closed");
        assert!(conn.is_autocommit());
    }

    #[test]
    fn other_threads_and_other_stores_get_their_own_connections() {
        let store = LocalStore::open_ephemeral().expect("store opens");
        let other = LocalStore::open_ephemeral().expect("store opens");
        {
            let conn = store.connection().expect("connection");
            conn.execute_batch("CREATE TEMP TABLE marker (x)")
                .expect("marker");
        }
        assert!(!has_marker(&other.connection().expect("connection")));
        std::thread::scope(|scope| {
            scope
                .spawn(|| assert!(!has_marker(&store.connection().expect("connection"))))
                .join()
                .expect("thread");
        });
        assert!(has_marker(&store.connection().expect("connection")));
    }

    #[test]
    fn dropping_a_store_closes_its_kept_connections() {
        let store = LocalStore::open_ephemeral().expect("store opens");
        let directory = store.path().parent().expect("parent").to_owned();
        store.list_accounts().expect("reads");
        assert!(directory.exists());
        drop(store);
        assert!(!directory.exists(), "nothing held the files open");
    }
}
