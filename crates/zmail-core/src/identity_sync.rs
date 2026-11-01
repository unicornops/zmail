//! Identities kept in step with a JMAP server's.
//!
//! Rob's decision of 1 November 2026: a sync adds the server's identities
//! Zmail doesn't have, keeps local edits, and offers the user to send a
//! local edit up to the server or to take the server's values instead. A
//! sync never overwrites a local edit silently.
//!
//! Each local identity linked to a server identity keeps the server's id
//! and the server's values as last seen ([`IdentityValues`]: the name, the
//! reply-to and the text signature; the email address is the match and
//! never changes, as RFC 8621 makes it immutable). Comparing the local
//! values, the values last seen and the server's values now tells who
//! changed what ([`merge`]):
//!
//! - only the server changed: the local identity takes the server's
//!   values;
//! - only the local identity changed: it keeps its values and differs
//!   from the server ([`IdentitySyncStatus::ChangedHere`]);
//! - both changed (to different values): it keeps its values too, and the
//!   difference is a conflict ([`IdentitySyncStatus::Conflict`]).
//!
//! An identity linked for the first time has no values last seen: its
//! blank fields take the server's ([`fill_blanks`]), and any other
//! difference is a conflict, since neither side can be said to be older.
//! A linked identity the server no longer lists is kept, marked
//! [`IdentitySyncStatus::NotOnServer`], for the user to remove or create
//! on the server again.

use crate::IdentitySummary;

/// What an identity says that the server keeps too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityValues {
    /// The display name.
    pub name: String,
    /// The reply-to addresses, as an address list (`Name <a@example.com>,
    /// b@example.com`); empty for none.
    pub reply_to: String,
    /// The plain-text signature.
    pub signature: String,
}

impl IdentityValues {
    /// The names of the fields that differ between `self` and `other`, in
    /// the order the frontends show them.
    pub fn differing_fields(&self, other: &Self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.name != other.name {
            fields.push("name");
        }
        if self.reply_to != other.reply_to {
            fields.push("reply-to");
        }
        if self.signature != other.signature {
            fields.push("signature");
        }
        fields
    }
}

/// An identity as a JMAP server lists it (`Identity/get`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    /// The server's id for it.
    pub id: String,
    /// Its email address.
    pub email: String,
    /// Its name, reply-to and text signature.
    pub values: IdentityValues,
    /// Whether the server lets the user delete it (`mayDelete`).
    pub may_delete: bool,
}

/// How a local identity stands against the server's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySyncStatus {
    /// Not linked to a server identity: an IMAP or Proton account's, or
    /// one a JMAP server has never listed.
    Local,
    /// The same as the server's.
    InSync,
    /// Changed in Zmail; the server's values are the ones last seen.
    ChangedHere,
    /// Changed in Zmail and on the server, to different values.
    Conflict,
    /// The server no longer lists it.
    NotOnServer,
}

impl IdentitySyncStatus {
    /// Whether the identity differs from the server's, so the user is
    /// offered to update the server or to use the server's values.
    pub fn differs(self) -> bool {
        matches!(self, Self::ChangedHere | Self::Conflict)
    }

    /// Whether the frontends draw a notice for it.
    pub fn needs_attention(self) -> bool {
        self.differs() || self == Self::NotOnServer
    }
}

/// An account's identity with how it stands against the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedIdentity {
    /// The identity.
    pub identity: IdentitySummary,
    /// Its name, reply-to and signature in Zmail.
    pub local: IdentityValues,
    /// The server's values as last seen, when it is linked to a server
    /// identity.
    pub server: Option<IdentityValues>,
    /// How it stands.
    pub status: IdentitySyncStatus,
}

/// What a sync does with a local identity linked to a server identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// The two agree.
    InSync,
    /// Only the server changed: the local identity takes its values.
    TakeServer,
    /// The local identity keeps its values and differs from the server's;
    /// `conflict` when the server changed too.
    KeepLocal {
        /// The server changed as well.
        conflict: bool,
    },
}

