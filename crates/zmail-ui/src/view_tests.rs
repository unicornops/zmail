//! View tests for [`WorkspaceView`] in a GPUI test window, backed by an
//! ephemeral store and an in-memory secret store.
//!
//! [`open_workspace`] is the harness: it opens the real workspace view the
//! app uses, with the real key bindings, so tests drive it with keystrokes,
//! clicks and actions and then check the view and the store.

use std::{sync::Arc, time::Duration};

use gpui::{
    AppContext as _, Entity, Focusable, Modifiers, TestAppContext, VisualTestContext, point,
};
use zmail_core::{
    AccountSyncSnapshot, AppBootstrap, AuthKind, CachePolicySummary, MailboxSnapshot,
    MessageSnapshot, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind, RuntimeSummary,
    ThemeAppearance, ThemeSummary, ThreadSnapshot, UpstreamPins,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::account_setup::AccountField;
use crate::actions::SaveDraft;
use crate::components::{self, TextInput, text_input};
use crate::keymap;
use crate::pane::Pane;
use crate::{DetailMode, InitialView, ListMode, WorkspaceView};

/// What the store holds when the window opens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    /// No account: the account setup form opens.
    NoAccount,
    /// One IMAP account with no mail.
    Account,
    /// One IMAP account with three Inbox threads and empty Archive and
    /// Trash mailboxes.
    Mail,
}

/// Keeps the store and the sync runtime alive for the test.
struct Harness {
    store: Arc<LocalStore>,
    /// The Inbox threads, in list order.
    threads: Vec<i64>,
    inbox_id: Option<i64>,
    archive_id: Option<i64>,
    trash_id: Option<i64>,
    _runtime: tokio::runtime::Runtime,
}

/// A theme with no colour tokens, so the bundled palette applies.
fn theme(appearance: ThemeAppearance) -> ThemeSummary {
    ThemeSummary {
        family_name: "Test".to_owned(),
        theme_name: "Test".to_owned(),
        appearance,
        background: None,
        surface: None,
        border: None,
        foreground: None,
        muted_foreground: None,
        accent: None,
        selected_background: None,
        hover_background: None,
        danger: None,
        warning: None,
        success: None,
        info: None,
        button_background: None,
        danger_background: None,
        warning_background: None,
        success_background: None,
    }
}

fn thread_snapshot(index: usize) -> ThreadSnapshot {
    let subject = format!("Quarterly report {index}");
    // Newer threads list first.
    let at = format!("2026-03-2{}T09:15:00Z", 5 - index);
    ThreadSnapshot {
        remote_thread_id: format!("thread-{index}"),
        subject: subject.clone(),
        participants: "Kai Santos".to_owned(),
        preview: "Numbers attached".to_owned(),
        is_flagged: false,
        has_attachments: false,
        unread_count: 1,
        latest_message_at: at.clone(),
        messages: vec![MessageSnapshot {
            remote_id: format!("message-{index}"),
            identity_email: "me@example.test".to_owned(),
            sender: "Kai Santos <kai@example.test>".to_owned(),
            reply_to: None,
            recipients: "me@example.test".to_owned(),
            subject,
            body_preview: "Numbers attached".to_owned(),
            body_text: Some("Numbers attached".to_owned()),
            sanitized_html: None,
            has_remote_content: false,
            is_read: false,
            is_flagged: false,
            received_at: at,
            attachments: Vec::new(),
        }],
    }
}

/// Opens the workspace on the inbox in a test window with the default
/// keymap bound.
fn open_workspace(
    cx: &mut TestAppContext,
    setup: Setup,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    open_workspace_on(cx, setup, |_| InitialView::Inbox)
}

