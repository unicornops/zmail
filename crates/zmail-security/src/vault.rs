//! The local store's encryption key (ADR 0005, `docs/adr/0005-store-encryption.md`).
//!
//! Each store has one random 256-bit data key. It lives in the OS keychain
//! under `store-<key id>`; beside the database, a small JSON record names
//! the key id and the formats, and holds nothing secret. The database and
//! the attachment files never use the data key directly: each gets its own
//! subkey through HKDF-SHA256, so the uses can't be confused.
//!
//! Creating a key is ordered so a crash can't strand data: the key is
//! written to the keychain and read back before the record exists, and
//! the record exists before anything is encrypted with the key.

use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, anyhow};

mod blob;

pub use blob::{BlobReader, BlobVault, file_key_id, is_encrypted_file};
use hkdf::Hkdf;
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Length of the data key and of every subkey, in bytes.
pub const KEY_LEN: usize = 32;

/// Length of a key id, in bytes.
const KEY_ID_LEN: usize = 16;

/// The record format this build writes and reads.
const RECORD_FORMAT: u64 = 1;

/// The database format the record names: SQLCipher 4 with a raw key.
const RECORD_DATABASE: &str = "sqlcipher4-raw-key";

/// The file format the record names (ADR 0005 §2).
const RECORD_FILES: &str = "zmb1-aes256gcm-stream";

/// Identifies a store key: its keychain entry is `store-<id>`, and every
/// encrypted file names the key it was written with.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyId([u8; KEY_ID_LEN]);

impl KeyId {
    /// A fresh random key id.
    fn generate() -> Self {
        let mut bytes = [0; KEY_ID_LEN];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// The id's raw bytes, as stored in file headers.
    pub fn as_bytes(&self) -> &[u8; KEY_ID_LEN] {
        &self.0
    }

    /// The id from the raw bytes of a file header.
    pub fn from_bytes(bytes: [u8; KEY_ID_LEN]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({self})")
    }
}

impl FromStr for KeyId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        let bytes = unhex(value).context("a key id is hexadecimal")?;
        let bytes: [u8; KEY_ID_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("a key id is {KEY_ID_LEN} bytes"))?;
        Ok(Self(bytes))
    }
}

/// What a subkey is for. Each purpose has its own HKDF label, so a subkey
/// for one can never serve another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subkey {
    /// The SQLCipher database key.
    Database,
    /// The key that file keys are derived from.
    Blob,
    /// The key that names encrypted files.
    BlobName,
}

impl Subkey {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Database => b"zmail/store/sqlcipher/v1",
            Self::Blob => b"zmail/store/blob/v1",
            Self::BlobName => b"zmail/store/blob-name/v1",
        }
    }
}

/// A store's data key. Cheap to clone; the secret is wiped from memory
/// when the last clone drops, and `Debug` never prints it.
#[derive(Clone)]
pub struct StoreKey {
    id: KeyId,
    secret: Arc<Zeroizing<[u8; KEY_LEN]>>,
}

impl StoreKey {
    /// A new random key with a new id, from the OS's random source.
    pub fn generate() -> Self {
        let mut secret = Zeroizing::new([0; KEY_LEN]);
        OsRng.fill_bytes(secret.as_mut());
        Self {
            id: KeyId::generate(),
            secret: Arc::new(secret),
        }
    }

    /// A key with a fixed id and secret, for checked-in test stores (the
    /// SQLCipher compatibility fixture in `zmail-store`). Never in a build
    /// that ships: only the `test-fixtures` feature has it.
    #[cfg(feature = "test-fixtures")]
    pub fn from_fixture(id: KeyId, secret: [u8; KEY_LEN]) -> Self {
        Self {
            id,
            secret: Arc::new(Zeroizing::new(secret)),
        }
    }

    /// The key's id.
    pub fn id(&self) -> KeyId {
        self.id
    }

    /// The subkey for `purpose`: HKDF-SHA256 with the key id as salt.
    pub fn subkey(&self, purpose: Subkey) -> Zeroizing<[u8; KEY_LEN]> {
        let hkdf = Hkdf::<Sha256>::new(Some(self.id.as_bytes()), &self.secret[..]);
        let mut subkey = Zeroizing::new([0; KEY_LEN]);
        hkdf.expand(purpose.label(), subkey.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        subkey
    }

    /// The value for SQLCipher's `PRAGMA key`: the database subkey as a raw
    /// key (`x'…'`), so SQLCipher skips its passphrase stretching. Wiped
    /// when dropped.
    pub fn sqlcipher_key(&self) -> Zeroizing<String> {
        let subkey = self.subkey(Subkey::Database);
        Zeroizing::new(format!("\"x'{}'\"", hex(subkey.as_ref())))
    }

    /// The secret as the keychain stores it: hexadecimal.
    fn secret_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex(&self.secret[..]))
    }

    /// Rebuilds a key from its id and the keychain's hexadecimal secret.
    fn from_secret_hex(id: KeyId, secret: &str) -> anyhow::Result<Self> {
        let bytes = Zeroizing::new(unhex(secret.trim()).context("the stored key is hexadecimal")?);
        let secret: [u8; KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("the stored key is not {KEY_LEN} bytes"))?;
        Ok(Self {
            id,
            secret: Arc::new(Zeroizing::new(secret)),
        })
    }

    /// Whether two keys are the same id and secret.
    fn same_as(&self, other: &Self) -> bool {
        self.id == other.id && self.secret[..] == other.secret[..]
    }
}

