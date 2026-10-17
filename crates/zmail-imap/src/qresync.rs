//! Quick mailbox resynchronization (RFC 7162 QRESYNC).
//!
//! With QRESYNC enabled, opening a mailbox with the stored UIDVALIDITY and
//! HIGHESTMODSEQ makes the server report, in the same round trip, every
//! message expunged since then (`VANISHED (EARLIER)`) and every message
//! whose flags changed (`FETCH` with `MODSEQ`). That replaces both the full
//! `UID FETCH 1:* (FLAGS)` scan CONDSTORE needs after an expunge and the
//! separate `CHANGEDSINCE` fetch.

use std::{collections::BTreeMap, ops::RangeInclusive};

use anyhow::{Context, Result, anyhow, bail};
use async_imap::{
    Session,
    imap_proto::{AttributeValue, MailboxDatum, Response, ResponseCode, Status},
    types::{Flag, Mailbox},
};

use crate::{DynImapStream, flag_name};

/// Longest `known-uids` set sent with the SELECT; a mailbox so fragmented
/// that its set is longer omits it, and the server reports vanished UIDs
/// without the filter instead.
const MAX_KNOWN_UIDS_LEN: usize = 16 * 1024;

/// What a `SELECT ... (QRESYNC ...)` reported.
#[derive(Debug, Default)]
pub(crate) struct QresyncDelta {
    /// Mailbox status from the SELECT, like `Session::select` returns.
    pub(crate) status: Mailbox,
    /// UID ranges expunged since the stored mod-sequence.
    pub(crate) vanished: Vec<RangeInclusive<u32>>,
    /// Current flags of every message changed since the stored
    /// mod-sequence, new messages included.
    pub(crate) changed: BTreeMap<u32, Vec<String>>,
}

/// Enables QRESYNC for the session (RFC 7162 section 3.2.3).
///
/// Returns `false` when the server refuses, so the caller keeps the
/// CONDSTORE path.
pub(crate) async fn enable(session: &mut Session<DynImapStream>) -> bool {
    session
        .run_command_and_check_ok("ENABLE QRESYNC")
        .await
        .is_ok()
}

/// Opens `mailbox` read-only with the stored UIDVALIDITY and mod-sequence
/// and collects what changed since.
///
/// When the returned status carries a different UIDVALIDITY the server
/// reported nothing (the stored state no longer applies) and the caller
/// must refresh the mailbox in full.
pub(crate) async fn examine(
    session: &mut Session<DynImapStream>,
    mailbox: &str,
    uid_validity: u32,
    modseq: u64,
    known_uids: Option<&str>,
) -> Result<QresyncDelta> {
    let known = known_uids
        .filter(|set| !set.is_empty() && set.len() <= MAX_KNOWN_UIDS_LEN)
        .map(|set| format!(" {set}"))
        .unwrap_or_default();
    let command = format!(
        "EXAMINE {} (QRESYNC ({uid_validity} {modseq}{known}))",
        quote_mailbox(mailbox)?
    );
    let tag = session
        .run_command(&command)
        .await
        .with_context(|| format!("failed to send the QRESYNC EXAMINE for {mailbox}"))?;

    // The responses are read here rather than through async-imap's
    // unsolicited channel, which holds at most 100 entries and silently
    // drops the rest; a QRESYNC open can report thousands of changes.
    let mut delta = QresyncDelta::default();
    loop {
        let response = session
            .read_response()
            .await
            .with_context(|| format!("failed to read the QRESYNC EXAMINE for {mailbox}"))?
            .ok_or_else(|| anyhow!("the IMAP server closed the connection during EXAMINE"))?;
        if apply_response(&mut delta, response.parsed(), &tag.0)? {
            return Ok(delta);
        }
    }
}

