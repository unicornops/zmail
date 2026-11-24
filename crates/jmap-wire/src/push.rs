//! What a server pushes (RFC 8620 section 7): the [`StateChange`]
//! object, whichever way it arrives (the event source of section 7.3,
//! the WebSocket of RFC 8887, or a push subscription's POST).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::method::null_as_default;

/// A StateChange object (RFC 8620 section 7.1): for each account in
/// which something changed, the new state of each data type that did.
///
/// It says *that* data changed, never what: compare a state with the
/// one your last `/get` or `/changes` returned, and fetch the changes
/// when they differ.
///
/// ```
/// use jmap_wire::StateChange;
///
/// // The example of RFC 8620 section 7.1.1.
/// let change = StateChange::parse(r#"{
///     "@type": "StateChange",
///     "changed": {
///         "a3123": {"Email": "d35ecb040aab", "EmailDelivery": "428d565f2440",
///                   "CalendarEvent": "87accfac587a"},
///         "a43461d": {"Mailbox": "0af7a512ce70", "CalendarEvent": "7a4297cecd76"}
///     }
/// }"#)?.expect("a StateChange");
/// assert_eq!(change.state("a3123", "Email"), Some("d35ecb040aab"));
/// assert!(change.touches("a43461d", ["Email", "Mailbox"]));
/// assert!(!change.touches("a3123", ["Mailbox"]));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StateChange {
    /// By account id, the new state of each data type that changed, by
    /// the type's name (`Email`, `Mailbox`, or one this crate has never
    /// heard of). A state that is not a string, which the RFC does not
    /// allow, is kept as its JSON text.
    #[serde(default, deserialize_with = "type_states")]
    pub changed: BTreeMap<String, BTreeMap<String, String>>,
    /// Over a WebSocket (RFC 8887 section 4.3.5.1): a token for the
    /// whole of the server's state, to hand back when enabling push on
    /// the next connection so that the server sends what was missed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_state: Option<String>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl StateChange {
    /// Reads pushed JSON: the StateChange it is, or `None` when it is
    /// an object of another `@type` (a `PushVerification`, say).
    ///
    /// # Errors
    ///
    /// When `data` is not a JSON object, or is a StateChange whose
    /// `changed` is not a map of maps.
    pub fn parse(data: &str) -> Result<Option<Self>, serde_json::Error> {
        Self::from_value(serde_json::from_str(data)?)
    }

    /// As [`parse`](Self::parse), for JSON that is already read.
    pub fn from_value(mut value: Value) -> Result<Option<Self>, serde_json::Error> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("a push object must be a JSON object"))?;
        if object.remove("@type") != Some(Value::from("StateChange")) {
            return Ok(None);
        }
        serde_json::from_value(value).map(Some)
    }

    /// The new state of data type `data_type` in `account_id`, when the
    /// change names it.
    pub fn state(&self, account_id: &str, data_type: &str) -> Option<&str> {
        self.changed
            .get(account_id)?
            .get(data_type)
            .map(String::as_str)
    }

    /// Whether any of `data_types` changed in `account_id`.
    pub fn touches<'a>(
        &self,
        account_id: &str,
        data_types: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.changed.get(account_id).is_some_and(|states| {
            data_types
                .into_iter()
                .any(|data_type| states.contains_key(data_type))
        })
    }

    /// The object as the JSON a server sends, with its `@type`.
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.insert("@type".to_owned(), Value::from("StateChange"));
        }
        value
    }
}

/// `changed`, with `null` read as empty and a state of another type
/// than a string kept as its JSON text.
fn type_states<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, D::Error> {
    let accounts: BTreeMap<String, Option<BTreeMap<String, Value>>> =
        null_as_default(deserializer)?;
    Ok(accounts
        .into_iter()
        .map(|(account, states)| {
            let states = states
                .unwrap_or_default()
                .into_iter()
                .map(|(data_type, state)| match state {
                    Value::String(state) => (data_type, state),
                    other => (data_type, other.to_string()),
                })
                .collect();
            (account, states)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn other_push_objects_are_not_state_changes() {
        assert_eq!(
            StateChange::parse(r#"{"@type":"PushVerification","pushSubscriptionId":"p"}"#)
                .expect("an object"),
            None
        );
        assert_eq!(StateChange::parse("{}").expect("an object"), None);
        assert!(StateChange::parse("not JSON").is_err());
        assert!(StateChange::parse("[]").is_err());
        assert!(StateChange::parse(r#"{"@type":"StateChange","changed":"all"}"#).is_err());
    }

    #[test]
    fn what_servers_leave_out_or_add_is_read() {
        let change = StateChange::parse(
            r#"{"@type":"StateChange","changed":{"a1":{"Email":"s2","FutureType":7},"a2":null},
                "pushState":"p9","vendor":true}"#,
        )
        .expect("an object")
        .expect("a StateChange");
        assert_eq!(change.state("a1", "Email"), Some("s2"));
        assert_eq!(change.state("a1", "FutureType"), Some("7"));
        assert!(change.changed["a2"].is_empty());
        assert_eq!(change.push_state.as_deref(), Some("p9"));
        assert_eq!(change.extra["vendor"], true);
        assert!(!change.touches("a2", ["Email"]));
        assert!(!change.touches("a3", ["Email"]));

        let bare = StateChange::parse(r#"{"@type":"StateChange","changed":null}"#)
            .expect("an object")
            .expect("a StateChange");
        assert!(bare.changed.is_empty());
        assert_eq!(
            bare.to_value(),
            json!({"@type": "StateChange", "changed": {}})
        );
    }
}