impl fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Why a store key couldn't be loaded or created.
#[derive(Debug, thiserror::Error)]
pub enum StoreKeyError {
    /// The record names a key the keychain doesn't have: the store can't be
    /// read, and only starting over (a resync from the servers) helps.
    #[error("the keychain has no key {key_id} for this mail store; it can't be read without it")]
    Missing {
        /// The key the record names.
        key_id: KeyId,
    },
    /// The keychain couldn't be reached or refused (locked, access denied,
    /// no Secret Service). Retrying may help; plaintext is never the fallback.
    #[error("the keychain is not available: {0:#}")]
    Keychain(anyhow::Error),
    /// The key record beside the store is unreadable or from a newer build.
    #[error("the store's key record is unusable: {0:#}")]
    Record(anyhow::Error),
}

/// Where store keys are kept: the OS keychain in the app, memory in tests.
pub trait StoreKeychain: Send + Sync {
    /// Saves `key` under its id, replacing an entry with the same id.
    fn save_store_key(&self, key: &StoreKey) -> anyhow::Result<()>;
    /// Loads the key with `id`; `Ok(None)` when there is no such entry.
    fn load_store_key(&self, id: KeyId) -> anyhow::Result<Option<StoreKey>>;
    /// Deletes the key with `id`; deleting a missing key is not an error.
    fn delete_store_key(&self, id: KeyId) -> anyhow::Result<()>;
}

impl StoreKeychain for crate::KeyringSecretStore {
    fn save_store_key(&self, key: &StoreKey) -> anyhow::Result<()> {
        store_entry(key.id)?
            .set_password(&key.secret_hex())
            .with_context(|| format!("failed to store the mail store key {}", key.id))
    }

    fn load_store_key(&self, id: KeyId) -> anyhow::Result<Option<StoreKey>> {
        match store_entry(id)?.get_password() {
            Ok(secret) => {
                let secret = Zeroizing::new(secret);
                StoreKey::from_secret_hex(id, &secret).map(Some)
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("failed to load the mail store key {id}"))
            }
        }
    }

    fn delete_store_key(&self, id: KeyId) -> anyhow::Result<()> {
        match store_entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to delete the mail store key {id}"))
            }
        }
    }
}

impl StoreKeychain for crate::InMemorySecretStore {
    fn save_store_key(&self, key: &StoreKey) -> anyhow::Result<()> {
        self.lock_store_keys()
            .insert(key.id, Zeroizing::new(key.secret_hex().to_string()));
        Ok(())
    }

    fn load_store_key(&self, id: KeyId) -> anyhow::Result<Option<StoreKey>> {
        self.lock_store_keys()
            .get(&id)
            .map(|secret| StoreKey::from_secret_hex(id, secret))
            .transpose()
    }

    fn delete_store_key(&self, id: KeyId) -> anyhow::Result<()> {
        self.lock_store_keys().remove(&id);
        Ok(())
    }
}

/// The keychain entry for a store key.
fn store_entry(id: KeyId) -> anyhow::Result<keyring::Entry> {
    keyring::Entry::new(crate::KEYRING_SERVICE, &format!("store-{id}"))
        .map_err(anyhow::Error::from)
        .context("failed to prepare secure storage for the mail store key")
}

/// The non-secret record beside the database: which key opens the store,
/// and in which formats it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreKeyRecord {
    /// The key the store is encrypted with.
    pub key_id: KeyId,
    /// The key a rotation is moving the store to, while one is under way
    /// (ADR 0005 §3). Whichever of the two opens the database tells how
    /// far it got.
    pub pending_key_id: Option<KeyId>,
}

impl StoreKeyRecord {
    /// The record's path for the database at `database`:
    /// `zmail.sqlite3` → `zmail.store-key.json`.
    pub fn path_for(database: &Path) -> PathBuf {
        let stem = database
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("zmail");
        database.with_file_name(format!("{stem}.store-key.json"))
    }

