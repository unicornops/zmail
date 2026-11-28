//! Signing in to a Proton account again from Setup → Accounts → Change
//! (issue #465), whether Proton has ended the session or it still works:
//! the account keeps its id, its mail and the place its sync had got to,
//! the new session takes the old one's keychain slots through the sync
//! service's replacement (which signs the old one out first), and a
//! sign-in as another Proton account is refused.
//!
//! Proton is faked at the same seam as the desktop app's view tests:
//! scripted sign-in steps, as `zmail-setup`'s Proton tests use, and a
//! Proton side that records what it was asked to do.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use crossterm::event::KeyCode;
use zmail_core::{
    AccountSyncSnapshot, AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind,
    time::HourCycle,
};
use zmail_proton::{SignInStep, SignedInAccount};
use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
use zmail_setup::proton::{BoxFuture, ProtonSide, ProtonSteps, SignInSteps};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use super::{
    assert_printable, assert_snapshot, draw, environment, key, keys, mailbox, now, text_mail,
    text_of, thread,
};
use crate::{
    accounts::Step,
    app::{App, AppOptions, Screen},
};

/// The password typed, which must never be drawn.
const PASSWORD: &str = "hunter2-proton-password";

/// Where the account's sync had got to.
const CHECKPOINT: &str = r#"{"event_id":"event-1"}"#;

/// Proton, as far as the terminal UI sees it: the sign-in's answers in
/// turn, and everything it was asked to do, in order.
#[derive(Default)]
struct FakeProton {
    answers: Mutex<Vec<Result<SignInStep, String>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeProton {
    fn answer(&self, answers: Vec<Result<SignInStep, String>>) {
        *self.answers.lock().expect("answers") = answers;
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("calls").push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    fn next(&self, call: String) -> BoxFuture<Result<SignInStep>> {
        self.record(call);
        let answer = self.answers.lock().expect("answers").remove(0);
        Box::pin(async move { answer.map_err(|error| anyhow!(error)) })
    }
}

/// A sign-in on the fake Proton: each step takes the next answer, as
/// `zmail-setup`'s scripted sign-in does.
#[derive(Clone)]
struct Script(Arc<FakeProton>);

impl SignInSteps for Script {
    fn login(&self) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        self.0.next("login".to_owned())
    }

    fn submit_totp(
        &self,
        code: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        self.0.next(format!("totp {code}"))
    }

    fn submit_mailbox_password(
        &self,
        _password: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        self.0.next("mailbox password".to_owned())
    }

    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> impl Future<Output = Result<()>> + Send + 'static + use<> {
        let fake = self.0.clone();
        async move {
            fake.record(format!("store {account_id}"));
            for (slot, secret) in [
                (SecretSlot::ProtonUid, "uid-new"),
                (SecretSlot::ProtonRefreshToken, "refresh-new"),
                (SecretSlot::ProtonKeyPass, "key-pass-new"),
            ] {
                secrets.set_account_secret_in_slot(account_id, slot, secret)?;
            }
            Ok(())
        }
    }

    fn abort(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.0.record("abort".to_owned());
        async {}
    }
}

/// The fake Proton side: its sign-ins are scripts too.
impl ProtonSide for Script {
    fn begin_sign_in(&self, username: &str, _password: &str) -> Result<ProtonSteps> {
        self.0.record(format!("sign in as {username}"));
        Ok(ProtonSteps::new(self.clone()))
    }

    fn replace_session(
        &self,
        account_id: i64,
        store: BoxFuture<Result<()>>,
    ) -> BoxFuture<Result<()>> {
        let fake = self.0.clone();
        Box::pin(async move {
            fake.record(format!("sign out the session of {account_id}"));
            let stored = store.await;
            fake.record(format!("replaced the session of {account_id}"));
            stored
        })
    }

    fn signed_in_again(&self, account_id: i64) {
        self.0.record(format!("sync {account_id}"));
    }
}

fn signed_in(email: &str) -> Result<SignInStep, String> {
    Ok(SignInStep::SignedIn(SignedInAccount {
        email: email.to_owned(),
        display_name: "Rob".to_owned(),
    }))
}

