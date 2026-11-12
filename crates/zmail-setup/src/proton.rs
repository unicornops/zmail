//! Adding a Proton Mail account: the sign-in's steps, without the UI
//! (the `proton` feature, matching `zmail-ui`'s and `zmail-tui`'s).
//!
//! Proton is not a server Zmail connects to with a password; Zmail signs
//! in to Proton's API the way Proton Mail Bridge does
//! (`zmail_proton::ProtonSignIn`, from `SyncService::begin_proton_sign_in`,
//! on the client configuration the app set up: Bridge's app version and a
//! User-Agent naming Zmail, ADR 0004). This module never builds a client
//! or chooses an identity of its own. The steps:
//!
//! 1. **Sign in:** address and password, under an honest note about what
//!    a third-party client on Proton's private API means ([`RISK_NOTE`]).
//! 2. **Verification**, when Proton asks for it: the frontend opens
//!    verify.proton.me in the browser, and signs in again once the user has
//!    finished there. Zmail never solves the check itself.
//! 3. **Two-factor code**, when the account has an authenticator app.
//! 4. **Mailbox password**, for accounts with a second password.
//!
//! [`ProtonFlow`] keeps where the sign-in is and turns each answer into
//! what the frontend does next ([`ProtonAction`]); the frontend runs the
//! steps it hands out on the sync runtime (`SyncService::run_proton`).
//! The account is stored once signed in, with the session (never the
//! password) in the keychain ([`store_signed_in`]).

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use zmail_core::{AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind};
use zmail_proton::{ProtonSignIn, SignInStep, SignedInAccount};
use zmail_security::SecretStore;
use zmail_store::LocalStore;

use crate::form::AccountField;

/// What a third-party Proton client means, shown on the sign-in step.
pub const RISK_NOTE: &str = "Zmail is an independent client, not made or supported by Proton. It talks to Proton's private API the way Proton Mail Bridge does and signs in as Bridge, because Proton doesn't accept other clients, so your Proton sessions list shows it as Proton Mail Bridge. Proton may change or block it at any time, and only allows it on paid plans. Zmail keeps a session token and the key that unlocks your mail in the system keychain, never your password.";

/// The risk note's title.
pub const RISK_TITLE: &str = "A third-party client for Proton";

/// What the verification step asks.
pub const VERIFICATION_DETAIL: &str = "Proton wants to check that a person is signing in. Finish the check on Proton's page in your browser, then continue here.";

/// What the verification step promises.
pub const VERIFICATION_CAPTION: &str =
    "Zmail doesn't see or solve the check; it only signs in again once you've finished.";

/// A step of the Proton sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtonStage {
    /// Address and password, with the risk note.
    SignIn,
    /// Human verification in the browser.
    Verification,
    /// The two-factor code.
    Totp,
    /// The mailbox password.
    MailboxPassword,
}

/// What the frontend does with a step's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtonAction {
    /// Open Proton's verification page in the browser and show the
    /// verification step.
    Verify {
        /// The verify.proton.me page (from Proton, untrusted text).
        url: String,
    },
    /// Show the two-factor step, with an empty field.
    AskTotp,
    /// Show the mailbox password step, with an empty field.
    AskMailboxPassword,
    /// Signed in: store the account ([`store_signed_in`]).
    SignedIn(SignedInAccount),
}

