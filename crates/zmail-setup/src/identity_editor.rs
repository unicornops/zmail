//! Editing an account's identities, the same in both frontends (#366):
//! the form's fields and validation, which changes each kind of account
//! allows, the wording, and the store writes.
//!
//! Per account, the editor lists the identities and offers:
//!
//! - **Edit**: the name, reply-to and text signature, and the address only
//!   of an identity the user made ([`AccountIdentities::address_editable`]).
//!   A JMAP identity linked to a server identity keeps the server's address
//!   (RFC 8621 makes it immutable), and a Proton identity is a Proton
//!   address. An edit to a linked JMAP identity is a local edit under
//!   `zmail_core::identity_sync`'s rules, so it then differs from the
//!   server; [`SAVE_AND_UPDATE`] sends it up straight away
//!   (`SyncService::push_identity_and_wait`).
//! - **Add**: name, address, reply-to and signature, for IMAP and JMAP
//!   accounts. A JMAP account can create it on the server too
//!   ([`ADD_AND_CREATE`], `SyncService::create_identity_on_server_and_wait`).
//!   Proton addresses are added in Proton's settings, never from Zmail
//!   (ADR 0004: Zmail calls no Proton API Bridge doesn't).
//! - **Remove**, after asking: never the primary identity, the last one or
//!   one a draft is written from (`LocalStore::identity_removal_blocker`);
//!   its mail is filed under the primary identity. A linked JMAP identity
//!   the server lets the user delete (`mayDelete`) can go from the server
//!   too ([`REMOVE_EVERYWHERE`], `SyncService::destroy_identity_and_wait`).
//!   Removed from Zmail only while the server still lists it, it is hidden
//!   (Rob's decision of 1 November 2026): syncs leave it out, and the
//!   account's [`HIDDEN_HEADING`] list offers [`BRING_BACK`]
//!   (`LocalStore::bring_back_identity`).
//! - **Make primary**: the identity new mail starts from.
//!
//! IMAP and Proton edits stay in Zmail. Reply-to is stored and synced but
//! not sent on outgoing mail yet; that is a separate decision.
//!
//! New drafts take an identity's values when they start; a draft already
//! open keeps the name, address and signature it started with.
//!
//! Names, addresses, reply-tos and signatures can come from a server: a
//! terminal filters what it shows (`zmail_security::terminal`).

use anyhow::{Result, bail};
use zmail_core::{
    ProtocolKind,
    identity_sync::{IdentitySyncStatus, IdentityValues, SyncedIdentity},
    participants::parse_address_list,
};
use zmail_store::{HiddenIdentity, LocalStore};

use crate::identities::REMOVE;

/// The button that opens the form for a new identity.
pub const ADD: &str = "Add Identity";
/// The button that opens the form on an identity.
pub const EDIT: &str = "Edit";
/// The button that saves the form in Zmail.
pub const SAVE: &str = "Save";
/// The button that saves the form and sends it to the server.
pub const SAVE_AND_UPDATE: &str = "Save and update the server";
/// The button that adds the new identity in Zmail.
pub const ADD_IN_ZMAIL: &str = "Add";
/// The button that adds the new identity and creates it on the server.
pub const ADD_AND_CREATE: &str = "Add and create on the server";
/// The button that makes an identity the primary one.
pub const MAKE_PRIMARY: &str = "Make primary";
/// The button that removes an identity from Zmail only.
pub const REMOVE_HERE: &str = REMOVE;
/// The button that removes an identity from Zmail and the server.
pub const REMOVE_EVERYWHERE: &str = "Remove from Zmail and the server";
/// The heading of an account's hidden server identities.
pub const HIDDEN_HEADING: &str = "Hidden identities";
/// What the hidden list says about them.
pub const HIDDEN_NOTE: &str =
    "Removed from Zmail only: the server still has them, and syncs leave them out.";
/// The button that un-hides a server identity.
pub const BRING_BACK: &str = "Bring back";
/// The mark beside the primary identity.
pub const PRIMARY: &str = "primary";
/// Why a Proton account can't add identities.
pub const PROTON_ADDRESSES: &str = "Proton addresses are added and removed in Proton's settings; Zmail lists them after the next sync.";

/// A field of the identity form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityField {
    /// The display name.
    Name,
    /// The email address.
    Address,
    /// The reply-to addresses.
    ReplyTo,
    /// The plain-text signature.
    Signature,
}

