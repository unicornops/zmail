//! Live change notifications for every mailbox of an IMAP account.
//!
//! Servers that advertise NOTIFY (RFC 5465) report changes to every
//! personal mailbox over one connection. Everywhere else a small, bounded
//! pool of IDLE (RFC 2177) connections watches the mailboxes where new mail
//! matters most (Inbox, Sent, Drafts) and one more connection polls the rest
//! with cheap `STATUS` commands. Watchers never sync: they report
//! [`WatchEvent`]s and the caller turns them into syncs, so the local store
//! stays the only thing the UI reads.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use async_imap::{
    extensions::idle::IdleResponse,
    imap_proto::{MailboxDatum, Response},
    types::NameAttribute,
};
use tokio::time::timeout;
use zmail_core::{ProtocolKind, SyncTarget};

use crate::{DynImapStream, connect_session, guess_role, list_mailboxes, parse_endpoint};

/// Most long-lived connections the watchers of one account hold.
///
/// Servers commonly cap connections per user and address (Dovecot's
/// default is 10, Gmail's 15) and a sync or body fetch opens one more for
/// its duration, so the watchers stay well below that: NOTIFY needs one,
/// the IDLE pool uses up to three plus one `STATUS` poller.
pub const MAX_WATCH_CONNECTIONS: usize = 4;
/// How often the `STATUS` poller checks the mailboxes nothing else watches.
pub const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// How long one IDLE (or NOTIFY wait) runs before it is renewed with
/// `DONE`. RFC 2177 asks for less than 29 minutes; renewing more often also
/// proves a quiet connection still works, because a dead one fails `DONE`.
pub const IDLE_RENEW_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How long the server gets to answer a `DONE`, `STATUS` or `NOTIFY`
/// before the connection counts as dead.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Progress reported by a watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// The watcher is connected. Changes made while it was not are not
    /// replayed, so the caller resyncs once on every connect.
    Connected,
    /// The server reported a change; `mailbox` names it when known.
    Changed {
        /// The mailbox that changed, or `None` when the server did not say.
        mailbox: Option<String>,
    },
}

/// How a watcher ended without a transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEnd {
    /// The server does not support the mechanism after all (it refused
    /// IDLE or NOTIFY); the caller falls back.
    Unsupported,
}

/// How the account's mailboxes are watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchPlan {
    /// One NOTIFY connection covers every personal mailbox.
    Notify,
    /// IDLE connections for `idle` and one `STATUS` poller for `polled`.
    Pool {
        /// Mailboxes each held open by their own IDLE connection.
        idle: Vec<String>,
        /// Mailboxes the `STATUS` poller checks.
        polled: Vec<String>,
    },
}

/// Lists the account's mailboxes and decides how to watch them within
/// `max_connections`.
///
/// `allow_notify` is `false` after a server advertised NOTIFY but refused
/// it, so the pool is used instead.
pub async fn plan_watch(
    target: &SyncTarget,
    secret: &str,
    max_connections: usize,
    allow_notify: bool,
) -> Result<WatchPlan> {
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let (notify, idle) = session
        .capabilities()
        .await
        .map(|capabilities| (capabilities.has_str("NOTIFY"), capabilities.has_str("IDLE")))
        .unwrap_or((false, false));
    if notify && allow_notify {
        let _ = session.logout().await;
        return Ok(WatchPlan::Notify);
    }
    let mailboxes = list_mailboxes(&mut session)
        .await?
        .into_iter()
        .map(|name| {
            let role = special_use_role(name.attributes())
                .map(str::to_owned)
                .or_else(|| guess_role(name.name()));
            (name.name().to_owned(), role)
        })
        .collect::<Vec<_>>();
    let _ = session.logout().await;
    Ok(pool_plan(&mailboxes, idle, max_connections))
}

/// Splits mailboxes between IDLE connections and the `STATUS` poller.
///
/// Inbox, Sent and Drafts get IDLE connections in that order, as many as
/// the budget allows after reserving one connection for the poller when
/// anything is left to poll. Without IDLE everything is polled.
pub(crate) fn pool_plan(
    mailboxes: &[(String, Option<String>)],
    idle_supported: bool,
    max_connections: usize,
) -> WatchPlan {
    let mut favourites = Vec::new();
    if idle_supported {
        for role in ["inbox", "sent", "drafts"] {
            let found = mailboxes.iter().find(|(name, mailbox_role)| {
                mailbox_role.as_deref() == Some(role)
                    || (role == "inbox" && name.eq_ignore_ascii_case("INBOX"))
            });
            if let Some((name, _)) = found
                && !favourites.contains(name)
            {
                favourites.push(name.clone());
            }
        }
    }
    let mut polled = mailboxes
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| !favourites.contains(name))
        .collect::<Vec<_>>();
    let idle_slots = if polled.is_empty() {
        max_connections
    } else {
        max_connections.saturating_sub(1)
    };
    if favourites.len() > idle_slots {
        // Favourites beyond the budget are polled like everything else.
        polled.splice(0..0, favourites.split_off(idle_slots));
    }
    WatchPlan::Pool {
        idle: favourites,
        polled,
    }
}

