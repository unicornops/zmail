//! View tests for [`WorkspaceView`] in a GPUI test window, backed by an
//! ephemeral store and an in-memory secret store.
//!
//! [`open_workspace`] is the harness: it opens the real workspace view the
//! app uses, so tests drive it with keystrokes and actions and then check
//! the view and the store.

use std::{sync::Arc, time::Duration};

use gpui::{AppContext as _, Entity, Focusable, TestAppContext, VisualTestContext};
use zmail_core::{
    AppBootstrap, AuthKind, CachePolicySummary, NewAccount, NewIdentity, OutgoingConfig,
    ProtocolKind, RuntimeSummary, ThemeAppearance, ThemeSummary, UpstreamPins,
};
use zmail_security::{InMemorySecretStore, SecretStore};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::account_setup::AccountField;
use crate::components::{TextInput, text_input};
use crate::{DetailMode, InitialView, ListMode, WorkspaceView};

/// Keeps the store and the sync runtime alive for the test.
struct Harness {
    store: Arc<LocalStore>,
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

/// Opens the workspace in a test window, with one IMAP account when
/// `with_account` is set (otherwise the account setup form opens).
fn open_workspace(
    cx: &mut TestAppContext,
    with_account: bool,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Harness) {
    cx.update(text_input::init);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the test runtime starts");
    let store = Arc::new(LocalStore::open_ephemeral().expect("the store opens"));
    if with_account {
        store
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
    }
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let sync_service = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
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
    let view_store = store.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        WorkspaceView::new(
            bootstrap,
            view_store,
            sync_service,
            secrets,
            notifier,
            InitialView::Inbox,
            window,
            cx,
        )
    });
    (
        view,
        cx,
        Harness {
            store,
            _runtime: runtime,
        },
    )
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

#[gpui::test]
fn compose_autosave_keeps_the_cursor_and_the_attachment_path(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, true);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (draft_id, to, body, attachment_path) = cx.read_entity(&view, |view, _| {
        let compose = view.compose.as_ref().expect("a draft is open");
        (
            compose.draft_id,
            compose.to.clone(),
            compose.body.clone(),
            compose.attachment_path.clone(),
        )
    });
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

    let same_inputs = cx.read_entity(&view, |view, _| {
        view.compose.as_ref().is_some_and(|compose| {
            compose.body == body && compose.attachment_path == attachment_path
        })
    });
    assert!(
        same_inputs,
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
    cx.update(|_, cx| view.update(cx, |view, cx| view.save_current_draft(cx)));
    cx.run_until_parked();
    assert_eq!(cursor(&body, cx), "Hello big ".len());
    let same_body = cx.read_entity(&view, |view, _| {
        view.compose
            .as_ref()
            .is_some_and(|compose| compose.body == body)
    });
    assert!(same_body, "saving does not rebuild the compose inputs");
}

#[gpui::test]
fn compose_subject_updates_the_drafts_list_after_autosave(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, true);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let subject = cx.read_entity(&view, |view, _| {
        view.compose
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
    let (view, cx, _harness) = open_workspace(cx, true);
    let search = cx.read_entity(&view, |view, _| view.search_query.clone());
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
    let (view, cx, _harness) = open_workspace(cx, false);
    assert!(cx.read_entity(&view, |view, _| matches!(
        view.detail_mode,
        DetailMode::AccountSetup
    )));

    // With no account, the form opens with the first field focused.
    cx.run_until_parked();
    cx.simulate_input("Work");
    let display_name = cx.read_entity(&view, |view, _| {
        view.account_setup.input(AccountField::DisplayName).clone()
    });
    assert_eq!(text(&display_name, cx), "Work");

    // Submitting reports the missing fields and focuses the first one.
    cx.update(|window, cx| view.update(cx, |view, cx| view.submit_account_setup(window, cx)));
    cx.run_until_parked();
    let has_error = |view: &WorkspaceView| {
        view.account_setup
            .errors
            .contains_key(&AccountField::Endpoint)
    };
    assert!(cx.read_entity(&view, |view, _| has_error(view)));

    // Typing goes to the focused endpoint field and clears its error.
    cx.simulate_input("imaps://mail.example.test");
    let endpoint = cx.read_entity(&view, |view, _| {
        view.account_setup.input(AccountField::Endpoint).clone()
    });
    assert_eq!(text(&endpoint, cx), "imaps://mail.example.test");
    assert!(!cx.read_entity(&view, |view, _| has_error(view)));
    assert_eq!(text(&display_name, cx), "Work");
}
