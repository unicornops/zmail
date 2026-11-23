//! Emails (RFC 8621 section 4): reading them (`Email/get`,
//! `Email/query`, `Email/changes`, `Email/queryChanges`) and
//! `Email/set`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Date, Thread, UtcDate,
    header::{self, HeaderProperty, ParsedForm},
};
use crate::{
    capability,
    changes::ChangesObject,
    method::{Get, GetObject, NoArguments, Object, Properties, SetObject, null_as_default},
    query::{Query, QueryChanges, QueryObject},
    record::record,
    request::{Call, IdsReference},
};

/// The Email data type (RFC 8621 section 4), for
/// [`Get`]`<Email>`, [`Query`]`<Email>`,
/// [`Changes`](crate::Changes)`<Email>`, [`QueryChanges`]`<Email>` and
/// [`Set`](crate::Set)`<Email>`.
///
/// # Reading
///
/// `Email/get` returns [`EmailRecord`]s with the properties asked for
/// ([`EmailProperty`]); without a list, a default set that has the
/// metadata, the usual header fields and the body part lists but
/// neither `bodyStructure` nor `headers` (section 4.2). Body text comes
/// in `bodyValues`, for the parts the `fetch_..._body_values` arguments
/// name:
///
/// ```
/// use jmap_wire::{Get, Request, mail::{Email, EmailProperty, HeaderProperty}};
/// use serde_json::json;
///
/// // The request of RFC 8621 section 4.2.1.
/// let get = Get::<Email>::new("abc")
///     .ids(["f123u456", "f123u457"])
///     .properties([
///         EmailProperty::ThreadId,
///         EmailProperty::MailboxIds,
///         EmailProperty::From,
///         EmailProperty::Subject,
///         EmailProperty::ReceivedAt,
///         HeaderProperty::urls("List-POST").into(),
///         EmailProperty::HtmlBody,
///         EmailProperty::BodyValues,
///     ])
///     .body_properties(["partId", "blobId", "size", "type"])
///     .fetch_html_body_values(true)
///     .max_body_value_bytes(256);
/// assert_eq!(
///     serde_json::to_value(&get)?,
///     json!({
///         "accountId": "abc",
///         "ids": ["f123u456", "f123u457"],
///         "properties": ["threadId", "mailboxIds", "from", "subject", "receivedAt",
///                        "header:List-POST:asURLs", "htmlBody", "bodyValues"],
///         "bodyProperties": ["partId", "blobId", "size", "type"],
///         "fetchHTMLBodyValues": true,
///         "maxBodyValueBytes": 256
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// # Changing
///
/// `Email/set` creates, updates (a [`PatchObject`](crate::PatchObject)
/// on `keywords` and `mailboxIds`, mostly) and destroys emails:
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

impl GetObject for Email {
    type Record = EmailRecord;
}

impl ChangesObject for Email {}

impl QueryObject for Email {
    type Condition = EmailFilterCondition;
    type Arguments = EmailQueryArguments;
}

/// The arguments `Email/get` adds to a `/get` (RFC 8621 section 4.2).
impl Get<Email> {
    /// The properties to return of each body part (`bodyProperties`),
    /// in `bodyStructure`, `textBody`, `htmlBody` and `attachments`.
    /// Without it: `partId`, `blobId`, `size`, `name`, `type`,
    /// `charset`, `disposition`, `cid`, `language` and `location`. A
    /// [`HeaderProperty`] is a body part property too.
    pub fn body_properties(self, properties: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let properties = properties.into_iter().map(Into::into).collect::<Vec<_>>();
        self.argument("bodyProperties", properties)
    }

    /// Whether `bodyValues` has the text of the `text/*` parts in
    /// `textBody` (`fetchTextBodyValues`); false unless set.
    pub fn fetch_text_body_values(self, fetch: bool) -> Self {
        self.argument("fetchTextBodyValues", fetch)
    }

    /// Whether `bodyValues` has the text of the `text/*` parts in
    /// `htmlBody` (`fetchHTMLBodyValues`); false unless set.
    pub fn fetch_html_body_values(self, fetch: bool) -> Self {
        self.argument("fetchHTMLBodyValues", fetch)
    }

    /// Whether `bodyValues` has the text of every `text/*` part in
    /// `bodyStructure` (`fetchAllBodyValues`); false unless set.
    pub fn fetch_all_body_values(self, fetch: bool) -> Self {
        self.argument("fetchAllBodyValues", fetch)
    }

    /// Cuts each body value off at this many octets
    /// (`maxBodyValueBytes`); a value that was cut says so
    /// ([`EmailBodyValue::is_truncated`]). 0, the default, is no limit:
    /// an email's whole text then comes in one answer, however large.
    pub fn max_body_value_bytes(self, bytes: u64) -> Self {
        self.argument("maxBodyValueBytes", bytes)
    }
}

impl Call<Get<Email>> {
    /// A reference to the thread ids of the emails this call returns
    /// (`/list/*/threadId`), for the `ids_from` of a `Thread/get` later
    /// in the request. The call must ask for `threadId`.
    pub fn thread_ids(&self) -> IdsReference<Thread> {
        self.ids_in("threadId")
    }
}