impl IdentityField {
    /// Every field, in form order.
    pub const ALL: [Self; 4] = [Self::Name, Self::Address, Self::ReplyTo, Self::Signature];

    /// The field's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Address => "Address",
            Self::ReplyTo => "Reply-To",
            Self::Signature => "Signature",
        }
    }

    /// What an empty field shows.
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Name => "Your name, as recipients see it",
            Self::Address => "you@example.com",
            Self::ReplyTo => "Optional: where replies go",
            Self::Signature => "Optional: added to new messages",
        }
    }
}

/// What the identity form holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityForm {
    /// The display name.
    pub name: String,
    /// The email address.
    pub email: String,
    /// The reply-to addresses, as an address list.
    pub reply_to: String,
    /// The plain-text signature.
    pub signature: String,
}

impl IdentityForm {
    /// The form for an identity, holding its values in Zmail.
    pub fn from_identity(identity: &SyncedIdentity) -> Self {
        Self {
            name: identity.local.name.clone(),
            email: identity.identity.email_address.clone(),
            reply_to: identity.local.reply_to.clone(),
            signature: identity.local.signature.clone(),
        }
    }

    /// A field's value.
    pub fn value(&self, field: IdentityField) -> &str {
        match field {
            IdentityField::Name => &self.name,
            IdentityField::Address => &self.email,
            IdentityField::ReplyTo => &self.reply_to,
            IdentityField::Signature => &self.signature,
        }
    }

    /// Sets a field's value.
    pub fn set_value(&mut self, field: IdentityField, value: impl Into<String>) {
        let value = value.into();
        match field {
            IdentityField::Name => self.name = value,
            IdentityField::Address => self.email = value,
            IdentityField::ReplyTo => self.reply_to = value,
            IdentityField::Signature => self.signature = value,
        }
    }

    /// The address as saved: trimmed.
    pub fn address(&self) -> &str {
        self.email.trim()
    }

    /// The name, reply-to and signature as saved: the name and reply-to
    /// trimmed, the signature with Unix line endings and no trailing
    /// blank lines.
    pub fn values(&self) -> IdentityValues {
        IdentityValues {
            name: self.name.trim().to_owned(),
            reply_to: self.reply_to.trim().to_owned(),
            signature: self
                .signature
                .replace("\r\n", "\n")
                .trim_end_matches(['\n', '\r', ' ', '\t'])
                .to_owned(),
        }
    }
}

/// What the form is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    /// Changing the identity with this id.
    Edit(i64),
    /// Adding an identity.
    Add,
}

/// An account's identities and the changes its kind allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountIdentities {
    /// The account.
    pub account_id: i64,
    /// What kind of account it is.
    pub protocol: ProtocolKind,
    /// Its identities, primary first.
    pub identities: Vec<SyncedIdentity>,
    /// Its server identities removed from Zmail only, which syncs leave
    /// out.
    pub hidden: Vec<HiddenIdentity>,
}

impl AccountIdentities {
    /// Reads an account's identities.
    pub fn load(store: &LocalStore, account_id: i64, protocol: ProtocolKind) -> Result<Self> {
        Ok(Self {
            account_id,
            protocol,
            identities: store.list_synced_identities(account_id)?,
            hidden: store.list_hidden_identities(account_id)?,
        })
    }

    /// The identity with this id.
    pub fn find(&self, identity_id: i64) -> Option<&SyncedIdentity> {
        self.identities
            .iter()
            .find(|identity| identity.identity.id == identity_id)
    }

    /// Whether identities can be added: not to a Proton account.
    pub fn can_add(&self) -> bool {
        self.protocol != ProtocolKind::Proton
    }

