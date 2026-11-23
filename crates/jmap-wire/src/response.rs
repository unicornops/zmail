//! The response envelope (RFC 8620 section 3.4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    error::{Error, MethodError},
    method::Method,
    request::{Call, Invocation},
};

/// A JMAP response (RFC 8620 section 3.4): the answers to a request's
/// method calls.
///
/// A call may have several answers (an `EmailSubmission/set` with
/// `onSuccessUpdateEmail` is followed by an `Email/set` answer with the
/// same call id, RFC 8621 section 7.5), and a call that failed has an
/// `error` answer (RFC 8620 section 3.6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Response {
    /// Every answer, in the order the server gave them.
    pub method_responses: Vec<Invocation>,
    /// Creation ids and the ids the server gave them; present only when
    /// the request had `createdIds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ids: Option<BTreeMap<String, String>>,
    /// The session's `state` as of this response; when it differs from
    /// the session's, the session has changed and should be fetched
    /// again. Empty when the server sent none.
    #[serde(default)]
    pub session_state: String,
}

impl Response {
    /// Every answer to the call with `call_id`, in order.
    pub fn responses_to<'a>(&'a self, call_id: &'a str) -> impl Iterator<Item = &'a Invocation> {
        self.method_responses
            .iter()
            .filter(move |response| response.call_id == call_id)
    }

    /// The typed answer to `call`: its first answer, decoded.
    ///
    /// # Errors
    ///
    /// [`Error::Method`] when the server answered the call with `error`,
    /// [`Error::MissingResponse`] when it gave no answer,
    /// [`Error::UnexpectedResponse`] when the answer has another name,
    /// and [`Error::Json`] when it does not decode.
    pub fn get<M: Method>(&self, call: &Call<M>) -> Result<M::Response, Error> {
        self.arguments_as(call.call_id(), call.name())
    }

    /// The arguments of the first answer to `call`, as JSON, with the
    /// same errors as [`get`](Self::get) but for decoding. For calls
    /// made with [`Request::call_raw`](crate::Request::call_raw), and for
    /// typed calls whose answer you would rather read yourself.
    pub fn arguments<M: ?Sized>(&self, call: &Call<M>) -> Result<&Value, Error> {
        self.first(call.call_id(), call.name())
    }

    /// The arguments of the first answer to the call with `call_id`,
    /// which must be named `name`, decoded as `T`.
    pub fn arguments_as<T: DeserializeOwned>(&self, call_id: &str, name: &str) -> Result<T, Error> {
        T::deserialize(self.first(call_id, name)?).map_err(|source| Error::json(name, source))
    }

    fn first(&self, call_id: &str, name: &str) -> Result<&Value, Error> {
        let response = self
            .method_responses
            .iter()
            .find(|response| response.call_id == call_id)
            .ok_or_else(|| Error::MissingResponse {
                call_id: call_id.to_owned(),
            })?;
        if response.name == name {
            return Ok(&response.arguments);
        }
        if response.name == "error" {
            return Err(Error::Method(
                MethodError::deserialize(&response.arguments)
                    .map_err(|source| Error::json("method error", source))?,
            ));
        }
        Err(Error::UnexpectedResponse {
            expected: name.to_owned(),
            found: response.name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{error::MethodErrorType, request::Request};

    /// The example response of RFC 8620 section 3.4.1, with this crate's
    /// call ids.
    fn rfc_response() -> Response {
        serde_json::from_value(json!({
            "methodResponses": [
                ["method1", {"arg1": 3, "arg2": "foo"}, "c0"],
                ["method2", {"isBlah": true}, "c1"],
                ["anotherResponseFromMethod2", {"data": 10, "yetmoredata": "Hello"}, "c1"],
                ["error", {"type": "unknownMethod"}, "c2"]
            ],
            "sessionState": "75128aab4b1b"
        }))
        .expect("decodes")
    }

    #[test]
    fn the_response_of_rfc_8620_section_3_4_1_is_read_by_call() {
        let mut request = Request::new();
        let first = request.call_raw("method1", json!({}));
        let second = request.call_raw("method2", json!({}));
        let third = request.call_raw("method3", json!({}));
        let unanswered = request.call_raw("method4", json!({}));

        let response = rfc_response();
        assert_eq!(response.session_state, "75128aab4b1b");
        assert_eq!(response.created_ids, None);
        assert_eq!(
            response.arguments(&first).expect("answered"),
            &json!({"arg1": 3, "arg2": "foo"})
        );
        // A call with two answers: the first is the call's own.
        assert_eq!(
            response.arguments(&second).expect("answered"),
            &json!({"isBlah": true})
        );
        let names = response
            .responses_to(second.call_id())
            .map(|answer| answer.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["method2", "anotherResponseFromMethod2"]);

        // A method-level error is the call's error, not the request's.
        match response.arguments(&third) {
            Err(Error::Method(error)) => {
                assert_eq!(error.error_type, MethodErrorType::UnknownMethod)
            }
            other => panic!("expected a method error, got {other:?}"),
        }
        assert!(matches!(
            response.arguments(&unanswered),
            Err(Error::MissingResponse { call_id }) if call_id == "c3"
        ));
    }

    #[test]
    fn an_answer_with_another_name_is_not_taken_for_the_calls() {
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Email/set", {}, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        assert!(matches!(
            response.arguments_as::<Value>("c0", "EmailSubmission/set"),
            Err(Error::UnexpectedResponse { expected, found })
                if expected == "EmailSubmission/set" && found == "Email/set"
        ));
    }

    #[test]
    fn created_ids_and_a_missing_session_state_are_read() {
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [],
            "createdIds": {"k1": "M17"}
        }))
        .expect("decodes");
        assert_eq!(response.session_state, "");
        assert_eq!(
            response.created_ids,
            Some(BTreeMap::from([("k1".to_owned(), "M17".to_owned())]))
        );
        assert!(serde_json::from_value::<Response>(json!({"sessionState": "s"})).is_err());
    }
}
