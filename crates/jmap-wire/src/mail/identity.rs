//! Identities (RFC 8621 section 6): who the user may send as.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::EmailAddress;
use crate::{
    capability,
    method::{GetObject, NoArguments, Object, PatchObject, Properties, SetObject},
};

/// The Identity data type (RFC 8621 section 6), for
/// [`Get`](crate::Get)`<Identity>` and [`Set`](crate::Set)`<Identity>`.
/// Its methods need the submission capability and no other.
#[derive(Debug, Clone, Copy)]
pub enum Identity {}

impl Object for Identity {
    const NAME: &'static str = "Identity";
    const CAPABILITY: &'static str = capability::SUBMISSION;
}

impl GetObject for Identity {
    type Record = IdentityRecord;
}

impl SetObject for Identity {
    type Create = NewIdentity;
    type Created = IdentityRecord;
    type Arguments = NoArguments;
}

/// An identity as the server returns it (RFC 8621 section 6). Every
/// property is optional: a `/get` may ask for only some, a `/set`
/// reports only those the server set, and servers leave some out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IdentityRecord {
    /// The identity's id.
    #[serde(default)]
    pub id: Option<String>,
    /// The name to send as.
    #[serde(default)]
    pub name: Option<String>,
    /// The address to send from; `*@example.com` allows any address of
    /// the domain.
    #[serde(default)]
    pub email: Option<String>,
    /// The `Reply-To` for new mail; `None` for `null` as for a property
    /// left out.
    #[serde(default)]
    pub reply_to: Option<Vec<EmailAddress>>,
    /// The `Bcc` for new mail.
    #[serde(default)]
    pub bcc: Option<Vec<EmailAddress>>,
    /// The signature for plain text mail.
    #[serde(default)]
    pub text_signature: Option<String>,
    /// The signature for HTML mail, as HTML.
    #[serde(default)]
    pub html_signature: Option<String>,
    /// Whether the user may destroy the identity.
    #[serde(default)]
    pub may_delete: Option<bool>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// An identity to create (RFC 8621 section 6.3). Properties that are
/// not set are left to the server's defaults.
///
/// ```
/// use jmap_wire::mail::{EmailAddress, NewIdentity};
/// use serde_json::json;
///
/// let identity = NewIdentity::new("joe@example.com")
///     .name("Joe Bloggs")
///     .reply_to(None)
///     .bcc(Some(vec![EmailAddress::new("joe+archive@example.com")]));
/// assert_eq!(
///     serde_json::to_value(&identity)?,
///     json!({
///         "email": "joe@example.com",
///         "name": "Joe Bloggs",
///         "replyTo": null,
///         "bcc": [{"name": null, "email": "joe+archive@example.com"}]
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewIdentity(Properties);

impl NewIdentity {
    /// An identity for `email`, which cannot be changed later.
    pub fn new(email: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("email", email.into());
        Self(properties)
    }

    /// The name to send as.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.0.set("name", name.into());
        self
    }

    /// The `Reply-To` for new mail; `None` is written as `null`.
    pub fn reply_to(mut self, addresses: Option<Vec<EmailAddress>>) -> Self {
        self.0.set_as("replyTo", &addresses);
        self
    }

    /// The `Bcc` for new mail; `None` is written as `null`.
    pub fn bcc(mut self, addresses: Option<Vec<EmailAddress>>) -> Self {
        self.0.set_as("bcc", &addresses);
        self
    }

    /// The signature for plain text mail.
    pub fn text_signature(mut self, signature: impl Into<String>) -> Self {
        self.0.set("textSignature", signature.into());
        self
    }

    /// The signature for HTML mail, as HTML.
    pub fn html_signature(mut self, signature: impl Into<String>) -> Self {
        self.0.set("htmlSignature", signature.into());
        self
    }

    /// Sets any other property.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// Changes to an identity (RFC 8621 section 6.3): a
/// [`PatchObject`] with a setter for each property a client may change.
/// The address is not among them: `email` is immutable.
///
/// ```
/// use jmap_wire::{PatchObject, Set, mail::{Identity, IdentityUpdate}};
///
/// let mut set = Set::<Identity>::new("u1");
/// set.update("i1", IdentityUpdate::new().name("Joe").reply_to(None));
/// # let _: PatchObject = IdentityUpdate::new().into();
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentityUpdate(PatchObject);

impl IdentityUpdate {
    /// No changes.
    pub fn new() -> Self {
        Self::default()
    }

    fn set(self, name: &str, value: Value) -> Self {
        Self(self.0.set(name, value))
    }

    /// The name to send as.
    pub fn name(self, name: impl Into<String>) -> Self {
        self.set("name", Value::String(name.into()))
    }