record! {
    /// An email as `Email/get` returns it (RFC 8621 section 4.1).
    ///
    /// Every property is optional, because a call returns only those it
    /// asked for and servers leave out or `null` what a message does not
    /// have. `None` therefore means "not returned" or "none": which of
    /// the two follows from the properties requested.
    ///
    /// Header fields fetched as `header:...` properties are read with
    /// [`header`](Self::header) and [`header_all`](Self::header_all).
    pub struct EmailRecord {
        /// The email's id.
        "id" => id: String,
        /// The id of the whole message as a blob, for downloading it.
        "blobId" => blob_id: String,
        /// The id of the thread it is in.
        "threadId" => thread_id: String,
        /// The mailboxes it is in: ids, each mapped to `true`. See
        /// [`mailbox_list`](Self::mailbox_list).
        "mailboxIds" => mailbox_ids: BTreeMap<String, bool>,
        /// Its keywords, each mapped to `true`: `$seen`, `$flagged`,
        /// `$draft`, `$answered` and any others, in lower case. See
        /// [`has_keyword`](Self::has_keyword).
        "keywords" => keywords: BTreeMap<String, bool>,
        /// The size of the whole message in octets.
        "size" => size: u64,
        /// When the server received it.
        "receivedAt" => received_at: UtcDate,
        /// The `Message-ID` field: one id for a well-formed message,
        /// without angle brackets.
        "messageId" => message_id: Vec<String>,
        /// The `In-Reply-To` field's ids.
        "inReplyTo" => in_reply_to: Vec<String>,
        /// The `References` field's ids.
        "references" => references: Vec<String>,
        /// The `Sender` field.
        "sender" => sender: Vec<EmailAddress>,
        /// The `From` field.
        "from" => from: Vec<EmailAddress>,
        /// The `To` field.
        "to" => to: Vec<EmailAddress>,
        /// The `Cc` field.
        "cc" => cc: Vec<EmailAddress>,
        /// The `Bcc` field.
        "bcc" => bcc: Vec<EmailAddress>,
        /// The `Reply-To` field.
        "replyTo" => reply_to: Vec<EmailAddress>,
        /// The `Subject` field, decoded.
        "subject" => subject: String,
        /// The `Date` field.
        "sentAt" => sent_at: Date,
        /// Every header field, in the message's order, values raw.
        "headers" => headers: Vec<EmailHeader>,
        /// The whole MIME structure.
        "bodyStructure" => body_structure: Box<EmailBodyPart>,
        /// The text of body parts, by part id: see the
        /// `fetch_..._body_values` arguments of [`Get<Email>`](Email)
        /// for which parts, and [`body_value`](Self::body_value).
        "bodyValues" => body_values: BTreeMap<String, EmailBodyValue>,
        /// The parts to show as the body when plain text is preferred.
        /// A message with only HTML has its `text/html` part here: a
        /// part's `type`, not the list it is in, says what it is.
        "textBody" => text_body: Vec<EmailBodyPart>,
        /// The parts to show as the body when HTML is preferred.
        "htmlBody" => html_body: Vec<EmailBodyPart>,
        /// The parts that are in neither body list, and the images,
        /// audio and video that are not in both: attachments and the
        /// inline parts HTML refers to by `cid:`.
        "attachments" => attachments: Vec<EmailBodyPart>,
        /// Whether the email has parts a client should offer as
        /// attachments, by the server's reckoning.
        "hasAttachment" => has_attachment: bool,
        /// A short plain text extract of the body, at most 256
        /// characters.
        "preview" => preview: String,
    }
}

impl EmailRecord {
    /// Whether the email has `keyword`. Keywords are case-insensitive
    /// (RFC 8621 section 4.1.1) and servers must return them in lower
    /// case; one in another case is matched all the same.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords
            .iter()
            .flatten()
            .any(|(name, set)| *set && name.eq_ignore_ascii_case(keyword))
    }

    /// The keywords the email has: those mapped to `true`, which per
    /// the RFC is all of them.
    pub fn keyword_list(&self) -> Vec<&str> {
        set_members(self.keywords.as_ref())
    }

    /// The ids of the mailboxes the email is in: those mapped to
    /// `true`.
    pub fn mailbox_list(&self) -> Vec<&str> {
        set_members(self.mailbox_ids.as_ref())
    }

    /// The fetched text of the body part `part_id`.
    pub fn body_value(&self, part_id: &str) -> Option<&EmailBodyValue> {
        self.body_values.as_ref()?.get(part_id)
    }

    /// The last instance of the header field `name`, in the form `F`
    /// it was fetched in (`header:{name}:as{F}`, or the last of
    /// `header:{name}:as{F}:all`). `None` when the property was not
    /// fetched in that form, the message has no such field, the server
    /// could not parse it, or the value is not of the form's type; the
    /// JSON itself is [`header_json`](Self::header_json).
    ///
    /// ```
    /// use jmap_wire::mail::{AsText, AsUrls, EmailRecord};
    /// use serde_json::json;
    ///
    /// let email: EmailRecord = serde_json::from_value(json!({
    ///     "id": "f123u457",
    ///     "header:List-POST:asURLs": ["mailto:partytime@lists.example.com"]
    /// }))?;
    /// assert_eq!(
    ///     email.header::<AsUrls>("List-Post"),
    ///     Some(vec!["mailto:partytime@lists.example.com".to_owned()])
    /// );
    /// assert_eq!(email.header::<AsText>("List-Post"), None, "not fetched as text");
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    pub fn header<F: ParsedForm>(&self, name: &str) -> Option<F::Value> {
        header::last::<F>(&self.extra, name)
    }

    /// Every instance of the header field `name`, in the form `F` it
    /// was fetched in (`header:{name}:as{F}:all`), in the message's
    /// order; empty when the message has none. Instances the server
    /// could not parse (`null`) are left out. `None` when the property
    /// was not fetched in that form.
    pub fn header_all<F: ParsedForm>(&self, name: &str) -> Option<Vec<F::Value>> {
        header::all::<F>(&self.extra, name)
    }

    /// What the server returned for a header field property, as JSON.
    pub fn header_json(&self, property: &HeaderProperty) -> Option<&Value> {
        header::find(&self.extra, property)
    }
}

