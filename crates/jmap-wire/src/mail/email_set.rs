//! Writing emails (RFC 8621 sections 4.6 to 4.8): what `Email/set`
//! creates ([`NewEmail`]) and changes ([`EmailUpdate`]), `Email/import`
//! ([`EmailImport`]) and what `Email/copy` copies ([`EmailCopy`]).

use std::{borrow::Cow, collections::BTreeMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Date, EmailAddress, EmailRecord, UtcDate, header::HeaderProperty};
use crate::{
    capability,
    error::SetError,
    method::{Method, PatchObject, Properties, SetOutcome, null_as_default},
};

/// A JMAP set: ids or keywords, each mapped to `true` (RFC 8621 section
/// 4.1.1).
fn set_of(members: impl IntoIterator<Item = impl Into<String>>) -> Value {
    Value::Object(
        members
            .into_iter()
            .map(|member| (member.into(), Value::Bool(true)))
            .collect::<Map<_, _>>(),
    )
}

/// An email to create with `Email/set` (RFC 8621 section 4.6): a draft,
/// mostly. The server makes the message (RFC 5322) from the properties
/// given, and adds a `Message-ID` and a `Date` when none is set.
///
/// A message that exists already as text is not created this way: it is
/// uploaded and imported ([`EmailImport`]).
///
/// The body is given as parts: either `bodyStructure`
/// ([`body_structure`](Self::body_structure)) for the whole MIME tree,
/// or the flat lists [`text_body`](Self::text_body),
/// [`html_body`](Self::html_body) and
/// [`attachments`](Self::attachments), from which the server builds the
/// tree. A part's content is text given in the same call
/// ([`NewBodyPart::text`] with [`body_value`](Self::body_value)) or a
/// blob uploaded before ([`NewBodyPart::blob`]).
///
/// ```
/// use jmap_wire::{Request, Set, mail::{Email, EmailAddress, NewBodyPart, NewEmail}};
/// use serde_json::json;
///
/// let draft = NewEmail::new()
///     .mailbox_ids(["drafts"])
///     .keywords(["$draft", "$seen"])
///     .from([EmailAddress::named("Joe Bloggs", "joe@example.com")])
///     .to([EmailAddress::new("jane@example.com")])
///     .subject("World domination")
///     .text_body(NewBodyPart::text("body"))
///     .body_value("body", "I have the most brilliant plan.")
///     .attachments([NewBodyPart::blob("G4c6751", "application/pdf").name("plan.pdf")]);
/// assert_eq!(
///     serde_json::to_value(&draft)?,
///     json!({
///         "mailboxIds": {"drafts": true},
///         "keywords": {"$draft": true, "$seen": true},
///         "from": [{"name": "Joe Bloggs", "email": "joe@example.com"}],
///         "to": [{"name": null, "email": "jane@example.com"}],
///         "subject": "World domination",
///         "textBody": [{"partId": "body", "type": "text/plain"}],
///         "bodyValues": {"body": {"value": "I have the most brilliant plan."}},
///         "attachments": [{"blobId": "G4c6751", "type": "application/pdf",
///                          "name": "plan.pdf", "disposition": "attachment"}]
///     })
/// );
///
/// let mut set = Set::<Email>::new("u1");
/// let creation_id = set.create(draft);
/// let mut request = Request::new();
/// request.call(set)?;
/// # let _ = creation_id;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # What a server refuses
///
/// Section 4.6 lists what must not be combined; a server should answer
/// `invalidProperties` for a creation that breaks one of these, and
/// this type does not check them:
///
/// - `bodyStructure` together with any of the three flat lists;
/// - the same header field twice (`subject` and `header:Subject`);
/// - a `Content-*` header field on the email rather than on a part;
/// - `textBody` or `htmlBody` with other than exactly one part, of
///   `text/plain` and `text/html`;
/// - an email in no mailbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewEmail(Properties);

impl NewEmail {
    /// An email with no properties set. It needs at least
    /// [`mailbox_ids`](Self::mailbox_ids).
    pub fn new() -> Self {
        Self::default()
    }