/// A runtime shared by these tests, kept for the process.
fn runtime() -> tokio::runtime::Handle {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("a runtime")
        })
        .handle()
        .clone()
}

/// A Proton account that has synced, with its mail and its session in the
/// keychain, on the Change screen.
struct Fixture {
    app: App,
    proton: Arc<FakeProton>,
    secrets: Arc<dyn SecretStore>,
    store: Arc<LocalStore>,
    account_id: i64,
}

impl Fixture {
    /// `ended`: Proton has ended the session, and the account says so.
    fn new(ended: bool) -> Self {
        let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "rob@proton.test".to_owned(),
                protocol: ProtocolKind::Proton,
                auth: AuthKind::ProtonSession,
                endpoint: "https://mail-api.proton.me".to_owned(),
                username: "rob".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "Rob".to_owned(),
                    email_address: "rob@proton.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("stored");
        store
            .replace_account_snapshot(
                account_id,
                &AccountSyncSnapshot {
                    checkpoint: CHECKPOINT.to_owned(),
                    detail: "Proton account up to date".to_owned(),
                    identities: None,
                    mailboxes: vec![mailbox(
                        "Inbox",
                        Some("inbox"),
                        vec![thread(
                            "report",
                            "Quarterly report",
                            false,
                            &[text_mail(
                                "Kai Santos <kai@proton.test>",
                                30,
                                false,
                                "Numbers attached",
                            )],
                        )],
                    )],
                },
            )
            .expect("the mail is stored");
        store
            .record_sync_success(account_id, "Proton account up to date", CHECKPOINT)
            .expect("synced");
        if ended {
            store
                .record_sync_failure(
                    account_id,
                    "Sync failed after retry",
                    format!("the Proton session for account {account_id} has ended; sign in again"),
                )
                .expect("the failure is recorded");
        }
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        for (slot, secret) in [
            (SecretSlot::ProtonUid, "uid-old"),
            (SecretSlot::ProtonRefreshToken, "refresh-old"),
            (SecretSlot::ProtonKeyPass, "key-pass-old"),
        ] {
            secrets
                .set_account_secret_in_slot(account_id, slot, secret)
                .expect("the old session is stored");
        }
        let sync = SyncService::new(runtime(), store.clone(), secrets.clone());
        let mut app = App::new(AppOptions {
            store: store.clone(),
            sync: Some(sync),
            runtime: Some(runtime()),
            env: environment(),
            clock: Arc::new(now),
            hour_cycle: HourCycle::H24,
            version: "0.0.0-test".to_owned(),
            arrivals: None,
            secrets: Some(secrets.clone()),
            network: None,
        });
        let proton = Arc::new(FakeProton::default());
        app.proton_backend = Some(Arc::new(Script(proton.clone())));
        // Setup → Accounts → Change.
        keys(&mut app, "sa");
        assert_eq!(app.screen, Screen::Accounts);
        keys(&mut app, "c");
        assert_eq!(app.screen, Screen::EditAccount);
        Self {
            app,
            proton,
            secrets,
            store,
            account_id,
        }
    }

    /// The account's keychain slot.
    fn slot(&self, slot: SecretSlot) -> Option<String> {
        self.secrets
            .get_account_secret_from_slot(self.account_id, slot)
            .ok()
            .map(|secret| secret.as_str().to_owned())
    }

    fn session(&self) -> [Option<String>; 3] {
        [
            self.slot(SecretSlot::ProtonUid),
            self.slot(SecretSlot::ProtonRefreshToken),
            self.slot(SecretSlot::ProtonKeyPass),
        ]
    }

    /// The account's conversations.
    fn threads(&self) -> Vec<i64> {
        let inbox = self
            .store
            .mailbox_id_by_role(self.account_id, "inbox")
            .expect("the lookup works")
            .expect("an inbox");
        self.store
            .list_threads(inbox, 0..10)
            .expect("the threads load")
            .into_iter()
            .map(|thread| thread.id)
            .collect()
    }