    /// Reads the record at `path`; `Ok(None)` when there is none.
    pub fn read(path: &Path) -> Result<Option<Self>, StoreKeyError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreKeyError::Record(
                    anyhow::Error::from(error)
                        .context(format!("failed to read {}", path.display())),
                ));
            }
        };
        Self::parse(&bytes)
            .map(Some)
            .map_err(|error| StoreKeyError::Record(error.context(path.display().to_string())))
    }

    fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).context("not JSON")?;
        let format = value
            .get("format")
            .and_then(serde_json::Value::as_u64)
            .context("no format")?;
        if format != RECORD_FORMAT {
            anyhow::bail!("format {format} is not supported by this version of Zmail");
        }
        let key_id = value
            .get("key_id")
            .and_then(serde_json::Value::as_str)
            .context("no key id")?
            .parse()?;
        let pending_key_id = value
            .get("pending_key_id")
            .and_then(serde_json::Value::as_str)
            .map(str::parse)
            .transpose()?;
        Ok(Self {
            key_id,
            pending_key_id,
        })
    }

    /// Writes the record to `path` atomically: a temporary file, synced,
    /// then renamed over the old record.
    pub fn write(&self, path: &Path) -> Result<(), StoreKeyError> {
        self.write_atomically(path).map_err(StoreKeyError::Record)
    }

    fn write_atomically(&self, path: &Path) -> anyhow::Result<()> {
        let mut json = serde_json::json!({
            "format": RECORD_FORMAT,
            "key_id": self.key_id.to_string(),
            "database": RECORD_DATABASE,
            "files": RECORD_FILES,
        });
        if let Some(pending) = self.pending_key_id {
            json["pending_key_id"] = serde_json::Value::from(pending.to_string());
        }
        let mut text =
            serde_json::to_string_pretty(&json).context("failed to encode the record")?;
        text.push('\n');
        let temporary = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?;
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .with_context(|| format!("failed to write {}", temporary.display()))?;
        }
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        sync_parent_dir(path);
        Ok(())
    }
}

/// Creates the key for a new store: a random key, saved to the keychain and
/// read back before the record at `record_path` names it.
pub fn create_store_key(
    keychain: &dyn StoreKeychain,
    record_path: &Path,
) -> Result<StoreKey, StoreKeyError> {
    let key = stash_new_key(keychain)?;
    StoreKeyRecord {
        key_id: key.id,
        pending_key_id: None,
    }
    .write(record_path)?;
    Ok(key)
}

/// A new random key, saved to the keychain and read back, that no record
/// names yet.
pub fn stash_new_key(keychain: &dyn StoreKeychain) -> Result<StoreKey, StoreKeyError> {
    let key = StoreKey::generate();
    keychain
        .save_store_key(&key)
        .map_err(StoreKeyError::Keychain)?;
    // A keychain that accepts a write but returns something else (or
    // nothing) would lose the store later; find out now, before any data
    // is encrypted.
    match keychain.load_store_key(key.id) {
        Ok(Some(stored)) if stored.same_as(&key) => {}
        Ok(_) => {
            let _ = keychain.delete_store_key(key.id);
            return Err(StoreKeyError::Keychain(anyhow!(
                "the keychain did not return the key it just stored"
            )));
        }
        Err(error) => return Err(StoreKeyError::Keychain(error)),
    }
    Ok(key)
}

/// Loads the key the record at `record_path` names. `Ok(None)` when there
/// is no record (a new or still plaintext store).
pub fn load_store_key(
    keychain: &dyn StoreKeychain,
    record_path: &Path,
) -> Result<Option<StoreKey>, StoreKeyError> {
    let Some(record) = StoreKeyRecord::read(record_path)? else {
        return Ok(None);
    };
    load_key(keychain, record.key_id).map(Some)
}

/// Loads the key with `id`, reporting a missing one as
/// [`StoreKeyError::Missing`].
pub fn load_key(keychain: &dyn StoreKeychain, id: KeyId) -> Result<StoreKey, StoreKeyError> {
    match keychain.load_store_key(id) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err(StoreKeyError::Missing { key_id: id }),
        Err(error) => Err(StoreKeyError::Keychain(error)),
    }
}

/// Makes a rename in `path`'s directory durable. Best effort: Windows has
/// no directory handles to sync, and the rename itself already happened.
pub fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `count` random bytes from the OS, as hexadecimal: for names that must
/// not be guessable, such as the open cache's directories.
pub fn random_hex(count: usize) -> String {
    let mut bytes = vec![0; count];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}

