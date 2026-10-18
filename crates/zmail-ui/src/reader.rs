//! The reader pane: the open conversation and its actions.
//!
//! [`ReaderView`] is its own entity with its own focus handle and the
//! `Reader` key context. The shell pushes the selected thread (loaded once
//! per selection or store refresh, never per frame) with
//! [`ReaderView::set_state`]. Its buttons dispatch the same actions as the
//! key bindings; the shell handles them, because replying and moving need
//! the selection and the account.
//!
//! The actions are one row of icon buttons above the conversation. The
//! More menu at its end holds Move to… (one entry per mailbox, until the
//! Phase 4 folder picker) and, when the pane is narrow, the actions that
//! no longer fit in the row.

use chrono::{DateTime, Local};
use gpui::{
    Action, AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, MouseButton, Render, ScrollHandle, SharedString, Subscription, Window, div, point,
    prelude::*, px, rgb,
};
use zmail_core::{
    MailboxSummary, ThreadMessage, ThreadView,
    participants::{parse_address_list, sender_identity},
    text::count_noun,
    time::{HourCycle, format_full_date, format_list_date},
};

use crate::actions::{
    Archive, Delete, Forward, MoveToMailbox, PageDown, PageUp, Reply, ReplyAll, SaveAttachments,
    ScrollDown, ScrollUp, ToggleFlag, ToggleRead,
};
use crate::components::{IconButton, Menu, MenuEntry, popover_below};
use crate::pane::pane_focus_indicator;
use crate::styles::{AVATAR_SIZE, UiColors};
use crate::tooltip::text_tooltip;
use crate::widgets::{
    EllipsisExt, action_divider, avatar, empty_state, format_size, label_chip,
    message_avatar_background,
};

/// The key context the reader sets.
pub(crate) const KEY_CONTEXT: &str = "Reader";

/// How far one arrow key scrolls the conversation.
const SCROLL_STEP: f32 = 48.;

/// What the shell shows in the reader.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ReaderState {
    pub(crate) thread: Option<ThreadView>,
    /// The account's mailboxes, for the move buttons.
    pub(crate) mailboxes: Vec<MailboxSummary>,
    /// The user's own addresses, shown as "me".
    pub(crate) own_addresses: Vec<String>,
    /// The pane is too narrow for every action button: only Reply,
    /// Archive and Delete stay in the row, the rest move to More.
    pub(crate) compact_actions: bool,
    /// How many conversations the list has selected. With more than one,
    /// the reader shows the count instead of a conversation.
    pub(crate) selected_count: usize,
}

pub(crate) struct ReaderView {
    focus_handle: FocusHandle,
    hour_cycle: HourCycle,
    state: ReaderState,
    scroll_handle: ScrollHandle,
    /// The open More menu and the subscription that closes it.
    more_menu: Option<(Entity<Menu>, Subscription)>,
    /// Set when a press on the More button closed the open menu, so the
    /// click that follows does not reopen it.
    swallow_more_click: bool,
    /// Test instrumentation: renders.
    #[cfg(test)]
    pub(crate) counters: crate::pane::RenderCounters,
}

