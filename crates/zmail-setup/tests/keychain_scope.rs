//! Account secrets belong to their store (#475, ADR 0005 §3, "Account
//! secrets").
//!
//! Every store on a machine shares the one keychain, as these tests share
//! one [`InMemoryKeychain`]: a second data directory (`ZMAIL_DATA_DIR`), a
//! new store after `reset-store`, a store whose key turns up again. Each
//! test goes through what the app does (adding and removing accounts with
//! `zmail_setup`, opening stores with `LocalStore::unlock` and
//! `LocalStore::account_secrets`) and ends by checking the name of every
//! keychain entry it touched ([`assert_entries_named_by_store`]).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use zmail_security::{
    InMemoryKeychain, Keychain, SecretSlot, SecretStore, StoreId,
    keychain::KeychainAccess,
    vault::{KeyId, StoreKey},
};
use zmail_setup::{
    accounts::{remove_account, store_account, store_authorized_account},
    form::AccountFormValues,
};
use zmail_store::LocalStore;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A password account's form.
fn password_form(email: &str, password: &str) -> AccountFormValues {
    AccountFormValues {
        name: "Jane".to_owned(),
        email: email.to_owned(),
        password: password.to_owned(),
        imap_host: "imap.example.test".to_owned(),
        smtp_host: "smtp.example.test".to_owned(),
        ..AccountFormValues::default()
    }
}

/// How an account is added: the two ways that clear and write secrets.
#[derive(Debug, Clone, Copy)]
enum Adding {
    /// `store_account` with a password.
    Password,
    /// `store_authorized_account` with an OAuth2 grant.
    OAuth,
}

/// Adds an account to `store` the way the frontends do; returns its id.
fn add_account(store: &Arc<LocalStore>, secrets: &Arc<dyn SecretStore>, adding: Adding) -> i64 {
    match adding {
        Adding::Password => {
            store_account(
                store,
                secrets,
                None,
                &password_form("sam@example.test", "second"),
            )
            .expect("added")
            .0
        }
        Adding::OAuth => {
            let form = AccountFormValues {
                email: "sam@gmail.com".to_owned(),
                provider: Some(zmail_autodiscover::Provider::Google),
                use_oauth: true,
                oauth_client_id: "42-abc.apps.googleusercontent.com".to_owned(),
                oauth_client_secret: "GOCSPX-x".to_owned(),
                imap_host: "imap.gmail.com".to_owned(),
                smtp_host: "smtp.gmail.com".to_owned(),
                ..AccountFormValues::default()
            };
            let tokens = zmail_auth::TokenSet {
                access_token: "second access".to_owned(),
                refresh_token: Some("second refresh".to_owned()),
                expires_in: Some(3600),
            };
            store_authorized_account(store, secrets.as_ref(), None, &form, &tokens)
                .expect("added")
                .0
        }
    }
}

/// What a test keeps in `slot` for the account of `owner`.
fn secret(owner: &str, slot: SecretSlot) -> String {
    format!("{owner} {}", slot.as_str())
}

/// Adds a password account to `store` and gives it a secret in every slot
/// there is, as a Proton session, an OAuth grant and a password would.
fn account_with_every_slot(
    store: &Arc<LocalStore>,
    secrets: &Arc<dyn SecretStore>,
    owner: &str,
) -> i64 {
    let (account_id, _) = store_account(
        store,
        secrets,
        None,
        &password_form("jane@example.test", "first"),
    )
    .expect("added");
    for slot in SecretSlot::ALL {
        secrets
            .set_account_secret_in_slot(account_id, slot, &secret(owner, slot))
            .expect("kept");
    }
    account_id
}

/// `account_id` reads `owner`'s secret in every slot.
fn assert_every_slot(secrets: &dyn SecretStore, account_id: i64, owner: &str) {
    for slot in SecretSlot::ALL {
        let found = secrets
            .get_account_secret_from_slot(account_id, slot)
            .unwrap_or_else(|error| panic!("{owner}'s {slot:?} is gone: {error:#}"));
        assert_eq!(found, secret(owner, slot), "{slot:?}");
    }
}

/// `account_id` has nothing in any slot.
fn assert_no_slot(secrets: &dyn SecretStore, account_id: i64) {
    for slot in SecretSlot::ALL {
        assert!(
            secrets
                .get_account_secret_from_slot(account_id, slot)
                .is_err(),
            "{slot:?} is still there"
        );
    }
}

