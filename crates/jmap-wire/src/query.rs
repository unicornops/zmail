//! The standard `/query` and `/queryChanges` methods (RFC 8620 sections
//! 5.5 and 5.6): the ids of the records that match a filter, sorted, a
//! window at a time, and how that list changed since.

use std::{borrow::Cow, collections::BTreeMap};

use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;

use crate::{
    method::{Method, Object, null_as_default},
    record::lenient,
    request::{Call, IdsReference},
};

/// A data type with `/query` and `/queryChanges` methods.
pub trait QueryObject: Object {
    /// The type's *FilterCondition*: the properties a record is matched
    /// on.
    type Condition: Serialize;
    /// The type's own `/query` arguments, beyond the standard ones.
    type Arguments: Serialize + Default;
}

/// What a `/query` matches records against (RFC 8620 section 5.5): one
/// condition of the data type, or conditions joined by `AND`, `OR` or
/// `NOT`, to any depth. A condition becomes a filter with `into()`.
///
/// ```
/// use jmap_wire::{Filter, mail::EmailFilterCondition};
/// use serde_json::json;
///
/// // In the inbox, and neither read nor from the newsletter.
/// let filter = Filter::and([
///     EmailFilterCondition::new().in_mailbox("mb-inbox").into(),
///     Filter::not([
///         EmailFilterCondition::new().has_keyword("$seen").into(),
///         EmailFilterCondition::new().from("news@example.com").into(),
///     ]),
/// ]);
/// assert_eq!(
///     serde_json::to_value(&filter)?,
///     json!({"operator": "AND", "conditions": [
///         {"inMailbox": "mb-inbox"},
///         {"operator": "NOT", "conditions": [
///             {"hasKeyword": "$seen"},
///             {"from": "news@example.com"}
///         ]}
///     ]})
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum Filter<C> {
    /// A *FilterCondition*: every property it has must match.
    Condition(C),
    /// A *FilterOperator* over other filters.
    Operator {
        /// How the conditions are joined.
        operator: Operator,
        /// The filters joined.
        conditions: Vec<Filter<C>>,
    },
}

/// How a *FilterOperator* joins its conditions (RFC 8620 section 5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Operator {
    /// All of the conditions must match.
    #[serde(rename = "AND")]
    And,
    /// At least one of the conditions must match.
    #[serde(rename = "OR")]
    Or,
    /// None of the conditions must match.
    #[serde(rename = "NOT")]
    Not,
}

impl<C> Filter<C> {
    fn operator(operator: Operator, conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::Operator {
            operator,
            conditions: conditions.into_iter().collect(),
        }
    }

    /// Matches what all of `conditions` match. With no conditions it
    /// matches everything.
    pub fn and(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::operator(Operator::And, conditions)
    }

    /// Matches what at least one of `conditions` matches.
    pub fn or(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::operator(Operator::Or, conditions)
    }

    /// Matches what none of `conditions` matches.
    pub fn not(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::operator(Operator::Not, conditions)
    }
}

impl<C> From<C> for Filter<C> {
    fn from(condition: C) -> Self {
        Self::Condition(condition)
    }
}

impl<C: Serialize> Serialize for Filter<C> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Condition(condition) => condition.serialize(serializer),
            Self::Operator {
                operator,
                conditions,
            } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("operator", operator)?;
                map.serialize_entry("conditions", conditions)?;
                map.end()
            }
        }
    }
}

