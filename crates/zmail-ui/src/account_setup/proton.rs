//! Adding a Proton Mail account: the sign-in steps of the add-account flow
//! (`zmail-ui`'s `proton` feature, off in release builds until the local
//! store is encrypted at rest, #139).
//!
//! Proton is not a server Zmail connects to with a password; Zmail signs
//! in to Proton's API the way Proton Mail Bridge does
//! (`zmail_proton::ProtonSignIn`), one step at a time:
//!
//! 1. **Sign in:** address and password, under an honest note about what
//!    a third-party client on Proton's private API means.
//! 2. **Verification**, when Proton asks for it: Zmail opens
//!    verify.proton.me in the browser, and signs in again once the user has
//!    finished there. Zmail never solves the check itself.
//! 3. **Two-factor code**, when the account has an authenticator app.
//! 4. **Mailbox password**, for accounts with a second password.
//!
//! The account is stored once signed in, with the session (never the
//! password) in the keychain, and the flow carries on with the colour and
//! first-sync steps.
//!
//! The same steps sign in to an existing Proton account again
//! ([`AccountSetupView::sign_in_to_proton_again`], from Settings →
//! Accounts' Re-authenticate), whether or not Proton has ended its session:
//! the old session is signed out and the new one goes into that account's
//! keychain slots (`SyncService::replace_proton_session`), so its mail
//! stays and nothing downloads again. A sign-in as another Proton account
//! is refused before anything happens to the old session. Every step runs
//! on the sync runtime; the view shows a spinner and keeps the user on the
//! step until it answers. The steps
//! themselves are `zmail_setup::proton`'s, shared with the terminal UI;
//! this file draws them.

use std::{future::Future, pin::Pin, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Global, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task, Window, div, px,
};
use zmail_core::{AccountColor, ProtocolKind, SyncPhase};
use zmail_proton::SignInStep;
use zmail_security::SecretStore;
use zmail_setup::accounts::{SyncFailure, sync_failure};
use zmail_setup::proton::{
    self as shared, ProtonAction, RISK_NOTE, RISK_TITLE, SignInSteps, VERIFICATION_CAPTION,
    VERIFICATION_DETAIL,
};
use zmail_sync::SyncService;

use super::render::{caption, field, heading, spinner};
use super::{AccountField, AccountSetupEvent, AccountSetupServices, AccountSetupView, Stage};
use crate::components::{Banner, BannerTone, Button};
use crate::styles::{SPACE_2, SPACE_4, UiColors};

/// A boxed future, as [`ProtonSide`] and [`Steps`] hand them back.
pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// What the Proton steps run against: the sync service's Proton
/// connection. View tests install a fake as a global instead, as they have
/// no Proton server to talk to.
#[derive(Clone)]
pub(crate) struct ProtonBackend(pub(crate) Arc<dyn ProtonSide>);

impl Global for ProtonBackend {}

impl ProtonBackend {
    /// The installed backend, or the sync service's.
    fn get(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_else(|| {
            let sync_service = cx.global::<AccountSetupServices>().sync_service.clone();
            Self(Arc::new(sync_service))
        })
    }
}

/// The Proton operations the sign-in steps need.
pub(crate) trait ProtonSide: Send + Sync {
    /// Starts signing in (no network yet).
    fn begin_sign_in(&self, username: &str, password: &str) -> anyhow::Result<ProtonSteps>;

    /// Replaces the account's session with the finished sign-in that
    /// `store` keeps in its keychain slots, signing the old one out first
    /// ([`SyncService::replace_proton_session`]).
    fn replace_session(
        &self,
        account_id: i64,
        store: BoxFuture<anyhow::Result<()>>,
    ) -> BoxFuture<anyhow::Result<()>>;

    /// After signing in again: syncs the account and sends the messages
    /// refused for the old session again.
    fn signed_in_again(&self, account_id: i64);
}

