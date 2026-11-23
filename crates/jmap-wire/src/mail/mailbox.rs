//! Mailboxes (RFC 8621 section 2): the named sets of emails an account
//! is organised in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability,
    changes::ChangesObject,
    error::error_types,
    method::{GetObject, Object, PatchObject, Properties, SetArguments, SetObject},
    query::{Query, QueryChanges, QueryObject},
    record::record,
    request::{Call, ResultReference},
};

/// The Mailbox data type (RFC 8621 section 2), for
/// [`Get`](crate::Get)`<Mailbox>` (all of an account's mailboxes when no
/// ids are given), [`Changes`](crate::Changes)`<Mailbox>`,
/// [`Query`]`<Mailbox>`, [`QueryChanges`]`<Mailbox>` and
/// [`Set`](crate::Set)`<Mailbox>` ([`NewMailbox`], [`MailboxUpdate`],
/// [`MailboxSetArguments`]).
///
/// ```
/// use jmap_wire::{Get, Request, capability, mail::Mailbox};
///
/// let mut request = Request::new();
/// request.call(Get::<Mailbox>::new("u1"))?;
/// assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub enum Mailbox {}

impl Object for Mailbox {
    const NAME: &'static str = "Mailbox";
    const CAPABILITY: &'static str = capability::MAIL;
}

impl GetObject for Mailbox {
    type Record = MailboxRecord;
}

impl ChangesObject for Mailbox {}

impl SetObject for Mailbox {
    type Create = NewMailbox;
    /// The `id` of the new mailbox and the properties the server set,
    /// such as `myRights` and the counts.
    type Created = MailboxRecord;
    type Arguments = MailboxSetArguments;
}

/// The argument `Mailbox/set` has beyond the standard ones (RFC 8621
/// section 2.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxSetArguments {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    on_destroy_remove_emails: bool,
}

impl MailboxSetArguments {
    /// Whether destroying a mailbox that has emails removes them from
    /// it, **destroying those that are in no other mailbox**. Unless
    /// set, such a mailbox is not destroyed (`mailboxHasEmail`).
    pub fn on_destroy_remove_emails(&mut self, remove: bool) -> &mut Self {
        self.on_destroy_remove_emails = remove;
        self
    }
}

impl SetArguments for MailboxSetArguments {}

/// A mailbox to create with `Mailbox/set` (RFC 8621 section 2.5).
///
/// ```
/// use jmap_wire::{Set, mail::{Mailbox, MailboxUpdate, NewMailbox}};
/// use serde_json::json;
///
/// let mut set = Set::<Mailbox>::new("u1");
/// let projects = set.create(NewMailbox::new("Projects"));
/// // A child of a mailbox made in the same call.
/// set.create(NewMailbox::new("2026").parent_id(Some(&format!("#{projects}"))));
/// set.update("mb7", MailboxUpdate::new().name("Old projects"));
/// set.destroy(["mb9"]);
/// set.arguments().on_destroy_remove_emails(true);
/// assert_eq!(
///     serde_json::to_value(&set)?,
///     json!({
///         "accountId": "u1",
///         "create": {
///             "k0": {"name": "Projects"},
///             "k1": {"name": "2026", "parentId": "#k0"}
///         },
///         "update": {"mb7": {"name": "Old projects"}},
///         "destroy": ["mb9"],
///         "onDestroyRemoveEmails": true
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewMailbox(Properties);

impl NewMailbox {
    /// A mailbox called `name` at the top level. A name must be unique
    /// among the mailboxes with the same parent (RFC 8621 section 2).
    pub fn new(name: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("name", name.into());
        Self(properties)
    }

    /// The parent mailbox: its id, `#` and a creation id of the same
    /// call, or `None` for the top level.
    pub fn parent_id(mut self, parent_id: Option<&str>) -> Self {
        self.0.set("parentId", parent_id);
        self
    }

    /// The mailbox's role; at most one mailbox of an account has each.
    pub fn role(mut self, role: Option<Role>) -> Self {
        self.0.set_as("role", &role);
        self
    }

