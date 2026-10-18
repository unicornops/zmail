//! The list header: the mailbox name, its unread count and the filter
//! menu (All / Unread / Flagged / Has attachments).
//!
//! The filter runs in the store query (`LocalStore::list_filtered_threads`
//! and `filtered_counts`), so a filtered list pages and counts like any
//! other. It is the list's own state and resets to All when another
//! mailbox is shown.

use gpui::{
    AnyElement, Context, DismissEvent, Focusable, FontWeight, IntoElement, MouseButton, Window,
    div, prelude::*, rgb,
};
use zmail_core::{ThreadFilter, text::count_noun};

use super::{ListMode, MessageListView};
use crate::actions::RetryOutbox;
use crate::components::{Badge, Button, ButtonSize, Menu, MenuEntry, popover_below_end};
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::UiColors;
use crate::widgets::{EllipsisExt, empty_state};

impl MessageListView {
    /// Threads the list shows in Mail mode: the mailbox's count, or the
    /// filter's when one is on.
    pub(super) fn thread_count(&self) -> usize {
        match self.filtered_total {
            Some(total) if self.filter != ThreadFilter::All => total,
            _ => self
                .state
                .mailbox
                .as_ref()
                .map(|mailbox| mailbox.total_threads)
                .unwrap_or_default(),
        }
    }

    /// Re-counts the threads the filter leaves (none to count for All).
    pub(super) fn count_filtered(&mut self, cx: &mut Context<Self>) {
        let selection = self.state.mailbox.as_ref().map(|mailbox| mailbox.selection);
        self.filtered_total = match selection {
            Some(selection) if self.filter != ThreadFilter::All => {
                match self.store.filtered_counts(&selection, self.filter) {
                    Ok(counts) => Some(counts.total_threads),
                    Err(error) => {
                        cx.emit(super::MessageListEvent::Error(error.to_string()));
                        None
                    }
                }
            }
            _ => None,
        };
    }

