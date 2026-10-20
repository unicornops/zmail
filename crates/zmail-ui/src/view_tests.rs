//! View tests for [`WorkspaceView`] in a GPUI test window, backed by an
//! ephemeral store and an in-memory secret store.
//!
//! [`open_workspace`] is the harness: it opens the real workspace view the
//! app uses, with the real key bindings, so tests drive it with keystrokes,
//! clicks and actions and then check the view and the store.

use std::{sync::Arc, time::Duration};

use gpui::{
    AppContext as _, Entity, Focusable, Modifiers, TestAppContext, VisualTestContext, point, px,
    size,
};
use zmail_core::{
    AccountSyncSnapshot, AppBootstrap, AuthKind, CachePolicySummary, MailboxSnapshot,
    MessageSnapshot, NewAccount, NewIdentity, OutgoingConfig, ProtocolKind, RuntimeSummary,
    ThemeAppearance, ThemeSummary, ThreadSnapshot, UpstreamPins, preferences::PaneWidths,
};
use zmail_core::{MailboxRole, MailboxSelection, NewDraft, VirtualMailbox, preferences::FolderRef};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::account_setup::AccountField;
use crate::components::{self, TextInput};
use crate::compose::ComposeView;
use crate::keymap;
use crate::layout::{LayoutMode, SinglePane};
use crate::pane::Pane;
use crate::styles::{DETAIL_MIN_WIDTH, MESSAGE_LIST_WIDTH, SIDEBAR_WIDTH};
use crate::{DetailMode, InitialView, ListMode, WorkspaceView};

mod compose;
mod list;
mod reader;
mod reader_attachments;
mod reader_safety;

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
    /// `Mail` plus a second account, "Home", whose Inbox holds the newest
    /// thread (flagged) and the oldest.
    Accounts,
}

/// Keeps the store and the sync runtime alive for the test.
struct Harness {
    store: Arc<LocalStore>,
    /// The Inbox threads, in list order.
    threads: Vec<i64>,
    inbox_id: Option<i64>,
    archive_id: Option<i64>,
    trash_id: Option<i64>,
    /// The "Home" account of [`Setup::Accounts`].
    home_account_id: Option<i64>,
    home_inbox_id: Option<i64>,
    /// Home's Inbox threads, newest first.
    home_threads: Vec<i64>,
    _runtime: tokio::runtime::Runtime,
}

