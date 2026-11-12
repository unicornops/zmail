//! Settings → Accounts: one card per account with its sign-in, sync
//! status, colour and cache budget, and Re-authenticate and Remove; plus
//! Add Account, which opens the add-account flow.
//!
//! - **Colour** overrides the colour derived from the account's id
//!   (`LocalStore::set_account_color`); "Automatic" goes back to it.
//! - **Cache budget** caps the downloaded bodies and attachments kept for
//!   the account (`LocalStore::set_account_disk_budget`). A new limit
//!   clears the oldest right away, in the background; they download again
//!   when opened.
//! - **Re-authenticate** asks for the password again, for OAuth2
//!   accounts runs the browser sign-in again with the stored client, and
//!   for a Proton account whose session Proton has ended runs Proton's
//!   sign-in again for that account, keeping its mail. A new
//!   password is tested against the incoming server before it replaces
//!   the saved one (`zmail_setup::check`); when the test fails, the dialog
//!   says why and offers Save anyway.
//! - **Remove** confirms in a dialog that says what goes: the account's
//!   local data and its keychain secrets, never the mail on the server.
//!   It also lists unsent Outbox messages and drafts, which exist only
//!   on this computer and go with the account. A message being sent at
//!   that moment blocks removal until it has gone.
//!
//! - **Identities** that differ from their JMAP server get a notice on
//!   the card, worded by `zmail_setup::identities`, with Update the
//!   server and Use the server's; one the server no longer lists offers
//!   to create it there again or to remove it from Zmail (after asking).
//!   Updating and creating run on the sync runtime and report back with a
//!   toast.
//! - **Identities** also lists every identity, with Edit…, Add Identity…,
//!   Remove… and Make primary (`identities`, #366).
//!
//! The sidebar's Fix… on a failing account opens this section on that
//! account ([`AccountsSettings::focus_account`]), as does Re-authenticate
//! on an Outbox message the server refused for the account's sign-in. The
//! banner there reads what kind of failure it is
//! (`zmail_setup::accounts::sync_failure`): one only signing in again
//! fixes offers Re-authenticate; a server that couldn't be reached says
//! so plainly ("Can't reach imap.example.com") and offers Try again; any
//! other shows the error with both.

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, IntoElement, Render, Role,
    SharedString, Subscription, Task, Window, div, prelude::*, px, rgb,
};
use zmail_core::{
    AccountColor, AccountSummary, AuthKind, OutboxJobState, SyncPhase,
    identity_sync::{IdentitySyncStatus, SyncedIdentity},
    reauth::ReauthReason,
};
use zmail_setup::{
    accounts::{
        RemoveError, SyncFailure, budget_label, budget_options, removal_blocker,
        removal_description, removal_lines, server_host, sync_failure, unreachable_sentence,
    },
    check::{CheckPart, check_label, stored_account},
    identity_editor::AccountIdentities,
};
use zmail_store::AccountRemovalImpact;

mod identities;

use self::identities::IdentityDialogs;
use super::{
    SettingsEvent, SettingsHost, dotted_label, section_heading, setting_row, setting_text,
    update_workspace,
};
use crate::a11y::{A11yFlags, AccessibleExt, Politeness, readable};
use crate::account_setup::SignInCheck;
use crate::components::{
    Banner, BannerTone, Button, ButtonSize, ComponentTheme, Modal, ModalState, Select, SelectEvent,
    TextInput, TextInputEvent,
};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY_STRONG, TYPE_CAPTION,
    TypeStyleExt, UiColors,
};
use crate::widgets::{EllipsisExt, format_size, icon, sync_badge};

/// What the fix banner advises for a failure only signing in again fixes.
/// `google` is a Gmail account signed in with the user's own client.
fn reauth_advice(reason: ReauthReason, auth: AuthKind, google: bool) -> &'static str {
    match (reason, auth) {
        (ReauthReason::GrantExpired, _) if google => zmail_setup::google::EXPIRED_ADVICE,
        (ReauthReason::GrantExpired, _) => {
            "Your provider no longer accepts Zmail's saved sign-in: it expired, or access was revoked. Sign in again to carry on."
        }
        (ReauthReason::SendRefused, AuthKind::OAuth2) => {
            "Signing in again asks your provider for permission to send mail as well as read it. Microsoft 365 accounts added before Zmail asked for that need it once. Messages waiting in the Outbox send once you have signed in."
        }
        (ReauthReason::SendRefused, _) => {
            "The outgoing server no longer accepts the saved password. Enter the current one, or a new app password. Messages waiting in the Outbox send once you have signed in."
        }
    }
}

/// Width of the cards' pickers.
const SELECT_WIDTH: f32 = 200.0;

