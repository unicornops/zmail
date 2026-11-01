//! Settings → Accounts → an account's identities (#366): the list on the
//! card, the editor dialog for editing and adding one, the remove
//! confirmation, and Make primary. The fields, validation, wording and
//! each account kind's rules are `zmail_setup::identity_editor`'s, so the
//! terminal UI's editor behaves the same.
//!
//! - **Edit…** opens the dialog on the identity's name, address, reply-to
//!   and multi-line signature. The address is read-only for a JMAP identity
//!   linked to a server identity and for a Proton address. On a linked JMAP
//!   identity, Save and update the server sends the edit up at once.
//! - **Add Identity…** opens it empty; a JMAP account can create the new
//!   identity on the server too. Proton accounts say where addresses are
//!   added instead.
//! - **Remove…** asks first, in a modal, and offers to remove a linked
//!   JMAP identity from the server too when the server allows it.
//! - **Make primary** picks the identity new mail starts from.
//! - **Hidden identities** lists the server identities removed from Zmail
//!   only, which syncs leave out, each with **Bring back**: it is back at
//!   once, and a sync starts to confirm it.
//!
//! Server requests run on the sync runtime and report with a toast, like
//! the identity notices'.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable as _, IntoElement, Role,
    SharedString, Window, div, prelude::*, px, rgb,
};
use zmail_core::identity_sync::SyncedIdentity;
use zmail_setup::identity_editor::{
    self, ADD, ADD_AND_CREATE, ADD_IN_ZMAIL, AccountIdentities, BRING_BACK, EDIT, FormMode,
    HIDDEN_HEADING, HIDDEN_NOTE, IdentityField, IdentityForm, MAKE_PRIMARY, PRIMARY,
    REMOVE_EVERYWHERE, REMOVE_HERE, SAVE, SAVE_AND_UPDATE, messages,
};
use zmail_store::HiddenIdentity;

use super::{AccountCard, AccountsSettings, refresh_workspace};
use crate::a11y::AccessibleExt;
use crate::components::{
    Badge, BadgeTone, Button, ButtonSize, Modal, ModalState, TextInput, TextInputEvent,
};
use crate::settings::{SettingsEvent, SettingsHost};
use crate::styles::{
    SPACE_1, SPACE_2, SPACE_3, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TypeStyleExt, UiColors,
};
use crate::widgets::{EllipsisExt, input_card};

/// Width of the identity dialog.
const DIALOG_WIDTH: f32 = 480.0;
/// Width of the remove confirmation when it offers the server too, so its
/// three buttons fit on one row.
const REMOVE_EVERYWHERE_WIDTH: f32 = 580.0;

/// The identity being removed, while the confirmation shows.
pub(super) struct PendingIdentityRemoval {
    account: AccountIdentities,
    identity: SyncedIdentity,
    /// Why it can't be removed, if it can't (a draft is written from it).
    blocker: Option<String>,
}

/// The identity dialog's and the remove confirmation's state.
pub(super) struct IdentityDialogs {
    dialog: ModalState,
    /// The account and what the dialog does, while it is open.
    editing: Option<(AccountIdentities, FormMode)>,
    name: Entity<TextInput>,
    address: Entity<TextInput>,
    reply_to: Entity<TextInput>,
    signature: Entity<TextInput>,
    /// The problems the last Save found, field by field.
    errors: Vec<(IdentityField, String)>,
    remove: ModalState,
    removal: Option<PendingIdentityRemoval>,
    remove_cancel: FocusHandle,
}

impl IdentityDialogs {
    pub(super) fn new(window: &mut Window, cx: &mut Context<AccountsSettings>) -> Self {
        let field = |field: IdentityField, cx: &mut Context<AccountsSettings>| {
            cx.new(|cx| {
                TextInput::new(cx)
                    .label(field.label())
                    .placeholder(field.placeholder())
            })
        };
        let name = field(IdentityField::Name, cx);
        let address = field(IdentityField::Address, cx);
        let reply_to = field(IdentityField::ReplyTo, cx);
        let signature = cx.new(|cx| {
            TextInput::new(cx)
                .multiline()
                .rows(3, 8)
                .label(IdentityField::Signature.label())
                .placeholder(IdentityField::Signature.placeholder())
        });
        // Enter in a one-line field saves, as in the other dialogs.
        for input in [&name, &address, &reply_to] {
            cx.subscribe_in(
                input,
                window,
                |this, _, event: &TextInputEvent, window, cx| {
                    if *event == TextInputEvent::Submitted {
                        this.save_identity_form(false, window, cx);
                    }
                },
            )
            .detach();
        }
        Self {
            dialog: ModalState::new(cx),
            editing: None,
            name,
            address,
            reply_to,
            signature,
            errors: Vec::new(),
            remove: ModalState::new(cx),
            removal: None,
            remove_cancel: cx.focus_handle(),
        }
    }

