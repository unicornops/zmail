//! Encrypted files beside the store: attachment blobs and draft
//! attachment copies (ADR 0005 §2).
//!
//! A file is a 56-byte header, then the contents in 64 KiB chunks sealed
//! with AES-256-GCM in the STREAM construction (`aead::stream`, a 32-bit
//! big-endian chunk counter and a last-chunk flag in each nonce). The last
//! chunk may be full; an empty file is one empty chunk.
//!
//! ```text
//! "ZMB1" | version 1 | 3 zero bytes | key id (16) | salt (32) | chunks…
//! ```
//!
//! Each file is sealed with its own key, HKDF-SHA256 of the store's blob
//! key and the file's random salt, so nonces never repeat across files.
//! The header is the associated data of every chunk: changing it, or
//! truncating, reordering or swapping chunks, fails authentication.
//!
//! File names are opaque: HMAC-SHA256 of the name the caller asks for
//! (which may hold a mailbox or an attachment's filename) under the blob
//! name key. They stay deterministic, so writing the same logical name
//! again replaces the file, as it did before encryption.

use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{
        Payload,
        stream::{DecryptorBE32, EncryptorBE32},
    },
};
use anyhow::{Context, Result, bail};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::{KEY_LEN, KeyId, StoreKey, Subkey, hex, sync_parent_dir};

/// The first bytes of every encrypted file.
const MAGIC: &[u8; 4] = b"ZMB1";

/// The format version after the magic.
const VERSION: u8 = 1;

/// Header length: magic, version, three reserved bytes, key id and salt.
pub const HEADER_LEN: usize = 4 + 1 + 3 + 16 + SALT_LEN;

/// Length of each file's random salt.
const SALT_LEN: usize = 32;

/// Plaintext bytes per chunk.
const CHUNK_LEN: usize = 64 * 1024;

/// Bytes the GCM tag adds to each chunk.
const TAG_LEN: usize = 16;

/// The STREAM nonce prefix. Every file has its own key, so the prefix
/// carries nothing and stays zero; the counter and last-chunk flag fill
/// the rest of the 96-bit nonce.
const NONCE_PREFIX: [u8; 7] = [0; 7];

/// HKDF label for a file's key.
const FILE_KEY_LABEL: &[u8] = b"zmail/store/blob-file/v1";

/// Reads and writes the encrypted files in one directory with one store
/// key. Cheap to clone; `Debug` shows the directory and key id only.
#[derive(Clone)]
pub struct BlobVault {
    inner: Arc<VaultInner>,
}

struct VaultInner {
    directory: PathBuf,
    key_id: KeyId,
    blob_key: Zeroizing<[u8; KEY_LEN]>,
    name_key: Zeroizing<[u8; KEY_LEN]>,
}

impl fmt::Debug for BlobVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobVault")
            .field("directory", &self.inner.directory)
            .field("key_id", &self.inner.key_id)
            .finish_non_exhaustive()
    }
}

impl BlobVault {
    /// The vault for the files in `directory`, sealed with `key`.
    pub fn new(directory: impl Into<PathBuf>, key: &StoreKey) -> Self {
        Self {
            inner: Arc::new(VaultInner {
                directory: directory.into(),
                key_id: key.id(),
                blob_key: key.subkey(Subkey::Blob),
                name_key: key.subkey(Subkey::BlobName),
            }),
        }
    }

    /// The directory the files live in.
    pub fn directory(&self) -> &Path {
        &self.inner.directory
    }

