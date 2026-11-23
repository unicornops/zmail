//! The standard `/copy` method (RFC 8620 section 5.4): copying records
//! from one account to another.

use std::{borrow::Cow, collections::BTreeMap, marker::PhantomData};

use serde::{Deserialize, Serialize, Serializer};

use crate::{
    error::SetError,
    method::{Method, SetObject, SetOutcome, null_as_default},
};

/// A data type with a `/copy` method.
pub trait CopyObject: SetObject {
    /// A record to copy: the id of the original and the properties to
    /// give the copy instead of the original's. It must serialise with
    /// an `id` property.
    type Copy: Serialize;
}

/// A standard `/copy` call (RFC 8620 section 5.4): copies records from
/// one account into another, which must be a different one. Within one
/// account there is nothing to copy: an email is put in a second
/// mailbox with an `Email/set` update
/// ([`EmailUpdate::add_to_mailbox`](crate::mail::EmailUpdate::add_to_mailbox)).
///
/// Moving between accounts is a copy and the destruction of the
/// original, which
/// [`on_success_destroy_original`](Self::on_success_destroy_original)
/// asks the server to do in the same call. The two are not atomic: the
/// copy may succeed and the destruction fail. The server then answers
/// with a second response, a `/set`, under the same call id
/// ([`Response::responses_to`](crate::Response::responses_to)).
///
/// ```
/// use jmap_wire::{CopyMethod, Request, mail::{Email, EmailCopy}};
/// use serde_json::json;
///
/// let mut copy = CopyMethod::<Email>::new("shared-account", "my-account");
/// let creation_id = copy.create(EmailCopy::new("M1").mailbox_ids(["inbox-of-mine"]));
/// copy.on_success_destroy_original(true);
/// assert_eq!(
///     serde_json::to_value(&copy)?,
///     json!({
///         "fromAccountId": "shared-account",
///         "accountId": "my-account",
///         "create": {"k0": {"id": "M1", "mailboxIds": {"inbox-of-mine": true}}},
///         "onSuccessDestroyOriginal": true
///     })
/// );
/// let mut request = Request::new();
/// let call = request.call(copy)?;
/// assert_eq!((call.name(), creation_id.as_str()), ("Email/copy", "k0"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct CopyMethod<O: CopyObject> {
    from_account_id: String,
    if_from_in_state: Option<String>,
    account_id: String,
    if_in_state: Option<String>,
    create: BTreeMap<String, O::Copy>,
    on_success_destroy_original: bool,
    destroy_from_if_in_state: Option<String>,
    next_creation: usize,
    object: PhantomData<fn() -> O>,
}

impl<O: CopyObject> CopyMethod<O> {
    /// A `/copy` from the account `from_account_id` into `account_id`
    /// that copies nothing yet.
    pub fn new(from_account_id: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self {
            from_account_id: from_account_id.into(),
            if_from_in_state: None,
            account_id: account_id.into(),
            if_in_state: None,
            create: BTreeMap::new(),
            on_success_destroy_original: false,
            destroy_from_if_in_state: None,
            next_creation: 0,
            object: PhantomData,
        }
    }

    /// Makes the call fail with `stateMismatch` unless the data type's
    /// state in the account copied from is `state`.
    pub fn if_from_in_state(&mut self, state: impl Into<String>) -> &mut Self {
        self.if_from_in_state = Some(state.into());
        self
    }

    /// Makes the call fail with `stateMismatch` unless the data type's
    /// state in the account copied into is `state`.
    pub fn if_in_state(&mut self, state: impl Into<String>) -> &mut Self {
        self.if_in_state = Some(state.into());
        self
    }

    /// Copies a record. Returns its creation id (`k0`, `k1`...), the
    /// key of the answer's `created` or `notCreated`.
    pub fn create(&mut self, record: O::Copy) -> String {
        let mut creation_id = format!("k{}", self.next_creation);
        while self.create.contains_key(&creation_id) {
            self.next_creation += 1;
            creation_id = format!("k{}", self.next_creation);
        }
        self.next_creation += 1;
        self.create.insert(creation_id.clone(), record);
        creation_id
    }

    /// Copies a record under a creation id of your choosing.
    pub fn create_as(&mut self, creation_id: impl Into<String>, record: O::Copy) -> &mut Self {
        self.create.insert(creation_id.into(), record);
        self
    }

    /// Whether the server should destroy each original it copied, with
    /// an implicit `/set` after the copy.
    pub fn on_success_destroy_original(&mut self, destroy: bool) -> &mut Self {
        self.on_success_destroy_original = destroy;
        self
    }

    /// The `ifInState` of that implicit `/set`: the state the account
    /// copied from must be in for the originals to be destroyed.
    pub fn destroy_from_if_in_state(&mut self, state: impl Into<String>) -> &mut Self {
        self.destroy_from_if_in_state = Some(state.into());
        self
    }
}