/// A theme with no colour tokens, so the bundled palette applies.
pub(crate) fn theme(appearance: ThemeAppearance) -> ThemeSummary {
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

/// Adds [`Setup::Accounts`]' second account.
fn add_home_account(store: &LocalStore, harness: &mut Harness) {
    let account_id = store
        .insert_account(&NewAccount {
            display_name: "Home".to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: "imaps://mail.example.org:993".to_owned(),
            username: "me@example.org".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(OutgoingConfig::smtp_default("mail.example.org")),
            identities: vec![NewIdentity {
                display_name: "Me at home".to_owned(),
                email_address: "me@example.org".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        })
        .expect("the account is stored");
    let thread = |id: &str, at: &str, flagged: bool| {
        let mut thread = thread_snapshot(0);
        thread.remote_thread_id = id.to_owned();
        thread.subject = format!("Home {id}");
        thread.latest_message_at = at.to_owned();
        thread.is_flagged = flagged;
        for message in &mut thread.messages {
            message.remote_id = format!("home-{id}");
            message.identity_email = "me@example.org".to_owned();
            message.received_at = at.to_owned();
            message.is_flagged = flagged;
        }
        thread
    };
    let folder = |name: &str, parent: Option<&str>| MailboxSnapshot {
        name: name.to_owned(),
        delimiter: Some("/".to_owned()),
        parent: parent.map(str::to_owned),
        ..Default::default()
    };
    store
        .replace_account_snapshot(
            account_id,
            &AccountSyncSnapshot {
                checkpoint: "test".to_owned(),
                detail: "test".to_owned(),
                identities: None,
                mailboxes: vec![
                    MailboxSnapshot {
                        name: "INBOX".to_owned(),
                        role: Some("inbox".to_owned()),
                        threads: vec![
                            thread("newest", "2026-03-26T09:15:00Z", true),
                            thread("oldest", "2026-03-01T09:15:00Z", false),
                        ],
                        ..Default::default()
                    },
                    MailboxSnapshot {
                        name: "Sent".to_owned(),
                        role: Some("sent".to_owned()),
                        ..Default::default()
                    },
                    folder("Projects", None),
                    folder("Projects/Website", Some("Projects")),
                    // "Lists" holds no mail, so it is not stored.
                    folder("Lists/rust", Some("Lists")),
                ],
            },
        )
        .expect("the mail is stored");
    let inbox_id = store
        .mailbox_id_by_role(account_id, "inbox")
        .expect("the mailbox lookup works");
    harness.home_account_id = Some(account_id);
    harness.home_inbox_id = inbox_id;
    harness.home_threads = store
        .list_threads(inbox_id.expect("an inbox"), 0..10)
        .expect("the threads load")
        .into_iter()
        .map(|thread| thread.id)
        .collect();
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
    open_workspace_with(cx, setup, initial_view, |_| {})
}

/// Like [`open_workspace_on`], running `prepare` on the seeded store
/// before the window opens (to save UI preferences, for example).
fn open_workspace_with(
    cx: &mut TestAppContext,
    setup: Setup,
    initial_view: impl FnOnce(&Harness) -> InitialView,
    prepare: impl FnOnce(&LocalStore),
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
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
        home_account_id: None,
        home_inbox_id: None,
        home_threads: Vec::new(),
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
        if matches!(setup, Setup::Mail | Setup::Accounts) {
            let mailbox = |name: &str, role: &str, threads| MailboxSnapshot {
                name: name.to_owned(),
                role: Some(role.to_owned()),
                threads,
                ..Default::default()
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
    if setup == Setup::Accounts {
        add_home_account(&store, &mut harness);
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
    prepare(&store);
    // The keymap the saved preferences pick, as `run()` binds it.
    let saved_keymap = store.ui_preferences().expect("the preferences load").keymap;
    cx.update(|cx| keymap::bind_keymap(saved_keymap, cx));
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

/// The only open compose window's editor, and a test context for that
/// window.
pub(super) fn compose_window(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (Entity<ComposeView>, VisualTestContext) {
    let handle = cx.read_entity(view, |view, _| {
        assert_eq!(view.compose_windows.len(), 1, "one compose window is open");
        view.compose_windows
            .values()
            .next()
            .map(|(handle, _)| *handle)
            .expect("a compose window")
    });
    let compose = handle
        .read_with(cx, |root, _| root.compose().clone())
        .expect("the compose window is open");
    let window_cx = VisualTestContext::from_window(handle.into(), cx);
    (compose, window_cx)
}

/// The toasts showing in the only open compose window.
pub(super) fn compose_window_toasts(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Vec<String> {
    let handle = cx.read_entity(view, |view, _| {
        view.compose_windows
            .values()
            .next()
            .map(|(handle, _)| *handle)
            .expect("a compose window")
    });
    handle
        .read_with(cx, |root, cx| root.toast_messages(cx))
        .expect("the compose window is open")
}

/// The inline reply below the open conversation, if one shows.
pub(super) fn inline_reply(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Option<Entity<ComposeView>> {
    let reader = cx.read_entity(view, |view, _| view.reader.clone());
    cx.read_entity(&reader, |reader, _| reader.state().reply.clone())
}

/// The compose pane's draft and its To and body inputs.
pub(super) fn compose_inputs(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (i64, Entity<TextInput>, Entity<TextInput>) {
    cx.read_entity(view, |view, cx| {
        let compose = view
            .compose
            .read(cx)
            .compose
            .as_ref()
            .expect("a draft is open");
        (
            compose.draft_id,
            compose.to.read(cx).input().clone(),
            compose.body.clone(),
        )
    })
}

// ---------------------------------------------------------------------------
// Text input integration (from the TextInput change)
// ---------------------------------------------------------------------------

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
fn recorded_server_hits_show_in_the_results_marked(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    // A word only the server knows, as the fallback records it.
    harness
        .store
        .record_server_search_hits(
            account_id,
            "albatross",
            &[zmail_core::ServerHitRef {
                mailbox_name: Some("Inbox".to_owned()),
                remote_id: "message-2".to_owned(),
            }],
        )
        .expect("hits record");
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("albatross");
    cx.simulate_keystrokes("enter");
    let results = cx.read_entity(&view, |view, _| {
        view.search_results
            .iter()
            .map(|hit| (hit.subject.clone(), hit.from_server))
            .collect::<Vec<_>>()
    });
    assert_eq!(results, vec![("Quarterly report 2".to_owned(), true)]);
}

#[gpui::test]
fn search_field_understands_query_operators(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    let select_all = if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    };
    let run = |query: &str, cx: &mut VisualTestContext| {
        focus(&search, cx);
        cx.simulate_keystrokes(select_all);
        cx.simulate_keystrokes("backspace");
        cx.simulate_input(query);
        cx.simulate_keystrokes("enter");
        cx.read_entity(&view, |view, _| {
            view.search_results
                .iter()
                .map(|hit| hit.subject.clone())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        run("from:kai is:unread \"report 1\"", cx),
        vec!["Quarterly report 1".to_owned()]
    );
    assert!(run("quarterly -from:kai", cx).is_empty());
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
    keys("secondary-2", cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_compose_in_pane_for_test(window, cx);
        })
    });
    cx.run_until_parked();
    for _ in 0..3 {
        if focused_pane(&view, cx) == Some(Pane::Detail) {
            break;
        }
        keys("f6", cx);
    }
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    let (_, to, _) = compose_inputs(&view, cx);
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

    // In the sidebar the arrows move through the rows: Inbox, then the
    // Favourites, then the account's folders.
    keys("tab", cx);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    let selection =
        |cx: &mut VisualTestContext| cx.read_entity(&view, |view, _| view.mailbox_selection());
    keys("down", cx);
    assert_eq!(
        selection(cx),
        Some(MailboxSelection::Virtual(VirtualMailbox::Flagged)),
        "down from Inbox selects Flagged"
    );
    keys("end", cx);
    let mailbox = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(mailbox, harness.trash_id, "the last folder is Trash");
    keys("up", cx);
    let mailbox = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(mailbox, harness.archive_id);
    keys("home", cx);
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

    // Sidebar.
    keys("secondary-b", cx);
    assert!(cx.read_entity(&view, |view, _| !view.sidebar_shown()));
    keys("secondary-b", cx);
    assert!(cx.read_entity(&view, |view, _| view.sidebar_shown()));

    // Theme.
    let appearance = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| view.bootstrap.theme.appearance)
    };
    let before = appearance(cx);
    keys("secondary-shift-t", cx);
    assert_ne!(appearance(cx), before);

    // Sync All and Sync Selected. Sync All goes first: it starts the one
    // account's sync, and Sync Selected reports the same whether that sync
    // is still running or has already failed (it runs on real threads, so
    // the test cannot know which).
    keys("shift-f5", cx);
    assert_eq!(status(cx), "Syncing 1 account…");
    keys("f5", cx);
    assert_eq!(status(cx), "Syncing Work…");

    // Search (Enter in the field runs it; see search_field_enter_runs_the_search).
    keys("secondary-f", cx);
    assert!(search_is_focused(&view, cx));
    keys("escape", cx);

    // Add Account.
    keys("secondary-shift-a", cx);
    assert_eq!(modes(cx).1, DetailMode::AccountSetup);
    keys("escape", cx);

    // Compose opens a window with focus in To.
    keys("secondary-n", cx);
    let (compose, mut window_cx) = compose_window(&view, cx);
    let to = window_cx.read_entity(&compose, |compose, cx| {
        compose
            .compose
            .as_ref()
            .expect("a draft is open")
            .to
            .read(cx)
            .input()
            .clone()
    });
    assert!(window_cx.update(|window, cx| to.focus_handle(cx).is_focused(window)));
    // Save Draft works from inside a text field, and says so in the
    // compose window.
    keys("secondary-s", &mut window_cx);
    assert_eq!(compose_window_toasts(&view, cx), ["Draft saved."]);
}

/// The messages of the visible toasts, oldest first.
fn toasts(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.read_entity(view, |view, cx| {
        view.toasts
            .read(cx)
            .visible()
            .into_iter()
            .map(|(_, message)| message.to_string())
            .collect()
    })
}

/// Presses the Undo button of the newest toast that has one.
fn click_toast_undo(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    let host = cx.read_entity(view, |view, _| view.toasts.clone());
    let undone = host.update_in(cx, |host, window, cx| host.undo_latest(window, cx));
    assert!(undone, "a toast offers Undo");
    cx.run_until_parked();
}

#[gpui::test]
fn archiving_selects_the_next_row_and_the_toast_undoes_it(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));

    keys("secondary-e", cx);
    assert_eq!(mailbox_of(&harness, threads[1]), harness.archive_id);
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[2]),
        "the row below takes the archived one's place"
    );
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    assert_eq!(toasts(&view, cx), ["Conversation archived"]);

    // The last row leaving selects the one above it.
    keys("secondary-e", cx);
    assert_eq!(mailbox_of(&harness, threads[2]), harness.archive_id);
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));
    assert_eq!(
        toasts(&view, cx),
        ["Conversation archived"],
        "only the latest change offers Undo"
    );

    // The toast's Undo brings it back and selects it.
    click_toast_undo(&view, cx);
    assert_eq!(mailbox_of(&harness, threads[2]), harness.inbox_id);
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));
    assert_eq!(toasts(&view, cx), ["Undone: Conversation archived"]);
    // Secondary+Z undoes the change before it.
    keys("secondary-z", cx);
    assert_eq!(mailbox_of(&harness, threads[1]), harness.inbox_id);
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    assert!(
        harness
            .store
            .list_pending_ops(harness.store.list_accounts().expect("accounts load")[0].id)
            .expect("the write-back queue loads")
            .is_empty(),
        "undone before any sync, so nothing goes to the server"
    );

    // Toasts close themselves.
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert!(toasts(&view, cx).is_empty());
}