/// The sign-in's steps as [`ProtonFlow`] runs them: `ProtonSignIn`'s, and
/// fakes in tests. Clones share the sign-in.
pub trait SignInSteps: Clone + Send + Sync + 'static {
    /// Signs in with the username and password (again, after
    /// verification).
    fn login(&self) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<Self>;
    /// Sends the two-factor code.
    fn submit_totp(
        &self,
        code: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<Self>;
    /// Sends the mailbox password.
    fn submit_mailbox_password(
        &self,
        password: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<Self>;
    /// Stores the finished session in the keychain for `account_id`.
    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> impl Future<Output = Result<()>> + Send + 'static + use<Self>;
    /// Signs an unfinished session out.
    fn abort(&self) -> impl Future<Output = ()> + Send + 'static + use<Self>;
}

impl SignInSteps for ProtonSignIn {
    fn login(&self) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        let sign_in = self.clone();
        async move { sign_in.login().await }
    }

    fn submit_totp(
        &self,
        code: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        let sign_in = self.clone();
        async move { sign_in.submit_totp(&code).await }
    }

    fn submit_mailbox_password(
        &self,
        password: String,
    ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
        let sign_in = self.clone();
        // `ProtonSignIn` keeps its own copy in a zeroizing buffer.
        async move { sign_in.submit_mailbox_password(&password).await }
    }

    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> impl Future<Output = Result<()>> + Send + 'static + use<> {
        let sign_in = self.clone();
        async move { sign_in.store(secrets.as_ref(), account_id).await }
    }

    fn abort(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let sign_in = self.clone();
        async move { sign_in.abort().await }
    }
}

/// The Proton sign-in in progress.
#[derive(Debug)]
pub struct ProtonFlow<S = ProtonSignIn> {
    sign_in: Option<S>,
    /// The step shown.
    pub stage: ProtonStage,
    /// The verification page Proton asked the user to visit.
    pub verify_url: Option<String>,
    /// A step is running.
    pub busy: bool,
    /// Why the last step failed (it may quote Proton: untrusted text).
    pub error: Option<String>,
}

impl<S> Default for ProtonFlow<S> {
    fn default() -> Self {
        Self {
            sign_in: None,
            stage: ProtonStage::SignIn,
            verify_url: None,
            busy: false,
            error: None,
        }
    }
}

/// Checks the sign-in step's fields.
pub fn check_sign_in(email: &str, password: &str) -> Vec<(AccountField, String)> {
    let mut errors = Vec::new();
    if email.trim().is_empty() {
        errors.push((
            AccountField::Email,
            "Enter your Proton address or username.".to_owned(),
        ));
    }
    if password.is_empty() {
        errors.push((
            AccountField::Password,
            "Enter your Proton password.".to_owned(),
        ));
    }
    errors
}

/// Why `email` can't be added: it is already in Zmail.
pub fn duplicate(store: &LocalStore, email: &str) -> Option<String> {
    let email = email.trim();
    let duplicate = store.list_accounts().ok().is_some_and(|accounts| {
        accounts.iter().any(|account| {
            account.protocol == ProtocolKind::Proton && account.username.eq_ignore_ascii_case(email)
        })
    });
    duplicate.then(|| format!("{email} is already in Zmail."))
}

impl<S: SignInSteps> ProtonFlow<S> {
    /// Starts signing in with a new sign-in: the first step to run. Call
    /// [`Self::abort`] first when an unfinished one may be in progress.
    pub fn start(
        &mut self,
        sign_in: S,
    ) -> impl Future<Output = Result<SignInStep>> + Send + use<S> {
        self.sign_in = Some(sign_in.clone());
        self.begin_step();
        sign_in.login()
    }

    /// The current step's Continue with what was typed (nothing for the
    /// verification step): the step to run, or the field's error. `None`
    /// when there is nothing to run (a step is running, or the sign-in
    /// step, which starts with [`Self::start`]).
    #[allow(clippy::type_complexity)]
    pub fn submit(
        &mut self,
        input: &str,
    ) -> Result<
        Option<std::pin::Pin<Box<dyn Future<Output = Result<SignInStep>> + Send + 'static>>>,
        (AccountField, String),
    > {
        if self.busy {
            return Ok(None);
        }
        let Some(sign_in) = self.sign_in.clone() else {
            return Ok(None);
        };
        let step: std::pin::Pin<Box<dyn Future<Output = Result<SignInStep>> + Send>> =
            match self.stage {
                ProtonStage::SignIn => return Ok(None),
                ProtonStage::Verification => Box::pin(sign_in.login()),
                ProtonStage::Totp => {
                    if input.trim().is_empty() {
                        return Err((
                            AccountField::ProtonTotp,
                            "Enter the code from your authenticator app.".to_owned(),
                        ));
                    }
                    Box::pin(sign_in.submit_totp(input.to_owned()))
                }
                ProtonStage::MailboxPassword => {
                    if input.is_empty() {
                        return Err((
                            AccountField::ProtonMailboxPassword,
                            "Enter your mailbox password.".to_owned(),
                        ));
                    }
                    Box::pin(sign_in.submit_mailbox_password(input.to_owned()))
                }
            };
        self.begin_step();
        Ok(Some(step))
    }

