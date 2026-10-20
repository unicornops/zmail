//! The Settings window (UI plan Phase 6): opening it, Appearance and
//! Sending applying live, theme import, undelivered changes and Copy
//! Diagnostics.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
use zmail_core::{
    ThemeAppearance,
    preferences::{Density, ReadingPanePosition, ThemeMode},
};
use zmail_store::LocalStore;

use super::{Harness, Setup, click, keys, open_workspace, open_workspace_with};
use crate::InitialView;
use crate::actions::ToggleDensity;
use crate::components::{ComponentTheme, Select};
use crate::layout::SinglePane;
use crate::settings::{self, SettingsSection, SettingsWindow};

/// The open Settings window's root, and a test context for that window.
fn settings_window(cx: &mut VisualTestContext) -> (Entity<SettingsWindow>, VisualTestContext) {
    let handle = cx
        .update(|_, cx| settings::window(cx))
        .expect("the Settings window is open");
    let root = handle
        .root(cx)
        .expect("the Settings window has its root view");
    let window_cx = VisualTestContext::from_window(handle.into(), cx);
    (root, window_cx)
}

fn pick(select: &Entity<Select>, index: usize, cx: &mut VisualTestContext) {
    select.update(cx, |select, cx| select.pick(index, cx));
    cx.run_until_parked();
}

fn preferences(harness: &Harness) -> zmail_core::preferences::UiPreferences {
    harness
        .store
        .ui_preferences()
        .expect("the preferences load")
}

/// Leaves one change the server refused for good: the first thread
/// marked read, dead-lettered with an error naming the account.
fn dead_letter_a_change(store: &LocalStore) -> i64 {
    let account_id = store.list_accounts().expect("accounts")[0].id;
    let inbox = store
        .mailbox_id_by_role(account_id, "inbox")
        .expect("the lookup works")
        .expect("an inbox");
    let thread = store
        .first_thread_id(inbox)
        .expect("the lookup works")
        .expect("a thread");
    store.set_thread_read(thread, true).expect("marked read");
    let op = store.list_pending_ops(account_id).expect("ops")[0].id;
    store
        .mark_pending_op_dead(
            op,
            "UID STORE rejected for me@example.test in Inbox on mail.example.test",
        )
        .expect("dead-lettered");
    op
}

#[gpui::test]
fn secondary_comma_opens_one_settings_window(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Mail);
    assert!(cx.update(|_, cx| settings::window(cx)).is_none());
    let windows = cx.update(|_, cx| cx.windows().len());

    keys("secondary-,", cx);
    let (root, mut window_cx) = settings_window(cx);
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows + 1);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Appearance
    );

    // Down in the section list moves to the next section.
    keys("down", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Sending
    );
    click("settings-nav-about", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::About
    );

    // Asking again brings the same window forward.
    cx.update(|_, cx| settings::open(None, cx));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows + 1);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::About,
        "it stays on its section"
    );
}

#[gpui::test]
fn appearance_changes_apply_to_the_main_window_live(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    cx.update(|_, cx| settings::open(Some(SettingsSection::Appearance), cx));
    cx.run_until_parked();
    let (root, mut window_cx) = settings_window(cx);
    let [theme, density, reading_pane] =
        root.read_with(&window_cx, |root, cx| root.appearance().read(cx).selects());

    // Light: the main window switches and every window draws light.
    pick(&theme, 1, &mut window_cx);
    assert_eq!(preferences(&harness).theme_mode, ThemeMode::Light);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.bootstrap.theme.appearance),
        ThemeAppearance::Light
    );
    assert!(cx.update(|_, cx| ComponentTheme::get(cx).is_light));

    pick(&density, 1, &mut window_cx);
    assert_eq!(preferences(&harness).density, Density::Compact);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.density),
        Density::Compact
    );

    // The View menu's toggle changes it back, and Settings follows.
    cx.dispatch_action(ToggleDensity);
    cx.run_until_parked();
    assert_eq!(
        density.read_with(&window_cx, |select, _| select.selected_index()),
        Some(0)
    );

    // Reading pane off: the list and the reader take turns beside the
    // sidebar, and opening a row shows it in place of the list.
    pick(&reading_pane, 1, &mut window_cx);
    assert_eq!(preferences(&harness).reading_pane, ReadingPanePosition::Off);
    let layout = cx.read_entity(&view, |view, _| view.pane_layout());
    assert!(layout.stacked && layout.sidebar.is_some());
    assert!(cx.debug_bounds("single-pane-back").is_none());
    click("thread-row-1", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.single_pane),
        SinglePane::Detail
    );
    assert!(cx.debug_bounds("single-pane-back").is_some());
    click("single-pane-back", cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.single_pane),
        SinglePane::List
    );

    // Back beside the list.
    pick(&reading_pane, 0, &mut window_cx);
    assert!(!cx.read_entity(&view, |view, _| view.pane_layout().stacked));
}