/// One account's card: its summary, cache use and pickers.
struct AccountCard {
    account: AccountSummary,
    auth: AuthKind,
    /// Bytes of bodies and attachments kept, once measured
    /// ([`AccountsSettings::measure_usage`]).
    usage_bytes: Option<u64>,
    color: Entity<Select>,
    budget: Entity<Select>,
    /// The budgets `budget` lists: [`CACHE_BUDGETS`], plus the saved one
    /// when it is not among them.
    budget_options: Vec<Option<u64>>,
    /// The error of an Outbox message the server refused for the
    /// account's sign-in, if any: the account can sync but not send.
    refused_send: Option<String>,
    /// A Gmail account signed in with the user's own OAuth client, whose
    /// sign-ins expire after 7 days while its app is in Testing.
    google_oauth: bool,
    /// The account's identities that differ from the server, or that the
    /// server no longer lists.
    identities: Vec<SyncedIdentity>,
    /// All the account's identities, for the Identities group.
    all_identities: AccountIdentities,
    _subscriptions: [Subscription; 2],
}

/// The account a Remove confirmation is for.
struct PendingRemoval {
    account_id: i64,
    name: String,
    impact: AccountRemovalImpact,
}

/// The Accounts section.
pub(crate) struct AccountsSettings {
    cards: Vec<AccountCard>,
    /// The account Fix… opened the section on.
    focused_account: Option<i64>,
    /// The password dialog of Re-authenticate.
    reauth: ModalState,
    reauth_account: Option<(i64, String)>,
    password: Entity<TextInput>,
    /// The new password's sign-in test while it runs: what it is doing.
    reauth_checking: Option<String>,
    reauth_check: Option<Task<()>>,
    /// Why the new password's sign-in test failed; the dialog then offers
    /// Save anyway.
    reauth_failure: Option<String>,
    /// The Remove confirmation.
    remove: ModalState,
    removal: Option<PendingRemoval>,
    remove_cancel: FocusHandle,
    /// The identity being sent to the server, whose buttons wait.
    identity_busy: Option<i64>,
    /// The identity editor and its remove confirmation.
    identity_dialogs: IdentityDialogs,
    _subscriptions: Vec<Subscription>,
}