    /// Where it sorts among its siblings: lower first.
    pub fn sort_order(mut self, sort_order: u32) -> Self {
        self.0.set("sortOrder", sort_order);
        self
    }

    /// Whether the user is subscribed to it.
    pub fn is_subscribed(mut self, is_subscribed: bool) -> Self {
        self.0.set("isSubscribed", is_subscribed);
        self
    }

    /// Sets any other property.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// The changes to one mailbox in a `Mailbox/set` update (RFC 8621
/// section 2.5), for [`Set::update`](crate::Set::update). The counts
/// and `myRights` are the server's to set.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use]
pub struct MailboxUpdate(PatchObject);

impl MailboxUpdate {
    /// An update that changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Renames the mailbox.
    pub fn name(self, name: impl Into<String>) -> Self {
        Self(self.0.set("name", name.into()))
    }

    /// Moves it under another mailbox, or to the top level with `None`.
    pub fn parent_id(self, parent_id: Option<&str>) -> Self {
        Self(self.0.set("parentId", parent_id))
    }

    /// Changes its role, or takes it away with `None`.
    pub fn role(self, role: Option<Role>) -> Self {
        Self(
            self.0
                .set("role", serde_json::to_value(role).unwrap_or(Value::Null)),
        )
    }

    /// Changes where it sorts among its siblings.
    pub fn sort_order(self, sort_order: u32) -> Self {
        Self(self.0.set("sortOrder", sort_order))
    }

    /// Subscribes the user to it, or unsubscribes them.
    pub fn is_subscribed(self, is_subscribed: bool) -> Self {
        Self(self.0.set("isSubscribed", is_subscribed))
    }
}

impl From<MailboxUpdate> for PatchObject {
    fn from(update: MailboxUpdate) -> Self {
        update.0
    }
}

impl QueryObject for Mailbox {
    type Condition = MailboxFilterCondition;
    type Arguments = MailboxQueryArguments;
}

record! {
    /// A mailbox as the server returns it (RFC 8621 section 2). Every
    /// property is optional: a `/get` may ask for only some, and a
    /// property that is `null` (a top-level mailbox's `parentId`, the
    /// `role` of a mailbox without one) reads as `None` like one left
    /// out.
    pub struct MailboxRecord {
        /// The mailbox's id.
        "id" => id: String,
        /// The name to show, such as `Inbox`.
        "name" => name: String,
        /// The id of the parent mailbox; `None` at the top level.
        "parentId" => parent_id: String,
        /// The mailbox's purpose, if it has a common one.
        "role" => role: Role,
        /// Where to show it among its siblings: lower first, the same
        /// by name. 0 when the server leaves it out is the RFC's
        /// default.
        "sortOrder" => sort_order: u32,
        /// How many emails are in the mailbox.
        "totalEmails" => total_emails: u64,
        /// How many of them are neither `$seen` nor `$draft`.
        "unreadEmails" => unread_emails: u64,
        /// How many threads have an email in the mailbox.
        "totalThreads" => total_threads: u64,
        /// How many of those threads are unread, by the server's
        /// reckoning.
        "unreadThreads" => unread_threads: u64,
        /// What the user may do with the mailbox.
        "myRights" => my_rights: MailboxRights,
        /// Whether the user wants to see the mailbox in their clients.
        "isSubscribed" => is_subscribed: bool,
    }
}

error_types! {
    /// A mailbox's role (RFC 8621 section 2): an attribute of the IANA
    /// "IMAP Mailbox Name Attributes" registry, in lower case. Roles of
    /// later registrations, and roles a server writes in another case,
    /// are kept as they came (`Other`); [`as_str`](Self::as_str) gives
    /// any role as text.
    Role {
        /// Every email of the account (RFC 6154).
        All => "all",
        /// Archived mail (RFC 6154).
        Archive => "archive",
        /// Drafts (RFC 6154).
        Drafts => "drafts",
        /// Flagged mail (RFC 6154).
        Flagged => "flagged",
        /// Mail the server deems important (RFC 8457).
        Important => "important",
        /// Where new mail arrives (RFC 8621).
        Inbox => "inbox",
        /// Spam (RFC 6154).
        Junk => "junk",
        /// Sent mail (RFC 6154).
        Sent => "sent",
        /// Mailboxes the user is subscribed to (RFC 8621).
        Subscribed => "subscribed",
        /// Deleted mail (RFC 6154).
        Trash => "trash",
    }
}

/// What the user may do with a mailbox (RFC 8621 section 2), after the
/// IMAP ACLs of RFC 4314. A right the server leaves out is not held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct MailboxRights {
    /// Use the mailbox in a query, and fetch the emails in it.
    pub may_read_items: bool,
    /// Add mail to it.
    pub may_add_items: bool,
    /// Remove mail from it.
    pub may_remove_items: bool,
    /// Add or remove `$seen` on its emails.
    pub may_set_seen: bool,
    /// Add or remove any other keyword on its emails.
    pub may_set_keywords: bool,
    /// Create a mailbox inside it.
    pub may_create_child: bool,
    /// Rename it or move it under another mailbox.
    pub may_rename: bool,
    /// Destroy it.
    pub may_delete: bool,
    /// Submit mail directly to it.
    pub may_submit: bool,
    /// Rights this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// What a `Mailbox/query` matches mailboxes on (RFC 8621 section 2.3):
/// every property set must match.
///
/// ```
/// use jmap_wire::mail::{MailboxFilterCondition, Role};
/// use serde_json::json;
///
/// let top_level_folders = MailboxFilterCondition::new().parent(None).role(None);
/// assert_eq!(
///     serde_json::to_value(&top_level_folders)?,
///     json!({"parentId": null, "role": null})
/// );
/// let sent = MailboxFilterCondition::new().role(Some(Role::Sent));
/// assert_eq!(serde_json::to_value(&sent)?, json!({"role": "sent"}));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct MailboxFilterCondition(Properties);

impl MailboxFilterCondition {
    /// A condition every mailbox matches.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mailboxes directly inside the mailbox `parent_id`, or at the top
    /// level for `None`.
    pub fn parent(mut self, parent_id: Option<&str>) -> Self {
        self.0.set_as("parentId", &parent_id);
        self
    }