/// Like [`open_workspace`], opening the window on the view `initial_view`
/// picks from the seeded store.
fn open_workspace_on(
    cx: &mut TestAppContext,
    setup: Setup,
    initial_view: impl FnOnce(&Harness) -> InitialView,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    cx.update(|cx| {
        components::init(cx);
        text_input::init(cx);
        keymap::bind_default_keymap(cx);
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the test runtime starts");
    let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
    let mut harness = Harness {
        store: store.clone(),
        threads: Vec::new(),
        inbox_id: None,
        archive_id: None,
        trash_id: None,
        _runtime: runtime,
    };
    if setup != Setup::NoAccount {
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Work".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.test:993".to_owned(),
                username: "me@example.test".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig::smtp_default("mail.example.test")),
                identities: vec![NewIdentity {
                    display_name: "Me".to_owned(),
                    email_address: "me@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account is stored");
        if setup == Setup::Mail {
            let mailbox = |name: &str, role: &str, threads| MailboxSnapshot {
                name: name.to_owned(),
                role: Some(role.to_owned()),
                threads,
            };
            store
                .replace_account_snapshot(
                    account_id,
                    &AccountSyncSnapshot {
                        checkpoint: "test".to_owned(),
                        detail: "test".to_owned(),
                        identities: None,
                        mailboxes: vec![
                            mailbox("Inbox", "inbox", (0..3).map(thread_snapshot).collect()),
                            mailbox("Archive", "archive", Vec::new()),
                            mailbox("Trash", "trash", Vec::new()),
                        ],
                    },
                )
                .expect("the mail is stored");
            let role = |role: &str| {
                store
                    .mailbox_id_by_role(account_id, role)
                    .expect("the mailbox lookup works")
            };
            harness.inbox_id = role("inbox");
            harness.archive_id = role("archive");
            harness.trash_id = role("trash");
            harness.threads = store
                .list_threads(harness.inbox_id.expect("an inbox"), 0..10)
                .expect("the threads load")
                .into_iter()
                .map(|thread| thread.id)
                .collect();
        }
    }
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let sync_service = SyncService::new(
        harness._runtime.handle().clone(),
        store.clone(),
        secrets.clone(),
    );
    let notifier = Arc::new(zmail_notify::NewMailNotifier::os_backed());
    let bootstrap = AppBootstrap {
        runtime: RuntimeSummary {
            worker_threads: 1,
            bridge_verified: true,
        },
        theme: theme(ThemeAppearance::Dark),
        alternate_theme: theme(ThemeAppearance::Light),
        upstream: UpstreamPins {
            zed_commit: String::new(),
            gpui_version: String::new(),
        },
    };
    let initial_view = initial_view(&harness);
    let (view, cx) = cx.add_window_view(move |window, cx| {
        WorkspaceView::new(
            bootstrap,
            store,
            sync_service,
            secrets,
            notifier,
            initial_view,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    (view, cx, harness)
}

fn focus(input: &Entity<TextInput>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    cx.run_until_parked();
}

fn text(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> String {
    cx.read_entity(input, |input, _| input.text().to_owned())
}

fn cursor(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> usize {
    cx.read_entity(input, |input, _| input.cursor_offset())
}

fn keys(keystrokes: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}

fn focused_pane(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<Pane> {
    cx.update(|window, cx| view.read(cx).focused_pane(window, cx))
}

fn selected_thread(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<i64> {
    cx.read_entity(view, |view, _| view.selected_thread_id)
}

fn search_input(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Entity<TextInput> {
    cx.read_entity(view, |view, _| view.search_query.clone())
}

fn search_is_focused(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    let search = search_input(view, cx);
    cx.update(|window, cx| search.focus_handle(cx).is_focused(window))
}

/// The mailbox a thread is in now, or `None` once it is gone.
fn mailbox_of(harness: &Harness, thread_id: i64) -> Option<i64> {
    harness
        .store
        .thread_view(thread_id)
        .expect("the thread loads")
        .map(|thread| thread.mailbox_id)
}

/// The open draft's inputs: To, body and attachment path.
fn compose_inputs(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (i64, Entity<TextInput>, Entity<TextInput>, Entity<TextInput>) {
    cx.read_entity(view, |view, cx| {
        let compose = view
            .compose
            .read(cx)
            .compose
            .as_ref()
            .expect("a draft is open");
        (
            compose.draft_id,
            compose.to.clone(),
            compose.body.clone(),
            compose.attachment_path.clone(),
        )
    })
}

// ---------------------------------------------------------------------------
// Text input integration (from the TextInput change)
// ---------------------------------------------------------------------------

#[gpui::test]
fn compose_autosave_keeps_the_cursor_and_the_attachment_path(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (draft_id, to, body, attachment_path) = compose_inputs(&view, cx);
    assert!(
        cx.update(|window, cx| to.focus_handle(cx).is_focused(window)),
        "Compose focuses the To field"
    );

    // Edit the middle of the body: every keystroke autosaves.
    focus(&body, cx);
    cx.simulate_input("Hello world");
    cx.simulate_keystrokes("left left left left left");
    cx.simulate_input("big ");
    assert_eq!(text(&body, cx), "Hello big world");
    assert_eq!(cursor(&body, cx), "Hello big ".len());
    let stored = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(stored.body, "Hello big world");

    // Half-type an attachment path, then let the debounced refresh and the
    // five-second background refresh run.
    focus(&attachment_path, cx);
    cx.simulate_input("/tmp/half");
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();

    let (_, _, same_body, same_path) = compose_inputs(&view, cx);
    assert!(
        same_body == body && same_path == attachment_path,
        "the refresh does not rebuild the compose inputs"
    );
    assert_eq!(text(&body, cx), "Hello big world");
    assert_eq!(
        cursor(&body, cx),
        "Hello big ".len(),
        "the cursor stays put"
    );
    assert_eq!(text(&attachment_path, cx), "/tmp/half");
    cx.simulate_input("way");
    assert_eq!(text(&attachment_path, cx), "/tmp/halfway");

    // Saving explicitly keeps the inputs too.
    cx.dispatch_action(SaveDraft);
    cx.run_until_parked();
    assert_eq!(cursor(&body, cx), "Hello big ".len());
    let (_, _, same_body, _) = compose_inputs(&view, cx);
    assert!(
        same_body == body,
        "saving does not rebuild the compose inputs"
    );
}

#[gpui::test]
fn compose_subject_updates_the_drafts_list_after_autosave(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let subject = cx.read_entity(&view, |view, cx| {
        view.compose
            .read(cx)
            .compose
            .as_ref()
            .expect("a draft is open")
            .subject
            .clone()
    });
    focus(&subject, cx);
    cx.simulate_input("Quarterly report");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let subjects = cx.read_entity(&view, |view, _| {
        view.drafts
            .iter()
            .map(|draft| draft.subject.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(subjects, vec!["Quarterly report".to_owned()]);
}

#[gpui::test]
fn search_field_enter_runs_the_search(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Account);
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("invoice");
    cx.simulate_keystrokes("enter");
    let (list_mode, query) = cx.read_entity(&view, |view, _| {
        (view.list_mode, view.search_results_query.clone())
    });
    assert_eq!(list_mode, ListMode::Search);
    assert_eq!(query, "invoice");
}

#[gpui::test]
fn account_setup_focuses_fields_and_clears_errors_on_edit(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    assert!(cx.read_entity(&view, |view, _| matches!(
        view.detail_mode,
        DetailMode::AccountSetup
    )));
    let input = |field: AccountField, cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| {
            view.account_setup.read(cx).form.input(field).clone()
        })
    };
    let has_error = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| {
            view.account_setup
                .read(cx)
                .form
                .errors
                .contains_key(&AccountField::Endpoint)
        })
    };

    // With no account, the form opens with the first field focused.
    cx.simulate_input("Work");
    let display_name = input(AccountField::DisplayName, cx);
    assert_eq!(text(&display_name, cx), "Work");

    // Secondary+Enter submits; it reports the missing fields and focuses
    // the first one.
    keys("secondary-enter", cx);
    assert!(has_error(cx));

    // Typing goes to the focused endpoint field and clears its error.
    cx.simulate_input("imaps://mail.example.test");
    let endpoint = input(AccountField::Endpoint, cx);
    assert_eq!(text(&endpoint, cx), "imaps://mail.example.test");
    assert!(!has_error(cx));
    assert_eq!(text(&display_name, cx), "Work");

    // Escape does not close the form while there is no account to show.
    keys("escape", cx);
    assert!(cx.read_entity(&view, |view, _| matches!(
        view.detail_mode,
        DetailMode::AccountSetup
    )));
}

// ---------------------------------------------------------------------------
// Pane focus and the keymap
// ---------------------------------------------------------------------------

#[gpui::test]
fn tab_and_f6_cycle_focus_between_panes(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    assert_eq!(
        focused_pane(&view, cx),
        Some(Pane::MessageList),
        "the message list has focus at startup"
    );

    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    keys("shift-tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    keys("shift-f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));

    // F6 also leaves a text field: from the search field it goes to the
    // first pane.
    keys("secondary-f", cx);
    assert!(search_is_focused(&view, cx));
    keys("f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));

    // Moving into compose focuses its first field, ready to type.
    keys("secondary-n", cx);
    keys("f6 f6 f6", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    let (_, to, _, _) = compose_inputs(&view, cx);
    assert!(cx.update(|window, cx| to.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn arrow_keys_move_the_selection_only_in_the_focused_pane(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));

    // The message list has focus: the arrows move through the threads.
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    keys("down down down", cx);
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[2]),
        "the selection stops at the last thread"
    );
    keys("up", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    keys("home", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));
    keys("end", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));

    // In the reader the arrows scroll; the selection stays.
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("up up down", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));

    // In the sidebar the arrows move through the mailboxes.
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("down", cx);
    let mailbox = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(
        mailbox, harness.archive_id,
        "down from Inbox selects the next mailbox"
    );
    keys("up", cx);
    let mailbox = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(mailbox, harness.inbox_id);
    // Enter moves on to the list.
    keys("enter", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

#[gpui::test]
fn typing_in_a_text_input_never_triggers_pane_shortcuts(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    let search = search_input(&view, cx);
    focus(&search, cx);

    cx.simulate_input("abc");
    // Keys the list and reader bind (delete, arrows, archive, flag, read,
    // undo) edit the text or do nothing while the field has focus.
    keys(
        "down up home end delete secondary-e secondary-shift-l secondary-shift-u",
        cx,
    );
    cx.simulate_input("d");
    assert_eq!(text(&search, cx), "abcd", "the keys only moved the cursor");
    assert_eq!(selected_thread(&view, cx), Some(first));
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);
    let thread = harness
        .store
        .thread_view(first)
        .expect("the thread loads")
        .expect("the thread exists");
    assert!(!thread.is_flagged);
    assert_eq!(thread.unread_count, 1);

    // Backspace edits the field rather than deleting the conversation.
    keys("backspace", cx);
    assert_eq!(text(&search, cx), "abc");
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);
}

#[gpui::test]
fn clicking_a_thread_moves_focus_to_the_list(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("abc");

    let bounds = cx
        .debug_bounds("thread-row-1")
        .expect("the second thread row is drawn");
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[1]));

    // Keys now go to the list, not to the field clicked last.
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[2]));
    assert_eq!(text(&search, cx), "abc");
}

#[gpui::test]
fn escape_closes_overlays(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);

    // The error banner.
    view.update(cx, |view, cx| {
        view.store_error = Some("Something failed.".to_owned());
        cx.notify();
    });
    keys("escape", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.store_error.clone()),
        None
    );

    // The account form, when there is an account to go back to.
    keys("secondary-shift-a", cx);
    assert!(cx.read_entity(&view, |view, _| matches!(
        view.detail_mode,
        DetailMode::AccountSetup
    )));
    keys("escape", cx);
    assert!(cx.read_entity(&view, |view, _| matches!(
        view.detail_mode,
        DetailMode::Reader
    )));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // The search field, then the search results.
    keys("secondary-f", cx);
    cx.simulate_input("Quarterly");
    keys("enter", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.list_mode),
        ListMode::Search
    );
    keys("escape", cx);
    assert!(!search_is_focused(&view, cx));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    keys("escape", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.list_mode),
        ListMode::Mail
    );
}

