//! `CommandPalette`: Secondary+K's search over every command, folder and
//! account.
//!
//! The workspace builds the entries when the palette opens ([`entries`]):
//!
//! - every command in the menu model that can run from where focus was,
//!   with the shortcut the menus show for it in the active keymap;
//! - every folder of every account ("Go to Archive", the account named
//!   beside it when there is more than one), plus the Unified Inbox,
//!   Flagged and Sent across accounts;
//! - every account (opening it opens its Inbox).
//!
//! Typing filters them with the in-house [fuzzy matcher](crate::fuzzy) and
//! highlights the matched letters. Recently used entries (kept in the UI
//! preferences) come first when nothing is typed and rank higher when
//! something is. Up and Down move the highlight, Enter or a click picks,
//! and Escape or a click outside closes the palette (the surrounding
//! [`Modal`](crate::components::Modal)). Picking emits
//! [`CommandPaletteEvent::Picked`]; the workspace closes the palette and
//! runs the entry.

use std::ops::Range;

use gpui::{
    Action, AnyElement, App, AppContext as _, Context, EventEmitter, FocusHandle, Focusable,
    FontWeight, HighlightStyle, IntoElement, Keymap, Render, Role, ScrollStrategy, SharedString,
    StyledText, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use zmail_core::{AccountSummary, MailboxRole, MailboxSummary, VirtualMailbox};

use crate::WorkspaceView;
use crate::a11y::{
    AccessibleExt, Politeness, announcement_key, highlight_announcement, live_region,
};
use crate::actions::ToggleCommandPalette;
use crate::components::actions::{SelectFirst, SelectLast, SelectNext, SelectPrevious};
use crate::components::kbd::spoken_keystrokes;
use crate::components::{ComponentTheme, Kbd, Modal, TextInput, TextInputEvent};
use crate::fuzzy::fuzzy_match;
use crate::menus::{MenuModel, MenuModelItem, binding_label, menu_binding};
use crate::pane::Pane;
use crate::sidebar::{folder_label, mailbox_icon_name};
use crate::styles::{
    ICON_SIZE_SM, Platform, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_8, TYPE_BODY, TYPE_CAPTION,
    TypeStyleExt,
};
use crate::widgets::{EllipsisExt, icon};

/// Key context of the palette, for its Up and Down bindings.
pub(crate) const KEY_CONTEXT: &str = "CommandPalette";

/// Width of the palette card.
pub(crate) const PALETTE_WIDTH: f32 = 560.0;

/// Height of one result row.
const ROW_HEIGHT: f32 = SPACE_8;

/// The most result rows shown before the list scrolls.
const VISIBLE_ROWS: usize = 10;

/// Score added to the most recently used entry when a query is typed; each
/// older one gets [`RECENT_STEP`] less.
const RECENT_BONUS: i32 = 40;
const RECENT_STEP: i32 = 4;

/// What picking an entry does.
pub(crate) enum PaletteTarget {
    /// Run this action from where focus was when the palette opened.
    Action(Box<dyn Action>),
    /// Show this mailbox.
    Mailbox(i64),
    /// Show this virtual mailbox.
    Virtual(VirtualMailbox),
}

/// One row the palette can show.
pub(crate) struct PaletteEntry {
    /// A stable key for the recently used list (`"action:zmail::Archive"`,
    /// `"mailbox:42"`, `"virtual:flagged"`, `"account:3"`).
    pub(crate) key: String,
    /// What the row says, and what typing matches first.
    pub(crate) label: SharedString,
    /// Muted text after the label (the account, the folder's parent);
    /// typing matches it too.
    pub(crate) detail: Option<SharedString>,
    pub(crate) icon: &'static str,
    /// The shortcut in GPUI syntax, for [`Kbd`].
    pub(crate) shortcut: Option<SharedString>,
    pub(crate) target: PaletteTarget,
}

/// What a screen reader reads for an entry: its label, then its detail.
fn entry_name(entry: &PaletteEntry) -> SharedString {
    match &entry.detail {
        Some(detail) => format!("{}, {detail}", entry.label).into(),
        None => entry.label.clone(),
    }
}

/// What the palette reports.
pub(crate) enum CommandPaletteEvent {
    /// The user picked this entry.
    Picked {
        /// The entry's key, for the recently used list.
        key: String,
        /// What to do.
        target: PaletteTarget,
    },
}

/// The entries for the current state of the app.
///
/// `available` says whether a command can run where focus was (the menus'
/// own rule); commands that cannot, and the text-editing commands that only
/// make sense in a text field, are left out.
pub(crate) fn entries(
    model: &MenuModel,
    keymap: &Keymap,
    available: impl Fn(&dyn Action, bool) -> bool,
    accounts: &[AccountSummary],
    mailboxes: &[MailboxSummary],
) -> Vec<PaletteEntry> {
    let mut entries = Vec::new();
    for section in &model.sections {
        for item in &section.items {
            let MenuModelItem::Command(command) = item else {
                continue;
            };
            let name = command.action.name();
            if name.starts_with("text_input::")
                || name == "zmail::ToggleCommandPalette"
                || !available(command.action.as_ref(), command.app_wide)
            {
                continue;
            }
            entries.push(PaletteEntry {
                key: format!("action:{name}"),
                label: command.label.into(),
                detail: None,
                icon: "chevron-right",
                shortcut: menu_binding(keymap, command.action.as_ref())
                    .map(|binding| binding_label(binding).into()),
                target: PaletteTarget::Action(command.action.boxed_clone()),
            });
        }
    }

    let several = accounts.len() > 1;
    let all_accounts = || several.then(|| SharedString::from("All accounts"));
    if several {
        entries.push(virtual_entry(
            VirtualMailbox::Unified(MailboxRole::Inbox),
            "inbox",
            None,
        ));
    }
    entries.push(virtual_entry(
        VirtualMailbox::Flagged,
        "flag",
        all_accounts(),
    ));
    // With one account its own Sent folder is the same list.
    if several {
        entries.push(virtual_entry(
            VirtualMailbox::Unified(MailboxRole::Sent),
            "send",
            all_accounts(),
        ));
    }

    for account in accounts {
        for mailbox in mailboxes
            .iter()
            .filter(|mailbox| mailbox.account_id == account.id)
        {
            let parent = mailbox
                .parent
                .as_deref()
                .map(|parent| parent_path(parent, mailbox.delimiter.as_deref()));
            let detail = match (several, parent) {
                (true, Some(parent)) => Some(format!("{} › {parent}", account.display_name)),
                (true, None) => Some(account.display_name.clone()),
                (false, parent) => parent,
            };
            entries.push(PaletteEntry {
                key: format!("mailbox:{}", mailbox.id),
                label: format!("Go to {}", folder_label(mailbox)).into(),
                detail: detail.map(SharedString::from),
                icon: mailbox_icon_name(mailbox.role),
                shortcut: None,
                target: PaletteTarget::Mailbox(mailbox.id),
            });
        }
    }

    for account in accounts {
        let Some(inbox) = mailboxes.iter().find(|mailbox| {
            mailbox.account_id == account.id && mailbox.role == Some(MailboxRole::Inbox)
        }) else {
            continue;
        };
        entries.push(PaletteEntry {
            key: format!("account:{}", account.id),
            label: account.display_name.clone().into(),
            detail: Some(account.username.clone().into()),
            icon: "user",
            shortcut: None,
            target: PaletteTarget::Mailbox(inbox.id),
        });
    }
    entries
}

fn virtual_entry(
    mailbox: VirtualMailbox,
    icon: &'static str,
    detail: Option<SharedString>,
) -> PaletteEntry {
    let key = match mailbox {
        VirtualMailbox::Unified(role) => format!("virtual:{}", role.as_str()),
        VirtualMailbox::Flagged => "virtual:flagged".to_owned(),
    };
    PaletteEntry {
        key,
        label: format!("Go to {}", mailbox.label()).into(),
        detail,
        icon,
        shortcut: None,
        target: PaletteTarget::Virtual(mailbox),
    }
}

/// A folder's parent path for display: `Projects/2026` as
/// `Projects › 2026`.
fn parent_path(parent: &str, delimiter: Option<&str>) -> String {
    match delimiter.filter(|delimiter| !delimiter.is_empty()) {
        Some(delimiter) => parent.split(delimiter).collect::<Vec<_>>().join(" › "),
        None => parent.to_owned(),
    }
}

/// An entry that matches the query.
struct PaletteMatch {
    /// Index into the entries.
    entry: usize,
    /// Byte offsets of the matched characters in the entry's label.
    positions: Vec<usize>,
}

/// The searchable list of entries.
pub(crate) struct CommandPalette {
    query: gpui::Entity<TextInput>,
    entries: Vec<PaletteEntry>,
    /// Keys of the recently used entries, most recent first.
    recent: Vec<String>,
    matches: Vec<PaletteMatch>,
    /// Index into `matches`.
    highlighted: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl CommandPalette {
    /// A palette over `entries`, with the `recent` keys first.
    pub(crate) fn new(
        entries: Vec<PaletteEntry>,
        recent: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(cx)
                .compact()
                .leading_icon("search")
                .label("Search commands, folders and accounts")
                .placeholder("Search commands, folders and accounts…")
        });
        // The field keeps Enter (it submits) and passes Up and Down on to
        // the palette's bindings.
        let subscription = cx.subscribe(
            &query,
            |palette, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => palette.refilter(cx),
                TextInputEvent::Submitted => palette.pick(palette.highlighted, cx),
            },
        );
        let mut palette = Self {
            query,
            entries,
            recent,
            matches: Vec::new(),
            highlighted: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        };
        palette.refilter(cx);
        palette
    }

    /// Types `text` into the search field (the screenshot fixture).
    pub(crate) fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |query, cx| query.set_text(text, cx));
        self.refilter(cx);
    }

    /// The labels of the matching entries, best first.
    #[cfg(test)]
    pub(crate) fn matching_labels(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|found| {
                let entry = &self.entries[found.entry];
                match &entry.detail {
                    Some(detail) => format!("{} ({detail})", entry.label),
                    None => entry.label.to_string(),
                }
            })
            .collect()
    }

    /// The highlighted entry's label.
    #[cfg(test)]
    pub(crate) fn highlighted_label(&self) -> Option<SharedString> {
        self.matches
            .get(self.highlighted)
            .map(|found| self.entries[found.entry].label.clone())
    }

    /// The shortcut shown beside the entry labelled `label`.
    #[cfg(test)]
    pub(crate) fn shortcut_for(&self, label: &str) -> Option<SharedString> {
        self.entries
            .iter()
            .find(|entry| entry.label == label)
            .and_then(|entry| entry.shortcut.clone())
    }

    fn recent_rank(&self, key: &str) -> Option<usize> {
        self.recent.iter().position(|recent| recent == key)
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_owned();
        let mut scored: Vec<(i32, Option<usize>, PaletteMatch)> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            // The label first, so the highlight offsets index into it.
            let text = match &entry.detail {
                Some(detail) => format!("{} {detail}", entry.label),
                None => entry.label.to_string(),
            };
            let Some(found) = fuzzy_match(&query, &text) else {
                continue;
            };
            let rank = self.recent_rank(&entry.key);
            let bonus = rank.map_or(0, |rank| {
                RECENT_BONUS - RECENT_STEP * i32::try_from(rank).unwrap_or(i32::MAX / 8)
            });
            let positions = found
                .positions
                .into_iter()
                .filter(|&offset| offset < entry.label.len())
                .collect();
            scored.push((
                found.score + bonus,
                rank,
                PaletteMatch {
                    entry: index,
                    positions,
                },
            ));
        }
        if query.is_empty() {
            // Recently used first, most recent first; then the rest in
            // their natural order.
            scored.sort_by_key(|(_, rank, found)| (rank.unwrap_or(usize::MAX), found.entry));
        } else {
            scored.sort_by_key(|(score, _, found)| (std::cmp::Reverse(*score), found.entry));
        }
        self.matches = scored.into_iter().map(|(_, _, found)| found).collect();
        self.highlighted = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn pick(&mut self, position: usize, cx: &mut Context<Self>) {
        let Some(found) = self.matches.get(position) else {
            return;
        };
        let entry = &self.entries[found.entry];
        let target = match &entry.target {
            PaletteTarget::Action(action) => PaletteTarget::Action(action.boxed_clone()),
            PaletteTarget::Mailbox(id) => PaletteTarget::Mailbox(*id),
            PaletteTarget::Virtual(mailbox) => PaletteTarget::Virtual(*mailbox),
        };
        cx.emit(CommandPaletteEvent::Picked {
            key: entry.key.clone(),
            target,
        });
    }

    fn highlight(&mut self, position: usize, cx: &mut Context<Self>) {
        if position < self.matches.len() {
            self.highlighted = position;
            self.scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.highlight((self.highlighted + 1) % self.matches.len(), cx);
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            let count = self.matches.len();
            self.highlight((self.highlighted + count - 1) % count, cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(self.matches.len().saturating_sub(1), cx);
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = ComponentTheme::get(cx);
        let (colors, platform) = (theme.colors, theme.platform.platform);
        let count = self.matches.len();
        range
            .filter_map(|position| {
                let found = self.matches.get(position)?;
                let entry = &self.entries[found.entry];
                let highlighted = position == self.highlighted;
                let (background, foreground, muted) = if highlighted {
                    (
                        Some(colors.selection_background),
                        colors.selection_foreground,
                        colors.selection_foreground,
                    )
                } else {
                    (None, colors.foreground, colors.muted)
                };
                let bold = HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..HighlightStyle::default()
                };
                let highlights = found
                    .positions
                    .iter()
                    .filter_map(|&offset| {
                        let ch = entry.label[offset..].chars().next()?;
                        Some((offset..offset + ch.len_utf8(), bold))
                    })
                    .collect::<Vec<_>>();
                let accessible_name = entry_name(entry);
                Some(
                    div()
                        .id(("command-palette-entry", position))
                        .debug_selector(move || format!("command-palette-{position}"))
                        .flex()
                        .w_full()
                        .items_center()
                        .gap(px(SPACE_2))
                        .h(px(ROW_HEIGHT))
                        .px(px(SPACE_2))
                        .rounded(px(RADIUS_SM))
                        .accessible(Role::ListBoxOption, accessible_name)
                        .aria_selected(highlighted)
                        .aria_position_in_set(position + 1)
                        .aria_size_of_set(count)
                        .when_some(entry.shortcut.as_ref(), |this, shortcut| {
                            this.aria_keyshortcuts(spoken_keystrokes(shortcut, platform))
                        })
                        .cursor_pointer()
                        .when_some(background, |this, background| this.bg(rgb(background)))
                        .text_color(rgb(foreground))
                        .on_hover(cx.listener(move |palette, hovered: &bool, _, cx| {
                            if *hovered && palette.highlighted != position {
                                palette.highlighted = position;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |palette, _, _, cx| palette.pick(position, cx)))
                        .child(icon(entry.icon, ICON_SIZE_SM, muted))
                        .child(div().flex_none().max_w_full().ellipsis().child(
                            StyledText::new(entry.label.clone()).with_highlights(highlights),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ellipsis()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(muted))
                                .when_some(entry.detail.clone(), |this, detail| this.child(detail)),
                        )
                        .when_some(entry.shortcut.clone(), |this, shortcut| {
                            this.child(Kbd::new(shortcut))
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let count = self.matches.len();
        let visible = count.min(VISIBLE_ROWS);
        let highlight = highlight_announcement(
            &self
                .matches
                .get(self.highlighted)
                .map(|found| entry_name(&self.entries[found.entry]))
                .unwrap_or_default(),
            self.highlighted,
            count,
        );

        div()
            .id("command-palette")
            .key_context(KEY_CONTEXT)
            .relative()
            .child(live_region(
                ("command-palette-highlight", announcement_key(&highlight)),
                highlight,
                Politeness::Polite,
            ))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .p(px(SPACE_2))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(self.query.clone()),
            )
            .child(
                div()
                    .id("command-palette-results")
                    .accessible(Role::ListBox, "Commands, folders and accounts")
                    .p(px(SPACE_1))
                    .when(count > 0, |this| {
                        this.child(
                            uniform_list(
                                "command-palette-list",
                                count,
                                cx.processor(|palette, range: Range<usize>, _, cx| {
                                    palette.render_rows(range, cx)
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .w_full()
                            .h(px(ROW_HEIGHT * visible as f32)),
                        )
                    })
                    .when(count == 0, |this| {
                        this.child(
                            div()
                                .px(px(SPACE_2))
                                .py(px(SPACE_3))
                                .type_style(TYPE_BODY)
                                .text_color(rgb(colors.muted))
                                .child("Nothing matches"),
                        )
                    }),
            )
    }
}

// ---------------------------------------------------------------------------
// The workspace side: opening the palette and running what it picks
// ---------------------------------------------------------------------------

/// The open palette and where focus was when it opened.
pub(crate) struct OpenPalette {
    palette: gpui::Entity<CommandPalette>,
    /// Commands run from here, as if the palette had never opened.
    origin: FocusHandle,
    _subscription: Subscription,
}

impl OpenPalette {
    /// The palette entity.
    pub(crate) fn palette(&self) -> &gpui::Entity<CommandPalette> {
        &self.palette
    }
}

impl WorkspaceView {
    /// Opens the palette, or closes it when it is open (Secondary+K).
    pub(crate) fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
        } else {
            self.open_command_palette(window, cx);
        }
    }

    /// Opens the palette over the current state, with focus in its search
    /// field.
    pub(crate) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            return;
        }
        let origin = window
            .focused(cx)
            .unwrap_or_else(|| self.focus_handle.clone());
        let keymap = cx.key_bindings();
        let entries = entries(
            &MenuModel::new(Platform::current()),
            &keymap.borrow(),
            |action, app_wide| app_wide || window.is_action_available_in(action, &origin),
            &self.accounts,
            &self.all_mailboxes,
        );
        let recent = self
            .store
            .ui_preferences()
            .map(|preferences| preferences.recent_commands)
            .unwrap_or_default();
        let palette = cx.new(|cx| CommandPalette::new(entries, recent, cx));
        let subscription = cx.subscribe_in(&palette, window, Self::on_command_palette_event);
        let field = palette.focus_handle(cx);
        self.palette_modal.open_focusing(&field, window, cx);
        self.command_palette = Some(OpenPalette {
            palette,
            origin,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Closes the palette and gives focus back to where it was.
    pub(crate) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_modal.close(window, cx);
        self.command_palette = None;
        cx.notify();
    }

    fn on_command_palette_event(
        &mut self,
        _: &gpui::Entity<CommandPalette>,
        event: &CommandPaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CommandPaletteEvent::Picked { key, target } = event;
        let origin = self
            .command_palette
            .as_ref()
            .map(|open| open.origin.clone())
            .unwrap_or_else(|| self.focus_handle.clone());
        self.close_command_palette(window, cx);
        if let Err(error) = self
            .store
            .update_ui_preferences(|preferences| preferences.note_recent_command(key))
        {
            self.store_error = Some(format!("Could not save the recent commands: {error:#}"));
        }
        match target {
            PaletteTarget::Action(action) => {
                // After this update: the handler may be this view's own.
                let action = action.boxed_clone();
                window.defer(cx, move |window, cx| {
                    origin.dispatch_action(action.as_ref(), window, cx);
                });
            }
            PaletteTarget::Mailbox(mailbox_id) => {
                self.select_mailbox(*mailbox_id, cx);
                self.focus_pane(Pane::MessageList, window, cx);
            }
            PaletteTarget::Virtual(mailbox) => {
                self.select_virtual_mailbox(*mailbox, cx);
                self.focus_pane(Pane::MessageList, window, cx);
            }
        }
    }

    /// The palette in its modal, while it is open.
    pub(crate) fn render_command_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.command_palette.as_ref()?;
        Some(
            Modal::new("command-palette-modal", self.palette_modal.focus_handle())
                .palette()
                .title("Command palette")
                .width(px(PALETTE_WIDTH))
                .child(open.palette.clone())
                .on_dismiss(cx.listener(|view, _, window, cx| {
                    view.close_command_palette(window, cx);
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zmail_core::{CachePolicySummary, ProtocolKind, SyncStatusSummary};

    fn account(id: i64, name: &str) -> AccountSummary {
        AccountSummary {
            id,
            display_name: name.to_owned(),
            protocol: ProtocolKind::Imap,
            endpoint: String::new(),
            username: format!("{}@example.test", name.to_lowercase()),
            identity_count: 1,
            cache_policy: CachePolicySummary::performance_first(),
            sync_status: SyncStatusSummary::idle(""),
            color_override: None,
        }
    }

    fn mailbox(id: i64, account_id: i64, name: &str, role: Option<MailboxRole>) -> MailboxSummary {
        let parent = name.rsplit_once('/').map(|(parent, _)| parent.to_owned());
        MailboxSummary {
            id,
            account_id,
            name: name.to_owned(),
            role,
            delimiter: Some("/".to_owned()),
            parent,
            total_threads: 0,
            unread_threads: 0,
        }
    }

    fn labels(entries: &[PaletteEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match &entry.detail {
                Some(detail) => format!("{} ({detail})", entry.label),
                None => entry.label.to_string(),
            })
            .collect()
    }

    #[test]
    fn folders_name_their_account_only_when_there_are_several() {
        let model = MenuModel::new(crate::styles::Platform::Linux);
        let keymap = Keymap::default();
        let mailboxes = [
            mailbox(1, 1, "INBOX", Some(MailboxRole::Inbox)),
            mailbox(2, 1, "Archive", Some(MailboxRole::Archive)),
            mailbox(3, 1, "Projects/Website", None),
            mailbox(4, 2, "INBOX", Some(MailboxRole::Inbox)),
        ];
        let one = entries(
            &model,
            &keymap,
            |_, _| false,
            &[account(1, "Work")],
            &mailboxes,
        );
        assert_eq!(
            labels(&one),
            [
                "Go to Flagged",
                "Go to Inbox",
                "Go to Archive",
                "Go to Website (Projects)",
                "Work (work@example.test)",
            ],
            "unavailable commands are left out"
        );
        let two = entries(
            &model,
            &keymap,
            |_, _| false,
            &[account(1, "Work"), account(2, "Home")],
            &mailboxes,
        );
        assert_eq!(
            labels(&two),
            [
                "Go to Unified Inbox",
                "Go to Flagged (All accounts)",
                "Go to Sent (All accounts)",
                "Go to Inbox (Work)",
                "Go to Archive (Work)",
                "Go to Website (Work › Projects)",
                "Go to Inbox (Home)",
                "Work (work@example.test)",
                "Home (home@example.test)",
            ]
        );
    }

    #[test]
    fn commands_skip_text_editing_and_the_palette_itself() {
        let model = MenuModel::new(crate::styles::Platform::Linux);
        let found = entries(&model, &Keymap::default(), |_, _| true, &[], &[]);
        let keys = found
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>();
        assert!(keys.contains(&"action:zmail::Archive"));
        assert!(keys.contains(&"action:zmail::Undo"));
        assert!(
            !keys
                .iter()
                .any(|key| key.starts_with("action:text_input::"))
        );
        assert!(!keys.contains(&"action:zmail::ToggleCommandPalette"));
    }
}
