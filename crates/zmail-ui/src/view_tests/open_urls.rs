//! `mailto:` links handed to the running app (#457): from the system's
//! open-URL event or a second launch, held until the store is unlocked,
//! then a filled compose window each.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_security::mailto::MailtoDraft;

use super::{Harness, Setup, open_workspace};
use crate::WorkspaceView;
use crate::open_urls::{self, OpenUrlInbox};

const LINK: &str = "mailto:ada@example.test,bo@example.test?cc=lead@example.test\
                    &subject=Picnic%20plans&body=Bring%20snacks%0D%0Aplease";

fn compose_windows(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> usize {
    cx.read_entity(view, |view, _| view.compose_windows.len())
}

/// The stored drafts, newest last, as mailto fields.
fn drafts(harness: &Harness) -> Vec<MailtoDraft> {
    let mut drafts = harness.store.list_drafts(None).expect("the drafts load");
    drafts.sort_by_key(|draft| draft.id);
    drafts
        .into_iter()
        .map(|summary| {
            let draft = harness
                .store
                .draft_detail(summary.id)
                .expect("the draft loads")
                .expect("the draft exists");
            MailtoDraft {
                to: draft.to,
                cc: draft.cc,
                bcc: draft.bcc,
                subject: draft.subject,
                body: draft.body,
            }
        })
        .collect()
}

#[gpui::test]
fn a_link_before_unlocking_waits_then_opens_a_filled_compose_window(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);

    // The store is still locked: no main window to open it in yet.
    cx.update(|_, cx| open_urls::receive(LINK, cx));
    cx.run_until_parked();
    assert_eq!(compose_windows(&view, cx), 0);
    assert_eq!(cx.update(|_, cx| OpenUrlInbox::held(cx)).len(), 1);
    assert!(drafts(&harness).is_empty());

    // Unlocked: the main window is open, and the link opens in it.
    let handle = cx
        .update(|window, _| window.window_handle().downcast::<WorkspaceView>())
        .expect("the workspace window");
    cx.update(|_, cx| open_urls::attach(handle, cx));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| OpenUrlInbox::held(cx)).is_empty());
    assert_eq!(compose_windows(&view, cx), 1);
    let expected = MailtoDraft {
        to: vec!["ada@example.test".to_owned(), "bo@example.test".to_owned()],
        cc: vec!["lead@example.test".to_owned()],
        bcc: Vec::new(),
        subject: "Picnic plans".to_owned(),
        body: "Bring snacks\r\nplease".to_owned(),
    };
    assert_eq!(drafts(&harness), [expected]);
    let (compose, _) = super::compose_window(&view, cx);
    let draft_id = cx.read_entity(&compose, |compose, _| compose.draft_id());
    let draft = harness
        .store
        .draft_detail(draft_id.expect("a draft"))
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(draft.identity_email, "me@example.test");

    // While Zmail runs, each link opens its own window at once.
    cx.update(|_, cx| open_urls::receive("mailto:kai@example.test", cx));
    cx.run_until_parked();
    assert_eq!(compose_windows(&view, cx), 2);
    assert_eq!(
        drafts(&harness)[1],
        MailtoDraft {
            to: vec!["kai@example.test".to_owned()],
            ..MailtoDraft::default()
        }
    );
}

#[gpui::test]
fn links_through_the_channel_open_once_the_window_does(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    let (sender, receiver) = crate::url_channel();
    cx.update(|_, cx| open_urls::listen(receiver, cx));
    assert!(sender.send(LINK.to_owned()));
    cx.run_until_parked();
    assert_eq!(compose_windows(&view, cx), 0, "held until the window opens");

    let handle = cx
        .update(|window, _| window.window_handle().downcast::<WorkspaceView>())
        .expect("the workspace window");
    cx.update(|_, cx| open_urls::attach(handle, cx));
    cx.run_until_parked();
    assert_eq!(compose_windows(&view, cx), 1);
    assert_eq!(drafts(&harness).len(), 1);
}

#[gpui::test]
fn anything_but_a_mailto_link_is_dropped(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    let handle = cx
        .update(|window, _| window.window_handle().downcast::<WorkspaceView>())
        .expect("the workspace window");
    cx.update(|_, cx| open_urls::attach(handle, cx));
    for link in [
        "https://example.com/",
        "file:///etc/passwd",
        "zmail://settings",
        "mailto:a@example.test%0D%0ARCPT%20TO:<b@example.test>",
        "",
    ] {
        cx.update(|_, cx| open_urls::receive(link, cx));
    }
    cx.run_until_parked();
    assert_eq!(compose_windows(&view, cx), 0);
    assert!(drafts(&harness).is_empty());
    assert!(cx.update(|_, cx| OpenUrlInbox::held(cx)).is_empty());
}