#[gpui::test]
fn every_toolbar_action_is_dispatchable_by_keybinding(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let modes = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| (view.list_mode, view.detail_mode))
    };
    let status = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| {
            view.status_message.clone().unwrap_or_default()
        })
    };

    // Mail / Drafts / Outbox.
    keys("secondary-2", cx);
    assert_eq!(modes(cx), (ListMode::Drafts, DetailMode::Compose));
    keys("secondary-3", cx);
    assert_eq!(modes(cx).0, ListMode::Outbox);
    // Retry Outbox (nothing queued, so it explains why).
    keys("secondary-shift-o", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.store_error.clone()),
        Some("Select a message in the outbox first.".to_owned())
    );
    keys("escape", cx);
    keys("secondary-1", cx);
    assert_eq!(modes(cx), (ListMode::Mail, DetailMode::Reader));

    // Theme.
    let appearance = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| view.bootstrap.theme.appearance)
    };
    let before = appearance(cx);
    keys("secondary-shift-t", cx);
    assert_ne!(appearance(cx), before);

    // Sync Selected and Sync All.
    keys("f5", cx);
    assert_eq!(status(cx), "Syncing Work…");
    keys("shift-f5", cx);
    // The account is already syncing, so Sync All starts nothing new.
    assert_eq!(status(cx), "Syncing 0 accounts…");

    // Search (Enter in the field runs it; see search_field_enter_runs_the_search).
    keys("secondary-f", cx);
    assert!(search_is_focused(&view, cx));
    keys("escape", cx);

    // Add Account.
    keys("secondary-shift-a", cx);
    assert_eq!(modes(cx).1, DetailMode::AccountSetup);
    keys("escape", cx);

    // Compose.
    keys("secondary-n", cx);
    assert_eq!(modes(cx), (ListMode::Drafts, DetailMode::Compose));
    let (_, to, _, _) = compose_inputs(&view, cx);
    assert!(cx.update(|window, cx| to.focus_handle(cx).is_focused(window)));
    // Save Draft works from inside a text field.
    keys("secondary-s", cx);
    assert_eq!(status(cx), "Draft saved.");
}

