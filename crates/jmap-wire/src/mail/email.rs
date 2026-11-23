//! Emails (RFC 8621 section 4): for now, `Email/set` and the address
//! type other data types share.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability,
    method::{NoArguments, Object, Properties, SetObject},
};

/// The Email data type (RFC 8621 section 4), for
/// [`Set`](crate::Set)`<Email>`: `Email/set`, which creates, updates
/// (a [`PatchObject`](crate::PatchObject) on `keywords` and
/// `mailboxIds`, mostly) and destroys emails.
///
/// ```
/// use jmap_wire::{Request, Set, capability, mail::Email};
///
/// let mut set = Set::<Email>::new("u1");
/// set.destroy(["M1", "M2"]);
/// let mut request = Request::new();
/// request.call(set)?;
/// assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub enum Email {}

impl Object for Email {
    const NAME: &'static str = "Email";
    const CAPABILITY: &'static str = capability::MAIL;
}

impl SetObject for Email {
    type Create = NewEmail;
    type Created = BTreeMap<String, Value>;
    type Arguments = NoArguments;
}

/// An email to create with `Email/set` (RFC 8621 section 4.6), property
/// by property. This crate has no typed setters for an email's
/// properties yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewEmail(Properties);

impl NewEmail {
    /// An email with no properties set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a property, such as `mailboxIds`, `keywords`, `subject` or
    /// `bodyStructure`.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// A name and an address, as in a header (RFC 8621 section 4.1.2.3) or
/// an identity's `replyTo` and `bcc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    /// The display name, if any. Written as `null` when there is none.
    #[serde(default)]
    pub name: Option<String>,
    /// The address itself.
    pub email: String,
}

impl EmailAddress {
    /// An address without a name.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }

    /// An address with a display name.
    pub fn named(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            email: email.into(),
        }
    }
}