/// The name builds before #475 gave an account's secret. Written here by
/// hand, as those builds wrote it, not with the code under test.
fn old_name(account_id: i64, slot: SecretSlot) -> String {
    match slot {
        SecretSlot::Password => format!("account-{account_id}"),
        slot => format!("account-{account_id}-{}", slot.as_str()),
    }
}

/// Whether `name` is one of the names builds before #475 used.
fn is_old_name(name: &str) -> bool {
    name.strip_prefix("account-").is_some_and(|rest| {
        let (id, slot) = rest.split_once('-').unwrap_or((rest, "password"));
        id.parse::<i64>().is_ok() && SecretSlot::ALL.iter().any(|s| s.as_str() == slot)
    })
}

/// Whether `name` is a store key's or a store marker's entry (ADR 0005 §3),
/// which belong to a store, not an account.
fn is_store_entry(name: &str) -> bool {
    let hex32 = |text: &str| text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit());
    name.strip_prefix("store-at-").is_some_and(hex32)
        || name.strip_prefix("store-").is_some_and(hex32)
}

/// The guard behind every test: each keychain entry the flows touched is
/// a store key or marker, or an account entry named by one of `stores`
/// (`store-<store id>-account-<id>-<slot>`, for a slot that exists). Old
/// names are only read and deleted, never written, and only when
/// `old_names_allowed` (a store from before #475 is moving its secrets).
/// A name built without its store fails here, whichever flow built it.
fn assert_entries_named_by_store(
    keychain: &InMemoryKeychain,
    stores: &[StoreId],
    old_names_allowed: bool,
) {
    let accesses = keychain.accesses();
    assert!(!accesses.is_empty(), "the flows used the keychain");
    for (access, name) in accesses {
        if is_store_entry(&name) {
            continue;
        }
        if is_old_name(&name) {
            assert!(
                old_names_allowed,
                "{name} was used by a store that has no secrets under the old names"
            );
            assert_ne!(access, KeychainAccess::Write, "{name} is an old name");
            continue;
        }
        let account_entry = name
            .strip_prefix("store-")
            .and_then(|rest| rest.split_once("-account-"))
            .and_then(|(store, rest)| {
                let (id, slot) = rest.split_once('-')?;
                Some((
                    store.parse::<StoreId>().ok()?,
                    id.parse::<i64>().ok()?,
                    slot,
                ))
            });
        let Some((store, _, slot)) = account_entry else {
            panic!("{name} ({access:?}) is not named by a store");
        };
        assert!(
            stores.contains(&store),
            "{name} names a store that is not one of {stores:?}"
        );
        assert!(
            SecretSlot::ALL.iter().any(|known| known.as_str() == slot),
            "{name} names an unknown slot"
        );
    }
}

/// A directory for an on-disk store, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zmail-keychain-scope-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("zmail.sqlite3")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Opens the store at `path` as the app does, its key in `keychain`.
fn unlock(path: &Path, keychain: &Arc<InMemoryKeychain>) -> Arc<LocalStore> {
    let (store, _) =
        LocalStore::unlock(path, keychain.as_ref(), &|_| {}).expect("the store unlocks");
    Arc::new(store)
}

/// A copy of `zmail-store`'s SQLCipher fixture: a store written by a build
/// from before #475, with one account. Opened with the fixture's key.
fn store_from_before_475(directory: &TempDir) -> Arc<LocalStore> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zmail-store/tests/fixtures/store-sqlcipher-4.14/store.sqlite3");
    let path = directory.0.join("store.sqlite3");
    fs::copy(fixture, &path).expect("copied");
    let key = StoreKey::from_fixture(KeyId::from_bytes([0x5a; 16]), [0x42; 32]);
    Arc::new(LocalStore::open(&path, key).expect("the old store opens"))
}

/// Puts `owner`'s secret in every slot under the names builds before #475
/// used for `account_id`. The guard doesn't see these writes: they are
/// what an earlier build did.
fn seed_old_names(keychain: &InMemoryKeychain, account_id: i64, owner: &str) {
    for slot in SecretSlot::ALL {
        keychain
            .write_entry(&old_name(account_id, slot), &secret(owner, slot))
            .expect("seeded");
    }
    keychain.clear_accesses();
}

