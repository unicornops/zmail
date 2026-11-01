//! Setup → Accounts → Change → Identities (#366), Alpine-style: Change
//! lists the account's identities; `Ret` or `E` opens the one under the
//! cursor in a form with labelled fields, `A` an empty form to add one.
//! `^X` saves, `^C` cancels. The signature edits in `ratatui-textarea`,
//! as Compose's message text does. On the list, `D` removes an identity
//! after asking and `*` makes it the primary one.
//!
//! The fields, validation, wording and each account kind's rules are
//! `zmail_setup::identity_editor`'s, the same as the GUI's:
//!
//! - a JMAP identity linked to a server identity and a Proton address keep
//!   their address, drawn read-only with why;
//! - after saving an edit to a linked JMAP identity, Zmail asks whether to
//!   update the server too; after adding one to a JMAP account, whether to
//!   create it there too;
//! - removing a linked JMAP identity the server lets the user delete asks
//!   whether to remove it from the server too (`S`);
//! - Proton accounts add no identities.
//!
//! Names, addresses, reply-tos and signatures may come from a server, so
//! the form holds them filtered (`sanitize_line`, line by line for the
//! signature), and a field left as it was saves the stored value, not the
//! filtered one.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;
use zmail_core::identity_sync::SyncedIdentity;
use zmail_security::terminal::sanitize_line;
use zmail_setup::identity_editor::{
    self, AccountIdentities, FormMode, IdentityField, IdentityForm, messages,
};

use crate::{
    accounts::EditAccount,
    app::{App, Prompt},
    compose::{body_editor, editor_input, line_editor, line_text},
};

/// What a background identity request does on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerChange {
    /// Send Zmail's values up (`Identity/set` update).
    Update,
    /// Create the identity there (`Identity/set` create).
    Create,
    /// Delete it there, then remove it from Zmail (`Identity/set`
    /// destroy).
    Remove,
}