/// One step of a sort (RFC 8620 section 5.5): a property to compare
/// records by, and how. Records equal by one comparator are compared by
/// the next.
///
/// ```
/// use jmap_wire::Comparator;
/// use serde_json::json;
///
/// // The example sort of RFC 8621 section 4.4.2.
/// let sort = [
///     Comparator::new("someInThreadHaveKeyword").with("keyword", "$flagged").descending(),
///     Comparator::new("subject").collation("i;ascii-casemap"),
///     Comparator::new("receivedAt").descending(),
/// ];
/// assert_eq!(
///     serde_json::to_value(&sort)?,
///     json!([
///         {"property": "someInThreadHaveKeyword", "keyword": "$flagged", "isAscending": false},
///         {"property": "subject", "collation": "i;ascii-casemap"},
///         {"property": "receivedAt", "isAscending": false}
///     ])
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparator {
    property: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_ascending: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    collation: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl Comparator {
    /// Sorts by `property`, ascending (the default, which is not
    /// written).
    pub fn new(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            is_ascending: None,
            collation: None,
            extra: BTreeMap::new(),
        }
    }

    /// Lowest first, written out.
    pub fn ascending(mut self) -> Self {
        self.is_ascending = Some(true);
        self
    }

    /// Highest first.
    pub fn descending(mut self) -> Self {
        self.is_ascending = Some(false);
        self
    }

    /// The collation to compare strings by, one of the session's
    /// [`collation_algorithms`](crate::CoreCapabilities::collation_algorithms).
    pub fn collation(mut self, collation: impl Into<String>) -> Self {
        self.collation = Some(collation.into());
        self
    }

    /// Another property of the comparator, such as the `keyword` of a
    /// `hasKeyword` sort (RFC 8621 section 4.4.2).
    pub fn with(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.extra.insert(name.into(), value.into());
        self
    }
}

/// Where a `/query` window starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Window {
    /// At this index; negative from the end.
    Position(i64),
    /// At this record's index plus the offset.
    Anchor(String, i64),
}

/// A standard `/query` call (RFC 8620 section 5.5): the ids of the
/// records that match `filter`, in `sort` order, from a position on and
/// at most `limit` of them.
///
/// ```
/// use jmap_wire::{Comparator, Get, Query, Request, mail::{Email, EmailFilterCondition}};
///
/// // The newest ten emails of the inbox and a few of their properties,
/// // in one request.
/// let mut request = Request::new();
/// let query = request.call(
///     Query::<Email>::new("u1")
///         .filter(EmailFilterCondition::new().in_mailbox("mb-inbox"))
///         .sort([Comparator::new("receivedAt").descending()])
///         .limit(10),
/// )?;
/// let emails = request.call(
///     Get::<Email>::new("u1")
///         .ids_from(query.ids())
///         .properties(["subject", "from", "receivedAt"]),
/// )?;
/// # let _ = emails;
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Query<O: QueryObject> {
    account_id: String,
    filter: Option<Filter<O::Condition>>,
    sort: Option<Vec<Comparator>>,
    window: Option<Window>,
    limit: Option<u64>,
    calculate_total: Option<bool>,
    arguments: O::Arguments,
}

impl<O: QueryObject> Query<O> {
    /// A query of every record of the type in the account, in the
    /// server's order.
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            filter: None,
            sort: None,
            window: None,
            limit: None,
            calculate_total: None,
            arguments: O::Arguments::default(),
        }
    }

    /// Only the records `filter` matches: a condition of the data type,
    /// or a [`Filter`] of several.
    pub fn filter(mut self, filter: impl Into<Filter<O::Condition>>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// The order of the results. Without one the order is the server's,
    /// though stable.
    pub fn sort(mut self, sort: impl IntoIterator<Item = Comparator>) -> Self {
        self.sort = Some(sort.into_iter().collect());
        self
    }

    /// The index of the first result to return, 0 unless set; a
    /// negative one counts from the end. Replaces an
    /// [`anchor`](Self::anchor).
    pub fn position(mut self, position: i64) -> Self {
        self.window = Some(Window::Position(position));
        self
    }

    /// Starts at the record `id`'s place in the results plus `offset`
    /// (which may be negative), instead of at a position; the answer's
    /// `position` then says where that is. The server answers
    /// `anchorNotFound` when `id` is not in the results.
    pub fn anchor(mut self, id: impl Into<String>, offset: i64) -> Self {
        self.window = Some(Window::Anchor(id.into(), offset));
        self
    }

    /// At most this many ids. A server may return fewer, and says so in
    /// the answer's `limit`.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether the answer should say how many records match in all,
    /// which may be slow for the server; false unless set.
    pub fn calculate_total(mut self, calculate_total: bool) -> Self {
        self.calculate_total = Some(calculate_total);
        self
    }

    /// The data type's own arguments.
    pub fn arguments(&mut self) -> &mut O::Arguments {
        &mut self.arguments
    }
}

