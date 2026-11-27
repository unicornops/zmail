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
//!
//! No connection is opened before SQLCipher has finished setting itself up
//! ([`sqlcipher_ready`]), and no key reaches SQLCipher unless it is a full
//! raw key ([`key_literal`]).

use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    path::Path,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, ErrorCode};
use zeroize::Zeroizing;
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

/// Set once a probe has shown SQLCipher fully set up in this process.
static SQLCIPHER_READY: AtomicBool = AtomicBool::new(false);

/// Held while probing, so a process's first opens wait for one probe.
static SQLCIPHER_SETUP: Mutex<()> = Mutex::new(());

/// How long to wait for another thread's SQLCipher set-up to finish.
const SQLCIPHER_SETUP_WAIT: Duration = Duration::from_secs(5);

/// Waits until SQLCipher has finished setting itself up in this process.
///
/// SQLite marks itself initialised, and lets other threads through, before
/// it runs SQLCipher's set-up (`SQLITE_EXTRA_INIT`). A connection keyed in
/// between is refused with SQLCipher's catch-all message, "PRAGMA key
/// requires a key of one or more characters", whatever the key (#514). So
/// the first open in a process goes through here alone: when it is the
/// process's first SQLite use, the probe's own open runs the whole set-up
/// before it returns; when another thread is part-way through it, the probe
/// is retried until the set-up is done.
pub(crate) fn sqlcipher_ready() -> Result<()> {
    if SQLCIPHER_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    let _setup = SQLCIPHER_SETUP
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if SQLCIPHER_READY.load(Ordering::Acquire) {
        return Ok(());
    }
    let deadline = Instant::now() + SQLCIPHER_SETUP_WAIT;
    let mut pause = Duration::from_millis(1);
    loop {
        match probe_sqlcipher() {
            Ok(()) => {
                SQLCIPHER_READY.store(true, Ordering::Release);
                return Ok(());
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("SQLCipher did not finish setting itself up"));
            }
            Err(_) => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(50));
            }
        }
    }
}

/// Whether SQLCipher is set up: it keys a throwaway in-memory database,
/// which fails until the set-up has begun, and has `sqlcipher_export`, the
/// last thing the set-up adds (new connections get it from then on).
fn probe_sqlcipher() -> Result<()> {
    let conn = Connection::open_in_memory().context("failed to open SQLite")?;
    conn.execute_batch("PRAGMA key = 'zmail-sqlcipher-probe';")
        .context("SQLCipher refused a key")?;
    let exports: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_function_list WHERE name = 'sqlcipher_export')",
            [],
            |row| row.get(0),
        )
        .context("failed to list SQLite's functions")?;
    if !exports {
        bail!("SQLCipher has not added sqlcipher_export yet");
    }
    Ok(())
}

/// Why a store key was refused before it reached SQLCipher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnusableStoreKey {
    /// No key material at all. SQLCipher would refuse it for `PRAGMA key`,
    /// and `ATTACH … KEY` with an empty key attaches a plaintext database.
    #[error("the store key for SQLCipher is empty")]
    Empty,
    /// Not SQLCipher's raw-key form, `"x'…'"` around 64 hexadecimal digits:
    /// SQLCipher would take it as a passphrase instead.
    #[error("the store key for SQLCipher is not a 32-byte raw key")]
    NotARawKey,
}

/// Length of a raw key in hexadecimal digits: 32 bytes.
const RAW_KEY_HEX_DIGITS: usize = 64;

/// `key` as the value of `PRAGMA key` or `ATTACH … KEY`, refused unless it
/// is a full raw key.
pub(crate) fn key_literal(key: &StoreKey) -> Result<Zeroizing<String>, UnusableStoreKey> {
    checked_key_literal(key.sqlcipher_key())
}