#[gpui::test]
fn moving_a_conversation_keeps_the_list_and_selects_the_next_row(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    let archive_id = harness.archive_id.expect("an archive");
    cx.dispatch_action(crate::actions::MoveToMailbox {
        mailbox_id: archive_id,
    });
    cx.run_until_parked();
    assert_eq!(mailbox_of(&harness, threads[0]), Some(archive_id));
    let shown = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(shown, harness.inbox_id, "the list stays on the Inbox");
    assert_eq!(selected_thread(&view, cx), Some(threads[1]));
    assert_eq!(toasts(&view, cx), ["Conversation moved to Archive"]);
}

#[gpui::test]
fn undo_in_a_text_field_edits_the_text_not_the_mail(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    keys("secondary-e", cx);
    assert_eq!(mailbox_of(&harness, first), harness.archive_id);

    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("abc");
    keys("secondary-z", cx);
    assert_eq!(text(&search, cx), "", "the field undid its typing");
    assert_eq!(
        mailbox_of(&harness, first),
        harness.archive_id,
        "the archive stays"
    );
}

/// The list's multi-row selection, as the shell keeps it.
fn selection(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<i64> {
    cx.read_entity(view, |view, _| view.list_selection.thread_ids.clone())
}

/// Clicks list row `index` with `modifiers` held.
fn click_row(index: usize, modifiers: Modifiers, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(Box::leak(format!("thread-row-{index}").into_boxed_str()))
        .expect("the row is drawn");
    cx.simulate_click(bounds.center(), modifiers);
    cx.run_until_parked();
}

/// What the reader shows: the selected count, and whether a conversation
/// is open.
fn reader_shows(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> (usize, bool) {
    cx.read_entity(view, |view, cx| {
        let state = view.reader.read(cx).state();
        (state.selected_count, state.thread.is_some())
    })
}

#[gpui::test]
fn modifier_clicks_build_a_selection_and_the_reader_counts_it(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));

    // Cmd/Ctrl-click adds a row to the one already selected.
    click_row(2, Modifiers::secondary_key(), cx);
    assert_eq!(selection(&view, cx), [threads[0], threads[2]]);
    assert_eq!(reader_shows(&view, cx), (2, false));
    assert!(cx.debug_bounds("bulk-bar").is_some(), "the bulk bar shows");
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Cmd/Ctrl-click again removes it; one row left is a plain selection.
    click_row(2, Modifiers::secondary_key(), cx);
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
    assert_eq!(selected_thread(&view, cx), Some(threads[0]));
    assert_eq!(reader_shows(&view, cx), (0, true));
    assert!(cx.debug_bounds("bulk-bar").is_none());

    // Shift-click selects from the anchor (the row clicked last) to here.
    click_row(1, Modifiers::none(), cx);
    click_row(2, Modifiers::shift(), cx);
    assert_eq!(selection(&view, cx), [threads[1], threads[2]]);
    click_row(0, Modifiers::shift(), cx);
    assert_eq!(selection(&view, cx), [threads[0], threads[1]]);

    // A plain click goes back to one row.
    click_row(2, Modifiers::none(), cx);
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));
}

#[gpui::test]
fn shift_arrows_select_and_the_bulk_archive_is_one_undo(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();

    keys("shift-down shift-down", cx);
    assert_eq!(selection(&view, cx), threads);
    keys("shift-up", cx);
    assert_eq!(selection(&view, cx), [threads[0], threads[1]]);

    keys("secondary-e", cx);
    for thread_id in &threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), harness.archive_id);
    }
    assert_eq!(mailbox_of(&harness, threads[2]), harness.inbox_id);
    assert_eq!(toasts(&view, cx), ["2 conversations archived"]);
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[2]),
        "the row after the archived ones is selected"
    );

    keys("secondary-z", cx);
    for thread_id in &threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), harness.inbox_id);
    }
}

#[gpui::test]
fn select_all_then_flag_read_and_escape(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    let thread = |id: i64| {
        harness
            .store
            .thread_view(id)
            .expect("the thread loads")
            .expect("the thread exists")
    };

    keys("secondary-a", cx);
    assert_eq!(selection(&view, cx), threads);
    assert_eq!(reader_shows(&view, cx), (3, false));

    // Flag and Mark Read act on every selected conversation, which stays
    // selected.
    keys("secondary-shift-l secondary-shift-u", cx);
    assert!(threads.iter().all(|id| thread(*id).is_flagged));
    assert!(threads.iter().all(|id| thread(*id).unread_count == 0));
    assert_eq!(selection(&view, cx), threads);
    assert_eq!(toasts(&view, cx), ["3 conversations marked as read"]);
    // All flagged now, so Flag removes the flags.
    keys("secondary-shift-l", cx);
    assert!(threads.iter().all(|id| !thread(*id).is_flagged));

    keys("escape", cx);
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
    assert_eq!(reader_shows(&view, cx), (0, true));
}

#[gpui::test]
fn the_window_can_open_on_a_selection(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_on(cx, Setup::Mail, |_| InitialView::MultiSelect);
    assert_eq!(selection(&view, cx), harness.threads);
    assert_eq!(reader_shows(&view, cx), (3, false));
    assert!(cx.debug_bounds("bulk-bar").is_some());
}

#[gpui::test]
fn the_bulk_bar_moves_the_selection_with_the_folder_picker(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    keys("shift-down", cx);

    let bounds = cx.debug_bounds("bulk-move").expect("Move to… shows");
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    cx.simulate_input("tra");
    keys("enter", cx);

    for thread_id in &threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), harness.trash_id);
    }
    assert_eq!(toasts(&view, cx), ["2 conversations moved to Trash"]);
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