    /// The mailboxes to put it in: at least one.
    pub fn mailbox_ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set("mailboxIds", set_of(ids));
        self
    }

    /// Its keywords, such as `$draft` and `$seen`.
    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set("keywords", set_of(keywords));
        self
    }

    /// When it was received; the time of creation unless set.
    pub fn received_at(mut self, date: impl Into<UtcDate>) -> Self {
        self.0.set_as("receivedAt", &date.into());
        self
    }

    /// The `Message-ID` field: ids without angle brackets. The server
    /// makes one when none is given.
    pub fn message_id(self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.ids("messageId", ids)
    }

    /// The `In-Reply-To` field: the ids of the messages this one
    /// answers.
    pub fn in_reply_to(self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.ids("inReplyTo", ids)
    }

    /// The `References` field: the ids of the thread so far.
    pub fn references(self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.ids("references", ids)
    }

    /// The `Sender` field.
    pub fn sender(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("sender", addresses)
    }

    /// The `From` field.
    pub fn from(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("from", addresses)
    }

    /// The `To` field.
    pub fn to(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("to", addresses)
    }

    /// The `Cc` field.
    pub fn cc(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("cc", addresses)
    }

    /// The `Bcc` field.
    pub fn bcc(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("bcc", addresses)
    }

    /// The `Reply-To` field.
    pub fn reply_to(self, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.addresses("replyTo", addresses)
    }

    /// The `Subject` field.
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.0.set("subject", subject.into());
        self
    }

    /// The `Date` field. The server sets it when none is given.
    pub fn sent_at(mut self, date: impl Into<Date>) -> Self {
        self.0.set_as("sentAt", &date.into());
        self
    }

    /// Any other header field, in the form `property` names (RFC 8621
    /// section 4.1.2): `HeaderProperty::text("X-Priority")` with a
    /// string, `HeaderProperty::addresses("Resent-To")` with a list of
    /// addresses, and so on. A field the email has a property for above
    /// must not be set this way too.
    pub fn header(mut self, property: HeaderProperty, value: impl Into<Value>) -> Self {
        self.0.set(property.to_string(), value);
        self
    }

    /// The whole MIME structure (`bodyStructure`), instead of the flat
    /// lists.
    pub fn body_structure(mut self, part: NewBodyPart) -> Self {
        self.0.set("bodyStructure", part.into_value());
        self
    }

    /// The plain text body (`textBody`): one part of type `text/plain`.
    pub fn text_body(mut self, part: NewBodyPart) -> Self {
        self.0
            .set("textBody", Value::Array(vec![part.into_value()]));
        self
    }

    /// The HTML body (`htmlBody`): one part of type `text/html`.
    pub fn html_body(mut self, part: NewBodyPart) -> Self {
        self.0
            .set("htmlBody", Value::Array(vec![part.into_value()]));
        self
    }

    /// The attachments (`attachments`), and the inline parts the HTML
    /// refers to by `cid:`.
    pub fn attachments(mut self, parts: impl IntoIterator<Item = NewBodyPart>) -> Self {
        self.0.set(
            "attachments",
            Value::Array(parts.into_iter().map(NewBodyPart::into_value).collect()),
        );
        self
    }

    /// The text of the part with this `partId` (`bodyValues`). Every
    /// part made with [`NewBodyPart::text`] needs one.
    pub fn body_value(mut self, part_id: impl Into<String>, text: impl Into<String>) -> Self {
        let mut values = match self.0.get("bodyValues") {
            Some(Value::Object(values)) => values.clone(),
            _ => Map::new(),
        };
        let mut value = Map::new();
        value.insert("value".to_owned(), Value::String(text.into()));
        values.insert(part_id.into(), Value::Object(value));
        self.0.set("bodyValues", Value::Object(values));
        self
    }

    /// Sets any other property, or one of those above as JSON of your
    /// own.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }

    fn ids(mut self, name: &str, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set(
            name,
            Value::Array(ids.into_iter().map(|id| Value::String(id.into())).collect()),
        );
        self
    }

    fn addresses(mut self, name: &str, addresses: impl IntoIterator<Item = EmailAddress>) -> Self {
        self.0
            .set_as(name, &addresses.into_iter().collect::<Vec<_>>());
        self
    }
}

/// A body part of an email to create (RFC 8621 section 4.6): its
/// content is either text given in the same call or a blob, never both,
/// or other parts.
///
/// ```
/// use jmap_wire::mail::NewBodyPart;
/// use serde_json::json;
///
/// // multipart/alternative of a plain and an HTML part.
/// let body = NewBodyPart::multipart(
///     "multipart/alternative",
///     [NewBodyPart::text("plain"), NewBodyPart::html("html")],
/// );
/// assert_eq!(
///     body.into_value(),
///     json!({"type": "multipart/alternative", "subParts": [
///         {"partId": "plain", "type": "text/plain"},
///         {"partId": "html", "type": "text/html"}
///     ]})
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewBodyPart(Properties);

impl NewBodyPart {
    /// A `text/plain` part whose text is the body value `part_id` of
    /// the email ([`NewEmail::body_value`]). The server chooses the
    /// charset and the transfer encoding.
    pub fn text(part_id: impl Into<String>) -> Self {
        Self::inline(part_id, "text/plain")
    }

    /// A `text/html` part whose text is the body value `part_id`.
    pub fn html(part_id: impl Into<String>) -> Self {
        Self::inline(part_id, "text/html")
    }

    /// A part of the media type `content_type` whose text is the body
    /// value `part_id`.
    pub fn inline(part_id: impl Into<String>, content_type: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("partId", part_id.into());
        properties.set("type", content_type.into());
        Self(properties)
    }