impl Focusable for ReaderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReaderView {
    pub(crate) fn new(hour_cycle: HourCycle, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            hour_cycle,
            state: ReaderState::default(),
            scroll_handle: ScrollHandle::new(),
            more_menu: None,
            swallow_more_click: false,
            #[cfg(test)]
            counters: Default::default(),
        }
    }

    /// Replaces what the reader shows; re-renders only when it changed. A
    /// different thread starts at the top.
    pub(crate) fn set_state(&mut self, state: ReaderState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        let thread_changed = self.state.thread.as_ref().map(|thread| thread.id)
            != state.thread.as_ref().map(|thread| thread.id);
        self.state = state;
        if thread_changed {
            self.scroll_handle.set_offset(point(px(0.), px(0.)));
            self.more_menu = None;
        }
        cx.notify();
    }

    /// What the reader shows.
    #[cfg(test)]
    pub(crate) fn state(&self) -> &ReaderState {
        &self.state
    }

    /// Whether the action bar is compact.
    #[cfg(test)]
    pub(crate) fn compact_actions(&self) -> bool {
        self.state.compact_actions
    }

    /// Whether the More menu is open.
    #[cfg(test)]
    pub(crate) fn more_menu(&self) -> Option<Entity<Menu>> {
        self.more_menu.as_ref().map(|(menu, _)| menu.clone())
    }

    /// Opens the More menu and gives it focus. Choosing an entry
    /// dispatches its action from the reader, like the buttons do.
    fn open_more_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = more_menu_items(self.state.thread.as_ref(), &self.state);
        if items.is_empty() {
            return;
        }
        let focus_handle = self.focus_handle.clone();
        let menu = Menu::build(window, cx, |mut menu| {
            for item in items {
                menu = match item {
                    MoreItem::Separator => menu.separator(),
                    MoreItem::Header(title) => menu.header(title),
                    MoreItem::Action(action) => {
                        let focus_handle = focus_handle.clone();
                        let mut entry = MenuEntry::new(action.label).icon(action.icon);
                        if let Some(shortcut) = action.shortcut {
                            entry = entry.shortcut(shortcut);
                        }
                        let command = action.action;
                        menu.entry(entry.on_select(move |window, cx| {
                            focus_handle.dispatch_action(command.as_ref(), window, cx);
                        }))
                    }
                };
            }
            menu
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |reader, menu, _: &DismissEvent, window, cx| {
                // An entry that moved focus elsewhere (none do yet) keeps
                // it; otherwise focus comes back to the reader.
                let refocus = menu.focus_handle(cx).contains_focused(window, cx)
                    || window.focused(cx).is_none();
                reader.more_menu = None;
                if refocus {
                    window.focus(&reader.focus_handle, cx);
                }
                cx.notify();
            },
        );
        window.focus(&menu.focus_handle(cx), cx);
        self.more_menu = Some((menu, subscription));
        cx.notify();
    }

    fn toggle_more_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.swallow_more_click) {
            return;
        }
        if self.more_menu.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        } else {
            self.open_more_menu(window, cx);
        }
    }

    fn scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let offset = self.scroll_handle.offset();
        let max = self.scroll_handle.max_offset().y;
        // Offsets grow negative as the content scrolls up.
        let y = (offset.y - px(delta)).clamp(-max, px(0.));
        self.scroll_handle.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn page_height(&self) -> f32 {
        let height: f32 = self.scroll_handle.bounds().size.height.into();
        (height - SCROLL_STEP).max(SCROLL_STEP)
    }

    fn scroll_up(&mut self, _: &ScrollUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-SCROLL_STEP, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(SCROLL_STEP, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page_height(), cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page_height(), cx);
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.counters.renders += 1;
        }
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);
        let thread = self.state.thread.as_ref();
        let actions = action_bar_buttons(
            thread,
            self.state.compact_actions,
            &self.focus_handle,
            &colors,
        );
        let has_more = !more_menu_items(thread, &self.state).is_empty();

        div()
            .id("reader")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            // ---- Action bar: one row, never wrapping ---------------------
            .child(
                pane_focus_indicator(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .min_h(px(ACTION_BAR_HEIGHT))
                        .px_3()
                        .overflow_hidden()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .when(thread.is_none(), |this| {
                    this.child(div().px_1().text_color(rgb(colors.muted)).child("Reader"))
                })
                .children(actions)
                .when(has_more, |this| {
                    this.child(div().flex_1()).child(
                        div()
                            .debug_selector(|| "reader-more".into())
                            .relative()
                            .flex_none()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|reader, _, _, _| {
                                    reader.swallow_more_click = reader.more_menu.is_some();
                                }),
                            )
                            .child(
                                IconButton::new("reader-more", "ellipsis", "More actions")
                                    .selected(self.more_menu.is_some())
                                    .on_click(cx.listener(|reader, _, window, cx| {
                                        reader.toggle_more_menu(window, cx);
                                    })),
                            )
                            .when_some(self.more_menu.as_ref(), |this, (menu, _)| {
                                this.child(popover_below(menu.clone()))
                            }),
                    )
                }),
            )
            .child(
                div()
                    .id("reader-scroll")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .px_4()
                    .py_4()
                    .child(if self.state.selected_count > 1 {
                        render_selection_summary(self.state.selected_count, &colors)
                    } else {
                        render_reader_detail(
                            thread,
                            self.hour_cycle,
                            &self.state.own_addresses,
                            &colors,
                        )
                    }),
            )
    }
}

/// Height of the reader's action bar.
const ACTION_BAR_HEIGHT: f32 = 48.0;

/// What the reader shows while the list has several conversations
/// selected.
fn render_selection_summary(count: usize, colors: &UiColors) -> AnyElement {
    empty_state(
        &format!("{count} conversations selected"),
        "Archive, delete, move or flag them together from the bar above the list. \
         Esc clears the selection.",
        "check-circle",
        None,
        colors,
    )
}

