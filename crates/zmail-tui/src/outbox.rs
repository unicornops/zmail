//! The Outbox and the postponed messages: lists read from the store.
//!
//! - **Outbox**: messages waiting to send, sending, and those that
//!   couldn't (failed, or refused by the server until the account signs
//!   in again, or held because a recipient's key changed), with the GUI's
//!   wording. `R` sends a failed one again ([`MailActions::retry_outbox_job`]
//!   and the outbox pass, as the GUI's Retry does) and `D` takes back one
//!   that hasn't started sending (the store can't take back the others),
//!   which returns it to the postponed messages. Sign-in and key decisions
//!   are made in the Zmail window.
//! - **Postponed**: drafts being written. Enter continues one, `D`
//!   deletes one after asking.
//!
//! [`MailActions::retry_outbox_job`]: zmail_actions::MailActions::retry_outbox_job

use chrono::{DateTime, FixedOffset};
use zmail_core::{
    DraftSummary, OutboxJobState, OutboxJobSummary, key_change::PinnedKeyChanged,
    reauth::ReauthReason, text::count_noun, time::parse_timestamp,
};
use zmail_security::terminal::sanitize_line;

use crate::{
    app::{App, Prompt, Screen},
    compose::{Confirm, Field},
    pane::PaneId,
};

/// A row of the Outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    /// The job.
    pub job: OutboxJobSummary,
    /// Who it is to, filtered.
    pub to: String,
    /// Its subject, filtered.
    pub subject: String,
    /// Where it stands, as the GUI words it.
    pub status: String,
    /// It failed.
    pub failed: bool,
    /// `R` can send it again.
    pub can_retry: bool,
    /// `D` can take it back.
    pub can_cancel: bool,
    /// What the server said, filtered.
    pub error: Option<String>,
}

/// "30 sec", "5 min", "2 h", "3 days", rounded up, as the GUI's outbox
/// says it.
fn short_duration(seconds: i64) -> String {
    let seconds = seconds.max(1);
    let ceil = |unit: i64| (seconds + unit - 1) / unit;
    if seconds < 60 {
        format!("{seconds} sec")
    } else if seconds < 60 * 60 {
        format!("{} min", ceil(60))
    } else if seconds < 24 * 60 * 60 {
        format!("{} h", ceil(60 * 60))
    } else {
        count_noun(ceil(24 * 60 * 60) as usize, "day", "days")
    }
}

/// Where `job` stands, in the GUI's words, and what can be done about it.
fn status(job: &OutboxJobSummary, now: &DateTime<FixedOffset>) -> (String, bool, bool, bool) {
    let refused = job
        .last_error_message
        .as_deref()
        .and_then(ReauthReason::of_message)
        .is_some();
    let key_change = job
        .last_error_message
        .as_deref()
        .and_then(PinnedKeyChanged::of_message)
        .is_some();
    // (text, failed, can retry, can take back)
    match job.state {
        OutboxJobState::Failed if refused => (
            "Sign in again to send (in the Zmail window)".to_owned(),
            true,
            false,
            false,
        ),
        OutboxJobState::Failed if key_change => (
            "A recipient\u{2019}s key changed (decide in the Zmail window)".to_owned(),
            true,
            false,
            false,
        ),
        OutboxJobState::Queued => match job
            .next_attempt_at
            .as_deref()
            .and_then(parse_timestamp)
            .map(|at| at.with_timezone(&chrono::Utc) - now.with_timezone(&chrono::Utc))
        {
            Some(wait) if wait.num_seconds() > 0 => (
                format!("Sending in {}", short_duration(wait.num_seconds())),
                false,
                false,
                true,
            ),
            _ => ("Waiting to send".to_owned(), false, false, true),
        },
        OutboxJobState::Sending => ("Sending\u{2026}".to_owned(), false, false, false),
        OutboxJobState::Sent => ("Sent".to_owned(), false, false, false),
        OutboxJobState::Failed => ("Couldn\u{2019}t send".to_owned(), true, true, false),
        OutboxJobState::Retrying => {
            let wait = job
                .next_attempt_at
                .as_deref()
                .and_then(parse_timestamp)
                .map(|at| at.with_timezone(&chrono::Utc) - now.with_timezone(&chrono::Utc));
            let text = match wait {
                Some(wait) if wait.num_seconds() > 0 => format!(
                    "Failed \u{2014} retrying in {}",
                    short_duration(wait.num_seconds())
                ),
                _ => "Failed \u{2014} retrying now".to_owned(),
            };
            (text, true, true, false)
        }
    }
}

