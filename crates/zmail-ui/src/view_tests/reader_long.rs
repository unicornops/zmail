//! A long message body in the reader: laid out a chunk per conversation
//! list item (#122), with selection, copy, links and the message's
//! Article working across the chunks.

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, Pixels, Role, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point, px,
};
use zmail_core::{AccountSyncSnapshot, MailboxSnapshot, MessageSnapshot, ThreadSnapshot};
use zmail_store::LocalStore;

use super::{Setup, keys, open_workspace_with};
use crate::a11y::audit;
use crate::pane::Pane;
use crate::reader::ReaderView;
use crate::{InitialView, WorkspaceView};

/// Paragraphs in the long body.
const PARAGRAPHS: usize = 300;

/// Paragraph `index` of the long body; the last one has a link.
fn paragraph_text(index: usize) -> String {
    if index + 1 == PARAGRAPHS {
        return "The full report is at https://example.com/report today.".to_owned();
    }
    format!("Paragraph {index}: the budget review notes for the launch, with the numbers.")
}

fn long_body() -> String {
    (0..PARAGRAPHS)
        .map(paragraph_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One unread conversation of one message with the long body.
fn seed(store: &LocalStore) {
    let account_id = store.list_accounts().expect("the accounts load")[0].id;
    let body = long_body();
    let message = MessageSnapshot {
        remote_id: "long".to_owned(),
        identity_email: "me@example.test".to_owned(),
        sender: "Priya Raman <priya@example.test>".to_owned(),
        reply_to: None,
        recipients: "me@example.test".to_owned(),
        subject: "Launch notes".to_owned(),
        body_preview: body.chars().take(80).collect(),
        body_text: Some(body),
        sanitized_html: None,
        raw_html: None,
        has_remote_content: false,
        is_read: false,
        is_flagged: false,
        received_at: "2026-03-24T11:00:00Z".to_owned(),
        attachments: Vec::new(),
    };
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![MailboxSnapshot {
                    name: "Inbox".to_owned(),
                    role: Some("inbox".to_owned()),
                    threads: vec![ThreadSnapshot {
                        remote_thread_id: "long".to_owned(),
                        subject: "Launch notes".to_owned(),
                        participants: "Priya Raman <priya@example.test>".to_owned(),
                        preview: "Paragraph 0".to_owned(),
                        is_flagged: false,
                        has_attachments: false,
                        unread_count: 1,
                        latest_message_at: "2026-03-24T11:00:00Z".to_owned(),
                        messages: vec![message],
                    }],
                    ..Default::default()
                }],
            },
        )
        .expect("the mail is stored");
}

fn open(
    cx: &mut TestAppContext,
) -> (
    Entity<WorkspaceView>,
    &mut VisualTestContext,
    Entity<ReaderView>,
) {
    let (view, cx, _harness) =
        open_workspace_with(cx, Setup::Account, |_| InitialView::Inbox, seed);
    let reader = cx.read_entity(&view, |view, _| view.reader.clone());
    (view, cx, reader)
}

fn bounds(selector: &'static str, cx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
    cx.debug_bounds(selector)
}

/// The bounds of `selector` once it is drawn inside the window (the list
/// also lays out a little beyond it).
fn on_screen(selector: &'static str, cx: &mut VisualTestContext) -> Option<Bounds<Pixels>> {
    let height = cx.update(|window, _| window.viewport_size().height);
    bounds(selector, cx).filter(|found| found.bottom() < height - px(40.))
}

/// Scrolls the reader down by `pixels`, a wheel step at a time, with the
/// pointer at `at`.
fn scroll_reader(at: gpui::Point<Pixels>, pixels: f32, cx: &mut VisualTestContext) {
    let mut left = pixels;
    while left > 0. {
        let step = left.min(200.);
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.), px(-step))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        left -= step;
    }
}