    /// The account is the one it was, with its mail, and syncs on from
    /// where it had got to: nothing downloads again.
    fn assert_kept(&self, threads: &[i64]) {
        let accounts = self.store.list_accounts().expect("accounts");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            [self.account_id],
            "no account is added"
        );
        assert_eq!(self.threads(), threads);
        let target = self
            .store
            .sync_target(self.account_id)
            .expect("loads")
            .expect("the account");
        assert_eq!(target.checkpoint.as_deref(), Some(CHECKPOINT));
    }

    fn signing_in_again(&self) -> Option<(i64, bool)> {
        self.app
            .add
            .as_ref()
            .and_then(|add| add.again.as_ref())
            .map(|again| (again.account_id, again.ended))
    }
}

fn old_session() -> [Option<String>; 3] {
    ["uid-old", "refresh-old", "key-pass-old"].map(|secret| Some(secret.to_owned()))
}

fn new_session() -> [Option<String>; 3] {
    ["uid-new", "refresh-new", "key-pass-new"].map(|secret| Some(secret.to_owned()))
}

/// Ticks until `done`, or fails after a few seconds.
fn wait_for(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        app.tick();
    }
}

/// The screen as text, checked for anything unprintable and the password.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let buffer = draw(app, width, height);
    assert_printable(&buffer);
    let text = text_of(&buffer);
    assert!(!text.contains(PASSWORD), "the password is drawn:\n{text}");
    text
}

fn proton_error(app: &App) -> Option<String> {
    app.add.as_ref().and_then(|add| add.proton.error.clone())
}

#[test]
fn signing_in_to_proton_again_after_the_session_ended_keeps_the_account_and_its_mail() {
    let mut fixture = Fixture::new(true);
    let account_id = fixture.account_id;
    let threads = fixture.threads();
    assert_eq!(threads.len(), 1);

    // Change's first row signs in to Proton again, for this account.
    let text = screen(&mut fixture.app, 80, 24);
    assert!(text.contains("Sign in to Proton again"), "{text}");
    key(&mut fixture.app, KeyCode::Enter);
    assert_eq!(
        fixture.app.screen,
        Screen::AddAccount,
        "{:?}",
        fixture.app.status
    );
    assert_eq!(fixture.app.add_step(), Some(Step::ProtonSignIn));
    assert_eq!(fixture.signing_in_again(), Some((account_id, true)));
    let text = screen(&mut fixture.app, 80, 24);
    assert!(text.contains("Sign in to Proton Mail again"), "{text}");
    assert!(
        text.contains("Proton ended Zmail's session for rob@proton.test."),
        "{text}"
    );
    assert!(
        text.contains("signs in as Bridge"),
        "the risk note:\n{text}"
    );

    // The address is the account's own and can't change: what is typed
    // is the password, wherever the cursor is.
    key(&mut fixture.app, KeyCode::Up);
    keys(&mut fixture.app, PASSWORD);
    key(&mut fixture.app, KeyCode::Backspace);
    keys(&mut fixture.app, &PASSWORD[PASSWORD.len() - 1..]);
    let add = fixture.app.add.as_ref().expect("signing in");
    assert_eq!(add.form.email, "rob");
    assert_eq!(add.form.password, PASSWORD);
    let text = screen(&mut fixture.app, 80, 24);
    assert!(text.contains("Email address : rob"), "{text}");
    assert!(text.contains("Password : [hidden]"), "{text}");
    assert_snapshot(
        "add-proton-again-ended-80x24",
        &draw(&mut fixture.app, 80, 24),
    );

    // Signed in as another Proton account: refused, back at the password,
    // with that sign-in signed out and the old session left alone.
    fixture.proton.answer(vec![
        signed_in("someone.else@proton.test"),
        Ok(SignInStep::Totp),
        signed_in("rob@proton.test"),
    ]);
    key(&mut fixture.app, KeyCode::Enter);
    wait_for(&mut fixture.app, "the refusal", |app| {
        proton_error(app).is_some()
    });
    let error = proton_error(&fixture.app).expect("an error");
    assert!(
        error
            .starts_with("You signed in as someone.else@proton.test, which isn't rob@proton.test."),
        "{error}"
    );
    assert_eq!(fixture.app.add_step(), Some(Step::ProtonSignIn));
    assert_eq!(fixture.proton.calls(), ["sign in as rob", "login", "abort"]);
    assert_eq!(fixture.session(), old_session());
    let text = screen(&mut fixture.app, 80, 24);
    assert!(text.contains("which isn't rob@proton.test"), "{text}");

    // Signed in as that account, after its two-factor code: the new
    // session takes the old one's place, and the account syncs.
    key(&mut fixture.app, KeyCode::Enter);
    wait_for(&mut fixture.app, "the code step", |app| {
        app.add_step() == Some(Step::ProtonTotp)
    });
    keys(&mut fixture.app, "123456");
    key(&mut fixture.app, KeyCode::Enter);
    wait_for(&mut fixture.app, "the sign-in", |app| app.add.is_none());
    assert_eq!(
        fixture.app.status.as_deref(),
        Some("Signed in to Proton again. rob@proton.test is syncing.")
    );
    assert_eq!(
        fixture.proton.calls(),
        [
            "sign in as rob".to_owned(),
            "login".to_owned(),
            "abort".to_owned(),
            "sign in as rob".to_owned(),
            "login".to_owned(),
            "totp 123456".to_owned(),
            format!("sign out the session of {account_id}"),
            format!("store {account_id}"),
            format!("replaced the session of {account_id}"),
            format!("sync {account_id}"),
        ]
    );
    assert_eq!(fixture.session(), new_session());
    fixture.assert_kept(&threads);
    // Back on the account's Change screen.
    assert_eq!(fixture.app.screen, Screen::EditAccount);
    assert_eq!(
        fixture.app.edit.as_ref().map(|edit| edit.account.id),
        Some(account_id)
    );
}