    /// The opaque file name for `logical_name`: 32 hex digits.
    pub fn storage_name(&self, logical_name: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.inner.name_key.as_ref())
            .expect("HMAC takes a key of any length");
        mac.update(logical_name.as_bytes());
        hex(&mac.finalize().into_bytes()[..16])
    }

    /// The path of the file `storage_name` names. Only plain file names
    /// are accepted, so a name from the database can't point elsewhere.
    pub fn path_of(&self, storage_name: &str) -> Result<PathBuf> {
        let name = Path::new(storage_name);
        let mut components = name.components();
        match (components.next(), components.next()) {
            (Some(std::path::Component::Normal(_)), None) => Ok(self.inner.directory.join(name)),
            _ => bail!("{storage_name:?} is not a file name in the vault"),
        }
    }

    /// Encrypts `contents` into the file for `logical_name`, replacing an
    /// earlier one, and returns its storage name.
    pub fn write(&self, logical_name: &str, contents: &[u8]) -> Result<String> {
        self.write_from(logical_name, &mut &*contents)
            .map(|(name, _)| name)
    }

    /// Encrypts everything `reader` yields into the file for
    /// `logical_name`, and returns its storage name and plaintext length.
    /// The file appears only once it is complete and synced.
    pub fn write_from(&self, logical_name: &str, reader: &mut dyn Read) -> Result<(String, u64)> {
        let name = self.storage_name(logical_name);
        let path = self.path_of(&name)?;
        fs::create_dir_all(&self.inner.directory)
            .with_context(|| format!("failed to create {}", self.inner.directory.display()))?;
        let mut suffix = [0; 8];
        OsRng.fill_bytes(&mut suffix);
        let temporary = self
            .inner
            .directory
            .join(format!("{name}.{}.tmp", hex(&suffix)));
        let result = (|| {
            let mut file = fs::File::create(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?;
            let length = self.seal(reader, &mut file)?;
            file.sync_all()
                .with_context(|| format!("failed to sync {}", temporary.display()))?;
            drop(file);
            fs::rename(&temporary, &path)
                .with_context(|| format!("failed to move {} into place", path.display()))?;
            Ok(length)
        })();
        match result {
            Ok(length) => {
                sync_parent_dir(&path);
                Ok((name, length))
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error)
            }
        }
    }

    /// Writes the header and the sealed chunks of `reader` to `out`.
    fn seal(&self, reader: &mut dyn Read, out: &mut dyn Write) -> Result<u64> {
        let mut salt = [0; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let header = header(self.inner.key_id, &salt);
        out.write_all(&header).context("failed to write")?;
        let mut encryptor = EncryptorBE32::from_aead(
            Aes256Gcm::new(file_key(&self.inner.blob_key, &salt).as_ref().into()),
            NONCE_PREFIX.as_ref().into(),
        );

        // Which chunk is last is only known once the next read comes back
        // empty, so one chunk is always held back.
        let mut current = Zeroizing::new(vec![0; CHUNK_LEN]);
        let mut next = Zeroizing::new(vec![0; CHUNK_LEN]);
        let mut current_len = read_full(reader, &mut current)?;
        let mut total = current_len as u64;
        loop {
            let next_len = if current_len == CHUNK_LEN {
                read_full(reader, &mut next)?
            } else {
                0
            };
            let payload = Payload {
                msg: &current[..current_len],
                aad: &header,
            };
            if next_len == 0 {
                let sealed = encryptor
                    .encrypt_last(payload)
                    .map_err(|_| anyhow::anyhow!("failed to encrypt"))?;
                out.write_all(&sealed).context("failed to write")?;
                return Ok(total);
            }
            let sealed = encryptor
                .encrypt_next(payload)
                .map_err(|_| anyhow::anyhow!("failed to encrypt"))?;
            out.write_all(&sealed).context("failed to write")?;
            std::mem::swap(&mut current, &mut next);
            current_len = next_len;
            total += next_len as u64;
        }
    }

    /// Decrypts the file at `path` into `out`, returning the plaintext
    /// length. Nothing unauthenticated is written: each chunk is checked
    /// before its bytes go out, and a truncated file fails at its end.
    pub fn decrypt_file_to(&self, path: &Path, out: &mut dyn Write) -> Result<u64> {
        let mut file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let mut header = [0; HEADER_LEN];
        file.read_exact(&mut header)
            .with_context(|| format!("{} is not an encrypted Zmail file", path.display()))?;
        let (key_id, salt) = parse_header(&header)
            .with_context(|| format!("{} is not an encrypted Zmail file", path.display()))?;
        if key_id != self.inner.key_id {
            bail!(
                "{} was encrypted with key {key_id}, not this store's key {}",
                path.display(),
                self.inner.key_id
            );
        }
        let mut decryptor = DecryptorBE32::from_aead(
            Aes256Gcm::new(file_key(&self.inner.blob_key, &salt).as_ref().into()),
            NONCE_PREFIX.as_ref().into(),
        );
        let mut current = vec![0; CHUNK_LEN + TAG_LEN];
        let mut next = vec![0; CHUNK_LEN + TAG_LEN];
        let mut current_len = read_full(&mut file, &mut current)?;
        let mut total = 0;
        let failed = || anyhow::anyhow!("{} is damaged or was tampered with", path.display());
        loop {
            let next_len = if current_len == current.len() {
                read_full(&mut file, &mut next)?
            } else {
                0
            };
            let payload = Payload {
                msg: &current[..current_len],
                aad: &header,
            };
            if next_len == 0 {
                let plain = Zeroizing::new(decryptor.decrypt_last(payload).map_err(|_| failed())?);
                out.write_all(&plain).context("failed to write")?;
                return Ok(total + plain.len() as u64);
            }
            let plain = Zeroizing::new(decryptor.decrypt_next(payload).map_err(|_| failed())?);
            out.write_all(&plain).context("failed to write")?;
            total += plain.len() as u64;
            std::mem::swap(&mut current, &mut next);
            current_len = next_len;
        }
    }

    /// Decrypts the file at `path` into memory.
    pub fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let mut contents = Vec::new();
        self.decrypt_file_to(path, &mut contents)?;
        Ok(contents)
    }

    /// Decrypts the file at `path` to a new file at `destination`, which
    /// must not exist yet. A failed copy is removed.
    pub fn decrypt_file_to_path(&self, path: &Path, destination: &Path) -> Result<u64> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        match self.decrypt_file_to(path, &mut file) {
            Ok(length) => Ok(length),
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(destination);
                Err(error)
            }
        }
    }
}

