//! Typed method calls: the [`Method`] trait, and the standard `/get` and
//! `/set` methods of RFC 8620 section 5 for any data type.
//!
//! A data type is a marker implementing [`Object`], with [`GetObject`]
//! and [`SetObject`] saying what its `/get` returns and what its `/set`
//! takes. [`Get`] and [`Set`] then are that type's `Foo/get` and
//! `Foo/set`. Data types this crate lacks can be added from outside in
//! the same way, or called as plain JSON with
//! [`Request::call_raw`](crate::Request::call_raw).

use std::{borrow::Cow, collections::BTreeMap, marker::PhantomData};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, ser::SerializeMap,
};
use serde_json::Value;

use crate::{
    error::SetError,
    request::{Argument, ResultReference},
};

/// A typed method call: its arguments, as they serialise, and the type
/// its answer decodes to.
pub trait Method: Serialize {
    /// What the server answers with.
    type Response: DeserializeOwned;

    /// The method's name, such as `Email/get`.
    fn name(&self) -> Cow<'static, str>;

    /// The capabilities the call needs in the request's `using`, Core
    /// aside. They may depend on the arguments.
    fn using(&self) -> Vec<Cow<'static, str>>;
}

/// A JMAP data type (RFC 8620 section 1.6.3).
pub trait Object {
    /// The type's name, the part of its method names before the `/`.
    const NAME: &'static str;
    /// The capability its methods need.
    const CAPABILITY: &'static str;
}

/// A data type with a `/get` method.
pub trait GetObject: Object {
    /// A record as `/get` returns it. Its properties should all be
    /// optional: a `/get` may ask for only some.
    type Record: DeserializeOwned;
}

/// A data type with a `/set` method.
pub trait SetObject: Object {
    /// A record to create.
    type Create: Serialize;
    /// What the server reports of a created record: its id and the other
    /// properties the server set.
    type Created: DeserializeOwned;
    /// The type's own `/set` arguments, beyond the standard ones.
    type Arguments: SetArguments;
}

/// A data type's own `/set` arguments, written into the call beside the
/// standard ones.
pub trait SetArguments: Serialize + Default {
    /// Capabilities these arguments need beyond the data type's own.
    fn using(&self) -> Vec<Cow<'static, str>> {
        Vec::new()
    }
}

/// No arguments beyond the standard ones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoArguments;

impl Serialize for NoArguments {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_map(Some(0))?.end()
    }
}

impl SetArguments for NoArguments {}

/// A null, or a missing value, as the type's default: servers differ on
/// which they send for "none" (Apache James writes `"notFound": null`).
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A standard `/get` call (RFC 8620 section 5.1).
///
/// ```
/// use jmap_wire::{Get, Request, mail::Identity};
///
/// let mut request = Request::new();
/// let call = request.call(Get::<Identity>::new("u1").ids(["i1"]))?;
/// assert_eq!(call.name(), "Identity/get");
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Get<O: GetObject> {
    account_id: String,
    ids: Option<Argument<Vec<String>>>,
    properties: Option<Vec<String>>,
    extra: BTreeMap<String, Value>,
    object: PhantomData<fn() -> O>,
}

impl<O: GetObject> Get<O> {
    /// A `/get` of every record of the type in the account, with every
    /// property.
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            ids: None,
            properties: None,
            extra: BTreeMap::new(),
            object: PhantomData,
        }
    }

    /// Only the records with these ids.
    pub fn ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.ids = Some(Argument::Value(ids.into_iter().map(Into::into).collect()));
        self
    }

    /// Only the records an earlier call's result names (`#ids`, RFC 8620
    /// section 3.7).
    pub fn ids_from(mut self, reference: ResultReference) -> Self {
        self.ids = Some(Argument::Reference(reference));
        self
    }

    /// Only these properties (the id always comes too).
    pub fn properties(mut self, properties: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.properties = Some(properties.into_iter().map(Into::into).collect());
        self
    }

    /// Sets any other argument, such as a data type's own or an
    /// extension's.
    pub fn argument(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extra.insert(name.into(), value.into());
        self
    }
}

impl<O: GetObject> Serialize for Get<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("accountId", &self.account_id)?;
        match &self.ids {
            Some(Argument::Value(ids)) => map.serialize_entry("ids", ids)?,
            Some(Argument::Reference(reference)) => map.serialize_entry("#ids", reference)?,
            None => {}
        }
        if let Some(properties) = &self.properties {
            map.serialize_entry("properties", properties)?;
        }
        for (name, value) in &self.extra {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<O: GetObject> Method for Get<O> {
    type Response = GetResponse<O::Record>;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/get", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(O::CAPABILITY)]
    }
}