    /// A part whose content is a blob, uploaded before
    /// ([`Client::upload`](crate::Client::upload)) or taken from
    /// another email, as an attachment of the media type
    /// `content_type`. A blob id the server does not have fails the
    /// creation with `blobNotFound`
    /// ([`SetError::blobs_not_found`]).
    pub fn blob(blob_id: impl Into<String>, content_type: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("blobId", blob_id.into());
        properties.set("type", content_type.into());
        properties.set("disposition", "attachment");
        Self(properties)
    }

    /// A `multipart/*` part of these parts.
    pub fn multipart(
        content_type: impl Into<String>,
        parts: impl IntoIterator<Item = NewBodyPart>,
    ) -> Self {
        let mut properties = Properties::new();
        properties.set("type", content_type.into());
        properties.set(
            "subParts",
            Value::Array(parts.into_iter().map(Self::into_value).collect()),
        );
        Self(properties)
    }

    /// The file name (`name`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.0.set("name", name.into());
        self
    }

    /// The disposition: `attachment` (what [`blob`](Self::blob) starts
    /// with) or `inline`.
    pub fn disposition(mut self, disposition: impl Into<String>) -> Self {
        self.0.set("disposition", disposition.into());
        self
    }

    /// The `Content-ID`, without angle brackets, for HTML to refer to
    /// the part by `cid:`. Sets the disposition to `inline` as well.
    pub fn cid(mut self, cid: impl Into<String>) -> Self {
        self.0.set("cid", cid.into());
        self.disposition("inline")
    }

    /// The charset of a blob's text. It must not be set on a part whose
    /// text is a body value.
    pub fn charset(mut self, charset: impl Into<String>) -> Self {
        self.0.set("charset", charset.into());
        self
    }

    /// The `Content-Language` tags.
    pub fn language(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set(
            "language",
            Value::Array(
                tags.into_iter()
                    .map(|tag| Value::String(tag.into()))
                    .collect(),
            ),
        );
        self
    }

    /// The `Content-Location`.
    pub fn location(mut self, location: impl Into<String>) -> Self {
        self.0.set("location", location.into());
        self
    }

    /// Any other header field of the part, as on
    /// [`NewEmail::header`]. `Content-Transfer-Encoding` must not be
    /// given: the server chooses it.
    pub fn header(mut self, property: HeaderProperty, value: impl Into<Value>) -> Self {
        self.0.set(property.to_string(), value);
        self
    }

    /// Sets any other property.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }

    /// The part as the JSON it is sent as.
    pub fn into_value(self) -> Value {
        serde_json::to_value(self.0).unwrap_or(Value::Null)
    }
}

/// The changes to one email in an `Email/set` update (RFC 8621 section
/// 4.6), for [`Set::update`](crate::Set::update). Only an email's
/// keywords and mailboxes can change; everything else of a message is
/// immutable.
///
/// Each of the two is changed either one member at a time or as a
/// whole:
///
/// | | One at a time | The whole set |
/// | --- | --- | --- |
/// | Keywords | [`set_keyword`](Self::set_keyword), [`clear_keyword`](Self::clear_keyword) | [`replace_keywords`](Self::replace_keywords) |
/// | Mailboxes | [`add_to_mailbox`](Self::add_to_mailbox), [`remove_from_mailbox`](Self::remove_from_mailbox), [`move_between`](Self::move_between) | [`replace_mailboxes`](Self::replace_mailboxes) |
///
/// Prefer the first column. A whole set overwrites what another client
/// changed in the meantime, and replacing an email's mailboxes takes it
/// out of every mailbox the caller did not know of: on a server where
/// mailboxes are labels, moving an email "to the archive" that way
/// strips its labels.
///
/// **A member is removed with `null`.** RFC 8620 section 5.3 removes a
/// key of a map by patching it to `null`, and RFC 8621 section 4.1.1
/// allows no other value than `true` in either set; a patch of `false`
/// is refused as `invalidProperties` by servers that hold to that.
/// There is deliberately no method here that takes a `bool`.
///
/// ```
/// use jmap_wire::{PatchObject, mail::EmailUpdate};
/// use serde_json::json;
///
/// // Unread, unflagged, and from the inbox to the archive.
/// let update = EmailUpdate::new()
///     .clear_keyword("$seen")
///     .clear_keyword("$flagged")
///     .move_between("inbox", "archive");
/// assert_eq!(
///     serde_json::to_value(PatchObject::from(update))?,
///     json!({
///         "keywords/$seen": null,
///         "keywords/$flagged": null,
///         "mailboxIds/inbox": null,
///         "mailboxIds/archive": true
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// # An email must stay in a mailbox
///
/// An email is in at least one mailbox at all times (RFC 8621 section
/// 4.1.1), and a change that leaves it in none is refused. The entries
/// of a patch are applied together, not in order (RFC 8620 section
/// 5.3), so removing an email from its only mailbox and adding it to
/// another in one update, as [`move_between`](Self::move_between) does,
/// is a valid change. To delete an email "to the trash", replace its
/// mailboxes with the trash mailbox alone (section 4.6).
///
/// # One change per property
///
/// A patch may not set a property and a path into it (RFC 8620 section
/// 5.3). When both are asked for, the later call wins:
/// `set_keyword("a").replace_keywords(["b"])` replaces, and
/// `replace_keywords(["b"]).set_keyword("a")` only sets `a`.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use]
pub struct EmailUpdate(PatchObject);

