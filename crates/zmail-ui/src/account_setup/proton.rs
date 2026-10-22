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
//! spinner and keeps the user on the step until it answers.

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task, Window, div, px,
};
use zmail_core::{
    AccountColor, AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind,
};
use zmail_proton::{ProtonSignIn, SignInStep, SignedInAccount};

use super::render::{caption, field, heading, spinner};
use super::{AccountField, AccountSetupEvent, AccountSetupServices, AccountSetupView, Stage};
use crate::components::{Banner, BannerTone, Button};
use crate::styles::{SPACE_2, SPACE_4, UiColors};

/// What a third-party Proton client means, shown on the sign-in step.
pub(crate) const RISK_NOTE: &str = "Zmail is an independent client, not made or supported by Proton. It talks to Proton's private API the way Proton Mail Bridge does and signs in as Bridge, because Proton doesn't accept other clients, so your Proton sessions list shows it as Proton Mail Bridge. Proton may change or block it at any time, and only allows it on paid plans. Zmail keeps a session token and the key that unlocks your mail in the system keychain, never your password.";

/// The Proton sign-in in progress.
#[derive(Default)]
pub(crate) struct ProtonFlow {
    sign_in: Option<ProtonSignIn>,
    /// The verification page Proton asked the user to visit.
    pub(crate) verify_url: Option<String>,
    /// A step is running.
    pub(crate) busy: bool,
    /// Why the last step failed.
    pub(crate) error: Option<String>,
    task: Option<Task<()>>,
}

impl AccountSetupView {
    /// The provider step's Proton Mail tile.
    pub(crate) fn choose_proton(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form.provider = None;
        self.form.use_oauth = false;
        self.form.protocol = ProtocolKind::Proton;
        self.form.errors.clear();
        self.proton.error = None;
        self.go(Stage::ProtonSignIn, window, cx);
    }