/// The identity form, while it is open on Change.
pub struct IdentityEditor {
    /// Editing an identity, or adding one.
    pub mode: FormMode,
    /// The focused field.
    pub focus: IdentityField,
    /// The four fields' editors, in [`IdentityField::ALL`] order.
    editors: [TextArea<'static>; 4],
    /// Whether the address can change.
    pub address_editable: bool,
    /// Why it can't, when it can't.
    pub address_note: Option<&'static str>,
    /// The identity's stored values, as they were opened.
    original: IdentityForm,
    /// The same, filtered, as the editors started with.
    shown: IdentityForm,
    /// The problems the last `^X` found.
    pub errors: Vec<(IdentityField, String)>,
}

fn index(field: IdentityField) -> usize {
    IdentityField::ALL
        .iter()
        .position(|candidate| *candidate == field)
        .unwrap_or(0)
}

/// `form` with every field filtered for the terminal.
fn filtered(form: &IdentityForm) -> IdentityForm {
    IdentityForm {
        name: sanitize_line(&form.name),
        email: sanitize_line(&form.email),
        reply_to: sanitize_line(&form.reply_to),
        signature: form
            .signature
            .lines()
            .map(sanitize_line)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

impl IdentityEditor {
    fn new(account: &AccountIdentities, identity: Option<&SyncedIdentity>) -> Self {
        let original = identity
            .map(IdentityForm::from_identity)
            .unwrap_or_default();
        let shown = filtered(&original);
        let editors = IdentityField::ALL.map(|field| match field {
            IdentityField::Signature => body_editor(shown.value(field)),
            _ => line_editor(shown.value(field)),
        });
        Self {
            mode: identity.map_or(FormMode::Add, |identity| {
                FormMode::Edit(identity.identity.id)
            }),
            focus: IdentityField::Name,
            editors,
            address_editable: identity.is_none_or(|identity| account.address_editable(identity)),
            address_note: identity.and_then(|identity| account.address_note(identity)),
            original,
            shown,
            errors: Vec::new(),
        }
    }

    /// A field's editor.
    pub fn editor(&self, field: IdentityField) -> &TextArea<'static> {
        &self.editors[index(field)]
    }

    /// A field's editor, to draw or type in.
    pub fn editor_mut(&mut self, field: IdentityField) -> &mut TextArea<'static> {
        &mut self.editors[index(field)]
    }

    /// The fields the cursor visits: all but a fixed address.
    fn focusable(&self) -> Vec<IdentityField> {
        IdentityField::ALL
            .into_iter()
            .filter(|field| *field != IdentityField::Address || self.address_editable)
            .collect()
    }

    fn move_focus(&mut self, by: isize) {
        let fields = self.focusable();
        let at = fields
            .iter()
            .position(|field| *field == self.focus)
            .unwrap_or(0);
        let next = at.saturating_add_signed(by).min(fields.len() - 1);
        self.focus = fields[next];
    }

    /// A field's text as typed.
    pub fn text(&self, field: IdentityField) -> String {
        match field {
            IdentityField::Signature => self.editor(field).lines().join("\n"),
            _ => line_text(self.editor(field)),
        }
    }

    /// The form as it would save: a field left as it opened keeps the
    /// stored (unfiltered) value.
    pub fn form(&self) -> IdentityForm {
        let mut form = IdentityForm::default();
        for field in IdentityField::ALL {
            let text = self.text(field);
            let value = if text == self.shown.value(field) {
                self.original.value(field).to_owned()
            } else {
                text
            };
            form.set_value(field, value);
        }
        form
    }

    /// The problem `^X` found with a field, if any.
    pub fn error(&self, field: IdentityField) -> Option<&str> {
        self.errors
            .iter()
            .find(|(errored, _)| *errored == field)
            .map(|(_, error)| error.as_str())
    }
}

/// A shared message as the status line says it: no closing full stop.
fn status_line(message: String) -> String {
    message.trim_end_matches('.').to_owned()
}

impl EditAccount {
    /// The account's identities and the rules its kind brings.
    pub fn account_identities(&self) -> AccountIdentities {
        AccountIdentities {
            account_id: self.account.id,
            protocol: self.account.protocol,
            identities: self.identities.clone(),
        }
    }
}

impl App {
    /// Opens the identity form on `identity_id`, or empty to add one.
    pub(crate) fn open_identity_form(&mut self, identity_id: Option<i64>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let account = edit.account_identities();
        let identity = match identity_id {
            Some(identity_id) => match account.find(identity_id) {
                Some(identity) => Some(identity),
                None => return,
            },
            None => {
                if let Some(note) = account.add_note() {
                    self.status = Some(note.to_owned());
                    return;
                }
                None
            }
        };
        edit.form = Some(IdentityEditor::new(&account, identity));
    }

    /// A key while the identity form is open.
    pub(crate) fn identity_form_key(&mut self, key: KeyEvent) {
        let Some(form) = self.edit.as_mut().and_then(|edit| edit.form.as_mut()) else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let signature = form.focus == IdentityField::Signature;
        match key.code {
            KeyCode::Char('x') if control => self.save_identity_form(),
            KeyCode::Char('c') if control => self.cancel_identity_form(),
            KeyCode::Esc => self.cancel_identity_form(),
            KeyCode::Char('u') if control => {
                let focus = form.focus;
                *form.editor_mut(focus) = if signature {
                    body_editor("")
                } else {
                    line_editor("")
                };
            }
            _ if control => {}
            KeyCode::BackTab => form.move_focus(-1),
            KeyCode::Tab => form.move_focus(1),
            KeyCode::Up if !signature || form.editor(form.focus).cursor().0 == 0 => {
                form.move_focus(-1);
            }
            KeyCode::Down | KeyCode::Enter if !signature => form.move_focus(1),
            _ => {
                let focus = form.focus;
                if let Some(input) = editor_input(key) {
                    form.editor_mut(focus).input(input);
                }
            }
        }
    }

    /// A paste while the identity form is open: text for the focused
    /// field, filtered (line by line in the signature).
    pub(crate) fn identity_form_paste(&mut self, pasted: &str) {
        let Some(form) = self.edit.as_mut().and_then(|edit| edit.form.as_mut()) else {
            return;
        };
        let focus = form.focus;
        let text = if focus == IdentityField::Signature {
            pasted
                .lines()
                .map(sanitize_line)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            sanitize_line(pasted)
        };
        form.editor_mut(focus).insert_str(text);
    }