/// The sync adapters write received attachments through the vault.
impl zmail_core::BlobWriter for BlobVault {
    fn write_blob(&self, logical_name: &str, contents: &[u8]) -> io::Result<String> {
        self.write(logical_name, contents)
            .map_err(|error| io::Error::other(format!("{error:#}")))
    }

    fn location(&self) -> &Path {
        self.directory()
    }
}

/// Whether the file at `path` starts with the encrypted-file magic.
pub fn is_encrypted_file(path: &Path) -> io::Result<bool> {
    let mut magic = [0; MAGIC.len()];
    let mut file = fs::File::open(path)?;
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(&magic == MAGIC),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn header(key_id: KeyId, salt: &[u8; SALT_LEN]) -> [u8; HEADER_LEN] {
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4] = VERSION;
    header[8..24].copy_from_slice(key_id.as_bytes());
    header[24..].copy_from_slice(salt);
    header
}

fn parse_header(header: &[u8; HEADER_LEN]) -> Result<(KeyId, [u8; SALT_LEN])> {
    if &header[..4] != MAGIC {
        bail!("no ZMB1 header");
    }
    if header[4] != VERSION || header[5..8] != [0; 3] {
        bail!("format version {} is not supported", header[4]);
    }
    let mut key_id = [0; 16];
    key_id.copy_from_slice(&header[8..24]);
    let mut salt = [0; SALT_LEN];
    salt.copy_from_slice(&header[24..]);
    Ok((KeyId::from_bytes(key_id), salt))
}

