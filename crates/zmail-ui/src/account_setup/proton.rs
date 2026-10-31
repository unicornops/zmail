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
//! first-sync steps. Every step runs on the sync runtime; the view shows a
//! spinner and keeps the user on the step until it answers. The steps
//! themselves are `zmail_setup::proton`'s, shared with the terminal UI;
//! this file draws them.

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task, Window, div, px,
};
use zmail_core::{AccountColor, ProtocolKind};
use zmail_proton::SignInStep;
use zmail_setup::proton::{
    self as shared, ProtonAction, RISK_NOTE, RISK_TITLE, VERIFICATION_CAPTION, VERIFICATION_DETAIL,
};

use super::render::{caption, field, heading, spinner};
use super::{AccountField, AccountSetupEvent, AccountSetupServices, AccountSetupView, Stage};
use crate::components::{Banner, BannerTone, Button};
use crate::styles::{SPACE_2, SPACE_4, UiColors};

/// The Proton sign-in in progress: the shared state machine and the task
/// running its current step.
#[derive(Default)]
pub(crate) struct ProtonFlow {
    pub(crate) core: shared::ProtonFlow,
    task: Option<Task<()>>,
}

impl AccountSetupView {
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
        if let Some(duplicate) = shared::duplicate(&services.store, &email) {
            self.proton.core.error = Some(duplicate);
            cx.notify();
            return;
        }
        self.proton_abort(cx);
        let sign_in = match services
            .sync_service
            .begin_proton_sign_in(&email, &password)
        {
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
            ProtonAction::SignedIn(account) => self.proton_store(account, window, cx),
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
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Sign in to Proton Mail",
                "Use your Proton address or username and your Proton password.",
                colors,
            ))
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
