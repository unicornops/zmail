// Ported from go-proton-api `event.go` (`GetLatestEventID`, `GetEvent`,
// `getEvent`, `maxCollectedEvents`) and `event_types.go` (`Event`,
// `RefreshFlag`, `EventAction`, the message, label and address events) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. The event stream
// ticker (`NewEventStream`) lives in Zmail's sync runtime; settings,
// notification and space events are kept only as "something changed".

//! The account's event stream.

use serde::Deserialize;

use crate::{
    address::Address, boolean::api_bool, client::Client, label::Label, manager::Request,
    message::MessageMetadata, response::Error,
};

/// The most events `get_events` collects in one call (`maxCollectedEvents`).
pub const MAX_COLLECTED_EVENTS: usize = 50;

/// What happened to an item (go-proton-api's `EventAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "i64")]
pub enum EventAction {
    /// The item was deleted.
    Delete,
    /// The item was created.
    Create,
    /// The item changed.
    Update,
    /// Only the item's flags changed.
    UpdateFlags,
    /// An action this client does not know.
    Other(i64),
}

impl From<i64> for EventAction {
    fn from(value: i64) -> Self {
        match value {
            0 => Self::Delete,
            1 => Self::Create,
            2 => Self::Update,
            3 => Self::UpdateFlags,
            other => Self::Other(other),
        }
    }
}

/// A message change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MessageEvent {
    /// The message ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// What happened.
    #[serde(rename = "Action")]
    pub action: EventAction,
    /// The message's metadata after the change (absent on delete).
    #[serde(rename = "Message", default)]
    pub message: Option<MessageMetadata>,
}

/// A label change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LabelEvent {
    /// The label ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// What happened.
    #[serde(rename = "Action")]
    pub action: EventAction,
    /// The label after the change (absent on delete).
    #[serde(rename = "Label", default)]
    pub label: Option<Label>,
}

/// An address change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AddressEvent {
    /// The address ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// What happened.
    #[serde(rename = "Action")]
    pub action: EventAction,
    /// The address after the change (absent on delete).
    #[serde(rename = "Address", default)]
    pub address: Option<Address>,
}

/// One event (go-proton-api's `Event`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Event {
    /// The event ID; the next poll continues from it.
    #[serde(rename = "EventID")]
    pub event_id: String,
    /// Refresh flags: the client must resync (`RefreshMail` is bit 1).
    #[serde(rename = "Refresh", default)]
    pub refresh: u8,
    /// Message changes.
    #[serde(rename = "Messages", default)]
    pub messages: Option<Vec<MessageEvent>>,
    /// Label changes.
    #[serde(rename = "Labels", default)]
    pub labels: Option<Vec<LabelEvent>>,
    /// Address changes.
    #[serde(rename = "Addresses", default)]
    pub addresses: Option<Vec<AddressEvent>>,
    /// Set when the user (keys, name) changed.
    #[serde(rename = "User", default)]
    pub user: Option<serde_json::Value>,
}

impl Event {
    /// Refresh bit for mail (`RefreshMail`).
    pub const REFRESH_MAIL: u8 = 1;

    /// Whether the server asks for a full mail resync.
    pub fn needs_mail_refresh(&self) -> bool {
        self.refresh & Self::REFRESH_MAIL != 0
    }

    /// The message changes, if any.
    pub fn message_events(&self) -> &[MessageEvent] {
        self.messages.as_deref().unwrap_or_default()
    }

    /// The label changes, if any.
    pub fn label_events(&self) -> &[LabelEvent] {
        self.labels.as_deref().unwrap_or_default()
    }

    /// The address changes, if any.
    pub fn address_events(&self) -> &[AddressEvent] {
        self.addresses.as_deref().unwrap_or_default()
    }

    /// Whether the event changes anything Zmail mirrors.
    pub fn has_changes(&self) -> bool {
        self.refresh != 0
            || self.user.is_some()
            || !self.message_events().is_empty()
            || !self.label_events().is_empty()
            || !self.address_events().is_empty()
    }
}

impl Client {
    /// Returns the ID of the newest event (`GetLatestEventID`); syncing from
    /// it later replays everything that happened after this call.
    pub async fn get_latest_event_id(&self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "EventID")]
            event_id: String,
        }
        let res: Res = self.json(&Request::get("/core/v4/events/latest")).await?;
        Ok(res.event_id)
    }

    /// Returns the events after `event_id`, following `More` up to
    /// [`MAX_COLLECTED_EVENTS`], and whether more remain (`GetEvent`).
    pub async fn get_events(&self, event_id: &str) -> Result<(Vec<Event>, bool), Error> {
        let (event, mut more) = self.get_event(event_id).await?;
        let mut events = vec![event];
        while more && events.len() < MAX_COLLECTED_EVENTS {
            let last = events
                .last()
                .map(|event| event.event_id.clone())
                .unwrap_or_default();
            let (event, next) = self.get_event(&last).await?;
            events.push(event);
            more = next;
        }
        Ok((events, more))
    }

    async fn get_event(&self, event_id: &str) -> Result<(Event, bool), Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(flatten)]
            event: Event,
            #[serde(rename = "More", default, deserialize_with = "api_bool")]
            more: bool,
        }
        let res: Res = self
            .json(&Request::get(format!("/core/v4/events/{event_id}")))
            .await?;
        Ok((res.event, res.more))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_events() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "EventID": "e2",
            "Refresh": 0,
            "Messages": [
                {"ID": "m1", "Action": 3, "Message": {"ID": "m1", "LabelIDs": ["0"], "Unread": 0}},
                {"ID": "m2", "Action": 0},
            ],
            "Labels": null,
            "UsedSpace": 10,
        }))
        .expect("event");
        assert!(event.has_changes());
        assert!(!event.needs_mail_refresh());
        assert_eq!(event.message_events()[0].action, EventAction::UpdateFlags);
        assert_eq!(event.message_events()[1].action, EventAction::Delete);
        assert!(event.message_events()[1].message.is_none());
        assert!(event.label_events().is_empty());

        let quiet: Event =
            serde_json::from_value(serde_json::json!({"EventID": "e3"})).expect("quiet");
        assert!(!quiet.has_changes());
        let refresh: Event =
            serde_json::from_value(serde_json::json!({"EventID": "e4", "Refresh": 255}))
                .expect("refresh");
        assert!(refresh.needs_mail_refresh());
    }
}
