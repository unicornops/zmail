//! The unlock step before the workspace (ADR 0005): a quick unlock opens
//! no window, a locked keychain offers Try Again and never opens the store
//! unencrypted, and a lost key offers Start Over.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{TestAppContext, WindowHandle};
use zmail_security::InMemorySecretStore;
use zmail_security::vault::{KeyId, StoreKey};
use zmail_store::{LocalStore, MigrationReport, StoreKeychain};

use crate::store_unlock::{
    POLL_INTERVAL, StoreUnlockView, UnlockPhase, Unlocker, migration_notice, start,
};

/// A keychain that is locked until `unlock`.
#[derive(Default)]
struct Keychain {
    locked: AtomicBool,
    keys: InMemorySecretStore,
}

impl StoreKeychain for Keychain {
    fn save_store_key(&self, key: &StoreKey) -> anyhow::Result<()> {
        if self.locked.load(Ordering::SeqCst) {
            anyhow::bail!("the keychain is locked");
        }
        self.keys.save_store_key(key)
    }

    fn load_store_key(&self, id: KeyId) -> anyhow::Result<Option<StoreKey>> {
        if self.locked.load(Ordering::SeqCst) {
            anyhow::bail!("the keychain is locked");
        }
        self.keys.load_store_key(id)
    }

    fn delete_store_key(&self, id: KeyId) -> anyhow::Result<()> {
        self.keys.delete_store_key(id)
    }
}

/// What `on_unlocked` received.
type Opened = Rc<RefCell<Option<(Arc<LocalStore>, Option<String>)>>>;

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "zmail-unlock-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("the scratch dir is created");
    directory
}

/// Starts unlocking the store at `path`, recording what opens.
fn unlock(path: PathBuf, keychain: Arc<Keychain>, cx: &mut TestAppContext) -> Opened {
    let opened: Opened = Rc::default();
    let record = opened.clone();
    cx.update(|cx| {
        let unlocker = Unlocker::new(path, keychain, move |store, notice, _| {
            *record.borrow_mut() = Some((store, notice));
            Ok(())
        });
        start(unlocker, cx);
    });
    opened
}

/// Lets the background unlock and its follower run.
fn settle(cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
    }
}

/// The open unlock window, if any.
fn unlock_window(cx: &mut TestAppContext) -> Option<WindowHandle<StoreUnlockView>> {
    cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|window| window.downcast::<StoreUnlockView>())
    })
}

fn phase(handle: WindowHandle<StoreUnlockView>, cx: &mut TestAppContext) -> UnlockPhase {
    handle
        .read_with(cx, |view, _| view.phase().clone())
        .expect("the window is open")
}

#[gpui::test]
fn a_quick_unlock_opens_the_workspace_without_the_window(cx: &mut TestAppContext) {
    let directory = scratch("quick");
    let keychain = Arc::new(Keychain::default());
    let opened = unlock(directory.join("zmail.sqlite3"), keychain, cx);
    settle(cx);

    let (store, notice) = opened.borrow_mut().take().expect("the store opened");
    assert!(store.needs_onboarding().expect("the new store reads"));
    assert_eq!(notice, None, "a new store has nothing to report");
    assert!(unlock_window(cx).is_none(), "no window for a quick unlock");
    drop(store);
    let _ = std::fs::remove_dir_all(directory);
}

#[gpui::test]
fn a_locked_keychain_offers_try_again_and_opens_nothing(cx: &mut TestAppContext) {
    let directory = scratch("locked");
    let keychain = Arc::new(Keychain::default());
    keychain.locked.store(true, Ordering::SeqCst);
    let opened = unlock(directory.join("zmail.sqlite3"), keychain.clone(), cx);
    settle(cx);

    assert!(opened.borrow().is_none(), "nothing opens without the key");
    let handle = unlock_window(cx).expect("the window explains");
    assert!(matches!(
        phase(handle, cx),
        UnlockPhase::KeychainUnavailable(detail) if detail.contains("locked")
    ));
    assert!(
        !directory.join("zmail.sqlite3").exists(),
        "no store is created without its key"
    );

    keychain.locked.store(false, Ordering::SeqCst);
    handle
        .update(cx, |view, _, cx| view.retry(cx))
        .expect("the window is open");
    settle(cx);
    assert!(opened.borrow().is_some(), "Try Again opens the store");
    assert!(unlock_window(cx).is_none(), "the window closes");
    let _ = std::fs::remove_dir_all(directory);
}

#[gpui::test]
fn a_lost_key_offers_start_over(cx: &mut TestAppContext) {
    let directory = scratch("lost");
    let path = directory.join("zmail.sqlite3");
    {
        let keychain = InMemorySecretStore::default();
        let (store, _) = LocalStore::unlock(&path, &keychain, &|_| {}).expect("the store opens");
        store
            .create_demo_account(zmail_core::ProtocolKind::Imap)
            .expect("an account");
    }

    // Another keychain: the store's key is not in it.
    let opened = unlock(path.clone(), Arc::new(Keychain::default()), cx);
    settle(cx);
    assert!(opened.borrow().is_none());
    let handle = unlock_window(cx).expect("the window explains");
    assert_eq!(phase(handle, cx), UnlockPhase::Unreadable);

    handle
        .update(cx, |view, _, cx| view.start_over(cx))
        .expect("the window is open");
    settle(cx);
    let (store, notice) = opened.borrow_mut().take().expect("a new store opens");
    assert!(store.needs_onboarding().expect("the new store reads"));
    let notice = notice.expect("the workspace says what happened");
    assert!(notice.contains("moved to"), "{notice}");
    let aside: Vec<_> = std::fs::read_dir(&directory)
        .expect("listed")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("unreadable-")
        })
        .collect();
    assert_eq!(aside.len(), 1, "the old store is kept aside");
    assert!(aside[0].path().join("zmail.sqlite3").exists());
    drop(store);
    let _ = std::fs::remove_dir_all(directory);
}

#[gpui::test]
fn every_phase_shows_its_actions(cx: &mut TestAppContext) {
    for (phase, buttons) in [
        (UnlockPhase::Working(None), &[][..]),
        (
            UnlockPhase::KeychainUnavailable("locked".to_owned()),
            &["store-unlock-quit", "store-unlock-retry"][..],
        ),
        (
            UnlockPhase::Unreadable,
            &["store-unlock-quit", "store-unlock-start-over"][..],
        ),
        (
            UnlockPhase::Failed("disk full".to_owned()),
            &["store-unlock-quit", "store-unlock-retry"][..],
        ),
    ] {
        let window = cx.add_window(|window, cx| StoreUnlockView::new(phase, None, window, cx));
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        for button in buttons {
            assert!(cx.debug_bounds(button).is_some(), "{button} shows");
        }
        let progress = cx.debug_bounds("store-unlock-progress").is_some();
        assert_eq!(
            progress,
            buttons.is_empty(),
            "only the work in progress has a bar"
        );
    }
}

#[test]
fn the_migration_notice_names_old_backups() {
    assert_eq!(
        migration_notice(&MigrationReport::default()),
        "Your mail store is now encrypted."
    );
    let notice = migration_notice(&MigrationReport {
        plaintext_backups: vec![PathBuf::from("/data/backups/zmail-backup-1.sqlite3")],
        encrypted_attachments: 3,
    });
    assert!(
        notice.contains("still unencrypted, in /data/backups"),
        "{notice}"
    );
}