    /// Marks a step as running.
    fn begin_step(&mut self) {
        self.busy = true;
        self.error = None;
    }

    /// A step's answer: what to show next, or (on an error) nothing, with
    /// the error kept for the current step.
    pub fn finish(&mut self, result: Result<SignInStep>) -> Option<ProtonAction> {
        self.busy = false;
        let step = match result {
            Ok(step) => step,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                return None;
            }
        };
        Some(match step {
            SignInStep::HumanVerification { url } => {
                self.verify_url = Some(url.clone());
                self.stage = ProtonStage::Verification;
                ProtonAction::Verify { url }
            }
            SignInStep::Totp => {
                self.stage = ProtonStage::Totp;
                ProtonAction::AskTotp
            }
            SignInStep::MailboxPassword => {
                self.stage = ProtonStage::MailboxPassword;
                ProtonAction::AskMailboxPassword
            }
            SignInStep::SignedIn(account) => ProtonAction::SignedIn(account),
        })
    }

    /// The sign-in, while one is in progress.
    pub fn sign_in(&self) -> Option<&S> {
        self.sign_in.as_ref()
    }

    /// Back to the sign-in step, keeping nothing of the unfinished
    /// session: its sign-out to run, if there was one.
    pub fn abort(&mut self) -> Option<impl Future<Output = ()> + Send + use<S>> {
        self.busy = false;
        self.verify_url = None;
        self.stage = ProtonStage::SignIn;
        self.sign_in.take().map(|sign_in| sign_in.abort())
    }

    /// The sign-in finished and its account is stored: forget it.
    pub fn done(&mut self) {
        self.sign_in = None;
        self.busy = false;
    }
}

/// The account a Proton sign-in adds: the identities come from Proton with
/// the first sync.
pub fn proton_account(username: &str, account: &SignedInAccount) -> NewAccount {
    let display_name = if account.display_name.is_empty() {
        account.email.clone()
    } else {
        account.display_name.clone()
    };
    NewAccount {
        display_name: account.email.clone(),
        protocol: ProtocolKind::Proton,
        auth: AuthKind::ProtonSession,
        endpoint: zmail_proton::API_HOST_URL.to_owned(),
        username: username.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: None,
        identities: vec![NewIdentity {
            display_name,
            email_address: account.email.clone(),
            signature: String::new(),
            is_primary: true,
        }],
    }
}

