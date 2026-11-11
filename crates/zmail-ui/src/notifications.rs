//! New-mail notifications, posted through GPUI's system notifications
//! (`UNUserNotificationCenter` on macOS, toasts on Windows, the XDG
//! notification service on Linux); a click on one brings the window
//! forward with its conversation open.
//!
//! The sync service composes notifications on its own threads
//! ([`zmail_notify::NewMailNotifier`]) into a [`zmail_notify::channel`];
//! a task on the main thread posts each one. Each gets an opaque tag, and
//! the conversation it opens stays here, so no server ids reach the
//! operating system's notification store. A click on a notification from
//! an earlier run, or one older than the last [`REMEMBERED_NOTIFICATIONS`],
//! only brings the window forward.
//!
//! Settings → Mail turns them off ([`NotificationPreferences::enabled`]),
//! from the next arrival on: nothing more reaches the operating system.
//!
//! [`NotificationPreferences::enabled`]: zmail_core::preferences::NotificationPreferences

use std::collections::VecDeque;
use std::sync::Arc;

use gpui::{App, Global, SharedString, SystemNotification, SystemNotificationResponse};
use zmail_core::preferences::NotificationPreferences;
use zmail_notify::{ChannelSink, NewMailNotification, NewMailNotifier, ThreadRef};

use crate::WorkspaceView;

/// The notifier the sync service reports new mail through.
pub(crate) type MailNotifier = NewMailNotifier<ChannelSink>;

/// How many of the latest notifications a click can still open.
const REMEMBERED_NOTIFICATIONS: usize = 64;

/// The conversations the latest notifications open, by tag.
#[derive(Default)]
struct NotificationTargets {
    posted: u64,
    targets: VecDeque<(SharedString, ThreadRef)>,
}

impl Global for NotificationTargets {}

/// The user's notification choices, and the notifier they apply to.
struct NotificationSettings {
    preferences: NotificationPreferences,
    /// The sync service's notifier: what it composes follows the
    /// preferences too.
    _notifier: Arc<MailNotifier>,
}

impl Global for NotificationSettings {}

impl NotificationTargets {
    /// A new tag for a notification that opens `target`.
    fn remember(&mut self, target: ThreadRef) -> SharedString {
        self.posted += 1;
        let tag = SharedString::from(format!("new-mail-{}", self.posted));
        if self.targets.len() == REMEMBERED_NOTIFICATIONS {
            self.targets.pop_front();
        }
        self.targets.push_back((tag.clone(), target));
        tag
    }

    fn target(&self, tag: &str) -> Option<&ThreadRef> {
        self.targets
            .iter()
            .find(|(posted, _)| posted == tag)
            .map(|(_, target)| target)
    }
}

/// Starts posting new-mail notifications and opening their conversations
/// when clicked, and returns the notifier for the sync service. Once per
/// app, before the sync service reports any mail.
pub(crate) fn start(cx: &mut App) -> Arc<MailNotifier> {
    let (sink, receiver) = zmail_notify::channel();
    cx.set_global(NotificationTargets::default());
    let notifier = Arc::new(NewMailNotifier::with_sink(sink));
    cx.set_global(NotificationSettings {
        preferences: NotificationPreferences::default(),
        _notifier: notifier.clone(),
    });
    cx.on_system_notification_response(open_clicked);
    cx.spawn(async move |cx| {
        while let Some(notification) = receiver.recv().await {
            cx.update(|cx| post(notification, cx));
        }
    })
    .detach();
    notifier
}

/// Applies the saved notification choices (at startup, and when Settings
/// changes them). Showing only "New mail" (`show_content`) reaches the
/// notifier once it can leave the sender and subject out.
pub(crate) fn apply_preferences(preferences: NotificationPreferences, cx: &mut App) {
    if cx.has_global::<NotificationSettings>() {
        cx.global_mut::<NotificationSettings>().preferences = preferences;
    }
}

/// Whether new mail raises notifications.
pub(crate) fn enabled(cx: &App) -> bool {
    cx.try_global::<NotificationSettings>()
        .is_none_or(|settings| settings.preferences.enabled)
}

/// Posts one notification to the operating system, unless the user
/// turned them off.
fn post(notification: NewMailNotification, cx: &mut App) {
    if !enabled(cx) {
        return;
    }
    let tag = cx
        .global_mut::<NotificationTargets>()
        .remember(notification.opens);
    cx.show_system_notification(SystemNotification {
        tag,
        title: notification.summary.into(),
        body: notification.body.into(),
        actions: Vec::new(),
    });
}

/// Brings the main window forward on its notification's conversation.
fn open_clicked(response: SystemNotificationResponse, cx: &mut App) {
    let target = cx
        .global::<NotificationTargets>()
        .target(&response.tag)
        .cloned();
    let Some(workspace) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<WorkspaceView>())
    else {
        return;
    };
    cx.activate(true);
    let _ = workspace.update(cx, |view, window, cx| {
        window.activate_window();
        if let Some(target) = target {
            view.open_notified_thread(&target, window, cx);
        }
    });
}