/// The members of a set written as a map to `true`.
fn set_members(set: Option<&BTreeMap<String, bool>>) -> Vec<&str> {
    set.into_iter()
        .flatten()
        .filter(|(_, member)| **member)
        .map(|(name, _)| name.as_str())
        .collect()
}

record! {
    /// A part of an email's body (RFC 8621 section 4.1.4), with the
    /// properties `bodyProperties` asked for.
    pub struct EmailBodyPart {
        /// The part's id within the email, the key of its text in
        /// `bodyValues`; `None` for a `multipart/*` part.
        "partId" => part_id: String,
        /// The id of the part's decoded content as a blob, for
        /// downloading it; `None` for a `multipart/*` part.
        "blobId" => blob_id: String,
        /// The size of that content in octets.
        "size" => size: u64,
        /// The part's own header fields, values raw.
        "headers" => headers: Vec<EmailHeader>,
        /// The file name the part gives itself.
        "name" => name: String,
        /// The media type, without parameters, such as `text/html`;
        /// `text/plain` is what a part without a `Content-Type` is.
        "type" => content_type: String,
        /// The `charset` parameter, or `us-ascii` for text without one.
        "charset" => charset: String,
        /// The `Content-Disposition` without parameters: `inline` or
        /// `attachment`, usually.
        "disposition" => disposition: String,
        /// The `Content-Id` without its angle brackets, which HTML
        /// refers to as `cid:`.
        "cid" => cid: String,
        /// The `Content-Language` tags.
        "language" => language: Vec<String>,
        /// The `Content-Location` URI.
        "location" => location: String,
        /// The parts inside a `multipart/*` part.
        "subParts" => sub_parts: Vec<EmailBodyPart>,
    }
}

impl EmailBodyPart {
    /// Whether the part is a `multipart/*` container.
    pub fn is_multipart(&self) -> bool {
        self.content_type
            .as_deref()
            .and_then(|content_type| content_type.get(..10))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("multipart/"))
    }

    /// The last instance of the part's header field `name` in the form
    /// `F`, when `bodyProperties` asked for it; as
    /// [`EmailRecord::header`].
    pub fn header<F: ParsedForm>(&self, name: &str) -> Option<F::Value> {
        header::last::<F>(&self.extra, name)
    }

    /// Every instance of the part's header field `name` in the form
    /// `F`; as [`EmailRecord::header_all`].
    pub fn header_all<F: ParsedForm>(&self, name: &str) -> Option<Vec<F::Value>> {
        header::all::<F>(&self.extra, name)
    }
}

record! {
    /// The text of a body part (RFC 8621 section 4.1.4), decoded to
    /// Unicode by the server, with line ends as LF.
    pub struct EmailBodyValue {
        /// The text. Cut short when [`is_truncated`](Self::is_truncated)
        /// says so.
        "value" => value: String,
        /// See [`is_encoding_problem`](Self::is_encoding_problem).
        "isEncodingProblem" => encoding_problem: bool,
        /// See [`is_truncated`](Self::is_truncated).
        "isTruncated" => truncated: bool,
    }
}

impl EmailBodyValue {
    /// Whether the server met a charset or transfer encoding it could
    /// not decode properly: the text may have replacement characters.
    /// False when the server does not say (the RFC's default).
    pub fn is_encoding_problem(&self) -> bool {
        self.encoding_problem.unwrap_or(false)
    }

    /// Whether the text was cut off at `maxBodyValueBytes`. False when
    /// the server does not say (the RFC's default).
    pub fn is_truncated(&self) -> bool {
        self.truncated.unwrap_or(false)
    }
}

/// One header field as it is in the message (RFC 8621 section 4.1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailHeader {
    /// The field's name, in the message's capitalisation.
    pub name: String,
    /// The field's value in raw form, usually with a leading space.
    pub value: String,
}

/// A group of addresses in a header field (RFC 8621 section 4.1.2.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddressGroup {
    /// The group's name; `None` for addresses outside any group.
    #[serde(default)]
    pub name: Option<String>,
    /// The addresses in it.
    #[serde(default, deserialize_with = "null_as_default")]
    pub addresses: Vec<EmailAddress>,
}