impl<O: QueryObject> Serialize for Query<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// Writes the entries of the data type's own arguments into the
        /// call's map.
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase", bound = "")]
        struct Call<'a, O: QueryObject> {
            account_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            filter: Option<&'a Filter<O::Condition>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            sort: Option<&'a Vec<Comparator>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            position: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            anchor: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            anchor_offset: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            calculate_total: Option<bool>,
            #[serde(flatten)]
            arguments: &'a O::Arguments,
        }

        let (position, anchor, anchor_offset) = match &self.window {
            Some(Window::Position(position)) => (Some(*position), None, None),
            Some(Window::Anchor(id, offset)) => (None, Some(id.as_str()), Some(*offset)),
            None => (None, None, None),
        };
        Call::<O> {
            account_id: &self.account_id,
            filter: self.filter.as_ref(),
            sort: self.sort.as_ref(),
            position,
            anchor,
            anchor_offset,
            limit: self.limit,
            calculate_total: self.calculate_total,
            arguments: &self.arguments,
        }
        .serialize(serializer)
    }
}

impl<O: QueryObject> Method for Query<O> {
    type Response = QueryResponse;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/query", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(O::CAPABILITY)]
    }
}

impl<O: QueryObject> Call<Query<O>> {
    /// A reference to the ids the query finds (`/ids`), for the
    /// `ids_from` of a `/get` later in the request: the idiomatic JMAP
    /// read, a query and a get in one round trip (RFC 8620 section
    /// 3.7).
    pub fn ids(&self) -> IdsReference<O> {
        IdsReference::new(self.result_reference("/ids"))
    }
}

/// The answer to a `/query` (RFC 8620 section 5.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct QueryResponse {
    /// The account queried.
    #[serde(default)]
    pub account_id: Option<String>,
    /// A string that changes when the results of this query (with this
    /// filter and sort) may have: compare it with a later answer's, or
    /// give it to `/queryChanges`.
    #[serde(default)]
    pub query_state: Option<String>,
    /// Whether the server can answer `/queryChanges` for this filter
    /// and sort.
    #[serde(default, deserialize_with = "lenient")]
    pub can_calculate_changes: bool,
    /// The index of the first of `ids` in the whole list of results.
    #[serde(default, deserialize_with = "lenient")]
    pub position: u64,
    /// The ids found, from `position` on.
    #[serde(default, deserialize_with = "null_as_default")]
    pub ids: Vec<String>,
    /// How many records match in all; only when the call asked with
    /// `calculate_total`.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub total: Option<u64>,
    /// The limit the server applied, when it is not the one asked for.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub limit: Option<u64>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A standard `/queryChanges` call (RFC 8620 section 5.6): how the
/// results of a query changed since a `query_state`, to update a cached
/// list of ids without fetching it again. The filter and sort must be
/// those of the query.
///
/// A server that cannot say answers with the `cannotCalculateChanges`
/// method error, or `tooManyChanges` when there are more than
/// `max_changes`.
#[derive(Debug, Clone)]
pub struct QueryChanges<O: QueryObject> {
    account_id: String,
    filter: Option<Filter<O::Condition>>,
    sort: Option<Vec<Comparator>>,
    since_query_state: String,
    max_changes: Option<u64>,
    up_to_id: Option<String>,
    calculate_total: Option<bool>,
    arguments: O::Arguments,
}

impl<O: QueryObject> QueryChanges<O> {
    /// The changes to the query's results in the account since
    /// `since_query_state`, the `query_state` of an earlier answer.
    pub fn new(account_id: impl Into<String>, since_query_state: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            filter: None,
            sort: None,
            since_query_state: since_query_state.into(),
            max_changes: None,
            up_to_id: None,
            calculate_total: None,
            arguments: O::Arguments::default(),
        }
    }

    /// The filter the query had.
    pub fn filter(mut self, filter: impl Into<Filter<O::Condition>>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// The sort the query had.
    pub fn sort(mut self, sort: impl IntoIterator<Item = Comparator>) -> Self {
        self.sort = Some(sort.into_iter().collect());
        self
    }

    /// At most this many changes, or the `tooManyChanges` error.
    pub fn max_changes(mut self, max_changes: u64) -> Self {
        self.max_changes = Some(max_changes);
        self
    }

    /// The last id of the results the client has cached, which lets the
    /// server leave out changes after it when the filter and sort are
    /// on properties that cannot change.
    pub fn up_to_id(mut self, id: impl Into<String>) -> Self {
        self.up_to_id = Some(id.into());
        self
    }

    /// Whether the answer should say how many records match now.
    pub fn calculate_total(mut self, calculate_total: bool) -> Self {
        self.calculate_total = Some(calculate_total);
        self
    }

    /// The data type's own arguments, which must be those the query had.
    pub fn arguments(&mut self) -> &mut O::Arguments {
        &mut self.arguments
    }
}

