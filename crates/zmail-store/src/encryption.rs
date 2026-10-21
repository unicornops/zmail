//! Opening the encrypted store, and encrypting a plaintext one
//! (ADR 0005 §3 and §4).
//!
//! The database's first bytes say what it is: SQLite's `SQLite format 3\0`
//! header for a plaintext store, random bytes (SQLCipher's salt) for an
//! encrypted one. That header, not the key record, decides whether a
//! migration is needed, so an interrupted migration is always picked up
//! where the files say it stopped.
//!
//! Encrypting is a copy: `sqlcipher_export` writes an encrypted
//! `<store>.encrypting` beside the plaintext file, the copy is checked
//! against the original, and one rename puts it in place. Until that
//! rename the plaintext store is untouched; after it, the encrypted one
//! is complete. There is no moment when neither is readable.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags};
use zmail_security::vault::{self, KeyId, StoreKey, StoreKeyError, StoreKeyRecord, StoreKeychain};

use crate::{LocalStore, connection};

/// SQLite's plaintext file header.
const PLAINTEXT_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// How often the export's progress is sampled.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// What the database file at a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFileState {
    /// No database yet (or an empty file, which SQLite treats the same).
    Missing,
    /// A plaintext SQLite database: it needs encrypting.
    Plaintext,
    /// Anything else: an encrypted SQLCipher database.
    Encrypted,
}

/// Reads the first bytes of the database at `path` to tell what it is.
pub fn store_file_state(path: &Path) -> Result<StoreFileState> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StoreFileState::Missing);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    let mut header = [0; 16];
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        }
    }
    Ok(match read {
        0 => StoreFileState::Missing,
        _ if header == *PLAINTEXT_HEADER => StoreFileState::Plaintext,
        _ => StoreFileState::Encrypted,
    })
}

/// Progress of encrypting a plaintext store, for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationProgress {
    /// Copying the database: `written` of about `total` bytes.
    Database {
        /// Bytes of the encrypted copy written so far.
        written: u64,
        /// The plaintext database's size; the copy ends up about this big.
        total: u64,
    },
    /// Checking the encrypted copy against the original.
    Verifying,
    /// Done.
    Finished,
}

/// What a migration did, for the message after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Backups made before encryption (`zmail-app backup`), which are still
    /// plaintext. Zmail leaves them alone; the user decides.
    pub plaintext_backups: Vec<PathBuf>,
}

/// Why the store couldn't be opened. The app shows each differently.
#[derive(Debug, thiserror::Error)]
pub enum StoreOpenError {
    /// The store is encrypted and its key is gone (a reset keychain, or a
    /// data directory copied from another machine). Nobody can read it;
    /// starting over means a resync, and local-only mail is lost.
    #[error(
        "this mail store is encrypted and its key isn't in the keychain{}; it can't be read",
        .key_id.map(|id| format!(" (key {id})")).unwrap_or_default()
    )]
    Unreadable {
        /// The key the record names, when there is a record.
        key_id: Option<KeyId>,
    },
    /// The keychain couldn't be reached or refused. Retrying may work;
    /// Zmail never opens the store without encryption instead.
    #[error("the keychain is not available: {0:#}")]
    Keychain(anyhow::Error),
    /// Anything else: a full disk, an unreadable file, a failed check.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<StoreKeyError> for StoreOpenError {
    fn from(error: StoreKeyError) -> Self {
        match error {
            StoreKeyError::Missing { key_id } => Self::Unreadable {
                key_id: Some(key_id),
            },
            StoreKeyError::Keychain(error) => Self::Keychain(error),
            error @ StoreKeyError::Record(_) => Self::Other(error.into()),
        }
    }
}