/// Right-clicks list row `index` and returns the menu's entry labels.
fn right_click_row(
    view: &Entity<WorkspaceView>,
    index: usize,
    cx: &mut VisualTestContext,
) -> Vec<String> {
    let bounds = cx
        .debug_bounds(Box::leak(format!("thread-row-{index}").into_boxed_str()))
        .expect("the row is drawn");
    cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(bounds.center(), gpui::MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let menu = context_menu(view, cx).expect("the context menu opens");
    cx.read_entity(&menu, |menu, _| {
        menu.entries()
            .map(|entry| entry.label().to_owned())
            .collect()
    })
}

fn context_menu(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Option<Entity<components::Menu>> {
    cx.read_entity(view, |view, cx| view.message_list.read(cx).context_menu())
}

/// Chooses menu item `index` (counting separators) of the open context
/// menu.
fn choose(view: &Entity<WorkspaceView>, index: usize, cx: &mut VisualTestContext) {
    let menu = context_menu(view, cx).expect("the context menu is open");
    menu.update_in(cx, |menu, window, cx| menu.activate(index, window, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn the_context_menu_acts_on_the_clicked_row(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();

    let labels = right_click_row(&view, 2, cx);
    assert_eq!(
        labels,
        ["Archive", "Delete", "Move to…", "Flag", "Mark as Read"]
    );
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[2]),
        "a right-click selects the row it acts on"
    );

    choose(&view, 0, cx);
    assert_eq!(mailbox_of(&harness, threads[2]), harness.archive_id);
    assert_eq!(toasts(&view, cx), ["Conversation archived"]);
    assert!(context_menu(&view, cx).is_none());
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

#[gpui::test]
fn the_context_menu_acts_on_the_selection_it_was_opened_on(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    keys("shift-down", cx);

    let labels = right_click_row(&view, 1, cx);
    assert_eq!(labels[0], "Archive 2 Conversations");
    assert_eq!(selection(&view, cx), [threads[0], threads[1]]);

    // Move to… opens the folder picker where the menu was.
    choose(&view, 2, cx);
    assert!(context_menu(&view, cx).is_none());
    cx.simulate_input("arch");
    keys("enter", cx);
    for thread_id in &threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), harness.archive_id);
    }
    assert_eq!(toasts(&view, cx), ["2 conversations moved to Archive"]);

    // A right-click outside the selection acts on that row alone.
    keys("secondary-z", cx);
    keys("secondary-a", cx);
    keys("escape", cx);
    keys("shift-down", cx);
    let labels = right_click_row(&view, 2, cx);
    assert_eq!(labels[0], "Archive");
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
}

/// Drags list row `index` onto the sidebar row `target` and drops it.
fn drag_row_to(index: usize, target: &str, cx: &mut VisualTestContext) {
    let from = cx
        .debug_bounds(Box::leak(format!("thread-row-{index}").into_boxed_str()))
        .expect("the row is drawn")
        .center();
    let to = cx
        .debug_bounds(Box::leak(target.to_owned().into_boxed_str()))
        .expect("the folder row is drawn")
        .center();
    cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        from + point(px(12.), px(12.)),
        gpui::MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(to, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn dragging_a_row_onto_a_folder_moves_it_and_can_be_undone(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    let archive_id = harness.archive_id.expect("an archive");

    drag_row_to(0, &format!("mailbox-row-{archive_id}"), cx);
    assert_eq!(mailbox_of(&harness, threads[0]), Some(archive_id));
    assert_eq!(toasts(&view, cx), ["Conversation moved to Archive"]);
    assert_eq!(
        selected_thread(&view, cx),
        Some(threads[1]),
        "the row below takes the dragged one's place"
    );
    assert!(!cx.update(|_, cx| cx.has_active_drag()));

    keys("secondary-z", cx);
    assert_eq!(mailbox_of(&harness, threads[0]), harness.inbox_id);

    // The folder the conversation is in does not take it.
    let inbox_id = harness.inbox_id.expect("an inbox");
    drag_row_to(1, &format!("mailbox-row-{inbox_id}"), cx);
    assert_eq!(mailbox_of(&harness, threads[1]), Some(inbox_id));
    assert_eq!(toasts(&view, cx), ["Undone: Conversation moved to Archive"]);
}

#[gpui::test]
fn another_accounts_folder_does_not_take_a_dragged_row(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let archive_id = harness.archive_id.expect("Work has an archive");
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();

    // Row 0 of the Unified Inbox is Home's newest conversation.
    drag_row_to(0, &format!("mailbox-row-{archive_id}"), cx);
    assert_eq!(
        mailbox_of(&harness, harness.home_threads[0]),
        harness.home_inbox_id
    );
    // Row 1 is Work's newest, which Work's Archive takes.
    drag_row_to(1, &format!("mailbox-row-{archive_id}"), cx);
    assert_eq!(mailbox_of(&harness, harness.threads[0]), Some(archive_id));
}

#[gpui::test]
fn dragging_a_selected_row_moves_the_whole_selection(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let threads = harness.threads.clone();
    let trash_id = harness.trash_id.expect("a trash");
    keys("shift-down", cx);

    drag_row_to(1, &format!("mailbox-row-{trash_id}"), cx);
    for thread_id in &threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), Some(trash_id));
    }
    assert_eq!(mailbox_of(&harness, threads[2]), harness.inbox_id);
    assert_eq!(toasts(&view, cx), ["2 conversations moved to Trash"]);
    assert_eq!(selection(&view, cx), Vec::<i64>::new());
    assert_eq!(selected_thread(&view, cx), Some(threads[2]));
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

    // Reply opens a draft below the conversation, with the cursor in the
    // body.
    view.update(cx, |view, cx| {
        view.selected_thread_id = Some(first);
        cx.notify();
    });
    keys("secondary-r", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::Reader
    );
    let reply = inline_reply(&view, cx).expect("an inline reply");
    let (draft_id, body) = cx.read_entity(&reply, |reply, _| {
        let compose = reply.compose.as_ref().expect("a draft is open");
        (compose.draft_id, compose.body.clone())
    });
    assert!(cx.update(|window, cx| body.focus_handle(cx).is_focused(window)));
    let draft = harness
        .store
        .draft_detail(draft_id)
        .expect("the draft loads")
        .expect("the draft exists");
    assert_eq!(draft.subject, "Re: Quarterly report 0");
    assert_eq!(draft.to, vec!["Kai Santos <kai@example.test>".to_owned()]);

    // Send queues it, with Undo while it waits.
    keys("secondary-enter", cx);
    assert_eq!(
        toasts(&view, cx).last().map(String::as_str),
        Some("Sending message…")
    );
    assert_ne!(
        harness
            .store
            .draft_detail(draft_id)
            .expect("the draft loads")
            .expect("the draft exists")
            .state,
        zmail_core::DraftState::Editing
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

// ---------------------------------------------------------------------------
// Toolbar, sidebar views and reader actions (UI plan Phase 2)
// ---------------------------------------------------------------------------

/// Clicks the centre of the element with debug selector `selector`.
/// [`click`] for a selector built at run time.
fn click_on(selector: String, cx: &mut VisualTestContext) {
    click(Box::leak(selector.into_boxed_str()), cx);
}

/// Whether an element with the run-time `selector` is drawn.
fn drawn(selector: String, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(Box::leak(selector.into_boxed_str()))
        .is_some()
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Resizes the test window to `width` x 900.
fn resize(width: f32, cx: &mut VisualTestContext) {
    cx.simulate_resize(size(px(width), px(900.)));
    cx.run_until_parked();
}

#[gpui::test]
fn toolbar_buttons_dispatch_their_actions(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);

    click("toolbar-compose", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.compose_windows.len()),
        1
    );

    // The sidebar toggle hides the sidebar and remembers it.
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
    click("toolbar-sidebar", cx);
    assert!(cx.read_entity(&view, |view, _| !view.sidebar_shown()));
    assert!(cx.debug_bounds("sidebar-drafts").is_none());
    let saved = |harness: &Harness| {
        harness
            .store
            .ui_preferences()
            .expect("the preferences load")
            .sidebar_collapsed
    };
    assert!(saved(&harness));
    click("toolbar-sidebar", cx);
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
    assert!(!saved(&harness));

    // The sync indicator syncs every account.
    click("toolbar-sync", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.status_message.clone()),
        Some("Syncing 1 account…".to_owned())
    );
}