// ---------------------------------------------------------------------------
// Two stores, one keychain
// ---------------------------------------------------------------------------

/// The bug in #475: adding an account to a second store cleared the first
/// store's secrets for the same account id. Fails on the code before the
/// fix (the first store's password reads "second").
#[test]
fn adding_an_account_to_a_second_store_leaves_the_first_stores_secrets() {
    for adding in [Adding::Password, Adding::OAuth] {
        let keychain = InMemoryKeychain::shared();
        let first = Arc::new(LocalStore::open_ephemeral().expect("a store"));
        let second = Arc::new(LocalStore::open_ephemeral().expect("another store"));
        let first_secrets = first.account_secrets(keychain.clone()).expect("secrets");
        let second_secrets = second.account_secrets(keychain.clone()).expect("secrets");

        let first_id = account_with_every_slot(&first, &first_secrets, "first");
        let second_id = add_account(&second, &second_secrets, adding);
        assert_eq!(first_id, second_id, "both stores start at the same id");

        assert_every_slot(first_secrets.as_ref(), first_id, "first");
        // The same after a restart.
        let reopened = first.account_secrets(keychain.clone()).expect("secrets");
        assert_every_slot(reopened.as_ref(), first_id, "first");
        // And the second store reads its own.
        let (slot, expected) = match adding {
            Adding::Password => (SecretSlot::Password, "second"),
            Adding::OAuth => (SecretSlot::RefreshToken, "second refresh"),
        };
        assert_eq!(
            second_secrets
                .get_account_secret_from_slot(second_id, slot)
                .expect("its own"),
            expected,
            "{adding:?}"
        );
        assert_ne!(
            first.store_id().expect("an id"),
            second.store_id().expect("an id")
        );
        assert_entries_named_by_store(
            &keychain,
            &[
                first.store_id().expect("an id"),
                second.store_id().expect("an id"),
            ],
            false,
        );
    }
}

#[test]
fn removing_an_account_in_one_store_leaves_the_other_stores_same_id() {
    let keychain = InMemoryKeychain::shared();
    let first = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    let second = Arc::new(LocalStore::open_ephemeral().expect("another store"));
    let first_secrets = first.account_secrets(keychain.clone()).expect("secrets");
    let second_secrets = second.account_secrets(keychain.clone()).expect("secrets");
    let first_id = account_with_every_slot(&first, &first_secrets, "first");
    let second_id = account_with_every_slot(&second, &second_secrets, "second");
    assert_eq!(first_id, second_id);

    remove_account(&second, second_secrets.as_ref(), None, second_id).expect("removed");

    assert_no_slot(second_secrets.as_ref(), second_id);
    assert_every_slot(first_secrets.as_ref(), first_id, "first");
    // Nothing of the second store's is left in the keychain.
    let second_prefix = format!("store-{}-", second.store_id().expect("an id"));
    assert!(
        keychain
            .entry_names()
            .iter()
            .all(|name| !name.starts_with(&second_prefix)),
        "{:?}",
        keychain.entry_names()
    );
    assert_entries_named_by_store(
        &keychain,
        &[
            first.store_id().expect("an id"),
            second.store_id().expect("an id"),
        ],
        false,
    );
}

/// `reset-store` moves a store whose key is gone aside and the next start
/// makes a new one in the same directory, whose ids start again at 1. If
/// the old key turns up again, the old store still has its secrets.
#[test]
fn a_new_store_in_the_same_directory_leaves_the_old_stores_secrets() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("reset");
    let path = directory.database();

    let old = unlock(&path, &keychain);
    let old_id = old.store_id().expect("an id");
    let old_secrets = old.account_secrets(keychain.clone()).expect("secrets");
    let account_id = account_with_every_slot(&old, &old_secrets, "old");
    drop(old_secrets);
    drop(old);

    // What `zmail-app reset-store` (and the window's Start Over) does.
    let aside = zmail_store::move_unreadable_store_aside(&path).expect("moved aside");

    let new = unlock(&path, &keychain);
    assert_ne!(
        new.store_id().expect("an id"),
        old_id,
        "a new store, a new id"
    );
    let new_secrets = new.account_secrets(keychain.clone()).expect("secrets");
    let new_account = add_account(&new, &new_secrets, Adding::Password);
    assert_eq!(new_account, account_id, "the new store starts at 1 again");
    remove_account(&new, new_secrets.as_ref(), None, new_account).expect("removed");
    let new_id = new.store_id().expect("an id");
    drop(new_secrets);
    drop(new);

    // The old key turned up: the old store opens with its secrets.
    let old = unlock(&aside.join("zmail.sqlite3"), &keychain);
    assert_eq!(old.store_id().expect("an id"), old_id);
    let old_secrets = old.account_secrets(keychain.clone()).expect("secrets");
    assert_every_slot(old_secrets.as_ref(), account_id, "old");
    assert_entries_named_by_store(&keychain, &[old_id, new_id], false);
}