    /// Why identities can't be added, when they can't.
    pub fn add_note(&self) -> Option<&'static str> {
        (!self.can_add()).then_some(PROTON_ADDRESSES)
    }

    /// Whether an identity's address can change: only one the user made,
    /// not a JMAP identity linked to a server identity nor a Proton
    /// address.
    pub fn address_editable(&self, identity: &SyncedIdentity) -> bool {
        self.protocol != ProtocolKind::Proton && !identity.is_linked()
    }

    /// Why an identity's address can't change, when it can't.
    pub fn address_note(&self, identity: &SyncedIdentity) -> Option<&'static str> {
        if self.protocol == ProtocolKind::Proton {
            Some("Proton sets this address.")
        } else if identity.is_linked() {
            Some("The server sets this address.")
        } else {
            None
        }
    }

    /// Whether saving an identity offers [`SAVE_AND_UPDATE`]: a JMAP
    /// identity linked to one the server still lists.
    pub fn offers_update_server(&self, identity: &SyncedIdentity) -> bool {
        self.protocol == ProtocolKind::Jmap
            && identity.is_linked()
            && identity.status != IdentitySyncStatus::NotOnServer
    }

    /// Whether adding an identity offers [`ADD_AND_CREATE`]: on a JMAP
    /// account, which keeps identities on the server.
    pub fn offers_create_on_server(&self) -> bool {
        self.protocol == ProtocolKind::Jmap
    }

    /// Whether removing an identity offers [`REMOVE_EVERYWHERE`]: a JMAP
    /// identity linked to one the server lists and lets the user delete.
    pub fn offers_remove_everywhere(&self, identity: &SyncedIdentity) -> bool {
        self.protocol == ProtocolKind::Jmap
            && identity.may_delete_on_server
            && identity.status != IdentitySyncStatus::NotOnServer
    }

    /// Whether an identity can become the primary one: any but the
    /// primary.
    pub fn can_make_primary(&self, identity: &SyncedIdentity) -> bool {
        !identity.identity.is_primary
    }

    /// Whether the list offers to remove an identity: not the primary one
    /// nor the last. (A draft written from it is checked when asked,
    /// `LocalStore::identity_removal_blocker`.)
    pub fn can_remove(&self, identity: &SyncedIdentity) -> bool {
        !identity.identity.is_primary && self.identities.len() > 1
    }

    /// The form's problems, field by field; empty when it can be saved.
    /// The address is checked only when it is new or can change, and an
    /// edited identity's other fields only when they changed: a value the
    /// server set is not the user's to fix before saving another field.
    pub fn validate(&self, mode: FormMode, form: &IdentityForm) -> Vec<(IdentityField, String)> {
        let mut errors = Vec::new();
        let stored = match mode {
            FormMode::Edit(identity_id) => self.find(identity_id).map(IdentityForm::from_identity),
            FormMode::Add => None,
        };
        let changed = |field: IdentityField| {
            stored
                .as_ref()
                .is_none_or(|stored| stored.value(field) != form.value(field))
        };
        let checks_address = match mode {
            FormMode::Add => true,
            FormMode::Edit(identity_id) => self
                .find(identity_id)
                .is_some_and(|identity| self.address_editable(identity)),
        };
        if checks_address {
            let address = form.address();
            if address.is_empty() {
                errors.push((IdentityField::Address, "Enter the address.".to_owned()));
            } else if !is_address(address) {
                errors.push((
                    IdentityField::Address,
                    "Enter an address like name@example.com.".to_owned(),
                ));
            } else if self.identities.iter().any(|identity| {
                identity
                    .identity
                    .email_address
                    .eq_ignore_ascii_case(address)
                    && mode != FormMode::Edit(identity.identity.id)
            }) {
                errors.push((
                    IdentityField::Address,
                    format!("This account already has an identity for {address}."),
                ));
            }
        }
        if changed(IdentityField::Name) && form.name.chars().any(char::is_control) {
            errors.push((
                IdentityField::Name,
                "The name can't contain line breaks or control characters.".to_owned(),
            ));
        }
        let reply_to = form.reply_to.trim();
        if changed(IdentityField::ReplyTo) && !reply_to.is_empty() {
            let addresses = parse_address_list(reply_to);
            let valid = !reply_to.chars().any(char::is_control)
                && !addresses.is_empty()
                && addresses.iter().all(|participant| {
                    participant
                        .email
                        .as_deref()
                        .is_some_and(|email| is_address(email.trim()))
                });
            if !valid {
                errors.push((
                    IdentityField::ReplyTo,
                    "Enter addresses like name@example.com, separated by commas.".to_owned(),
                ));
            }
        }
        if changed(IdentityField::Signature)
            && form
                .signature
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            errors.push((
                IdentityField::Signature,
                "The signature can't contain control characters.".to_owned(),
            ));
        }
        errors
    }
}

/// Whether `address` is a deliverable address, by the rule compose uses:
/// one `@` with text on both sides, no spaces, brackets, commas or
/// control characters, and a domain that neither starts nor ends with a
/// dot nor has two in a row. (A single-label domain such as `localhost`
/// is allowed, as servers may have one.)
pub fn is_address(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && !address
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '<' | '>' | ',' | '"'))
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

/// The remove confirmation's title.
pub fn removal_title(identity: &SyncedIdentity) -> String {
    format!("Remove {}?", identity.identity.email_address)
}