impl LocalStore {
    /// Opens the store at `path` with its key from `keychain`: creates the
    /// key for a new store, encrypts a plaintext store first (reporting
    /// through `progress`), and opens an encrypted one. The report is
    /// `Some` when this call encrypted the store.
    pub fn unlock(
        path: impl AsRef<Path>,
        keychain: &dyn StoreKeychain,
        progress: &(dyn Fn(MigrationProgress) + Sync),
    ) -> Result<(Self, Option<MigrationReport>), StoreOpenError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let record = StoreKeyRecord::path_for(path);
        let state = store_file_state(path)?;
        let key = match vault::load_store_key(keychain, &record)? {
            Some(key) => key,
            // An encrypted store whose record is gone: the key can't be
            // found, and a new one would not open it.
            None if state == StoreFileState::Encrypted => {
                return Err(StoreOpenError::Unreadable { key_id: None });
            }
            None => vault::create_store_key(keychain, &record)?,
        };
        let report = match state {
            StoreFileState::Plaintext => Some(encrypt_plaintext_store(path, &key, progress)?),
            StoreFileState::Missing | StoreFileState::Encrypted => None,
        };
        let store = Self::open(path, key)?;
        Ok((store, report))
    }
}

/// Encrypts the plaintext store at `path` in place, by copy (see the
/// module docs), and returns what the user should know afterwards.
pub fn encrypt_plaintext_store(
    path: &Path,
    key: &StoreKey,
    progress: &(dyn Fn(MigrationProgress) + Sync),
) -> Result<MigrationReport> {
    encrypt_plaintext_store_until(path, key, progress, None)
}

/// The steps of a migration, so tests can stop one at each and check that
/// the next open recovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStep {
    /// After the encrypted copy is written, before it is checked.
    Exported,
    /// After the copy is checked and synced, before the rename.
    Verified,
    /// After the rename, before the plaintext `-wal`/`-shm` are removed.
    Renamed,
}

pub(crate) fn encrypt_plaintext_store_until(
    path: &Path,
    key: &StoreKey,
    progress: &(dyn Fn(MigrationProgress) + Sync),
    stop_after: Option<MigrationStep>,
) -> Result<MigrationReport> {
    let stop = |step| -> Result<()> {
        if stop_after == Some(step) {
            bail!("migration stopped after {step:?} (test)");
        }
        Ok(())
    };
    if store_file_state(path)? != StoreFileState::Plaintext {
        bail!("{} is not a plaintext store", path.display());
    }
    let encrypting = suffixed(path, ".encrypting");
    remove_with_journals(&encrypting)?;

    // `ATTACH` inherits these flags, and it has to create the copy.
    let source = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {}", path.display()))?;
    source
        .busy_timeout(Duration::from_secs(5))
        .context("failed to configure SQLite busy timeout")?;
    // Everything in the WAL goes into the main file first, so the copy and
    // its size estimate see all of it.
    source
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .context("failed to checkpoint the plaintext store")?;
    let total = fs::metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let user_version: i64 = source
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("failed to read the store's user version")?;

    let target = encrypting
        .to_str()
        .context("the store path must be valid UTF-8")?
        .replace('\'', "''");
    let attach = zeroize::Zeroizing::new(format!(
        "ATTACH DATABASE '{target}' AS encrypted KEY {};",
        *key.sqlcipher_key()
    ));
    source
        .execute_batch(&attach)
        .context("failed to create the encrypted copy")?;
    let exported = export_with_progress(&source, &encrypting, total, progress);
    source
        .execute_batch("DETACH DATABASE encrypted;")
        .context("failed to close the encrypted copy")?;
    exported?;
    stop(MigrationStep::Exported)?;

    progress(MigrationProgress::Verifying);
    {
        let copy = connection::open_keyed(&encrypting, key)?;
        copy.pragma_update(None, "user_version", user_version)
            .context("failed to copy the store's user version")?;
        verify_copy(&source, &copy)?;
        // Nothing may be left in the copy's own WAL when it is renamed into
        // place.
        copy.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .context("failed to checkpoint the encrypted copy")?;
    }
    sync_file(&encrypting)?;
    stop(MigrationStep::Verified)?;

    drop(source);
    fs::rename(&encrypting, path).with_context(|| {
        format!(
            "failed to move the encrypted store into place at {}",
            path.display()
        )
    })?;
    vault::sync_parent_dir(path);
    stop(MigrationStep::Renamed)?;

    // The plaintext store's WAL and index are empty after the checkpoint;
    // remove them so no SQLite connection mistakes them for the new file's.
    remove_journals(path)?;
    remove_with_journals(&encrypting)?;

    progress(MigrationProgress::Finished);
    Ok(MigrationReport {
        plaintext_backups: plaintext_backups(path),
    })
}

