//! The sync checkpoint a Proton account stores between syncs.
//!
//! It names the event to read the account's changes from and, while older
//! mail is still being fetched, the cursor of that backfill: the oldest
//! message listed so far. Zmail 0.17.0 and earlier stored the bare event
//! ID, which still reads as "events from this ID, all history fetched", so
//! existing accounts carry on without a resync.
//!
//! It also keeps each mirrored label's mailbox name by label ID, as Bridge
//! keeps its label map, so a renamed or moved label renames its mailbox
//! instead of losing its mail. Checkpoints from before 0.33.1 have no map;
//! the next sync that reads the labels fills it in.
//!
//! While older mail arrives it also keeps what the last stretch cost: its
//! requests, any rate limiting, the write and the time since the stretch
//! before (#478), for the account's sync details. Older Zmail ignores it.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use zmail_proton_api::RequestTally;

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
    /// Each mirrored label's mailbox name, by label ID, as of the last
    /// sync that read the labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The history is being listed again because Proton asked for a full
    /// resync: the backfill rewrites each message it lists in place
    /// (correcting where it is) instead of only adding mail.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relist: bool,
    /// What the last stretch of older mail cost, while the backfill runs.
    /// Only shown: one that does not read is left out rather than making
    /// the checkpoint unusable.
    #[serde(
        default,
        rename = "stretch",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient"
    )]
    pub last_stretch: Option<StretchCost>,
}

/// What one stretch of older mail cost: the sync that fetched and wrote
/// it (#478).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StretchCost {
    /// Messages listed.
    pub messages: usize,
    /// The whole sync, from the event poll to the stored checkpoint, in
    /// milliseconds.
    pub total_ms: u64,
    /// The event poll the sync starts with.
    pub events: RequestCost,
    /// The addresses and labels the stretch places its messages with.
    pub lookups: RequestCost,
    /// The pages of message metadata.
    pub pages: RequestCost,
    /// Writing the stretch into the mirror, in milliseconds.
    pub write_ms: u64,
    /// When the stretch was written, in milliseconds since the Unix
    /// epoch, to time the next one from.
    pub ended_at_ms: u64,
    /// From the end of the stretch before to the start of this one (the
    /// pause, and whatever else the sync service did), in milliseconds;
    /// `None` when no stretch ended shortly before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_previous_ms: Option<u64>,
}

impl StretchCost {
    /// Longest gap between two stretches still counted as the download
    /// carrying on, rather than Zmail having been closed in between.
    pub const MAX_GAP_MS: u64 = 10 * 60 * 1000;

    /// The download rate this stretch shows, in messages a minute: its
    /// messages over its own time and the gap before it. `None` without
    /// a stretch just before.
    pub fn messages_per_minute(&self) -> Option<u64> {
        let cycle = self.since_previous_ms?.saturating_add(self.total_ms).max(1);
        u64::try_from(self.messages)
            .ok()
            .map(|messages| messages.saturating_mul(60_000) / cycle)
    }

    /// The time since `previous` ended, at `started_at_ms`, when it ended
    /// at most [`Self::MAX_GAP_MS`] before.
    pub fn gap_after(previous: Option<&Self>, started_at_ms: u64) -> Option<u64> {
        let gap = started_at_ms.checked_sub(previous?.ended_at_ms)?;
        (gap <= Self::MAX_GAP_MS).then_some(gap)
    }

    /// All the requests of the stretch.
    pub fn requests(&self) -> RequestCost {
        let mut all = self.events;
        for phase in [self.lookups, self.pages] {
            all.requests = all.requests.saturating_add(phase.requests);
            all.rate_limited = all.rate_limited.saturating_add(phase.rate_limited);
            all.ms = all.ms.saturating_add(phase.ms);
            all.waited_ms = all.waited_ms.saturating_add(phase.waited_ms);
        }
        all
    }
}

/// The requests of one part of a stretch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestCost {
    /// Requests made (a retried request counts once).
    pub requests: u32,
    /// Answers that were rate limiting (429) or Proton unavailable (503).
    pub rate_limited: u32,
    /// How long the part took, waits included, in milliseconds.
    pub ms: u64,
    /// Time spent waiting to retry, in milliseconds.
    pub waited_ms: u64,
}

