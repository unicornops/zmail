//! The reader (UI plan Phase 4): the conversation list, collapsed and
//! expanded messages, folded quoted text, and n/p/Enter.

use std::time::Duration;

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext,
    VisualTestContext, point, px,
};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_store::LocalStore;

use super::{Harness, Setup, click, click_row, focused_pane, keys, open_workspace_with};
use crate::pane::Pane;
use crate::reader::{PopoverAnchor, ReaderView};
use crate::{InitialView, WorkspaceView};

fn message(sender: &str, at: &str, is_read: bool, body: &str) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: format!("{sender}-{at}"),
        identity_email: "me@example.test".to_owned(),
        sender: sender.to_owned(),
        reply_to: None,
        recipients: "me@example.test".to_owned(),
        subject: "Q4 roadmap review".to_owned(),
        body_preview: body.chars().take(80).collect(),
        body_text: Some(body.to_owned()),
        sanitized_html: None,
        has_remote_content: false,
        is_read,
        is_flagged: false,
        received_at: at.to_owned(),
        attachments: Vec::new(),
    }
}

/// Replaces the account's mail with one Inbox conversation: two read
/// messages (an HTML one with a mismatched link, then one ending in a
/// quote) and an unread two-paragraph reply. Archive is empty.
fn seed_conversation(store: &LocalStore) {
    let messages = vec![
        MessageSnapshot {
            // HTML only, with a link whose text shows another site.
            body_text: None,
            sanitized_html: Some(
                "<p>Attached is the draft roadmap.</p>\
                 <p><a href=\"http://evil.example/login\">https://bank.example/login</a></p>"
                    .to_owned(),
            ),
            ..message(
                "Priya Raman <priya@example.test>",
                "2026-03-24T09:00:00Z",
                true,
                "Attached is the draft roadmap.",
            )
        },
        message(
            "Marcus Chen <marcus@example.test>",
            "2026-03-24T10:00:00Z",
            true,
            "Looks good overall.\n\nMarcus\n\nOn Tue, Priya Raman wrote:\n> Attached is the draft roadmap.\n>\n> Priya",
        ),
        message(
            "Priya Raman <priya@example.test>",
            "2026-03-24T11:00:00Z",
            false,
            "Thanks, I moved offline search to November.\n\n\
             The plan is at https://example.com/roadmap for now.",
        ),
    ];
    store_conversation(store, messages);
}

/// Replaces the account's mail with one Inbox conversation of `messages`
/// (the last one unread) and an empty Archive.
fn store_conversation(store: &LocalStore, messages: Vec<MessageSnapshot>) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![
                    MailboxSnapshot {
                        name: "Inbox".to_owned(),
                        role: Some("inbox".to_owned()),
                        threads: vec![ThreadSnapshot {
                        remote_thread_id: "roadmap".to_owned(),
                        subject: "Q4 roadmap review".to_owned(),
                        participants:
                            "Priya Raman <priya@example.test>, Marcus Chen <marcus@example.test>"
                                .to_owned(),
                        preview: "Thanks, I moved offline search to November.".to_owned(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: 1,
                        latest_message_at: "2026-03-24T11:00:00Z".to_owned(),
                        messages,
                    }],
                        ..Default::default()
                    },
                    MailboxSnapshot {
                        name: "Archive".to_owned(),
                        role: Some("archive".to_owned()),
                        ..Default::default()
                    },
                ],
            },
        )
        .expect("the mail is stored");
}

/// Opens the window on the seeded conversation.
fn open_conversation(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ReaderView>,
) {
    let (view, cx, reader, _harness) = open_conversation_with_store(cx);
    (view, cx, reader)
}

/// [`open_conversation`], keeping the store.
fn open_conversation_with_store(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ReaderView>,
    Harness,
) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Account,
        |_| InitialView::Inbox,
        seed_conversation,
    );
    let reader = cx.read_entity(&view, |view, _| view.reader.clone());
    (view, cx, reader, harness)
}