#[gpui::test]
fn a_hidden_sidebar_stays_hidden_after_a_restart(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| preferences.sidebar_collapsed = true)
                .expect("the preferences save");
        },
    );
    assert!(cx.read_entity(&view, |view, _| !view.sidebar_shown()));
    assert!(cx.debug_bounds("sidebar-drafts").is_none());
}

#[gpui::test]
fn the_sidebar_lists_drafts_and_outbox(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let mode = |cx: &mut VisualTestContext| cx.read_entity(&view, |view, _| view.list_mode);

    click("sidebar-drafts", cx);
    assert_eq!(mode(cx), ListMode::Drafts);
    click("sidebar-outbox", cx);
    assert_eq!(mode(cx), ListMode::Outbox);

    // They are Favourites, between Flagged and Sent and after Sent.
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Sidebar, window, cx)
    });
    keys("home", cx);
    assert_eq!(mode(cx), ListMode::Mail);
    keys("down down", cx);
    assert_eq!(mode(cx), ListMode::Drafts);
    keys("down down", cx);
    assert_eq!(mode(cx), ListMode::Outbox);
    keys("up", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| (view.list_mode, view.mailbox_selection())),
        (
            ListMode::Mail,
            Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
                MailboxRole::Sent
            )))
        )
    );
}

#[gpui::test]
fn the_reader_more_menu_moves_the_thread(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    resize(1440., cx);
    let reader = cx.read_entity(&view, |view, _| view.reader.clone());

    click("reader-more", cx);
    assert!(cx.read_entity(&reader, |reader, _| reader.more_menu().is_some()));
    // Wide enough for every button, so the menu holds only Move to…,
    // which opens the folder picker in its place.
    keys("down enter", cx);
    assert!(cx.read_entity(&reader, |reader, _| reader.more_menu().is_none()));
    assert_eq!(
        cx.read_entity(&reader, |reader, _| reader.popover_anchor()),
        Some(crate::reader::PopoverAnchor::ActionBar),
        "the picker opened below More"
    );
    cx.simulate_input("arch");
    keys("enter", cx);
    assert_eq!(mailbox_of(&harness, first), harness.archive_id);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    assert_eq!(
        cx.read_entity(&reader, |reader, _| reader.popover_anchor()),
        None
    );

    // Escape closes the menu without acting.
    click("reader-more", cx);
    keys("escape", cx);
    assert!(cx.read_entity(&reader, |reader, _| reader.more_menu().is_none()));
}

#[gpui::test]
fn a_narrow_reader_moves_actions_into_more(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let compact = |cx: &mut VisualTestContext| {
        let reader = cx.read_entity(&view, |view, _| view.reader.clone());
        cx.read_entity(&reader, |reader, _| reader.compact_actions())
    };
    resize(1440., cx);
    assert!(!compact(cx));
    // The overlay layout gives the reader the width beside the list.
    resize(1000., cx);
    assert!(!compact(cx));
    resize(800., cx);
    assert!(compact(cx));

    // Reply All is now the first entry of More; the reply opens below
    // the conversation.
    click("reader-more", cx);
    keys("down enter", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| (view.list_mode, view.detail_mode)),
        (ListMode::Mail, DetailMode::Reader)
    );
    assert!(inline_reply(&view, cx).is_some());
}

// ---------------------------------------------------------------------------
// Responsive layout (UI plan Phase 2)
// ---------------------------------------------------------------------------

fn layout_mode(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> LayoutMode {
    cx.read_entity(view, |view, _| view.pane_layout().mode)
}

fn overlay_open(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    cx.read_entity(view, |view, _| view.sidebar_overlay_open)
}

#[gpui::test]
fn the_layout_follows_the_window_width(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);

    // Wide: sidebar, list and reader side by side.
    resize(1440., cx);
    assert_eq!(layout_mode(&view, cx), LayoutMode::Wide);
    for selector in ["sidebar-drafts", "thread-row-0", "reader-more"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} at 1440");
    }

    // Overlay: the sidebar pane goes; list and reader stay.
    resize(900., cx);
    assert_eq!(layout_mode(&view, cx), LayoutMode::Overlay);
    assert!(cx.debug_bounds("sidebar-drafts").is_none());
    assert!(cx.debug_bounds("thread-row-0").is_some());
    assert!(cx.debug_bounds("reader-more").is_some());

    // Single: the list alone.
    resize(720., cx);
    assert_eq!(layout_mode(&view, cx), LayoutMode::Single);
    assert!(cx.debug_bounds("thread-row-0").is_some());
    assert!(cx.debug_bounds("reader-more").is_none());

    // Back to wide: everything returns.
    resize(1440., cx);
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
}

#[gpui::test]
fn the_sidebar_overlay_opens_and_closes(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    resize(900., cx);

    // The toggle opens it with focus in it; Escape closes it.
    keys("secondary-b", cx);
    assert!(overlay_open(&view, cx));
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    keys("escape", cx);
    assert!(!overlay_open(&view, cx));
    assert!(cx.debug_bounds("sidebar-drafts").is_none());
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // A click beside it closes it.
    click("toolbar-sidebar", cx);
    assert!(overlay_open(&view, cx));
    let scrim = cx
        .debug_bounds("sidebar-scrim")
        .expect("the scrim is drawn");
    cx.simulate_click(
        point(scrim.right() - px(20.), scrim.center().y),
        Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(!overlay_open(&view, cx));

    // Picking a view in it closes it and shows that view.
    click("toolbar-sidebar", cx);
    click("sidebar-drafts", cx);
    assert!(!overlay_open(&view, cx));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.list_mode),
        ListMode::Drafts
    );

    // Tab reaches the sidebar through the overlay, and leaving it closes
    // the overlay.
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Detail, window, cx)
    });
    keys("f6", cx);
    assert!(overlay_open(&view, cx));
    keys("f6", cx);
    assert!(!overlay_open(&view, cx));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Widening the window turns it back into a pane.
    keys("secondary-b", cx);
    resize(1440., cx);
    assert!(!overlay_open(&view, cx));
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
}