    /// The current Proton step's Continue.
    pub(crate) fn proton_advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proton.busy {
            return;
        }
        match self.stage {
            Stage::ProtonSignIn => self.proton_start(window, cx),
            Stage::ProtonVerification => {
                if let Some(sign_in) = self.proton.sign_in.clone() {
                    self.proton_run(async move { sign_in.login().await }, window, cx);
                }
            }
            Stage::ProtonTotp => {
                let code = self.form.text(AccountField::ProtonTotp, cx);
                if code.trim().is_empty() {
                    self.show_proton_field_error(
                        AccountField::ProtonTotp,
                        "Enter the code from your authenticator app.",
                        window,
                        cx,
                    );
                    return;
                }
                if let Some(sign_in) = self.proton.sign_in.clone() {
                    self.proton_run(async move { sign_in.submit_totp(&code).await }, window, cx);
                }
            }
            Stage::ProtonMailboxPassword => {
                let password = self.form.text(AccountField::ProtonMailboxPassword, cx);
                if password.is_empty() {
                    self.show_proton_field_error(
                        AccountField::ProtonMailboxPassword,
                        "Enter your mailbox password.",
                        window,
                        cx,
                    );
                    return;
                }
                if let Some(sign_in) = self.proton.sign_in.clone() {
                    self.proton_run(
                        async move { sign_in.submit_mailbox_password(&password).await },
                        window,
                        cx,
                    );
                }
            }
            _ => {}
        }
    }

    fn show_proton_field_error(
        &mut self,
        field: AccountField,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_errors(vec![(field, message.to_owned())], window, cx);
    }

    /// Checks the address and password, then signs in.
    fn proton_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let email = self.form.text(AccountField::Email, cx).trim().to_owned();
        let password = self.form.text(AccountField::Password, cx);
        let mut errors = Vec::new();
        if email.is_empty() {
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
        if self.show_errors(errors, window, cx) {
            return;
        }
        let services = cx.global::<AccountSetupServices>().clone();
        let duplicate = services.store.list_accounts().ok().is_some_and(|accounts| {
            accounts.iter().any(|account| {
                account.protocol == ProtocolKind::Proton
                    && account.username.eq_ignore_ascii_case(&email)
            })
        });
        if duplicate {
            self.proton.error = Some(format!("{email} is already in Zmail."));
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
                self.proton.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        self.proton.sign_in = Some(sign_in.clone());
        self.proton_run(async move { sign_in.login().await }, window, cx);
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
        self.proton.busy = true;
        self.proton.error = None;
        self.proton.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await.and_then(|step| step);
            let _ = this.update_in(cx, |this, window, cx| {
                this.proton.busy = false;
                match result {
                    Ok(step) => this.proton_handle_step(step, window, cx),
                    Err(error) => this.proton.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Shows what the sign-in needs next.
    fn proton_handle_step(
        &mut self,
        step: SignInStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match step {
            SignInStep::HumanVerification { url } => {
                cx.open_url(&url);
                self.proton.verify_url = Some(url);
                self.go(Stage::ProtonVerification, window, cx);
            }
            SignInStep::Totp => {
                self.form.set_text(AccountField::ProtonTotp, "", cx);
                self.go(Stage::ProtonTotp, window, cx);
            }
            SignInStep::MailboxPassword => {
                self.form
                    .set_text(AccountField::ProtonMailboxPassword, "", cx);
                self.go(Stage::ProtonMailboxPassword, window, cx);
            }
            SignInStep::SignedIn(account) => self.proton_store(account, window, cx),
        }
    }

    /// Stores the signed-in account and its session, then carries on with
    /// the colour step and the first sync.
    fn proton_store(
        &mut self,
        account: SignedInAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sign_in) = self.proton.sign_in.clone() else {
            return;
        };
        let services = cx.global::<AccountSetupServices>().clone();
        let username = self.form.text(AccountField::Email, cx).trim().to_owned();
        let new_account = proton_account(&username, &account);
        let account_id = match services.store.insert_account(&new_account) {
            Ok(account_id) => account_id,
            Err(error) => {
                cx.emit(AccountSetupEvent::Error(error.to_string()));
                return;
            }
        };
        let secrets = services.secrets.clone();
        let store_session = async move { sign_in.store(secrets.as_ref(), account_id).await };
        let run = services.sync_service.run_proton(store_session);
        self.proton.busy = true;
        self.proton.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = run.await.and_then(|stored| stored);
            let _ = this.update_in(cx, |this, window, cx| {
                this.proton.busy = false;
                let services = cx.global::<AccountSetupServices>().clone();
                if let Err(error) = result {
                    let _ = services.store.delete_account(account_id);
                    this.proton.error =
                        Some(format!("Could not keep the Proton session: {error:#}"));
                    cx.notify();
                    return;
                }
                // The password is not needed any more.
                this.form.set_text(AccountField::Password, "", cx);
                this.form
                    .set_text(AccountField::ProtonMailboxPassword, "", cx);
                this.proton.sign_in = None;
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
        self.proton.busy = false;
        self.proton.verify_url = None;
        if let Some(sign_in) = self.proton.sign_in.take() {
            let services = cx.global::<AccountSetupServices>().clone();
            let abort = services
                .sync_service
                .run_proton(async move { sign_in.abort().await });
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
                    .title("A third-party client for Proton"),
            )
            .child(field(self, AccountField::Email, colors))
            .child(field(self, AccountField::Password, colors))
            .children(self.proton_status(colors))
            .into_any_element()
    }

    /// The human verification step.
    pub(crate) fn render_proton_verification(&self, colors: &UiColors) -> AnyElement {
        let url = self.proton.verify_url.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Verify it's you",
                "Proton wants to check that a person is signing in. Finish the check on Proton's page in your browser, then continue here.",
                colors,
            ))
            .child(
                div().flex().child(
                    Button::new("account-setup-proton-verify", "Open the verification page")
                        .icon("external-link")
                        .on_click(move |_: &ClickEvent, _, cx: &mut App| cx.open_url(&url)),
                ),
            )
            .child(caption(
                "Zmail doesn't see or solve the check; it only signs in again once you've finished.",
                colors,
            ))
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
        if self.proton.busy {
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
        self.proton.error.clone().map(|error| {
            Banner::new("account-setup-proton-error", SharedString::from(error))
                .tone(BannerTone::Danger)
                .title("Couldn't sign in")
                .into_any_element()
        })
    }
}

/// The account a Proton sign-in adds: the identities come from Proton with
/// the first sync.
fn proton_account(username: &str, account: &SignedInAccount) -> NewAccount {
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

#[cfg(test)]
mod tests {
    use super::*;

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