/// The role a SPECIAL-USE (RFC 6154) attribute names.
fn special_use_role(attributes: &[NameAttribute<'_>]) -> Option<&'static str> {
    attributes.iter().find_map(|attribute| match attribute {
        NameAttribute::Sent => Some("sent"),
        NameAttribute::Drafts => Some("drafts"),
        NameAttribute::Archive | NameAttribute::All => Some("archive"),
        NameAttribute::Junk => Some("junk"),
        NameAttribute::Trash => Some("trash"),
        _ => None,
    })
}

/// Holds one IDLE connection on `mailbox`, reporting every change, until
/// the connection fails.
///
/// The IDLE is renewed every [`IDLE_RENEW_INTERVAL`]; a renewal the server
/// does not answer in time ends the watcher with an error so the caller
/// reconnects. Returns `Ok(WatchEnd::Unsupported)` when the server refuses
/// IDLE.
pub async fn watch_idle<F>(
    target: &SyncTarget,
    secret: &str,
    mailbox: &str,
    mut on_event: F,
) -> Result<WatchEnd>
where
    F: FnMut(WatchEvent) + Send,
{
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    session
        .examine(mailbox)
        .await
        .with_context(|| format!("failed to open {mailbox} for IDLE"))?;
    let mut connected = false;
    loop {
        let mut idle = session.idle();
        if idle.init().await.is_err() {
            return Ok(WatchEnd::Unsupported);
        }
        if !connected {
            connected = true;
            on_event(WatchEvent::Connected);
        }
        idle_until_renewal(&mut idle, |_| {
            on_event(WatchEvent::Changed {
                mailbox: Some(mailbox.to_owned()),
            });
        })
        .await?;
        session = timeout(COMMAND_TIMEOUT, idle.done())
            .await
            .map_err(|_| anyhow!("the IMAP server did not answer DONE on {mailbox}"))?
            .with_context(|| format!("failed to renew the IDLE on {mailbox}"))?;
    }
}

/// Holds one NOTIFY connection (RFC 5465) for every personal mailbox,
/// reporting the mailbox each change names, until the connection fails.
///
/// Returns `Ok(WatchEnd::Unsupported)` when the server refuses the NOTIFY
/// request or IDLE, which is used to wait for the notifications.
pub async fn watch_notify<F>(target: &SyncTarget, secret: &str, mut on_event: F) -> Result<WatchEnd>
where
    F: FnMut(WatchEvent) + Send,
{
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    // Flag changes in other mailboxes are optional for servers (RFC 5465
    // section 5.4); a server that rejects them still reports new and
    // expunged messages.
    let mut accepted = false;
    for request in [
        "NOTIFY SET (personal (MessageNew MessageExpunge FlagChange MailboxName))",
        "NOTIFY SET (personal (MessageNew MessageExpunge MailboxName))",
    ] {
        match timeout(COMMAND_TIMEOUT, session.run_command_and_check_ok(request)).await {
            Ok(Ok(())) => {
                accepted = true;
                break;
            }
            Ok(Err(_)) => {}
            Err(_) => bail!("the IMAP server did not answer NOTIFY"),
        }
    }
    if !accepted {
        return Ok(WatchEnd::Unsupported);
    }

    let mut connected = false;
    loop {
        // No mailbox is selected; IDLE only keeps the connection reading
        // the notifications the server sends.
        let mut idle = session.idle();
        if idle.init().await.is_err() {
            return Ok(WatchEnd::Unsupported);
        }
        if !connected {
            connected = true;
            on_event(WatchEvent::Connected);
        }
        idle_until_renewal(&mut idle, |response| {
            on_event(WatchEvent::Changed {
                mailbox: notified_mailbox(response),
            });
        })
        .await?;
        session = timeout(COMMAND_TIMEOUT, idle.done())
            .await
            .map_err(|_| anyhow!("the IMAP server did not answer DONE"))?
            .context("failed to renew the NOTIFY wait")?;
    }
}

/// Polls `mailboxes` with `STATUS` every `interval`, reporting each one
/// whose counters moved, until the connection fails.
///
/// The first pass records a baseline (the caller resyncs on connect).
/// `HIGHESTMODSEQ` is included when the server has CONDSTORE so flag-only
/// changes are seen too; without it, `UNSEEN` still catches read-state
/// changes.
pub async fn poll_status<F>(
    target: &SyncTarget,
    secret: &str,
    mailboxes: &[String],
    interval: Duration,
    mut on_event: F,
) -> Result<()>
where
    F: FnMut(WatchEvent) + Send,
{
    if target.protocol != ProtocolKind::Imap {
        bail!("IMAP adapter received a non-IMAP sync target");
    }
    let endpoint = parse_endpoint(&target.endpoint)?;
    let mut session = connect_session(&endpoint, &target.username, secret, target.auth).await?;
    let condstore = session
        .capabilities()
        .await
        .map(|capabilities| capabilities.has_str("CONDSTORE"))
        .unwrap_or(false);
    let items = if condstore {
        "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ)"
    } else {
        "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)"
    };
    on_event(WatchEvent::Connected);

    let mut baseline = HashMap::new();
    loop {
        for mailbox in mailboxes {
            let status = timeout(COMMAND_TIMEOUT, session.status(mailbox, items))
                .await
                .map_err(|_| anyhow!("the IMAP server did not answer STATUS for {mailbox}"))?
                .with_context(|| format!("failed to poll the status of {mailbox}"))?;
            let counters = (
                status.exists,
                status.uid_next,
                status.uid_validity,
                status.unseen,
                status.highest_modseq,
            );
            if baseline
                .insert(mailbox.clone(), counters)
                .is_some_and(|previous| previous != counters)
            {
                on_event(WatchEvent::Changed {
                    mailbox: Some(mailbox.clone()),
                });
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// Waits on an initialised IDLE, handing every server response to
/// `on_response`, until [`IDLE_RENEW_INTERVAL`] has passed.
async fn idle_until_renewal<F>(
    idle: &mut async_imap::extensions::idle::Handle<DynImapStream>,
    mut on_response: F,
) -> Result<()>
where
    F: FnMut(&Response<'_>),
{
    let renew_at = tokio::time::Instant::now() + IDLE_RENEW_INTERVAL;
    loop {
        let remaining = renew_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let (wait, _stop) = idle.wait_with_timeout(remaining);
        match wait.await.context("the IMAP IDLE failed")? {
            IdleResponse::NewData(response) => on_response(response.parsed()),
            IdleResponse::Timeout => return Ok(()),
            // Nothing else stops this wait, so the stream ended: the
            // server closed the connection.
            IdleResponse::ManualInterrupt => bail!("the IMAP server closed the IDLE connection"),
        }
    }
}

/// The mailbox a NOTIFY response names: `STATUS` responses carry it, and a
/// `LIST` response announces a created or renamed mailbox.
fn notified_mailbox(response: &Response<'_>) -> Option<String> {
    match response {
        Response::MailboxData(MailboxDatum::Status { mailbox, .. }) => Some(mailbox.to_string()),
        Response::MailboxData(MailboxDatum::List { name, .. }) => Some(name.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::imap_proto::parser::parse_response;

    fn mailboxes(entries: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        entries
            .iter()
            .map(|(name, role)| ((*name).to_owned(), role.map(str::to_owned)))
            .collect()
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn favourites_get_idle_and_the_rest_is_polled() {
        let plan = pool_plan(
            &mailboxes(&[
                ("INBOX", None),
                ("Archive", Some("archive")),
                ("Sent Items", Some("sent")),
                ("Drafts", Some("drafts")),
                ("Projects", None),
            ]),
            true,
            4,
        );
        assert_eq!(
            plan,
            WatchPlan::Pool {
                idle: names(&["INBOX", "Sent Items", "Drafts"]),
                polled: names(&["Archive", "Projects"]),
            }
        );
    }

    #[test]
    fn the_budget_reserves_the_poller_and_overflow_is_polled() {
        let plan = pool_plan(
            &mailboxes(&[
                ("INBOX", Some("inbox")),
                ("Sent", Some("sent")),
                ("Drafts", Some("drafts")),
                ("Projects", None),
            ]),
            true,
            2,
        );
        assert_eq!(
            plan,
            WatchPlan::Pool {
                idle: names(&["INBOX"]),
                polled: names(&["Sent", "Drafts", "Projects"]),
            }
        );
    }

    #[test]
    fn nothing_left_to_poll_frees_the_poller_slot() {
        let plan = pool_plan(
            &mailboxes(&[("INBOX", None), ("Sent", Some("sent"))]),
            true,
            2,
        );
        assert_eq!(
            plan,
            WatchPlan::Pool {
                idle: names(&["INBOX", "Sent"]),
                polled: Vec::new(),
            }
        );
    }

    #[test]
    fn without_idle_everything_is_polled() {
        let plan = pool_plan(
            &mailboxes(&[("INBOX", None), ("Sent", Some("sent"))]),
            false,
            4,
        );
        assert_eq!(
            plan,
            WatchPlan::Pool {
                idle: Vec::new(),
                polled: names(&["INBOX", "Sent"]),
            }
        );
    }

    #[test]
    fn special_use_attributes_name_roles() {
        assert_eq!(
            special_use_role(&[NameAttribute::Marked, NameAttribute::Sent]),
            Some("sent")
        );
        assert_eq!(special_use_role(&[NameAttribute::NoInferiors]), None);
    }

    #[test]
    fn notify_status_responses_name_their_mailbox() {
        let (_, status) =
            parse_response(b"* STATUS Projects (MESSAGES 3 UIDNEXT 9)\r\n").expect("valid");
        assert_eq!(notified_mailbox(&status), Some("Projects".to_owned()));
        let (_, list) = parse_response(b"* LIST () \"/\" \"New Folder\"\r\n").expect("valid");
        assert_eq!(notified_mailbox(&list), Some("New Folder".to_owned()));
        let (_, exists) = parse_response(b"* 4 EXISTS\r\n").expect("valid");
        assert_eq!(notified_mailbox(&exists), None);
    }
}
