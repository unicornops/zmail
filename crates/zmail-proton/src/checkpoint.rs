//! The sync checkpoint a Proton account stores between syncs.
//!
//! It names the event to read the account's changes from and, while older
//! mail is still being fetched, the cursor of that backfill: the oldest
//! message listed so far. Zmail 0.17.0 and earlier stored the bare event
//! ID, which still reads as "events from this ID, all history fetched", so
//! existing accounts carry on without a resync.

use serde::{Deserialize, Serialize};

/// Where a Proton account's sync carries on from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtonCheckpoint {
    /// The event to read the account's changes from.
    pub event_id: String,
    /// The oldest message listed so far, while older mail is still to be
    /// fetched; `None` once the whole history is in the mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill_end_id: Option<String>,
    /// How many messages the first sync and the backfill have listed, for
    /// the progress text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub listed: usize,
    /// Proton's message count when the first sync started, for the
    /// progress text; `None` when it is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
}

impl ProtonCheckpoint {
    /// Reads a stored checkpoint: the JSON form, or a bare event ID from
    /// Zmail 0.17.0 and earlier. Returns `None` when there is nothing
    /// usable, so the account syncs from scratch.
    pub fn parse(stored: &str) -> Option<Self> {
        let stored = stored.trim();
        if stored.is_empty() {
            return None;
        }
        if !stored.starts_with('{') {
            // Proton event IDs are URL-safe base64, never JSON.
            return Some(Self {
                event_id: stored.to_owned(),
                ..Self::default()
            });
        }
        let mut checkpoint: Self = serde_json::from_str(stored).ok()?;
        if checkpoint.backfill_end_id.as_deref() == Some("") {
            checkpoint.backfill_end_id = None;
        }
        (!checkpoint.event_id.is_empty()).then_some(checkpoint)
    }

    /// The form stored in the sync state.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a Proton checkpoint always serialises")
    }

    /// Whether older mail is still to be fetched.
    pub fn backfill_pending(&self) -> bool {
        self.backfill_end_id.is_some()
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_bare_event_id_as_complete_history() {
        let checkpoint = ProtonCheckpoint::parse("ACXDmTaBub14w==").expect("checkpoint");
        assert_eq!(checkpoint.event_id, "ACXDmTaBub14w==");
        assert!(!checkpoint.backfill_pending());
        assert_eq!(ProtonCheckpoint::parse("  "), None);
        assert_eq!(ProtonCheckpoint::parse(""), None);
    }

    #[test]
    fn round_trips_the_json_form() {
        let pending = ProtonCheckpoint {
            event_id: "e1".to_owned(),
            backfill_end_id: Some("m512".to_owned()),
            listed: 512,
            total: Some(85_000),
        };
        let json = pending.to_json();
        assert_eq!(
            json,
            r#"{"event_id":"e1","backfill_end_id":"m512","listed":512,"total":85000}"#
        );
        assert_eq!(ProtonCheckpoint::parse(&json), Some(pending));

        let complete = ProtonCheckpoint {
            event_id: "e2".to_owned(),
            ..ProtonCheckpoint::default()
        };
        assert_eq!(complete.to_json(), r#"{"event_id":"e2"}"#);
        assert_eq!(ProtonCheckpoint::parse(&complete.to_json()), Some(complete));
    }

    #[test]
    fn an_empty_cursor_means_complete_history() {
        let checkpoint = ProtonCheckpoint::parse(r#"{"event_id":"e1","backfill_end_id":""}"#)
            .expect("checkpoint");
        assert!(!checkpoint.backfill_pending());
    }

    #[test]
    fn unusable_json_syncs_from_scratch() {
        assert_eq!(ProtonCheckpoint::parse("{not json"), None);
        assert_eq!(ProtonCheckpoint::parse(r#"{"event_id":""}"#), None);
    }
}
