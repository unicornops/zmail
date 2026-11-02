//! How both frontends word an identity that differs from its JMAP server,
//! and the choices they offer (`zmail_core::identity_sync`; Rob's decision
//! of 1 November 2026).
//!
//! - A differing identity offers [`UPDATE_SERVER`] (send Zmail's name,
//!   reply-to and signature up, `SyncService::push_identity_and_wait`)
//!   and [`USE_SERVER`] (replace Zmail's with the server's,
//!   `LocalStore::use_server_identity`).
//! - One the server no longer lists offers [`RECREATE`] (create it on the
//!   server again, `SyncService::recreate_identity_and_wait`) and
//!   [`REMOVE`] (`LocalStore::remove_identity`), and is never removed
//!   without asking.
//!
//! Names and addresses come from the server: a terminal filters what
//! these functions return before drawing it.

use zmail_core::identity_sync::{IdentitySyncStatus, SyncedIdentity};

/// The button that sends Zmail's values to the server.
pub const UPDATE_SERVER: &str = "Update the server";
/// The button that replaces Zmail's values with the server's.
pub const USE_SERVER: &str = "Use the server's";
/// The button that creates an identity on the server again.
pub const RECREATE: &str = "Create it on the server";
/// The button that removes an identity from Zmail.
pub const REMOVE: &str = "Remove from Zmail";

/// A short word for how an identity stands, for a list of identities.
pub fn status_label(status: IdentitySyncStatus) -> &'static str {
    match status {
        IdentitySyncStatus::Local => "",
        IdentitySyncStatus::InSync => "same as the server",
        IdentitySyncStatus::ChangedHere => "changed in Zmail",
        IdentitySyncStatus::Conflict => "changed in Zmail and on the server",
        IdentitySyncStatus::NotOnServer => "no longer on the server",
    }
}

/// The identity as a list shows it: `Name <address>`, or the address.
pub fn identity_line(identity: &SyncedIdentity) -> String {
    let name = identity.local.name.trim();
    if name.is_empty() {
        identity.identity.email_address.clone()
    } else {
        format!("{name} <{}>", identity.identity.email_address)
    }
}

/// A notice's title for an identity that needs attention; `None` for one
/// that doesn't.
pub fn notice_title(identity: &SyncedIdentity) -> Option<String> {
    let address = &identity.identity.email_address;
    match identity.status {
        IdentitySyncStatus::ChangedHere | IdentitySyncStatus::Conflict => {
            Some(format!("{address} differs from the server"))
        }
        IdentitySyncStatus::NotOnServer => Some(format!("{address} is no longer on the server")),
        IdentitySyncStatus::Local | IdentitySyncStatus::InSync => None,
    }
}

/// What a notice says: which fields differ, who changed them, and what
/// the two choices do.
pub fn notice_text(identity: &SyncedIdentity) -> String {
    let fields = identity
        .server
        .as_ref()
        .map(|server| identity.local.differing_fields(server))
        .unwrap_or_default();
    let (fields, differ, them) = match fields.as_slice() {
        [] => ("Its details".to_owned(), "differ", "them"),
        [one] => (format!("Its {one}"), "differs", "it"),
        [rest @ .., last] => (
            format!("Its {} and {last}", rest.join(", ")),
            "differ",
            "them",
        ),
    };
    match identity.status {
        IdentitySyncStatus::ChangedHere => format!(
            "{fields} changed in Zmail. Update the server to use Zmail's everywhere, or use \
             the server's to undo the change here."
        ),
        IdentitySyncStatus::Conflict => format!(
            "{fields} {differ} in Zmail and on the server, and Zmail can't tell which is \
             newer. Update the server to keep Zmail's, or use the server's to replace {them} \
             here."
        ),
        IdentitySyncStatus::NotOnServer => "It was removed on the server, or by another app. \
             Create it on the server again, or remove it from Zmail; its mail stays, filed \
             under the account's main identity."
            .to_owned(),
        IdentitySyncStatus::Local | IdentitySyncStatus::InSync => String::new(),
    }
}

/// What the server has for a differing identity, field by field, for the
/// notice's details: `(field, Zmail's, the server's)`.
pub fn differences(identity: &SyncedIdentity) -> Vec<(&'static str, String, String)> {
    let Some(server) = &identity.server else {
        return Vec::new();
    };
    let local = &identity.local;
    let shown = |value: &str| {
        if value.trim().is_empty() {
            "(none)".to_owned()
        } else {
            value.to_owned()
        }
    };
    local
        .differing_fields(server)
        .into_iter()
        .map(|field| {
            let (ours, theirs) = match field {
                "name" => (&local.name, &server.name),
                "reply-to" => (&local.reply_to, &server.reply_to),
                _ => (&local.signature, &server.signature),
            };
            (field, shown(ours), shown(theirs))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use zmail_core::{
        IdentitySummary,
        identity_sync::{IdentitySyncStatus, IdentityValues, SyncedIdentity},
    };

    use super::{differences, identity_line, notice_text, notice_title};

    fn identity(status: IdentitySyncStatus) -> SyncedIdentity {
        let local = IdentityValues {
            name: "Support".to_owned(),
            reply_to: String::new(),
            signature: "-- Support".to_owned(),
        };
        SyncedIdentity {
            identity: IdentitySummary {
                id: 2,
                display_name: local.name.clone(),
                email_address: "help@example.com".to_owned(),
                signature: local.signature.clone(),
                reply_to: local.reply_to.clone(),
                is_primary: false,
            },
            server: Some(IdentityValues {
                name: "Help desk".to_owned(),
                reply_to: "tickets@example.com".to_owned(),
                signature: "-- Support".to_owned(),
            }),
            local,
            status,
            may_delete_on_server: true,
        }
    }

    #[test]
    fn notices_say_what_differs_and_what_the_choices_do() {
        let conflict = identity(IdentitySyncStatus::Conflict);
        assert_eq!(identity_line(&conflict), "Support <help@example.com>");
        assert_eq!(
            notice_title(&conflict).as_deref(),
            Some("help@example.com differs from the server")
        );
        assert!(
            notice_text(&conflict)
                .starts_with("Its name and reply-to differ in Zmail and on the server"),
            "{}",
            notice_text(&conflict)
        );
        assert!(
            notice_text(&identity(IdentitySyncStatus::ChangedHere)).contains("changed in Zmail")
        );
        assert_eq!(
            differences(&conflict),
            [
                ("name", "Support".to_owned(), "Help desk".to_owned()),
                (
                    "reply-to",
                    "(none)".to_owned(),
                    "tickets@example.com".to_owned()
                ),
            ]
        );
        assert!(
            notice_title(&identity(IdentitySyncStatus::NotOnServer))
                .is_some_and(|title| title.ends_with("is no longer on the server"))
        );
        assert_eq!(notice_title(&identity(IdentitySyncStatus::InSync)), None);
    }
}
