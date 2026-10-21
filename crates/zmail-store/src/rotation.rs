//! Rotating the store key (ADR 0005 §3): every byte the store keeps is
//! re-encrypted under a new data key, and the old key is deleted.
//!
//! The key record names the new key as pending for the whole rotation, and
//! the database itself says how far it got: it opens with exactly one of
//! the two keys. The steps:
//!
//! 1. A new key goes into the keychain (read back), and the record names it
//!    as pending.
//! 2. `sqlcipher_export` copies the database to `<store>.rekeying` under the
//!    new key, and the copy is checked against the original.
//! 3. Every attachment file is re-encrypted under the new key and a new
//!    opaque name, and the copy's rows point at the new names. The old
//!    files stay.
//! 4. The copy is renamed over the database.
//! 5. The record names the new key alone; the old key's files and its
//!    keychain entry are deleted.
//!
//! A crash before step 4 leaves a database that opens with the old key: the
//! next start undoes the rotation (the copy, the new key's files and the
//! new key go). A crash after it leaves one that opens with the new key:
//! the next start finishes step 5.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use zmail_security::vault::{
    self, BlobVault, KeyId, StoreKey, StoreKeyRecord, StoreKeychain, file_key_id,
};

use crate::encryption::{
    export_with_progress, remove_journals, remove_with_journals, store_file_state, suffixed,
    sync_file, verify_copy,
};
use crate::{
    LocalStore, MigrationProgress, StoreFileState, StoreOpenError, attachment_storage_dir,
    connection, lock::StoreLock,
};

/// The steps of a rotation, so tests can stop one at each and check that
/// the next open recovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RotationStep {
    /// After the database copy is written and checked.
    Exported,
    /// After the attachment files are re-encrypted.
    Reencrypted,
    /// After the copy replaced the database, before the old key goes.
    Renamed,
}

impl LocalStore {
    /// Re-encrypts the closed store at `path` under a new key from
    /// `keychain` and deletes the old key, reporting progress like a
    /// migration. Returns the new key's id.
    ///
    /// The store must not be open anywhere, in this process or another:
    /// rotation rewrites its files, and refuses to start otherwise.
    pub fn rotate_key(
        path: impl AsRef<Path>,
        keychain: &dyn StoreKeychain,
        progress: &(dyn Fn(MigrationProgress) + Sync),
    ) -> Result<KeyId, StoreOpenError> {
        rotate_key_until(path.as_ref(), keychain, progress, None)
    }
}

pub(crate) fn rotate_key_until(
    path: &Path,
    keychain: &dyn StoreKeychain,
    progress: &(dyn Fn(MigrationProgress) + Sync),
    stop_after: Option<RotationStep>,
) -> Result<KeyId, StoreOpenError> {
    let Some(lock) = StoreLock::exclusive(path)? else {
        return Err(StoreOpenError::Other(anyhow::anyhow!(
            "the mail store is open; quit Zmail (and any zmail-app command) before changing \
             its key"
        )));
    };
    let lock = Arc::new(lock);
    resolve_interrupted(path, keychain)?;
    if store_file_state(path)? != StoreFileState::Encrypted {
        return Err(StoreOpenError::Other(anyhow::anyhow!(
            "there is no encrypted mail store at {}",
            path.display()
        )));
    }
    let record_path = StoreKeyRecord::path_for(path);
    let record =
        StoreKeyRecord::read(&record_path)?.ok_or(StoreOpenError::Unreadable { key_id: None })?;
    let old_key = vault::load_key(keychain, record.key_id)?;

    let new_key = vault::stash_new_key(keychain)?;
    StoreKeyRecord {
        key_id: old_key.id(),
        pending_key_id: Some(new_key.id()),
    }
    .write(&record_path)?;

    // The lock is held until the old key is gone, so nothing opens the
    // store half way.
    rekey_files(path, &old_key, &new_key, &lock, progress, stop_after)?;
    if stop_after == Some(RotationStep::Renamed) {
        return Err(StoreOpenError::Other(anyhow::anyhow!(
            "rotation stopped after {:?} (test)",
            RotationStep::Renamed
        )));
    }
    finish(path, keychain, old_key.id(), new_key.id())?;
    drop(lock);
    progress(MigrationProgress::Finished);
    Ok(new_key.id())
}