impl EmailUpdate {
    /// An update that changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Gives the email `keyword` (`"keywords/<keyword>": true`):
    /// `$seen` to mark it read, `$flagged` to flag it.
    pub fn set_keyword(self, keyword: &str) -> Self {
        self.member("keywords", keyword, Value::Bool(true))
    }

    /// Takes `keyword` off the email (`"keywords/<keyword>": null`):
    /// `$seen` to mark it unread, `$flagged` to unflag it. Clearing a
    /// keyword the email does not have changes nothing.
    pub fn clear_keyword(self, keyword: &str) -> Self {
        self.member("keywords", keyword, Value::Null)
    }

    /// Replaces all of the email's keywords with these, none if empty.
    pub fn replace_keywords(
        mut self,
        keywords: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.0.clear_property("keywords");
        Self(self.0.set("keywords", set_of(keywords)))
    }

    /// Puts the email in the mailbox `mailbox_id` as well
    /// (`"mailboxIds/<id>": true`), leaving it where else it is.
    pub fn add_to_mailbox(self, mailbox_id: &str) -> Self {
        self.member("mailboxIds", mailbox_id, Value::Bool(true))
    }

    /// Takes the email out of the mailbox `mailbox_id`
    /// (`"mailboxIds/<id>": null`), leaving it where else it is. The
    /// server refuses this when it is in no other mailbox and the same
    /// update adds none.
    pub fn remove_from_mailbox(self, mailbox_id: &str) -> Self {
        self.member("mailboxIds", mailbox_id, Value::Null)
    }

    /// Moves the email from one mailbox to another, leaving it in any
    /// other mailbox it is in: [`remove_from_mailbox`] and
    /// [`add_to_mailbox`] in one update.
    ///
    /// [`remove_from_mailbox`]: Self::remove_from_mailbox
    /// [`add_to_mailbox`]: Self::add_to_mailbox
    pub fn move_between(self, from_mailbox_id: &str, to_mailbox_id: &str) -> Self {
        self.remove_from_mailbox(from_mailbox_id)
            .add_to_mailbox(to_mailbox_id)
    }

    /// Replaces all of the email's mailboxes with these: it leaves
    /// every other mailbox, known to the caller or not. There must be
    /// at least one.
    pub fn replace_mailboxes(
        mut self,
        mailbox_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.0.clear_property("mailboxIds");
        Self(self.0.set("mailboxIds", set_of(mailbox_ids)))
    }

    /// Whether the update changes nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn member(mut self, property: &str, member: &str, value: Value) -> Self {
        self.0.clear_whole(property);
        Self(self.0.set(PatchObject::path([property, member]), value))
    }
}

impl From<EmailUpdate> for PatchObject {
    fn from(update: EmailUpdate) -> Self {
        update.0
    }
}

/// `Email/import` (RFC 8621 section 4.8): adds messages (RFC 5322) that
/// were uploaded as blobs ([`Client::upload`](crate::Client::upload))
/// to the account's emails. It is how a message that exists as text
/// gets in: a sent message's copy, mail from another account.
///
/// ```
/// use jmap_wire::{Request, mail::{EmailImport, EmailToImport}};
/// use serde_json::json;
///
/// let mut import = EmailImport::new("u1");
/// let creation_id = import.email(
///     EmailToImport::new("blob-of-the-message", ["sent"])
///         .keywords(["$seen"])
///         .received_at("2026-11-23T10:00:00Z"),
/// );
/// let mut request = Request::new();
/// let call = request.call(import)?;
/// assert_eq!(
///     request.method_calls()[0].arguments,
///     json!({
///         "accountId": "u1",
///         "emails": {"k0": {
///             "blobId": "blob-of-the-message",
///             "mailboxIds": {"sent": true},
///             "keywords": {"$seen": true},
///             "receivedAt": "2026-11-23T10:00:00Z"
///         }}
///     })
/// );
/// # let _ = (call, creation_id);
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct EmailImport {
    account_id: String,
    if_in_state: Option<String>,
    emails: BTreeMap<String, EmailToImport>,
    next_creation: usize,
}