/// Render the full thread reader: subject, metadata, and conversation
/// cards. Returns an [`AnyElement`] ready to be placed
/// inside a scrollable container.
fn render_reader_detail(
    selected_thread: Option<&ThreadView>,
    hour_cycle: HourCycle,
    own_addresses: &[String],
    colors: &UiColors,
) -> AnyElement {
    match selected_thread {
        Some(thread) => {
            let now = chrono::Local::now();
            let latest_message_at = thread
                .messages
                .last()
                .map(|message| format_full_date(&message.received_at, &now.timezone(), hour_cycle))
                .unwrap_or_else(|| "No local messages".to_owned());
            let participants = participant_names(&thread.participants, own_addresses);

            div()
                .flex()
                .flex_col()
                .gap_5()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w_full()
                        .min_w_0()
                        // The subject gets the full pane width so it wraps
                        // at the pane edge.
                        .child(
                            div()
                                .w_full()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .child(thread.subject.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(thread_header_badges(thread, colors)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .w_full()
                                .min_w_0()
                                .child(metadata_pair("Participants", &participants, colors))
                                .child(metadata_pair("Latest", &latest_message_at, colors))
                                .child(metadata_pair(
                                    "Mailbox",
                                    &format!(
                                        "{} \u{2022} {}",
                                        thread.mailbox_name,
                                        count_noun(thread.message_count, "message", "messages")
                                    ),
                                    colors,
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Conversation"),
                        )
                        .child(
                            div()
                                .rounded_lg()
                                .bg(rgb(colors.surface))
                                .border_1()
                                .border_color(rgb(colors.border))
                                .children(thread.messages.iter().enumerate().map(
                                    |(index, message)| {
                                        render_thread_message(
                                            index,
                                            message,
                                            &now,
                                            hour_cycle,
                                            own_addresses,
                                            colors,
                                        )
                                        .into_any_element()
                                    },
                                )),
                        ),
                )
                .into_any_element()
        }
        None => empty_state(
            "No message selected",
            "Select a conversation to read it here.",
            "mail-open",
            None,
            colors,
        ),
    }
}

/// One conversation command: its label, icon, shortcut and action.
struct ReaderAction {
    label: SharedString,
    icon: &'static str,
    /// The key binding, shown in tooltips and menus.
    shortcut: Option<&'static str>,
    action: Box<dyn Action>,
    /// Whether the button stays in a compact action bar.
    compact: bool,
}

impl ReaderAction {
    fn new(
        label: &'static str,
        icon: &'static str,
        shortcut: &'static str,
        action: impl Action,
    ) -> Self {
        Self {
            label: label.into(),
            icon,
            shortcut: Some(shortcut),
            action: Box::new(action),
            compact: false,
        }
    }

    /// Keep the button when the bar is compact.
    fn keep_when_compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

/// A group of [`ReaderAction`]s; groups are separated in the action bar
/// and the More menu.
type ActionGroup = Vec<ReaderAction>;

/// The thread's commands, in order, grouped: respond, file, mark.
fn reader_actions(thread: &ThreadView) -> [ActionGroup; 3] {
    let mut mark = vec![
        ReaderAction::new(
            if thread.is_flagged { "Unflag" } else { "Flag" },
            "flag",
            "secondary-shift-l",
            ToggleFlag,
        ),
        if thread.unread_count > 0 {
            ReaderAction::new("Mark Read", "eye", "secondary-shift-u", ToggleRead)
        } else {
            ReaderAction::new("Mark Unread", "eye-off", "secondary-shift-u", ToggleRead)
        },
    ];
    let thread_has_attachments = thread
        .messages
        .iter()
        .any(|message| !message.attachments.is_empty());
    if thread_has_attachments {
        mark.push(ReaderAction::new(
            "Save Attachments",
            "save",
            "secondary-shift-s",
            SaveAttachments,
        ));
    }
    [
        vec![
            ReaderAction::new("Reply", "reply", "secondary-r", Reply).keep_when_compact(),
            ReaderAction::new("Reply All", "reply-all", "secondary-shift-r", ReplyAll),
            ReaderAction::new("Forward", "forward", "secondary-shift-f", Forward),
        ],
        vec![
            ReaderAction::new("Archive", "archive", "secondary-e", Archive).keep_when_compact(),
            ReaderAction::new("Delete", "trash-2", "secondary-backspace", Delete)
                .keep_when_compact(),
        ],
        mark,
    ]
}

/// The action bar's icon buttons, with dividers between the groups. Each
/// button dispatches its action from the reader, so it does exactly what
/// the key binding does.
///
/// Returns an empty [`Vec`] when no thread is selected.
fn action_bar_buttons(
    thread: Option<&ThreadView>,
    compact: bool,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> Vec<AnyElement> {
    let Some(thread) = thread else {
        return Vec::new();
    };
    let mut elements = Vec::new();
    for group in reader_actions(thread) {
        let buttons: Vec<AnyElement> = group
            .into_iter()
            .filter(|action| !compact || action.compact)
            .map(|action| {
                let focus_handle = focus_handle.clone();
                let command = action.action;
                // Icons are unique within the bar, so they name the buttons.
                let mut button =
                    IconButton::new(format!("reader-{}", action.icon), action.icon, action.label)
                        .selected(action.icon == "flag" && thread.is_flagged)
                        .on_click(move |_, window, cx| {
                            focus_handle.dispatch_action(command.as_ref(), window, cx)
                        });
                if let Some(shortcut) = action.shortcut {
                    button = button.shortcut(shortcut);
                }
                button.into_any_element()
            })
            .collect();
        if buttons.is_empty() {
            continue;
        }
        if !elements.is_empty() {
            elements.push(
                div()
                    .mx_1()
                    .child(action_divider(colors))
                    .into_any_element(),
            );
        }
        elements.extend(buttons);
    }
    elements
}

/// One row of the More menu.
enum MoreItem {
    Action(ReaderAction),
    Separator,
    Header(String),
}

/// The More menu: the actions a compact bar leaves out, then Move to…
/// with one entry per other mailbox. Empty when there is nothing to put
/// in it (no thread, or a full bar and no other mailbox).
fn more_menu_items(thread: Option<&ThreadView>, state: &ReaderState) -> Vec<MoreItem> {
    let Some(thread) = thread else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if state.compact_actions {
        for group in reader_actions(thread) {
            let hidden: Vec<ReaderAction> =
                group.into_iter().filter(|action| !action.compact).collect();
            if hidden.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(MoreItem::Separator);
            }
            items.extend(hidden.into_iter().map(MoreItem::Action));
        }
    }
    let destinations: Vec<&MailboxSummary> = state
        .mailboxes
        .iter()
        .filter(|mailbox| mailbox.id != thread.mailbox_id)
        .collect();
    if !destinations.is_empty() {
        if !items.is_empty() {
            items.push(MoreItem::Separator);
        }
        items.push(MoreItem::Header("Move to".to_owned()));
        items.extend(destinations.into_iter().map(|mailbox| {
            MoreItem::Action(ReaderAction {
                label: mailbox.name.clone().into(),
                icon: "folder",
                shortcut: None,
                action: Box::new(MoveToMailbox {
                    mailbox_id: mailbox.id,
                }),
                compact: false,
            })
        }));
    }
    items
}

fn render_thread_message(
    index: usize,
    message: &ThreadMessage,
    now: &DateTime<Local>,
    hour_cycle: HourCycle,
    own_addresses: &[String],
    colors: &UiColors,
) -> gpui::Div {
    let (sender_name, sender_email) = sender_identity(&message.sender);
    let avatar_color = message_avatar_background(&sender_name, colors);
    let recipients = participant_names(&message.recipients, own_addresses);
    let date = format_list_date(&message.received_at, now, hour_cycle);
    let full_date = format_full_date(&message.received_at, &now.timezone(), hour_cycle);
    let status_badges = message_status_badges(message, colors);

    div()
        .px_4()
        .py_4()
        .when(index > 0, |this| {
            this.border_t_1().border_color(rgb(colors.border))
        })
        .child(
            div()
                .flex()
                .items_start()
                .gap_3()
                .child(avatar(&sender_name, AVATAR_SIZE, avatar_color, colors))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_start()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .w_full()
                                                .ellipsis()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(sender_name),
                                        )
                                        .when_some(sender_email, |this, email| {
                                            this.child(
                                                div()
                                                    .w_full()
                                                    .ellipsis()
                                                    .text_sm()
                                                    .text_color(rgb(colors.muted))
                                                    .child(email),
                                            )
                                        })
                                        .when(!recipients.is_empty(), |this| {
                                            this.child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(colors.muted))
                                                    .ellipsis()
                                                    .child(format!("To: {recipients}")),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .id(("reader-message-date", message.id as u64))
                                        .flex_none()
                                        .whitespace_nowrap()
                                        .text_sm()
                                        .text_color(rgb(colors.muted))
                                        .tooltip(text_tooltip(full_date, *colors))
                                        .child(date),
                                ),
                        )
                        .when(!status_badges.is_empty(), |this| {
                            this.child(div().flex().flex_wrap().gap_2().children(status_badges))
                        })
                        .when(!message.attachments.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(message_attachments(message, colors)),
                            )
                        })
                        .children(sender_safety_banner(message, colors))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .text_sm()
                                .child(message.rendered_body.clone()),
                        ),
                ),
        )
}