impl From<RequestTally> for RequestCost {
    fn from(tally: RequestTally) -> Self {
        Self {
            requests: tally.requests,
            rate_limited: tally.rate_limited,
            ms: millis(tally.elapsed),
            waited_ms: millis(tally.retry_wait),
        }
    }
}

/// Whole milliseconds, saturating.
pub(crate) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// A stretch cost that does not read is none.
fn lenient<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<StretchCost>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
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
    fn keeps_the_label_map_and_the_relist_flag() {
        let checkpoint = ProtonCheckpoint {
            event_id: "ev".to_owned(),
            backfill_end_id: Some("m9".to_owned()),
            listed: 512,
            total: Some(2_000),
            labels: BTreeMap::from([("0".to_owned(), "INBOX".to_owned())]),
            relist: true,
            last_stretch: None,
        };
        assert_eq!(
            ProtonCheckpoint::parse(&checkpoint.to_json()),
            Some(checkpoint)
        );
        // Older checkpoints have neither.
        let older = ProtonCheckpoint::parse(r#"{"event_id":"ev","listed":3}"#).expect("parses");
        assert!(older.labels.is_empty());
        assert!(!older.relist);
        assert!(!older.to_json().contains("relist"));
    }

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
            ..ProtonCheckpoint::default()
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

    // Issue #478: the last stretch's cost rides along, and one that does
    // not read leaves the rest of the checkpoint usable.
    #[test]
    fn keeps_the_last_stretch_cost() {
        let stretch = StretchCost {
            messages: 1_016,
            total_ms: 4_300,
            events: RequestCost {
                requests: 1,
                rate_limited: 0,
                ms: 300,
                waited_ms: 0,
            },
            lookups: RequestCost {
                requests: 2,
                rate_limited: 1,
                ms: 12_500,
                waited_ms: 12_000,
            },
            pages: RequestCost {
                requests: 8,
                rate_limited: 0,
                ms: 3_300,
                waited_ms: 0,
            },
            write_ms: 200,
            ended_at_ms: 1_800_000_000_000,
            since_previous_ms: Some(1_700),
        };
        let checkpoint = ProtonCheckpoint {
            event_id: "e1".to_owned(),
            backfill_end_id: Some("m9".to_owned()),
            last_stretch: Some(stretch),
            ..ProtonCheckpoint::default()
        };
        assert_eq!(
            ProtonCheckpoint::parse(&checkpoint.to_json()),
            Some(checkpoint)
        );
        let all = stretch.requests();
        assert_eq!((all.requests, all.rate_limited), (11, 1));
        assert_eq!((all.ms, all.waited_ms), (16_100, 12_000));
        // 1,016 messages in 1.7 s + 4.3 s.
        assert_eq!(stretch.messages_per_minute(), Some(10_160));
        assert_eq!(
            StretchCost {
                since_previous_ms: None,
                ..stretch
            }
            .messages_per_minute(),
            None
        );

        let odd =
            ProtonCheckpoint::parse(r#"{"event_id":"e1","backfill_end_id":"m9","stretch":"x"}"#)
                .expect("still a checkpoint");
        assert_eq!(odd.backfill_end_id.as_deref(), Some("m9"));
        assert_eq!(odd.last_stretch, None);
    }

    #[test]
    fn a_gap_counts_only_after_a_recent_stretch() {
        let previous = StretchCost {
            ended_at_ms: 10_000,
            ..StretchCost::default()
        };
        assert_eq!(StretchCost::gap_after(Some(&previous), 11_500), Some(1_500));
        assert_eq!(StretchCost::gap_after(None, 11_500), None);
        // A clock that went back, and Zmail closed for an hour.
        assert_eq!(StretchCost::gap_after(Some(&previous), 9_000), None);
        assert_eq!(
            StretchCost::gap_after(Some(&previous), 10_000 + 3_600_000),
            None
        );
    }

    #[test]
    fn unusable_json_syncs_from_scratch() {
        assert_eq!(ProtonCheckpoint::parse("{not json"), None);
        assert_eq!(ProtonCheckpoint::parse(r#"{"event_id":""}"#), None);
    }
}