impl App {
    /// Reads the Outbox: every message not sent yet, newest change first.
    pub(crate) fn load_outbox(&mut self) {
        let jobs = match self.store.list_outbox_jobs(None) {
            Ok(jobs) => jobs,
            Err(error) => return self.report(error),
        };
        // The store's times are the system clock's.
        let now = chrono::Utc::now().fixed_offset();
        self.outbox = jobs
            .into_iter()
            .filter(|job| job.state != OutboxJobState::Sent)
            .map(|job| {
                let draft = self.store.draft_detail(job.draft_id).ok().flatten();
                let (status, failed, can_retry, can_cancel) = status(&job, &now);
                OutboxRow {
                    to: draft
                        .as_ref()
                        .map(|draft| {
                            draft
                                .to
                                .iter()
                                .chain(&draft.cc)
                                .chain(&draft.bcc)
                                .map(|recipient| sanitize_line(recipient))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                    subject: draft
                        .as_ref()
                        .map(|draft| sanitize_line(&draft.subject))
                        .unwrap_or_default(),
                    error: job
                        .last_error_message
                        .as_deref()
                        .map(sanitize_line)
                        .filter(|error| !error.is_empty()),
                    status,
                    failed,
                    can_retry,
                    can_cancel,
                    job,
                }
            })
            .collect();
        self.outbox_cursor = self.outbox_cursor.min(self.outbox.len().saturating_sub(1));
    }

    /// Reads the postponed messages.
    pub(crate) fn load_postponed(&mut self) {
        self.postponed = self.postponed_drafts();
        self.postponed_cursor = self
            .postponed_cursor
            .min(self.postponed.len().saturating_sub(1));
    }

    /// Opens the Outbox.
    pub(crate) fn open_outbox(&mut self) {
        self.list_from = self.screen;
        self.load_outbox();
        self.goto(Screen::Outbox);
        self.reveal.insert(PaneId::Outbox);
    }

    /// Opens the postponed messages.
    pub(crate) fn open_postponed(&mut self) {
        if !matches!(self.screen, Screen::Postponed | Screen::Help) {
            self.list_from = self.screen;
        }
        self.load_postponed();
        self.goto(Screen::Postponed);
        self.reveal.insert(PaneId::Postponed);
    }

    /// `R` in the Outbox: sends a failed message again.
    pub(crate) fn retry_outbox(&mut self) {
        let Some(row) = self.outbox.get(self.outbox_cursor).cloned() else {
            self.status = Some("The Outbox is empty".to_owned());
            return;
        };
        if !row.can_retry {
            self.status = Some(format!("{}: nothing to retry", row.status));
            return;
        }
        match self.actions.retry_outbox_job(row.job.id, &mut |_| {}) {
            Ok(()) => {
                // As the GUI's Retry: the pass runs here when this Zmail
                // syncs, else the one that does picks it up.
                if let Some(sync) = &self.sync {
                    let _ = sync.process_outbox(Some(row.job.account_id));
                }
                self.status = Some("Queued for another send attempt".to_owned());
            }
            Err(error) => self.report(error),
        }
        self.load_outbox();
    }

    /// `D` in the Outbox: takes back a message that hasn't started sending.
    pub(crate) fn cancel_outbox(&mut self) {
        let Some(row) = self.outbox.get(self.outbox_cursor).cloned() else {
            self.status = Some("The Outbox is empty".to_owned());
            return;
        };
        if !row.can_cancel {
            self.status = Some(format!(
                "{}: only a message that hasn't started sending can be taken back",
                row.status
            ));
            return;
        }
        match self
            .actions
            .cancel_send(row.job.id, row.job.draft_id, &mut |_| {})
        {
            Ok(true) => {
                if self
                    .pending_send
                    .is_some_and(|pending| pending.job_id == row.job.id)
                {
                    self.pending_send = None;
                }
                self.status = Some("Taken back: it is with your postponed messages (C)".to_owned());
            }
            Ok(false) => {
                self.status = Some("Too late: the message has started sending".to_owned());
            }
            Err(error) => self.report(error),
        }
        self.load_outbox();
    }

    /// Enter in the Outbox: what the server said, in full, on the status
    /// line.
    pub(crate) fn outbox_details(&mut self) {
        if let Some(row) = self.outbox.get(self.outbox_cursor) {
            self.status = Some(match &row.error {
                Some(error) => format!("{}: {error}", row.status),
                None => row.status.clone(),
            });
        }
    }

    /// Enter on a postponed message: continues it.
    pub(crate) fn continue_postponed(&mut self) {
        let Some(draft) = self.postponed.get(self.postponed_cursor).cloned() else {
            self.status = Some("No postponed messages".to_owned());
            return;
        };
        self.open_compose(draft.id, false, Field::Body);
    }

    /// `D` on a postponed message: asks before deleting it.
    pub(crate) fn delete_postponed(&mut self) {
        if let Some(draft) = self.postponed.get(self.postponed_cursor) {
            self.prompt = Some(Prompt::Confirm(Confirm::DeleteDraft(draft.id)));
        }
    }

    /// Deletes postponed draft `draft_id`, confirmed.
    pub(crate) fn delete_draft(&mut self, draft_id: i64) {
        match self.actions.delete_draft(draft_id, &mut |_| {}) {
            Ok(()) => self.status = Some("Postponed message deleted".to_owned()),
            Err(error) => self.report(error),
        }
        self.load_postponed();
        if self.postponed.is_empty() && self.screen == Screen::Postponed {
            self.goto(self.list_from);
        }
    }

    /// The postponed message rows' text: To and subject, filtered.
    pub(crate) fn postponed_label(draft: &DraftSummary) -> (String, String) {
        let to = sanitize_line(&draft.recipient_summary);
        let subject = sanitize_line(&draft.subject);
        (
            if to.is_empty() {
                "(no recipients)".to_owned()
            } else {
                to
            },
            if subject.is_empty() {
                "(no subject)".to_owned()
            } else {
                subject
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::short_duration;

    #[test]
    fn waits_read_as_the_guis_outbox_says_them() {
        assert_eq!(short_duration(0), "1 sec");
        assert_eq!(short_duration(45), "45 sec");
        assert_eq!(short_duration(61), "2 min");
        assert_eq!(short_duration(3 * 60 * 60), "3 h");
        assert_eq!(short_duration(2 * 24 * 60 * 60 + 1), "3 days");
    }
}