fn thread_header_badges(thread: &ThreadView, colors: &UiColors) -> Vec<AnyElement> {
    let mut badges = Vec::new();

    if thread.unread_count > 0 {
        badges.push(
            label_chip(
                format!("{} unread", thread.unread_count),
                Some("eye-off"),
                colors.button_background,
                colors.accent,
            )
            .into_any_element(),
        );
    }

    if thread.is_flagged {
        badges.push(
            label_chip(
                "Flagged",
                Some("flag"),
                colors.button_background,
                colors.warning,
            )
            .into_any_element(),
        );
    }

    if thread.has_attachments {
        badges.push(
            label_chip(
                "Attachments",
                Some("paperclip"),
                colors.button_background,
                colors.info,
            )
            .into_any_element(),
        );
    }

    if thread.has_remote_content {
        badges.push(
            label_chip(
                "Remote content blocked",
                Some("shield"),
                colors.button_background,
                colors.danger,
            )
            .into_any_element(),
        );
    }

    badges
}

fn message_attachments(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    message
        .attachments
        .iter()
        .map(|attachment| {
            label_chip(
                format!(
                    "{} ({})",
                    attachment.filename,
                    format_size(attachment.size_bytes)
                ),
                Some("paperclip"),
                colors.button_background,
                colors.info,
            )
            .into_any_element()
        })
        .collect()
}