/// `literal` when it is SQLCipher's raw-key form, `"x'…'"` around exactly
/// [`RAW_KEY_HEX_DIGITS`] hexadecimal digits.
fn checked_key_literal(literal: Zeroizing<String>) -> Result<Zeroizing<String>, UnusableStoreKey> {
    let digits = literal
        .strip_prefix("\"x'")
        .and_then(|rest| rest.strip_suffix("'\""));
    match digits {
        _ if literal.is_empty() => Err(UnusableStoreKey::Empty),
        Some("") => Err(UnusableStoreKey::Empty),
        Some(digits)
            if digits.len() == RAW_KEY_HEX_DIGITS
                && digits.bytes().all(|digit| digit.is_ascii_hexdigit()) =>
        {
            Ok(literal)
        }
        _ => Err(UnusableStoreKey::NotARawKey),
    }
}

/// Opens a connection to the encrypted database at `path` with `key`, and
/// configures it. A wrong key, or a database that isn't encrypted, fails
/// here rather than at the first query.
pub(crate) fn open_keyed(path: &Path, key: &StoreKey) -> Result<Connection> {
    sqlcipher_ready()?;
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
/// An empty or malformed key is refused here and never reaches SQLCipher.
pub(crate) fn apply_key(conn: &Connection, key: &StoreKey) -> Result<()> {
    let literal = key_literal(key).context("failed to key the store connection")?;
    let statement = Zeroizing::new(format!("PRAGMA key = {};", *literal));
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
    use rusqlite::Connection;
    use zeroize::Zeroizing;
    use zmail_security::vault::StoreKey;

    use super::{UnusableStoreKey, apply_key, checked_key_literal, key_literal, sqlcipher_ready};
    use crate::LocalStore;

    fn literal(value: &str) -> Result<Zeroizing<String>, UnusableStoreKey> {
        checked_key_literal(Zeroizing::new(value.to_owned()))
    }

    #[test]
    fn an_empty_key_never_reaches_sqlcipher() {
        assert_eq!(literal("").err(), Some(UnusableStoreKey::Empty));
        assert_eq!(literal("\"x''\"").err(), Some(UnusableStoreKey::Empty));
    }

    #[test]
    fn only_a_full_raw_key_reaches_sqlcipher() {
        let hex = "0123456789abcdefABCDEF0123456789".repeat(2);
        assert!(literal(&format!("\"x'{hex}'\"")).is_ok());
        for refused in [
            // A passphrase, short raw keys, a raw key with a salt, other quoting.
            "passphrase".to_owned(),
            "\"x'00'\"".to_owned(),
            format!("\"x'{}'\"", &hex[..62]),
            format!("\"x'{hex}{}'\"", &hex[..32]),
            format!("\"x'{}zz'\"", &hex[..62]),
            format!("x'{hex}'"),
            format!("'x'{hex}''"),
        ] {
            assert_eq!(
                literal(&refused).err(),
                Some(UnusableStoreKey::NotARawKey),
                "{refused}"
            );
        }
    }

    #[test]
    fn every_store_key_is_a_full_raw_key() {
        for _ in 0..64 {
            key_literal(&StoreKey::generate()).expect("a generated key is a raw key");
        }
    }

    #[test]
    fn a_refused_key_is_a_typed_error_naming_the_cause() {
        // As `apply_key` reports it.
        let error = anyhow::Error::from(literal("").expect_err("refused"))
            .context("failed to key the store connection");
        assert_eq!(
            error.downcast_ref::<UnusableStoreKey>(),
            Some(&UnusableStoreKey::Empty)
        );
        assert!(format!("{error:#}").contains("the store key for SQLCipher is empty"));
    }

    #[test]
    fn sqlcipher_is_set_up_before_a_connection_is_keyed() {
        sqlcipher_ready().expect("SQLCipher is set up");
        sqlcipher_ready().expect("and stays so");
        let conn = Connection::open_in_memory().expect("opens");
        apply_key(&conn, &StoreKey::generate()).expect("keys");
        let exports: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_function_list WHERE name = 'sqlcipher_export')",
                [],
                |row| row.get(0),
            )
            .expect("lists functions");
        assert!(exports);
    }

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