/// Which of the conversation's messages are expanded, in order.
fn expanded(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Vec<bool> {
    cx.read_entity(reader, |reader, _| {
        let thread = reader
            .state()
            .thread
            .as_ref()
            .expect("a conversation is open");
        thread
            .messages
            .iter()
            .map(|message| reader.conversation().is_expanded(message.id))
            .collect()
    })
}

fn current(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Option<usize> {
    cx.read_entity(reader, |reader, _| reader.conversation().current())
}

fn drawn(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn read_messages_collapse_and_the_latest_and_unread_expand(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);

    assert_eq!(expanded(&reader, cx), [false, false, true]);
    assert!(drawn("reader-subject", cx));
    assert!(drawn("reader-message-summary-0", cx));
    assert!(drawn("reader-message-summary-1", cx));
    assert!(drawn("reader-message-header-2", cx));
    assert!(drawn("reader-footer", cx), "the inline reply's slot");
    assert_eq!(
        current(&reader, cx),
        Some(2),
        "the unread message is current"
    );

    // Marking the conversation read (it was clicked open) does not
    // collapse what is open.
    click_row(0, Modifiers::none(), cx);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let unread = cx.read_entity(&reader, |reader, _| {
        reader
            .state()
            .thread
            .as_ref()
            .map(|thread| thread.unread_count)
    });
    assert_eq!(unread, Some(0), "the conversation was marked read");
    assert_eq!(expanded(&reader, cx), [false, false, true]);
}

#[gpui::test]
fn clicking_a_summary_expands_it_and_its_header_collapses_it(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);

    click("reader-message-summary-0", cx);
    assert_eq!(expanded(&reader, cx), [true, false, true]);
    assert_eq!(current(&reader, cx), Some(0));
    assert!(drawn("reader-message-header-0", cx));

    click("reader-message-header-0", cx);
    assert_eq!(expanded(&reader, cx), [false, false, true]);
    assert!(drawn("reader-message-summary-0", cx));
}

#[gpui::test]
fn n_and_p_move_between_messages_and_enter_toggles(cx: &mut TestAppContext) {
    let (view, cx, reader) = open_conversation(cx);
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));

    keys("p", cx);
    assert_eq!(current(&reader, cx), Some(1));
    keys("enter", cx);
    assert_eq!(expanded(&reader, cx), [false, true, true]);
    keys("p p", cx);
    assert_eq!(
        current(&reader, cx),
        Some(0),
        "p stops at the first message"
    );
    keys("n n n", cx);
    assert_eq!(current(&reader, cx), Some(2), "n stops at the last message");
    keys("enter", cx);
    assert_eq!(expanded(&reader, cx), [false, true, false]);
}

#[gpui::test]
fn quoted_text_folds_behind_a_toggle(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    let marcus = cx.read_entity(&reader, |reader, _| {
        reader
            .state()
            .thread
            .as_ref()
            .expect("a conversation")
            .messages[1]
            .id
    });
    click("reader-message-summary-1", cx);

    assert!(drawn("reader-fold-1", cx), "the quoted reply folds");
    let unfolded = |cx: &mut VisualTestContext| {
        cx.read_entity(&reader, |r, _| r.conversation().is_unfolded(marcus))
    };
    assert!(!unfolded(cx));
    click("reader-fold-1", cx);
    assert!(unfolded(cx));
    assert_eq!(
        expanded(&reader, cx),
        [false, true, true],
        "the toggle does not collapse the message"
    );
}