fn message_status_badges(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    // The render mode (plain text, sanitized HTML, preview) is an internal
    // detail; it stays on `ThreadMessage` for diagnostics but is not shown.
    let mut badges = Vec::new();

    if !message.is_read {
        badges.push(
            label_chip(
                "Unread",
                Some("eye-off"),
                colors.button_background,
                colors.accent,
            )
            .into_any_element(),
        );
    }

    if message.is_flagged {
        badges.push(
            label_chip(
                "Flagged",
                Some("flag"),
                colors.button_background,
                colors.warning,
            )
            .into_any_element(),
        );
    }

    if message.has_remote_content {
        badges.push(
            label_chip(
                "Remote content blocked",
                Some("shield"),
                colors.button_background,
                colors.danger,
            )
            .into_any_element(),
        );
    }

    badges
}

/// Renders the inline phishing-warning banner for a message's sender
/// identity (issue #91): lookalike From domains and Reply-To mismatches.
/// Information-first and non-blocking, matching the remote-content
/// stance in docs/phase5-hardening.md.
fn sender_safety_banner(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    let report = zmail_security::sender_safety::analyze_sender_identity(
        &message.sender,
        message.reply_to.as_deref(),
    );
    if report.is_safe() {
        return Vec::new();
    }

    vec![
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(colors.warning))
            .bg(rgb(colors.warning_background))
            .flex()
            .flex_col()
            .gap_1()
            .children(
                report
                    .warnings()
                    .into_iter()
                    .map(|warning| {
                        div()
                            .text_sm()
                            .text_color(rgb(colors.warning))
                            .child(warning)
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element(),
    ]
}

/// One "Label: value" line; the value ends in an ellipsis when it does not
/// fit the pane.
fn metadata_pair(label: &str, value: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(colors.muted))
                .child(format!("{label}:")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .ellipsis()
                .child(value.to_owned()),
        )
}

/// Display names from an address list, with the user's own addresses
/// shown as "me": `Alice Johnson, me`.
fn participant_names(addresses: &str, own_addresses: &[String]) -> String {
    parse_address_list(addresses)
        .iter()
        .map(|participant| {
            let is_me = participant.email.as_deref().is_some_and(|email| {
                own_addresses
                    .iter()
                    .any(|own| own.eq_ignore_ascii_case(email.trim()))
            });
            if is_me {
                "me".to_owned()
            } else {
                participant.display_name().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