fn unhex(text: &str) -> anyhow::Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        anyhow::bail!("odd length");
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            text.get(index..index + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .context("not a hexadecimal digit")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemorySecretStore;

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "zmail-vault-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    #[test]
    fn a_created_key_loads_back_from_the_keychain() {
        let directory = temp_dir("create");
        let record = StoreKeyRecord::path_for(&directory.join("zmail.sqlite3"));
        assert_eq!(record, directory.join("zmail.store-key.json"));
        let keychain = InMemorySecretStore::default();

        assert!(
            load_store_key(&keychain, &record)
                .expect("no record is not an error")
                .is_none()
        );
        let key = create_store_key(&keychain, &record).expect("key is created");
        let loaded = load_store_key(&keychain, &record)
            .expect("key loads")
            .expect("the record names it");
        assert!(loaded.same_as(&key));
        assert_eq!(
            *loaded.sqlcipher_key(),
            *key.sqlcipher_key(),
            "the same key opens the same database"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn the_record_holds_no_secret() {
        let directory = temp_dir("record");
        let record = directory.join("zmail.store-key.json");
        let keychain = InMemorySecretStore::default();
        let key = create_store_key(&keychain, &record).expect("key is created");

        let text = fs::read_to_string(&record).expect("record is written");
        assert!(text.contains(&key.id().to_string()));
        assert!(!text.contains(key.secret_hex().as_str()));
        for purpose in [Subkey::Database, Subkey::Blob, Subkey::BlobName] {
            assert!(!text.contains(&hex(key.subkey(purpose).as_ref())));
        }
        assert!(!directory.join("zmail.store-key.json.tmp").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_key_missing_from_the_keychain_is_reported_as_missing() {
        let directory = temp_dir("missing");
        let record = directory.join("zmail.store-key.json");
        let keychain = InMemorySecretStore::default();
        let key = create_store_key(&keychain, &record).expect("key is created");
        keychain.delete_store_key(key.id()).expect("deleted");

        match load_store_key(&keychain, &record) {
            Err(StoreKeyError::Missing { key_id }) => assert_eq!(key_id, key.id()),
            other => panic!("expected a missing key, got {other:?}"),
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn an_unreadable_or_newer_record_is_an_error_not_a_new_store() {
        let directory = temp_dir("bad-record");
        let record = directory.join("zmail.store-key.json");
        let keychain = InMemorySecretStore::default();

        fs::write(&record, b"{not json").expect("written");
        assert!(matches!(
            load_store_key(&keychain, &record),
            Err(StoreKeyError::Record(_))
        ));
        fs::write(&record, br#"{"format": 2, "key_id": "00"}"#).expect("written");
        assert!(matches!(
            load_store_key(&keychain, &record),
            Err(StoreKeyError::Record(_))
        ));
        let _ = fs::remove_dir_all(directory);
    }

    /// A keychain that accepts writes and then has nothing, as a broken
    /// Secret Service can.
    struct ForgetfulKeychain;

    impl StoreKeychain for ForgetfulKeychain {
        fn save_store_key(&self, _: &StoreKey) -> anyhow::Result<()> {
            Ok(())
        }
        fn load_store_key(&self, _: KeyId) -> anyhow::Result<Option<StoreKey>> {
            Ok(None)
        }
        fn delete_store_key(&self, _: KeyId) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_keychain_that_loses_the_key_fails_before_the_record_is_written() {
        let directory = temp_dir("forgetful");
        let record = directory.join("zmail.store-key.json");
        assert!(matches!(
            create_store_key(&ForgetfulKeychain, &record),
            Err(StoreKeyError::Keychain(_))
        ));
        assert!(!record.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn subkeys_differ_by_purpose_and_by_key() {
        let key = StoreKey::generate();
        let other = StoreKey::generate();
        let database = key.subkey(Subkey::Database);
        let blob = key.subkey(Subkey::Blob);
        let name = key.subkey(Subkey::BlobName);
        assert_ne!(*database, *blob);
        assert_ne!(*blob, *name);
        assert_ne!(*database, *name);
        assert_ne!(*database, *other.subkey(Subkey::Database));
        assert_eq!(*database, *key.subkey(Subkey::Database));
        assert_ne!(key.id(), other.id());
    }

    #[test]
    fn debug_output_never_shows_the_secret() {
        let key = StoreKey::generate();
        let debug = format!("{key:?}");
        assert!(debug.contains(&key.id().to_string()));
        assert!(!debug.contains(key.secret_hex().as_str()));
        let pragma = key.sqlcipher_key();
        assert!(pragma.starts_with("\"x'") && pragma.ends_with("'\""));
        assert_eq!(pragma.len(), KEY_LEN * 2 + 5);
    }

    #[test]
    fn key_ids_round_trip_through_text() {
        let id = KeyId::generate();
        assert_eq!(id.to_string().parse::<KeyId>().expect("parses"), id);
        assert!("abc".parse::<KeyId>().is_err());
        assert!("zz".repeat(KEY_ID_LEN).parse::<KeyId>().is_err());
    }
}