fn popover(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Option<PopoverAnchor> {
    cx.read_entity(reader, |reader, _| reader.popover_anchor())
}

#[gpui::test]
fn an_expanded_header_has_the_sender_recipients_reply_and_more(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    for part in [
        "reader-sender-2",
        "reader-recipients-2",
        "reader-reply-2",
        "reader-message-more-2",
    ] {
        assert!(drawn(part, cx), "{part}");
    }
    assert!(
        !drawn("reader-reply-0", cx),
        "a collapsed message has no buttons"
    );

    // The recipient summary shows the full lines, and the message stays
    // open.
    let shows_details = |cx: &mut VisualTestContext| {
        cx.read_entity(&reader, |reader, _| {
            let thread = reader.state().thread.as_ref().expect("a conversation");
            reader.conversation().shows_details(thread.messages[2].id)
        })
    };
    click("reader-recipients-2", cx);
    assert!(shows_details(cx));
    assert_eq!(expanded(&reader, cx), [false, false, true]);
    click("reader-recipients-2", cx);
    assert!(!shows_details(cx));
}

#[gpui::test]
fn the_sender_menu_copies_the_address(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    click("reader-sender-2", cx);
    assert_eq!(popover(&reader, cx), Some(PopoverAnchor::Sender(2)));
    assert_eq!(expanded(&reader, cx), [false, false, true], "still open");

    keys("down enter", cx);
    assert_eq!(popover(&reader, cx), None);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("priya@example.test".to_owned())
    );

    // A second click on the name closes its menu.
    click("reader-sender-2", cx);
    assert_eq!(popover(&reader, cx), Some(PopoverAnchor::Sender(2)));
    click("reader-sender-2", cx);
    assert_eq!(popover(&reader, cx), None);
}

#[gpui::test]
fn a_message_reply_quotes_that_message_in_the_inline_reply(cx: &mut TestAppContext) {
    let (view, cx, _reader, harness) = open_conversation_with_store(cx);
    click("reader-message-summary-0", cx);
    click("reader-reply-0", cx);

    // The reply opens below the conversation, not in the compose pane.
    let reply = super::inline_reply(&view, cx).expect("an inline reply");
    assert!(drawn("reader-inline-reply", cx));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        crate::DetailMode::Reader
    );

    let drafts = harness.store.list_drafts(None).expect("the drafts load");
    assert_eq!(drafts.len(), 1);
    let draft = harness
        .store
        .draft_detail(drafts[0].id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert!(draft.to.iter().any(|to| to.contains("priya@example.test")));
    assert!(
        draft.body.contains("> Attached is the draft roadmap."),
        "quotes the first message, not the latest: {}",
        draft.body
    );
    assert_eq!(
        cx.read_entity(&reply, |reply, _| reply.draft_id()),
        Some(drafts[0].id)
    );
}

#[gpui::test]
fn a_message_more_menu_moves_and_shows_the_plain_text(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    click("reader-message-more-2", cx);
    assert_eq!(popover(&reader, cx), Some(PopoverAnchor::MessageMore(2)));
    assert_eq!(expanded(&reader, cx), [false, false, true], "still open");

    // Move to… is first; it opens the folder picker below the button.
    keys("down enter", cx);
    assert_eq!(popover(&reader, cx), Some(PopoverAnchor::MessageMore(2)));
    keys("escape", cx);
    assert_eq!(popover(&reader, cx), None);

    // Move to…, Mark Read, then View Plain Text.
    click("reader-message-more-2", cx);
    keys("down down down enter", cx);
    assert!(drawn("reader-source-text", cx));
    keys("escape", cx);
    assert!(!drawn("reader-source-text", cx));
}

/// The bounds of the element `selector` names.
fn paragraph(selector: &'static str, cx: &mut VisualTestContext) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
}

fn selected(reader: &Entity<ReaderView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.read_entity(reader, |reader, _| reader.selected_text())
}