/// Folds one response of the QRESYNC open into `delta`, returning `true`
/// once the tagged completion arrives.
fn apply_response(delta: &mut QresyncDelta, response: &Response<'_>, tag: &str) -> Result<bool> {
    match response {
        Response::Done {
            tag: done_tag,
            status,
            information,
            ..
        } if done_tag.0 == tag => match status {
            Status::Ok => return Ok(true),
            _ => bail!(
                "the IMAP server rejected the QRESYNC EXAMINE: {}",
                information.as_deref().unwrap_or("no detail")
            ),
        },
        Response::Data {
            status: Status::Ok,
            code: Some(code),
            ..
        } => match code {
            ResponseCode::UidValidity(value) => delta.status.uid_validity = Some(*value),
            ResponseCode::UidNext(value) => delta.status.uid_next = Some(*value),
            ResponseCode::HighestModSeq(value) => delta.status.highest_modseq = Some(*value),
            ResponseCode::Unseen(value) => delta.status.unseen = Some(*value),
            _ => {}
        },
        Response::MailboxData(MailboxDatum::Exists(count)) => delta.status.exists = *count,
        Response::MailboxData(MailboxDatum::Recent(count)) => delta.status.recent = *count,
        Response::Vanished { uids, .. } => delta.vanished.extend(uids.iter().cloned()),
        Response::Fetch(_, attributes) => {
            let mut uid = None;
            let mut flags = None;
            for attribute in attributes {
                match attribute {
                    AttributeValue::Uid(value) => uid = Some(*value),
                    AttributeValue::Flags(values) => {
                        flags = Some(
                            values
                                .iter()
                                .map(|value| flag_name(&Flag::from(value.as_ref())))
                                .collect::<Vec<_>>(),
                        );
                    }
                    _ => {}
                }
            }
            // QRESYNC FETCH responses always carry the UID (RFC 7162
            // section 3.2.4); one without flags changed nothing we track.
            if let (Some(uid), Some(flags)) = (uid, flags) {
                delta.changed.insert(uid, flags);
            }
        }
        _ => {}
    }
    Ok(false)
}

/// Applies a QRESYNC delta to the flag map recorded by the last sync: the
/// vanished UIDs leave, the changed ones take their new flags.
pub(crate) fn apply_delta(
    previous: &BTreeMap<u32, Vec<String>>,
    vanished: &[RangeInclusive<u32>],
    changed: &BTreeMap<u32, Vec<String>>,
) -> BTreeMap<u32, Vec<String>> {
    let mut flags = previous.clone();
    for range in vanished {
        // Ranges may be huge (servers without expunge history report
        // everything they cannot rule out), so only known UIDs are visited.
        let gone = flags
            .range(range.clone())
            .map(|(uid, _)| *uid)
            .collect::<Vec<_>>();
        for uid in gone {
            flags.remove(&uid);
        }
    }
    for (uid, value) in changed {
        // A UID can be reported both changed and vanished when it was
        // flagged and then expunged; the expunge wins.
        if !vanished.iter().any(|range| range.contains(uid)) {
            flags.insert(*uid, value.clone());
        }
    }
    flags
}

/// Encodes ascending UIDs as a compact IMAP sequence set (`1:3,7,9:12`).
pub(crate) fn uid_set<I>(uids: I) -> String
where
    I: IntoIterator<Item = u32>,
{
    let mut set = String::new();
    let mut range: Option<(u32, u32)> = None;
    let push = |set: &mut String, (start, end): (u32, u32)| {
        if !set.is_empty() {
            set.push(',');
        }
        if start == end {
            set.push_str(&start.to_string());
        } else {
            set.push_str(&format!("{start}:{end}"));
        }
    };
    for uid in uids {
        range = match range {
            Some((start, end)) if end.checked_add(1) == Some(uid) => Some((start, uid)),
            Some(previous) => {
                push(&mut set, previous);
                Some((uid, uid))
            }
            None => Some((uid, uid)),
        };
    }
    if let Some(last) = range {
        push(&mut set, last);
    }
    set
}