impl<O: CopyObject> Serialize for CopyMethod<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase", bound = "")]
        struct Call<'a, O: CopyObject> {
            from_account_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            if_from_in_state: Option<&'a str>,
            account_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            if_in_state: Option<&'a str>,
            create: &'a BTreeMap<String, O::Copy>,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            on_success_destroy_original: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            destroy_from_if_in_state: Option<&'a str>,
        }

        Call::<O> {
            from_account_id: &self.from_account_id,
            if_from_in_state: self.if_from_in_state.as_deref(),
            account_id: &self.account_id,
            if_in_state: self.if_in_state.as_deref(),
            create: &self.create,
            on_success_destroy_original: self.on_success_destroy_original,
            destroy_from_if_in_state: self.destroy_from_if_in_state.as_deref(),
        }
        .serialize(serializer)
    }
}

impl<O: CopyObject> Method for CopyMethod<O> {
    type Response = CopyResponse<O::Created>;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/copy", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(O::CAPABILITY)]
    }
}

/// The answer to a `/copy` (RFC 8620 section 5.4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", bound(deserialize = "C: Deserialize<'de>"))]
#[non_exhaustive]
pub struct CopyResponse<C> {
    /// The account the records were copied from.
    #[serde(default)]
    pub from_account_id: Option<String>,
    /// The account they were copied into.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The data type's state in the account copied into, before the
    /// call.
    #[serde(default)]
    pub old_state: Option<String>,
    /// Its state after the call.
    #[serde(default)]
    pub new_state: Option<String>,
    /// The copies made, by creation id, with the properties the server
    /// set: the copy's id above all, which is not the original's.
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: BTreeMap<String, C>,
    /// Why records were not copied, by creation id. `alreadyExists`
    /// names the record that is there already
    /// ([`SetError::existing_id`]).
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_created: BTreeMap<String, SetError>,
}

impl<C> CopyResponse<C> {
    /// What became of the copy made under `creation_id`.
    pub fn outcome_of(&self, creation_id: &str) -> SetOutcome<'_, &C> {
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        capability,
        error::SetErrorType,
        method::{NoArguments, Object},
        request::Request,
        response::Response,
    };

    /// The `Todo` data type of RFC 8620 section 5.7's examples.
    struct Todo;

    impl Object for Todo {
        const NAME: &'static str = "Todo";
        const CAPABILITY: &'static str = "urn:example:todo";
    }

    impl SetObject for Todo {
        type Create = Value;
        type Created = Value;
        type Arguments = NoArguments;
    }

    impl CopyObject for Todo {
        type Copy = Value;
    }

    /// Every argument of RFC 8620 section 5.4, and both halves of the
    /// answer with the implicit `/set` that follows it.
    #[test]
    fn a_copy_is_written_and_its_answer_read() {
        let mut copy = CopyMethod::<Todo>::new("a", "b");
        copy.if_from_in_state("s1")
            .if_in_state("s2")
            .create_as("k1", json!({"id": "t1"}))
            .on_success_destroy_original(true)
            .destroy_from_if_in_state("s1");
        assert_eq!(copy.create(json!({"id": "t2", "title": "Copy"})), "k0");
        let mut request = Request::new();
        let call = request.call(copy).expect("encodes");
        assert_eq!(request.using(), [capability::CORE, "urn:example:todo"]);
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "fromAccountId": "a",
                "ifFromInState": "s1",
                "accountId": "b",
                "ifInState": "s2",
                "create": {"k0": {"id": "t2", "title": "Copy"}, "k1": {"id": "t1"}},
                "onSuccessDestroyOriginal": true,
                "destroyFromIfInState": "s1"
            })
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [
                ["Todo/copy", {
                    "fromAccountId": "a", "accountId": "b",
                    "oldState": "s2", "newState": "s3",
                    "created": {"k0": {"id": "t9"}},
                    "notCreated": {"k1": {"type": "alreadyExists", "existingId": "t7"}}
                }, "c0"],
                ["Todo/set", {"accountId": "a", "destroyed": ["t2"]}, "c0"]
            ],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        assert_eq!(answer.from_account_id.as_deref(), Some("a"));
        assert_eq!(answer.new_state.as_deref(), Some("s3"));
        assert_eq!(answer.outcome_of("k0").done(), Some(&json!({"id": "t9"})));
        let refusal = answer.outcome_of("k1").refusal().expect("refused");
        assert_eq!(refusal.error_type, SetErrorType::AlreadyExists);
        assert_eq!(refusal.existing_id(), Some("t7"));
        assert_eq!(answer.outcome_of("k2"), SetOutcome::Unanswered);
        // The implicit destruction of the original follows under the
        // same call id.
        assert_eq!(response.responses_to("c0").count(), 2);
    }

    #[test]
    fn a_plain_copy_sends_no_optional_argument_and_reads_nulls() {
        let mut copy = CopyMethod::<Todo>::new("a", "b");
        copy.create(json!({"id": "t1"}));
        assert_eq!(
            serde_json::to_value(&copy).expect("encodes"),
            json!({"fromAccountId": "a", "accountId": "b", "create": {"k0": {"id": "t1"}}})
        );
        let answer: CopyResponse<Value> = serde_json::from_value(json!({
            "fromAccountId": "a", "accountId": "b", "oldState": null,
            "newState": "s", "created": null, "notCreated": null
        }))
        .expect("decodes");
        assert!(answer.created.is_empty() && answer.not_created.is_empty());
    }
}