    fn input(&self, field: IdentityField) -> &Entity<TextInput> {
        match field {
            IdentityField::Name => &self.name,
            IdentityField::Address => &self.address,
            IdentityField::ReplyTo => &self.reply_to,
            IdentityField::Signature => &self.signature,
        }
    }

    fn form(&self, cx: &Context<AccountsSettings>) -> IdentityForm {
        let mut form = IdentityForm::default();
        for field in IdentityField::ALL {
            form.set_value(field, self.input(field).read(cx).text());
        }
        form
    }
}

impl AccountsSettings {
    /// Opens the identity dialog on identity `identity_id` of
    /// `account_id`, or empty to add one when `identity_id` is `None`.
    pub(crate) fn open_identity_editor(
        &mut self,
        account_id: i64,
        identity_id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self
            .cards
            .iter()
            .find(|card| card.account.id == account_id)
            .map(|card| card.all_identities.clone())
        else {
            return;
        };
        let (mode, form, address_editable) = match identity_id {
            Some(identity_id) => {
                let Some(identity) = account.find(identity_id) else {
                    return;
                };
                (
                    FormMode::Edit(identity_id),
                    IdentityForm::from_identity(identity),
                    account.address_editable(identity),
                )
            }
            None if account.can_add() => (FormMode::Add, IdentityForm::default(), true),
            None => return,
        };
        let dialogs = &self.identity_dialogs;
        for field in IdentityField::ALL {
            let text = form.value(field).to_owned();
            dialogs
                .input(field)
                .update(cx, |input, cx| input.set_text(text, cx));
        }
        dialogs.address.update(cx, |input, cx| {
            input.set_disabled(!address_editable, window, cx)
        });
        let first = dialogs.name.read(cx).focus_handle(cx);
        self.identity_dialogs.errors.clear();
        self.identity_dialogs.editing = Some((account, mode));
        self.identity_dialogs
            .dialog
            .open_focusing(&first, window, cx);
        cx.notify();
    }

    /// Saves the identity dialog: in Zmail, then, with `on_server`, on the
    /// server too (Save and update the server, or Add and create on the
    /// server). Problems keep the dialog open, under their fields.
    pub(crate) fn save_identity_form(
        &mut self,
        on_server: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((account, mode)) = self.identity_dialogs.editing.clone() else {
            return;
        };
        let form = self.identity_dialogs.form(cx);
        let errors = account.validate(mode, &form);
        if !errors.is_empty() {
            self.identity_dialogs.errors = errors;
            cx.notify();
            return;
        }
        let store = SettingsHost::get(cx).store.clone();
        let saved = match mode {
            FormMode::Edit(identity_id) => {
                identity_editor::save_identity(&store, &account, identity_id, &form)
                    .map(|()| identity_id)
            }
            FormMode::Add => identity_editor::add_identity(&store, &account, &form),
        };
        let identity_id = match saved {
            Ok(identity_id) => identity_id,
            Err(error) => {
                cx.emit(SettingsEvent::Error(format!("Could not save: {error:#}")));
                return;
            }
        };
        self.close_identity_dialogs(window, cx);
        refresh_workspace(cx);
        let email = store
            .identity_link(identity_id)
            .ok()
            .flatten()
            .map(|link| link.email)
            .unwrap_or_else(|| form.address().to_owned());
        let create = mode == FormMode::Add;
        // Only a change the server lacks goes up; an unchanged identity
        // has nothing to send.
        let send = on_server
            && (create
                || store
                    .identity_link(identity_id)
                    .ok()
                    .flatten()
                    .is_some_and(|link| link.status.differs()));
        if send {
            self.identity_to_server(identity_id, email, create, cx);
        } else {
            cx.emit(SettingsEvent::Status(if create {
                messages::added(&email)
            } else {
                messages::saved(&email)
            }));
        }
        self.refresh(cx);
    }