#[gpui::test]
fn conversation_actions_have_keys_and_undo(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    let thread = |cx: &mut VisualTestContext| {
        let _ = cx;
        harness
            .store
            .thread_view(first)
            .expect("the thread loads")
            .expect("the thread exists")
    };
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Flag, read, and undo, from the list.
    keys("secondary-shift-l", cx);
    assert!(thread(cx).is_flagged);
    keys("secondary-shift-u", cx);
    assert_eq!(thread(cx).unread_count, 0);
    keys("secondary-z", cx);
    assert_eq!(thread(cx).unread_count, 1, "undo restored the unread state");
    assert!(thread(cx).is_flagged);

    // Archive and undo, from the reader.
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    keys("secondary-e", cx);
    assert_eq!(mailbox_of(&harness, first), harness.archive_id);
    keys("secondary-z", cx);
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);

    // Delete moves to the trash.
    keys("shift-tab", cx);
    view.update(cx, |view, cx| {
        view.selected_thread_id = Some(first);
        cx.notify();
    });
    keys("delete", cx);
    assert_eq!(mailbox_of(&harness, first), harness.trash_id);
    keys("secondary-z", cx);
    assert_eq!(mailbox_of(&harness, first), harness.inbox_id);

    // Reply opens a draft with the cursor in the body.
    view.update(cx, |view, cx| {
        view.selected_thread_id = Some(first);
        cx.notify();
    });
    keys("secondary-r", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::Compose
    );
    let (draft_id, _, body, _) = compose_inputs(&view, cx);
    assert!(cx.update(|window, cx| body.focus_handle(cx).is_focused(window)));
    let draft = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(draft.subject, "Re: Quarterly report 0");
    assert_eq!(draft.to, vec!["Kai Santos <kai@example.test>".to_owned()]);

    // Send queues it.
    keys("secondary-enter", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.status_message.clone()),
        Some("Message queued for sending.".to_owned())
    );
}

