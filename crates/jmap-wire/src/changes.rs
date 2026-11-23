//! The standard `/changes` method (RFC 8620 section 5.2): what was
//! created, updated and destroyed since a state.

use std::{borrow::Cow, collections::BTreeMap, marker::PhantomData};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    method::{Method, Object, null_as_default},
    request::{Call, IdsReference},
};

/// A data type with a `/changes` method.
pub trait ChangesObject: Object {}

/// A standard `/changes` call (RFC 8620 section 5.2): the ids of the
/// records created, updated and destroyed since `since_state`, a state
/// an earlier `/get`, `/set` or `/changes` of the data type returned.
///
/// The answer may be partial: while
/// [`has_more_changes`](ChangesResponse::has_more_changes) is true, call
/// again from its `new_state`. A server that cannot say what changed
/// since the state answers with the `cannotCalculateChanges` method
/// error, and everything held of the type has to be fetched again.
///
/// ```
/// use jmap_wire::{Changes, Get, Request, mail::Email};
///
/// // What changed, and the new and changed emails, in one request.
/// let mut request = Request::new();
/// let changes = request.call(Changes::<Email>::new("u1", "780605").max_changes(50))?;
/// request.call(Get::<Email>::new("u1").ids_from(changes.created()))?;
/// request.call(
///     Get::<Email>::new("u1")
///         .ids_from(changes.updated())
///         .properties(["keywords", "mailboxIds"]),
/// )?;
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", bound = "")]
pub struct Changes<O: ChangesObject> {
    account_id: String,
    since_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_changes: Option<u64>,
    #[serde(skip)]
    object: PhantomData<fn() -> O>,
}

impl<O: ChangesObject> Changes<O> {
    /// The changes in the account since `since_state`.
    pub fn new(account_id: impl Into<String>, since_state: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            since_state: since_state.into(),
            max_changes: None,
            object: PhantomData,
        }
    }

    /// At most this many ids in the answer; it must be greater than 0.
    /// Without it the server chooses.
    pub fn max_changes(mut self, max_changes: u64) -> Self {
        self.max_changes = Some(max_changes);
        self
    }
}

impl<O: ChangesObject> Method for Changes<O> {
    type Response = ChangesResponse;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/changes", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(O::CAPABILITY)]
    }
}

impl<O: ChangesObject> Call<Changes<O>> {
    /// A reference to the ids of the records created (`/created`), for
    /// the `ids_from` of a `/get` later in the request.
    pub fn created(&self) -> IdsReference<O> {
        IdsReference::new(self.result_reference("/created"))
    }

    /// A reference to the ids of the records updated (`/updated`).
    pub fn updated(&self) -> IdsReference<O> {
        IdsReference::new(self.result_reference("/updated"))
    }
}

