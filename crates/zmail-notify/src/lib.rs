//! Operating-system notifications for new mail.
//!
//! One notification is raised per arrival batch (one sync that found new
//! mail), naming the account and the arrivals, subject to a flood cap:
//! beyond [`MAX_NOTIFICATIONS_PER_MINUTE`] batches the detail collapses
//! into a single summary line until the window frees up again.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Maximum number of new-mail notifications per rolling minute.
pub const MAX_NOTIFICATIONS_PER_MINUTE: usize = 6;

/// Window of the notification flood cap.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Maximum number of arrival subjects listed in a notification body.
const MAX_SUBJECTS_PER_NOTIFICATION: usize = 3;

/// One new message (or reply) surfaced to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailArrival {
    /// Subject of the thread that received the message.
    pub subject: String,
    /// Participant summary of the thread.
    pub sender: String,
}

/// A batch of new mail found by one sync of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailBatch {
    /// Display name of the account that received the mail.
    pub account: String,
    /// Arrivals found by the sync, newest first.
    pub arrivals: Vec<NewMailArrival>,
}

/// Sink that renders one composed notification.
pub trait NotificationSink: Send + Sync {
    /// Shows a notification with the given summary and body.
    fn show(&self, summary: &str, body: &str);
}

/// [`NotificationSink`] backed by `notify-rust`, which resolves to D-Bus
/// notifications on Linux, notification center on macOS, and toasts on
/// Windows.
pub struct OsNotificationSink;

impl NotificationSink for OsNotificationSink {
    fn show(&self, summary: &str, body: &str) {
        // A missing notification daemon must never take the sync path
        // down; failures are dropped.
        use notify_rust::Notification;
        let _ = Notification::new().summary(summary).body(body).show();
    }
}

/// Consumer of new-mail batches; the sync service reports through this.
pub trait Notifier: Send + Sync {
    /// Raises one notification for the batch.
    fn notify_new_mail(&self, batch: &NewMailBatch);
}

/// Raises one OS notification per new-mail batch, capped by a rolling
/// per-minute budget, and suppressed while the quiet flag is set (the GUI
/// sets it while its window is focused).
pub struct NewMailNotifier<S: NotificationSink> {
    sink: S,
    quiet: AtomicBool,
    recent: Mutex<VecDeque<Instant>>,
}

impl NewMailNotifier<OsNotificationSink> {
    /// Constructs the OS-backed notifier; `quiet` starts cleared.
    pub fn os_backed() -> Self {
        Self::with_sink(OsNotificationSink)
    }
}

impl<S: NotificationSink> NewMailNotifier<S> {
    /// Constructs a notifier over a custom sink.
    pub fn with_sink(sink: S) -> Self {
        Self {
            sink,
            quiet: AtomicBool::new(false),
            recent: Mutex::new(VecDeque::new()),
        }
    }

    /// Sets the quiet flag: while set, notifications are suppressed.
    pub fn set_quiet(&self, quiet: bool) {
        self.quiet.store(quiet, Ordering::SeqCst);
    }

    /// Returns whether the quiet flag is currently set.
    pub fn is_quiet(&self) -> bool {
        self.quiet.load(Ordering::SeqCst)
    }

    /// Raises one notification for the batch, honouring the flood cap.
    pub fn notify_new_mail(&self, batch: &NewMailBatch) {
        if batch.arrivals.is_empty() || self.is_quiet() {
            return;
        }

        let summary = if batch.arrivals.len() == 1 {
            format!("New mail: {}", batch.arrivals[0].subject)
        } else {
            format!("New mail: {} messages", batch.arrivals.len())
        };
        let body = self.compose_body(&batch.account, &batch.arrivals, self.reserve_slot());

        self.sink.show(&summary, &body);
    }

    /// Records this batch against the rolling budget, returning whether
    /// the batch is over the per-minute cap.
    fn reserve_slot(&self) -> bool {
        let mut recent = self
            .recent
            .lock()
            .expect("notification rate tracking mutex should not be poisoned");
        let now = Instant::now();
        while let Some(front) = recent.front()
            && now.saturating_duration_since(*front) > RATE_WINDOW
        {
            recent.pop_front();
        }
        recent.push_back(now);
        recent.len() > MAX_NOTIFICATIONS_PER_MINUTE
    }

