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
//! - **Re-authenticate** asks for the password again, or for OAuth2
//!   accounts runs the browser sign-in again with the stored client.
//! - **Remove** confirms in a dialog that says what goes: the account's
//!   local data and its keychain secrets, never the mail on the server.
//!   It also lists unsent Outbox messages and drafts, which exist only
//!   on this computer and go with the account. A message being sent at
//!   that moment blocks removal until it has gone.
//!
//! The sidebar's Fix… on a failing account opens this section on that
//! account ([`AccountsSettings::focus_account`]), as does Re-authenticate
//! on an Outbox message the server refused for the account's sign-in.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, IntoElement, Render, Role,
    SharedString, Subscription, Window, div, prelude::*, px, rgb,
};
use zmail_core::{
    AccountColor, AccountSummary, AuthKind, OutboxJobState, SyncPhase, reauth::ReauthReason,
    text::count_noun,
};
use zmail_security::SecretSlot;
use zmail_store::AccountRemovalImpact;

use super::{
    SettingsEvent, SettingsHost, dotted_label, section_heading, setting_row, setting_text,
    update_workspace,
};
use crate::a11y::AccessibleExt;
use crate::components::{
    Banner, BannerTone, Button, ButtonSize, ComponentTheme, Modal, ModalState, Select, SelectEvent,
    TextInput, TextInputEvent,
};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_LG, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY_STRONG, TYPE_CAPTION,
    TypeStyleExt, UiColors,
};
use crate::widgets::{EllipsisExt, format_size, icon, sync_badge};

/// What the fix banner says about a message the server refused for the
/// account's sign-in, ending with the stored error.
fn refused_send_message(auth: AuthKind, error: &str) -> String {
    let advice = match auth {
        AuthKind::OAuth2 => {
            "Signing in again asks your provider for permission to send mail as well as read it. Microsoft 365 accounts added before zmail asked for that need it once."
        }
        _ => {
            "The outgoing server no longer accepts the saved password. Enter the current one, or a new app password."
        }
    };
    format!("{advice} Messages waiting in the Outbox send once you have signed in. {error}")
}