    fn cancel_identity_form(&mut self) {
        if let Some(edit) = self.edit.as_mut() {
            edit.form = None;
        }
        self.status = Some("Nothing changed".to_owned());
    }

    /// `^X`: checks the form, saves it, then for JMAP asks whether the
    /// server should have it too.
    fn save_identity_form(&mut self) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let account = edit.account_identities();
        let Some(form) = edit.form.as_mut() else {
            return;
        };
        let values = form.form();
        let mode = form.mode;
        form.errors = account.validate(mode, &values);
        if let Some((field, error)) = form.errors.first() {
            self.status = Some(format!("{}: {error}", field.label()));
            return;
        }
        let saved = match mode {
            FormMode::Edit(identity_id) => {
                identity_editor::save_identity(&self.store, &account, identity_id, &values)
                    .map(|()| identity_id)
            }
            FormMode::Add => identity_editor::add_identity(&self.store, &account, &values),
        };
        let identity_id = match saved {
            Ok(identity_id) => identity_id,
            Err(error) => return self.report(error),
        };
        edit.form = None;
        self.refresh_identities();
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        if let Some(index) = edit
            .identities
            .iter()
            .position(|identity| identity.identity.id == identity_id)
        {
            edit.cursor = crate::accounts::EDIT_ROWS + index;
        }
        let account = edit.account_identities();
        let Some(identity) = account.find(identity_id) else {
            return;
        };
        let email = sanitize_line(&identity.identity.email_address);
        let create = mode == FormMode::Add;
        let ask = if create {
            account.offers_create_on_server()
        } else {
            account.offers_update_server(identity) && identity.status.differs()
        };
        if ask {
            self.prompt = Some(Prompt::IdentityToServer {
                identity_id,
                email,
                create,
            });
        } else {
            self.status = Some(status_line(if create {
                messages::added(&email)
            } else {
                messages::saved(&email)
            }));
        }
    }

    /// The answer to "update the server too?" or "create it there too?".
    pub(crate) fn identity_to_server_answer(
        &mut self,
        identity_id: i64,
        email: &str,
        create: bool,
        yes: bool,
    ) {
        if yes {
            let change = if create {
                ServerChange::Create
            } else {
                ServerChange::Update
            };
            return self.identity_on_server(identity_id, change);
        }
        self.status = Some(status_line(if create {
            messages::added(email)
        } else {
            messages::saved(email)
        }));
    }

    /// `D` on an identity: asks first, unless the rules keep it.
    pub(crate) fn ask_remove_identity(&mut self) {
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        let Some(identity) = edit.identity_at_cursor() else {
            return;
        };
        let account = edit.account_identities();
        let identity_id = identity.identity.id;
        let email = sanitize_line(&identity.identity.email_address);
        let everywhere = account.offers_remove_everywhere(identity);
        let note = identity_editor::removal_text(&account, identity);
        match self.store.identity_removal_blocker(identity_id) {
            Ok(Some(blocker)) => self.status = Some(sanitize_line(&blocker)),
            Ok(None) => {
                self.prompt = Some(Prompt::RemoveIdentity {
                    identity_id,
                    email,
                    note,
                    everywhere,
                });
            }
            Err(error) => self.report(error),
        }
    }

    /// `*` on an identity: new mail starts from it.
    pub(crate) fn make_primary_identity(&mut self) {
        let Some(identity) = self.edit.as_ref().and_then(EditAccount::identity_at_cursor) else {
            return;
        };
        if identity.identity.is_primary {
            self.status = Some("This is already the primary identity".to_owned());
            return;
        }
        let identity_id = identity.identity.id;
        let email = sanitize_line(&identity.identity.email_address);
        match self.store.set_primary_identity(identity_id) {
            Ok(()) => self.status = Some(status_line(messages::made_primary(&email))),
            Err(error) => self.report(error),
        }
        self.refresh_identities();
        // The primary identity lists first: follow it there.
        if let Some(edit) = self.edit.as_mut()
            && let Some(index) = edit
                .identities
                .iter()
                .position(|identity| identity.identity.id == identity_id)
        {
            edit.cursor = crate::accounts::EDIT_ROWS + index;
        }
    }
}