impl AccountsSettings {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| {
            TextInput::new(cx)
                .masked()
                .label("Password")
                .placeholder("Password or app password")
        });
        let subscriptions = vec![cx.subscribe_in(
            &password,
            window,
            |this, _, event: &TextInputEvent, window, cx| {
                if *event == TextInputEvent::Submitted {
                    this.save_password(window, cx);
                }
            },
        )];
        let mut this = Self {
            cards: Vec::new(),
            focused_account: None,
            reauth: ModalState::new(cx),
            reauth_account: None,
            password,
            reauth_checking: None,
            reauth_check: None,
            reauth_failure: None,
            remove: ModalState::new(cx),
            removal: None,
            remove_cancel: cx.focus_handle(),
            identity_busy: None,
            identity_dialogs: IdentityDialogs::new(window, cx),
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this.measure_usage(cx);
        this
    }

    /// Measure each account's cache use in the background. It reads every
    /// stored body, so it runs when the section opens and after a budget
    /// change, not on every refresh.
    pub(crate) fn measure_usage(&mut self, cx: &mut Context<Self>) {
        let store = SettingsHost::get(cx).store.clone();
        let ids: Vec<i64> = self.cards.iter().map(|card| card.account.id).collect();
        let measuring = cx.background_spawn(async move {
            ids.into_iter()
                .map(|id| (id, store.account_blob_usage(id).ok()))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let usage = measuring.await;
            let _ = this.update(cx, |this, cx| {
                for (id, bytes) in usage {
                    if let Some(card) = this.cards.iter_mut().find(|card| card.account.id == id) {
                        card.usage_bytes = bytes;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-read the accounts and their status.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = SettingsHost::get(cx).store.clone();
        let accounts = store.list_accounts().unwrap_or_default();
        let mut cards = Vec::with_capacity(accounts.len());
        for account in accounts {
            let budget_mb = store
                .account_disk_budget_bytes(account.id)
                .ok()
                .flatten()
                .map(|bytes| bytes / (1024 * 1024));
            let target = store.sync_target(account.id).ok().flatten();
            let auth = target
                .as_ref()
                .map(|target| target.auth)
                .unwrap_or_default();
            let google_oauth = target.and_then(|target| target.oauth2).and_then(|config| {
                zmail_auth::OAuth2Preset::for_token_endpoint(&config.token_endpoint)
            }) == Some(zmail_auth::OAuth2Preset::Gmail);
            let existing = self
                .cards
                .iter()
                .position(|card| card.account.id == account.id)
                .map(|index| self.cards.remove(index));
            let mut card = match existing {
                Some(card) => card,
                None => self.new_card(&account, cx),
            };
            card.color.update(cx, |select, cx| {
                select.set_selected(Some(color_index(account.color_override)), cx)
            });
            let options = budget_options(budget_mb);
            if options != card.budget_options {
                card.budget = budget_select(&options, cx);
                card._subscriptions[1] = self.watch_budget(&card.budget, account.id, cx);
                card.budget_options = options;
            }
            let selected = card
                .budget_options
                .iter()
                .position(|option| *option == budget_mb);
            card.budget
                .update(cx, |select, cx| select.set_selected(selected, cx));
            card.refused_send = store
                .list_outbox_jobs(Some(account.id))
                .unwrap_or_default()
                .into_iter()
                .filter(|job| job.state == OutboxJobState::Failed)
                .filter_map(|job| job.last_error_message)
                .find(|message| ReauthReason::of_message(message).is_some());
            card.all_identities = AccountIdentities {
                account_id: account.id,
                protocol: account.protocol,
                identities: store.list_synced_identities(account.id).unwrap_or_default(),
                hidden: store.list_hidden_identities(account.id).unwrap_or_default(),
            };
            card.identities = card
                .all_identities
                .identities
                .iter()
                .filter(|identity| identity.status.needs_attention())
                .cloned()
                .collect();
            card.account = account;
            card.auth = auth;
            card.google_oauth = google_oauth;
            cards.push(card);
        }
        self.cards = cards;
        cx.notify();
    }

    fn new_card(&mut self, account: &AccountSummary, cx: &mut Context<Self>) -> AccountCard {
        let derived = AccountColor::derived(account.id);
        let labels = std::iter::once(format!("Automatic ({})", derived.label())).chain(
            AccountColor::ALL
                .iter()
                .map(|color| color.label().to_owned()),
        );
        let color = cx.new(|cx| {
            Select::new(labels, Some(color_index(account.color_override)), cx)
                .label("Colour")
                .width(px(SELECT_WIDTH))
        });
        let account_id = account.id;
        let color_subscription = cx.subscribe(&color, move |this, _, event: &SelectEvent, cx| {
            let SelectEvent::Changed(index) = *event;
            let color = index.checked_sub(1).map(|index| AccountColor::ALL[index]);
            this.set_color(account_id, color, cx);
        });
        let options = budget_options(None);
        let budget = budget_select(&options, cx);
        let budget_subscription = self.watch_budget(&budget, account.id, cx);
        AccountCard {
            account: account.clone(),
            auth: AuthKind::default(),
            usage_bytes: None,
            color,
            budget,
            budget_options: options,
            refused_send: None,
            google_oauth: false,
            identities: Vec::new(),
            all_identities: AccountIdentities {
                account_id: account.id,
                protocol: account.protocol,
                identities: Vec::new(),
                hidden: Vec::new(),
            },
            _subscriptions: [color_subscription, budget_subscription],
        }
    }

    fn watch_budget(
        &self,
        budget: &Entity<Select>,
        account_id: i64,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(budget, move |this, _, event: &SelectEvent, cx| {
            let SelectEvent::Changed(index) = *event;
            let budget = this
                .cards
                .iter()
                .find(|card| card.account.id == account_id)
                .and_then(|card| card.budget_options.get(index).copied());
            if let Some(budget) = budget {
                this.set_budget(account_id, budget, cx);
            }
        })
    }

    /// Show the section on `account_id`, as Fix… does.
    pub(crate) fn focus_account(&mut self, account_id: i64, cx: &mut Context<Self>) {
        self.focused_account = Some(account_id);
        self.refresh(cx);
    }

    /// The account Fix… opened the section on.
    #[cfg(test)]
    pub(crate) fn focused_account(&self) -> Option<i64> {
        self.focused_account
    }

    /// An account's colour and budget pickers, for tests to drive.
    #[cfg(test)]
    pub(crate) fn selects(&self, account_id: i64) -> Option<(Entity<Select>, Entity<Select>)> {
        self.cards
            .iter()
            .find(|card| card.account.id == account_id)
            .map(|card| (card.color.clone(), card.budget.clone()))
    }

    /// The password field of Re-authenticate.
    #[cfg(test)]
    pub(crate) fn password_input(&self) -> &Entity<TextInput> {
        &self.password
    }

    /// The titles of the identity notices, card by card.
    #[cfg(test)]
    pub(crate) fn identity_notice_titles(&self) -> Vec<String> {
        self.cards
            .iter()
            .flat_map(|card| &card.identities)
            .filter_map(zmail_setup::identities::notice_title)
            .collect()
    }

    /// Whether the Remove confirmation is open.
    #[cfg(test)]
    pub(crate) fn removal_open(&self) -> bool {
        self.remove.is_open()
    }

    /// Whether the Re-authenticate dialog is open.
    #[cfg(test)]
    pub(crate) fn reauth_open(&self) -> bool {
        self.reauth.is_open()
    }

    fn set_color(&mut self, account_id: i64, color: Option<AccountColor>, cx: &mut Context<Self>) {
        if let Err(error) = SettingsHost::get(cx)
            .store
            .set_account_color(account_id, color)
        {
            cx.emit(SettingsEvent::Error(format!(
                "Could not save the colour: {error:#}"
            )));
            return;
        }
        refresh_workspace(cx);
        self.refresh(cx);
    }

    /// Save a cache budget and, for a limit, clear what is over it in the
    /// background.
    fn set_budget(&mut self, account_id: i64, budget_mb: Option<u64>, cx: &mut Context<Self>) {
        let store = SettingsHost::get(cx).store.clone();
        if let Err(error) = store.set_account_disk_budget(account_id, budget_mb) {
            cx.emit(SettingsEvent::Error(format!(
                "Could not save the cache budget: {error:#}"
            )));
            return;
        }
        if let Some(budget_mb) = budget_mb {
            let evicting = cx.background_spawn(async move {
                store.evict_account_blobs(account_id, budget_mb * 1024 * 1024)
            });
            cx.spawn(async move |this, cx| {
                let freed = evicting.await;
                let _ = this.update(cx, |this, cx| {
                    match freed {
                        Ok(0) => {}
                        Ok(freed) => cx.emit(SettingsEvent::Status(format!(
                            "Cleared {} of older downloads.",
                            format_size(freed)
                        ))),
                        Err(error) => cx.emit(SettingsEvent::Error(format!(
                            "Could not clear older downloads: {error:#}"
                        ))),
                    }
                    this.refresh(cx);
                    this.measure_usage(cx);
                });
            })
            .detach();
        }
        self.refresh(cx);
    }

    /// Re-authenticate: the password dialog, or the browser sign-in for an
    /// OAuth2 account.
    pub(crate) fn reauthenticate(
        &mut self,
        account_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = self.cards.iter().find(|card| card.account.id == account_id) else {
            return;
        };
        match card.auth {
            AuthKind::Password => {
                self.reauth_account = Some((account_id, card.account.display_name.clone()));
                self.reauth_failure = None;
                self.password.update(cx, |input, cx| input.set_text("", cx));
                self.reauth.open(window, cx);
                cx.notify();
            }
            AuthKind::OAuth2 => self.sign_in_again(account_id, cx),
            AuthKind::ProtonSession => self.sign_in_to_proton_again(account_id, cx),
        }
    }

    /// Re-authenticate on a Proton account: Proton's sign-in again for
    /// this account, once Proton has ended its session. While the session
    /// is still in use it would write its own refresh token over the new
    /// one, so until then this only says there is nothing to do.
    fn sign_in_to_proton_again(&mut self, account_id: i64, cx: &mut Context<Self>) {
        let Some(card) = self.cards.iter().find(|card| card.account.id == account_id) else {
            return;
        };
        // The last failure, still standing (a retry may be under way):
        // the session ended, or was never kept.
        let status = &card.account.sync_status;
        let ended = (status.phase == SyncPhase::Failed || status.failure_count > 0)
            && status
                .last_error_message
                .as_deref()
                .is_some_and(|error| sync_failure(error) == SyncFailure::SignIn);
        if !ended {
            cx.emit(SettingsEvent::Status(
                "Proton hasn't ended Zmail's session, so there is nothing to sign in to again. If it does, this account says so here, and Re-authenticate signs you in again, keeping your mail.".to_owned(),
            ));
            return;
        }
        #[cfg(feature = "proton")]
        crate::account_setup::open_proton_sign_in_again(account_id, cx);
        #[cfg(not(feature = "proton"))]
        cx.emit(SettingsEvent::Error(
            "This build of Zmail has no Proton Mail support, so it can't sign in to Proton. A build with Proton keeps the account and signs in again.".to_owned(),
        ));
    }

    /// Try again: syncs the account now, instead of waiting for its next
    /// attempt.
    pub(crate) fn try_again(&mut self, account_id: i64, cx: &mut Context<Self>) {
        let Some(card) = self.cards.iter().find(|card| card.account.id == account_id) else {
            return;
        };
        let name = card.account.display_name.clone();
        match SettingsHost::get(cx).sync_service.sync_account(account_id) {
            Ok(_) => cx.emit(SettingsEvent::Status(format!("Trying {name} again."))),
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not start syncing {name}: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    /// The Re-authenticate dialog's Save and Sync: tests the new password
    /// against the incoming server, then saves it.
    pub(crate) fn save_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((account_id, _)) = self.reauth_account.clone() else {
            return;
        };
        let password = self.password.read(cx).text().to_owned();
        if password.is_empty() || self.reauth_checking.is_some() {
            return;
        }
        let target = SettingsHost::get(cx)
            .store
            .sync_target(account_id)
            .ok()
            .flatten();
        let Some(target) = target else {
            return self.commit_password(window, cx);
        };
        let account = Arc::new(stored_account(&target));
        self.reauth_failure = None;
        self.reauth_checking = Some(check_label(&account, CheckPart::Incoming));
        let running = SignInCheck::get(cx).spawn(
            account,
            Arc::from(password.as_str()),
            CheckPart::Incoming,
            cx,
        );
        self.reauth_check = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reauth_checking = None;
                this.reauth_check = None;
                match result {
                    Ok(()) => this.commit_password(window, cx),
                    Err(failure) => {
                        this.reauth_failure = Some(failure.summary());
                        this.password
                            .update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    /// Saves the password typed in the Re-authenticate dialog, tested or
    /// not (Save anyway), and syncs.
    pub(crate) fn commit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((account_id, name)) = self.reauth_account.clone() else {
            return;
        };
        let password = self.password.read(cx).text().to_owned();
        if password.is_empty() {
            return;
        }
        let host = SettingsHost::get(cx);
        match zmail_setup::accounts::save_password(
            host.secrets.as_ref(),
            &host.sync_service,
            account_id,
            &password,
        ) {
            Ok(()) => cx.emit(SettingsEvent::Status(format!(
                "Password saved. {name} is syncing."
            ))),
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not save the password in the keychain: {error:#}"
            ))),
        }
        self.cancel_dialogs(window, cx);
    }

    /// Runs the OAuth2 browser sign-in again with the account's stored
    /// client, off the GUI thread; the account's sync status shows the
    /// wait and the result.
    fn sign_in_again(&mut self, account_id: i64, cx: &mut Context<Self>) {
        let host = SettingsHost::get(cx);
        let (store, secrets, sync_service) = (
            host.store.clone(),
            host.secrets.clone(),
            host.sync_service.clone(),
        );
        let (preset, client) = match zmail_setup::accounts::reauthorization_client(
            &store,
            secrets.as_ref(),
            account_id,
        ) {
            Ok(found) => found,
            Err(message) => {
                cx.emit(SettingsEvent::Error(message.to_owned()));
                return;
            }
        };
        let _ = store.begin_sync_attempt(
            account_id,
            SyncPhase::Connecting,
            "Waiting for browser authorization",
        );
        let spawned = std::thread::Builder::new()
            .name("zmail-oauth2-reauth".to_owned())
            .spawn(move || {
                let tokens = client.authorize_interactively();
                zmail_setup::accounts::finish_reauthorization(
                    &store,
                    secrets.as_ref(),
                    &sync_service,
                    account_id,
                    &client,
                    tokens,
                );
            });
        match spawned {
            Ok(_) => cx.emit(SettingsEvent::Status(format!(
                "Finish signing in to {} in your browser.",
                preset.name()
            ))),
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not start the sign-in: {error}"
            ))),
        }
        self.refresh(cx);
    }

    /// Update the server: sends the identity's values in Zmail to its
    /// server identity, on the sync runtime.
    pub(crate) fn update_server_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        self.identity_on_server(identity_id, false, cx);
    }

    /// Creates an identity the server no longer lists there again.
    pub(crate) fn recreate_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        self.identity_on_server(identity_id, true, cx);
    }

    fn identity_on_server(&mut self, identity_id: i64, create: bool, cx: &mut Context<Self>) {
        if self.identity_busy.is_some() {
            return;
        }
        let Some(email) = self.identity_email(identity_id) else {
            return;
        };
        self.identity_busy = Some(identity_id);
        let sync = SettingsHost::get(cx).sync_service.clone();
        cx.spawn(async move |this, cx| {
            let result = if create {
                sync.recreate_identity_and_wait(identity_id).await
            } else {
                sync.push_identity_and_wait(identity_id).await
            };
            let _ = this.update(cx, |this, cx| {
                this.identity_busy = None;
                cx.emit(match (result, create) {
                    (Ok(()), false) => {
                        SettingsEvent::Status(format!("Updated {email} on the server."))
                    }
                    (Ok(()), true) => {
                        SettingsEvent::Status(format!("Created {email} on the server."))
                    }
                    (Err(error), false) => SettingsEvent::Error(format!(
                        "Could not update {email} on the server: {error:#}"
                    )),
                    (Err(error), true) => SettingsEvent::Error(format!(
                        "Could not create {email} on the server: {error:#}"
                    )),
                });
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Use the server's: replaces the identity's values in Zmail with the
    /// server's.
    pub(crate) fn use_server_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        let Some(email) = self.identity_email(identity_id) else {
            return;
        };
        match SettingsHost::get(cx).store.use_server_identity(identity_id) {
            Ok(()) => cx.emit(SettingsEvent::Status(format!(
                "{email} now has the server's details."
            ))),
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not take the server's details for {email}: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    fn identity_email(&self, identity_id: i64) -> Option<String> {
        self.cards
            .iter()
            .flat_map(|card| &card.identities)
            .find(|identity| identity.identity.id == identity_id)
            .map(|identity| identity.identity.email_address.clone())
    }

    /// The notice for an identity that differs from the server, or that
    /// the server no longer lists, with its two choices.
    fn render_identity_notice(
        &self,
        identity: &SyncedIdentity,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use zmail_setup::identities::{
            RECREATE, REMOVE, UPDATE_SERVER, USE_SERVER, differences, notice_text, notice_title,
        };
        let identity_id = identity.identity.id;
        let busy = self.identity_busy.is_some();
        let mut banner = Banner::new(
            ("identity-notice", identity_id as u64),
            notice_text(identity),
        )
        .tone(BannerTone::Caution)
        .title(notice_title(identity).unwrap_or_default());
        for (field, ours, theirs) in differences(identity) {
            banner = banner
                .detail(format!("In Zmail, {field}"), ours)
                .detail(format!("On the server, {field}"), theirs);
        }
        let button = |name: &'static str,
                      label: &'static str,
                      action: fn(&mut Self, i64, &mut Context<Self>)| {
            div()
                .debug_selector(move || format!("{name}-{identity_id}"))
                .child(
                    Button::new((name, identity_id as u64), label)
                        .size(ButtonSize::Small)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| action(this, identity_id, cx))),
                )
        };
        let banner = if identity.status == IdentitySyncStatus::NotOnServer {
            banner
                .action(button(
                    "identity-recreate",
                    RECREATE,
                    Self::recreate_identity,
                ))
                .action(
                    div()
                        .debug_selector(move || format!("identity-remove-{identity_id}"))
                        .child(
                            Button::new(("identity-remove", identity_id as u64), REMOVE)
                                .size(ButtonSize::Small)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_remove_identity(identity_id, window, cx)
                                })),
                        ),
                )
        } else {
            banner
                .action(button(
                    "identity-update-server",
                    UPDATE_SERVER,
                    Self::update_server_identity,
                ))
                .action(button(
                    "identity-use-server",
                    USE_SERVER,
                    Self::use_server_identity,
                ))
        };
        div().pt(px(SPACE_3)).child(banner).into_any_element()
    }

    /// Open the Remove confirmation for `account_id`.
    pub(crate) fn confirm_remove(
        &mut self,
        account_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = self.cards.iter().find(|card| card.account.id == account_id) else {
            return;
        };
        let impact = match SettingsHost::get(cx)
            .store
            .account_removal_impact(account_id)
        {
            Ok(impact) => impact,
            Err(error) => {
                cx.emit(SettingsEvent::Error(format!(
                    "Could not check the account: {error:#}"
                )));
                return;
            }
        };
        self.removal = Some(PendingRemoval {
            account_id,
            name: card.account.display_name.clone(),
            impact,
        });
        // Focus starts on Cancel, the safe choice.
        self.remove
            .open_focusing(&self.remove_cancel.clone(), window, cx);
        cx.notify();
    }

    /// Remove the account the confirmation is for: its local data, then
    /// its keychain secrets.
    pub(crate) fn remove_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(removal) = self.removal.take() else {
            return;
        };
        self.remove.close(window, cx);
        if removal_blocker(&removal.impact).is_some() {
            return;
        }
        let host = SettingsHost::get(cx);
        let forgot = match zmail_setup::accounts::remove_account(
            &host.store,
            host.secrets.as_ref(),
            Some(&host.sync_service),
            removal.account_id,
        ) {
            Ok(()) => Ok(()),
            Err(RemoveError::LocalData(error)) => {
                cx.emit(SettingsEvent::Error(format!(
                    "Could not remove {}: {error:#}",
                    removal.name
                )));
                return;
            }
            Err(RemoveError::Keychain(error)) => Err(error),
        };
        if self.focused_account == Some(removal.account_id) {
            self.focused_account = None;
        }
        refresh_workspace(cx);
        self.refresh(cx);
        match forgot {
            Ok(()) => cx.emit(SettingsEvent::Status(format!(
                "Removed {} from this computer. Its mail on the server is untouched.",
                removal.name
            ))),
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Removed {}, but its keychain entries could not all be deleted: {error:#}",
                removal.name
            ))),
        }
    }

    fn cancel_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password.update(cx, |input, cx| input.set_text("", cx));
        self.reauth_checking = None;
        self.reauth_check = None;
        self.reauth_failure = None;
        self.reauth.close(window, cx);
        self.remove.close(window, cx);
        self.reauth_account = None;
        self.removal = None;
        self.close_identity_dialogs(window, cx);
        cx.notify();
    }

    fn render_card(
        &self,
        card: &AccountCard,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account = &card.account;
        let account_id = account.id;
        let status = &account.sync_status;
        let (status_icon, status_color, _) = sync_badge(status.phase, colors);
        let failing = matches!(status.phase, SyncPhase::Failed | SyncPhase::Backoff);
        let status_text = match (&status.last_error_message, failing) {
            // A server that couldn't be reached is said plainly; the error
            // is in the Fix… banner's details.
            (Some(error), true) if sync_failure(error) == SyncFailure::Unreachable => format!(
                "{}: {}",
                status.phase.label(),
                unreachable_sentence(server_host(&account.endpoint).as_deref())
            ),
            (Some(error), true) => format!("{}: {error}", status.phase.label()),
            _ => format!("{} \u{2022} {}", status.phase.label(), status.detail),
        };
        let usage = card
            .usage_bytes
            .map(|bytes| format!(" Using {} now.", format_size(bytes)))
            .unwrap_or_default();
        let budget_description: SharedString = format!(
            "Downloaded message bodies and attachments kept on this computer. Over the limit, the oldest are cleared and download again when opened.{usage}"
        )
        .into();
        let focused = self.focused_account == Some(account_id);
        let card_id = format!("account-card-{account_id}");
        div()
            .id(SharedString::from(card_id.clone()))
            .debug_selector(move || card_id.clone())
            // Names the card's controls (Re-authenticate, Remove, Colour…)
            // for a screen reader.
            .accessible(Role::Group, account.display_name.clone())
            .aria_description(format!(
                "{}, {}. {status_text}",
                account.username,
                account.protocol.label()
            ))
            .flex()
            .flex_col()
            .mt(px(SPACE_4))
            .px(px(SPACE_4))
            .pt(px(SPACE_3))
            .rounded(px(RADIUS_LG))
            .bg(rgb(colors.surface))
            .border_1()
            .border_color(rgb(if focused {
                colors.focus_ring
            } else {
                colors.border
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .pb(px(SPACE_3))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_1))
                            .flex_1()
                            .min_w_0()
                            .child(dotted_label(
                                account.color(),
                                div()
                                    .type_style(TYPE_BODY_STRONG)
                                    .ellipsis()
                                    .child(account.display_name.clone()),
                            ))
                            .child(
                                div()
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(colors.muted))
                                    .ellipsis()
                                    .child(format!(
                                        "{} \u{2022} {}",
                                        account.username,
                                        account.protocol.label()
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_1))
                                    .type_style(TYPE_CAPTION)
                                    .text_color(rgb(if failing {
                                        colors.danger
                                    } else {
                                        colors.muted
                                    }))
                                    .child(icon(status_icon, ICON_SIZE_SM, status_color))
                                    .child(div().flex_1().min_w_0().child(status_text)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(SPACE_2))
                            .child(
                                div()
                                    .debug_selector(move || format!("reauth-{account_id}"))
                                    .child(
                                        Button::new(
                                            ("reauth", account_id as u64),
                                            "Re-authenticate",
                                        )
                                        .size(ButtonSize::Small)
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.reauthenticate(account_id, window, cx)
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .debug_selector(move || format!("remove-{account_id}"))
                                    .child(
                                        Button::new(("remove", account_id as u64), "Remove…")
                                            .size(ButtonSize::Small)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.confirm_remove(account_id, window, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
            .children(
                card.identities
                    .iter()
                    .map(|identity| self.render_identity_notice(identity, cx))
                    .collect::<Vec<_>>(),
            )
            .child(self.render_identities(card, colors, cx))
            .child(setting_row(
                "Colour",
                Some("Marks this account's mail and folders.".into()),
                card.color.clone(),
                colors,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_4))
                    .py(px(SPACE_3))
                    .child(setting_text(
                        "Cache budget",
                        Some(budget_description),
                        colors,
                    ))
                    .child(div().flex_none().child(card.budget.clone())),
            )
            .into_any_element()
    }

    /// The banner Fix… (or an Outbox message's Re-authenticate) opens the
    /// section with, while that account fails to sync or to send.
    fn render_fix_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card = self
            .cards
            .iter()
            .find(|card| Some(card.account.id) == self.focused_account)?;
        let status = &card.account.sync_status;
        let name = &card.account.display_name;
        let (title, error, failure) =
            if matches!(status.phase, SyncPhase::Failed | SyncPhase::Backoff) {
                let error = status
                    .last_error_message
                    .clone()
                    .unwrap_or_else(|| "The last sync failed.".to_owned());
                let failure = sync_failure(&error);
                (format!("{name} can't sync"), error, failure)
            } else {
                // The Outbox's refusals are all sign-in ones.
                let error = card.refused_send.clone()?;
                (format!("{name} can't send"), error, SyncFailure::SignIn)
            };
        // A failure only signing in again fixes says why and how, a server
        // that couldn't be reached says so plainly, both with the stored
        // error under Show details; any other shows the error.
        let reason = ReauthReason::of_message(&error);
        let title = match (reason, failure) {
            (Some(ReauthReason::GrantExpired), _) => format!("{name}'s sign-in has expired"),
            (_, SyncFailure::Unreachable) => format!("{name} can't connect"),
            _ => title,
        };
        let banner = match (reason, failure) {
            (Some(reason), _) => Banner::new(
                "account-fix",
                reauth_advice(reason, card.auth, card.google_oauth),
            )
            .detail("Error", error),
            (None, SyncFailure::SignIn) => Banner::new(
                "account-fix",
                "The server refused Zmail's sign-in. If you changed your password, or your provider wants a new app password, sign in again.",
            )
            .detail("Error", error),
            (None, SyncFailure::Unreachable) => Banner::new(
                "account-fix",
                unreachable_sentence(server_host(&card.account.endpoint).as_deref()),
            )
            .detail("Error", error),
            (None, SyncFailure::Other) => Banner::new("account-fix", error),
        };
        let account_id = card.account.id;
        let mut banner = banner.tone(BannerTone::Caution).title(title);
        if failure != SyncFailure::SignIn {
            banner = banner.action(
                div().debug_selector(|| "account-fix-retry".into()).child(
                    Button::new("account-fix-retry", "Try again")
                        .size(ButtonSize::Small)
                        .icon("refresh-cw")
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.try_again(account_id, cx)),
                        ),
                ),
            );
        }
        if failure != SyncFailure::Unreachable {
            banner = banner.action(
                div().debug_selector(|| "account-fix-reauth".into()).child(
                    Button::new("account-fix-reauth", "Re-authenticate")
                        .size(ButtonSize::Small)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reauthenticate(account_id, window, cx)
                        })),
                ),
            );
        }
        // An expired Google sign-in links to the guide's fix.
        let banner = if reason == Some(ReauthReason::GrantExpired) && card.google_oauth {
            banner.action(
                div().debug_selector(|| "account-fix-guide".into()).child(
                    Button::new("account-fix-guide", "How to stop this")
                        .size(ButtonSize::Small)
                        .icon("external-link")
                        .on_click(|_, _, cx: &mut App| cx.open_url(zmail_setup::google::GUIDE_URL)),
                ),
            )
        } else {
            banner
        };
        Some(div().pt(px(SPACE_2)).child(banner).into_any_element())
    }

    fn render_reauth_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.reauth.is_open() {
            return None;
        }
        let (_, name) = self.reauth_account.as_ref()?;
        let colors = ComponentTheme::get(cx).colors;
        let checking = self.reauth_checking.is_some();
        // While the password is tested: what is happening. After a failed
        // test: why, read out as it appears.
        let status = match (&self.reauth_checking, &self.reauth_failure) {
            (Some(label), _) => Some(
                div()
                    .debug_selector(|| "reauth-checking".into())
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(readable("reauth-checking", format!("{label}…")))
                    .into_any_element(),
            ),
            (None, Some(failure)) => {
                let text = SharedString::from(failure.clone());
                Some(
                    div()
                        .id("reauth-failure")
                        .debug_selector(|| "reauth-failure".into())
                        .accessible(Role::Alert, text.clone())
                        .aria_value(text.clone())
                        .a11y_flags(A11yFlags {
                            live: Some(Politeness::Assertive),
                            ..A11yFlags::default()
                        })
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.danger))
                        .child(text)
                        .into_any_element(),
                )
            }
            (None, None) => None,
        };
        let failed = self.reauth_failure.is_some();
        Some(
            Modal::new("reauth-dialog", self.reauth.focus_handle())
                .title(format!("Sign in to {name} again"))
                .description(
                    "Zmail keeps the password in your system keychain. If your provider uses app passwords, enter a new one here. Zmail tests it before saving it.",
                )
                .child(self.password.clone())
                .children(status)
                .footer(
                    Button::new("reauth-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_dialogs(window, cx))),
                )
                .when(failed, |this| {
                    this.footer(
                        div().debug_selector(|| "reauth-save-anyway".into()).child(
                            Button::new("reauth-save-anyway", "Save anyway")
                                .on_click(cx.listener(|this, _, window, cx| this.commit_password(window, cx))),
                        ),
                    )
                })
                .footer(
                    div().debug_selector(|| "reauth-save".into()).child(
                        Button::new("reauth-save", "Save and Sync")
                            .primary()
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, window, cx| this.save_password(window, cx))),
                    ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.cancel_dialogs(window, cx)))
                .into_any_element(),
        )
    }

    fn render_remove_dialog(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.remove.is_open() {
            return None;
        }
        let removal = self.removal.as_ref()?;
        let blocker = removal_blocker(&removal.impact);
        let blocked = blocker.is_some();
        let lines = removal_lines(&removal.impact);
        Some(
            Modal::new("remove-account-dialog", self.remove.focus_handle())
                .title(format!("Remove \u{201c}{}\u{201d}?", removal.name))
                .description(removal_description(&removal.name))
                .children(lines.into_iter().map(|line| {
                    div()
                        .type_style(TYPE_BODY_STRONG)
                        .child(line)
                        .into_any_element()
                }))
                .when_some(blocker, |this, blocker| {
                    this.child(div().text_color(rgb(colors.danger)).child(blocker))
                })
                .footer(
                    div().debug_selector(|| "remove-cancel".into()).child(
                        Button::new("remove-cancel", "Cancel")
                            .track_focus(&self.remove_cancel)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_dialogs(window, cx)),
                            ),
                    ),
                )
                .footer(
                    div().debug_selector(|| "remove-confirm".into()).child(
                        Button::new("remove-confirm", "Remove Account")
                            .danger()
                            .disabled(blocked)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.remove_account(window, cx)),
                            ),
                    ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.cancel_dialogs(window, cx)))
                .into_any_element(),
        )
    }
}

