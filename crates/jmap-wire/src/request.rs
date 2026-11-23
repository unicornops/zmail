//! The request envelope (RFC 8620 section 3.3): method calls with their
//! call ids, result references between them, and a `using` worked out
//! from the calls themselves.

use std::{borrow::Cow, collections::BTreeMap, marker::PhantomData};

use serde::{
    Deserialize, Serialize, Serializer,
    de::{self, Deserializer, SeqAccess, Visitor},
    ser::{SerializeSeq, SerializeStruct},
};
use serde_json::Value;

use crate::{capability, error::Error, method::Method};

/// One method call or one method response: a name, an arguments object
/// and a call id, written as a three-element array (RFC 8620 section
/// 3.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// The method name, or the response name (`error` for a method-level
    /// error).
    pub name: String,
    /// The arguments object.
    pub arguments: Value,
    /// The call id: the client's own string, which the server echoes on
    /// every response to the call.
    pub call_id: String,
}

impl Serialize for Invocation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(3))?;
        seq.serialize_element(&self.name)?;
        seq.serialize_element(&self.arguments)?;
        seq.serialize_element(&self.call_id)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Invocation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct InvocationVisitor;

        impl<'de> Visitor<'de> for InvocationVisitor {
            type Value = Invocation;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an Invocation: [name, arguments, call id]")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Invocation, A::Error> {
                let name = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let arguments = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let call_id = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(2, &self))?;
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(4, &self));
                }
                Ok(Invocation {
                    name,
                    arguments,
                    call_id,
                })
            }
        }

        deserializer.deserialize_seq(InvocationVisitor)
    }
}

/// A reference to the result of an earlier method call in the same
/// request, used in place of an argument (RFC 8620 section 3.7). The
/// argument's name is then written with a `#` in front.
///
/// Typed calls take one where an argument allows it (such as
/// [`Get::ids_from`](crate::Get::ids_from)); get one from
/// [`Call::result_reference`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultReference {
    /// The call id of the earlier call.
    pub result_of: String,
    /// The response name that call must have had.
    pub name: String,
    /// A JSON Pointer into that response's arguments, where `*` maps
    /// through an array.
    pub path: String,
}

/// An argument that is either given or taken from an earlier call's
/// result (RFC 8620 section 3.7).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Argument<T> {
    /// The value itself.
    Value(T),
    /// A reference to an earlier result; the argument's name gets a `#`.
    Reference(ResultReference),
}

/// A method call that was added to a [`Request`]: its call id and, for
/// typed calls, the type its answer decodes to.
///
/// Pass it to [`Response::get`](crate::Response::get) for the answer, or
/// use [`result_reference`](Self::result_reference) to feed its result
/// to a later call.
#[derive(Debug)]
pub struct Call<M: ?Sized> {
    call_id: String,
    name: String,
    method: PhantomData<fn() -> M>,
}

impl<M: ?Sized> Clone for Call<M> {
    fn clone(&self) -> Self {
        Self {
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            method: PhantomData,
        }
    }
}

impl<M: ?Sized> Call<M> {
    /// The call's id in the request.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// The method that was called.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A reference to this call's result at `path` (a JSON Pointer into
    /// the response's arguments, such as `/ids` or `/list/*/threadId`).
    pub fn result_reference(&self, path: impl Into<String>) -> ResultReference {
        ResultReference {
            result_of: self.call_id.clone(),
            name: self.name.clone(),
            path: path.into(),
        }
    }
}

/// A method call made by name with JSON arguments: what
/// [`Request::call_raw`] returns a [`Call`] of.
#[derive(Debug)]
pub enum Raw {}

/// How a request's `using` is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Using {
    /// Core, then what the method calls need, then what was added.
    Computed { added: Vec<String> },
    /// Exactly this.
    Exactly(Vec<String>),
}