#[gpui::test]
fn the_window_opens_with_the_saved_reading_pane(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| {
                    preferences.reading_pane = ReadingPanePosition::Off;
                })
                .expect("saved");
        },
    );
    assert!(cx.read_entity(&view, |view, _| view.pane_layout().stacked));
}

#[gpui::test]
fn undo_send_delay_is_a_choice_of_steps(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Sending),
        |store| {
            // Saved by hand as 15, which is not one of the steps.
            store
                .update_ui_preferences(|preferences| preferences.undo_send_delay_secs = 15)
                .expect("saved");
        },
    );
    let (root, mut window_cx) = settings_window(cx);
    let select = root.read_with(&window_cx, |root, cx| {
        root.sending().read(cx).select().clone()
    });
    assert_eq!(
        select.read_with(&window_cx, |select, _| select
            .selected_label()
            .map(|label| label.to_string())),
        Some("15 seconds".to_owned()),
        "the saved value is offered as it is"
    );
    pick(&select, 0, &mut window_cx);
    assert_eq!(preferences(&harness).undo_send_delay_secs, 0);
    let select = root.read_with(&window_cx, |root, cx| {
        root.sending().read(cx).select().clone()
    });
    pick(&select, 4, &mut window_cx);
    assert_eq!(preferences(&harness).undo_send_delay_secs, 20);
}

#[gpui::test]
fn importing_a_zed_theme_uses_it_and_bad_files_say_why(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| preferences.theme_mode = ThemeMode::Dark)
                .expect("saved");
        },
    );
    cx.update(|_, cx| settings::open(None, cx));
    cx.run_until_parked();
    let (root, mut window_cx) = settings_window(cx);
    let appearance = root.read_with(&window_cx, |root, _| root.appearance().clone());
    let directory = std::env::temp_dir().join(format!("zmail-theme-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("the directory is created");

    // Not a theme: nothing changes and the banner says why.
    let bad = directory.join("notes.json");
    std::fs::write(&bad, r#"{"name": "Notes"}"#).expect("written");
    appearance.update(&mut window_cx, |appearance, cx| {
        appearance.import_theme_from(&bad, cx)
    });
    window_cx.run_until_parked();
    let error = appearance.read_with(&window_cx, |appearance, _| {
        appearance.import_error().map(str::to_owned)
    });
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("not a Zed theme")),
        "{error:?}"
    );
    assert_eq!(harness.store.custom_theme().expect("loads"), None);

    // A dark-only family replaces the dark theme the window shows.
    let good = directory.join("night.json");
    std::fs::write(
        &good,
        r##"{"name": "Night Owl", "author": "test", "themes": [
            {"name": "Night Owl", "appearance": "dark",
             "style": {"background": "#011627ff", "text": "#d6deebff"}}
        ]}"##,
    )
    .expect("written");
    appearance.update(&mut window_cx, |appearance, cx| {
        appearance.import_theme_from(&good, cx)
    });
    window_cx.run_until_parked();
    assert_eq!(
        appearance.read_with(&window_cx, |appearance, _| appearance
            .imported_theme()
            .map(str::to_owned)),
        Some("Night Owl".to_owned())
    );
    assert!(appearance.read_with(&window_cx, |appearance, _| {
        appearance.import_error().is_none()
    }));
    assert!(harness.store.custom_theme().expect("loads").is_some());
    assert_eq!(
        cx.read_entity(&view, |view, _| view.bootstrap.theme.family_name.clone()),
        "Night Owl"
    );

    // Light keeps the bundled light theme: the family has none.
    let [theme, _, _] = appearance.read_with(&window_cx, |appearance, _| appearance.selects());
    pick(&theme, 1, &mut window_cx);
    assert_eq!(
        cx.read_entity(&view, |view, _| view.bootstrap.theme.family_name.clone()),
        "Test"
    );

    appearance.update(&mut window_cx, |appearance, cx| {
        appearance.use_default_theme(cx)
    });
    window_cx.run_until_parked();
    assert_eq!(harness.store.custom_theme().expect("loads"), None);
    let _ = std::fs::remove_dir_all(&directory);
}