/// Rotating the key copies the database: the store keeps its id, and so
/// its secrets.
#[test]
fn a_rotated_store_keeps_its_secrets() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("rotate");
    let path = directory.database();
    let store = unlock(&path, &keychain);
    let store_id = store.store_id().expect("an id");
    let secrets = store.account_secrets(keychain.clone()).expect("secrets");
    let account_id = account_with_every_slot(&store, &secrets, "mine");
    drop(secrets);
    drop(store);

    LocalStore::rotate_key(&path, keychain.as_ref(), &|_| {}).expect("rotated");

    let store = unlock(&path, &keychain);
    assert_eq!(store.store_id().expect("an id"), store_id);
    let secrets = store.account_secrets(keychain.clone()).expect("secrets");
    assert_every_slot(secrets.as_ref(), account_id, "mine");
    assert_entries_named_by_store(&keychain, &[store_id], false);
}

// ---------------------------------------------------------------------------
// Account ids and slots
// ---------------------------------------------------------------------------

#[test]
fn account_ids_are_not_handed_out_twice() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("ids");
    let path = directory.database();
    let store = unlock(&path, &keychain);
    let secrets = store.account_secrets(keychain.clone()).expect("secrets");

    let first = add_account(&store, &secrets, Adding::Password);
    let second = add_account(&store, &secrets, Adding::Password);
    assert_eq!((first, second), (1, 2));
    // The highest id goes, then every id: none comes back.
    remove_account(&store, secrets.as_ref(), None, second).expect("removed");
    let third = add_account(&store, &secrets, Adding::OAuth);
    assert_eq!(third, 3);
    for account in [first, third] {
        remove_account(&store, secrets.as_ref(), None, account).expect("removed");
    }
    let store_id = store.store_id().expect("an id");
    drop(secrets);
    drop(store);
    // Not after reopening either.
    let store = unlock(&path, &keychain);
    let secrets = store.account_secrets(keychain.clone()).expect("secrets");
    assert_eq!(add_account(&store, &secrets, Adding::Password), 4);
    assert_entries_named_by_store(&keychain, &[store_id], false);
}

#[test]
fn removing_an_account_clears_every_slot() {
    let keychain = InMemoryKeychain::shared();
    let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
    let secrets = store.account_secrets(keychain.clone()).expect("secrets");
    let kept = account_with_every_slot(&store, &secrets, "kept");
    let removed = account_with_every_slot(&store, &secrets, "removed");

    remove_account(&store, secrets.as_ref(), None, removed).expect("removed");

    assert_no_slot(secrets.as_ref(), removed);
    assert_every_slot(secrets.as_ref(), kept, "kept");
    // Every slot there is (`SecretSlot::ALL`, which a test in
    // zmail-security keeps complete) was named and is gone.
    let store_id = store.store_id().expect("an id");
    for slot in SecretSlot::ALL {
        let name = zmail_security::keychain::account_entry_name(store_id, removed, slot);
        assert!(!keychain.entry_names().contains(&name), "{name}");
    }
    assert_eq!(keychain.entry_names().len(), SecretSlot::ALL.len());
    assert_entries_named_by_store(&keychain, &[store_id], false);
}

// ---------------------------------------------------------------------------
// Moving secrets from the names before #475
// ---------------------------------------------------------------------------