impl<O: QueryObject> Serialize for QueryChanges<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase", bound = "")]
        struct Call<'a, O: QueryObject> {
            account_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            filter: Option<&'a Filter<O::Condition>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            sort: Option<&'a Vec<Comparator>>,
            since_query_state: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            max_changes: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            up_to_id: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            calculate_total: Option<bool>,
            #[serde(flatten)]
            arguments: &'a O::Arguments,
        }

        Call::<O> {
            account_id: &self.account_id,
            filter: self.filter.as_ref(),
            sort: self.sort.as_ref(),
            since_query_state: &self.since_query_state,
            max_changes: self.max_changes,
            up_to_id: self.up_to_id.as_deref(),
            calculate_total: self.calculate_total,
            arguments: &self.arguments,
        }
        .serialize(serializer)
    }
}

impl<O: QueryObject> Method for QueryChanges<O> {
    type Response = QueryChangesResponse;

    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(format!("{}/queryChanges", O::NAME))
    }

    fn using(&self) -> Vec<Cow<'static, str>> {
        vec![Cow::Borrowed(O::CAPABILITY)]
    }
}

impl<O: QueryObject> Call<QueryChanges<O>> {
    /// A reference to the ids added to the results (`/added/*/id`), for
    /// the `ids_from` of a `/get` later in the request.
    pub fn added_ids(&self) -> IdsReference<O> {
        IdsReference::new(self.result_reference("/added/*/id"))
    }
}