#[gpui::test]
fn undelivered_changes_can_be_retried_or_discarded(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::About),
        |store| {
            dead_letter_a_change(store);
            dead_letter_a_change(store);
        },
    );
    let account_id = harness.store.list_accounts().expect("accounts")[0].id;
    let (root, mut window_cx) = settings_window(cx);
    let about = root.read_with(&window_cx, |root, _| root.about().clone());
    let ops: Vec<i64> = about.read_with(&window_cx, |about, _| {
        about
            .diagnostics()
            .dead_ops()
            .map(|(_, op)| op.id)
            .collect()
    });
    assert_eq!(ops.len(), 2);

    click_selector(&format!("retry-op-{}", ops[0]), &mut window_cx);
    assert_eq!(harness.store.dead_op_count(account_id).expect("count"), 1);
    assert_eq!(
        harness.store.pending_op_count(account_id).expect("count"),
        1
    );

    click_selector(&format!("discard-op-{}", ops[1]), &mut window_cx);
    assert_eq!(harness.store.dead_op_count(account_id).expect("count"), 0);
    assert_eq!(
        harness.store.pending_op_count(account_id).expect("count"),
        1
    );
    assert_eq!(
        about.read_with(&window_cx, |about, _| about
            .diagnostics()
            .dead_ops()
            .count()),
        0
    );
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert!(
        toasts.iter().any(|toast| toast.contains("discarded")),
        "{toasts:?}"
    );
}

/// Clicks the Retry or Discard button of an undelivered change.
fn click_selector(selector: &str, cx: &mut VisualTestContext) {
    super::click_on(selector.to_owned(), cx);
}

#[gpui::test]
fn diagnostics_never_include_mail_or_secrets(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Settings(SettingsSection::About),
        |store| {
            dead_letter_a_change(store);
            let account_id = store.list_accounts().expect("accounts")[0].id;
            store
                .record_sync_failure(
                    account_id,
                    "Signing in to Work failed",
                    "LOGIN me@example.test rejected by imaps://mail.example.test:993 (Work)",
                )
                .expect("recorded");
        },
    );
    super::queue_test_message(&harness.store, "Secret plans for Friday");
    let (root, mut window_cx) = settings_window(cx);
    // Tall enough that the button at the end of the section is on screen.
    window_cx.simulate_resize(size(px(820.0), px(2000.0)));
    window_cx.run_until_parked();
    click("copy-diagnostics", &mut window_cx);
    let report = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("the diagnostics are on the clipboard");

    // What a bug report needs is there.
    assert!(report.contains("Zmail 0.0.0-test"), "{report}");
    assert!(report.contains("Account 1: IMAP"), "{report}");
    assert!(report.contains("Account 2: IMAP"), "{report}");
    assert!(
        report.contains("Last error: LOGIN [redacted] rejected by [redacted]"),
        "{report}"
    );
    assert!(report.contains("Undelivered mark_read"), "{report}");

    // Nothing that says whose mail it is, or what it says.
    for private in [
        // Account names, sign-ins, servers and identities.
        "Work",
        "Home",
        "me@example.test",
        "me@example.org",
        "example.test",
        "example.org",
        "Me at home",
        // Folder names.
        "Inbox",
        "INBOX",
        "Projects",
        "Archive",
        // Message content and correspondents.
        "Quarterly report",
        "Numbers attached",
        "Kai Santos",
        "kai@example",
        "Secret plans",
        "Hello",
        // Remote ids.
        "message-0",
        "thread-0",
    ] {
        assert!(
            !report.to_lowercase().contains(&private.to_lowercase()),
            "the report contains {private:?}:\n{report}"
        );
    }
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert_eq!(toasts, ["Diagnostics copied to the clipboard."]);
}