#[gpui::test]
fn the_single_pane_layout_goes_to_the_reader_and_back(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    resize(720., cx);
    let showing_reader = |cx: &mut VisualTestContext| cx.debug_bounds("reader-more").is_some();

    // Arrow keys move through the list without leaving it.
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::MessageList, window, cx)
    });
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[1]));
    assert!(!showing_reader(cx));

    // A click opens the reader; the back button returns.
    click("thread-row-2", cx);
    assert!(showing_reader(cx));
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[2]));
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    click("single-pane-back", cx);
    assert!(!showing_reader(cx));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Enter opens it; Escape and the back keys return.
    for back in ["escape", "alt-left", "secondary-["] {
        keys("enter", cx);
        assert!(showing_reader(cx), "enter before {back}");
        keys(back, cx);
        assert!(!showing_reader(cx), "{back}");
        assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    }

    // A reply shows the conversation with the reply below it, and back
    // returns to the list.
    keys("secondary-r", cx);
    assert!(cx.read_entity(&view, |view, _| {
        view.single_pane == SinglePane::Detail && view.detail_mode == DetailMode::Reader
    }));
    assert!(inline_reply(&view, cx).is_some());
    click("single-pane-back", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.single_pane),
        SinglePane::List
    );
}

#[gpui::test]
fn narrowing_the_window_keeps_the_focused_pane_in_view(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    resize(1440., cx);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Detail, window, cx)
    });
    resize(720., cx);
    assert!(cx.debug_bounds("reader-more").is_some());

    resize(1440., cx);
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Sidebar, window, cx)
    });
    resize(900., cx);
    assert!(overlay_open(&view, cx));
}

#[gpui::test]
fn dragged_pane_widths_are_saved_and_restored(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    resize(1440., cx);
    let widths = |cx: &mut VisualTestContext| {
        cx.read_entity(&view, |view, _| {
            let layout = view.pane_layout();
            (layout.sidebar, layout.list)
        })
    };
    assert_eq!(widths(cx), (Some(SIDEBAR_WIDTH), MESSAGE_LIST_WIDTH));

    // Drag the sidebar's edge 40px right.
    let edge = point(px(SIDEBAR_WIDTH), px(400.));
    let button = Some(gpui::MouseButton::Left);
    cx.simulate_event(gpui::MouseDownEvent {
        position: edge,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
        first_mouse: false,
    });
    for x in [SIDEBAR_WIDTH + 10., SIDEBAR_WIDTH + 40.] {
        cx.simulate_event(gpui::MouseMoveEvent {
            position: point(px(x), px(400.)),
            pressed_button: button,
            modifiers: Modifiers::none(),
        });
    }
    cx.simulate_event(gpui::MouseUpEvent {
        position: point(px(SIDEBAR_WIDTH + 40.), px(400.)),
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 1,
    });
    cx.run_until_parked();
    assert_eq!(widths(cx), (Some(SIDEBAR_WIDTH + 40.), MESSAGE_LIST_WIDTH));

    // Saved once the drag settles.
    let saved = |harness: &Harness| {
        harness
            .store
            .ui_preferences()
            .expect("the preferences load")
            .pane_widths
    };
    assert_eq!(saved(&harness).sidebar, None);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(saved(&harness).sidebar, Some(SIDEBAR_WIDTH + 40.));
    assert_eq!(saved(&harness).message_list, Some(MESSAGE_LIST_WIDTH));

    // A narrower window clamps the panes without losing the saved width.
    resize(1100., cx);
    let (sidebar, list) = widths(cx);
    assert_eq!(sidebar, Some(SIDEBAR_WIDTH + 40.));
    assert!(1100. - SIDEBAR_WIDTH - 40. - list >= DETAIL_MIN_WIDTH);
}

#[gpui::test]
fn saved_pane_widths_apply_at_startup(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| {
                    preferences.pane_widths = PaneWidths {
                        sidebar: Some(240.),
                        message_list: Some(460.),
                    };
                })
                .expect("the preferences save");
        },
    );
    resize(1440., cx);
    let layout = cx.read_entity(&view, |view, _| view.pane_layout());
    assert_eq!((layout.sidebar, layout.list), (Some(240.), 460.));
}

#[gpui::test]
fn the_window_can_open_on_the_sidebar(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_on(cx, Setup::Mail, |_| InitialView::Sidebar);
    assert_eq!(focused_pane(&view, cx), Some(Pane::Sidebar));
    resize(900., cx);
    assert!(overlay_open(&view, cx));
    assert!(cx.debug_bounds("sidebar-drafts").is_some());
}

#[gpui::test]
fn a_window_opened_on_a_thread_keeps_the_reader_when_narrowed(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_on(cx, Setup::Mail, |harness| {
        InitialView::Thread(harness.threads[1])
    });
    assert_eq!(focused_pane(&view, cx), Some(Pane::Detail));
    resize(720., cx);
    assert!(cx.debug_bounds("reader-more").is_some());
    assert!(cx.debug_bounds("single-pane-back").is_some());
}

// ---------------------------------------------------------------------------
// Virtual mailboxes (UI plan Phase 3)
// ---------------------------------------------------------------------------

fn selected_account(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<i64> {
    cx.read_entity(view, |view, _| view.selected_account_id)
}

#[gpui::test]
fn the_unified_inbox_lists_every_account_and_follows_the_open_thread(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let work = cx.read_entity(&view, |view, _| view.accounts[0].id);
    let home = harness.home_account_id;
    assert_eq!(selected_account(&view, cx), Some(work));

    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    // The newest thread is Home's, so the shell now acts on Home.
    assert_eq!(selected_thread(&view, cx), Some(harness.home_threads[0]));
    assert_eq!(selected_account(&view, cx), home);
    let state = cx.read_entity(&view, |view, cx| view.message_list.read(cx).state().clone());
    let mailbox = state.mailbox.clone().expect("the list shows a mailbox");
    assert_eq!(mailbox.name, "Unified Inbox");
    assert_eq!(mailbox.total_threads, 5);
    assert_eq!(state.row_account_name(work), Some("Work"));

    // Down moves to Work's newest thread: the account follows it.
    keys("down", cx);
    assert_eq!(selected_thread(&view, cx), Some(harness.threads[0]));
    assert_eq!(selected_account(&view, cx), Some(work));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.mailbox_selection()),
        Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
            MailboxRole::Inbox
        ))),
        "the list stays on the Unified Inbox"
    );
    keys("end", cx);
    assert_eq!(
        selected_thread(&view, cx),
        harness.home_threads.last().copied()
    );

    // The choice is saved for the next launch.
    assert_eq!(
        harness
            .store
            .ui_preferences()
            .expect("the preferences load")
            .last_mailbox,
        Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
            MailboxRole::Inbox
        )))
    );
}