/// The name of an Email property, for the `properties` of an
/// `Email/get` (RFC 8621 sections 4.1 and 4.2).
///
/// ```
/// use jmap_wire::mail::{EmailProperty, HeaderProperty};
///
/// assert_eq!(EmailProperty::ReceivedAt.to_string(), "receivedAt");
/// assert_eq!(
///     EmailProperty::from(HeaderProperty::text("X-Mailer")).to_string(),
///     "header:X-Mailer:asText"
/// );
/// assert_eq!(EmailProperty::from("htmlBody"), EmailProperty::HtmlBody);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmailProperty {
    /// `id`. Always returned.
    Id,
    /// `blobId`.
    BlobId,
    /// `threadId`.
    ThreadId,
    /// `mailboxIds`.
    MailboxIds,
    /// `keywords`.
    Keywords,
    /// `size`.
    Size,
    /// `receivedAt`.
    ReceivedAt,
    /// `messageId`.
    MessageId,
    /// `inReplyTo`.
    InReplyTo,
    /// `references`.
    References,
    /// `sender`.
    Sender,
    /// `from`.
    From,
    /// `to`.
    To,
    /// `cc`.
    Cc,
    /// `bcc`.
    Bcc,
    /// `replyTo`.
    ReplyTo,
    /// `subject`.
    Subject,
    /// `sentAt`.
    SentAt,
    /// `hasAttachment`.
    HasAttachment,
    /// `preview`.
    Preview,
    /// `bodyValues`.
    BodyValues,
    /// `textBody`.
    TextBody,
    /// `htmlBody`.
    HtmlBody,
    /// `attachments`.
    Attachments,
    /// `bodyStructure`. Not among the default properties.
    BodyStructure,
    /// `headers`: every header field, raw. Not among the default
    /// properties.
    Headers,
    /// One header field in a form of the caller's choosing.
    Header(HeaderProperty),
    /// Any other property, by name.
    Other(String),
}

impl EmailProperty {
    const NAMED: [(Self, &'static str); 26] = [
        (Self::Id, "id"),
        (Self::BlobId, "blobId"),
        (Self::ThreadId, "threadId"),
        (Self::MailboxIds, "mailboxIds"),
        (Self::Keywords, "keywords"),
        (Self::Size, "size"),
        (Self::ReceivedAt, "receivedAt"),
        (Self::MessageId, "messageId"),
        (Self::InReplyTo, "inReplyTo"),
        (Self::References, "references"),
        (Self::Sender, "sender"),
        (Self::From, "from"),
        (Self::To, "to"),
        (Self::Cc, "cc"),
        (Self::Bcc, "bcc"),
        (Self::ReplyTo, "replyTo"),
        (Self::Subject, "subject"),
        (Self::SentAt, "sentAt"),
        (Self::HasAttachment, "hasAttachment"),
        (Self::Preview, "preview"),
        (Self::BodyValues, "bodyValues"),
        (Self::TextBody, "textBody"),
        (Self::HtmlBody, "htmlBody"),
        (Self::Attachments, "attachments"),
        (Self::BodyStructure, "bodyStructure"),
        (Self::Headers, "headers"),
    ];
}

impl std::fmt::Display for EmailProperty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(property) => property.fmt(f),
            Self::Other(name) => f.write_str(name),
            named => f.write_str(
                Self::NAMED
                    .iter()
                    .find(|(property, _)| property == named)
                    .map_or("", |(_, name)| name),
            ),
        }
    }
}

impl From<&str> for EmailProperty {
    fn from(name: &str) -> Self {
        if let Some((property, _)) = Self::NAMED.iter().find(|(_, named)| *named == name) {
            return property.clone();
        }
        match name.parse() {
            Ok(header) => Self::Header(header),
            Err(_) => Self::Other(name.to_owned()),
        }
    }
}

impl From<HeaderProperty> for EmailProperty {
    fn from(property: HeaderProperty) -> Self {
        Self::Header(property)
    }
}

impl From<EmailProperty> for String {
    fn from(property: EmailProperty) -> Self {
        property.to_string()
    }
}

/// What an `Email/query` matches emails on (RFC 8621 section 4.4.1):
/// every property set must match. Join conditions with
/// [`Filter`](crate::Filter) for `OR` and `NOT`.
///
/// How text is matched is the server's business; most match words
/// without regard to case, and a quoted phrase as a phrase.
///
/// ```
/// use jmap_wire::mail::{EmailFilterCondition, UtcDate};
/// use serde_json::json;
///
/// let condition = EmailFilterCondition::new()
///     .in_mailbox("mb-inbox")
///     .after(UtcDate::new("2026-06-01T00:00:00Z"))
///     .not_keyword("$seen")
///     .header("List-Id", None);
/// assert_eq!(
///     serde_json::to_value(&condition)?,
///     json!({
///         "inMailbox": "mb-inbox",
///         "after": "2026-06-01T00:00:00Z",
///         "notKeyword": "$seen",
///         "header": ["List-Id"]
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EmailFilterCondition(Properties);