#[test]
fn a_store_from_before_475_moves_its_secrets_from_the_old_names() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("migrate");
    let store = store_from_before_475(&directory);
    let account_id = store.list_accounts().expect("accounts")[0].id;
    seed_old_names(&keychain, account_id, "old build");

    let secrets = store.account_secrets(keychain.clone()).expect("secrets");

    assert_every_slot(secrets.as_ref(), account_id, "old build");
    let store_id = store.store_id().expect("an id");
    let names = keychain.entry_names();
    assert_eq!(names.len(), SecretSlot::ALL.len(), "{names:?}");
    for name in &names {
        assert!(!is_old_name(name), "{name} is still under its old name");
        assert!(name.starts_with(&format!("store-{store_id}-account-{account_id}-")));
    }
    // A later start finds nothing to move and leaves the old names alone.
    let before = keychain.accesses().len();
    let again = store.account_secrets(keychain.clone()).expect("secrets");
    assert_every_slot(again.as_ref(), account_id, "old build");
    assert!(
        keychain.accesses()[before..]
            .iter()
            .all(|(_, name)| !is_old_name(name)),
        "the account is no longer looked for under the old names"
    );
    // A new account in the store never was.
    let added = add_account(&store, &again, Adding::Password);
    assert!(added > account_id);
    assert_entries_named_by_store(&keychain, &[store_id], true);
}

/// The migration must not bring #475 back: a store made after the
/// upgrade, whose first account has the same id, neither takes nor deletes
/// the old store's secrets under the old names, whatever it does with its
/// own account. The old store then still moves all of them.
#[test]
fn a_new_store_never_takes_or_deletes_secrets_under_the_old_names() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("steal");
    let old = store_from_before_475(&directory);
    let account_id = old.list_accounts().expect("accounts")[0].id;
    seed_old_names(&keychain, account_id, "old build");

    // The new store opens first and does everything with the same id.
    let new = Arc::new(LocalStore::open_ephemeral().expect("a new store"));
    let new_secrets = new.account_secrets(keychain.clone()).expect("secrets");
    let new_account = account_with_every_slot(&new, &new_secrets, "new");
    assert_eq!(new_account, account_id);
    assert_every_slot(new_secrets.as_ref(), new_account, "new");
    remove_account(&new, new_secrets.as_ref(), None, new_account).expect("removed");
    let again = add_account(&new, &new_secrets, Adding::OAuth);
    assert_no_slot(new_secrets.as_ref(), new_account);
    remove_account(&new, new_secrets.as_ref(), None, again).expect("removed");
    let new_id = new.store_id().expect("an id");
    assert_entries_named_by_store(&keychain, &[new_id], false);
    for slot in SecretSlot::ALL {
        assert!(
            keychain.entry_names().contains(&old_name(account_id, slot)),
            "{slot:?} is still under its old name"
        );
    }

    // The store they belong to moves them all.
    let old_secrets = old.account_secrets(keychain.clone()).expect("secrets");
    assert_every_slot(old_secrets.as_ref(), account_id, "old build");
    assert!(keychain.entry_names().iter().all(|name| !is_old_name(name)));
    assert_entries_named_by_store(&keychain, &[new_id, old.store_id().expect("an id")], true);
}

/// A keychain that fails every access to the entries named in
/// `failing`, while `broken` is set: a locked keychain, or a crash.
struct FlakyKeychain {
    inner: Arc<InMemoryKeychain>,
    failing: Vec<String>,
    broken: AtomicBool,
}

impl FlakyKeychain {
    fn check(&self, name: &str) -> Result<()> {
        if self.broken.load(Ordering::SeqCst) && self.failing.iter().any(|failing| failing == name)
        {
            bail!("the keychain is locked");
        }
        Ok(())
    }
}

impl Keychain for FlakyKeychain {
    fn read_entry(&self, name: &str) -> Result<Option<String>> {
        self.check(name)?;
        self.inner.read_entry(name)
    }

    fn write_entry(&self, name: &str, secret: &str) -> Result<()> {
        self.check(name)?;
        self.inner.write_entry(name, secret)
    }

    fn delete_entry(&self, name: &str) -> Result<()> {
        self.check(name)?;
        self.inner.delete_entry(name)
    }
}