impl ProtonSide for SyncService {
    fn begin_sign_in(&self, username: &str, password: &str) -> anyhow::Result<ProtonSteps> {
        self.begin_proton_sign_in(username, password)
            .map(ProtonSteps::new)
    }

    fn replace_session(
        &self,
        account_id: i64,
        store: BoxFuture<anyhow::Result<()>>,
    ) -> BoxFuture<anyhow::Result<()>> {
        Box::pin(self.replace_proton_session(account_id, store))
    }

    fn signed_in_again(&self, account_id: i64) {
        let _ = self.sync_account(account_id);
        let _ = self.sync_watchers();
        let _ = self.retry_refused_sends(account_id);
    }
}

/// A sign-in's steps as the flow runs them: Proton's
/// (`zmail_proton::ProtonSignIn`), or a view test's fake.
#[derive(Clone)]
pub(crate) struct ProtonSteps(Arc<dyn Steps>);

impl ProtonSteps {
    pub(crate) fn new(steps: impl SignInSteps) -> Self {
        Self(Arc::new(steps))
    }
}

/// [`SignInSteps`] with its futures boxed, so the steps can be any type.
trait Steps: Send + Sync {
    fn login(&self) -> BoxFuture<anyhow::Result<SignInStep>>;
    fn submit_totp(&self, code: String) -> BoxFuture<anyhow::Result<SignInStep>>;
    fn submit_mailbox_password(&self, password: String) -> BoxFuture<anyhow::Result<SignInStep>>;
    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> BoxFuture<anyhow::Result<()>>;
    fn abort(&self) -> BoxFuture<()>;
}

impl<S: SignInSteps> Steps for S {
    fn login(&self) -> BoxFuture<anyhow::Result<SignInStep>> {
        Box::pin(SignInSteps::login(self))
    }

    fn submit_totp(&self, code: String) -> BoxFuture<anyhow::Result<SignInStep>> {
        Box::pin(SignInSteps::submit_totp(self, code))
    }

    fn submit_mailbox_password(&self, password: String) -> BoxFuture<anyhow::Result<SignInStep>> {
        Box::pin(SignInSteps::submit_mailbox_password(self, password))
    }

    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> BoxFuture<anyhow::Result<()>> {
        Box::pin(SignInSteps::store(self, secrets, account_id))
    }

    fn abort(&self) -> BoxFuture<()> {
        Box::pin(SignInSteps::abort(self))
    }
}

impl SignInSteps for ProtonSteps {
    fn login(&self) -> impl Future<Output = anyhow::Result<SignInStep>> + Send + 'static + use<> {
        self.0.login()
    }

    fn submit_totp(
        &self,
        code: String,
    ) -> impl Future<Output = anyhow::Result<SignInStep>> + Send + 'static + use<> {
        self.0.submit_totp(code)
    }

    fn submit_mailbox_password(
        &self,
        password: String,
    ) -> impl Future<Output = anyhow::Result<SignInStep>> + Send + 'static + use<> {
        self.0.submit_mailbox_password(password)
    }

    fn store(
        &self,
        secrets: Arc<dyn SecretStore>,
        account_id: i64,
    ) -> impl Future<Output = anyhow::Result<()>> + Send + 'static + use<> {
        self.0.store(secrets, account_id)
    }

    fn abort(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.0.abort()
    }
}

/// The Proton sign-in in progress: the shared state machine and the task
/// running its current step.
#[derive(Default)]
pub(crate) struct ProtonFlow {
    pub(crate) core: shared::ProtonFlow<ProtonSteps>,
    task: Option<Task<()>>,
    /// The existing account this sign-in is for, when signing in again.
    pub(crate) again: Option<SignInAgain>,
}

/// An existing Proton account to sign in to again.
#[derive(Debug, Clone)]
pub(crate) struct SignInAgain {
    pub(crate) account_id: i64,
    pub(crate) name: String,
    /// Whether Proton has ended the account's session (its last failure,
    /// still standing, is a sign-in one), rather than the user asking to
    /// sign in again while it works.
    pub(crate) ended: bool,
}

