//! New-mail notifications.
//!
//! One notification is raised per arrival batch (one sync that found new
//! mail), naming the account and the arrivals, subject to a flood cap:
//! beyond [`MAX_NOTIFICATIONS_PER_MINUTE`] batches the detail collapses
//! into a single summary line until the window frees up again.
//!
//! Each notification names the conversation a click on it opens (the
//! batch's newest arrival). The sync service composes notifications on its
//! own threads; the GUI posts them to the operating system from its main
//! thread, so it reads them from a [`channel`].
//!
//! Subjects and senders come from mail, so they go through
//! [`notification_text`] first: control, bidirectional and zero-width
//! characters removed, line breaks collapsed (a subject cannot forge the
//! account or sender lines), and at most [`MAX_FIELD_CHARS`] characters.
//! Freedesktop notification servers may read the body as markup, so on
//! Linux and the BSDs the body is escaped for it ([`escape_markup`]).
//! [`NewMailNotifier::set_show_content`] leaves subjects and senders out
//! altogether.

use std::{
    collections::VecDeque,
    future::poll_fn,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    time::{Duration, Instant},
};

use zmail_security::terminal::sanitize_line;

/// Maximum number of new-mail notifications per rolling minute.
pub const MAX_NOTIFICATIONS_PER_MINUTE: usize = 6;

/// Window of the notification flood cap.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Maximum number of arrival subjects listed in a notification body.
const MAX_SUBJECTS_PER_NOTIFICATION: usize = 3;

/// The most characters of a subject, sender or account name a
/// notification shows; longer ones are cut with an ellipsis.
pub const MAX_FIELD_CHARS: usize = 120;

/// A string from mail as a notification shows it: control,
/// bidirectional and zero-width characters and terminal escape sequences
/// removed (`zmail_security::terminal::sanitize_line`), every run of
/// whitespace, line breaks included, made one space, and cut to
/// [`MAX_FIELD_CHARS`] characters.
pub fn notification_text(text: &str) -> String {
    let line = sanitize_line(text);
    if line.chars().count() <= MAX_FIELD_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(MAX_FIELD_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('\u{2026}');
    cut
}

/// `text` escaped for a freedesktop notification body, which servers
/// with the `body-markup` capability read as a subset of XML: `&`, `<`
/// and `>` become entities, so mail cannot add bold text, links or
/// images, or blank the body with a stray `<`.
pub fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// Whether notification bodies are posted to a freedesktop notification
/// server, which may read them as markup.
const BODY_IS_MARKUP: bool = cfg!(all(unix, not(target_os = "macos")));

/// One new message (or reply) surfaced to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailArrival {
    /// Subject of the thread that received the message.
    pub subject: String,
    /// Participant summary of the thread.
    pub sender: String,
    /// Name of the mailbox holding the thread.
    pub mailbox: String,
    /// The thread's id on the server, which outlives the local mirror's
    /// thread ids.
    pub remote_thread_id: String,
}

/// A batch of new mail found by one sync of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailBatch {
    /// Local id of the account that received the mail.
    pub account_id: i64,
    /// Display name of the account that received the mail.
    pub account: String,
    /// Arrivals found by the sync, newest first.
    pub arrivals: Vec<NewMailArrival>,
}

/// The conversation a notification opens, named as the sync names it so
/// it still resolves after a later sync renumbers the local mirror.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadRef {
    /// Local id of the account.
    pub account_id: i64,
    /// Name of the mailbox holding the thread.
    pub mailbox: String,
    /// The thread's id on the server.
    pub remote_thread_id: String,
}

/// One composed new-mail notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMailNotification {
    /// The notification's headline.
    pub summary: String,
    /// The lines below the headline.
    pub body: String,
    /// The conversation a click on the notification opens.
    pub opens: ThreadRef,
}

/// Sink that renders one composed notification.
pub trait NotificationSink: Send + Sync {
    /// Shows the notification.
    fn show(&self, notification: NewMailNotification);
}

/// Consumer of new-mail batches; the sync service reports through this.
pub trait Notifier: Send + Sync {
    /// Raises one notification for the batch.
    fn notify_new_mail(&self, batch: &NewMailBatch);
}

/// Raises one notification per new-mail batch, capped by a rolling
/// per-minute budget, and suppressed while the quiet flag is set (the GUI
/// sets it while its window is focused).
pub struct NewMailNotifier<S: NotificationSink> {
    sink: S,
    quiet: AtomicBool,
    show_content: AtomicBool,
    recent: Mutex<VecDeque<Instant>>,
}