    /// Mailboxes whose name contains `name`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.0.set("name", name.into());
        self
    }

    /// Mailboxes with exactly this role, or with none for `None`.
    pub fn role(mut self, role: Option<Role>) -> Self {
        self.0.set_as("role", &role);
        self
    }

    /// Mailboxes that have a role (true) or have none (false).
    pub fn has_any_role(mut self, has_any_role: bool) -> Self {
        self.0.set("hasAnyRole", has_any_role);
        self
    }

    /// Mailboxes the user is subscribed to (true) or is not (false).
    pub fn is_subscribed(mut self, is_subscribed: bool) -> Self {
        self.0.set("isSubscribed", is_subscribed);
        self
    }

    /// Sets any other property of the condition, for an extension.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

/// The arguments `Mailbox/query` and `Mailbox/queryChanges` add (RFC
/// 8621 section 2.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MailboxQueryArguments {
    /// Sort as a tree: a mailbox before the mailboxes inside it, and
    /// siblings by the sort. False unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_as_tree: Option<bool>,
    /// Leave out a mailbox when one of its ancestors does not match the
    /// filter. False unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_as_tree: Option<bool>,
}

impl Query<Mailbox> {
    /// Sorts the mailboxes as a tree (`sortAsTree`).
    pub fn sort_as_tree(mut self, sort_as_tree: bool) -> Self {
        self.arguments().sort_as_tree = Some(sort_as_tree);
        self
    }

    /// Leaves out mailboxes whose ancestors do not match
    /// (`filterAsTree`).
    pub fn filter_as_tree(mut self, filter_as_tree: bool) -> Self {
        self.arguments().filter_as_tree = Some(filter_as_tree);
        self
    }
}