#[gpui::test]
fn a_selection_across_accounts_acts_on_each_and_offers_no_folders(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    let (home_newest, work_newest) = (harness.home_threads[0], harness.threads[0]);

    // Home's newest and Work's newest: two accounts, so no Move to….
    keys("shift-down", cx);
    assert_eq!(selection(&view, cx), [home_newest, work_newest]);
    assert!(cx.debug_bounds("bulk-bar").is_some());
    assert!(cx.debug_bounds("bulk-move").is_none());
    // Flag acts on both accounts' threads.
    keys("secondary-shift-l", cx);
    for thread_id in [home_newest, work_newest] {
        assert!(
            harness
                .store
                .thread_view(thread_id)
                .expect("the thread loads")
                .expect("the thread exists")
                .is_flagged
        );
    }

    // Work's two newest: one account, so its folders are offered.
    keys("escape shift-down", cx);
    assert_eq!(
        selection(&view, cx),
        [harness.threads[0], harness.threads[1]]
    );
    let bounds = cx.debug_bounds("bulk-move").expect("Move to… shows");
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    cx.simulate_input("arch");
    keys("enter", cx);
    for thread_id in &harness.threads[..2] {
        assert_eq!(mailbox_of(&harness, *thread_id), harness.archive_id);
    }
}

#[gpui::test]
fn flagged_lists_flagged_threads_across_accounts(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Flagged, cx)
    });
    cx.run_until_parked();
    let state = cx.read_entity(&view, |view, cx| view.message_list.read(cx).state().clone());
    let mailbox = state.mailbox.expect("the list shows a mailbox");
    assert_eq!(
        (mailbox.name.as_str(), mailbox.total_threads),
        ("Flagged", 1)
    );
    assert_eq!(selected_thread(&view, cx), Some(harness.home_threads[0]));
}

#[gpui::test]
fn picking_another_accounts_mailbox_switches_the_account(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let home_inbox = harness.home_inbox_id.expect("Home has an inbox");
    view.update(cx, |view, cx| view.select_mailbox(home_inbox, cx));
    cx.run_until_parked();
    assert_eq!(selected_account(&view, cx), harness.home_account_id);
    assert_eq!(selected_thread(&view, cx), Some(harness.home_threads[0]));
    let identities = cx.read_entity(&view, |view, _| view.own_addresses());
    assert_eq!(identities, ["me@example.org"], "replies come from Home");
    let state = cx.read_entity(&view, |view, cx| view.message_list.read(cx).state().clone());
    assert_eq!(
        state.row_account_name(harness.home_account_id.expect("Home")),
        None,
        "one account's mailbox does not name the account on each row"
    );
}

#[gpui::test]
fn the_last_mailbox_reopens_after_a_restart(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| {
                    preferences.last_mailbox =
                        Some(MailboxSelection::Virtual(VirtualMailbox::Flagged));
                })
                .expect("the preferences save");
        },
    );
    assert_eq!(
        cx.read_entity(&view, |view, _| view.virtual_mailbox),
        Some(VirtualMailbox::Flagged)
    );
    assert_eq!(selected_thread(&view, cx), Some(harness.home_threads[0]));
}

#[gpui::test]
fn archiving_in_the_unified_inbox_selects_the_next_row_and_undo_brings_it_back(
    cx: &mut TestAppContext,
) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let work = cx.read_entity(&view, |view, _| view.accounts[0].id);
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Unified(MailboxRole::Inbox), cx)
    });
    cx.run_until_parked();
    // Rows: Home's newest, then Work's three, then Home's oldest.
    keys("down", cx);
    let archived = harness.threads[0];
    assert_eq!(selected_thread(&view, cx), Some(archived));

    keys("secondary-e", cx);
    assert_eq!(mailbox_of(&harness, archived), harness.archive_id);
    assert_eq!(
        selected_thread(&view, cx),
        Some(harness.threads[1]),
        "the row that took its place is selected"
    );
    assert_eq!(selected_account(&view, cx), Some(work));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.virtual_mailbox),
        Some(VirtualMailbox::Unified(MailboxRole::Inbox)),
        "the list stays on the Unified Inbox"
    );

    keys("secondary-z", cx);
    assert_eq!(mailbox_of(&harness, archived), harness.inbox_id);
    assert_eq!(
        selected_thread(&view, cx),
        Some(archived),
        "undo selects the conversation it brought back"
    );
    assert_eq!(
        cx.read_entity(&view, |view, _| view.selected_mailbox_id),
        harness.inbox_id
    );
}

#[gpui::test]
fn unflagging_in_flagged_moves_on_across_accounts_and_undo_reselects(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let work = cx.read_entity(&view, |view, _| view.accounts[0].id);
    let work_thread = harness.threads[0];
    harness
        .store
        .set_thread_flagged(work_thread, true)
        .expect("the thread is flagged");
    view.update(cx, |view, cx| {
        view.select_virtual_mailbox(VirtualMailbox::Flagged, cx)
    });
    cx.run_until_parked();
    let home_thread = harness.home_threads[0];
    assert_eq!(selected_thread(&view, cx), Some(home_thread));

    // Unflagging takes Home's thread out of Flagged: Work's is next.
    keys("secondary-shift-l", cx);
    assert_eq!(selected_thread(&view, cx), Some(work_thread));
    assert_eq!(selected_account(&view, cx), Some(work));

    keys("secondary-z", cx);
    assert_eq!(selected_thread(&view, cx), Some(home_thread));
    assert_eq!(selected_account(&view, cx), harness.home_account_id);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.selected_mailbox_id),
        harness.home_inbox_id
    );
}

// ---------------------------------------------------------------------------
// Sidebar (UI plan Phase 3)
// ---------------------------------------------------------------------------

/// The id of `account`'s mailbox called `name`.
fn mailbox_named(harness: &Harness, account_id: i64, name: &str) -> i64 {
    harness
        .store
        .list_mailboxes(account_id)
        .expect("the mailboxes list")
        .into_iter()
        .find(|mailbox| mailbox.name == name)
        .unwrap_or_else(|| panic!("{name} exists"))
        .id
}

#[gpui::test]
fn the_sidebar_shows_inboxes_favourites_and_each_accounts_folders(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    resize(1440., cx);
    let work = cx.read_entity(&view, |view, _| view.accounts[0].id);
    let home = harness.home_account_id.expect("Home");
    let home_inbox = harness.home_inbox_id.expect("Home's inbox");
    let work_inbox = harness.inbox_id.expect("Work's inbox");

    for selector in [
        "sidebar-unified-inbox",
        "sidebar-flagged",
        "sidebar-drafts",
        "sidebar-sent",
        "sidebar-outbox",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    for selector in [
        format!("inbox-row-{work_inbox}"),
        format!("inbox-row-{home_inbox}"),
        format!("account-row-{work}"),
        format!("account-row-{home}"),
        format!(
            "mailbox-row-{}",
            mailbox_named(&harness, home, "Projects/Website")
        ),
        format!("folder-group-{home}-Lists"),
    ] {
        assert!(drawn(selector.clone(), cx), "{selector} is drawn");
    }

    // The Unified Inbox, then one account's inbox.
    click("sidebar-unified-inbox", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.mailbox_selection()),
        Some(MailboxSelection::Virtual(VirtualMailbox::Unified(
            MailboxRole::Inbox
        )))
    );
    click_on(format!("inbox-row-{home_inbox}"), cx);
    assert_eq!(selected_account(&view, cx), Some(home));
    assert_eq!(
        cx.read_entity(&view, |view, _| view.selected_mailbox_id),
        Some(home_inbox)
    );
    click("sidebar-sent", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.virtual_mailbox),
        Some(VirtualMailbox::Unified(MailboxRole::Sent))
    );
}