/// A JMAP request (RFC 8620 section 3.3): a batch of method calls.
///
/// # `using`
///
/// `using` is worked out from the calls in the request, in order of
/// first use:
///
/// 1. `urn:ietf:params:jmap:core`, always: the request envelope, result
///    references and errors are all of JMAP Core, and every example
///    request in RFC 8620 and RFC 8621 lists it;
/// 2. what each call needs: a typed call says so itself
///    ([`Method::using`]), and a call made by name is looked up by its
///    data type ([`capability::for_method`]);
/// 3. anything added with [`add_capability`](Self::add_capability), for
///    extensions this crate has no table entry for.
///
/// Nothing else is listed. A server must reject a request naming a
/// capability it does not support (section 3.6.1), so a client that
/// lists capabilities its calls do not use fails against servers that
/// would have answered those calls.
/// [`set_capabilities`](Self::set_capabilities) replaces the computed
/// list altogether.
///
/// ```
/// use jmap_wire::{Request, capability};
/// use serde_json::json;
///
/// let mut request = Request::new();
/// request.call_raw("Mailbox/get", json!({"accountId": "u1"}));
/// request.call_raw("Email/query", json!({"accountId": "u1"}));
/// assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    using: Using,
    /// Capabilities the calls need, in order of first use.
    needed: Vec<String>,
    method_calls: Vec<Invocation>,
    created_ids: Option<BTreeMap<String, String>>,
    next_call: usize,
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    /// A request with no method calls.
    pub fn new() -> Self {
        Self {
            using: Using::Computed { added: Vec::new() },
            needed: Vec::new(),
            method_calls: Vec::new(),
            created_ids: None,
            next_call: 0,
        }
    }

    /// Adds a typed method call. Its capabilities join `using`.
    ///
    /// Fails only when the call cannot be written as JSON, which the
    /// calls of this crate never do.
    pub fn call<M: Method>(&mut self, method: M) -> Result<Call<M>, Error> {
        let name = method.name();
        for capability in method.using() {
            self.need(&capability);
        }
        let arguments = serde_json::to_value(&method).map_err(Error::Encode)?;
        Ok(self.push(name, arguments))
    }

    /// Adds a method call by name with its arguments as JSON: the way to
    /// call anything this crate has no type for. The capability of a
    /// data type of RFC 8620 or RFC 8621 joins `using`
    /// ([`capability::for_method`]); for any other, say which with
    /// [`add_capability`](Self::add_capability).
    ///
    /// ```
    /// use jmap_wire::Request;
    /// use serde_json::json;
    ///
    /// let mut request = Request::new();
    /// let cards = request.call_raw("ContactCard/get", json!({"accountId": "u1", "ids": null}));
    /// request.add_capability("urn:ietf:params:jmap:contacts");
    /// assert_eq!(cards.call_id(), "c0");
    /// assert_eq!(
    ///     request.using(),
    ///     ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:contacts"]
    /// );
    /// ```
    pub fn call_raw(&mut self, name: impl Into<String>, arguments: Value) -> Call<Raw> {
        let name = name.into();
        if let Some(capability) = capability::for_method(&name) {
            self.need(capability);
        }
        self.push(Cow::Owned(name), arguments)
    }

    fn push<M: ?Sized>(&mut self, name: Cow<'static, str>, arguments: Value) -> Call<M> {
        let call_id = format!("c{}", self.next_call);
        self.next_call += 1;
        self.method_calls.push(Invocation {
            name: name.clone().into_owned(),
            arguments,
            call_id: call_id.clone(),
        });
        Call {
            call_id,
            name: name.into_owned(),
            method: PhantomData,
        }
    }

    fn need(&mut self, capability: &str) {
        if !self.needed.iter().any(|needed| needed == capability) {
            self.needed.push(capability.to_owned());
        }
    }

    /// Adds a capability to the computed `using`, for a call whose
    /// capability this crate cannot know. No effect after
    /// [`set_capabilities`](Self::set_capabilities).
    pub fn add_capability(&mut self, capability: impl Into<String>) -> &mut Self {
        let capability = capability.into();
        if let Using::Computed { added } = &mut self.using
            && !added.contains(&capability)
        {
            added.push(capability);
        }
        self
    }

    /// Sends exactly these capabilities as `using`, whatever the calls
    /// are: nothing is added, not even Core.
    pub fn set_capabilities(
        &mut self,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        self.using = Using::Exactly(capabilities.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the request's `createdIds` (RFC 8620 section 3.3): creation
    /// ids and the ids they stand for, from an earlier response. When
    /// given, even empty, the response has `createdIds` too.
    pub fn set_created_ids(&mut self, created_ids: BTreeMap<String, String>) -> &mut Self {
        self.created_ids = Some(created_ids);
        self
    }

    /// The capabilities the request will send as `using`.
    pub fn using(&self) -> Vec<&str> {
        match &self.using {
            Using::Exactly(capabilities) => capabilities.iter().map(String::as_str).collect(),
            Using::Computed { added } => {
                let mut using = vec![capability::CORE];
                for capability in self.needed.iter().chain(added) {
                    if !using.contains(&capability.as_str()) {
                        using.push(capability);
                    }
                }
                using
            }
        }
    }

    /// The method calls, in order.
    pub fn method_calls(&self) -> &[Invocation] {
        &self.method_calls
    }

    /// Whether the request has no method calls.
    pub fn is_empty(&self) -> bool {
        self.method_calls.is_empty()
    }
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = 2 + usize::from(self.created_ids.is_some());
        let mut request = serializer.serialize_struct("Request", fields)?;
        request.serialize_field("using", &self.using())?;
        request.serialize_field("methodCalls", &self.method_calls)?;
        if let Some(created_ids) = &self.created_ids {
            request.serialize_field("createdIds", created_ids)?;
        }
        request.end()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::capability::{CORE, MAIL, SUBMISSION};

    /// The example request of RFC 8620 section 3.3.1, call ids aside
    /// (the RFC's are `c1` to `c3`; this crate counts from `c0`).
    #[test]
    fn the_request_of_rfc_8620_section_3_3_1_is_written_as_the_rfc_has_it() {
        let mut request = Request::new();
        request.call_raw("method1", json!({"arg1": "arg1data", "arg2": "arg2data"}));
        request.call_raw("method2", json!({"arg1": "arg1data"}));
        request.call_raw("method3", json!({}));
        request.set_capabilities([CORE, MAIL]);
        assert_eq!(
            serde_json::to_value(&request).expect("encodes"),
            json!({
                "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                "methodCalls": [
                    ["method1", {"arg1": "arg1data", "arg2": "arg2data"}, "c0"],
                    ["method2", {"arg1": "arg1data"}, "c1"],
                    ["method3", {}, "c2"]
                ]
            })
        );
    }

    /// The regression test for unicornops/zmail#132: a request that only
    /// uses Core and Mail names no other capability, so a server without
    /// contacts, calendars, Sieve, quota and the rest (Apache James, for
    /// one) does not reject it with `unknownCapability`.
    #[test]
    fn a_request_that_uses_core_and_mail_lists_only_core_and_mail() {
        let mut request = Request::new();
        request.call_raw("Mailbox/get", json!({"accountId": "a1"}));
        request.call_raw("Email/query", json!({"accountId": "a1"}));
        request.call_raw("Email/get", json!({"accountId": "a1", "ids": []}));
        request.call_raw("Thread/get", json!({"accountId": "a1", "ids": []}));
        request.call_raw("Email/set", json!({"accountId": "a1", "destroy": ["e1"]}));
        request.call_raw("Core/echo", json!({}));
        let sent = serde_json::to_value(&request).expect("encodes");
        assert_eq!(sent["using"], json!([CORE, MAIL]));
    }

    #[test]
    fn using_follows_the_calls_in_order_of_first_use() {
        let mut request = Request::new();
        assert_eq!(request.using(), [CORE], "the envelope alone is Core");
        request.call_raw("Identity/get", json!({}));
        assert_eq!(request.using(), [CORE, SUBMISSION]);
        request.call_raw("Email/get", json!({}));
        request.call_raw("EmailSubmission/set", json!({}));
        assert_eq!(request.using(), [CORE, SUBMISSION, MAIL]);

        // A method this crate has no table entry for adds nothing by
        // itself; the caller says what it needs, once.
        request.call_raw("Calendar/get", json!({}));
        assert_eq!(request.using(), [CORE, SUBMISSION, MAIL]);
        request
            .add_capability("urn:ietf:params:jmap:calendars")
            .add_capability("urn:ietf:params:jmap:calendars")
            .add_capability(MAIL);
        assert_eq!(
            request.using(),
            [CORE, SUBMISSION, MAIL, "urn:ietf:params:jmap:calendars"]
        );
    }

    #[test]
    fn using_can_be_replaced_outright() {
        let mut request = Request::new();
        request.call_raw("Email/get", json!({}));
        request.set_capabilities(["https://example.com/apis/foobar"]);
        request.call_raw("Identity/get", json!({}));
        request.add_capability(MAIL);
        assert_eq!(request.using(), ["https://example.com/apis/foobar"]);
    }

    /// The request of RFC 8620 section 3.7's first example: `Foo/get`
    /// takes its ids from `Foo/changes`.
    #[test]
    fn a_result_reference_is_written_as_rfc_8620_section_3_7_has_it() {
        let mut request = Request::new();
        let changes = request.call_raw(
            "Foo/changes",
            json!({"accountId": "A1", "sinceState": "abcdef"}),
        );
        let reference = changes.result_reference("/created");
        assert_eq!(
            reference,
            ResultReference {
                result_of: "c0".to_owned(),
                name: "Foo/changes".to_owned(),
                path: "/created".to_owned(),
            }
        );
        request.call_raw("Foo/get", json!({"accountId": "A1", "#ids": reference}));
        assert_eq!(
            serde_json::to_value(request.method_calls()).expect("encodes"),
            json!([
                ["Foo/changes", {"accountId": "A1", "sinceState": "abcdef"}, "c0"],
                ["Foo/get", {
                    "accountId": "A1",
                    "#ids": {"resultOf": "c0", "name": "Foo/changes", "path": "/created"}
                }, "c1"]
            ])
        );
    }

    #[test]
    fn created_ids_are_sent_only_when_given() {
        let mut request = Request::new();
        request.call_raw("Core/echo", json!({}));
        assert!(
            serde_json::to_value(&request)
                .expect("encodes")
                .get("createdIds")
                .is_none()
        );
        request.set_created_ids(BTreeMap::from([("k1".to_owned(), "M17".to_owned())]));
        assert_eq!(
            serde_json::to_value(&request).expect("encodes")["createdIds"],
            json!({"k1": "M17"})
        );
    }

    #[test]
    fn an_invocation_must_have_exactly_three_elements() {
        let invocation: Invocation =
            serde_json::from_value(json!(["error", {"type": "unknownMethod"}, "c3"]))
                .expect("decodes");
        assert_eq!(invocation.name, "error");
        assert_eq!(invocation.call_id, "c3");
        assert!(serde_json::from_value::<Invocation>(json!(["a", {}])).is_err());
        assert!(serde_json::from_value::<Invocation>(json!(["a", {}, "c", 1])).is_err());
        assert!(serde_json::from_value::<Invocation>(json!({"name": "a"})).is_err());
    }
}