/// Steps 2 to 4: the database copy, the attachment files, the swap.
fn rekey_files(
    path: &Path,
    old_key: &StoreKey,
    new_key: &StoreKey,
    lock: &Arc<StoreLock>,
    progress: &(dyn Fn(MigrationProgress) + Sync),
    stop_after: Option<RotationStep>,
) -> Result<()> {
    let stop = |step| -> Result<()> {
        if stop_after == Some(step) {
            bail!("rotation stopped after {step:?} (test)");
        }
        Ok(())
    };
    let rekeying = suffixed(path, ".rekeying");
    remove_with_journals(&rekeying)?;
    // Applies any pending schema migration first, so the copy is current.
    let store = LocalStore::open_with_lock(path.to_owned(), old_key.clone(), lock.clone())?;
    let old_vault = store.vault.clone();
    let source = connection::open_keyed(path, old_key)?;
    source
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .context("failed to checkpoint the store")?;
    let total = fs::metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let user_version: i64 = source
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("failed to read the store's user version")?;
    let target = rekeying
        .to_str()
        .context("the store path must be valid UTF-8")?
        .replace('\'', "''");
    let attach = zeroize::Zeroizing::new(format!(
        "ATTACH DATABASE '{target}' AS rekeyed KEY {};",
        *new_key.sqlcipher_key()
    ));
    source
        .execute_batch(&attach)
        .context("failed to create the re-encrypted copy")?;
    let exported = export_with_progress(&source, "rekeyed", &rekeying, total, progress);
    source
        .execute_batch("DETACH DATABASE rekeyed;")
        .context("failed to close the re-encrypted copy")?;
    exported?;

    progress(MigrationProgress::Verifying);
    let copy = connection::open_keyed(&rekeying, new_key)?;
    copy.pragma_update(None, "user_version", user_version)
        .context("failed to copy the store's user version")?;
    verify_copy(&source, &copy)?;
    stop(RotationStep::Exported)?;

    let directory = attachment_storage_dir(path);
    let new_vault = BlobVault::new(&directory, new_key);
    reencrypt_files(&copy, &directory, &old_vault, &new_vault, progress)?;
    stop(RotationStep::Reencrypted)?;

    copy.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .context("failed to checkpoint the re-encrypted copy")?;
    drop(copy);
    remove_journals(&rekeying)?;
    sync_file(&rekeying)?;
    drop(source);
    drop(store);
    connection::close_dropped_store_connections();
    fs::rename(&rekeying, path).with_context(|| {
        format!(
            "failed to move the re-encrypted store into place at {}",
            path.display()
        )
    })?;
    vault::sync_parent_dir(path);
    remove_journals(path)?;
    Ok(())
}