/// Stores a signed-in account: the account, then its session in the
/// keychain (removing the account again if that fails). Returns the new
/// account's id and display name; the caller starts its first sync.
pub fn store_signed_in<S: SignInSteps>(
    store: &Arc<LocalStore>,
    secrets: &Arc<dyn SecretStore>,
    sign_in: &S,
    username: &str,
    account: &SignedInAccount,
) -> Result<(
    NewAccount,
    i64,
    impl Future<Output = Result<()>> + Send + use<S>,
)> {
    let new_account = proton_account(username.trim(), account);
    let account_id = store.insert_account(&new_account)?;
    crate::accounts::forget_stale_secrets(store, secrets.as_ref(), account_id)?;
    let store = store.clone();
    let session = sign_in.store(secrets.clone(), account_id);
    let stored = async move {
        if let Err(error) = session.await {
            let _ = store.delete_account(account_id);
            return Err(error.context("Could not keep the Proton session"));
        }
        Ok(())
    };
    Ok((new_account, account_id, stored))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, anyhow};
    use zmail_core::{AuthKind, ProtocolKind};
    use zmail_proton::{SignInStep, SignedInAccount};
    use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
    use zmail_store::LocalStore;

    use super::{
        ProtonAction, ProtonFlow, ProtonStage, RISK_NOTE, SignInSteps, check_sign_in, duplicate,
        proton_account, store_signed_in,
    };
    use crate::form::AccountField;

    /// A scripted sign-in: each call takes the next answer, and records
    /// what was sent.
    #[derive(Clone, Default)]
    struct Script {
        answers: Arc<Mutex<Vec<Result<SignInStep, String>>>>,
        sent: Arc<Mutex<Vec<String>>>,
        store_fails: bool,
    }

    impl Script {
        fn new(answers: Vec<Result<SignInStep, String>>) -> Self {
            Self {
                answers: Arc::new(Mutex::new(answers)),
                ..Self::default()
            }
        }

        fn answer(
            &self,
            sent: String,
        ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
            self.sent.lock().expect("lock").push(sent);
            let answer = self.answers.lock().expect("lock").remove(0);
            async move { answer.map_err(|error| anyhow!(error)) }
        }
    }

    use std::future::Future;

    impl SignInSteps for Script {
        fn login(&self) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
            self.answer("login".to_owned())
        }

        fn submit_totp(
            &self,
            code: String,
        ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
            self.answer(format!("totp {code}"))
        }

        fn submit_mailbox_password(
            &self,
            _password: String,
        ) -> impl Future<Output = Result<SignInStep>> + Send + 'static + use<> {
            self.answer("mailbox password".to_owned())
        }

        fn store(
            &self,
            secrets: Arc<dyn SecretStore>,
            account_id: i64,
        ) -> impl Future<Output = Result<()>> + Send + 'static + use<> {
            let fails = self.store_fails;
            async move {
                if fails {
                    return Err(anyhow!("the keychain is locked"));
                }
                secrets.set_account_secret_in_slot(account_id, SecretSlot::ProtonUid, "uid")
            }
        }

        fn abort(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
            self.sent.lock().expect("lock").push("abort".to_owned());
            async {}
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    fn signed_in() -> SignedInAccount {
        SignedInAccount {
            email: "zmail@proton.me".to_owned(),
            display_name: "Zmail".to_owned(),
        }
    }

    #[test]
    fn verification_then_totp_then_the_mailbox_password() {
        let script = Script::new(vec![
            Ok(SignInStep::HumanVerification {
                url: "https://verify.proton.me/?token".to_owned(),
            }),
            Ok(SignInStep::Totp),
            Err("Proton did not accept that code".to_owned()),
            Ok(SignInStep::MailboxPassword),
            Ok(SignInStep::SignedIn(signed_in())),
        ]);
        let mut flow = ProtonFlow::<Script>::default();
        let first = flow.start(script.clone());
        assert!(flow.busy);
        assert_eq!(
            flow.finish(block_on(first)),
            Some(ProtonAction::Verify {
                url: "https://verify.proton.me/?token".to_owned()
            })
        );
        assert_eq!(flow.stage, ProtonStage::Verification);
        assert!(flow.verify_url.is_some());

        // "I've finished": sign in again.
        let again = flow.submit("").expect("no field").expect("a step");
        assert_eq!(flow.finish(block_on(again)), Some(ProtonAction::AskTotp));
        assert_eq!(flow.stage, ProtonStage::Totp);

        // An empty code is refused before Proton sees it.
        assert_eq!(
            flow.submit("  ").err().map(|(field, _)| field),
            Some(AccountField::ProtonTotp)
        );
        // A wrong code stays on the step with Proton's answer.
        let wrong = flow.submit("000000").expect("a code").expect("a step");
        assert_eq!(flow.finish(block_on(wrong)), None);
        assert_eq!(flow.stage, ProtonStage::Totp);
        assert!(
            flow.error
                .as_deref()
                .is_some_and(|error| error.contains("code"))
        );

        let right = flow.submit("123456").expect("a code").expect("a step");
        assert!(flow.error.is_none(), "a new step clears the error");
        assert_eq!(
            flow.finish(block_on(right)),
            Some(ProtonAction::AskMailboxPassword)
        );
        assert_eq!(
            flow.submit("").err().map(|(field, _)| field),
            Some(AccountField::ProtonMailboxPassword)
        );
        let unlock = flow.submit("second").expect("a password").expect("a step");
        assert_eq!(
            flow.finish(block_on(unlock)),
            Some(ProtonAction::SignedIn(signed_in()))
        );
        assert_eq!(
            *script.sent.lock().expect("lock"),
            [
                "login",
                "login",
                "totp 000000",
                "totp 123456",
                "mailbox password"
            ],
            "the mailbox password itself is never recorded"
        );
    }

    #[test]
    fn going_back_signs_the_unfinished_session_out() {
        let script = Script::new(vec![Ok(SignInStep::Totp)]);
        let mut flow = ProtonFlow::<Script>::default();
        let first = flow.start(script.clone());
        flow.finish(block_on(first));
        let abort = flow.abort().expect("a session to sign out");
        block_on(abort);
        assert_eq!(flow.stage, ProtonStage::SignIn);
        assert!(flow.sign_in().is_none());
        assert_eq!(
            script.sent.lock().expect("lock").last().map(String::as_str),
            Some("abort")
        );
        assert!(flow.abort().is_none(), "nothing left to sign out");
    }

    #[test]
    fn a_running_step_takes_no_second_continue() {
        let script = Script::new(vec![Ok(SignInStep::Totp)]);
        let mut flow = ProtonFlow::<Script>::default();
        let _first = flow.start(script);
        assert!(flow.submit("123456").expect("no error").is_none());
    }

    #[test]
    fn the_sign_in_fields_and_duplicates_are_checked() {
        let fields: Vec<_> = check_sign_in(" ", "")
            .into_iter()
            .map(|(field, _)| field)
            .collect();
        assert_eq!(fields, [AccountField::Email, AccountField::Password]);
        assert!(check_sign_in("zmail", "secret").is_empty());

        let store = LocalStore::open_ephemeral().expect("a store");
        assert!(duplicate(&store, "zmail").is_none());
        store
            .insert_account(&proton_account("zmail", &signed_in()))
            .expect("stored");
        assert_eq!(
            duplicate(&store, " ZMAIL ").as_deref(),
            Some("ZMAIL is already in Zmail.")
        );
    }

    #[test]
    fn a_signed_in_account_is_stored_with_its_session_or_not_at_all() {
        let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        let (account, account_id, session) = store_signed_in(
            &store,
            &secrets,
            &Script::default(),
            " zmail ",
            &signed_in(),
        )
        .expect("stored");
        block_on(session).expect("the session is kept");
        assert_eq!(account.username, "zmail");
        assert!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::ProtonUid)
                .is_ok()
        );

        let failing = Script {
            store_fails: true,
            ..Script::default()
        };
        let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
        let (_, _, session) =
            store_signed_in(&store, &secrets, &failing, "zmail", &signed_in()).expect("inserted");
        assert!(block_on(session).is_err());
        assert!(
            store.list_accounts().expect("accounts").is_empty(),
            "an account without its session is removed again"
        );
    }

    #[test]
    fn a_signed_in_account_becomes_a_proton_account() {
        let account = proton_account(
            "zmail",
            &SignedInAccount {
                email: "zmail@proton.me".to_owned(),
                display_name: String::new(),
            },
        );
        assert_eq!(account.protocol, ProtocolKind::Proton);
        assert_eq!(account.auth, AuthKind::ProtonSession);
        assert_eq!(account.username, "zmail");
        assert_eq!(account.display_name, "zmail@proton.me");
        assert_eq!(account.identities[0].display_name, "zmail@proton.me");
        assert!(account.outgoing.is_none());
        assert!(RISK_NOTE.contains("paid plans") && RISK_NOTE.contains("block"));
    }
}