impl EmailImport {
    /// An import into the account that imports nothing yet.
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            if_in_state: None,
            emails: BTreeMap::new(),
            next_creation: 0,
        }
    }

    /// Makes the call fail with `stateMismatch` unless the Email state
    /// is `state`.
    pub fn if_in_state(&mut self, state: impl Into<String>) -> &mut Self {
        self.if_in_state = Some(state.into());
        self
    }

    /// Imports a message. Returns its creation id (`k0`, `k1`...), the
    /// key of the answer's `created` or `notCreated`; `#` and the
    /// creation id stands for the new email's id later in the request,
    /// as the `emailId` of an EmailSubmission, say.
    pub fn email(&mut self, email: EmailToImport) -> String {
        let mut creation_id = format!("k{}", self.next_creation);
        while self.emails.contains_key(&creation_id) {
            self.next_creation += 1;
            creation_id = format!("k{}", self.next_creation);
        }
        self.next_creation += 1;
        self.emails.insert(creation_id.clone(), email);
        creation_id
    }

    /// Imports a message under a creation id of your choosing.
    pub fn email_as(&mut self, creation_id: impl Into<String>, email: EmailToImport) -> &mut Self {
        self.emails.insert(creation_id.into(), email);
        self
    }
}

impl Serialize for EmailImport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Call<'a> {
            account_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            if_in_state: Option<&'a str>,
            emails: &'a BTreeMap<String, EmailToImport>,
        }

        Call {
            account_id: &self.account_id,
            if_in_state: self.if_in_state.as_deref(),
            emails: &self.emails,
        }
        .serialize(serializer)
    }
}

impl Method for EmailImport {
    type Response = EmailImportResponse;

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("Email/import")
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(capability::MAIL)]
    }
}

/// One message of an [`EmailImport`] (RFC 8621 section 4.8, the
/// *EmailImport* object).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EmailToImport(Properties);

impl EmailToImport {
    /// The message in the blob `blob_id`, into these mailboxes: at
    /// least one.
    pub fn new(
        blob_id: impl Into<String>,
        mailbox_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut properties = Properties::new();
        properties.set("blobId", blob_id.into());
        properties.set("mailboxIds", set_of(mailbox_ids));
        Self(properties)
    }

    /// The keywords to give the email; none unless set.
    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set("keywords", set_of(keywords));
        self
    }

    /// When the email was received. Unless set, the server takes the
    /// time of the message's newest `Received` field, or the time of
    /// the import.
    pub fn received_at(mut self, date: impl Into<UtcDate>) -> Self {
        self.0.set_as("receivedAt", &date.into());
        self
    }

    /// Sets any other property.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// The answer to an `Email/import` (RFC 8621 section 4.8).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EmailImportResponse {
    /// The account imported into.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The Email state before the call.
    #[serde(default)]
    pub old_state: Option<String>,
    /// The Email state after the call.
    #[serde(default)]
    pub new_state: Option<String>,
    /// The emails created, by creation id: their `id`, `blobId`,
    /// `threadId` and `size`. The `blobId` differs from the one
    /// imported when the server repaired the message.
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: BTreeMap<String, EmailRecord>,
    /// Why messages were not imported, by creation id:
    /// `invalidProperties` (the blob, a mailbox or a keyword is not
    /// valid), `overQuota`, `invalidEmail`, or `alreadyExists` from a
    /// server that forbids duplicates, with the id of the email that is
    /// there ([`SetError::existing_id`]).
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_created: BTreeMap<String, SetError>,
}

impl EmailImportResponse {
    /// What became of the message imported under `creation_id`.
    pub fn outcome_of(&self, creation_id: &str) -> SetOutcome<'_, &EmailRecord> {
        match (
            self.created.get(creation_id),
            self.not_created.get(creation_id),
        ) {
            (Some(created), _) => SetOutcome::Done(created),
            (None, Some(error)) => SetOutcome::Refused(error),
            (None, None) => SetOutcome::Unanswered,
        }
    }
}

/// One email of an `Email/copy`
/// ([`CopyMethod`](crate::CopyMethod)`<Email>`, RFC 8621 section 4.7):
/// the email to copy from the other account, and the mailboxes,
/// keywords and received date of the copy, which are all a copy may
/// set. The message itself is copied as it is.
///
/// The mailboxes are those of the account copied into, so a copy needs
/// its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EmailCopy(Properties);

impl EmailCopy {
    /// A copy of the email `id` of the account copied from.
    pub fn new(id: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("id", id.into());
        Self(properties)
    }