/// Step 3: re-encrypts every attachment file the copy's rows name, and
/// points the rows at the new names.
fn reencrypt_files(
    copy: &Connection,
    directory: &Path,
    old_vault: &BlobVault,
    new_vault: &BlobVault,
    progress: &(dyn Fn(MigrationProgress) + Sync),
) -> Result<()> {
    let received: Vec<String> = {
        let mut statement =
            copy.prepare("SELECT DISTINCT storage_path FROM message_attachments")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let drafts: Vec<(i64, String)> = {
        let mut statement = copy.prepare("SELECT id, storage_path FROM draft_attachments")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let total = received.len() + drafts.len();
    let mut done = 0;
    progress(MigrationProgress::Attachments { done, total });
    for name in received {
        let Ok(old) = old_vault.path_of(&name) else {
            continue;
        };
        let new_name = reencrypt(&old, &name, old_vault, new_vault)?;
        copy.execute(
            "UPDATE message_attachments SET storage_path = ?2 WHERE storage_path = ?1",
            params![name, new_name],
        )
        .context("failed to record a re-encrypted attachment")?;
        done += 1;
        progress(MigrationProgress::Attachments { done, total });
    }
    for (id, path) in drafts {
        let old = PathBuf::from(&path);
        let Some(name) = old.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let name = name.to_owned();
        // A draft copy is stored by absolute path; if the data directory has
        // moved since, it is beside the other attachments.
        let old = if old.is_file() {
            old
        } else {
            directory.join(&name)
        };
        let new_name = reencrypt(&old, &name, old_vault, new_vault)?;
        copy.execute(
            "UPDATE draft_attachments SET storage_path = ?2 WHERE id = ?1",
            params![id, new_vault.path_of(&new_name)?.display().to_string()],
        )
        .context("failed to record a re-encrypted draft attachment")?;
        done += 1;
        progress(MigrationProgress::Attachments { done, total });
    }
    Ok(())
}

/// Re-encrypts the file at `old` (named `name`) under the new vault,
/// streaming, and returns its new name. A missing file keeps missing.
fn reencrypt(
    old: &Path,
    name: &str,
    old_vault: &BlobVault,
    new_vault: &BlobVault,
) -> Result<String> {
    if old.is_file() {
        let mut reader = old_vault.reader(old)?;
        let (new_name, _) = new_vault.write_from(name, &mut reader)?;
        Ok(new_name)
    } else {
        Ok(new_vault.storage_name(name))
    }
}

/// Step 5: the record names the new key alone, and the old key's files and
/// keychain entry go.
fn finish(path: &Path, keychain: &dyn StoreKeychain, old: KeyId, new: KeyId) -> Result<()> {
    StoreKeyRecord {
        key_id: new,
        pending_key_id: None,
    }
    .write(&StoreKeyRecord::path_for(path))?;
    remove_files_with_key(&attachment_storage_dir(path), old)?;
    keychain
        .delete_store_key(old)
        .context("failed to delete the old store key from the keychain")?;
    Ok(())
}

/// Undoes a rotation that stopped before the swap: the copy, the new key's
/// files and the new key go, and the record names the old key alone.
fn undo(path: &Path, keychain: &dyn StoreKeychain, old: KeyId, new: KeyId) -> Result<()> {
    remove_with_journals(&suffixed(path, ".rekeying"))?;
    remove_files_with_key(&attachment_storage_dir(path), new)?;
    StoreKeyRecord {
        key_id: old,
        pending_key_id: None,
    }
    .write(&StoreKeyRecord::path_for(path))?;
    keychain
        .delete_store_key(new)
        .context("failed to delete an unused store key from the keychain")?;
    Ok(())
}

/// Finishes or undoes a rotation that a crash interrupted, depending on
/// which key opens the database. Runs under the exclusive lock.
pub(crate) fn resolve_interrupted(
    path: &Path,
    keychain: &dyn StoreKeychain,
) -> Result<(), StoreOpenError> {
    let record_path = StoreKeyRecord::path_for(path);
    let Some(StoreKeyRecord {
        key_id: old,
        pending_key_id: Some(new),
    }) = StoreKeyRecord::read(&record_path)?
    else {
        return Ok(());
    };
    let opens = |id: KeyId| -> Result<bool, StoreOpenError> {
        let key = match vault::load_key(keychain, id) {
            Ok(key) => key,
            Err(vault::StoreKeyError::Missing { .. }) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        match connection::open_keyed(path, &key) {
            Ok(_) => Ok(true),
            Err(error) if connection::is_not_a_database(&error) => Ok(false),
            Err(error) => Err(error.into()),
        }
    };
    if opens(old)? {
        undo(path, keychain, old, new)?;
    } else if opens(new)? {
        finish(path, keychain, old, new)?;
    } else {
        return Err(StoreOpenError::Unreadable { key_id: Some(old) });
    }
    connection::close_dropped_store_connections();
    Ok(())
}

/// Deletes the encrypted files in `directory` written with `key`.
fn remove_files_with_key(directory: &Path, key: KeyId) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to list {}", directory.display()));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && file_key_id(&path).ok().flatten() == Some(key) {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use zmail_core::{
        AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, NewDraft,
        ProtocolKind, ThreadSnapshot,
    };
    use zmail_security::InMemorySecretStore;

    use super::*;

    /// Text in the seeded message and attachments, and nowhere else.
    const PHRASE: &str = "albatross-quarry-semaphore";

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "zmail-rotation-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    fn no_progress(_: MigrationProgress) {}

    /// A store with a message, its attachment and a draft attachment, all
    /// carrying [`PHRASE`]. Returns the account.
    fn seeded_store(path: &Path, keychain: &InMemorySecretStore) -> i64 {
        let (store, _) = LocalStore::unlock(path, keychain, &no_progress).expect("unlocks");
        let account = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account");
        let target = store.sync_target(account).expect("target").expect("exists");
        let storage_path = target
            .blobs
            .write("incoming-INBOX-1-0-notes.txt", PHRASE.as_bytes())
            .expect("blob");
        let snapshot = AccountSyncSnapshot {
            checkpoint: "seed".to_owned(),
            detail: "seed".to_owned(),
            identities: None,
            mailboxes: vec![MailboxSnapshot {
                name: "Inbox".to_owned(),
                role: Some("inbox".to_owned()),
                threads: vec![ThreadSnapshot {
                    remote_thread_id: "t1".to_owned(),
                    subject: "Plans".to_owned(),
                    participants: "Alice -> me@example.com".to_owned(),
                    preview: "Plans".to_owned(),
                    is_flagged: false,
                    has_attachments: true,
                    unread_count: 1,
                    latest_message_at: "2026-10-01T09:00:00Z".to_owned(),
                    messages: vec![MessageSnapshot {
                        remote_id: "m1".to_owned(),
                        identity_email: "me@example.com".to_owned(),
                        sender: "Alice <alice@example.com>".to_owned(),
                        reply_to: None,
                        recipients: "me@example.com".to_owned(),
                        subject: "Plans".to_owned(),
                        body_preview: "Plans".to_owned(),
                        body_text: Some(format!("The plan is {PHRASE}.")),
                        sanitized_html: None,
                        has_remote_content: false,
                        is_read: false,
                        is_flagged: false,
                        received_at: "2026-10-01T09:00:00Z".to_owned(),
                        attachments: vec![MessageAttachment {
                            filename: "notes.txt".to_owned(),
                            content_type: "text/plain".to_owned(),
                            size_bytes: PHRASE.len() as u64,
                            storage_path,
                        }],
                    }],
                }],
                ..Default::default()
            }],
        };
        store
            .replace_account_snapshot(account, &snapshot)
            .expect("snapshot");
        let source_dir = temp_dir("draft-source");
        let source = source_dir.join("draft.txt");
        fs::write(&source, PHRASE).expect("written");
        let identity = store.list_identities(account).expect("identities")[0].id;
        let draft = store
            .create_draft(&NewDraft {
                account_id: account,
                identity_id: identity,
                to: vec!["bob@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Draft".to_owned(),
                body: "Draft".to_owned(),
            })
            .expect("draft");
        store.add_draft_attachment(draft, &source).expect("added");
        let _ = fs::remove_dir_all(source_dir);
        account
    }

    /// Every encrypted file in the store's attachment directory, by key.
    fn file_keys(path: &Path) -> Vec<KeyId> {
        let mut keys: Vec<KeyId> = fs::read_dir(attachment_storage_dir(path))
            .expect("listed")
            .flatten()
            .filter_map(|entry| file_key_id(&entry.path()).expect("readable"))
            .collect();
        keys.dedup();
        keys
    }

    /// Checks that the store opens with `keychain` and still has its mail,
    /// attachment and draft attachment, all under `key`.
    fn assert_intact(path: &Path, keychain: &InMemorySecretStore, account: i64, key: KeyId) {
        let (store, _) = LocalStore::unlock(path, keychain, &no_progress).expect("unlocks");
        assert_eq!(store.store_key().id(), key);
        assert_eq!(
            store
                .search_threads(account, PHRASE, 10)
                .expect("search")
                .len(),
            1
        );
        let attachment: i64 = store
            .connection()
            .expect("connection")
            .query_row("SELECT id FROM message_attachments", [], |row| row.get(0))
            .expect("attachment");
        assert_eq!(
            store
                .attachment_blob(attachment)
                .expect("blob")
                .read()
                .expect("decrypts"),
            PHRASE.as_bytes()
        );
        let draft_path: String = store
            .connection()
            .expect("connection")
            .query_row("SELECT storage_path FROM draft_attachments", [], |row| {
                row.get(0)
            })
            .expect("draft attachment");
        assert_eq!(
            store
                .vault
                .read_file(Path::new(&draft_path))
                .expect("decrypts"),
            PHRASE.as_bytes()
        );
        drop(store);
        assert_eq!(
            file_keys(path),
            vec![key],
            "every file is under the one key"
        );
        let record = StoreKeyRecord::read(&StoreKeyRecord::path_for(path))
            .expect("readable")
            .expect("exists");
        assert_eq!(record.key_id, key);
        assert_eq!(record.pending_key_id, None);
        assert!(!suffixed(path, ".rekeying").exists());
    }

    /// Whether any file under `directory` holds `needle` in plaintext.
    fn plaintext_anywhere(directory: &Path, needle: &[u8]) -> bool {
        fs::read_dir(directory)
            .expect("listed")
            .flatten()
            .any(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    plaintext_anywhere(&path, needle)
                } else {
                    fs::read(&path)
                        .expect("readable")
                        .windows(needle.len())
                        .any(|window| window == needle)
                }
            })
    }

    #[test]
    fn rotation_re_encrypts_everything_and_deletes_the_old_key() {
        let directory = temp_dir("rotate");
        let path = directory.join("zmail.sqlite3");
        let keychain = InMemorySecretStore::default();
        let account = seeded_store(&path, &keychain);
        let old = StoreKeyRecord::read(&StoreKeyRecord::path_for(&path))
            .expect("readable")
            .expect("exists")
            .key_id;

        let new = LocalStore::rotate_key(&path, &keychain, &no_progress).expect("rotates");
        assert_ne!(new, old);
        assert_intact(&path, &keychain, account, new);
        assert!(
            keychain.load_store_key(old).expect("readable").is_none(),
            "the old key is gone"
        );
        assert!(!plaintext_anywhere(&directory, PHRASE.as_bytes()));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rotation_is_refused_while_the_store_is_open() {
        let directory = temp_dir("busy");
        let path = directory.join("zmail.sqlite3");
        let keychain = InMemorySecretStore::default();
        seeded_store(&path, &keychain);
        let (open, _) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");

        let error = LocalStore::rotate_key(&path, &keychain, &no_progress).expect_err("refused");
        assert!(format!("{error:#}").contains("quit Zmail"), "{error:#}");
        // A second open alongside the first still works.
        let (second, _) = LocalStore::unlock(&path, &keychain, &no_progress).expect("unlocks");
        drop(second);
        drop(open);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn an_interrupted_rotation_is_undone_or_finished_on_the_next_open() {
        for step in [
            RotationStep::Exported,
            RotationStep::Reencrypted,
            RotationStep::Renamed,
        ] {
            let directory = temp_dir("interrupted");
            let path = directory.join("zmail.sqlite3");
            let keychain = InMemorySecretStore::default();
            let account = seeded_store(&path, &keychain);
            let old = StoreKeyRecord::read(&StoreKeyRecord::path_for(&path))
                .expect("readable")
                .expect("exists")
                .key_id;

            rotate_key_until(&path, &keychain, &no_progress, Some(step)).expect_err("stopped");
            let pending = StoreKeyRecord::read(&StoreKeyRecord::path_for(&path))
                .expect("readable")
                .expect("exists")
                .pending_key_id
                .expect("the rotation is pending");

            // Before the swap the old key still opens the store, so the
            // rotation is undone; after it, finished.
            let expected = if step == RotationStep::Renamed {
                pending
            } else {
                old
            };
            assert_intact(&path, &keychain, account, expected);
            let unused = if expected == old { pending } else { old };
            assert!(
                keychain.load_store_key(unused).expect("readable").is_none(),
                "{step:?}: the unused key is deleted"
            );
            assert!(!plaintext_anywhere(&directory, PHRASE.as_bytes()));
            let _ = fs::remove_dir_all(directory);
        }
    }
}