/// The answer to a `/get` (RFC 8620 section 5.1).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", bound(deserialize = "R: Deserialize<'de>"))]
#[non_exhaustive]
pub struct GetResponse<R> {
    /// The account the records are of.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The state of the data type; `None` from servers that keep none
    /// for it.
    #[serde(default)]
    pub state: Option<String>,
    /// The records found.
    #[serde(default, deserialize_with = "null_as_default")]
    pub list: Vec<R>,
    /// The ids asked for that are no record.
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_found: Vec<String>,
}

/// The changes to one record in a `/set` update: a *PatchObject* (RFC
/// 8620 section 5.3). Each entry sets the value at a path; setting
/// `null` puts the property back to its default, or removes a key from
/// a map.
///
/// Removing is [`remove`](Self::remove), which writes `null`; a map
/// entry is never "removed" by writing `false`.
///
/// ```
/// use jmap_wire::PatchObject;
/// use serde_json::json;
///
/// let patch = PatchObject::new()
///     .set(PatchObject::path(["keywords", "$flagged"]), true)
///     .remove(PatchObject::path(["keywords", "$seen"]))
///     .set("mailboxIds", json!({"mb-archive": true}));
/// assert_eq!(
///     serde_json::to_value(&patch)?,
///     json!({
///         "keywords/$flagged": true,
///         "keywords/$seen": null,
///         "mailboxIds": {"mb-archive": true}
///     })
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PatchObject(BTreeMap<String, Value>);

impl PatchObject {
    /// A patch that changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the value at `path`: a property name, or a path into one
    /// made with [`path`](Self::path).
    pub fn set(mut self, path: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.insert(path.into(), value.into());
        self
    }

    /// Puts the property at `path` back to its default, or removes the
    /// key at `path` from a map, by setting it to `null` as RFC 8620
    /// section 5.3 says.
    pub fn remove(mut self, path: impl Into<String>) -> Self {
        self.0.insert(path.into(), Value::Null);
        self
    }

    /// A path from its parts, such as `["keywords", "$seen"]`: the JSON
    /// Pointer (RFC 6901) of the parts without its leading `/`, as a
    /// PatchObject wants it, with `~` and `/` inside a part escaped.
    pub fn path<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
        parts
            .into_iter()
            .map(|part| part.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The paths and the values they are set to.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(path, value)| (path.as_str(), value))
    }
}

/// A standard `/set` call (RFC 8620 section 5.3): records to create,
/// update and destroy, in one call.
///
/// ```
/// use jmap_wire::{PatchObject, Request, Set, mail::{Identity, NewIdentity}};
///
/// let mut set = Set::<Identity>::new("u1");
/// let creation_id = set.create(NewIdentity::new("joe@example.com").name("Joe"));
/// set.update("i1", PatchObject::new().set("name", "Joseph"));
/// set.destroy(["i2"]);
///
/// let mut request = Request::new();
/// let call = request.call(set)?;
/// assert_eq!((call.name(), creation_id.as_str()), ("Identity/set", "k0"));
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Set<O: SetObject> {
    account_id: String,
    if_in_state: Option<String>,
    create: BTreeMap<String, O::Create>,
    update: BTreeMap<String, PatchObject>,
    destroy: Option<Argument<Vec<String>>>,
    arguments: O::Arguments,
    next_creation: usize,
}