/// Runs `sqlcipher_export` into the attached copy, reporting the copy's
/// size from another thread while it runs.
fn export_with_progress(
    source: &Connection,
    encrypting: &Path,
    total: u64,
    progress: &(dyn Fn(MigrationProgress) + Sync),
) -> Result<()> {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                let written = fs::metadata(encrypting).map(|m| m.len()).unwrap_or(0);
                progress(MigrationProgress::Database {
                    written: written.min(total),
                    total,
                });
                std::thread::sleep(PROGRESS_INTERVAL);
            }
        });
        let result = source
            .query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))
            .context("failed to write the encrypted copy of the store");
        done.store(true, Ordering::Relaxed);
        result
    })?;
    progress(MigrationProgress::Database {
        written: total,
        total,
    });
    Ok(())
}

/// Checks the encrypted copy: SQLCipher's page MACs, SQLite's structure,
/// and every table's row count against the original.
fn verify_copy(source: &Connection, copy: &Connection) -> Result<()> {
    let mut statement = copy
        .prepare("PRAGMA cipher_integrity_check")
        .context("failed to check the encrypted copy")?;
    let problems: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .context("failed to check the encrypted copy")?
        .collect::<rusqlite::Result<_>>()
        .context("failed to check the encrypted copy")?;
    if !problems.is_empty() {
        bail!(
            "the encrypted copy failed its integrity check: {}",
            problems.join("; ")
        );
    }
    let quick: String = copy
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .context("failed to check the encrypted copy")?;
    if quick != "ok" {
        bail!("the encrypted copy failed its structure check: {quick}");
    }

    let tables: Vec<String> = {
        let mut statement = source.prepare(
            "SELECT name FROM main.sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             AND sql NOT LIKE 'CREATE VIRTUAL TABLE%' ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for table in tables {
        let count_sql = format!("SELECT COUNT(*) FROM \"{}\"", table.replace('"', "\"\""));
        let original: i64 = source.query_row(&count_sql, [], |row| row.get(0))?;
        let copied: i64 = copy
            .query_row(&count_sql, [], |row| row.get(0))
            .with_context(|| format!("the encrypted copy has no table {table}"))?;
        if original != copied {
            bail!("the encrypted copy of {table} has {copied} rows, the original {original}");
        }
    }
    Ok(())
}

/// Plaintext backups in the store's default backup directory.
fn plaintext_backups(path: &Path) -> Vec<PathBuf> {
    let Some(directory) = path.parent().map(|parent| parent.join("backups")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut backups: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqlite3")
        })
        .filter(|path| store_file_state(path).ok() == Some(StoreFileState::Plaintext))
        .collect();
    backups.sort();
    backups
}

/// Moves an unreadable store (its database, journals, key record and
/// attachment files) into `unreadable-<time>` beside it, so a new store can
/// start there and the old files stay recoverable if the key turns up.
/// Returns the directory the files went to.
pub fn move_unreadable_store_aside(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("the store path has no parent directory")?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let aside = parent.join(format!("unreadable-{nanos}"));
    fs::create_dir_all(&aside).with_context(|| format!("failed to create {}", aside.display()))?;
    let mut moving = vec![
        path.to_owned(),
        suffixed(path, "-wal"),
        suffixed(path, "-shm"),
        StoreKeyRecord::path_for(path),
        parent.join("attachments"),
    ];
    moving.retain(|path| path.exists());
    for from in moving {
        let name = from
            .file_name()
            .ok_or_else(|| anyhow!("{} has no file name", from.display()))?;
        let to = aside.join(name);
        fs::rename(&from, &to)
            .with_context(|| format!("failed to move {} to {}", from.display(), to.display()))?;
    }
    Ok(aside)
}

/// The database's companion file `<path><suffix>`.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes `<path>-wal`, `<path>-shm` and `<path>-journal` if present.
fn remove_journals(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let journal = suffixed(path, suffix);
        match fs::remove_file(&journal) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove {}", journal.display()));
            }
        }
    }
    Ok(())
}

/// Removes `path` and its journals: a partial copy from an interrupted run.
fn remove_with_journals(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove {}", path.display()));
        }
    }
    remove_journals(path)
}