    /// The `Reply-To` for new mail; `None` clears it.
    pub fn reply_to(self, addresses: Option<Vec<EmailAddress>>) -> Self {
        self.set("replyTo", addresses_value(addresses))
    }

    /// The `Bcc` for new mail; `None` clears it.
    pub fn bcc(self, addresses: Option<Vec<EmailAddress>>) -> Self {
        self.set("bcc", addresses_value(addresses))
    }

    /// The signature for plain text mail.
    pub fn text_signature(self, signature: impl Into<String>) -> Self {
        self.set("textSignature", Value::String(signature.into()))
    }

    /// The signature for HTML mail, as HTML.
    pub fn html_signature(self, signature: impl Into<String>) -> Self {
        self.set("htmlSignature", Value::String(signature.into()))
    }
}

impl From<IdentityUpdate> for PatchObject {
    fn from(update: IdentityUpdate) -> Self {
        update.0
    }
}

/// `EmailAddress[]|null` as JSON.
fn addresses_value(addresses: Option<Vec<EmailAddress>>) -> Value {
    serde_json::to_value(addresses).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Get, Request, Response, Set};

    /// The `Identity/get` request and response of RFC 8621 section 6.4
    /// (the HTML signature, wrapped in the RFC, on one line).
    #[test]
    fn the_identity_get_of_rfc_8621_section_6_4_is_written_and_read() {
        let mut request = Request::new();
        let call = request.call(Get::<Identity>::new("acme")).expect("encodes");
        assert_eq!(
            serde_json::to_value(&request).expect("encodes"),
            json!({
                "using": [capability::CORE, capability::SUBMISSION],
                "methodCalls": [["Identity/get", {"accountId": "acme"}, "c0"]]
            })
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Identity/get", {
                "accountId": "acme",
                "state": "99401312ae-11-333",
                "list": [
                    {
                        "id": "XD-3301-222-11_22AAz",
                        "name": "Joe Bloggs",
                        "email": "joe@example.com",
                        "replyTo": null,
                        "bcc": [{"name": null, "email": "joe+archive@example.com"}],
                        "textSignature": "-- \nJoe Bloggs\nMaster of Email",
                        "htmlSignature":
                            "<div><b>Joe Bloggs</b></div><div>Master of Email</div>",
                        "mayDelete": false
                    },
                    {
                        "id": "XD-9911312-11_22AAz",
                        "name": "Joe B",
                        "email": "*@example.com",
                        "replyTo": null,
                        "bcc": null,
                        "textSignature": "",
                        "htmlSignature": "",
                        "mayDelete": true
                    }
                ],
                "notFound": []
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let identities = response.get(&call).expect("answered");
        assert_eq!(identities.state.as_deref(), Some("99401312ae-11-333"));
        let [joe, wildcard] = identities.list.as_slice() else {
            panic!("two identities");
        };
        assert_eq!(joe.id.as_deref(), Some("XD-3301-222-11_22AAz"));
        assert_eq!(joe.reply_to, None);
        assert_eq!(
            joe.bcc,
            Some(vec![EmailAddress::new("joe+archive@example.com")])
        );
        assert_eq!(joe.may_delete, Some(false));
        assert_eq!(wildcard.email.as_deref(), Some("*@example.com"));
        assert_eq!(wildcard.bcc, None);
        assert_eq!(wildcard.text_signature.as_deref(), Some(""));
    }

    #[test]
    fn an_identity_set_creates_updates_and_destroys() {
        let mut set = Set::<Identity>::new("acme");
        let creation = set.create(
            NewIdentity::new("help@example.com")
                .name("Help desk")
                .text_signature("-- Help")
                .property("sortOrder", 100),
        );
        set.update(
            "i1",
            IdentityUpdate::new()
                .name("Joe")
                .reply_to(Some(vec![EmailAddress::named("Tickets", "t@example.com")]))
                .bcc(None)
                .html_signature("<b>Joe</b>"),
        );
        set.destroy(["i2"]);
        assert_eq!(
            serde_json::to_value(&set).expect("encodes"),
            json!({
                "accountId": "acme",
                "create": {creation: {
                    "email": "help@example.com", "name": "Help desk",
                    "textSignature": "-- Help", "sortOrder": 100
                }},
                "update": {"i1": {
                    "name": "Joe",
                    "replyTo": [{"name": "Tickets", "email": "t@example.com"}],
                    "bcc": null,
                    "htmlSignature": "<b>Joe</b>"
                }},
                "destroy": ["i2"]
            })
        );
    }

    #[test]
    fn records_keep_properties_from_later_specifications() {
        let record: IdentityRecord =
            serde_json::from_value(json!({"id": "i1", "sortOrder": 100})).expect("decodes");
        assert_eq!(record.extra["sortOrder"], 100);
        assert_eq!(record.email, None);
    }
}