    /// Shows only the threads `filter` matches.
    pub(crate) fn set_filter(&mut self, filter: ThreadFilter, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.page = None;
        self.selected_index_hint = None;
        self.count_filtered(cx);
        self.thread_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// The filter in effect.
    #[cfg(test)]
    pub(crate) fn filter(&self) -> ThreadFilter {
        self.filter
    }

    fn toggle_filter_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.swallow_filter_click) {
            return;
        }
        if self.filter_menu.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
            return;
        }
        let this = cx.weak_entity();
        let current = self.filter;
        let menu = Menu::build(window, cx, |mut menu| {
            for filter in ThreadFilter::ALL {
                let this = this.clone();
                menu = menu.entry(
                    MenuEntry::new(filter.label())
                        .checked(filter == current)
                        .on_select(move |_, cx| {
                            this.update(cx, |list, cx| list.set_filter(filter, cx)).ok();
                        }),
                );
            }
            let index = ThreadFilter::ALL
                .iter()
                .position(|filter| *filter == current)
                .unwrap_or_default();
            menu.with_selected(index)
        });
        let subscription =
            cx.subscribe_in(&menu, window, |list, _, _: &DismissEvent, window, cx| {
                // Choosing or dismissing hands focus back to the rows.
                list.filter_menu = None;
                window.focus(&list.focus_handle, cx);
                cx.notify();
            });
        window.focus(&menu.focus_handle(cx), cx);
        self.filter_menu = Some((menu, subscription));
        cx.notify();
    }

    pub(super) fn render_header(
        &self,
        focused: bool,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, subtitle) = self.header_text();
        let mail = self.state.mode == ListMode::Mail;
        let unread = self
            .state
            .mailbox
            .as_ref()
            .filter(|_| mail)
            .map(|mailbox| mailbox.unread_threads)
            .unwrap_or_default();
        let can_filter = mail && self.state.mailbox.is_some();
        let can_retry = self.state.mode == ListMode::Outbox && !self.state.outbox_jobs.is_empty();

        pane_focus_indicator(
            div()
                .px_3()
                .py_3()
                .border_b_1()
                .border_color(rgb(colors.border)),
            focused,
            colors,
        )
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .ellipsis()
                                .child(title),
                        )
                        .when(unread > 0, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .debug_selector(|| "list-unread-count".into())
                                    .child(Badge::count(unread)),
                            )
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .ellipsis()
                        .child(subtitle),
                ),
        )
        .when(can_filter, |this| this.child(self.render_filter_button(cx)))
        // Retry lives with the outbox it acts on.
        .when(can_retry, |this| {
            this.child(
                Button::new("outbox-retry", "Retry")
                    .icon("refresh-cw")
                    .size(ButtonSize::Small)
                    .shortcut("secondary-shift-o")
                    .on_click(dispatch_from(&self.focus_handle, RetryOutbox)),
            )
        })
        .into_any_element()
    }

    /// The filter button: a quiet "All", or the active filter highlighted.
    fn render_filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .relative()
            .flex_none()
            .debug_selector(|| "list-filter".into())
            // A press while the menu is open closes it (the menu treats it
            // as a click outside); the click that follows must not reopen
            // it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|list, _, _, _| {
                    list.swallow_filter_click = list.filter_menu.is_some();
                }),
            )
            .child(
                Button::new("list-filter", self.filter.label())
                    .icon("list-filter")
                    .size(ButtonSize::Small)
                    .ghost()
                    .selected(self.filter != ThreadFilter::All)
                    .tooltip("Filter conversations")
                    .on_click(cx.listener(|list, _, window, cx| {
                        list.toggle_filter_menu(window, cx);
                    })),
            )
            .when_some(self.filter_menu.as_ref(), |this, (menu, _)| {
                this.child(popover_below_end(menu.clone()))
            })
            .into_any_element()
    }

    /// The list when the filter leaves nothing, with a way back to All.
    pub(super) fn render_filtered_empty(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = self
            .state
            .mailbox
            .as_ref()
            .map(|mailbox| mailbox.name.as_str())
            .unwrap_or("this mailbox");
        let title = match self.filter {
            ThreadFilter::All | ThreadFilter::Unread => {
                format!("No unread conversations in {name}")
            }
            ThreadFilter::Flagged => format!("No flagged conversations in {name}"),
            ThreadFilter::HasAttachments => format!("No attachments in {name}"),
        };
        empty_state(
            &title,
            "The filter hides every conversation here.",
            "list-filter",
            Some(
                div()
                    .debug_selector(|| "list-filter-show-all".into())
                    .child(
                        Button::new("list-filter-show-all", "Show All").on_click(cx.listener(
                            |list, _, _, cx| {
                                list.set_filter(ThreadFilter::All, cx);
                            },
                        )),
                    )
                    .into_any_element(),
            ),
            colors,
        )
    }

    fn header_text(&self) -> (String, String) {
        let mailbox = self.state.mailbox.as_ref();
        let title = match self.state.mode {
            ListMode::Mail => mailbox
                .map(|mailbox| mailbox.name.clone())
                .unwrap_or_else(|| "Mail".to_owned()),
            ListMode::Search => "Search".to_owned(),
            ListMode::Drafts => "Drafts".to_owned(),
            ListMode::Outbox => "Outbox".to_owned(),
        };
        let subtitle = match self.state.mode {
            ListMode::Mail => mailbox
                .map(|mailbox| {
                    let total = count_noun(mailbox.total_threads, "conversation", "conversations");
                    if self.filter == ThreadFilter::All {
                        total
                    } else {
                        format!("{} of {total}", self.thread_count())
                    }
                })
                .unwrap_or_else(|| "No mailbox selected".to_owned()),
            ListMode::Search => format!(
                "{} for \u{201c}{}\u{201d}{}",
                count_noun(self.state.search_results.len(), "result", "results"),
                self.state.search_query,
                if self.state.searching_server {
                    " \u{2022} searching the server\u{2026}"
                } else {
                    ""
                }
            ),
            ListMode::Drafts => count_noun(self.state.drafts.len(), "draft", "drafts"),
            ListMode::Outbox => count_noun(
                self.state.outbox_jobs.len(),
                "message waiting to send",
                "messages waiting to send",
            ),
        };
        (title, subtitle)
    }
}