fn sync_file(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("failed to sync {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use zmail_core::{
        AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ProtocolKind, ThreadSnapshot,
    };
    use zmail_security::InMemorySecretStore;

    use super::*;

    /// Text that appears in the seeded message and nowhere else.
    const SECRET_PHRASE: &str = "zebracorn-lighthouse-quartermaster";

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "zmail-encryption-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    fn no_progress(_: MigrationProgress) {}

    /// Seeds one account with one message carrying [`SECRET_PHRASE`].
    fn seed(store: &LocalStore) -> i64 {
        let account = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account");
        let message = MessageSnapshot {
            remote_id: "secret-1".to_owned(),
            identity_email: "me@example.com".to_owned(),
            sender: "Alice <alice@example.com>".to_owned(),
            reply_to: None,
            recipients: "me@example.com".to_owned(),
            subject: "Quarterly plans".to_owned(),
            body_preview: "The plan".to_owned(),
            body_text: Some(format!("The plan is {SECRET_PHRASE}.")),
            sanitized_html: None,
            has_remote_content: false,
            is_read: false,
            is_flagged: false,
            received_at: "2026-10-01T09:00:00Z".to_owned(),
            attachments: Vec::new(),
        };
        let snapshot = AccountSyncSnapshot {
            checkpoint: "seed".to_owned(),
            detail: "seed".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "secret-thread".to_owned(),
                    subject: "Quarterly plans".to_owned(),
                    participants: "Alice -> me@example.com".to_owned(),
                    preview: "The plan".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-10-01T09:00:00Z".to_owned(),
                    messages: vec![message],
                }],
                ..Default::default()
            }],
        };
        store
            .replace_account_snapshot(account, &snapshot)
            .expect("snapshot applies");
        account
    }

    fn finds_secret(store: &LocalStore, account: i64) -> bool {
        store
            .search_threads(account, SECRET_PHRASE, 10)
            .expect("search runs")
            .len()
            == 1
    }

    /// Writes a plaintext copy of `store` to `target`, as a store from
    /// before encryption would be.
    fn plaintext_copy(store: &LocalStore, target: &Path) {
        let conn = connection::open_keyed(store.path(), store.store_key()).expect("opens");
        conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS plain KEY '';",
            target.display()
        ))
        .expect("attach");
        conn.query_row("SELECT sqlcipher_export('plain')", [], |_| Ok(()))
            .expect("export");
        conn.execute_batch("DETACH DATABASE plain;")
            .expect("detach");
        assert_eq!(
            store_file_state(target).expect("state"),
            StoreFileState::Plaintext
        );
    }

    /// Every file under `directory`, as raw bytes, contains `needle`.
    fn any_file_contains(directory: &Path, needle: &[u8]) -> Option<PathBuf> {
        for entry in fs::read_dir(directory).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = any_file_contains(&path, needle) {
                    return Some(found);
                }
            } else if fs::read(&path)
                .expect("readable")
                .windows(needle.len())
                .any(|window| window == needle)
            {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn the_file_header_tells_plaintext_from_encrypted() {
        let directory = temp_dir("state");
        let path = directory.join("zmail.sqlite3");
        assert_eq!(
            store_file_state(&path).expect("state"),
            StoreFileState::Missing
        );
        fs::write(&path, b"").expect("written");
        assert_eq!(
            store_file_state(&path).expect("state"),
            StoreFileState::Missing
        );
        fs::remove_file(&path).expect("removed");

        let store = LocalStore::open(&path, StoreKey::generate()).expect("opens");
        seed(&store);
        assert_eq!(
            store_file_state(&path).expect("state"),
            StoreFileState::Encrypted
        );
        let plain = directory.join("plain.sqlite3");
        plaintext_copy(&store, &plain);
        drop(store);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn the_store_opens_only_with_its_key() {
        let directory = temp_dir("key");
        let path = directory.join("zmail.sqlite3");
        let key = StoreKey::generate();
        let store = LocalStore::open(&path, key.clone()).expect("opens");
        let account = seed(&store);
        drop(store);

        let wrong = LocalStore::open(&path, StoreKey::generate()).expect_err("wrong key fails");
        assert!(
            format!("{wrong:#}").contains("can't be decrypted"),
            "{wrong:#}"
        );
        let unkeyed = Connection::open(&path).expect("SQLite opens any file");
        assert!(
            unkeyed
                .query_row("SELECT COUNT(*) FROM accounts", [], |row| row
                    .get::<_, i64>(0))
                .is_err(),
            "no key, no data"
        );
        drop(unkeyed);

        let store = LocalStore::open(&path, key).expect("the right key opens it");
        assert!(finds_secret(&store, account));
        drop(store);
        assert_eq!(
            any_file_contains(&directory, SECRET_PHRASE.as_bytes()),
            None,
            "the message text is nowhere on disk in plaintext"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_plaintext_store_is_refused_until_it_is_encrypted() {
        let directory = temp_dir("refuse");
        let source = LocalStore::open_ephemeral().expect("opens");
        seed(&source);
        let path = directory.join("zmail.sqlite3");
        plaintext_copy(&source, &path);

        let error = LocalStore::open(&path, StoreKey::generate()).expect_err("refused");
        assert!(
            format!("{error:#}").contains("not encrypted yet"),
            "{error:#}"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn unlock_creates_a_key_for_a_new_store_and_finds_it_again() {
        let directory = temp_dir("unlock");
        let path = directory.join("zmail.sqlite3");
        let keychain = InMemorySecretStore::default();

        let (store, report) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");
        assert_eq!(report, None, "a new store needs no migration");
        let account = seed(&store);
        let key_id = store.store_key().id();
        drop(store);
        assert!(StoreKeyRecord::path_for(&path).exists());

        let (store, report) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");
        assert_eq!(report, None);
        assert_eq!(store.store_key().id(), key_id);
        assert!(finds_secret(&store, account));
        drop(store);

        // The keychain lost the key: the store is unreadable, not replaced.
        keychain.delete_store_key(key_id).expect("deleted");
        match LocalStore::unlock(&path, &keychain, &no_progress) {
            Err(StoreOpenError::Unreadable { key_id: Some(id) }) => assert_eq!(id, key_id),
            other => panic!("expected an unreadable store, got {other:?}"),
        }
        // So did the record: still unreadable, and no new key is made for it.
        fs::remove_file(StoreKeyRecord::path_for(&path)).expect("removed");
        assert!(matches!(
            LocalStore::unlock(&path, &keychain, &no_progress),
            Err(StoreOpenError::Unreadable { key_id: None })
        ));
        assert!(!StoreKeyRecord::path_for(&path).exists());

        // Starting over moves the old files aside and opens a fresh store.
        let aside = move_unreadable_store_aside(&path).expect("moved aside");
        assert!(aside.join("zmail.sqlite3").exists());
        assert!(!path.exists());
        let (fresh, _) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");
        assert!(fresh.needs_onboarding().expect("reads"));
        drop(fresh);
        let _ = fs::remove_dir_all(directory);
    }

    /// A keychain that can't be reached.
    struct LockedKeychain;

    impl StoreKeychain for LockedKeychain {
        fn save_store_key(&self, _: &StoreKey) -> Result<()> {
            bail!("the keychain is locked")
        }
        fn load_store_key(&self, _: KeyId) -> Result<Option<StoreKey>> {
            bail!("the keychain is locked")
        }
        fn delete_store_key(&self, _: KeyId) -> Result<()> {
            bail!("the keychain is locked")
        }
    }

    #[test]
    fn a_locked_keychain_blocks_and_never_falls_back_to_plaintext() {
        let directory = temp_dir("locked");
        let path = directory.join("zmail.sqlite3");
        let source = LocalStore::open_ephemeral().expect("opens");
        seed(&source);
        plaintext_copy(&source, &path);

        assert!(matches!(
            LocalStore::unlock(&path, &LockedKeychain, &no_progress),
            Err(StoreOpenError::Keychain(_))
        ));
        assert_eq!(
            store_file_state(&path).expect("state"),
            StoreFileState::Plaintext,
            "the store is left as it was"
        );
        assert!(!StoreKeyRecord::path_for(&path).exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_plaintext_store_is_encrypted_with_its_data_intact() {
        let directory = temp_dir("migrate");
        let path = directory.join("zmail.sqlite3");
        let source = LocalStore::open_ephemeral().expect("opens");
        let account = seed(&source);
        plaintext_copy(&source, &path);
        let backups = directory.join("backups");
        fs::create_dir_all(&backups).expect("backups dir");
        let old_backup = backups.join("zmail-backup-1.sqlite3");
        plaintext_copy(&source, &old_backup);
        drop(source);
        assert!(any_file_contains(&directory, SECRET_PHRASE.as_bytes()).is_some());

        let seen = Mutex::new(Vec::new());
        let keychain = InMemorySecretStore::default();
        let (store, report) = LocalStore::unlock(&path, &keychain, &|step| {
            seen.lock().expect("lock").push(step);
        })
        .expect("unlocks");

        let report = report.expect("the store was migrated");
        assert_eq!(report.plaintext_backups, vec![old_backup.clone()]);
        assert_eq!(
            store_file_state(&path).expect("state"),
            StoreFileState::Encrypted
        );
        assert!(
            finds_secret(&store, account),
            "search survives the migration"
        );
        assert_eq!(store.list_accounts().expect("accounts").len(), 1);
        let seen = seen.into_inner().expect("lock");
        assert!(matches!(
            seen.first(),
            Some(MigrationProgress::Database { .. })
        ));
        assert!(seen.contains(&MigrationProgress::Verifying));
        assert_eq!(seen.last(), Some(&MigrationProgress::Finished));
        drop(store);

        // Only the old backup, which the user owns, still holds plaintext.
        fs::remove_file(&old_backup).expect("removed");
        assert_eq!(
            any_file_contains(&directory, SECRET_PHRASE.as_bytes()),
            None
        );
        assert!(!suffixed(&path, ".encrypting").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_migration_interrupted_at_any_step_recovers_on_the_next_open() {
        for step in [
            MigrationStep::Exported,
            MigrationStep::Verified,
            MigrationStep::Renamed,
        ] {
            let directory = temp_dir("crash");
            let path = directory.join("zmail.sqlite3");
            let source = LocalStore::open_ephemeral().expect("opens");
            let account = seed(&source);
            plaintext_copy(&source, &path);
            drop(source);

            let keychain = InMemorySecretStore::default();
            let key =
                vault::create_store_key(&keychain, &StoreKeyRecord::path_for(&path)).expect("key");
            let error = encrypt_plaintext_store_until(&path, &key, &no_progress, Some(step))
                .expect_err("stopped");
            assert!(format!("{error:#}").contains("stopped"), "{error:#}");
            let expected = if step == MigrationStep::Renamed {
                StoreFileState::Encrypted
            } else {
                StoreFileState::Plaintext
            };
            assert_eq!(
                store_file_state(&path).expect("state"),
                expected,
                "{step:?}"
            );

            let (store, report) =
                LocalStore::unlock(&path, &keychain, &no_progress).expect("recovers");
            assert_eq!(report.is_some(), step != MigrationStep::Renamed, "{step:?}");
            assert!(finds_secret(&store, account), "{step:?}");
            drop(store);
            assert!(!suffixed(&path, ".encrypting").exists(), "{step:?}");
            assert_eq!(
                any_file_contains(&directory, SECRET_PHRASE.as_bytes()),
                None,
                "{step:?}"
            );
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn a_leftover_partial_copy_is_replaced() {
        let directory = temp_dir("partial");
        let path = directory.join("zmail.sqlite3");
        let source = LocalStore::open_ephemeral().expect("opens");
        let account = seed(&source);
        plaintext_copy(&source, &path);
        drop(source);
        fs::write(suffixed(&path, ".encrypting"), b"half a database").expect("written");

        let keychain = InMemorySecretStore::default();
        let (store, report) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");
        assert!(report.is_some());
        assert!(finds_secret(&store, account));
        drop(store);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn backups_are_encrypted_with_the_store_key() {
        let store = LocalStore::open_ephemeral().expect("opens");
        let account = seed(&store);
        let directory = temp_dir("backup");
        let backup = store.create_backup(&directory, 2).expect("backup");
        assert_eq!(
            store_file_state(&backup).expect("state"),
            StoreFileState::Encrypted
        );
        assert_eq!(
            any_file_contains(&directory, SECRET_PHRASE.as_bytes()),
            None
        );
        let restored = LocalStore::open(&backup, store.store_key().clone()).expect("opens");
        assert!(finds_secret(&restored, account));
        drop(restored);
        let _ = fs::remove_dir_all(directory);
    }
}