impl QueryChanges<Mailbox> {
    /// The `sortAsTree` the query had.
    pub fn sort_as_tree(mut self, sort_as_tree: bool) -> Self {
        self.arguments().sort_as_tree = Some(sort_as_tree);
        self
    }

    /// The `filterAsTree` the query had.
    pub fn filter_as_tree(mut self, filter_as_tree: bool) -> Self {
        self.arguments().filter_as_tree = Some(filter_as_tree);
        self
    }
}

impl Call<crate::Changes<Mailbox>> {
    /// A reference to the answer's `updatedProperties`, for the
    /// [`properties_from`](crate::Get::properties_from) of a
    /// `Mailbox/get` of the updated mailboxes later in the request, so
    /// that only the counts are fetched when only they changed (RFC
    /// 8621 section 2.2).
    pub fn updated_properties(&self) -> ResultReference {
        self.result_reference("/updatedProperties")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Get, GetResponse, Request, Response, Set, SetErrorType};

    /// RFC 8621 section 2.5: the extra argument, and the two SetErrors
    /// of destroying a mailbox.
    #[test]
    fn a_mailbox_set_is_written_and_its_answer_read() {
        let mut set = Set::<Mailbox>::new("u1");
        let created = set.create(
            NewMailbox::new("Receipts")
                .parent_id(None)
                .role(None)
                .sort_order(3)
                .is_subscribed(true),
        );
        set.update(
            "mb1",
            MailboxUpdate::new()
                .name("Archive 2026")
                .parent_id(Some("mb2"))
                .role(Some(Role::Archive))
                .sort_order(1)
                .is_subscribed(false),
        );
        set.destroy(["mb3", "mb4"]);
        let mut request = Request::new();
        let call = request.call(set).expect("encodes");
        assert_eq!(call.name(), "Mailbox/set");
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "accountId": "u1",
                "create": {"k0": {"name": "Receipts", "parentId": null, "role": null,
                                   "sortOrder": 3, "isSubscribed": true}},
                "update": {"mb1": {"name": "Archive 2026", "parentId": "mb2",
                                    "role": "archive", "sortOrder": 1, "isSubscribed": false}},
                "destroy": ["mb3", "mb4"]
            }),
            "onDestroyRemoveEmails is left out unless asked for"
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Mailbox/set", {
                "accountId": "u1", "oldState": "1", "newState": "2",
                "created": {"k0": {"id": "mb9", "totalEmails": 0,
                    "myRights": {"mayReadItems": true}}},
                "updated": {"mb1": null},
                "notDestroyed": {
                    "mb3": {"type": "mailboxHasChild"},
                    "mb4": {"type": "mailboxHasEmail"}
                }
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        let mailbox = answer.outcome_of_create(&created).done().expect("created");
        assert_eq!(mailbox.id.as_deref(), Some("mb9"));
        assert!(
            mailbox
                .my_rights
                .as_ref()
                .is_some_and(|rights| rights.may_read_items)
        );
        assert!(answer.outcome_of_update("mb1").is_done());
        let refused = |id: &str| {
            answer
                .outcome_of_destroy(id)
                .refusal()
                .map(|error| error.error_type.clone())
        };
        assert_eq!(refused("mb3"), Some(SetErrorType::MailboxHasChild));
        assert_eq!(refused("mb4"), Some(SetErrorType::MailboxHasEmail));
    }

    /// The `Mailbox/get` call and answer of RFC 8621 section 2.6 (the
    /// RFC elides the second mailbox with `...`).
    #[test]
    fn the_mailbox_get_of_rfc_8621_section_2_6_is_written_and_read() {
        let mut request = Request::new();
        let call = request
            .call(Get::<Mailbox>::new("u33084183"))
            .expect("encodes");
        assert_eq!(
            serde_json::to_value(&request.method_calls()[0]).expect("encodes"),
            json!(["Mailbox/get", {"accountId": "u33084183"}, "c0"]),
            "no ids: all of them, as the RFC's `\"ids\": null`"
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Mailbox/get", {
                "accountId": "u33084183",
                "state": "78540",
                "list": [{
                    "id": "MB23cfa8094c0f41e6",
                    "name": "Inbox",
                    "parentId": null,
                    "role": "inbox",
                    "sortOrder": 10,
                    "totalEmails": 16307,
                    "unreadEmails": 13905,
                    "totalThreads": 5833,
                    "unreadThreads": 5128,
                    "myRights": {
                        "mayAddItems": true,
                        "mayRename": false,
                        "maySubmit": true,
                        "mayDelete": false,
                        "maySetKeywords": true,
                        "mayRemoveItems": true,
                        "mayCreateChild": true,
                        "maySetSeen": true,
                        "mayReadItems": true
                    },
                    "isSubscribed": true
                }, {
                    "id": "MB674cc24095db49ce",
                    "name": "Important mail"
                }],
                "notFound": []
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        assert_eq!(answer.state.as_deref(), Some("78540"));
        let inbox = &answer.list[0];
        assert_eq!(inbox.id.as_deref(), Some("MB23cfa8094c0f41e6"));
        assert_eq!(inbox.name.as_deref(), Some("Inbox"));
        assert_eq!(inbox.parent_id, None);
        assert_eq!(inbox.role, Some(Role::Inbox));
        assert_eq!(inbox.sort_order, Some(10));
        assert_eq!(
            (inbox.total_emails, inbox.unread_emails),
            (Some(16307), Some(13905))
        );
        assert_eq!(
            (inbox.total_threads, inbox.unread_threads),
            (Some(5833), Some(5128))
        );
        let rights = inbox.my_rights.as_ref().expect("rights");
        assert!(rights.may_read_items && rights.may_add_items && rights.may_submit);
        assert!(!rights.may_rename && !rights.may_delete);
        assert_eq!(inbox.is_subscribed, Some(true));
        assert!(inbox.extra.is_empty());
        assert_eq!(answer.list[1].role, None);
    }

    /// The third answer of RFC 8621 section 2.6's second example: only
    /// the counts of an updated mailbox.
    #[test]
    fn a_mailbox_with_only_the_properties_asked_for_is_read() {
        let answer: GetResponse<MailboxRecord> = serde_json::from_value(json!({
            "accountId": "u33084183",
            "state": "78542",
            "list": [{
                "id": "MB23cfa8094c0f41e6",
                "totalEmails": 16307,
                "unreadEmails": 13903,
                "totalThreads": 5833,
                "unreadThreads": 5127
            }],
            "notFound": []
        }))
        .expect("decodes");
        let mailbox = &answer.list[0];
        assert_eq!(mailbox.unread_emails, Some(13903));
        assert_eq!(
            (mailbox.name.as_deref(), mailbox.my_rights.as_ref()),
            (None, None)
        );
    }

    #[test]
    fn roles_and_rights_this_crate_does_not_know_are_kept() {
        let mailbox: MailboxRecord = serde_json::from_value(json!({
            "id": "m1",
            "role": "snoozed",
            "sortOrder": -1,
            "myRights": {"mayReadItems": true, "mayAdmin": true},
            "namespace": "Personal"
        }))
        .expect("decodes");
        assert_eq!(mailbox.role, Some(Role::Other("snoozed".to_owned())));
        assert_eq!(mailbox.role.as_ref().map(Role::as_str), Some("snoozed"));
        assert_eq!(mailbox.sort_order, None, "not an UnsignedInt");
        assert_eq!(mailbox.extra["sortOrder"], -1);
        assert_eq!(mailbox.extra["namespace"], "Personal");
        let rights = mailbox.my_rights.expect("rights");
        assert!(rights.may_read_items && !rights.may_add_items);
        assert_eq!(rights.extra["mayAdmin"], true);

        // A role in capitals is not one of the registry's, and is kept
        // as the server wrote it.
        let shouting: MailboxRecord =
            serde_json::from_value(json!({"role": "JUNK"})).expect("decodes");
        assert_eq!(shouting.role, Some(Role::Other("JUNK".to_owned())));
    }
}