/// The cache budgets offered, in MiB; `None` is no limit.
const CACHE_BUDGETS: [Option<u64>; 7] = [
    None,
    Some(250),
    Some(500),
    Some(1024),
    Some(2 * 1024),
    Some(5 * 1024),
    Some(10 * 1024),
];

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
    /// The Remove confirmation.
    remove: ModalState,
    removal: Option<PendingRemoval>,
    remove_cancel: FocusHandle,
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
            remove: ModalState::new(cx),
            removal: None,
            remove_cancel: cx.focus_handle(),
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
            let auth = store
                .sync_target(account.id)
                .ok()
                .flatten()
                .map(|target| target.auth)
                .unwrap_or_default();
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
            card.account = account;
            card.auth = auth;
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
                self.password.update(cx, |input, cx| input.set_text("", cx));
                self.reauth.open(window, cx);
                cx.notify();
            }
            AuthKind::OAuth2 => self.sign_in_again(account_id, cx),
            // Signing in to Proton again arrives with Proton onboarding
            // (#237); until then no Proton account can exist here.
            AuthKind::ProtonSession => {}
        }
    }

    /// Save the password typed in the Re-authenticate dialog and sync.
    pub(crate) fn save_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((account_id, name)) = self.reauth_account.clone() else {
            return;
        };
        let password = self.password.read(cx).text().to_owned();
        if password.is_empty() {
            return;
        }
        let host = SettingsHost::get(cx);
        match host.secrets.set_account_secret(account_id, &password) {
            Ok(()) => {
                let _ = host.sync_service.sync_account(account_id);
                let _ = host.sync_service.sync_watchers();
                let _ = host.sync_service.retry_refused_sends(account_id);
                cx.emit(SettingsEvent::Status(format!(
                    "Password saved. {name} is syncing."
                )));
            }
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
        let config = store
            .sync_target(account_id)
            .ok()
            .flatten()
            .and_then(|target| target.oauth2);
        let Some((preset, config)) = config.and_then(|config| {
            zmail_auth::OAuth2Preset::for_token_endpoint(&config.token_endpoint)
                .map(|preset| (preset, config))
        }) else {
            cx.emit(SettingsEvent::Error(
                "This account signs in through a custom OAuth service, so Zmail cannot start its sign-in again. Remove the account and add it again."
                    .to_owned(),
            ));
            return;
        };
        let client_secret = secrets
            .get_account_secret_from_slot(account_id, SecretSlot::ClientSecret)
            .ok();
        let client = preset.client(
            config.client_id,
            client_secret,
            zmail_auth::DEFAULT_REDIRECT_PORT,
        );
        let _ = store.begin_sync_attempt(
            account_id,
            SyncPhase::Connecting,
            "Waiting for browser authorization",
        );
        let spawned = std::thread::Builder::new()
            .name("zmail-oauth2-reauth".to_owned())
            .spawn(move || {
                let result = client.authorize_interactively().and_then(|tokens| {
                    zmail_sync::persist_oauth2_grant(
                        &store,
                        secrets.as_ref(),
                        account_id,
                        &client,
                        &tokens,
                    )
                });
                match result {
                    Ok(()) => {
                        let _ = sync_service.sync_account(account_id);
                        let _ = sync_service.sync_watchers();
                        // Messages refused for the old sign-in go now.
                        let _ = sync_service.retry_refused_sends(account_id);
                    }
                    Err(error) => {
                        let _ = store.record_sync_failure(
                            account_id,
                            "Browser authorization failed",
                            error.to_string(),
                        );
                    }
                }
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
        if let Err(error) = host.store.delete_account(removal.account_id) {
            cx.emit(SettingsEvent::Error(format!(
                "Could not remove {}: {error:#}",
                removal.name
            )));
            return;
        }
        let secrets = host.secrets.clone();
        let mut forgot = secrets.delete_account_secret(removal.account_id);
        for slot in [
            SecretSlot::Password,
            SecretSlot::ClientSecret,
            SecretSlot::AccessToken,
            SecretSlot::RefreshToken,
        ] {
            forgot = forgot.and(secrets.delete_account_secret_in_slot(removal.account_id, slot));
        }
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
        self.reauth.close(window, cx);
        self.remove.close(window, cx);
        self.reauth_account = None;
        self.removal = None;
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
        let (title, message) = if matches!(status.phase, SyncPhase::Failed | SyncPhase::Backoff) {
            (
                format!("{name} can't sync"),
                status
                    .last_error_message
                    .clone()
                    .unwrap_or_else(|| "The last sync failed.".to_owned()),
            )
        } else {
            let refused = card.refused_send.as_ref()?;
            (
                format!("{name} can't send"),
                refused_send_message(card.auth, refused),
            )
        };
        let account_id = card.account.id;
        Some(
            div()
                .pt(px(SPACE_2))
                .child(
                    Banner::new("account-fix", message)
                        .tone(BannerTone::Caution)
                        .title(title)
                        .action(
                            Button::new("account-fix-reauth", "Re-authenticate")
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.reauthenticate(account_id, window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_reauth_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.reauth.is_open() {
            return None;
        }
        let (_, name) = self.reauth_account.as_ref()?;
        Some(
            Modal::new("reauth-dialog", self.reauth.focus_handle())
                .title(format!("Sign in to {name} again"))
                .description(
                    "Zmail keeps the password in your system keychain. If your provider uses app passwords, enter a new one here.",
                )
                .child(self.password.clone())
                .footer(
                    Button::new("reauth-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_dialogs(window, cx))),
                )
                .footer(
                    div().debug_selector(|| "reauth-save".into()).child(
                        Button::new("reauth-save", "Save and Sync")
                            .primary()
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

/// What the Remove confirmation says goes, and what stays.
pub(crate) fn removal_description(name: &str) -> String {
    format!(
        "This deletes {name}'s local data from this computer: its downloaded mail, attachments and search index, and its password and sign-in tokens in the system keychain. Mail on the server is not touched, and adding the account again downloads it again."
    )
}

/// The unsent mail and drafts that go with the account, which exist only
/// on this computer.
pub(crate) fn removal_lines(impact: &AccountRemovalImpact) -> Vec<String> {
    let mut lines = Vec::new();
    if impact.unsent_messages > 0 {
        lines.push(format!(
            "{} in the Outbox will be deleted without being sent.",
            count_noun(impact.unsent_messages, "unsent message", "unsent messages")
        ));
    }
    if impact.drafts > 0 {
        lines.push(format!(
            "{} will be deleted.",
            count_noun(impact.drafts, "draft", "drafts")
        ));
    }
    lines
}

/// Why the account cannot be removed right now, if it cannot.
pub(crate) fn removal_blocker(impact: &AccountRemovalImpact) -> Option<String> {
    if impact.sending_messages > 0 {
        return Some(
            "A message from this account is being sent. Try again once it has gone.".to_owned(),
        );
    }
    None
}

/// The colour picker's index for a saved colour: 0 is Automatic.
fn color_index(color: Option<AccountColor>) -> usize {
    color.map_or(0, |color| color.index() + 1)
}

fn budget_options(saved_mb: Option<u64>) -> Vec<Option<u64>> {
    let mut options = CACHE_BUDGETS.to_vec();
    if !options.contains(&saved_mb) {
        options.push(saved_mb);
        options.sort_by_key(|budget| budget.unwrap_or(0));
    }
    options
}

fn budget_select(options: &[Option<u64>], cx: &mut Context<AccountsSettings>) -> Entity<Select> {
    let labels: Vec<String> = options.iter().map(|budget| budget_label(*budget)).collect();
    cx.new(|cx| {
        Select::new(labels, None, cx)
            .label("Cache budget")
            .width(px(SELECT_WIDTH))
    })
}

/// How a cache budget reads in the picker.
pub(crate) fn budget_label(budget_mb: Option<u64>) -> String {
    match budget_mb {
        None => "No limit".to_owned(),
        Some(mb) if mb >= 1024 && mb % 1024 == 0 => format!("{} GB", mb / 1024),
        Some(mb) => format!("{mb} MB"),
    }
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
    }
}

#[cfg(test)]
mod tests {
    use zmail_store::AccountRemovalImpact;

    use super::*;

    #[test]
    fn budgets_read_in_megabytes_and_gigabytes() {
        assert_eq!(budget_label(None), "No limit");
        assert_eq!(budget_label(Some(250)), "250 MB");
        assert_eq!(budget_label(Some(2048)), "2 GB");
        assert_eq!(budget_label(Some(1500)), "1500 MB");
        assert_eq!(budget_options(Some(300)).len(), CACHE_BUDGETS.len() + 1);
        assert_eq!(budget_options(Some(500)).len(), CACHE_BUDGETS.len());
    }

    #[test]
    fn the_confirmation_lists_unsent_mail_and_drafts() {
        let impact = AccountRemovalImpact {
            unsent_messages: 2,
            sending_messages: 0,
            drafts: 1,
        };
        assert_eq!(
            removal_lines(&impact),
            [
                "2 unsent messages in the Outbox will be deleted without being sent.",
                "1 draft will be deleted.",
            ]
        );
        assert_eq!(removal_blocker(&impact), None);
        assert!(
            removal_blocker(&AccountRemovalImpact {
                sending_messages: 1,
                ..impact
            })
            .is_some()
        );
        assert!(removal_lines(&AccountRemovalImpact::default()).is_empty());
        let description = removal_description("Work");
        assert!(description.contains("Mail on the server is not touched"));
        assert!(description.contains("keychain"));
    }
}