    fn compose_body(&self, account: &str, arrivals: &[NewMailArrival], over_cap: bool) -> String {
        let mut lines = vec![account.to_owned()];
        if over_cap {
            lines.push(
                "Mail is arriving quickly; further detail is paused for a moment.".to_owned(),
            );
            return lines.join("\n");
        }
        for arrival in arrivals.iter().take(MAX_SUBJECTS_PER_NOTIFICATION) {
            lines.push(format!("{} — {}", arrival.subject, arrival.sender));
        }
        if arrivals.len() > MAX_SUBJECTS_PER_NOTIFICATION {
            lines.push(format!(
                "… and {} more",
                arrivals.len() - MAX_SUBJECTS_PER_NOTIFICATION
            ));
        }
        lines.join("\n")
    }
}

impl<S: NotificationSink> NotificationSink for Arc<S> {
    fn show(&self, summary: &str, body: &str) {
        (**self).show(summary, body);
    }
}

impl<S: NotificationSink> Notifier for NewMailNotifier<S> {
    fn notify_new_mail(&self, batch: &NewMailBatch) {
        Self::notify_new_mail(self, batch)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        MAX_NOTIFICATIONS_PER_MINUTE, NewMailArrival, NewMailBatch, NewMailNotifier,
        NotificationSink,
    };

    #[derive(Default)]
    struct RecordingSink {
        shown: Mutex<Vec<(String, String)>>,
    }

    impl RecordingSink {
        fn notifications(&self) -> Vec<(String, String)> {
            self.shown
                .lock()
                .expect("recording sink mutex should not be poisoned")
                .clone()
        }
    }

    impl NotificationSink for RecordingSink {
        fn show(&self, summary: &str, body: &str) {
            self.shown
                .lock()
                .expect("recording sink mutex should not be poisoned")
                .push((summary.to_owned(), body.to_owned()));
        }
    }

    fn batch(count: usize) -> NewMailBatch {
        NewMailBatch {
            account: "Work".to_owned(),
            arrivals: (0..count)
                .map(|index| NewMailArrival {
                    subject: format!("Subject {index}"),
                    sender: "sender@example.test".to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn raises_one_notification_per_batch() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&batch(2));
        let shown = sink.notifications();
        assert_eq!(shown.len(), 1, "one notification per arrival batch");
        assert!(shown[0].0.contains("2 messages"));
        assert!(shown[0].1.contains("Work"));
    }

    #[test]
    fn quiet_batches_raise_nothing() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.set_quiet(true);
        notifier.notify_new_mail(&batch(1));
        assert!(sink.notifications().is_empty());
        assert!(notifier.is_quiet());
    }

    #[test]
    fn empty_batches_raise_nothing() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&NewMailBatch {
            account: "Work".to_owned(),
            arrivals: Vec::new(),
        });
        assert!(sink.notifications().is_empty());
    }

    #[test]
    fn flood_cap_collapses_detail_after_the_budget() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        for _ in 0..MAX_NOTIFICATIONS_PER_MINUTE {
            notifier.notify_new_mail(&batch(1));
        }
        assert_eq!(sink.notifications().len(), MAX_NOTIFICATIONS_PER_MINUTE);
        assert!(
            !sink
                .notifications()
                .last()
                .expect("batch recorded")
                .1
                .contains("paused")
        );

        notifier.notify_new_mail(&batch(1));
        let shown = sink.notifications();
        assert_eq!(shown.len(), MAX_NOTIFICATIONS_PER_MINUTE + 1);
        assert!(
            shown
                .last()
                .expect("over-cap batch recorded")
                .1
                .contains("paused"),
            "batches beyond the cap collapse into a summary"
        );
    }

    #[test]
    fn bodies_list_at_most_three_subjects() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&batch(5));
        let body = &sink.notifications()[0].1;
        assert!(body.contains("Subject 0"));
        assert!(body.contains("Subject 2"));
        assert!(!body.contains("Subject 3"));
        assert!(body.contains("… and 2 more"));
    }
}