/// The answer to a `/changes` (RFC 8620 section 5.2).
///
/// A record may be in more than one list: one created and updated since
/// the old state may be in both `created` and `updated`, one created and
/// destroyed in both `created` and `destroyed` or in neither.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChangesResponse {
    /// The account the changes are of.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The state the changes are from: the call's `since_state`.
    #[serde(default)]
    pub old_state: Option<String>,
    /// The state the changes lead to. `None` only from a server that
    /// leaves it out, against the RFC; there is then no state to go on
    /// from.
    #[serde(default)]
    pub new_state: Option<String>,
    /// Whether there are more changes after `new_state`, which another
    /// call from that state returns.
    #[serde(default, deserialize_with = "null_as_default")]
    pub has_more_changes: bool,
    /// The ids of the records created.
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: Vec<String>,
    /// The ids of the records updated.
    #[serde(default, deserialize_with = "null_as_default")]
    pub updated: Vec<String>,
    /// The ids of the records destroyed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub destroyed: Vec<String>,
    /// What a data type adds to the answer, such as `Mailbox/changes`
    /// its `updatedProperties`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ChangesResponse {
    /// The `updatedProperties` of a `Mailbox/changes` (RFC 8621 section
    /// 2.2): when only the counts of the updated mailboxes changed, the
    /// properties that may have; `None` when the server cannot tell, or
    /// for any other data type.
    pub fn updated_properties(&self) -> Option<Vec<String>> {
        Vec::deserialize(self.extra.get("updatedProperties")?).ok()
    }

    /// How many ids the answer has in all.
    pub fn len(&self) -> usize {
        self.created.len() + self.updated.len() + self.destroyed.len()
    }

    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        Error, Get, MethodErrorType, Request, Response,
        mail::{Email, Mailbox},
    };

    /// The `Email/changes` call and answer of RFC 8621 section 4.10
    /// (the RFC's destroyed list goes on with `...`).
    #[test]
    fn the_email_changes_of_rfc_8621_section_4_10_is_written_and_read() {
        let mut request = Request::new();
        let call = request
            .call(Changes::<Email>::new("ue150411c", "780605").max_changes(50))
            .expect("encodes");
        assert_eq!(
            serde_json::to_value(&request.method_calls()[0]).expect("encodes"),
            json!(["Email/changes", {
                "accountId": "ue150411c",
                "sinceState": "780605",
                "maxChanges": 50
            }, "c0"])
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Email/changes", {
                "accountId": "ue150411c",
                "oldState": "780605",
                "newState": "780800",
                "hasMoreChanges": false,
                "created": ["Me8de6c9f6de198239b982ea2"],
                "updated": ["Ma783e5cdf5f2deffbc97930a"],
                "destroyed": ["M9bd17497e2a99cb345fc1d0a"]
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let changes = response.get(&call).expect("answered");
        assert_eq!(changes.old_state.as_deref(), Some("780605"));
        assert_eq!(changes.new_state.as_deref(), Some("780800"));
        assert!(!changes.has_more_changes);
        assert_eq!(changes.created, ["Me8de6c9f6de198239b982ea2"]);
        assert_eq!(changes.updated, ["Ma783e5cdf5f2deffbc97930a"]);
        assert_eq!(changes.destroyed, ["M9bd17497e2a99cb345fc1d0a"]);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes.updated_properties(), None);
    }

    /// The request and the first answer of RFC 8621 section 2.6's
    /// second example: `Mailbox/changes`, then the created mailboxes
    /// and, of the updated ones, only the properties that changed.
    #[test]
    fn the_mailbox_changes_of_rfc_8621_section_2_6_feeds_two_gets() {
        let account = "u33084183";
        let mut request = Request::new();
        let changes = request
            .call(Changes::<Mailbox>::new(account, "78540"))
            .expect("encodes");
        request
            .call(Get::<Mailbox>::new(account).ids_from(changes.created()))
            .expect("encodes");
        request
            .call(
                Get::<Mailbox>::new(account)
                    .ids_from(changes.updated())
                    .properties_from(changes.updated_properties()),
            )
            .expect("encodes");
        assert_eq!(
            serde_json::to_value(request.method_calls()).expect("encodes"),
            json!([
                ["Mailbox/changes", {"accountId": "u33084183", "sinceState": "78540"}, "c0"],
                ["Mailbox/get", {
                    "accountId": "u33084183",
                    "#ids": {"resultOf": "c0", "name": "Mailbox/changes", "path": "/created"}
                }, "c1"],
                ["Mailbox/get", {
                    "accountId": "u33084183",
                    "#ids": {"resultOf": "c0", "name": "Mailbox/changes", "path": "/updated"},
                    "#properties": {
                        "resultOf": "c0",
                        "name": "Mailbox/changes",
                        "path": "/updatedProperties"
                    }
                }, "c2"]
            ])
        );

        let answer: ChangesResponse = serde_json::from_value(json!({
            "accountId": "u33084183",
            "oldState": "78541",
            "newState": "78542",
            "hasMoreChanges": false,
            "updatedProperties": ["totalEmails", "unreadEmails", "totalThreads", "unreadThreads"],
            "created": [],
            "updated": ["MB23cfa8094c0f41e6"],
            "destroyed": []
        }))
        .expect("decodes");
        assert_eq!(
            answer.updated_properties().expect("only counts changed"),
            [
                "totalEmails",
                "unreadEmails",
                "totalThreads",
                "unreadThreads"
            ]
        );
        assert_eq!(answer.updated, ["MB23cfa8094c0f41e6"]);
    }

    #[test]
    fn nulls_and_missing_lists_are_no_changes_and_the_error_is_typed() {
        let answer: ChangesResponse = serde_json::from_value(json!({
            "accountId": "a1", "oldState": "1", "newState": null, "hasMoreChanges": null,
            "created": null, "updatedProperties": null
        }))
        .expect("decodes");
        assert!(answer.is_empty() && !answer.has_more_changes);
        assert_eq!(answer.new_state, None);
        assert_eq!(answer.updated_properties(), None);

        let mut request = Request::new();
        let call = request
            .call(Changes::<Email>::new("a1", "long-ago"))
            .expect("encodes");
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["error", {"type": "cannotCalculateChanges"}, "c0"]]
        }))
        .expect("decodes");
        match response.get(&call) {
            Err(Error::Method(error)) => {
                assert_eq!(error.error_type, MethodErrorType::CannotCalculateChanges);
            }
            other => panic!("expected the method error, got {other:?}"),
        }
    }
}