/// Quotes a mailbox name the way async-imap does for its own commands.
fn quote_mailbox(name: &str) -> Result<String> {
    if name.contains(['\r', '\n']) {
        bail!("IMAP mailbox names must not contain line breaks");
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::imap_proto::parser::parse_response;

    fn parse(line: &str) -> Response<'_> {
        let (rest, response) = parse_response(line.as_bytes()).expect("valid IMAP response");
        assert!(rest.is_empty());
        response
    }

    fn flags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn collects_status_vanished_and_changed_flags() {
        let mut delta = QresyncDelta::default();
        for line in [
            "* 12 EXISTS\r\n",
            "* OK [UIDVALIDITY 7] UIDs valid\r\n",
            "* OK [UIDNEXT 40] Predicted next UID\r\n",
            "* OK [HIGHESTMODSEQ 90] Highest\r\n",
            "* OK [CLOSED] Previous mailbox closed\r\n",
            "* VANISHED (EARLIER) 3:5,9\r\n",
            "* 2 FETCH (UID 4 FLAGS (\\Seen) MODSEQ (88))\r\n",
            "* 6 FETCH (UID 20 FLAGS (\\Seen \\Flagged $Label) MODSEQ (89))\r\n",
        ] {
            assert!(!apply_response(&mut delta, &parse(line), "A7").expect("untagged"));
        }
        assert!(
            apply_response(
                &mut delta,
                &parse("A7 OK [READ-ONLY] EXAMINE completed\r\n"),
                "A7"
            )
            .expect("tagged OK")
        );

        assert_eq!(delta.status.exists, 12);
        assert_eq!(delta.status.uid_validity, Some(7));
        assert_eq!(delta.status.uid_next, Some(40));
        assert_eq!(delta.status.highest_modseq, Some(90));
        assert_eq!(delta.vanished, vec![3..=5, 9..=9]);
        assert_eq!(delta.changed[&4], flags(&["\\Seen"]));
        assert_eq!(
            delta.changed[&20],
            flags(&["\\Seen", "\\Flagged", "$Label"])
        );
    }

    #[test]
    fn other_tags_do_not_end_the_open_and_rejections_fail() {
        let mut delta = QresyncDelta::default();
        assert!(!apply_response(&mut delta, &parse("A6 OK done\r\n"), "A7").expect("other tag"));
        assert!(apply_response(&mut delta, &parse("A7 NO [CANNOT] nope\r\n"), "A7").is_err());
    }

    #[test]
    fn deltas_drop_vanished_uids_and_take_changed_flags() {
        let previous = BTreeMap::from([
            (1, flags(&[])),
            (3, flags(&["\\Seen"])),
            (4, flags(&[])),
            (8, flags(&[])),
        ]);
        let changed = BTreeMap::from([
            (4, flags(&["\\Flagged"])),
            (8, flags(&["\\Seen"])),
            (10, flags(&[])),
        ]);
        let merged = apply_delta(&previous, &[2..=3, 8..=8], &changed);
        assert_eq!(
            merged,
            BTreeMap::from([
                (1, flags(&[])),
                (4, flags(&["\\Flagged"])),
                (10, flags(&[]))
            ]),
            "vanished UIDs leave even when also reported changed; new UIDs join"
        );
    }

    #[test]
    fn huge_vanished_ranges_only_touch_known_uids() {
        let previous = BTreeMap::from([(5, flags(&[])), (4_000_000_000, flags(&[]))]);
        let everything = std::iter::once(1..=u32::MAX - 1).collect::<Vec<_>>();
        let merged = apply_delta(&previous, &everything, &BTreeMap::new());
        assert!(merged.is_empty());
    }

    #[test]
    fn uid_sets_compress_runs() {
        assert_eq!(uid_set([1, 2, 3, 7, 9, 10, 11, 12]), "1:3,7,9:12");
        assert_eq!(uid_set([5]), "5");
        assert_eq!(uid_set(Vec::<u32>::new()), "");
        assert_eq!(uid_set([u32::MAX - 1, u32::MAX]), "4294967294:4294967295");
    }

    #[test]
    fn mailbox_names_are_quoted() {
        assert_eq!(
            quote_mailbox(r#"Work "A"\B"#).expect("valid"),
            r#""Work \"A\"\\B""#
        );
        assert!(quote_mailbox("bad\r\nname").is_err());
    }
}