macro_rules! text_conditions {
    ($($(#[$meta:meta])* $name:ident => $json:literal,)*) => {
        $(
            $(#[$meta])*
            pub fn $name(mut self, text: impl Into<String>) -> Self {
                self.0.set($json, text.into());
                self
            }
        )*
    };
}

impl EmailFilterCondition {
    /// A condition every email matches.
    pub fn new() -> Self {
        Self::default()
    }

    /// Emails in none but these mailboxes do not match
    /// (`inMailboxOtherThan`): for leaving out what is only in Trash or
    /// Junk.
    pub fn in_mailbox_other_than(
        mut self,
        mailbox_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let ids = mailbox_ids.into_iter().map(Into::into).collect::<Vec<_>>();
        self.0.set("inMailboxOtherThan", ids);
        self
    }

    /// Emails received before this time (`before`).
    pub fn before(mut self, date: impl Into<UtcDate>) -> Self {
        self.0.set("before", date.into().as_str());
        self
    }

    /// Emails received at this time or after (`after`).
    pub fn after(mut self, date: impl Into<UtcDate>) -> Self {
        self.0.set("after", date.into().as_str());
        self
    }

    /// Emails of at least this many octets (`minSize`).
    pub fn min_size(mut self, octets: u64) -> Self {
        self.0.set("minSize", octets);
        self
    }

    /// Emails of fewer than this many octets (`maxSize`).
    pub fn max_size(mut self, octets: u64) -> Self {
        self.0.set("maxSize", octets);
        self
    }

    /// Emails whose `hasAttachment` is this (`hasAttachment`).
    pub fn has_attachment(mut self, has_attachment: bool) -> Self {
        self.0.set("hasAttachment", has_attachment);
        self
    }

    /// Emails with the header field `name` (`header`), and with `text`
    /// in its value when given.
    pub fn header(mut self, name: impl Into<String>, text: Option<&str>) -> Self {
        let mut header = vec![name.into()];
        header.extend(text.map(str::to_owned));
        self.0.set("header", header);
        self
    }

    text_conditions! {
        /// Emails in this mailbox (`inMailbox`).
        in_mailbox => "inMailbox",
        /// Emails in a thread whose emails all have this keyword
        /// (`allInThreadHaveKeyword`).
        all_in_thread_have_keyword => "allInThreadHaveKeyword",
        /// Emails in a thread where some email has this keyword
        /// (`someInThreadHaveKeyword`).
        some_in_thread_have_keyword => "someInThreadHaveKeyword",
        /// Emails in a thread where no email has this keyword
        /// (`noneInThreadHaveKeyword`).
        none_in_thread_have_keyword => "noneInThreadHaveKeyword",
        /// Emails with this keyword (`hasKeyword`).
        has_keyword => "hasKeyword",
        /// Emails without this keyword (`notKeyword`).
        not_keyword => "notKeyword",
        /// Emails with this text in `From`, `To`, `Cc`, `Bcc`,
        /// `Subject` or, on most servers, the body (`text`).
        text => "text",
        /// Emails with this text in `From` (`from`).
        from => "from",
        /// Emails with this text in `To` (`to`).
        to => "to",
        /// Emails with this text in `Cc` (`cc`).
        cc => "cc",
        /// Emails with this text in `Bcc` (`bcc`).
        bcc => "bcc",
        /// Emails with this text in `Subject` (`subject`).
        subject => "subject",
        /// Emails with this text in a body part (`body`).
        body => "body",
    }

    /// Sets any other property of the condition, for an extension.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// The argument `Email/query` and `Email/queryChanges` add (RFC 8621
/// sections 4.4 and 4.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EmailQueryArguments {
    /// Keep only the first email of each thread in the results. False
    /// unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collapse_threads: Option<bool>,
}

impl Query<Email> {
    /// Keeps only the first email of each thread in the results
    /// (`collapseThreads`, RFC 8621 section 4.4.3).
    pub fn collapse_threads(mut self, collapse: bool) -> Self {
        self.arguments().collapse_threads = Some(collapse);
        self
    }
}

impl QueryChanges<Email> {
    /// The `collapseThreads` the query had.
    pub fn collapse_threads(mut self, collapse: bool) -> Self {
        self.arguments().collapse_threads = Some(collapse);
        self
    }
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
    /// The address itself. It need not be a valid one: a server parses
    /// what the message has as best it can (RFC 8621 section 4.1.2.3).
    /// Empty when a server leaves it out or sends `null`.
    #[serde(default, deserialize_with = "null_as_default")]
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        GetResponse,
        mail::{AsRaw, AsText},
    };

    fn record(value: Value) -> EmailRecord {
        serde_json::from_value(value).expect("an object decodes as an email")
    }

    /// The shapes servers give the same thing: left out, `null`, empty.
    #[test]
    fn missing_null_and_empty_read_alike_where_they_mean_alike() {
        let cases = [
            json!({"id": "e1"}),
            json!({"id": "e1", "keywords": null, "mailboxIds": null, "from": null,
                   "subject": null, "bodyValues": null, "attachments": null,
                   "hasAttachment": null, "receivedAt": null, "messageId": null}),
        ];
        for case in cases {
            let email = record(case);
            assert_eq!(email.id.as_deref(), Some("e1"));
            assert!(email.keywords.is_none() && email.mailbox_ids.is_none());
            assert!(!email.has_keyword("$seen"));
            assert!(email.keyword_list().is_empty() && email.mailbox_list().is_empty());
            assert!(email.from.is_none() && email.subject.is_none());
            assert!(email.body_value("1").is_none());
            assert!(email.attachments.is_none() && email.has_attachment.is_none());
            assert!(email.received_at.is_none() && email.message_id.is_none());
            assert!(email.extra.is_empty(), "{:?}", email.extra);
        }
        let empty = record(
            json!({"id": "e1", "keywords": {}, "from": [], "attachments": [],
                                 "bodyValues": {}, "subject": ""}),
        );
        assert_eq!(empty.keywords, Some(BTreeMap::new()));
        assert_eq!(empty.from, Some(Vec::new()), "an empty list is not none");
        assert_eq!(empty.subject.as_deref(), Some(""));
        assert_eq!(empty.attachments, Some(Vec::new()));
    }

    #[test]
    fn keywords_and_mailboxes_are_the_members_mapped_to_true() {
        let email = record(json!({
            "keywords": {"$seen": true, "$Flagged": true, "$draft": false},
            "mailboxIds": {"mb-inbox": true, "mb-gone": false}
        }));
        assert!(email.has_keyword("$seen") && email.has_keyword("$SEEN"));
        assert!(email.has_keyword("$flagged"), "keywords ignore case");
        assert!(!email.has_keyword("$draft") && !email.has_keyword("$answered"));
        assert_eq!(email.keyword_list(), ["$Flagged", "$seen"]);
        assert_eq!(email.mailbox_list(), ["mb-inbox"]);
    }

    /// One property of another type than the RFC's costs that property,
    /// not the email, and the JSON is kept.
    #[test]
    fn a_property_of_the_wrong_type_is_none_and_kept_as_json() {
        let odd = [
            ("size", json!("large")),
            ("size", json!(-4)),
            ("size", json!(12.5)),
            ("keywords", json!(["$seen"])),
            ("keywords", json!({"$seen": 1})),
            ("mailboxIds", json!("mb-inbox")),
            ("receivedAt", json!(1_700_000_000)),
            ("messageId", json!("one@example.com")),
            ("from", json!("Joe <joe@example.com>")),
            (
                "from",
                json!([{"name": "Joe", "email": "joe@example.com"}, "x"]),
            ),
            ("subject", json!(["a", "b"])),
            ("hasAttachment", json!("yes")),
            ("preview", json!(false)),
            ("bodyValues", json!([{"value": "text"}])),
            ("textBody", json!({"partId": "1"})),
            ("attachments", json!([{"partId": "1"}, null])),
            ("bodyStructure", json!("multipart/mixed")),
            ("headers", json!([{"name": "Subject"}])),
            ("sentAt", json!({"date": "today"})),
            ("threadId", json!(7)),
        ];
        for (name, value) in odd {
            let email = record(json!({"id": "e1", "subject": "kept", name: value.clone()}));
            assert_eq!(email.id.as_deref(), Some("e1"), "{name}");
            assert_eq!(
                email.extra.get(name),
                Some(&value),
                "{name} is kept as it came"
            );
            if name != "subject" {
                assert_eq!(email.subject.as_deref(), Some("kept"), "{name}");
            }
            let again = serde_json::to_value(&email).expect("encodes");
            assert_eq!(again[name], value, "{name} is written back");
        }
    }

    #[test]
    fn body_parts_and_values_are_read_part_by_part() {
        let email = record(json!({
            "bodyStructure": {
                "type": "multipart/mixed", "partId": null, "blobId": null, "size": 0,
                "header:Content-Type": " multipart/mixed; boundary=\"b\"",
                "subParts": [
                    {"partId": "1", "blobId": "B1", "size": 12, "type": "text/plain",
                     "charset": "utf-8", "disposition": null, "cid": null, "language": null,
                     "location": null, "name": null,
                     "headers": [{"name": "Content-Type", "value": " text/plain"}]},
                    {"partId": "2", "blobId": "B2", "size": "big", "type": "image/png",
                     "name": "logo.png", "disposition": "inline", "cid": "logo@example.com",
                     "language": ["en", "fr"], "location": "https://example.com/logo.png",
                     "futurePartProperty": 1}
                ]
            },
            "bodyValues": {
                "1": {"value": "Hello", "isEncodingProblem": false, "isTruncated": true},
                "2": {"value": "bare"},
                "3": {"isTruncated": "no"},
                "4": {"value": null, "isEncodingProblem": true}
            }
        }));
        let structure = email.body_structure.as_deref().expect("a structure");
        assert!(structure.is_multipart());
        assert_eq!(
            (structure.part_id.as_ref(), structure.blob_id.as_ref()),
            (None, None)
        );
        assert_eq!(
            structure.header::<AsRaw>("content-type").as_deref(),
            Some(" multipart/mixed; boundary=\"b\"")
        );
        let parts = structure.sub_parts.as_ref().expect("sub-parts");
        assert_eq!(parts[0].content_type.as_deref(), Some("text/plain"));
        assert_eq!(parts[0].size, Some(12));
        assert_eq!(
            parts[0].headers.as_ref().expect("headers")[0].name,
            "Content-Type"
        );
        assert!(!parts[0].is_multipart() && parts[0].sub_parts.is_none());
        assert_eq!(parts[1].size, None);
        assert_eq!(parts[1].extra["size"], "big");
        assert_eq!(parts[1].extra["futurePartProperty"], 1);
        assert_eq!(parts[1].cid.as_deref(), Some("logo@example.com"));
        assert_eq!(parts[1].language.as_ref().expect("languages").len(), 2);

        let first = email.body_value("1").expect("fetched");
        assert_eq!(first.value.as_deref(), Some("Hello"));
        assert!(first.is_truncated() && !first.is_encoding_problem());
        let bare = email.body_value("2").expect("fetched");
        assert!(
            !bare.is_truncated() && !bare.is_encoding_problem(),
            "the RFC's defaults"
        );
        let valueless = email.body_value("3").expect("fetched");
        assert_eq!(valueless.value, None);
        assert_eq!(valueless.extra["isTruncated"], "no");
        assert!(
            email
                .body_value("4")
                .expect("fetched")
                .is_encoding_problem()
        );
        assert!(email.body_value("5").is_none());
        assert!(!EmailBodyPart::default().is_multipart());
        assert!(
            !record(json!({"bodyStructure": {"type": "multi"}}))
                .body_structure
                .expect("a part")
                .is_multipart()
        );
    }

    #[test]
    fn addresses_are_read_as_best_they_can_be() {
        let email = record(json!({
            "from": [{"name": "Joe Bloggs", "email": "joe@example.com"}],
            "to": [{"email": "no-name@example.com"}, {"name": null, "email": "not an address"}],
            "cc": [{"name": "No Address"}, {"name": "Null Address", "email": null}],
            "header:Resent-From:asGroupedAddresses": [{"name": "Team", "addresses": null}]
        }));
        assert_eq!(
            email.from.expect("from"),
            [EmailAddress::named("Joe Bloggs", "joe@example.com")]
        );
        let to = email.to.expect("to");
        assert_eq!(to[0], EmailAddress::new("no-name@example.com"));
        assert_eq!(to[1].email, "not an address");
        let cc = email.cc.expect("cc");
        assert_eq!((cc[0].email.as_str(), cc[1].email.as_str()), ("", ""));
        let groups = header::last::<crate::mail::AsGroupedAddresses>(&email.extra, "Resent-From")
            .expect("fetched");
        assert!(groups[0].addresses.is_empty());
    }

    #[test]
    fn header_properties_stay_with_the_email_and_are_read_by_form() {
        let email = record(json!({
            "id": "e1",
            "subject": "Convenience",
            "header:Subject:asText": "By property",
            "header:Delivered-To:all": [" me@example.com", " <alias@example.com>"],
            "header:X-Original-To:all": []
        }));
        assert_eq!(
            email.header::<AsText>("subject").as_deref(),
            Some("By property")
        );
        assert_eq!(
            email.header_all::<AsRaw>("Delivered-To").expect("fetched"),
            [" me@example.com", " <alias@example.com>"]
        );
        assert_eq!(email.header_all::<AsRaw>("X-Original-To"), Some(Vec::new()));
        assert_eq!(email.header_all::<AsRaw>("X-Not-Fetched"), None);
        assert_eq!(
            email.header_json(&HeaderProperty::raw("x-original-to").all()),
            Some(&json!([]))
        );
        assert_eq!(email.extra.len(), 3);
    }

    #[test]
    fn property_names_are_written_as_the_rfc_has_them() {
        for (property, name) in &EmailProperty::NAMED {
            assert_eq!(&property.to_string(), name);
            assert_eq!(&EmailProperty::from(*name), property);
        }
        assert_eq!(
            EmailProperty::from("header:Delivered-To:all"),
            EmailProperty::Header(HeaderProperty::raw("Delivered-To").all())
        );
        assert_eq!(
            EmailProperty::from("futureProperty"),
            EmailProperty::Other("futureProperty".to_owned())
        );
        assert_eq!(EmailProperty::Other("x".to_owned()).to_string(), "x");
        assert_eq!(String::from(EmailProperty::HtmlBody), "htmlBody");
    }

    /// Every condition of RFC 8621 section 4.4.1.
    #[test]
    fn every_filter_condition_is_written_under_its_name() {
        let condition = EmailFilterCondition::new()
            .in_mailbox("mb1")
            .in_mailbox_other_than(["mb-trash", "mb-junk"])
            .before("2026-06-02T00:00:00Z")
            .after(UtcDate::new("2026-06-01T00:00:00Z"))
            .min_size(1024)
            .max_size(4096)
            .all_in_thread_have_keyword("$seen")
            .some_in_thread_have_keyword("$flagged")
            .none_in_thread_have_keyword("$draft")
            .has_keyword("$answered")
            .not_keyword("$forwarded")
            .has_attachment(true)
            .text("budget")
            .from("alice")
            .to("bob")
            .cc("carol")
            .bcc("dave")
            .subject("report")
            .body("figures")
            .header("X-Priority", Some("1"))
            .property("https://example.com/starred", true);
        assert_eq!(
            serde_json::to_value(&condition).expect("encodes"),
            json!({
                "inMailbox": "mb1",
                "inMailboxOtherThan": ["mb-trash", "mb-junk"],
                "before": "2026-06-02T00:00:00Z",
                "after": "2026-06-01T00:00:00Z",
                "minSize": 1024,
                "maxSize": 4096,
                "allInThreadHaveKeyword": "$seen",
                "someInThreadHaveKeyword": "$flagged",
                "noneInThreadHaveKeyword": "$draft",
                "hasKeyword": "$answered",
                "notKeyword": "$forwarded",
                "hasAttachment": true,
                "text": "budget",
                "from": "alice",
                "to": "bob",
                "cc": "carol",
                "bcc": "dave",
                "subject": "report",
                "body": "figures",
                "header": ["X-Priority", "1"],
                "https://example.com/starred": true
            })
        );
        assert_eq!(
            serde_json::to_value(EmailFilterCondition::new()).expect("encodes"),
            json!({}),
            "no properties: every email matches"
        );
    }

    #[test]
    fn the_body_value_arguments_are_written_only_when_set() {
        let plain = Get::<Email>::new("a1").ids(["e1"]);
        assert_eq!(
            serde_json::to_value(&plain).expect("encodes"),
            json!({"accountId": "a1", "ids": ["e1"]})
        );
        let all = Get::<Email>::new("a1")
            .ids(["e1"])
            .fetch_text_body_values(false)
            .fetch_all_body_values(true)
            .body_properties([
                "partId".to_owned(),
                HeaderProperty::raw("Content-Type").into(),
            ]);
        assert_eq!(
            serde_json::to_value(&all).expect("encodes"),
            json!({
                "accountId": "a1",
                "ids": ["e1"],
                "bodyProperties": ["partId", "header:Content-Type"],
                "fetchAllBodyValues": true,
                "fetchTextBodyValues": false
            })
        );
    }

    /// Any JSON at all where a record belongs: an object always decodes,
    /// nothing panics, and what was decoded survives being written and
    /// read again. The values come from a small generator with a fixed
    /// seed, biased towards the property names of the mail types with
    /// values of every JSON type under them.
    #[test]
    fn no_json_makes_a_record_panic_or_an_object_fail() {
        const NAMES: [&str; 34] = [
            "id",
            "blobId",
            "threadId",
            "mailboxIds",
            "keywords",
            "size",
            "receivedAt",
            "messageId",
            "inReplyTo",
            "references",
            "sender",
            "from",
            "to",
            "cc",
            "bcc",
            "replyTo",
            "subject",
            "sentAt",
            "headers",
            "bodyStructure",
            "bodyValues",
            "textBody",
            "htmlBody",
            "attachments",
            "hasAttachment",
            "preview",
            "partId",
            "type",
            "subParts",
            "value",
            "isTruncated",
            "name",
            "email",
            "header:X-Any:asAddresses:all",
        ];
        /// xorshift64: the same values on every run.
        struct Random(u64);
        impl Random {
            fn next(&mut self, below: u64) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 % below
            }
            fn value(&mut self, depth: u32) -> Value {
                let kinds = if depth == 0 { 6 } else { 8 };
                match self.next(kinds) {
                    0 => Value::Null,
                    1 => json!(self.next(2) == 0),
                    2 => json!(self.next(1 << 40) as i64 - (1 << 20)),
                    3 => json!(self.next(1000) as f64 / 7.0),
                    4 => json!(NAMES[self.next(NAMES.len() as u64) as usize]),
                    5 => json!("2026-11-03T16:15:05Z"),
                    6 => Value::Array((0..self.next(4)).map(|_| self.value(depth - 1)).collect()),
                    _ => self.object(depth - 1),
                }
            }
            fn object(&mut self, depth: u32) -> Value {
                Value::Object(
                    (0..self.next(7))
                        .map(|_| {
                            let name = NAMES[self.next(NAMES.len() as u64) as usize];
                            (name.to_owned(), self.value(depth))
                        })
                        .collect(),
                )
            }
        }

        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        for round in 0..4000 {
            let object = random.object(4);
            let decoded: EmailRecord = serde_json::from_value(object.clone())
                .unwrap_or_else(|error| panic!("round {round}: {error}: {object}"));
            let written = serde_json::to_value(&decoded).expect("encodes");
            let again: EmailRecord = serde_json::from_value(written).expect("decodes again");
            assert_eq!(again, decoded, "round {round}: {object}");
            // The same object as the other records, and in a list.
            serde_json::from_value::<EmailBodyPart>(object.clone()).expect("a part");
            serde_json::from_value::<EmailBodyValue>(object.clone()).expect("a value");
            serde_json::from_value::<crate::mail::MailboxRecord>(object.clone())
                .expect("a mailbox");
            serde_json::from_value::<crate::mail::ThreadRecord>(object.clone()).expect("a thread");
            let _ = decoded.header_all::<crate::mail::AsAddresses>("X-Any");
            let _ = decoded.header::<AsText>("X-Any");

            // Anything at all as a whole answer: an error or a value,
            // never a panic.
            let anything = random.value(4);
            let _ = serde_json::from_value::<GetResponse<EmailRecord>>(anything.clone());
            let _ = serde_json::from_value::<crate::QueryResponse>(anything.clone());
            let _ = serde_json::from_value::<crate::ChangesResponse>(anything.clone());
            let _ = serde_json::from_value::<crate::Response>(anything);
            let answer = json!({"list": [object], "notFound": null, "state": random.value(0)});
            if let Ok(answer) = serde_json::from_value::<GetResponse<EmailRecord>>(answer) {
                assert_eq!(answer.list.len(), 1);
            }
        }
    }
}