impl AccountSetupView {
    /// Signs in to the existing Proton account `account_id` again: the
    /// sign-in step, with its address filled in and nothing to go back to.
    pub(crate) fn sign_in_to_proton_again(
        &mut self,
        account_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = cx.global::<AccountSetupServices>().store.clone();
        let Some(account) = store
            .list_accounts()
            .ok()
            .and_then(|accounts| accounts.into_iter().find(|found| found.id == account_id))
        else {
            return;
        };
        self.proton_abort(cx);
        self.form.provider = None;
        self.form.use_oauth = false;
        self.form.protocol = ProtocolKind::Proton;
        self.form.errors.clear();
        self.form
            .set_text(AccountField::Email, &account.username, cx);
        self.form.set_text(AccountField::Password, "", cx);
        self.proton.core.error = None;
        // The last failure, still standing (a retry may be under way).
        let status = &account.sync_status;
        let ended = (status.phase == SyncPhase::Failed || status.failure_count > 0)
            && status
                .last_error_message
                .as_deref()
                .is_some_and(|error| sync_failure(error) == SyncFailure::SignIn);
        self.proton.again = Some(SignInAgain {
            account_id,
            name: account.display_name,
            ended,
        });
        self.history.clear();
        self.stage = Stage::ProtonSignIn;
        self.focus_stage(window, cx);
        cx.notify();
    }

    /// The account this flow signs in to again, if it does.
    pub(crate) fn signing_in_again(&self) -> Option<&SignInAgain> {
        self.proton.again.as_ref()
    }

    /// The provider step's Proton Mail tile.
    pub(crate) fn choose_proton(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.provider = None;
        self.form.use_oauth = false;
        self.form.protocol = ProtocolKind::Proton;
        self.form.errors.clear();
        self.proton.core.error = None;
        self.go(Stage::ProtonSignIn, window, cx);
    }