#[gpui::test]
fn sidebar_folders_collapse_and_stay_collapsed(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    resize(1440., cx);
    let home = harness.home_account_id.expect("Home");
    let website = format!(
        "mailbox-row-{}",
        mailbox_named(&harness, home, "Projects/Website")
    );
    let projects = mailbox_named(&harness, home, "Projects");
    assert!(drawn(website.clone(), cx));

    // A folder's chevron hides its subfolders.
    click_on(format!("mailbox-row-{projects}-chevron"), cx);
    assert!(!drawn(website.clone(), cx), "Website is hidden");
    assert!(drawn(format!("mailbox-row-{projects}"), cx));
    assert_ne!(
        cx.read_entity(&view, |view, _| view.selected_mailbox_id),
        Some(projects),
        "the chevron does not open the folder"
    );

    // Clicking an account header collapses the whole account.
    click_on(format!("account-row-{home}"), cx);
    assert!(!drawn(format!("mailbox-row-{projects}"), cx));

    let saved = harness
        .store
        .ui_preferences()
        .expect("the preferences load")
        .sidebar_tree;
    assert_eq!(saved.collapsed_accounts, [home]);
    assert_eq!(
        saved.collapsed_folders,
        [FolderRef {
            account_id: home,
            name: "Projects".to_owned(),
        }]
    );

    // From the keyboard, Enter on the header expands it again; the
    // folder stays collapsed.
    view.update_in(cx, |view, window, cx| {
        view.focus_pane(Pane::Sidebar, window, cx)
    });
    keys("end enter", cx);
    assert!(drawn(format!("mailbox-row-{projects}"), cx));
    assert!(!drawn(website, cx));
}

#[gpui::test]
fn collapsed_folders_load_collapsed(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Inbox,
        |store| {
            let home = store.list_accounts().expect("accounts")[1].id;
            store
                .update_ui_preferences(|preferences| {
                    preferences.sidebar_tree.collapsed_accounts = vec![home];
                })
                .expect("the preferences save");
        },
    );
    resize(1440., cx);
    let _ = view;
    let home = harness.home_account_id.expect("Home");
    assert!(drawn(format!("account-row-{home}"), cx));
    assert!(!drawn(
        format!("mailbox-row-{}", mailbox_named(&harness, home, "Projects")),
        cx
    ));
}

#[gpui::test]
fn a_failing_account_offers_fix(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Inbox,
        |store| {
            let home = store.list_accounts().expect("accounts")[1].id;
            store
                .record_sync_failure(home, "Sync failed", "authentication failed")
                .expect("the failure is recorded");
        },
    );
    resize(1440., cx);
    let work = cx.read_entity(&view, |view, _| view.accounts[0].id);
    let home = harness.home_account_id.expect("Home");
    assert!(!drawn(format!("account-fix-{work}"), cx), "Work is fine");

    click_on(format!("account-fix-{home}"), cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.detail_mode),
        DetailMode::AccountSetup
    );
    let notice = cx.read_entity(&view, |view, cx| {
        view.account_setup.read(cx).form.notice.clone()
    });
    assert_eq!(
        notice.as_deref(),
        Some("Home can't sync: authentication failed")
    );
    assert!(
        drawn(
            format!("mailbox-row-{}", mailbox_named(&harness, home, "Projects")),
            cx
        ),
        "Fix… does not collapse the account"
    );
}

/// Queues a one-line draft from the first account and returns the job.
fn queue_test_message(store: &LocalStore, subject: &str) -> i64 {
    let account_id = store.list_accounts().expect("accounts")[0].id;
    let identity = store.list_identities(account_id).expect("identities")[0].id;
    let draft_id = store
        .create_draft(&NewDraft {
            account_id,
            identity_id: identity,
            to: vec!["kai@example.test".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body: "Hello".to_owned(),
        })
        .expect("the draft is stored");
    store.queue_draft(draft_id).expect("queued")
}

/// Delivers outbox job `job_id` the way the sync service does.
fn deliver(store: &LocalStore, job_id: i64) {
    let claimed = store
        .claim_due_outbox_job(None)
        .expect("the claim runs")
        .expect("a job is due");
    assert_eq!(claimed.job_id, job_id);
    store
        .record_outbox_sent(job_id)
        .expect("the send is recorded");
}

#[gpui::test]
fn mail_queued_in_the_last_session_says_when_it_has_gone(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            queue_test_message(store, "Left in the undo window");
        },
    );
    let carried = cx.read_entity(&view, |view, _| view.last_session_jobs.clone());
    assert_eq!(carried.len(), 1, "the waiting job is from the last session");
    assert!(
        toasts(&view, cx).is_empty(),
        "nothing is said before it sends"
    );

    deliver(&harness.store, carried[0]);
    view.update(cx, |view, cx| view.refresh_from_store(cx));
    assert_eq!(
        toasts(&view, cx),
        vec!["1 message from your last session was sent".to_owned()]
    );

    // Mail sent in this session gets no such notice.
    let job = queue_test_message(&harness.store, "Sent today");
    deliver(&harness.store, job);
    view.update(cx, |view, cx| view.refresh_from_store(cx));
    assert_eq!(toasts(&view, cx).len(), 1, "no second notice");
}

#[gpui::test]
fn the_outbox_row_counts_waiting_mail_and_retries_failures(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    assert!(cx.debug_bounds("sidebar-outbox-retry").is_none());
    let account_id = cx.read_entity(&view, |view, _| view.accounts[0].id);
    let identity = harness
        .store
        .list_identities(account_id)
        .expect("identities")[0]
        .id;
    let draft_id = harness
        .store
        .create_draft(&NewDraft {
            account_id,
            identity_id: identity,
            to: vec!["kai@example.test".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Report".to_owned(),
            body: "Attached".to_owned(),
        })
        .expect("the draft is stored");
    let job = harness.store.queue_draft(draft_id).expect("queued");
    harness
        .store
        .record_outbox_failure(job, "connection refused")
        .expect("the failure is recorded");
    view.update(cx, |view, cx| {
        view.refresh_from_store(cx);
        cx.notify();
    });
    cx.run_until_parked();

    click("sidebar-outbox-retry", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.status_message.clone()),
        Some("1 message queued for another send attempt.".to_owned())
    );
}