/// Presses at `from`, drags to `to` and releases.
fn drag(from: Point<Pixels>, to: Point<Pixels>, cx: &mut VisualTestContext) {
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn dragging_selects_body_text_and_secondary_c_copies_it(cx: &mut TestAppContext) {
    let (view, cx, reader) = open_conversation(cx);
    let first = paragraph("reader-paragraph-2-0", cx);
    let second = paragraph("reader-paragraph-2-1", cx);
    assert!(
        second.top() > first.bottom(),
        "paragraphs have space between them"
    );

    // Across the first paragraph, past its end.
    let middle = first.top() + first.size.height / 2.;
    drag(
        point(first.left() + px(1.), middle),
        point(first.right() + px(40.), middle),
        cx,
    );
    assert_eq!(
        selected(&reader, cx).as_deref(),
        Some("Thanks, I moved offline search to November.")
    );
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("secondary-c", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("Thanks, I moved offline search to November.".to_owned())
    );

    // Into the next paragraph and below the text: to the end of the body,
    // with the blank line between.
    drag(
        point(first.left() + px(1.), middle),
        point(second.left(), second.bottom() + px(30.)),
        cx,
    );
    let text = selected(&reader, cx).expect("a selection");
    assert!(text.starts_with("Thanks"), "{text}");
    assert!(text.ends_with("for now."), "{text}");
    assert!(text.contains(".\n\nThe plan"), "{text}");

    // A press outside the text clears it.
    click("reader-subject", cx);
    assert_eq!(selected(&reader, cx), None);

    // Secondary+A selects the current message's text.
    keys("secondary-a", cx);
    assert!(selected(&reader, cx).is_some_and(|text| text.ends_with("for now.")));
}

#[gpui::test]
fn html_mail_reads_as_paragraphs_with_cells_apart(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Account,
        |_| InitialView::Inbox,
        |store| {
            store_conversation(
                store,
                vec![MessageSnapshot {
                    body_text: None,
                    sanitized_html: Some(
                        zmail_security::sanitize_html_for_display(concat!(
                            "<h1>Your order</h1>",
                            "<table><tr><td>Plan</td><td>Price</td></tr>",
                            "<tr><td>Starter</td><td>EUR&nbsp;9</td></tr></table>",
                            "<p>Line one<br>line two</p>",
                            "<p>Track it at <a href=\"http://shop.example/track\">the shop</a>.</p>",
                        ))
                        .html,
                    ),
                    ..message(
                        "Shop <orders@shop.example>",
                        "2026-03-24T09:00:00Z",
                        false,
                        "Your order",
                    )
                }],
            )
        },
    );
    let reader = cx.read_entity(&view, |view, _| view.reader.clone());
    click_row(0, Modifiers::none(), cx);
    for selector in [
        "reader-paragraph-0-0",
        "reader-paragraph-0-1",
        "reader-paragraph-0-2",
        "reader-paragraph-0-3",
    ] {
        assert!(drawn(selector, cx), "{selector}");
    }
    click("reader-subject", cx);
    keys("secondary-a", cx);
    assert_eq!(
        selected(&reader, cx).as_deref(),
        Some(
            "Your order\n\nPlan  Price\nStarter  EUR 9\n\nLine one\nline two\n\n\
             Track it at the shop."
        )
    );
    let links = cx.read_entity(&reader, |reader, _| {
        reader
            .state()
            .thread
            .as_ref()
            .map(|thread| thread.messages[0].body_links.clone())
            .unwrap_or_default()
    });
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, "http://shop.example/track");
}

#[gpui::test]
fn a_link_shows_its_real_target_before_opening(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open_conversation(cx);
    let prompt = |cx: &mut VisualTestContext| {
        cx.read_entity(&reader, |reader, _| {
            reader
                .link_prompt()
                .map(|prompt| (prompt.target.clone(), prompt.shown_host.clone()))
        })
    };

    // The HTML message's second paragraph is a link showing bank.example
    // that goes to evil.example, over plain http: the scheme is the
    // original one (#198).
    click("reader-message-summary-0", cx);
    let body = paragraph("reader-paragraph-0-1", cx);
    let link = point(body.left() + px(12.), body.bottom() - px(6.));
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        prompt(cx),
        Some((
            "http://evil.example/login".to_owned(),
            Some("bank.example".to_owned())
        ))
    );
    assert!(drawn("reader-link-target", cx));
    assert!(drawn("reader-link-warning", cx));
    assert_eq!(cx.opened_url(), None, "nothing opens without confirming");

    keys("escape", cx);
    assert_eq!(prompt(cx), None);
    assert_eq!(cx.opened_url(), None);

    // A link whose text is its target opens without a warning, once
    // confirmed.
    let reply = paragraph("reader-paragraph-2-1", cx);
    let link = point(
        reply.left() + px(160.),
        reply.top() + reply.size.height / 2.,
    );
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        prompt(cx),
        Some(("https://example.com/roadmap".to_owned(), None))
    );
    assert!(!drawn("reader-link-warning", cx));
    click("reader-link-open", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/roadmap")
    );
    assert_eq!(prompt(cx), None);
}