/// A move cut short (a crash, or a keychain that stopped answering part
/// way) loses nothing: every secret is under one name or the other, a
/// secret written under the new name since is kept, and the next start
/// finishes the move.
#[test]
fn an_interrupted_move_finishes_later_without_losing_a_secret() {
    let keychain = InMemoryKeychain::shared();
    let directory = TempDir::new("interrupted");
    let store = store_from_before_475(&directory);
    let account_id = store.list_accounts().expect("accounts")[0].id;
    let store_id = store.store_id().expect("an id");
    seed_old_names(&keychain, account_id, "old build");
    // A crash after copying the password but before deleting its old
    // entry; and a refresh token written under the new name since (the
    // old one is stale).
    keychain
        .write_entry(
            &zmail_security::keychain::account_entry_name(
                store_id,
                account_id,
                SecretSlot::Password,
            ),
            &secret("old build", SecretSlot::Password),
        )
        .expect("copied");
    keychain
        .write_entry(
            &zmail_security::keychain::account_entry_name(
                store_id,
                account_id,
                SecretSlot::RefreshToken,
            ),
            "newer refresh token",
        )
        .expect("written");
    keychain.clear_accesses();
    // And now the keychain refuses the old Proton key password.
    let flaky = Arc::new(FlakyKeychain {
        inner: keychain.clone(),
        failing: vec![old_name(account_id, SecretSlot::ProtonKeyPass)],
        broken: AtomicBool::new(true),
    });

    let secrets = store.account_secrets(flaky.clone()).expect("secrets");

    // What could move did; the slot that couldn't is still under its old
    // name, and reading it fails rather than finding nothing.
    for slot in SecretSlot::ALL {
        let found = secrets.get_account_secret_from_slot(account_id, slot);
        match slot {
            SecretSlot::ProtonKeyPass => assert!(found.is_err()),
            SecretSlot::RefreshToken => assert_eq!(found.expect("moved"), "newer refresh token"),
            slot => assert_eq!(found.expect("moved"), secret("old build", slot)),
        }
    }
    assert!(
        keychain
            .entry_names()
            .contains(&old_name(account_id, SecretSlot::ProtonKeyPass))
    );

    // The keychain answers again: the same session moves the slot when it
    // is read.
    flaky.broken.store(false, Ordering::SeqCst);
    assert_eq!(
        secrets
            .get_account_secret_from_slot(account_id, SecretSlot::ProtonKeyPass)
            .expect("moved now"),
        secret("old build", SecretSlot::ProtonKeyPass)
    );
    // The next start finishes and nothing is left under an old name.
    let again = store.account_secrets(flaky).expect("secrets");
    assert!(keychain.entry_names().iter().all(|name| !is_old_name(name)));
    assert_eq!(keychain.entry_names().len(), SecretSlot::ALL.len());
    for slot in SecretSlot::ALL {
        let found = again
            .get_account_secret_from_slot(account_id, slot)
            .expect("there");
        if slot == SecretSlot::RefreshToken {
            assert_eq!(found, "newer refresh token");
        } else {
            assert_eq!(found, secret("old build", slot));
        }
    }
    assert_entries_named_by_store(&keychain, &[store_id], true);
}

/// The guard itself: a name built without the store fails it.
#[test]
fn the_guard_catches_an_entry_named_without_its_store() {
    let keychain = InMemoryKeychain::shared();
    let store = LocalStore::open_ephemeral().expect("a store");
    let store_id = store.store_id().expect("an id");
    keychain
        .write_entry(
            &zmail_security::keychain::account_entry_name(store_id, 1, SecretSlot::Password),
            "fine",
        )
        .expect("written");
    assert_entries_named_by_store(&keychain, &[store_id], false);

    for unscoped in ["account-1", "account-1-refresh-token", "something-else"] {
        let keychain = InMemoryKeychain::shared();
        keychain.write_entry(unscoped, "x").expect("written");
        let caught = std::panic::catch_unwind(|| {
            assert_entries_named_by_store(&keychain, &[store_id], true);
        });
        assert!(caught.is_err(), "{unscoped} was not caught");
    }
    // Another store's name is caught too.
    let keychain = InMemoryKeychain::shared();
    keychain
        .write_entry(
            &zmail_security::keychain::account_entry_name(
                StoreId::generate(),
                1,
                SecretSlot::Password,
            ),
            "x",
        )
        .expect("written");
    assert!(
        std::panic::catch_unwind(|| {
            assert_entries_named_by_store(&keychain, &[store_id], false);
        })
        .is_err()
    );
}