    /// The current Proton step's Continue.
    pub(crate) fn proton_advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proton.core.busy {
            return;
        }
        let input = match self.stage {
            Stage::ProtonSignIn => {
                self.proton_start(window, cx);
                return;
            }
            Stage::ProtonTotp => self.form.text(AccountField::ProtonTotp, cx),
            Stage::ProtonMailboxPassword => self.form.text(AccountField::ProtonMailboxPassword, cx),
            _ => String::new(),
        };
        match self.proton.core.submit(&input) {
            Ok(Some(step)) => self.proton_run(step, window, cx),
            Ok(None) => {}
            Err((field, message)) => {
                self.show_errors(vec![(field, message)], window, cx);
            }
        }
    }

    /// Checks the address and password, then signs in.
    fn proton_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.form.text(AccountField::Email, cx).trim().to_owned();
        let password = self.form.text(AccountField::Password, cx);
        if self.show_errors(shared::check_sign_in(&email, &password), window, cx) {
            return;
        }
        let services = cx.global::<AccountSetupServices>().clone();
        if self.proton.again.is_none()
            && let Some(duplicate) = shared::duplicate(&services.store, &email)
        {
            self.proton.core.error = Some(duplicate);
            cx.notify();
            return;
        }
        self.proton_abort(cx);
        let sign_in = match ProtonBackend::get(cx).0.begin_sign_in(&email, &password) {
            Ok(sign_in) => sign_in,
            Err(error) => {
                self.proton.core.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let step = self.proton.core.start(sign_in);
        self.proton_run(step, window, cx);
    }

    /// Runs a sign-in step on the sync runtime and moves on with its
    /// answer.
    fn proton_run(
        &mut self,
        step: impl std::future::Future<Output = anyhow::Result<SignInStep>> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = cx.global::<AccountSetupServices>().clone();
        let run = services.sync_service.run_proton(step);
        self.proton.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await.and_then(|step| step);
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(action) = this.proton.core.finish(result) {
                    this.proton_handle(action, window, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Shows what the sign-in needs next.
    fn proton_handle(&mut self, action: ProtonAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ProtonAction::Verify { url } => {
                cx.open_url(&url);
                self.go(Stage::ProtonVerification, window, cx);
            }
            ProtonAction::AskTotp => {
                self.form.set_text(AccountField::ProtonTotp, "", cx);
                self.go(Stage::ProtonTotp, window, cx);
            }
            ProtonAction::AskMailboxPassword => {
                self.form
                    .set_text(AccountField::ProtonMailboxPassword, "", cx);
                self.go(Stage::ProtonMailboxPassword, window, cx);
            }
            ProtonAction::SignedIn(account) => match self.proton.again.clone() {
                Some(again) => self.proton_store_again(again, account, window, cx),
                None => self.proton_store(account, window, cx),
            },
        }
    }

    /// Stores the signed-in account and its session, then carries on with
    /// the colour step and the first sync.
    fn proton_store(
        &mut self,
        account: zmail_proton::SignedInAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sign_in) = self.proton.core.sign_in().cloned() else {
            return;
        };
        let services = cx.global::<AccountSetupServices>().clone();
        let username = self.form.text(AccountField::Email, cx);
        let (new_account, account_id, store_session) = match shared::store_signed_in(
            &services.store,
            &services.secrets,
            &sign_in,
            &username,
            &account,
        ) {
            Ok(stored) => stored,
            Err(error) => {
                cx.emit(AccountSetupEvent::Error(error.to_string()));
                return;
            }
        };
        let run = services.sync_service.run_proton(store_session);
        self.proton.core.busy = true;
        self.proton.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await.and_then(|stored| stored);
            let _ = this.update_in(cx, |this, window, cx| {
                this.proton.core.busy = false;
                let services = cx.global::<AccountSetupServices>().clone();
                if let Err(error) = result {
                    this.proton.core.error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
                // The password is not needed any more.
                this.form.set_text(AccountField::Password, "", cx);
                this.form
                    .set_text(AccountField::ProtonMailboxPassword, "", cx);
                this.proton.core.done();
                let _ = services.sync_service.sync_account(account_id);
                let _ = services.sync_service.sync_watchers();
                this.account_id = Some(account_id);
                this.color = AccountColor::derived(account_id);
                this.history.clear();
                cx.emit(AccountSetupEvent::Added {
                    account_id,
                    message: format!("{} added.", new_account.display_name),
                });
                this.go(Stage::Colour, window, cx);
            });
        }));
        cx.notify();
    }

    /// Keeps the new session for the account signed in to again, in place
    /// of the old one (signed out first, even while it still works), then
    /// syncs it, with the messages refused for the old session sent again,
    /// and closes the flow. A sign-in as another Proton account is refused
    /// before anything happens to the old session.
    fn proton_store_again(
        &mut self,
        again: SignInAgain,
        account: zmail_proton::SignedInAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sign_in) = self.proton.core.sign_in().cloned() else {
            return;
        };
        let services = cx.global::<AccountSetupServices>().clone();
        let store_session = match shared::store_signed_in_again(
            &services.store,
            &services.secrets,
            &sign_in,
            again.account_id,
            &account,
        ) {
            Ok(store_session) => store_session,
            Err(message) => {
                // Signed in to another account: sign that session out and
                // start again from the password.
                self.proton_abort(cx);
                self.proton.core.error = Some(message);
                self.history.clear();
                self.stage = Stage::ProtonSignIn;
                self.focus_stage(window, cx);
                cx.notify();
                return;
            }
        };
        let backend = ProtonBackend::get(cx);
        let replace = backend
            .0
            .replace_session(again.account_id, Box::pin(store_session));
        let run = services.sync_service.run_proton(replace);
        self.proton.core.busy = true;
        self.proton.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await.and_then(|stored| stored);
            let _ = this.update_in(cx, |this, _, cx| {
                this.proton.core.busy = false;
                if let Err(error) = result {
                    this.proton.core.error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
                this.form.set_text(AccountField::Password, "", cx);
                this.form
                    .set_text(AccountField::ProtonMailboxPassword, "", cx);
                this.proton.core.done();
                let account_id = again.account_id;
                // Messages refused for the old session go now.
                backend.0.signed_in_again(account_id);
                let message = format!("Signed in to Proton again. {} is syncing.", again.name);
                cx.defer(move |cx| super::window::show_status_everywhere(&message, cx));
                cx.emit(AccountSetupEvent::Finished {
                    account_id: Some(account_id),
                });
            });
        }));
        cx.notify();
    }

    /// Leaves a sign-in that did not finish: signs its session out.
    pub(crate) fn proton_abort(&mut self, cx: &mut Context<Self>) {
        self.proton.task = None;
        if let Some(abort) = self.proton.core.abort() {
            let services = cx.global::<AccountSetupServices>().clone();
            let abort = services.sync_service.run_proton(abort);
            cx.background_spawn(abort).detach();
        }
    }

    /// The Proton sign-in step.
    pub(crate) fn render_proton_sign_in(&self, colors: &UiColors) -> AnyElement {
        let (title, detail) = match &self.proton.again {
            Some(again) if again.ended => (
                "Sign in to Proton Mail again".to_owned(),
                format!(
                    "Proton ended Zmail's session for {}. Sign in to carry on: its mail stays on this computer and nothing downloads again.",
                    again.name
                ),
            ),
            Some(again) => (
                "Sign in to Proton Mail again".to_owned(),
                format!(
                    "Once you have signed in, Zmail signs out of the session it has been using for {}. Its mail stays on this computer and nothing downloads again.",
                    again.name
                ),
            ),
            None => (
                "Sign in to Proton Mail".to_owned(),
                "Use your Proton address or username and your Proton password.".to_owned(),
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(title, detail, colors))
            .child(
                Banner::new("account-setup-proton-risk", RISK_NOTE)
                    .tone(BannerTone::Caution)
                    .title(RISK_TITLE),
            )
            .child(field(self, AccountField::Email, colors))
            .child(field(self, AccountField::Password, colors))
            .children(self.proton_status(colors))
            .into_any_element()
    }

    /// The human verification step.
    pub(crate) fn render_proton_verification(&self, colors: &UiColors) -> AnyElement {
        let url = self.proton.core.verify_url.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading("Verify it's you", VERIFICATION_DETAIL, colors))
            .child(
                div().flex().child(
                    Button::new("account-setup-proton-verify", "Open the verification page")
                        .icon("external-link")
                        .on_click(move |_: &ClickEvent, _, cx: &mut App| cx.open_url(&url)),
                ),
            )
            .child(caption(VERIFICATION_CAPTION, colors))
            .children(self.proton_status(colors))
            .into_any_element()
    }

    /// The two-factor code step.
    pub(crate) fn render_proton_totp(&self, colors: &UiColors) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Two-factor authentication",
                "Enter the code your authenticator app shows for Proton.",
                colors,
            ))
            .child(field(self, AccountField::ProtonTotp, colors))
            .children(self.proton_status(colors))
            .into_any_element()
    }

    /// The mailbox password step.
    pub(crate) fn render_proton_mailbox_password(&self, colors: &UiColors) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Mailbox password",
                "This Proton account has a second password that unlocks its mail.",
                colors,
            ))
            .child(field(self, AccountField::ProtonMailboxPassword, colors))
            .children(self.proton_status(colors))
            .into_any_element()
    }

    /// A spinner while a step runs, or why the last one failed.
    fn proton_status(&self, colors: &UiColors) -> Option<AnyElement> {
        if self.proton.core.busy {
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(spinner("account-setup-proton-spinner", colors.accent))
                    .child(caption("Talking to Proton…", colors))
                    .into_any_element(),
            );
        }
        self.proton.core.error.clone().map(|error| {
            Banner::new("account-setup-proton-error", SharedString::from(error))
                .tone(BannerTone::Danger)
                .title("Couldn't sign in")
                .into_any_element()
        })
    }
}