/// The answer to a `/queryChanges` (RFC 8620 section 5.6). To bring a
/// cached list of ids up to date, take out every id in `removed`, then
/// put in each of `added` at its index, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct QueryChangesResponse {
    /// The account queried.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The state the changes are from.
    #[serde(default)]
    pub old_query_state: Option<String>,
    /// The state the changes lead to.
    #[serde(default)]
    pub new_query_state: Option<String>,
    /// How many records match now; only when the call asked.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub total: Option<u64>,
    /// The ids no longer in the results, or that may have moved (those
    /// are in `added` again).
    #[serde(default, deserialize_with = "null_as_default")]
    pub removed: Vec<String>,
    /// The ids now in the results, each with its index there, lowest
    /// index first.
    #[serde(default, deserialize_with = "null_as_default")]
    pub added: Vec<AddedItem>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// An id added to a query's results, and where (RFC 8620 section 5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedItem {
    /// The record's id.
    pub id: String,
    /// Its index in the results, in the new state.
    pub index: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        Get, Request, Response, capability,
        mail::{Email, EmailFilterCondition, Mailbox, MailboxFilterCondition, Thread},
    };

    /// The four calls of RFC 8620 section 3.7's second example (also
    /// RFC 8621 section 4.10's first): the newest ten threads of the
    /// inbox and three properties of every email in them, each call fed
    /// by the one before. The RFC's call ids are `t0` to `t3`.
    #[test]
    fn the_chained_calls_of_rfc_8620_section_3_7_are_written_with_typed_references() {
        let mut request = Request::new();
        let mut query = Query::<Email>::new("A1")
            .filter(EmailFilterCondition::new().in_mailbox("id_of_inbox"))
            .sort([Comparator::new("receivedAt").descending()])
            .position(0)
            .limit(10)
            .calculate_total(true);
        query.arguments().collapse_threads = Some(true);
        let query = request.call(query).expect("encodes");
        let found = request
            .call(
                Get::<Email>::new("A1")
                    .ids_from(query.ids())
                    .properties(["threadId"]),
            )
            .expect("encodes");
        let threads = request
            .call(Get::<Thread>::new("A1").ids_from(found.thread_ids()))
            .expect("encodes");
        request
            .call(
                Get::<Email>::new("A1")
                    .ids_from(threads.email_ids())
                    .properties(["from", "receivedAt", "subject"]),
            )
            .expect("encodes");

        assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
        assert_eq!(
            serde_json::to_value(request.method_calls()).expect("encodes"),
            json!([
                ["Email/query", {
                    "accountId": "A1",
                    "filter": {"inMailbox": "id_of_inbox"},
                    "sort": [{"property": "receivedAt", "isAscending": false}],
                    "collapseThreads": true,
                    "position": 0,
                    "limit": 10,
                    "calculateTotal": true
                }, "c0"],
                ["Email/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c0", "name": "Email/query", "path": "/ids"},
                    "properties": ["threadId"]
                }, "c1"],
                ["Thread/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c1", "name": "Email/get", "path": "/list/*/threadId"}
                }, "c2"],
                ["Email/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c2", "name": "Thread/get", "path": "/list/*/emailIds"},
                    "properties": ["from", "receivedAt", "subject"]
                }, "c3"]
            ])
        );
        request
            .check_references()
            .expect("every reference resolves");

        // The answers to the first three calls, as the RFC has them
        // (its lists go on with `...`).
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [
                ["Email/query", {
                    "accountId": "A1",
                    "queryState": "abcdefg",
                    "canCalculateChanges": true,
                    "position": 0,
                    "total": 101,
                    "ids": ["msg1023", "msg223", "msg110", "msg93", "msg91",
                            "msg38", "msg36", "msg33", "msg11", "msg1"]
                }, "c0"],
                ["Email/get", {
                    "accountId": "A1",
                    "state": "123456",
                    "list": [
                        {"id": "msg1023", "threadId": "trd194"},
                        {"id": "msg223", "threadId": "trd114"}
                    ],
                    "notFound": []
                }, "c1"],
                ["Thread/get", {
                    "accountId": "A1",
                    "state": "123456",
                    "list": [
                        {"id": "trd194", "emailIds": ["msg1020", "msg1021", "msg1023"]},
                        {"id": "trd114", "emailIds": ["msg201", "msg223"]}
                    ],
                    "notFound": []
                }, "c2"]
            ],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = response.get(&query).expect("answered");
        assert_eq!(answer.query_state.as_deref(), Some("abcdefg"));
        assert!(answer.can_calculate_changes);
        assert_eq!((answer.position, answer.total), (0, Some(101)));
        assert_eq!(answer.ids.len(), 10);
        assert_eq!(
            response.get(&found).expect("answered").list[1]
                .thread_id
                .as_deref(),
            Some("trd114")
        );
        assert_eq!(
            response.get(&threads).expect("answered").list[0].email_ids,
            Some(vec![
                "msg1020".to_owned(),
                "msg1021".to_owned(),
                "msg1023".to_owned()
            ])
        );
    }

    /// The `Todo/query` filter of RFC 8620 section 5.7.
    #[test]
    fn filters_nest_and_an_anchor_replaces_the_position() {
        let filter: Filter<Value> = Filter::or([
            json!({"hasKeyword": "music"}).into(),
            json!({"hasKeyword": "video"}).into(),
        ]);
        assert_eq!(
            serde_json::to_value(&filter).expect("encodes"),
            json!({
                "operator": "OR",
                "conditions": [{"hasKeyword": "music"}, {"hasKeyword": "video"}]
            })
        );
        assert_eq!(
            serde_json::to_value(Filter::<Value>::and([])).expect("encodes"),
            json!({"operator": "AND", "conditions": []})
        );

        let query = Query::<Email>::new("a1")
            .position(20)
            .anchor("M7", -1)
            .limit(5);
        assert_eq!(
            serde_json::to_value(&query).expect("encodes"),
            json!({"accountId": "a1", "anchor": "M7", "anchorOffset": -1, "limit": 5})
        );
        assert_eq!(
            serde_json::to_value(Query::<Email>::new("a1").anchor("M7", 0).position(-10))
                .expect("encodes"),
            json!({"accountId": "a1", "position": -10})
        );
        assert_eq!(
            serde_json::to_value(Query::<Email>::new("a1")).expect("encodes"),
            json!({"accountId": "a1"}),
            "a query of everything sends only its account"
        );
    }

    /// RFC 8621 section 2.3: the mailbox conditions and tree arguments.
    #[test]
    fn a_mailbox_query_has_its_own_conditions_and_arguments() {
        let query = Query::<Mailbox>::new("a1")
            .filter(Filter::or([
                MailboxFilterCondition::new()
                    .parent(None)
                    .has_any_role(true)
                    .into(),
                MailboxFilterCondition::new().name("Projects").into(),
            ]))
            .sort([Comparator::new("sortOrder"), Comparator::new("name")])
            .sort_as_tree(true)
            .filter_as_tree(false);
        assert_eq!(
            serde_json::to_value(&query).expect("encodes"),
            json!({
                "accountId": "a1",
                "filter": {"operator": "OR", "conditions": [
                    {"parentId": null, "hasAnyRole": true},
                    {"name": "Projects"}
                ]},
                "sort": [{"property": "sortOrder"}, {"property": "name"}],
                "sortAsTree": true,
                "filterAsTree": false
            })
        );
    }

    /// The `Email/queryChanges` call and answer of RFC 8621 section
    /// 4.10.
    #[test]
    fn the_query_changes_of_rfc_8621_section_4_10_is_written_and_read() {
        let mut changes = QueryChanges::<Email>::new("ue150411c", "09aa9a075588-780599:0")
            .filter(EmailFilterCondition::new().in_mailbox("fb666a55"))
            .sort([Comparator::new("receivedAt").descending()])
            .up_to_id("Mc2781d5e856a908d8a35a564")
            .max_changes(25)
            .calculate_total(true);
        changes.arguments().collapse_threads = Some(true);
        let mut request = Request::new();
        let call = request.call(changes).expect("encodes");
        assert_eq!(
            serde_json::to_value(&request.method_calls()[0]).expect("encodes"),
            json!(["Email/queryChanges", {
                "accountId": "ue150411c",
                "filter": {"inMailbox": "fb666a55"},
                "sort": [{"property": "receivedAt", "isAscending": false}],
                "collapseThreads": true,
                "sinceQueryState": "09aa9a075588-780599:0",
                "upToId": "Mc2781d5e856a908d8a35a564",
                "maxChanges": 25,
                "calculateTotal": true
            }, "c0"])
        );
        assert_eq!(call.added_ids().as_reference().path, "/added/*/id");

        let answer: QueryChangesResponse = serde_json::from_value(json!({
            "accountId": "ue150411c",
            "oldQueryState": "09aa9a075588-780599:0",
            "newQueryState": "e35e9facf117-780615:0",
            "added": [{"id": "Me8de6c9f6de198239b982ea2", "index": 0}],
            "removed": ["M9bd17497e2a99cb345fc1d0a"],
            "total": 115
        }))
        .expect("decodes");
        assert_eq!(
            answer.new_query_state.as_deref(),
            Some("e35e9facf117-780615:0")
        );
        assert_eq!(answer.removed, ["M9bd17497e2a99cb345fc1d0a"]);
        assert_eq!(
            answer.added,
            [AddedItem {
                id: "Me8de6c9f6de198239b982ea2".to_owned(),
                index: 0
            }]
        );
        assert_eq!(answer.total, Some(115));
    }

    #[test]
    fn a_query_answer_with_nulls_and_oddities_still_gives_its_ids() {
        let answer: QueryResponse = serde_json::from_value(json!({
            "accountId": "a1", "queryState": null, "canCalculateChanges": null,
            "position": -1, "ids": null, "total": "unknown", "limit": 256,
            "later": {"a": 1}
        }))
        .expect("decodes");
        assert_eq!(answer.query_state, None);
        assert!(!answer.can_calculate_changes);
        assert_eq!(
            (answer.position, answer.total, answer.limit),
            (0, None, Some(256))
        );
        assert!(answer.ids.is_empty());
        assert_eq!(answer.extra["later"], json!({"a": 1}));
        // Ids that are not strings are an error, not an empty list.
        assert!(serde_json::from_value::<QueryResponse>(json!({"ids": [1, 2]})).is_err());
    }
}
