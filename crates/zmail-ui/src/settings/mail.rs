//! Settings → Mail: how HTML mail shows, new-mail notifications and
//! background indexing for search.
//!
//! Each row is a picker over a [`UiPreferences`] field, saved and applied
//! as soon as it changes; nothing here waits for a restart:
//!
//! - **HTML mail** (`html_as_text`): the reader closes or reopens its web
//!   pages at once. `ZMAIL_WEB_RENDERER=0`, or a build without a web
//!   renderer, keeps plain text whatever is chosen; the row says so and
//!   its picker is disabled.
//! - **New mail notifications** (`notifications.enabled`): from the next
//!   arrival. Showing only "New mail" instead of the sender and subject
//!   (`notifications.show_content`) is stored already; its row is added
//!   once the notifier can leave them out.
//! - **Background indexing** (`indexing`): the indexer reads the switches
//!   before each batch, so turning it off stops after the current batch,
//!   and turning it on resumes after the next sync. Only Linux reports
//!   battery power so far, and no platform reports a metered connection
//!   (`zmail_sync::power`), which the rows say.
//!
//! [`UiPreferences`]: zmail_core::preferences::UiPreferences

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use zmail_core::preferences::UiPreferences;

use super::{
    SettingsEvent, SettingsHost, apply_to_workspace, group_heading, section_heading, setting_row,
};
use crate::components::{ComponentTheme, Select, SelectEvent};

/// Width of the section's pickers.
const SELECT_WIDTH: f32 = 200.0;