#[gpui::test]
fn hovering_a_row_re_renders_only_its_pane_without_querying_the_store(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let counters = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, cx| {
            (
                view.sidebar.read(cx).counters,
                view.message_list.read(cx).counters,
                view.reader.read(cx).counters,
            )
        })
    };
    let first = cx.debug_bounds("thread-row-0").expect("row 0 is drawn");
    let second = cx.debug_bounds("thread-row-1").expect("row 1 is drawn");
    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.run_until_parked();
    let (sidebar, list, reader) = counters(cx);

    cx.simulate_mouse_move(second.center(), None, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_move(
        point(second.center().x, second.center().y + gpui::px(1.)),
        None,
        Modifiers::none(),
    );
    cx.run_until_parked();
    let (sidebar_after, list_after, reader_after) = counters(cx);

    assert!(
        list_after.renders > list.renders,
        "the hovered list re-rendered"
    );
    assert_eq!(
        list_after.queries, list.queries,
        "re-rendering reused the cached rows"
    );
    assert_eq!(
        sidebar_after.renders, sidebar.renders,
        "the sidebar did not"
    );
    assert_eq!(reader_after.renders, reader.renders, "the reader did not");
}

#[gpui::test]
fn the_window_can_open_on_a_thread_without_marking_it_read(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_on(cx, Setup::Mail, |harness| {
        InitialView::Thread(harness.threads[2])
    });
    let third = harness.threads[2];
    assert_eq!(selected_thread(&view, cx), Some(third));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::Reader
    );
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    let thread = harness
        .store
        .thread_view(third)
        .expect("the thread loads")
        .expect("the thread exists");
    assert_eq!(
        thread.unread_count, 1,
        "opening on a thread leaves it unread"
    );
}

#[gpui::test]
fn the_window_can_open_on_account_setup(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_on(cx, Setup::Mail, |_| InitialView::AccountSetup);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::AccountSetup
    );
    // The first field has focus, ready to type.
    cx.simulate_input("Work");
    let display_name = cx.read_entity(&view, |view, cx| {
        view.account_setup
            .read(cx)
            .form
            .input(AccountField::DisplayName)
            .clone()
    });
    assert_eq!(text(&display_name, cx), "Work");
}