fn file_key(blob_key: &[u8; KEY_LEN], salt: &[u8; SALT_LEN]) -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = Zeroizing::new([0; KEY_LEN]);
    Hkdf::<Sha256>::new(Some(salt), blob_key)
        .expand(FILE_KEY_LABEL, key.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Reads until `buffer` is full or the reader ends; returns the length.
fn read_full(reader: &mut dyn Read, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("failed to read"),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(name: &str) -> (BlobVault, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "zmail-blob-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        (BlobVault::new(&directory, &StoreKey::generate()), directory)
    }

    fn contents(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index * 31 % 251) as u8).collect()
    }

    #[test]
    fn files_round_trip_at_every_chunk_boundary() {
        let (vault, directory) = vault("round-trip");
        for length in [
            0,
            1,
            CHUNK_LEN - 1,
            CHUNK_LEN,
            CHUNK_LEN + 1,
            3 * CHUNK_LEN,
            3 * CHUNK_LEN + 17,
        ] {
            let plain = contents(length);
            let name = vault
                .write(&format!("file-{length}"), &plain)
                .expect("written");
            let path = vault.path_of(&name).expect("path");
            assert!(is_encrypted_file(&path).expect("readable"));
            let stored = fs::read(&path).expect("readable");
            let chunks = length.div_ceil(CHUNK_LEN).max(1);
            assert_eq!(
                stored.len(),
                HEADER_LEN + length + chunks * TAG_LEN,
                "{length}"
            );
            assert_eq!(vault.read_file(&path).expect("decrypts"), plain, "{length}");
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn names_are_opaque_and_stable() {
        let (vault, directory) = vault("names");
        let name = vault
            .write("incoming-INBOX-42-0-salary review.pdf", b"x")
            .expect("written");
        assert_eq!(name.len(), 32);
        assert!(name.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            vault.storage_name("incoming-INBOX-42-0-salary review.pdf"),
            name
        );
        let again = vault
            .write("incoming-INBOX-42-0-salary review.pdf", b"new contents")
            .expect("rewritten");
        assert_eq!(again, name, "the same logical name replaces the file");
        let files: Vec<_> = fs::read_dir(&directory).expect("listed").collect();
        assert_eq!(files.len(), 1, "no temporary files are left behind");
        assert_eq!(
            vault
                .read_file(&vault.path_of(&name).expect("path"))
                .expect("decrypts"),
            b"new contents"
        );

        let other = BlobVault::new(&directory, &StoreKey::generate());
        assert_ne!(
            other.storage_name("incoming-INBOX-42-0-salary review.pdf"),
            name
        );
        assert!(vault.path_of("../escape").is_err());
        assert!(vault.path_of("a/b").is_err());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn plaintext_never_reaches_the_file() {
        let (vault, directory) = vault("plaintext");
        let secret = b"the merger closes on friday".repeat(5000);
        let name = vault.write("secret", &secret).expect("written");
        let stored = fs::read(vault.path_of(&name).expect("path")).expect("readable");
        assert!(
            !stored
                .windows(b"merger".len())
                .any(|window| window == b"merger")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn tampering_truncation_and_the_wrong_key_are_refused() {
        let (vault, directory) = vault("tamper");
        let plain = contents(2 * CHUNK_LEN + 100);
        let name = vault.write("file", &plain).expect("written");
        let path = vault.path_of(&name).expect("path");
        let original = fs::read(&path).expect("readable");

        let mut flipped = original.clone();
        flipped[HEADER_LEN + 10] ^= 1;
        fs::write(&path, &flipped).expect("written");
        assert!(vault.read_file(&path).is_err(), "a flipped bit");

        let mut header = original.clone();
        header[30] ^= 1;
        fs::write(&path, &header).expect("written");
        assert!(vault.read_file(&path).is_err(), "a changed salt");

        // Cut at a chunk boundary: every remaining chunk authenticates, but
        // none is marked last.
        fs::write(&path, &original[..HEADER_LEN + 2 * (CHUNK_LEN + TAG_LEN)]).expect("written");
        assert!(vault.read_file(&path).is_err(), "a dropped last chunk");

        let mut swapped = original[..HEADER_LEN].to_vec();
        swapped.extend_from_slice(
            &original[HEADER_LEN + CHUNK_LEN + TAG_LEN..][..CHUNK_LEN + TAG_LEN],
        );
        swapped.extend_from_slice(&original[HEADER_LEN..][..CHUNK_LEN + TAG_LEN]);
        swapped.extend_from_slice(&original[HEADER_LEN + 2 * (CHUNK_LEN + TAG_LEN)..]);
        fs::write(&path, &swapped).expect("written");
        assert!(vault.read_file(&path).is_err(), "reordered chunks");

        fs::write(&path, &original).expect("written");
        let other = BlobVault::new(&directory, &StoreKey::generate());
        let error = other.read_file(&path).expect_err("another key");
        assert!(
            format!("{error:#}").contains("encrypted with key"),
            "{error:#}"
        );
        assert_eq!(vault.read_file(&path).expect("decrypts"), plain);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_failed_decryption_leaves_no_partial_copy() {
        let (vault, directory) = vault("partial");
        let name = vault
            .write("file", &contents(CHUNK_LEN * 2))
            .expect("written");
        let path = vault.path_of(&name).expect("path");
        let mut damaged = fs::read(&path).expect("readable");
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        fs::write(&path, &damaged).expect("written");
        let destination = directory.join("out.bin");
        assert!(vault.decrypt_file_to_path(&path, &destination).is_err());
        assert!(!destination.exists());
        let _ = fs::remove_dir_all(directory);
    }
}