impl<O: SetObject> Set<O> {
    /// A `/set` in the account that changes nothing yet.
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            if_in_state: None,
            create: BTreeMap::new(),
            update: BTreeMap::new(),
            destroy: None,
            arguments: O::Arguments::default(),
            next_creation: 0,
        }
    }

    /// Makes the call fail with `stateMismatch` unless the data type's
    /// state is `state`.
    pub fn if_in_state(&mut self, state: impl Into<String>) -> &mut Self {
        self.if_in_state = Some(state.into());
        self
    }

    /// Creates a record. Returns its creation id (`k0`, `k1`...), the
    /// key of the answer's `created` or `notCreated`; `#` and the
    /// creation id stands for the new record's id later in the request.
    pub fn create(&mut self, record: O::Create) -> String {
        let mut creation_id = format!("k{}", self.next_creation);
        while self.create.contains_key(&creation_id) {
            self.next_creation += 1;
            creation_id = format!("k{}", self.next_creation);
        }
        self.next_creation += 1;
        self.create.insert(creation_id.clone(), record);
        creation_id
    }

    /// Creates a record under a creation id of your choosing.
    pub fn create_as(&mut self, creation_id: impl Into<String>, record: O::Create) -> &mut Self {
        self.create.insert(creation_id.into(), record);
        self
    }

    /// Changes the record `id` as `patch` says.
    pub fn update(&mut self, id: impl Into<String>, patch: PatchObject) -> &mut Self {
        self.update.insert(id.into(), patch);
        self
    }

    /// Destroys the records with these ids, in addition to any named
    /// before.
    pub fn destroy(&mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> &mut Self {
        let ids = ids.into_iter().map(Into::into);
        match &mut self.destroy {
            Some(Argument::Value(destroy)) => destroy.extend(ids),
            _ => self.destroy = Some(Argument::Value(ids.collect())),
        }
        self
    }

    /// Destroys the records an earlier call's result names (`#destroy`,
    /// RFC 8620 section 3.7), instead of any named before.
    pub fn destroy_from(&mut self, reference: ResultReference) -> &mut Self {
        self.destroy = Some(Argument::Reference(reference));
        self
    }

    /// The data type's own arguments.
    pub fn arguments(&mut self) -> &mut O::Arguments {
        &mut self.arguments
    }
}

impl<O: SetObject> Serialize for Set<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// Writes the entries of the data type's own arguments into the
        /// call's map.
        #[derive(Serialize)]
        #[serde(bound = "")]
        struct Call<'a, O: SetObject> {
            #[serde(rename = "accountId")]
            account_id: &'a str,
            #[serde(rename = "ifInState", skip_serializing_if = "Option::is_none")]
            if_in_state: Option<&'a str>,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            create: &'a BTreeMap<String, O::Create>,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            update: &'a BTreeMap<String, PatchObject>,
            #[serde(skip_serializing_if = "Option::is_none")]
            destroy: Option<&'a Vec<String>>,
            #[serde(rename = "#destroy", skip_serializing_if = "Option::is_none")]
            destroy_from: Option<&'a ResultReference>,
            #[serde(flatten)]
            arguments: &'a O::Arguments,
        }

        let (destroy, destroy_from) = match &self.destroy {
            Some(Argument::Value(ids)) => (Some(ids), None),
            Some(Argument::Reference(reference)) => (None, Some(reference)),
            None => (None, None),
        };
        Call::<O> {
            account_id: &self.account_id,
            if_in_state: self.if_in_state.as_deref(),
            create: &self.create,
            update: &self.update,
            destroy,
            destroy_from,
            arguments: &self.arguments,
        }
        .serialize(serializer)
    }
}

impl<O: SetObject> Method for Set<O> {
    type Response = SetResponse<O::Created>;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/set", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        let mut using = vec![Cow::Borrowed(O::CAPABILITY)];
        using.extend(self.arguments.using());
        using
    }
}

/// The answer to a `/set` (RFC 8620 section 5.3). Every record of the
/// call is in exactly one of the six maps and lists, when the server
/// keeps to the RFC; check for the record you asked about rather than
/// assuming so.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", bound(deserialize = "C: Deserialize<'de>"))]
#[non_exhaustive]
pub struct SetResponse<C> {
    /// The account the changes were made in.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The data type's state before the call.
    #[serde(default)]
    pub old_state: Option<String>,
    /// The data type's state after the call.
    #[serde(default)]
    pub new_state: Option<String>,
    /// The records created, by creation id, with the properties the
    /// server set.
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: BTreeMap<String, C>,
    /// The records updated, by id; a value is the properties the server
    /// changed beyond those asked for, if any.
    #[serde(default, deserialize_with = "null_as_default")]
    pub updated: BTreeMap<String, Option<Value>>,
    /// The ids of the records destroyed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub destroyed: Vec<String>,
    /// Why records were not created, by creation id.
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_created: BTreeMap<String, SetError>,
    /// Why records were not updated, by id.
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_updated: BTreeMap<String, SetError>,
    /// Why records were not destroyed, by id.
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_destroyed: BTreeMap<String, SetError>,
}

/// A record written as a JSON object property by property: what the
/// record builders of this crate (such as
/// [`NewIdentity`](crate::mail::NewIdentity)) are underneath, so that
/// an explicit `null` can be told from a property left out, and a
/// property this crate has no setter for can still be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Properties(BTreeMap<String, Value>);