impl<S: NotificationSink> NewMailNotifier<S> {
    /// Constructs a notifier over a sink.
    pub fn with_sink(sink: S) -> Self {
        Self {
            sink,
            quiet: AtomicBool::new(false),
            show_content: AtomicBool::new(true),
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

    /// Sets whether notifications show subjects and senders (the
    /// default). Without them a notification says only "New mail" (or
    /// how many messages) and the account, for a lock screen or a shared
    /// screen.
    pub fn set_show_content(&self, show: bool) {
        self.show_content.store(show, Ordering::SeqCst);
    }

    /// Returns whether notifications show subjects and senders.
    pub fn shows_content(&self) -> bool {
        self.show_content.load(Ordering::SeqCst)
    }

    /// Raises one notification for the batch, honouring the flood cap.
    pub fn notify_new_mail(&self, batch: &NewMailBatch) {
        let Some(newest) = batch.arrivals.first() else {
            return;
        };
        if self.is_quiet() {
            return;
        }

        let show_content = self.shows_content();
        let summary = match batch.arrivals.len() {
            1 if show_content => format!("New mail: {}", notification_text(&newest.subject)),
            1 => "New mail".to_owned(),
            count => format!("New mail: {count} messages"),
        };
        let over_cap = self.reserve_slot();
        let body = self.compose_body(&batch.account, &batch.arrivals, over_cap || !show_content);
        let body = if BODY_IS_MARKUP {
            escape_markup(&body)
        } else {
            body
        };

        self.sink.show(NewMailNotification {
            summary,
            body,
            opens: ThreadRef {
                account_id: batch.account_id,
                mailbox: newest.mailbox.clone(),
                remote_thread_id: newest.remote_thread_id.clone(),
            },
        });
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

    /// The body: the account, then up to three arrivals, unless
    /// `account_only` (over the flood cap, or content hidden).
    fn compose_body(
        &self,
        account: &str,
        arrivals: &[NewMailArrival],
        account_only: bool,
    ) -> String {
        let mut lines = vec![notification_text(account)];
        if account_only {
            if self.shows_content() {
                lines.push(
                    "Mail is arriving quickly; further detail is paused for a moment.".to_owned(),
                );
            }
            return lines.join("\n");
        }
        for arrival in arrivals.iter().take(MAX_SUBJECTS_PER_NOTIFICATION) {
            lines.push(format!(
                "{} — {}",
                notification_text(&arrival.subject),
                notification_text(&arrival.sender)
            ));
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
    fn show(&self, notification: NewMailNotification) {
        (**self).show(notification);
    }
}

impl<S: NotificationSink> Notifier for NewMailNotifier<S> {
    fn notify_new_mail(&self, batch: &NewMailBatch) {
        Self::notify_new_mail(self, batch)
    }
}

/// A [`NotificationSink`] and the [`NotificationReceiver`] that reads what
/// it is shown, in order, from any executor.
pub fn channel() -> (ChannelSink, NotificationReceiver) {
    let shared = Arc::new(Mutex::new(ChannelState::default()));
    (
        ChannelSink {
            shared: shared.clone(),
        },
        NotificationReceiver { shared },
    )
}

/// The sending half of a [`channel`]. Notifications shown after the
/// receiver is gone are dropped.
pub struct ChannelSink {
    shared: Arc<Mutex<ChannelState>>,
}

/// The receiving half of a [`channel`].
pub struct NotificationReceiver {
    shared: Arc<Mutex<ChannelState>>,
}

#[derive(Default)]
struct ChannelState {
    queue: VecDeque<NewMailNotification>,
    /// The task waiting in [`NotificationReceiver::recv`].
    waker: Option<Waker>,
    sink_dropped: bool,
    receiver_dropped: bool,
}

fn lock_channel(shared: &Mutex<ChannelState>) -> std::sync::MutexGuard<'_, ChannelState> {
    shared
        .lock()
        .expect("notification channel mutex should not be poisoned")
}

impl NotificationSink for ChannelSink {
    fn show(&self, notification: NewMailNotification) {
        let waker = {
            let mut state = lock_channel(&self.shared);
            if state.receiver_dropped {
                return;
            }
            state.queue.push_back(notification);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Drop for ChannelSink {
    fn drop(&mut self) {
        let waker = {
            let mut state = lock_channel(&self.shared);
            state.sink_dropped = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl NotificationReceiver {
    /// Waits for the next notification; `None` once the sink is gone and
    /// everything it showed has been read.
    pub async fn recv(&self) -> Option<NewMailNotification> {
        poll_fn(|cx| {
            let mut state = lock_channel(&self.shared);
            if let Some(notification) = state.queue.pop_front() {
                Poll::Ready(Some(notification))
            } else if state.sink_dropped {
                Poll::Ready(None)
            } else {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

impl Drop for NotificationReceiver {
    fn drop(&mut self) {
        let mut state = lock_channel(&self.shared);
        state.receiver_dropped = true;
        state.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Wake, Waker},
    };

    use super::{
        BODY_IS_MARKUP, MAX_FIELD_CHARS, MAX_NOTIFICATIONS_PER_MINUTE, NewMailArrival,
        NewMailBatch, NewMailNotification, NewMailNotifier, NotificationSink, ThreadRef, channel,
        escape_markup, notification_text,
    };

    #[derive(Default)]
    struct RecordingSink {
        shown: Mutex<Vec<NewMailNotification>>,
    }

    impl RecordingSink {
        fn notifications(&self) -> Vec<NewMailNotification> {
            self.shown
                .lock()
                .expect("recording sink mutex should not be poisoned")
                .clone()
        }
    }

    impl NotificationSink for RecordingSink {
        fn show(&self, notification: NewMailNotification) {
            self.shown
                .lock()
                .expect("recording sink mutex should not be poisoned")
                .push(notification);
        }
    }

    fn batch(count: usize) -> NewMailBatch {
        NewMailBatch {
            account_id: 7,
            account: "Work".to_owned(),
            arrivals: (0..count)
                .map(|index| NewMailArrival {
                    subject: format!("Subject {index}"),
                    sender: "sender@example.test".to_owned(),
                    mailbox: if index == 0 { "INBOX" } else { "Lists" }.to_owned(),
                    remote_thread_id: format!("thread-{index}"),
                })
                .collect(),
        }
    }

    fn notification(index: usize) -> NewMailNotification {
        NewMailNotification {
            summary: format!("New mail: Subject {index}"),
            body: "Work".to_owned(),
            opens: ThreadRef {
                account_id: 7,
                mailbox: "INBOX".to_owned(),
                remote_thread_id: format!("thread-{index}"),
            },
        }
    }

    /// Counts its wakes.
    #[derive(Default)]
    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn raises_one_notification_per_batch() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&batch(2));
        let shown = sink.notifications();
        assert_eq!(shown.len(), 1, "one notification per arrival batch");
        assert!(shown[0].summary.contains("2 messages"));
        assert!(shown[0].body.contains("Work"));
    }

    #[test]
    fn notifications_open_the_newest_arrival() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&batch(3));
        notifier.notify_new_mail(&batch(1));
        let expected = ThreadRef {
            account_id: 7,
            mailbox: "INBOX".to_owned(),
            remote_thread_id: "thread-0".to_owned(),
        };
        let shown = sink.notifications();
        assert_eq!(shown[0].opens, expected, "a batch opens its newest arrival");
        assert_eq!(shown[1].opens, expected);
        assert_eq!(shown[1].summary, "New mail: Subject 0");
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
        notifier.notify_new_mail(&batch(0));
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
                .body
                .contains("paused")
        );

        notifier.notify_new_mail(&batch(2));
        let shown = sink.notifications();
        assert_eq!(shown.len(), MAX_NOTIFICATIONS_PER_MINUTE + 1);
        let over_cap = shown.last().expect("over-cap batch recorded");
        assert!(
            over_cap.body.contains("paused"),
            "batches beyond the cap collapse into a summary"
        );
        assert!(
            !over_cap.body.contains("Subject"),
            "the summary lists no subjects"
        );
        assert_eq!(
            over_cap.opens.remote_thread_id, "thread-0",
            "a collapsed notification still opens the newest arrival"
        );
    }

    #[test]
    fn bodies_list_at_most_three_subjects() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&batch(5));
        let body = &sink.notifications()[0].body;
        assert!(body.contains("Subject 0"));
        assert!(body.contains("Subject 2"));
        assert!(!body.contains("Subject 3"));
        assert!(body.contains("… and 2 more"));
    }

    fn hostile_batch() -> NewMailBatch {
        NewMailBatch {
            account_id: 7,
            account: "Work".to_owned(),
            arrivals: vec![NewMailArrival {
                subject: "<a href=\"https://evil.example\">Your invoice</a>\nPersonal\r\nboss@corp.example \u{202E}fdp.exe\u{200B} & <b".to_owned(),
                sender: "Bank\u{1b}[2J\nSupport".to_owned(),
                mailbox: "INBOX".to_owned(),
                remote_thread_id: "thread-0".to_owned(),
            }],
        }
    }

    #[test]
    fn mail_text_is_one_clean_short_line() {
        assert_eq!(
            notification_text("Hello\nWorld\r\n\tagain  "),
            "Hello World again"
        );
        assert_eq!(
            notification_text("in\u{202E}voice\u{200B}.pdf\u{2066} \u{1b}]8;;https://x\u{7}link"),
            "invoice.pdf link"
        );
        let long = "x".repeat(500);
        let cut = notification_text(&long);
        assert_eq!(cut.chars().count(), MAX_FIELD_CHARS);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(
            notification_text(&"é".repeat(MAX_FIELD_CHARS))
                .chars()
                .count(),
            MAX_FIELD_CHARS
        );
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape_markup("<a href=\"x\">a & b</a>"),
            "&lt;a href=\"x\"&gt;a &amp; b&lt;/a&gt;"
        );
    }

    #[test]
    fn hostile_subjects_and_senders_cannot_forge_lines_or_markup() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        notifier.notify_new_mail(&hostile_batch());
        let shown = &sink.notifications()[0];
        assert!(!shown.summary.contains('\n'), "{:?}", shown.summary);
        assert!(!shown.summary.contains('\u{202E}'));
        // The account line and one arrival line, nothing more.
        assert_eq!(shown.body.lines().count(), 2, "{:?}", shown.body);
        assert!(shown.body.starts_with("Work\n"));
        for hidden in ['\u{1b}', '\u{202E}', '\u{200B}', '\r'] {
            assert!(!shown.body.contains(hidden), "{:?}", shown.body);
        }
        if BODY_IS_MARKUP {
            assert!(!shown.body.contains('<'), "{:?}", shown.body);
            assert!(shown.body.contains("&lt;a href="), "{:?}", shown.body);
            assert!(shown.body.contains(" &amp; &lt;b"), "{:?}", shown.body);
        } else {
            assert!(shown.body.contains("<a href="), "{:?}", shown.body);
        }
    }

    #[test]
    fn hidden_content_leaves_out_subjects_and_senders() {
        let sink = Arc::new(RecordingSink::default());
        let notifier = NewMailNotifier::with_sink(sink.clone());
        assert!(notifier.shows_content());
        notifier.set_show_content(false);
        assert!(!notifier.shows_content());
        notifier.notify_new_mail(&batch(1));
        notifier.notify_new_mail(&batch(3));
        let shown = sink.notifications();
        assert_eq!(shown[0].summary, "New mail");
        assert_eq!(shown[0].body, "Work");
        assert_eq!(shown[1].summary, "New mail: 3 messages");
        assert_eq!(shown[1].body, "Work");
        assert_eq!(shown[1].opens.remote_thread_id, "thread-0");
    }

    #[test]
    fn the_channel_delivers_in_order_and_wakes_the_receiver() {
        let (sink, receiver) = channel();
        let waker = Arc::new(CountingWaker::default());
        let task_waker = Waker::from(waker.clone());
        let mut context = Context::from_waker(&task_waker);

        {
            let mut next = pin!(receiver.recv());
            assert!(next.as_mut().poll(&mut context).is_pending());
            sink.show(notification(1));
            assert_eq!(
                waker.0.load(Ordering::SeqCst),
                1,
                "showing wakes the receiver"
            );
            assert_eq!(
                next.as_mut().poll(&mut context),
                Poll::Ready(Some(notification(1)))
            );
        }

        sink.show(notification(2));
        sink.show(notification(3));
        for expected in [2, 3] {
            let mut next = pin!(receiver.recv());
            assert_eq!(
                next.as_mut().poll(&mut context),
                Poll::Ready(Some(notification(expected)))
            );
        }

        sink.show(notification(4));
        drop(sink);
        let mut next = pin!(receiver.recv());
        assert_eq!(
            next.as_mut().poll(&mut context),
            Poll::Ready(Some(notification(4))),
            "what was shown before the sink went is still read"
        );
        let mut after = pin!(receiver.recv());
        assert_eq!(after.as_mut().poll(&mut context), Poll::Ready(None));
    }

    #[test]
    fn the_channel_drops_notifications_once_the_receiver_is_gone() {
        let (sink, receiver) = channel();
        drop(receiver);
        sink.show(notification(1));
        assert!(
            sink.shared
                .lock()
                .expect("notification channel mutex should not be poisoned")
                .queue
                .is_empty()
        );
    }
}