#[test]
fn signing_in_to_proton_again_while_the_session_works_replaces_it() {
    let mut fixture = Fixture::new(false);
    let account_id = fixture.account_id;
    let threads = fixture.threads();

    // The session works: Change still signs in again, and says what
    // happens to the session in use.
    key(&mut fixture.app, KeyCode::Enter);
    assert_eq!(
        fixture.app.screen,
        Screen::AddAccount,
        "{:?}",
        fixture.app.status
    );
    assert_eq!(fixture.signing_in_again(), Some((account_id, false)));
    let text = screen(&mut fixture.app, 80, 24);
    assert!(
        text.contains("Once you have signed in, Zmail signs out of the session it has been using"),
        "{text}"
    );
    assert!(!text.contains("Proton ended"), "{text}");
    assert_snapshot("add-proton-again-80x24", &draw(&mut fixture.app, 80, 24));

    // Esc leaves it as it was, back on Change.
    key(&mut fixture.app, KeyCode::Esc);
    assert_eq!(fixture.app.screen, Screen::EditAccount);
    assert!(fixture.app.add.is_none());
    assert!(fixture.proton.calls().is_empty());
    assert_eq!(fixture.session(), old_session());
    assert_snapshot(
        "account-change-proton-80x24",
        &draw(&mut fixture.app, 80, 24),
    );

    // Signed in: the session in use is signed out, then the new one is
    // kept in its slots, and the account syncs.
    key(&mut fixture.app, KeyCode::Enter);
    keys(&mut fixture.app, PASSWORD);
    fixture.proton.answer(vec![signed_in("ROB@proton.test")]);
    key(&mut fixture.app, KeyCode::Enter);
    wait_for(&mut fixture.app, "the sign-in", |app| app.add.is_none());
    assert_eq!(
        fixture.app.status.as_deref(),
        Some("Signed in to Proton again. rob@proton.test is syncing.")
    );
    assert_eq!(
        fixture.proton.calls(),
        [
            "sign in as rob".to_owned(),
            "login".to_owned(),
            format!("sign out the session of {account_id}"),
            format!("store {account_id}"),
            format!("replaced the session of {account_id}"),
            format!("sync {account_id}"),
        ]
    );
    assert_eq!(fixture.session(), new_session());
    fixture.assert_kept(&threads);
    let text = screen(&mut fixture.app, 80, 24);
    assert!(!text.contains(PASSWORD), "{text}");
}