impl Properties {
    /// No properties.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a property.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<Value>) {
        self.0.insert(name.into(), value.into());
    }

    /// Sets a property to something serialisable; one that cannot be
    /// written as JSON (which no type of this crate is) becomes `null`.
    pub(crate) fn set_as(&mut self, name: &str, value: &impl Serialize) {
        self.set(name, serde_json::to_value(value).unwrap_or(Value::Null));
    }

    /// A property's value, if set.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{capability, error::SetErrorType, request::Request, response::Response};

    /// The `Todo` data type of RFC 8620 section 5.7's examples.
    struct Todo;

    impl Object for Todo {
        const NAME: &'static str = "Todo";
        const CAPABILITY: &'static str = "urn:example:todo";
    }

    impl GetObject for Todo {
        type Record = Value;
    }

    impl SetObject for Todo {
        type Create = Value;
        type Created = Value;
        type Arguments = NoArguments;
    }

    /// The first request and response of RFC 8620 section 5.7: a
    /// `Todo/query` and a `Todo/get` of what it finds (the RFC's call
    /// ids are `0` and `1`, and its list goes on with `...`).
    #[test]
    fn the_query_and_get_of_rfc_8620_section_5_7_are_written_and_read() {
        let mut request = Request::new();
        let query = request.call_raw(
            "Todo/query",
            json!({
                "accountId": "x",
                "filter": {
                    "operator": "OR",
                    "conditions": [{"hasKeyword": "music"}, {"hasKeyword": "video"}]
                },
                "sort": [{"property": "title"}],
                "position": 0,
                "limit": 10
            }),
        );
        let get = request
            .call(Get::<Todo>::new("x").ids_from(query.result_reference("/ids")))
            .expect("encodes");
        // Todo is no data type of RFC 8620 or 8621: the typed call
        // brings its capability, the call by name brings none.
        assert_eq!(request.using(), [capability::CORE, "urn:example:todo"]);
        assert_eq!(
            serde_json::to_value(&request.method_calls()[1]).expect("encodes"),
            json!(["Todo/get", {
                "accountId": "x",
                "#ids": {"resultOf": "c0", "name": "Todo/query", "path": "/ids"}
            }, "c1"])
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [
                ["Todo/query", {
                    "accountId": "x",
                    "queryState": "y13213",
                    "canCalculateChanges": true,
                    "position": 0,
                    "ids": ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]
                }, "c0"],
                ["Todo/get", {
                    "accountId": "x",
                    "state": "10324",
                    "list": [{
                        "id": "a",
                        "title": "Practise Piano",
                        "keywords": {"music": true, "beethoven": true, "mozart": true,
                                     "liszt": true, "rachmaninov": true},
                        "neuralNetworkTimeEstimation": 3600
                    }, {
                        "id": "b",
                        "title": "Watch Daft Punk music video",
                        "keywords": {"music": true, "video": true, "trance": true},
                        "neuralNetworkTimeEstimation": 18000
                    }]
                }, "c1"]
            ],
            "sessionState": "s"
        }))
        .expect("decodes");
        assert_eq!(
            response.arguments(&query).expect("answered")["queryState"],
            "y13213"
        );
        let todos = response.get(&get).expect("answered");
        assert_eq!(todos.account_id.as_deref(), Some("x"));
        assert_eq!(todos.state.as_deref(), Some("10324"));
        assert_eq!(todos.list.len(), 2);
        assert_eq!(todos.list[0]["title"], "Practise Piano");
        assert!(
            todos.not_found.is_empty(),
            "the RFC's answer leaves notFound out"
        );
    }

    #[test]
    fn a_get_can_take_its_ids_from_an_earlier_call_and_only_some_properties() {
        let mut request = Request::new();
        let changes =
            request.call_raw("Todo/changes", json!({"accountId": "x", "sinceState": "1"}));
        request
            .call(
                Get::<Todo>::new("x")
                    .ids_from(changes.result_reference("/created"))
                    .properties(["title"])
                    .argument("fetchExtras", true),
            )
            .expect("encodes");
        assert_eq!(
            request.method_calls()[1].arguments,
            json!({
                "accountId": "x",
                "#ids": {"resultOf": "c0", "name": "Todo/changes", "path": "/created"},
                "properties": ["title"],
                "fetchExtras": true
            })
        );
    }

    /// The `Todo/set` calls of RFC 8620 section 5.7: the minimal patch
    /// that adds one keyword and removes another, with its answer, and
    /// the create whose creation id an update refers to.
    #[test]
    fn the_sets_of_rfc_8620_section_5_7_are_written_and_read() {
        let mut set = Set::<Todo>::new("x");
        set.if_in_state("10324");
        set.update(
            "a",
            PatchObject::new()
                .set(PatchObject::path(["keywords", "chopin"]), true)
                .remove(PatchObject::path(["keywords", "mozart"])),
        );
        let mut request = Request::new();
        let call = request.call(set).expect("encodes");
        assert_eq!(
            request.method_calls()[0].arguments,
            json!({
                "accountId": "x",
                "ifInState": "10324",
                "update": {"a": {"keywords/chopin": true, "keywords/mozart": null}}
            })
        );

        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Todo/set", {
                "accountId": "x",
                "oldState": "10324",
                "newState": "10329",
                "updated": {"a": {"neuralNetworkTimeEstimation": 5400}}
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&call).expect("answered");
        assert_eq!(answer.old_state.as_deref(), Some("10324"));
        assert_eq!(answer.new_state.as_deref(), Some("10329"));
        assert_eq!(
            answer.updated["a"],
            Some(json!({"neuralNetworkTimeEstimation": 5400}))
        );
        assert!(answer.created.is_empty() && answer.not_updated.is_empty());

        let mut set = Set::<Todo>::new("x");
        set.create_as("k15", json!({"title": "Warm up with scales"}));
        set.update("a", PatchObject::new().set("subTodoIds", json!(["#k15"])));
        assert_eq!(
            serde_json::to_value(&set).expect("encodes"),
            json!({
                "accountId": "x",
                "create": {"k15": {"title": "Warm up with scales"}},
                "update": {"a": {"subTodoIds": ["#k15"]}}
            })
        );
    }

    /// The outcomes of RFC 8620 section 5.3 the examples do not show.
    #[test]
    fn a_set_answer_says_what_was_done_and_what_was_refused() {
        let answer: SetResponse<Value> = serde_json::from_value(json!({
            "accountId": "x",
            "created": {"k15": {"id": "e"}},
            "updated": {"a": null},
            "destroyed": ["c"],
            "notDestroyed": {"b": {"type": "notFound"}}
        }))
        .expect("decodes");
        assert_eq!(answer.created["k15"]["id"], "e");
        assert_eq!(answer.updated.get("a"), Some(&None));
        assert_eq!(answer.destroyed, ["c"]);
        assert_eq!(answer.not_destroyed["b"].error_type, SetErrorType::NotFound);
    }

    #[test]
    fn a_set_that_changes_nothing_sends_only_its_account() {
        let set = Set::<Todo>::new("x");
        assert_eq!(
            serde_json::to_value(&set).expect("encodes"),
            json!({"accountId": "x"})
        );
    }

    #[test]
    fn creation_ids_count_up_and_skip_ones_already_taken() {
        let mut set = Set::<Todo>::new("x");
        set.create_as("k1", json!({}));
        assert_eq!(set.create(json!({})), "k0");
        assert_eq!(set.create(json!({})), "k2");
        set.destroy(["a"]).destroy(["b"]);
        assert_eq!(
            serde_json::to_value(&set).expect("encodes")["destroy"],
            json!(["a", "b"])
        );
    }

    /// RFC 8620 section 5.3: a patch path is a JSON Pointer without its
    /// leading slash, and `null` removes.
    #[test]
    fn patch_paths_are_json_pointers_and_removal_is_null() {
        assert_eq!(PatchObject::path(["keywords", "$seen"]), "keywords/$seen");
        assert_eq!(
            PatchObject::path(["mailboxIds", "a/b~c"]),
            "mailboxIds/a~1b~0c"
        );
        let patch = PatchObject::new().remove(PatchObject::path(["keywords", "$seen"]));
        assert_eq!(
            serde_json::to_string(&patch).expect("encodes"),
            r#"{"keywords/$seen":null}"#
        );
        assert!(!patch.is_empty());
        assert_eq!(patch.entries().count(), 1);
    }

    /// What Apache James 3.9 writes where the RFC has an empty list or
    /// a state: `null`.
    #[test]
    fn nulls_where_a_list_or_a_map_belongs_are_read_as_empty() {
        let get: GetResponse<Value> = serde_json::from_value(json!({
            "accountId": "a1", "state": null, "notFound": null, "list": null
        }))
        .expect("decodes");
        assert!(get.list.is_empty() && get.not_found.is_empty() && get.state.is_none());
        let set: SetResponse<Value> = serde_json::from_value(json!({
            "accountId": "a1", "created": null, "updated": null, "destroyed": null,
            "notCreated": null, "notUpdated": null, "notDestroyed": null
        }))
        .expect("decodes");
        assert!(set.created.is_empty() && set.not_destroyed.is_empty());
    }
}