    /// Sends a saved identity to the server (an update, or a create for a
    /// new one), on the sync runtime.
    fn identity_to_server(
        &mut self,
        identity_id: i64,
        email: String,
        create: bool,
        cx: &mut Context<Self>,
    ) {
        self.identity_busy = Some(identity_id);
        let sync = SettingsHost::get(cx).sync_service.clone();
        cx.spawn(async move |this, cx| {
            let result = if create {
                sync.create_identity_on_server_and_wait(identity_id).await
            } else {
                sync.push_identity_and_wait(identity_id).await
            };
            let _ = this.update(cx, |this, cx| {
                this.identity_busy = None;
                cx.emit(match (result, create) {
                    (Ok(()), true) => SettingsEvent::Status(messages::added_and_created(&email)),
                    (Ok(()), false) => SettingsEvent::Status(messages::saved_and_updated(&email)),
                    (Err(error), _) => SettingsEvent::Error(messages::server_refused(
                        &email,
                        &format!("{error:#}"),
                    )),
                });
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Opens the remove confirmation for identity `identity_id`.
    pub(crate) fn confirm_remove_identity(
        &mut self,
        identity_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((account, identity)) = self.cards.iter().find_map(|card| {
            card.all_identities
                .find(identity_id)
                .map(|identity| (card.all_identities.clone(), identity.clone()))
        }) else {
            return;
        };
        let blocker = match SettingsHost::get(cx)
            .store
            .identity_removal_blocker(identity_id)
        {
            Ok(blocker) => blocker,
            Err(error) => {
                cx.emit(SettingsEvent::Error(format!(
                    "Could not check the identity: {error:#}"
                )));
                return;
            }
        };
        self.identity_dialogs.removal = Some(PendingIdentityRemoval {
            account,
            identity,
            blocker,
        });
        // Focus starts on Cancel, the safe choice.
        let cancel = self.identity_dialogs.remove_cancel.clone();
        self.identity_dialogs
            .remove
            .open_focusing(&cancel, window, cx);
        cx.notify();
    }

    /// Removes the identity the confirmation is for: from Zmail, or with
    /// `everywhere` from the server first.
    pub(crate) fn remove_identity_confirmed(
        &mut self,
        everywhere: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(removal) = self.identity_dialogs.removal.take() else {
            return;
        };
        self.close_identity_dialogs(window, cx);
        if removal.blocker.is_some() {
            return;
        }
        let identity_id = removal.identity.identity.id;
        let email = removal.identity.identity.email_address.clone();
        if !everywhere {
            match SettingsHost::get(cx).store.remove_identity(identity_id) {
                Ok(()) => {
                    refresh_workspace(cx);
                    cx.emit(SettingsEvent::Status(messages::removed(&email, false)));
                }
                Err(error) => cx.emit(SettingsEvent::Error(format!(
                    "Could not remove {email}: {error:#}"
                ))),
            }
            self.refresh(cx);
            return;
        }
        self.identity_busy = Some(identity_id);
        let sync = SettingsHost::get(cx).sync_service.clone();
        cx.spawn(async move |this, cx| {
            let result = sync.destroy_identity_and_wait(identity_id).await;
            let _ = this.update(cx, |this, cx| {
                this.identity_busy = None;
                match result {
                    Ok(()) => {
                        refresh_workspace(cx);
                        cx.emit(SettingsEvent::Status(messages::removed(&email, true)));
                    }
                    Err(error) => cx.emit(SettingsEvent::Error(format!(
                        "Could not remove {email} from the server: {error:#}"
                    ))),
                }
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Bring back: un-hides a server identity, back in Zmail at once, and
    /// syncs the account to confirm it.
    pub(crate) fn bring_back_identity(
        &mut self,
        account_id: i64,
        hidden_id: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(email) = self
            .cards
            .iter()
            .find(|card| card.account.id == account_id)
            .and_then(|card| {
                card.all_identities
                    .hidden
                    .iter()
                    .find(|hidden| hidden.id == hidden_id)
            })
            .map(|hidden| hidden.email.clone())
        else {
            return;
        };
        let host = SettingsHost::get(cx);
        match host.store.bring_back_identity(hidden_id) {
            Ok(_) => {
                let _ = host.sync_service.sync_account(account_id);
                refresh_workspace(cx);
                cx.emit(SettingsEvent::Status(messages::brought_back(&email)));
            }
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not bring {email} back: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    /// Make primary: new mail on the account starts from this identity.
    pub(crate) fn make_primary_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        let Some(email) = self.any_identity_email(identity_id) else {
            return;
        };
        match SettingsHost::get(cx)
            .store
            .set_primary_identity(identity_id)
        {
            Ok(()) => {
                refresh_workspace(cx);
                cx.emit(SettingsEvent::Status(messages::made_primary(&email)));
            }
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not make {email} the primary identity: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    fn any_identity_email(&self, identity_id: i64) -> Option<String> {
        self.cards
            .iter()
            .find_map(|card| card.all_identities.find(identity_id))
            .map(|identity| identity.identity.email_address.clone())
    }

    pub(super) fn close_identity_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialogs = &mut self.identity_dialogs;
        dialogs.dialog.close(window, cx);
        dialogs.remove.close(window, cx);
        dialogs.editing = None;
        dialogs.removal = None;
        dialogs.errors.clear();
        cx.notify();
    }

    /// Whether the identity dialog is open.
    #[cfg(test)]
    pub(crate) fn identity_editor_open(&self) -> bool {
        self.identity_dialogs.dialog.is_open()
    }

    /// Whether the identity remove confirmation is open.
    #[cfg(test)]
    pub(crate) fn identity_removal_open(&self) -> bool {
        self.identity_dialogs.remove.is_open()
    }

    /// The identity dialog's input for `field`, for tests to type in.
    #[cfg(test)]
    pub(crate) fn identity_input(&self, field: IdentityField) -> Entity<TextInput> {
        self.identity_dialogs.input(field).clone()
    }

    /// The problems the identity dialog shows.
    #[cfg(test)]
    pub(crate) fn identity_errors(&self) -> Vec<(IdentityField, String)> {
        self.identity_dialogs.errors.clone()
    }

    /// The card's Identities group: each identity with its marks and
    /// buttons, and Add Identity… (or why there is none).
    pub(super) fn render_identities(
        &self,
        card: &AccountCard,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account = &card.all_identities;
        let account_id = card.account.id;
        let busy = self.identity_busy.is_some();
        let add: AnyElement = if account.can_add() {
            div()
                .debug_selector(move || format!("identity-add-{account_id}"))
                .child(
                    Button::new(
                        ("identity-add", account_id as u64),
                        format!("{ADD}\u{2026}"),
                    )
                    .size(ButtonSize::Small)
                    .icon("user-plus")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_identity_editor(account_id, None, window, cx)
                    })),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let rows = account
            .identities
            .iter()
            .map(|identity| self.render_identity_row(account, identity, busy, colors, cx))
            .collect::<Vec<_>>();
        div()
            .id(("account-identities", account_id as u64))
            .accessible(Role::Group, "Identities".to_owned())
            .flex()
            .flex_col()
            .py(px(SPACE_3))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_2))
                    .child(div().type_style(TYPE_BODY_STRONG).child("Identities"))
                    .child(add),
            )
            .when_some(account.add_note(), |this, note| {
                this.child(
                    div()
                        .pt(px(SPACE_1))
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(note),
                )
            })
            .children(rows)
            .when(!account.hidden.is_empty(), |this| {
                this.child(self.render_hidden_identities(account, busy, colors, cx))
            })
            .into_any_element()
    }

    /// The account's hidden server identities, each with Bring back.
    fn render_hidden_identities(
        &self,
        account: &AccountIdentities,
        busy: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account_id = account.account_id;
        let row = |hidden: &HiddenIdentity, cx: &mut Context<Self>| {
            let hidden_id = hidden.id;
            let name = hidden.values.name.trim();
            let line = if name.is_empty() {
                hidden.email.clone()
            } else {
                format!("{name} <{}>", hidden.email)
            };
            div()
                .id(("hidden-identity", hidden_id as u64))
                .accessible(Role::Group, line.clone())
                .flex()
                .items_center()
                .gap(px(SPACE_2))
                .pt(px(SPACE_2))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .ellipsis()
                        .child(line),
                )
                .child(
                    div()
                        .flex_none()
                        .debug_selector(move || format!("identity-bring-back-{hidden_id}"))
                        .child(
                            Button::new(("identity-bring-back", hidden_id as u64), BRING_BACK)
                                .size(ButtonSize::Small)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.bring_back_identity(account_id, hidden_id, cx)
                                })),
                        ),
                )
                .into_any_element()
        };
        let rows = account
            .hidden
            .iter()
            .map(|hidden| row(hidden, cx))
            .collect::<Vec<_>>();
        div()
            .id(("hidden-identities", account_id as u64))
            .debug_selector(move || format!("hidden-identities-{account_id}"))
            .accessible(Role::Group, HIDDEN_HEADING.to_owned())
            .flex()
            .flex_col()
            .pt(px(SPACE_3))
            .child(div().type_style(TYPE_BODY_STRONG).child(HIDDEN_HEADING))
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(HIDDEN_NOTE),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_identity_row(
        &self,
        account: &AccountIdentities,
        identity: &SyncedIdentity,
        busy: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identity_id = identity.identity.id;
        let account_id = account.account_id;
        let status = zmail_setup::identities::status_label(identity.status);
        let line = zmail_setup::identities::identity_line(identity);
        let button = |name: &'static str, label: String| {
            Button::new((name, identity_id as u64), label)
                .size(ButtonSize::Small)
                .disabled(busy)
        };
        let mut actions = Vec::new();
        if account.can_make_primary(identity) {
            actions.push(
                div()
                    .debug_selector(move || format!("identity-make-primary-{identity_id}"))
                    .child(
                        button("identity-make-primary", MAKE_PRIMARY.to_owned()).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.make_primary_identity(identity_id, cx)
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        actions.push(
            div()
                .debug_selector(move || format!("identity-edit-{identity_id}"))
                .child(
                    button("identity-edit", format!("{EDIT}\u{2026}")).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.open_identity_editor(account_id, Some(identity_id), window, cx)
                        },
                    )),
                )
                .into_any_element(),
        );
        if account.can_remove(identity) {
            actions.push(
                div()
                    .debug_selector(move || format!("identity-delete-{identity_id}"))
                    .child(
                        button("identity-delete", "Remove\u{2026}".to_owned()).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.confirm_remove_identity(identity_id, window, cx)
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        let row_id = format!("identity-row-{identity_id}");
        div()
            .id(SharedString::from(row_id.clone()))
            .debug_selector(move || row_id.clone())
            .accessible(Role::Group, line.clone())
            .aria_description(
                [
                    identity.identity.is_primary.then_some(PRIMARY),
                    (!status.is_empty()).then_some(status),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
            )
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(SPACE_2))
            .pt(px(SPACE_2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .flex_1()
                    .min_w_0()
                    .child(div().min_w_0().type_style(TYPE_BODY).ellipsis().child(line))
                    .when(identity.identity.is_primary, |this| {
                        this.child(Badge::label("Primary").tone(BadgeTone::Neutral))
                    })
                    .when(!status.is_empty(), |this| {
                        this.child(
                            div()
                                .flex_none()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(if identity.status.needs_attention() {
                                    colors.warning
                                } else {
                                    colors.muted
                                }))
                                .child(status),
                        )
                    }),
            )
            .child(div().flex().flex_none().gap(px(SPACE_2)).children(actions))
            .into_any_element()
    }

    /// The identity dialog, while it is open.
    pub(super) fn render_identity_dialog(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialogs = &self.identity_dialogs;
        if !dialogs.dialog.is_open() {
            return None;
        }
        let (account, mode) = dialogs.editing.as_ref()?;
        let identity = match mode {
            FormMode::Edit(identity_id) => Some(account.find(*identity_id)?),
            FormMode::Add => None,
        };
        let (title, description) = match identity {
            Some(identity) => (
                format!("Edit {}", identity.identity.email_address),
                "New messages use these details; messages already being written keep theirs.",
            ),
            None => (
                "Add an identity".to_owned(),
                "Another address you send from on this account, such as an alias.",
            ),
        };
        let address_note = identity.and_then(|identity| account.address_note(identity));
        let field = |field: IdentityField| {
            let error = dialogs
                .errors
                .iter()
                .find(|(errored, _)| *errored == field)
                .map(|(_, error)| error.clone());
            input_card(field.label(), dialogs.input(field), colors)
                .debug_selector(move || format!("identity-field-{field:?}"))
                .when(field == IdentityField::Address, |this| {
                    this.when_some(address_note, |this, note| {
                        this.child(
                            div()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .child(note),
                        )
                    })
                })
                .when_some(error, |this, error| {
                    let text = SharedString::from(format!("{}: {error}", field.label()));
                    this.child(
                        div()
                            .id(SharedString::from(format!("identity-error-{field:?}")))
                            .debug_selector(move || format!("identity-error-{field:?}"))
                            .accessible(Role::Alert, text.clone())
                            .aria_value(text)
                            .type_style(TYPE_CAPTION)
                            .text_color(rgb(colors.danger))
                            .child(error),
                    )
                })
        };
        let server_button = match identity {
            Some(identity) if account.offers_update_server(identity) => Some(SAVE_AND_UPDATE),
            None if account.offers_create_on_server() => Some(ADD_AND_CREATE),
            _ => None,
        };
        let save_label = if identity.is_some() {
            SAVE
        } else {
            ADD_IN_ZMAIL
        };
        let busy = self.identity_busy.is_some();
        Some(
            Modal::new("identity-dialog", dialogs.dialog.focus_handle())
                .title(title)
                .description(description)
                .width(px(DIALOG_WIDTH))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_3))
                        .children(IdentityField::ALL.map(field)),
                )
                .footer(div().debug_selector(|| "identity-cancel".into()).child(
                    Button::new("identity-cancel", "Cancel").on_click(
                        cx.listener(|this, _, window, cx| this.close_identity_dialogs(window, cx)),
                    ),
                ))
                .when_some(server_button, |this, label| {
                    this.footer(
                        div()
                            .debug_selector(|| "identity-save-server".into())
                            .child(
                                Button::new("identity-save-server", label)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_identity_form(true, window, cx)
                                    })),
                            ),
                    )
                })
                .footer(
                    div().debug_selector(|| "identity-save".into()).child(
                        Button::new("identity-save", save_label)
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_identity_form(false, window, cx)
                            })),
                    ),
                )
                .on_dismiss(
                    cx.listener(|this, _, window, cx| this.close_identity_dialogs(window, cx)),
                )
                .into_any_element(),
        )
    }

    /// The identity remove confirmation, while it is open.
    pub(super) fn render_identity_remove_dialog(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialogs = &self.identity_dialogs;
        if !dialogs.remove.is_open() {
            return None;
        }
        let removal = dialogs.removal.as_ref()?;
        let blocked = removal.blocker.is_some();
        let everywhere = removal.account.offers_remove_everywhere(&removal.identity);
        let busy = self.identity_busy.is_some();
        Some(
            Modal::new("identity-remove-dialog", dialogs.remove.focus_handle())
                .when(everywhere, |this| this.width(px(REMOVE_EVERYWHERE_WIDTH)))
                .title(identity_editor::removal_title(&removal.identity))
                .description(identity_editor::removal_text(
                    &removal.account,
                    &removal.identity,
                ))
                .when_some(removal.blocker.clone(), |this, blocker| {
                    this.child(div().text_color(rgb(colors.danger)).child(blocker))
                })
                .footer(
                    div()
                        .debug_selector(|| "identity-remove-cancel".into())
                        .child(
                            Button::new("identity-remove-cancel", "Cancel")
                                .track_focus(&dialogs.remove_cancel)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_identity_dialogs(window, cx)
                                })),
                        ),
                )
                .when(everywhere, |this| {
                    this.footer(
                        div()
                            .debug_selector(|| "identity-remove-everywhere".into())
                            .child(
                                Button::new("identity-remove-everywhere", REMOVE_EVERYWHERE)
                                    .danger()
                                    .disabled(blocked || busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remove_identity_confirmed(true, window, cx)
                                    })),
                            ),
                    )
                })
                .footer(
                    div()
                        .debug_selector(|| "identity-remove-confirm".into())
                        .child(
                            Button::new("identity-remove-confirm", REMOVE_HERE)
                                .danger()
                                .disabled(blocked)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.remove_identity_confirmed(false, window, cx)
                                })),
                        ),
                )
                .on_dismiss(
                    cx.listener(|this, _, window, cx| this.close_identity_dialogs(window, cx)),
                )
                .into_any_element(),
        )
    }
}