/// What a sync does with an identity whose values are `local`, which
/// last saw the server with `last_seen` (`None` when it is linked for the
/// first time), now that the server says `server`.
pub fn merge(
    local: &IdentityValues,
    last_seen: Option<&IdentityValues>,
    server: &IdentityValues,
) -> Merge {
    if local == server {
        return Merge::InSync;
    }
    match last_seen {
        Some(seen) if local == seen => Merge::TakeServer,
        Some(seen) if server == seen => Merge::KeepLocal { conflict: false },
        _ => Merge::KeepLocal { conflict: true },
    }
}

/// `local` with its blank fields taken from `server`, for an identity
/// linked for the first time: filling in what Zmail lacks overwrites no
/// edit.
pub fn fill_blanks(local: &IdentityValues, server: &IdentityValues) -> IdentityValues {
    let pick = |local: &str, server: &str| {
        if local.trim().is_empty() {
            server.to_owned()
        } else {
            local.to_owned()
        }
    };
    IdentityValues {
        name: pick(&local.name, &server.name),
        reply_to: pick(&local.reply_to, &server.reply_to),
        signature: pick(&local.signature, &server.signature),
    }
}

#[cfg(test)]
mod tests {
    use super::{IdentitySyncStatus, IdentityValues, Merge, fill_blanks, merge};

    fn values(name: &str, signature: &str) -> IdentityValues {
        IdentityValues {
            name: name.to_owned(),
            reply_to: String::new(),
            signature: signature.to_owned(),
        }
    }

    #[test]
    fn only_the_server_changing_takes_the_servers_values() {
        let seen = values("Rob", "-- Rob");
        let server = values("Rob L", "-- Rob");
        assert_eq!(merge(&seen, Some(&seen), &server), Merge::TakeServer);
    }

    #[test]
    fn only_the_local_side_changing_keeps_the_local_edit() {
        let seen = values("Rob", "-- Rob");
        let local = values("Rob", "-- Rob, from Zmail");
        assert_eq!(
            merge(&local, Some(&seen), &seen),
            Merge::KeepLocal { conflict: false }
        );
    }

    #[test]
    fn both_changing_is_a_conflict_unless_they_agree() {
        let seen = values("Rob", "-- Rob");
        let local = values("Rob", "-- local");
        let server = values("Rob", "-- server");
        assert_eq!(
            merge(&local, Some(&seen), &server),
            Merge::KeepLocal { conflict: true }
        );
        // The same edit on both sides is no conflict.
        assert_eq!(merge(&local, Some(&seen), &local), Merge::InSync);
        // Nothing changed.
        assert_eq!(merge(&seen, Some(&seen), &seen), Merge::InSync);
    }

    #[test]
    fn a_first_link_fills_blanks_and_calls_other_differences_a_conflict() {
        let local = values("Rob", "");
        let server = values("Robert", "-- Robert");
        let filled = fill_blanks(&local, &server);
        assert_eq!(filled, values("Rob", "-- Robert"));
        assert_eq!(
            merge(&filled, None, &server),
            Merge::KeepLocal { conflict: true }
        );
        assert_eq!(
            merge(&fill_blanks(&values("", ""), &server), None, &server),
            Merge::InSync
        );
    }

    #[test]
    fn differing_fields_and_statuses_read_as_the_frontends_show_them() {
        let mut other = values("Rob", "-- Rob");
        other.reply_to = "team@example.com".to_owned();
        assert_eq!(
            values("Robert", "-- Rob").differing_fields(&other),
            ["name", "reply-to"]
        );
        assert!(IdentitySyncStatus::Conflict.differs());
        assert!(IdentitySyncStatus::ChangedHere.differs());
        assert!(!IdentitySyncStatus::NotOnServer.differs());
        assert!(IdentitySyncStatus::NotOnServer.needs_attention());
        assert!(!IdentitySyncStatus::InSync.needs_attention());
        assert!(!IdentitySyncStatus::Local.needs_attention());
    }
}