    /// The mailboxes, of the account copied into, to put the copy in.
    pub fn mailbox_ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set("mailboxIds", set_of(ids));
        self
    }

    /// The copy's keywords, instead of the original's.
    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.set("keywords", set_of(keywords));
        self
    }

    /// The copy's received date, instead of the original's.
    pub fn received_at(mut self, date: impl Into<UtcDate>) -> Self {
        self.0.set_as("receivedAt", &date.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        CopyMethod, Request, Response, Set, SetErrorType,
        mail::{Email, HeaderProperty},
    };

    fn patch(update: EmailUpdate) -> String {
        serde_json::to_string(&PatchObject::from(update)).expect("encodes")
    }

    /// RFC 8620 section 5.3: a key leaves a map by being patched to
    /// `null`. RFC 8621 section 4.1.1: the only value in `keywords` is
    /// `true`. So no keyword change is ever written as `false`.
    #[test]
    fn clearing_a_keyword_sends_null_never_false() {
        assert_eq!(
            patch(EmailUpdate::new().clear_keyword("$seen")),
            r#"{"keywords/$seen":null}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().clear_keyword("$flagged")),
            r#"{"keywords/$flagged":null}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().set_keyword("$seen")),
            r#"{"keywords/$seen":true}"#
        );
        for update in [
            EmailUpdate::new().clear_keyword("$seen").set_keyword("a"),
            EmailUpdate::new()
                .remove_from_mailbox("m")
                .add_to_mailbox("n"),
            EmailUpdate::new()
                .replace_keywords(["a"])
                .replace_mailboxes(["m"]),
        ] {
            assert!(!patch(update).contains("false"));
        }
    }

    /// RFC 8621 section 4.6: mailboxes are added and removed one at a
    /// time with the patch syntax, and leaving one is `null`.
    #[test]
    fn leaving_a_mailbox_sends_null_and_touches_no_other() {
        assert_eq!(
            patch(EmailUpdate::new().remove_from_mailbox("inbox")),
            r#"{"mailboxIds/inbox":null}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().add_to_mailbox("label")),
            r#"{"mailboxIds/label":true}"#
        );
        // BTreeMap order: the entries of a patch have no order on the
        // wire either (RFC 8620 section 5.3).
        assert_eq!(
            patch(EmailUpdate::new().move_between("inbox", "archive")),
            r#"{"mailboxIds/archive":true,"mailboxIds/inbox":null}"#
        );
        // A move to where it is already only adds.
        assert_eq!(
            patch(EmailUpdate::new().move_between("inbox", "inbox")),
            r#"{"mailboxIds/inbox":true}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().replace_mailboxes(["trash"])),
            r#"{"mailboxIds":{"trash":true}}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().replace_keywords(Vec::<String>::new())),
            r#"{"keywords":{}}"#
        );
    }

    /// RFC 8620 section 5.3: no two patches where one pointer is the
    /// prefix of the other.
    #[test]
    fn a_property_is_never_patched_whole_and_in_part() {
        assert_eq!(
            patch(
                EmailUpdate::new()
                    .set_keyword("a")
                    .clear_keyword("b")
                    .replace_keywords(["c"])
            ),
            r#"{"keywords":{"c":true}}"#
        );
        assert_eq!(
            patch(EmailUpdate::new().replace_keywords(["c"]).set_keyword("a")),
            r#"{"keywords/a":true}"#
        );
        assert_eq!(
            patch(
                EmailUpdate::new()
                    .replace_mailboxes(["m"])
                    .add_to_mailbox("n")
                    .set_keyword("a")
            ),
            r#"{"keywords/a":true,"mailboxIds/n":true}"#
        );
        assert!(EmailUpdate::new().is_empty());
    }

    /// Ids and keywords with the characters a JSON Pointer escapes.
    #[test]
    fn members_are_escaped_as_json_pointer_parts() {
        assert_eq!(
            patch(
                EmailUpdate::new()
                    .add_to_mailbox("a/b~c")
                    .clear_keyword("x/y")
            ),
            r#"{"keywords/x~1y":null,"mailboxIds/a~1b~0c":true}"#
        );
    }

    #[test]
    fn an_email_set_carries_typed_updates_and_reads_each_outcome() {
        let mut set = Set::<Email>::new("u1");
        set.if_in_state("s1");
        set.update("M1", EmailUpdate::new().clear_keyword("$seen"));
        set.update("M2", EmailUpdate::new().move_between("a", "b"));
        let created = set.create(NewEmail::new().mailbox_ids(["drafts"]));
        let mut request = Request::new();
        let call = request.call(set).expect("encodes");
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "accountId": "u1",
                "ifInState": "s1",
                "create": {"k0": {"mailboxIds": {"drafts": true}}},
                "update": {
                    "M1": {"keywords/$seen": null},
                    "M2": {"mailboxIds/a": null, "mailboxIds/b": true}
                }
            })
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Email/set", {
                "accountId": "u1", "oldState": "s1", "newState": "s2",
                "created": {"k0": {"id": "M9", "blobId": "B9", "threadId": "T9", "size": 120}},
                "updated": {"M1": null},
                "notUpdated": {"M2": {"type": "invalidProperties",
                    "properties": ["mailboxIds"], "description": "no mailbox b"}}
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        let email = answer.outcome_of_create(&created).done().expect("created");
        assert_eq!(
            (email.id.as_deref(), email.blob_id.as_deref(), email.size),
            (Some("M9"), Some("B9"), Some(120))
        );
        assert!(answer.outcome_of_update("M1").is_done());
        let refusal = answer.outcome_of_update("M2").refusal().expect("refused");
        assert_eq!(refusal.error_type, SetErrorType::InvalidProperties);
        assert!(refusal.names_property("mailboxIds"));
        assert!(!refusal.names_property("mailbox"));
    }

    /// The SetErrors RFC 8621 section 4.6 adds, with their properties,
    /// and one from no RFC.
    #[test]
    fn the_set_errors_of_emails_are_read_with_their_properties() {
        let answer: crate::SetResponse<EmailRecord> = serde_json::from_value(json!({
            "notCreated": {
                "k0": {"type": "blobNotFound", "notFound": ["B1", "B2"]},
                "k1": {"type": "tooLarge"},
                "k2": {"type": "overQuota", "description": "Over quota"}
            },
            "notUpdated": {
                "M1": {"type": "tooManyKeywords"},
                "M2": {"type": "tooManyMailboxes"},
                "M3": {"type": "notFound"},
                "M4": {"type": "invalidPatch"},
                "M5": {"type": "mailboxLocked", "retryAfter": 30}
            }
        }))
        .expect("decodes");
        let refusal = |id: &str| answer.outcome_of_create(id).refusal().expect("refused");
        assert_eq!(refusal("k0").error_type, SetErrorType::BlobNotFound);
        assert_eq!(refusal("k0").blobs_not_found(), ["B1", "B2"]);
        assert_eq!(refusal("k1").error_type, SetErrorType::TooLarge);
        assert_eq!(refusal("k2").to_string(), "overQuota (Over quota)");
        assert!(refusal("k2").blobs_not_found().is_empty());
        let refusal = |id: &str| answer.outcome_of_update(id).refusal().expect("refused");
        assert_eq!(refusal("M1").error_type, SetErrorType::TooManyKeywords);
        assert_eq!(refusal("M2").error_type, SetErrorType::TooManyMailboxes);
        assert_eq!(refusal("M3").error_type, SetErrorType::NotFound);
        assert_eq!(refusal("M4").error_type, SetErrorType::InvalidPatch);
        assert_eq!(refusal("M5").error_type.as_str(), "mailboxLocked");
        assert_eq!(refusal("M5").extra["retryAfter"], 30);
    }

    #[test]
    fn a_new_email_writes_every_settable_property() {
        let email = NewEmail::new()
            .mailbox_ids(["m1"])
            .keywords(["$draft"])
            .received_at("2026-11-23T10:00:00Z")
            .message_id(["id1@example.com"])
            .in_reply_to(["id0@example.com"])
            .references(["id0@example.com"])
            .sender([EmailAddress::new("s@example.com")])
            .from([EmailAddress::new("f@example.com")])
            .to([EmailAddress::new("t@example.com")])
            .cc([EmailAddress::new("c@example.com")])
            .bcc([EmailAddress::new("b@example.com")])
            .reply_to([EmailAddress::new("r@example.com")])
            .subject("S")
            .sent_at("2026-11-23T11:00:00+01:00")
            .header(HeaderProperty::text("X-Priority"), "1")
            .body_structure(NewBodyPart::multipart(
                "multipart/mixed",
                [
                    NewBodyPart::multipart(
                        "multipart/related",
                        [
                            NewBodyPart::html("h"),
                            NewBodyPart::blob("B1", "image/png").cid("logo@example.com"),
                        ],
                    ),
                    NewBodyPart::blob("B2", "text/csv")
                        .name("a.csv")
                        .charset("utf-8")
                        .language(["en"])
                        .location("https://example.com/a.csv")
                        .header(HeaderProperty::text("X-Part"), "p"),
                ],
            ))
            .body_value("h", "<p>Hi</p>")
            .property("x", 1);
        assert_eq!(
            serde_json::to_value(&email).expect("encodes"),
            json!({
                "mailboxIds": {"m1": true},
                "keywords": {"$draft": true},
                "receivedAt": "2026-11-23T10:00:00Z",
                "messageId": ["id1@example.com"],
                "inReplyTo": ["id0@example.com"],
                "references": ["id0@example.com"],
                "sender": [{"name": null, "email": "s@example.com"}],
                "from": [{"name": null, "email": "f@example.com"}],
                "to": [{"name": null, "email": "t@example.com"}],
                "cc": [{"name": null, "email": "c@example.com"}],
                "bcc": [{"name": null, "email": "b@example.com"}],
                "replyTo": [{"name": null, "email": "r@example.com"}],
                "subject": "S",
                "sentAt": "2026-11-23T11:00:00+01:00",
                "header:X-Priority:asText": "1",
                "bodyStructure": {"type": "multipart/mixed", "subParts": [
                    {"type": "multipart/related", "subParts": [
                        {"partId": "h", "type": "text/html"},
                        {"blobId": "B1", "type": "image/png", "cid": "logo@example.com",
                         "disposition": "inline"}
                    ]},
                    {"blobId": "B2", "type": "text/csv", "disposition": "attachment",
                     "name": "a.csv", "charset": "utf-8", "language": ["en"],
                     "location": "https://example.com/a.csv", "header:X-Part:asText": "p"}
                ]},
                "bodyValues": {"h": {"value": "<p>Hi</p>"}},
                "x": 1
            })
        );
        // Two body values, and the flat lists.
        let email = NewEmail::new()
            .text_body(NewBodyPart::text("t"))
            .html_body(NewBodyPart::html("h"))
            .body_value("t", "Hi")
            .body_value("h", "<p>Hi</p>");
        assert_eq!(
            serde_json::to_value(&email).expect("encodes"),
            json!({
                "textBody": [{"partId": "t", "type": "text/plain"}],
                "htmlBody": [{"partId": "h", "type": "text/html"}],
                "bodyValues": {"t": {"value": "Hi"}, "h": {"value": "<p>Hi</p>"}}
            })
        );
    }

    #[test]
    fn an_import_is_written_and_its_answer_read() {
        let mut import = EmailImport::new("u1");
        import.if_in_state("s1");
        import.email_as("k1", EmailToImport::new("B1", ["m1", "m2"]));
        let first = import.email(EmailToImport::new("B0", ["m1"]).keywords(["$seen"]));
        let second = import.email(EmailToImport::new("B2", ["m1"]).property("x", 1));
        assert_eq!((first.as_str(), second.as_str()), ("k0", "k2"));
        let mut request = Request::new();
        let call = request.call(import).expect("encodes");
        assert_eq!(call.name(), "Email/import");
        assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "accountId": "u1",
                "ifInState": "s1",
                "emails": {
                    "k0": {"blobId": "B0", "mailboxIds": {"m1": true},
                           "keywords": {"$seen": true}},
                    "k1": {"blobId": "B1", "mailboxIds": {"m1": true, "m2": true}},
                    "k2": {"blobId": "B2", "mailboxIds": {"m1": true}, "x": 1}
                }
            })
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Email/import", {
                "accountId": "u1", "oldState": "s1", "newState": "s2",
                "created": {"k0": {"id": "M1", "blobId": "B0", "threadId": "T1", "size": 300}},
                "notCreated": {
                    "k1": {"type": "alreadyExists", "existingId": "M0"},
                    "k2": {"type": "invalidProperties", "properties": ["mailboxIds"]}
                }
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        assert_eq!(answer.new_state.as_deref(), Some("s2"));
        let email = answer.outcome_of("k0").done().expect("imported");
        assert_eq!(email.id.as_deref(), Some("M1"));
        assert_eq!(email.thread_id.as_deref(), Some("T1"));
        assert_eq!(
            answer
                .outcome_of("k1")
                .refusal()
                .and_then(SetError::existing_id),
            Some("M0")
        );
        assert!(
            answer
                .outcome_of("k2")
                .refusal()
                .is_some_and(|error| error.names_property("mailboxIds"))
        );
        assert_eq!(answer.outcome_of("k9"), SetOutcome::Unanswered);

        // Nulls where the RFC allows them, and nothing at all.
        let answer: EmailImportResponse = serde_json::from_value(json!({
            "accountId": "u1", "oldState": null, "newState": "s", "created": null,
            "notCreated": null
        }))
        .expect("decodes");
        assert!(answer.created.is_empty() && answer.not_created.is_empty());
        let empty: EmailImportResponse = serde_json::from_value(json!({})).expect("decodes");
        assert_eq!(empty, EmailImportResponse::default());
    }

    /// RFC 8621 section 4.7: only the mailboxes, keywords and received
    /// date of a copy may be set.
    #[test]
    fn an_email_copy_is_written_and_its_answer_read() {
        let mut copy = CopyMethod::<Email>::new("from", "to");
        let creation_id = copy.create(
            EmailCopy::new("M1")
                .mailbox_ids(["m9"])
                .keywords(["$seen"])
                .received_at("2026-11-23T10:00:00Z"),
        );
        let mut request = Request::new();
        let call = request.call(copy).expect("encodes");
        assert_eq!(call.name(), "Email/copy");
        assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "fromAccountId": "from",
                "accountId": "to",
                "create": {"k0": {"id": "M1", "mailboxIds": {"m9": true},
                    "keywords": {"$seen": true}, "receivedAt": "2026-11-23T10:00:00Z"}}
            })
        );
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Email/copy", {
                "fromAccountId": "from", "accountId": "to", "newState": "s2",
                "created": {"k0": {"id": "M7", "blobId": "B7", "threadId": "T7", "size": 9}}
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        let copied = answer.outcome_of(&creation_id).done().expect("copied");
        assert_eq!(copied.id.as_deref(), Some("M7"));
    }
}