/// A switch's two options, in picker order: the value each sets and its
/// label.
type Choice = [(bool, &'static str); 2];

const HTML_MAIL: Choice = [(false, "Formatted"), (true, "As Plain Text")];
const ON_OFF: Choice = [(true, "On"), (false, "Off")];
const KEEP_INDEXING: Choice = [(false, "Pause"), (true, "Keep Indexing")];

/// A row's picker over one switch of the preferences.
struct Switch {
    select: Entity<Select>,
    choice: Choice,
    get: fn(&UiPreferences) -> bool,
}

/// The Mail section.
pub(crate) struct MailSettings {
    html_as_text: Switch,
    notifications: Switch,
    indexing: Switch,
    index_on_battery: Switch,
    index_on_metered: Switch,
    _subscriptions: Vec<Subscription>,
}

impl MailSettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let mut switch = |label: &'static str,
                          choice: Choice,
                          what: &'static str,
                          get: fn(&UiPreferences) -> bool,
                          set: fn(&mut UiPreferences, bool),
                          cx: &mut Context<Self>| {
            let select = cx.new(|cx| {
                Select::new(choice.map(|(_, label)| label).to_vec(), None, cx)
                    .label(label)
                    .width(px(SELECT_WIDTH))
            });
            subscriptions.push(
                cx.subscribe(&select, move |this, _, event: &SelectEvent, cx| {
                    let SelectEvent::Changed(index) = *event;
                    let on = choice[index].0;
                    match SettingsHost::get(cx)
                        .store
                        .update_ui_preferences(|preferences| set(preferences, on))
                    {
                        Ok(_) => apply_to_workspace(cx),
                        Err(error) => cx.emit(SettingsEvent::Error(format!(
                            "Could not save {what}: {error:#}"
                        ))),
                    }
                    this.reload(cx);
                }),
            );
            Switch {
                select,
                choice,
                get,
            }
        };
        let html_as_text = switch(
            "HTML mail",
            HTML_MAIL,
            "how HTML mail shows",
            |preferences| preferences.html_as_text,
            |preferences, on| preferences.html_as_text = on,
            cx,
        );
        let notifications = switch(
            "New mail notifications",
            ON_OFF,
            "the notification setting",
            |preferences| preferences.notifications.enabled,
            |preferences, on| preferences.notifications.enabled = on,
            cx,
        );
        let indexing = switch(
            "Background indexing",
            ON_OFF,
            "the indexing setting",
            |preferences| preferences.indexing.enabled,
            |preferences, on| preferences.indexing.enabled = on,
            cx,
        );
        let index_on_battery = switch(
            "On battery power",
            KEEP_INDEXING,
            "the indexing setting",
            |preferences| preferences.indexing.on_battery,
            |preferences, on| preferences.indexing.on_battery = on,
            cx,
        );
        let index_on_metered = switch(
            "On metered connections",
            KEEP_INDEXING,
            "the indexing setting",
            |preferences| preferences.indexing.on_metered,
            |preferences, on| preferences.indexing.on_metered = on,
            cx,
        );
        let mut this = Self {
            html_as_text,
            notifications,
            indexing,
            index_on_battery,
            index_on_metered,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    /// Show the saved choices.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let preferences = SettingsHost::get(cx)
            .store
            .ui_preferences()
            .unwrap_or_default();
        for switch in self.switches() {
            let on = (switch.get)(&preferences);
            let index = switch.choice.iter().position(|(value, _)| *value == on);
            switch
                .select
                .update(cx, |select, cx| select.set_selected(index, cx));
        }
        let formatting_unavailable = crate::reader::formatting_unavailable().is_some();
        self.html_as_text.select.update(cx, |select, cx| {
            select.set_disabled(formatting_unavailable, cx)
        });
        // Pausing only matters while indexing runs.
        for switch in [&self.index_on_battery, &self.index_on_metered] {
            switch.select.update(cx, |select, cx| {
                select.set_disabled(!preferences.indexing.enabled, cx)
            });
        }
    }

    fn switches(&self) -> [&Switch; 5] {
        [
            &self.html_as_text,
            &self.notifications,
            &self.indexing,
            &self.index_on_battery,
            &self.index_on_metered,
        ]
    }

    /// The pickers, in the order they show, for tests to drive.
    #[cfg(test)]
    pub(crate) fn selects(&self) -> [Entity<Select>; 5] {
        self.switches().map(|switch| switch.select.clone())
    }
}

/// What the HTML mail row says.
fn html_mail_description() -> SharedString {
    let description = "Formatted shows a message's HTML in a locked-down view, with remote content blocked until you allow it. As Plain Text shows only its text, without pictures or layout. Applies at once.";
    match crate::reader::formatting_unavailable() {
        Some(reason) => format!("{description} {reason}").into(),
        None => description.into(),
    }
}

impl Render for MailSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .flex_col()
            .child(section_heading("Mail"))
            .child(setting_row(
                "HTML mail",
                Some(html_mail_description()),
                self.html_as_text.select.clone(),
                &colors,
            ))
            .child(setting_row(
                "New mail notifications",
                Some(
                    "A system notification names the sender and subject when new mail arrives while Zmail is not the window in front. Applies from the next new mail."
                        .into(),
                ),
                self.notifications.select.clone(),
                &colors,
            ))
            .child(group_heading("Search indexing", &colors))
            .child(setting_row(
                "Background indexing",
                Some(
                    "Downloads the messages you have not opened, newest first and within each account's cache budget, so search can find words in them. IMAP accounts only; other accounts download whole messages as they sync. Turning it off stops after the current batch; turning it on resumes after the next sync."
                        .into(),
                ),
                self.indexing.select.clone(),
                &colors,
            ))
            .child(setting_row(
                "On battery power",
                Some(
                    "Whether indexing keeps going while the computer runs on battery. Zmail can tell only on Linux so far; elsewhere indexing never pauses for this."
                        .into(),
                ),
                self.index_on_battery.select.clone(),
                &colors,
            ))
            .child(setting_row(
                "On metered connections",
                Some(
                    "Whether indexing keeps going on a connection that charges by the amount of data. Zmail cannot tell yet when a connection is metered, so for now this changes nothing."
                        .into(),
                ),
                self.index_on_metered.select.clone(),
                &colors,
            ))
    }
}