/// What the remove confirmation says: what goes, what stays, and what the
/// server does with an identity it still lists.
pub fn removal_text(account: &AccountIdentities, identity: &SyncedIdentity) -> String {
    let mut text = "It goes from Zmail; its mail stays, filed under the account's primary \
                    identity."
        .to_owned();
    let on_server = account.protocol == ProtocolKind::Jmap
        && identity.is_linked()
        && identity.status != IdentitySyncStatus::NotOnServer;
    if on_server {
        text.push_str(if account.offers_remove_everywhere(identity) {
            " The server still lists it, so Zmail hides it from later syncs (Bring back \
             undoes that), or you can remove it from the server too."
        } else {
            " The server still lists it and doesn't let Zmail delete it, so Zmail hides it from \
             later syncs; Bring back undoes that."
        });
    }
    text
}

/// Saves the form for identity `identity_id`: its address when it can
/// change, then its name, reply-to and signature. A linked JMAP identity
/// then differs from the server until the user updates the server or
/// takes the server's values. Fails, with the first problem, when the
/// form doesn't validate.
pub fn save_identity(
    store: &LocalStore,
    account: &AccountIdentities,
    identity_id: i64,
    form: &IdentityForm,
) -> Result<()> {
    let Some(identity) = account.find(identity_id) else {
        bail!("that identity no longer exists");
    };
    if let Some((_, error)) = account
        .validate(FormMode::Edit(identity_id), form)
        .into_iter()
        .next()
    {
        bail!(error);
    }
    if account.address_editable(identity) && form.address() != identity.identity.email_address {
        store.set_identity_address(identity_id, form.address())?;
    }
    store.update_identity(identity_id, &form.values())
}

/// Adds the form as a new identity of the account and returns its id.
/// Fails for a Proton account, and when the form doesn't validate.
pub fn add_identity(
    store: &LocalStore,
    account: &AccountIdentities,
    form: &IdentityForm,
) -> Result<i64> {
    if let Some(note) = account.add_note() {
        bail!(note);
    }
    if let Some((_, error)) = account.validate(FormMode::Add, form).into_iter().next() {
        bail!(error);
    }
    store.add_identity(account.account_id, form.address(), &form.values())
}

/// What the frontends say once a change is done.
pub mod messages {
    /// After saving.
    pub fn saved(email: &str) -> String {
        format!("Saved {email}.")
    }

    /// After saving and updating the server.
    pub fn saved_and_updated(email: &str) -> String {
        format!("Saved {email} and updated the server.")
    }

    /// After adding.
    pub fn added(email: &str) -> String {
        format!("Added {email}.")
    }

    /// After adding and creating it on the server.
    pub fn added_and_created(email: &str) -> String {
        format!("Added {email} and created it on the server.")
    }

    /// After making it primary.
    pub fn made_primary(email: &str) -> String {
        format!("New mail now starts from {email}.")
    }

    /// After removing it, from Zmail or from the server too.
    pub fn removed(email: &str, everywhere: bool) -> String {
        if everywhere {
            format!("Removed {email} from Zmail and the server.")
        } else {
            format!("Removed {email} from Zmail.")
        }
    }

    /// After bringing a hidden identity back.
    pub fn brought_back(email: &str) -> String {
        format!("Brought back {email}.")
    }

    /// When the server refused, after the local change was kept.
    pub fn server_refused(email: &str, error: &str) -> String {
        format!("Kept {email} in Zmail, but the server refused: {error}")
    }
}

#[cfg(test)]
mod tests {
    use zmail_core::{
        AuthKind, CachePolicySummary, IdentitySummary, NewAccount, NewIdentity, ProtocolKind,
        identity_sync::{IdentitySyncStatus, IdentityValues, ServerIdentity, SyncedIdentity},
    };
    use zmail_store::LocalStore;

    use super::{
        AccountIdentities, FormMode, IdentityField, IdentityForm, add_identity, is_address,
        removal_text, save_identity,
    };

    fn identity(id: i64, email: &str, primary: bool, linked: bool) -> SyncedIdentity {
        let local = IdentityValues {
            name: "Rob".to_owned(),
            reply_to: String::new(),
            signature: "-- Rob".to_owned(),
        };
        SyncedIdentity {
            identity: IdentitySummary {
                id,
                display_name: local.name.clone(),
                email_address: email.to_owned(),
                signature: local.signature.clone(),
                reply_to: local.reply_to.clone(),
                is_primary: primary,
            },
            server: linked.then(|| local.clone()),
            local,
            status: if linked {
                IdentitySyncStatus::InSync
            } else {
                IdentitySyncStatus::Local
            },
            may_delete_on_server: linked,
        }
    }