#[gpui::test]
fn a_long_body_is_laid_out_in_parts_and_selects_across_them(cx: &mut TestAppContext) {
    audit::start();
    let (_view, cx, reader) = open(cx);
    let entries = audit::finish();

    // The message takes several list items, and only the part on screen
    // is laid out.
    let parts = cx.read_entity(&reader, |reader, _| reader.message_parts(0));
    assert!(parts > 2, "the long body is split ({parts} parts)");
    assert!(bounds("reader-paragraph-0-0", cx).is_some());
    assert!(bounds("reader-paragraph-0-299", cx).is_none());

    // The Article names the message.
    let mut articles = audit::labels(&entries, Role::Article);
    articles.dedup();
    assert_eq!(articles.len(), 1, "one message: {articles:?}");
    assert!(articles[0].contains("Priya Raman"), "{articles:?}");

    // A drag from the first paragraph, scrolled down past the first part
    // while the button is held, selects across the parts.
    let first = bounds("reader-paragraph-0-0", cx).expect("the first paragraph is drawn");
    let start = point(first.left() + px(1.), first.top() + first.size.height / 2.);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    audit::start();
    let mut target = None;
    for _ in 0..200 {
        scroll_reader(start, 200., cx);
        if let Some(found) = on_screen("reader-paragraph-0-150", cx) {
            target = Some(found);
            break;
        }
    }
    let target = target.expect("scrolling reaches paragraph 150");
    // The later parts say which message they continue.
    let entries = audit::finish();
    assert!(
        audit::labels(&entries, Role::Group)
            .iter()
            .any(|label| label.ends_with(", continued") && label.contains("Priya Raman")),
        "{entries:?}"
    );
    assert!(
        bounds("reader-paragraph-0-0", cx).is_none(),
        "the first part is off screen"
    );
    let end = point(
        target.right() + px(40.),
        target.top() + target.size.height / 2.,
    );
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let expected = (0..=150)
        .map(paragraph_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    let selected = cx.read_entity(&reader, |reader, _| reader.selected_text());
    assert_eq!(selected.as_deref(), Some(expected.as_str()));
    keys("secondary-c", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(expected)
    );

    // Secondary+A selects the whole body, every part.
    keys("secondary-a", cx);
    let all = cx.read_entity(&reader, |reader, _| reader.selected_text());
    assert_eq!(all, Some(long_body()));
}

#[gpui::test]
fn a_link_in_a_later_part_opens_its_prompt(cx: &mut TestAppContext) {
    let (_view, cx, reader) = open(cx);
    let first = bounds("reader-paragraph-0-0", cx).expect("the first paragraph is drawn");
    let over = point(first.left() + px(1.), first.top());
    let mut last = None;
    for _ in 0..400 {
        scroll_reader(over, 200., cx);
        if let Some(found) = on_screen("reader-paragraph-0-299", cx) {
            last = Some(found);
            break;
        }
    }
    let last = last.expect("scrolling reaches the last paragraph");
    // "The full report is at " is 22 characters, and the test platform's
    // text is 8.4 px a character: the URL runs from about 185 px to 400.
    let link = point(last.left() + px(250.), last.top() + last.size.height / 2.);
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    let prompt = cx.read_entity(&reader, |reader, _| {
        reader.link_prompt().map(|prompt| prompt.target.clone())
    });
    assert_eq!(prompt.as_deref(), Some("https://example.com/report"));
}

#[gpui::test]
fn collapsing_a_long_message_takes_its_parts_out_of_the_list(cx: &mut TestAppContext) {
    let (view, cx, reader) = open(cx);
    let parts =
        |cx: &mut VisualTestContext| cx.read_entity(&reader, |reader, _| reader.message_parts(0));
    let expanded = parts(cx);
    assert!(expanded > 2);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Detail, window, cx)
    });
    keys("enter", cx);
    assert_eq!(parts(cx), 1, "a collapsed message is one item");
    assert!(bounds("reader-message-summary-0", cx).is_some());
    keys("enter", cx);
    assert_eq!(parts(cx), expanded);
    assert!(bounds("reader-paragraph-0-0", cx).is_some());
}