/// The colour picker's index for a saved colour: 0 is Automatic.
fn color_index(color: Option<AccountColor>) -> usize {
    color.map_or(0, |color| color.index() + 1)
}

fn budget_select(options: &[Option<u64>], cx: &mut Context<AccountsSettings>) -> Entity<Select> {
    let labels: Vec<String> = options.iter().map(|budget| budget_label(*budget)).collect();
    cx.new(|cx| {
        Select::new(labels, None, cx)
            .label("Cache budget")
            .width(px(SELECT_WIDTH))
    })
}

/// Re-read the store in the main window, so a colour or a removal shows
/// there at once.
fn refresh_workspace(cx: &mut App) {
    update_workspace(cx, |view, _, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
}

impl Render for AccountsSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let cards: Vec<AnyElement> = self
            .cards
            .iter()
            .map(|card| self.render_card(card, &colors, cx))
            .collect();
        let empty = cards.is_empty();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(section_heading("Accounts"))
                    .child(
                        div()
                            .debug_selector(|| "settings-add-account".into())
                            .child(
                                Button::new("settings-add-account", "Add Account…")
                                    .icon("user-plus")
                                    .on_click(|_, _, cx| {
                                        crate::account_setup::open_add_account(cx)
                                    }),
                            ),
                    ),
            )
            .children(self.render_fix_banner(cx))
            .when(empty, |this| {
                this.child(
                    div()
                        .pt(px(SPACE_2))
                        .text_color(rgb(colors.muted))
                        .child("No accounts yet. Add one to start reading mail."),
                )
            })
            .children(cards)
            .children(self.render_reauth_dialog(cx))
            .children(self.render_remove_dialog(&colors, cx))
            .children(self.render_identity_dialog(&colors, cx))
            .children(self.render_identity_remove_dialog(&colors, cx))
    }
}