    fn account(protocol: ProtocolKind, identities: Vec<SyncedIdentity>) -> AccountIdentities {
        AccountIdentities {
            account_id: 1,
            protocol,
            identities,
            hidden: Vec::new(),
        }
    }

    fn form(name: &str, email: &str, reply_to: &str, signature: &str) -> IdentityForm {
        IdentityForm {
            name: name.to_owned(),
            email: email.to_owned(),
            reply_to: reply_to.to_owned(),
            signature: signature.to_owned(),
        }
    }

    fn fields(errors: &[(IdentityField, String)]) -> Vec<IdentityField> {
        errors.iter().map(|(field, _)| *field).collect()
    }

    #[test]
    fn addresses_follow_the_compose_rule() {
        for good in ["rob@example.com", "jmap@localhost", "a.b+c@sub.example.ie"] {
            assert!(is_address(good), "{good}");
        }
        for bad in [
            "",
            "rob",
            "@example.com",
            "rob@",
            "rob@@example.com",
            "rob @example.com",
            "rob@.example.com",
            "rob@example.com.",
            "rob@example..com",
            "Rob <rob@example.com>",
            "rob@example.com,",
            "rob@exa\nmple.com",
        ] {
            assert!(!is_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_form_checks_each_field() {
        let imap = account(
            ProtocolKind::Imap,
            vec![identity(1, "rob@example.com", true, false)],
        );
        assert!(
            imap.validate(
                FormMode::Add,
                &form(
                    "Help desk",
                    " help@example.com ",
                    "Tickets <tickets@example.com>, ops@example.com",
                    "-- \nThe help desk\r\n\tExample Ltd"
                )
            )
            .is_empty()
        );
        let errors = imap.validate(
            FormMode::Add,
            &form("Bad\nname", "ROB@example.com", "not an address", "\x1b[2J"),
        );
        assert_eq!(
            fields(&errors),
            [
                IdentityField::Address,
                IdentityField::Name,
                IdentityField::ReplyTo,
                IdentityField::Signature
            ]
        );
        assert!(
            errors[0].1.contains("already has an identity"),
            "{errors:?}"
        );
        assert_eq!(
            fields(&imap.validate(FormMode::Add, &form("", "", "", ""))),
            [IdentityField::Address]
        );
        assert_eq!(
            fields(&imap.validate(FormMode::Add, &form("", "nope", "", ""))),
            [IdentityField::Address]
        );
        // Editing keeps its own address.
        assert!(
            imap.validate(FormMode::Edit(1), &form("Rob", "rob@example.com", "", ""))
                .is_empty()
        );
        // The values are trimmed, and the signature's line endings
        // normalised.
        let values = form(" Rob ", "x@y", " a@b ", "-- Rob\r\nline\n\n").values();
        assert_eq!(values.name, "Rob");
        assert_eq!(values.reply_to, "a@b");
        assert_eq!(values.signature, "-- Rob\nline");
    }

    #[test]
    fn each_kind_of_account_allows_its_own_changes() {
        let primary = identity(1, "rob@example.com", true, true);
        let alias = identity(2, "help@example.com", false, true);
        let added = identity(3, "me@example.com", false, false);
        let jmap = account(
            ProtocolKind::Jmap,
            vec![primary.clone(), alias.clone(), added.clone()],
        );
        assert!(jmap.can_add() && jmap.offers_create_on_server());
        assert!(!jmap.address_editable(&alias), "a linked address is fixed");
        assert!(jmap.address_editable(&added));
        assert!(jmap.offers_update_server(&alias));
        assert!(!jmap.offers_update_server(&added));
        assert!(jmap.offers_remove_everywhere(&alias));
        assert!(!jmap.offers_remove_everywhere(&added));
        assert!(!jmap.can_remove(&primary) && jmap.can_remove(&alias));
        assert!(!jmap.can_make_primary(&primary) && jmap.can_make_primary(&alias));
        assert!(removal_text(&jmap, &alias).contains("hides it from later syncs"));
        assert!(removal_text(&jmap, &alias).contains("remove it from the server too"));
        let mut derived = alias.clone();
        derived.may_delete_on_server = false;
        assert!(!jmap.offers_remove_everywhere(&derived));
        assert!(removal_text(&jmap, &derived).contains("doesn't let Zmail delete it"));
        let mut gone = alias.clone();
        gone.status = IdentitySyncStatus::NotOnServer;
        assert!(!jmap.offers_update_server(&gone) && !jmap.offers_remove_everywhere(&gone));
        assert!(!removal_text(&jmap, &gone).contains("server"));
        // A linked identity's address isn't checked (it can't change), nor
        // a value the server set that the user left alone.
        assert!(
            jmap.validate(FormMode::Edit(2), &form("Help", "", "", ""))
                .is_empty()
        );
        let mut hostile = alias.clone();
        hostile.local.name = "Help\x1b[2J".to_owned();
        let served = account(ProtocolKind::Jmap, vec![primary.clone(), hostile]);
        assert!(
            served
                .validate(FormMode::Edit(2), &form("Help\x1b[2J", "", "", "-- new"))
                .is_empty()
        );
        assert_eq!(
            fields(&served.validate(FormMode::Edit(2), &form("Help\x1b[3J", "", "", ""))),
            [IdentityField::Name]
        );

        let imap = account(ProtocolKind::Imap, vec![identity(1, "a@b.c", true, false)]);
        assert!(imap.can_add() && !imap.offers_create_on_server());
        assert!(!imap.can_remove(&imap.identities[0]), "the last one stays");

        let proton = account(
            ProtocolKind::Proton,
            vec![identity(1, "rob@proton.me", true, false)],
        );
        assert!(!proton.can_add());
        assert!(
            proton
                .add_note()
                .is_some_and(|note| note.contains("Proton's settings"))
        );
        assert!(!proton.address_editable(&proton.identities[0]));
        assert!(!proton.offers_update_server(&proton.identities[0]));
        assert_eq!(
            proton.address_note(&proton.identities[0]),
            Some("Proton sets this address.")
        );
    }

    fn store_with(protocol: ProtocolKind) -> (LocalStore, i64) {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Work".to_owned(),
                protocol,
                auth: AuthKind::Password,
                endpoint: "https://mail.example.com".to_owned(),
                username: "rob@example.com".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "Rob".to_owned(),
                    email_address: "rob@example.com".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account is stored");
        (store, account_id)
    }

    #[test]
    fn saving_and_adding_go_through_the_rules() {
        let (store, account_id) = store_with(ProtocolKind::Jmap);
        store
            .merge_server_identities(
                account_id,
                &[ServerIdentity {
                    id: "i1".to_owned(),
                    email: "rob@example.com".to_owned(),
                    values: IdentityValues {
                        name: "Rob".to_owned(),
                        ..IdentityValues::default()
                    },
                    may_delete: false,
                }],
            )
            .expect("merged");
        let load =
            || AccountIdentities::load(&store, account_id, ProtocolKind::Jmap).expect("identities");
        let account = load();
        let primary = account.identities[0].identity.id;

        // A linked identity: the name and signature change, the address
        // doesn't, and it now differs from the server.
        save_identity(
            &store,
            &account,
            primary,
            &form("Rob L", "other@example.com", "", "-- Rob L"),
        )
        .expect("saved");
        let account = load();
        assert_eq!(
            account.identities[0].identity.email_address,
            "rob@example.com"
        );
        assert_eq!(account.identities[0].local.signature, "-- Rob L");
        assert_eq!(
            account.identities[0].status,
            IdentitySyncStatus::ChangedHere
        );

        // A new one, then a change of its address.
        let added = add_identity(&store, &account, &form("Help", "help@example.com", "", ""))
            .expect("added");
        let account = load();
        assert!(!account.find(added).expect("listed").identity.is_primary);
        assert!(add_identity(&store, &account, &form("", "HELP@example.com", "", "")).is_err());
        save_identity(
            &store,
            &account,
            added,
            &form("Help", "desk@example.com", "", ""),
        )
        .expect("saved");
        assert_eq!(
            load().find(added).expect("listed").identity.email_address,
            "desk@example.com"
        );
        let error = save_identity(&store, &load(), added, &form("Help", "nope", "", ""))
            .expect_err("refused");
        assert!(
            error.to_string().contains("like name@example.com"),
            "{error}"
        );

        // Proton adds nothing.
        let (proton, proton_id) = store_with(ProtocolKind::Proton);
        let account =
            AccountIdentities::load(&proton, proton_id, ProtocolKind::Proton).expect("identities");
        assert!(add_identity(&proton, &account, &form("", "a@b.c", "", "")).is_err());
    }
}
