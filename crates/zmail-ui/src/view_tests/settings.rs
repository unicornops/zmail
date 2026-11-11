//! The Settings window (UI plan Phase 6): opening it, Appearance and
//! Sending applying live, theme import, undelivered changes and Copy
//! Diagnostics.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, px, size};
use zmail_core::{
    AccountColor, ThemeAppearance,
    preferences::{Density, KeymapName, ReadingPanePosition, ThemeMode},
};
use zmail_store::LocalStore;

use super::onboarding::fake_sign_in_check;
use super::{Harness, Setup, click, drawn, focus, keys, open_workspace, open_workspace_with};
use crate::InitialView;
use crate::actions::ToggleDensity;
use crate::components::{ComponentTheme, Select};
use crate::layout::SinglePane;
use crate::settings::{self, SettingsSection, SettingsWindow};
use zmail_setup::check::CheckPart;
use zmail_setup::identity_editor::IdentityField;

/// The open Settings window's root, and a test context for that window.
pub(super) fn settings_window(
    cx: &mut VisualTestContext,
) -> (Entity<SettingsWindow>, VisualTestContext) {
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
        SettingsSection::Accounts
    );

    // Down in the section list moves to the next section.
    keys("down", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Appearance
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
    cx.update(|_, cx| settings::open(Some(SettingsSection::Appearance), cx));
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

#[gpui::test]
fn account_colour_and_cache_budget_save_and_show(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let account_id = harness.store.list_accounts().expect("accounts")[0].id;
    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    let (color, budget) = accounts
        .read_with(&window_cx, |accounts, _| accounts.selects(account_id))
        .expect("the account has a card");
    assert_eq!(
        color.read_with(&window_cx, |select, _| select
            .selected_label()
            .map(|label| label.to_string())),
        Some("Automatic (Blue)".to_owned())
    );

    // Pink (Automatic, then the palette in order).
    pick(&color, 1 + AccountColor::Pink.index(), &mut window_cx);
    assert_eq!(
        harness.store.list_accounts().expect("accounts")[0].color(),
        AccountColor::Pink
    );
    assert_eq!(
        cx.read_entity(&view, |view, _| view.accounts[0].color()),
        AccountColor::Pink,
        "the main window shows it at once"
    );
    pick(&color, 0, &mut window_cx);
    assert_eq!(
        harness.store.list_accounts().expect("accounts")[0].color_override,
        None
    );

    // 1 GB.
    pick(&budget, 3, &mut window_cx);
    assert_eq!(
        harness
            .store
            .account_disk_budget_bytes(account_id)
            .expect("loads"),
        Some(1024 * 1024 * 1024)
    );
    pick(&budget, 0, &mut window_cx);
    assert_eq!(
        harness
            .store
            .account_disk_budget_bytes(account_id)
            .expect("loads"),
        None
    );
}

#[gpui::test]
fn reauthenticate_saves_a_new_password_in_the_keychain(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let account_id = harness.store.list_accounts().expect("accounts")[0].id;
    let check = fake_sign_in_check(cx);
    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    click_selector(&format!("reauth-{account_id}"), &mut window_cx);
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.reauth_open()));
    let password = accounts.read_with(&window_cx, |accounts, _| accounts.password_input().clone());
    password.update(&mut window_cx, |input, cx| {
        input.set_text("new-app-password", cx)
    });
    click("reauth-save", &mut window_cx);
    // The new password was tested against the incoming server first.
    assert_eq!(
        check.tested.lock().expect("lock").as_slice(),
        [(CheckPart::Incoming, "new-app-password".to_owned())]
    );
    assert!(!accounts.read_with(&window_cx, |accounts, _| accounts.reauth_open()));
    let saved = cx.update(|_, cx| {
        settings::SettingsHost::get(cx)
            .secrets
            .get_account_secret(account_id)
            .ok()
    });
    assert_eq!(saved.as_deref(), Some("new-app-password"));
    assert!(
        password.read_with(&window_cx, |input, _| input.text().is_empty()),
        "the password does not stay in the field"
    );
}

#[gpui::test]
fn a_new_password_the_server_refuses_is_kept_out_of_the_keychain(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let account_id = harness.store.list_accounts().expect("accounts")[0].id;
    let secret = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            settings::SettingsHost::get(cx)
                .secrets
                .get_account_secret(account_id)
                .ok()
        })
    };
    let before = secret(cx);
    let check = fake_sign_in_check(cx);
    check.fail(CheckPart::Incoming, true);
    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    click_selector(&format!("reauth-{account_id}"), &mut window_cx);
    let password = accounts.read_with(&window_cx, |accounts, _| accounts.password_input().clone());
    password.update(&mut window_cx, |input, cx| input.set_text("typo", cx));
    click("reauth-save", &mut window_cx);

    // The dialog stays open with the reason, and the saved password is
    // untouched.
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.reauth_open()));
    assert!(window_cx.debug_bounds("reauth-failure").is_some());
    assert_eq!(secret(cx), before);
    assert_eq!(
        password.read_with(&window_cx, |input, _| input.text().to_owned()),
        "typo",
        "the field stays as typed, to correct"
    );

    // Save anyway keeps it all the same, for a server that is down.
    click("reauth-save-anyway", &mut window_cx);
    assert!(!accounts.read_with(&window_cx, |accounts, _| accounts.reauth_open()));
    assert_eq!(secret(cx).as_deref(), Some("typo"));
    assert_eq!(check.parts(), [CheckPart::Incoming]);
}

#[gpui::test]
fn remove_confirms_then_deletes_local_data_and_secrets(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace_with(
        cx,
        Setup::Accounts,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let work = harness.store.list_accounts().expect("accounts")[0].id;
    super::queue_test_message(&harness.store, "Not sent yet");
    cx.update(|_, cx| {
        settings::SettingsHost::get(cx)
            .secrets
            .set_account_secret(work, "work-password")
            .expect("stored");
    });
    let (root, mut window_cx) = settings_window(cx);
    window_cx.simulate_resize(size(px(820.0), px(1400.0)));
    window_cx.run_until_parked();
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());

    // Cancel keeps everything.
    click_selector(&format!("remove-{work}"), &mut window_cx);
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.removal_open()));
    click("remove-cancel", &mut window_cx);
    assert!(!accounts.read_with(&window_cx, |accounts, _| accounts.removal_open()));
    assert_eq!(harness.store.list_accounts().expect("accounts").len(), 2);

    click_selector(&format!("remove-{work}"), &mut window_cx);
    click("remove-confirm", &mut window_cx);
    let left = harness.store.list_accounts().expect("accounts");
    assert_eq!(left.len(), 1);
    assert_ne!(left[0].id, work);
    assert!(
        harness
            .store
            .list_outbox_jobs(Some(work))
            .expect("outbox")
            .is_empty()
    );
    let secret = cx.update(|_, cx| {
        settings::SettingsHost::get(cx)
            .secrets
            .get_account_secret(work)
            .ok()
    });
    assert_eq!(secret, None, "the keychain entry is gone");
    assert_eq!(
        cx.read_entity(&view, |view, _| view.accounts.len()),
        1,
        "the main window no longer shows it"
    );
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert_eq!(
        toasts,
        ["Removed Work from this computer. Its mail on the server is untouched."]
    );
}

/// Links the account's identity to a server identity with another name
/// (a conflict), and leaves a second identity the server stopped
/// listing; returns their ids.
fn seed_identities_needing_attention(store: &LocalStore) -> (i64, i64) {
    use zmail_core::identity_sync::{IdentityValues, ServerIdentity};
    let account_id = store.list_accounts().expect("accounts")[0].id;
    let primary = store.list_identities(account_id).expect("identities")[0].clone();
    let server = |id: &str, email: &str, name: &str| ServerIdentity {
        id: id.to_owned(),
        email: email.to_owned(),
        values: IdentityValues {
            name: name.to_owned(),
            reply_to: String::new(),
            signature: "-- from the server".to_owned(),
        },
        may_delete: true,
    };
    let both = [
        server("i1", &primary.email_address, "Name on the server"),
        server("i2", "help@example.test", "Help desk"),
    ];
    store
        .merge_server_identities(account_id, &both)
        .expect("merged");
    store
        .merge_server_identities(account_id, &both[..1])
        .expect("merged");
    let help = store.list_identities(account_id).expect("identities")[1].id;
    (primary.id, help)
}

#[gpui::test]
fn identities_that_differ_from_the_server_offer_both_directions(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |store| {
            seed_identities_needing_attention(store);
        },
    );
    let account_id = harness.store.list_accounts().expect("accounts")[0].id;
    let identities = harness
        .store
        .list_identities(account_id)
        .expect("identities");
    let (primary, help) = (identities[0].id, identities[1].id);
    let (root, mut window_cx) = settings_window(cx);
    window_cx.simulate_resize(size(px(820.0), px(1400.0)));
    window_cx.run_until_parked();

    // The differing identity: both choices; the removed one: its two.
    for selector in [
        format!("identity-update-server-{primary}"),
        format!("identity-use-server-{primary}"),
        format!("identity-recreate-{help}"),
        format!("identity-remove-{help}"),
    ] {
        assert!(
            drawn(selector.clone(), &mut window_cx),
            "{selector} is drawn"
        );
    }
    assert!(!drawn(
        format!("identity-use-server-{help}"),
        &mut window_cx
    ));

    // (Update the server runs on the sync runtime, which GPUI's test
    // scheduler can't wait for; zmail-jmap and the live James test cover
    // it.)
    let notice = root.read_with(&window_cx, |root, cx| {
        root.accounts().read(cx).identity_notice_titles()
    });
    assert_eq!(
        notice,
        [
            "me@example.test differs from the server",
            "help@example.test is no longer on the server"
        ]
    );

    // Use the server's: the name changes and the notice goes.
    click_selector(&format!("identity-use-server-{primary}"), &mut window_cx);
    assert_eq!(
        harness
            .store
            .list_identities(account_id)
            .expect("identities")[0]
            .display_name,
        "Name on the server"
    );
    assert!(!drawn(
        format!("identity-use-server-{primary}"),
        &mut window_cx
    ));

    // Remove from Zmail, after asking.
    click_selector(&format!("identity-remove-{help}"), &mut window_cx);
    assert!(root.read_with(&window_cx, |root, cx| {
        root.accounts().read(cx).identity_removal_open()
    }));
    click_selector("identity-remove-confirm", &mut window_cx);
    assert_eq!(
        harness
            .store
            .list_identities(account_id)
            .expect("identities")
            .len(),
        1
    );
    assert!(!drawn(format!("identity-remove-{help}"), &mut window_cx));
}

/// Adds a JMAP account whose identity is linked to a server identity the
/// user may delete, plus a second linked one, and a Proton account;
/// returns the JMAP account and its two identities, and the Proton
/// account.
fn seed_jmap_and_proton_accounts(store: &LocalStore) -> (i64, i64, i64, i64) {
    use zmail_core::{
        AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind,
        identity_sync::{IdentityValues, ServerIdentity},
    };
    let account = |name: &str, protocol: ProtocolKind, email: &str| NewAccount {
        display_name: name.to_owned(),
        protocol,
        auth: AuthKind::Password,
        endpoint: "https://mail.example.test".to_owned(),
        username: email.to_owned(),
        cache_policy: CachePolicySummary::performance_first(),
        outgoing: None,
        identities: vec![NewIdentity {
            display_name: "Rob".to_owned(),
            email_address: email.to_owned(),
            signature: String::new(),
            is_primary: true,
        }],
    };
    let jmap = store
        .insert_account(&account("Fastmail", ProtocolKind::Jmap, "rob@fast.test"))
        .expect("stored");
    let proton = store
        .insert_account(&account("Proton", ProtocolKind::Proton, "rob@proton.test"))
        .expect("stored");
    let server = |id: &str, email: &str| ServerIdentity {
        id: id.to_owned(),
        email: email.to_owned(),
        values: IdentityValues {
            name: "Rob".to_owned(),
            ..IdentityValues::default()
        },
        may_delete: true,
    };
    store
        .merge_server_identities(
            jmap,
            &[
                server("i1", "rob@fast.test"),
                server("i2", "help@fast.test"),
            ],
        )
        .expect("merged");
    let identities = store.list_identities(jmap).expect("identities");
    (jmap, identities[0].id, identities[1].id, proton)
}

fn accounts_section(
    root: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> Entity<crate::settings::AccountsSettings> {
    root.read_with(cx, |root, _| root.accounts().clone())
}

fn set_field(
    accounts: &Entity<crate::settings::AccountsSettings>,
    field: IdentityField,
    text: &str,
    cx: &mut VisualTestContext,
) {
    let input = accounts.read_with(cx, |accounts, _| accounts.identity_input(field));
    input.update(cx, |input, cx| input.set_text(text, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn an_identity_is_edited_and_a_server_set_address_stays_fixed(cx: &mut TestAppContext) {
    use zmail_core::identity_sync::IdentitySyncStatus;

    let mut seeded = (0, 0, 0, 0);
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |store| seeded = seed_jmap_and_proton_accounts(store),
    );
    let (jmap, linked, _, proton) = seeded;
    let imap = harness.store.list_accounts().expect("accounts")[0].id;
    let mine = harness.store.list_identities(imap).expect("identities")[0].id;
    let (root, mut window_cx) = settings_window(cx);
    window_cx.simulate_resize(size(px(820.0), px(2400.0)));
    window_cx.run_until_parked();
    let accounts = accounts_section(&root, &mut window_cx);

    // An IMAP identity: every field, the address too.
    click_selector(&format!("identity-edit-{mine}"), &mut window_cx);
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.identity_editor_open()));
    let address = accounts.read_with(&window_cx, |accounts, _| {
        accounts.identity_input(IdentityField::Address)
    });
    assert!(!address.read_with(&window_cx, |input, _| input.is_disabled()));
    assert!(!drawn("identity-save-server".into(), &mut window_cx));

    // A bad address keeps the dialog open, saying why.
    set_field(
        &accounts,
        IdentityField::Address,
        "not an address",
        &mut window_cx,
    );
    click_selector("identity-save", &mut window_cx);
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.identity_editor_open()));
    assert_eq!(
        accounts.read_with(&window_cx, |accounts, _| accounts
            .identity_errors()
            .into_iter()
            .map(|(field, _)| field)
            .collect::<Vec<_>>()),
        [IdentityField::Address]
    );
    assert!(drawn("identity-error-Address".into(), &mut window_cx));

    set_field(
        &accounts,
        IdentityField::Address,
        "rob@example.test",
        &mut window_cx,
    );
    set_field(
        &accounts,
        IdentityField::Name,
        "Rob at work",
        &mut window_cx,
    );
    set_field(
        &accounts,
        IdentityField::ReplyTo,
        "team@example.test",
        &mut window_cx,
    );
    set_field(
        &accounts,
        IdentityField::Signature,
        "Regards,\nRob\nExample Ltd",
        &mut window_cx,
    );
    click_selector("identity-save", &mut window_cx);
    assert!(!accounts.read_with(&window_cx, |accounts, _| accounts.identity_editor_open()));
    let saved = &harness
        .store
        .list_synced_identities(imap)
        .expect("identities")[0];
    assert_eq!(saved.identity.email_address, "rob@example.test");
    assert_eq!(saved.local.name, "Rob at work");
    assert_eq!(saved.local.reply_to, "team@example.test");
    assert_eq!(saved.local.signature, "Regards,\nRob\nExample Ltd");
    assert!(
        root.read_with(&window_cx, |root, cx| root.toast_messages(cx))
            .contains(&"Saved rob@example.test.".to_owned())
    );

    // A JMAP identity linked to the server: its address is the server's,
    // and the edit can go up at once.
    click_selector(&format!("identity-edit-{linked}"), &mut window_cx);
    assert!(address.read_with(&window_cx, |input, _| input.is_disabled()));
    assert!(drawn("identity-save-server".into(), &mut window_cx));
    set_field(
        &accounts,
        IdentityField::Signature,
        "-- Rob, from Zmail",
        &mut window_cx,
    );
    // (Save and update the server runs on the sync runtime, which GPUI's
    // test scheduler can't wait for; the live James test covers it.)
    click_selector("identity-save", &mut window_cx);
    let synced = &harness
        .store
        .list_synced_identities(jmap)
        .expect("identities")[0];
    assert_eq!(synced.local.signature, "-- Rob, from Zmail");
    assert_eq!(synced.identity.email_address, "rob@fast.test");
    assert_eq!(synced.status, IdentitySyncStatus::ChangedHere);
    // Which the account's notice now offers to send up.
    assert!(drawn(
        format!("identity-update-server-{linked}"),
        &mut window_cx
    ));

    // A Proton address is fixed, and Proton adds no identities.
    let proton_identity = harness.store.list_identities(proton).expect("identities")[0].id;
    assert!(!drawn(format!("identity-add-{proton}"), &mut window_cx));
    click_selector(&format!("identity-edit-{proton_identity}"), &mut window_cx);
    assert!(address.read_with(&window_cx, |input, _| input.is_disabled()));
    assert!(!drawn("identity-save-server".into(), &mut window_cx));
    click_selector("identity-cancel", &mut window_cx);
    assert!(!accounts.read_with(&window_cx, |accounts, _| accounts.identity_editor_open()));
}

#[gpui::test]
fn identities_are_added_made_primary_and_removed_after_asking(cx: &mut TestAppContext) {
    let mut seeded = (0, 0, 0, 0);
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |store| seeded = seed_jmap_and_proton_accounts(store),
    );
    let (jmap, _, help, _) = seeded;
    let imap = harness.store.list_accounts().expect("accounts")[0].id;
    let first = harness.store.list_identities(imap).expect("identities")[0].id;
    let (root, mut window_cx) = settings_window(cx);
    window_cx.simulate_resize(size(px(820.0), px(2400.0)));
    window_cx.run_until_parked();
    let accounts = accounts_section(&root, &mut window_cx);
    // The only identity can't be removed.
    assert!(!drawn(format!("identity-delete-{first}"), &mut window_cx));

    // Add: IMAP keeps it in Zmail only.
    click_selector(&format!("identity-add-{imap}"), &mut window_cx);
    assert!(!drawn("identity-save-server".into(), &mut window_cx));
    set_field(&accounts, IdentityField::Name, "Alerts", &mut window_cx);
    set_field(
        &accounts,
        IdentityField::Address,
        "ME@example.test",
        &mut window_cx,
    );
    click_selector("identity-save", &mut window_cx);
    assert!(
        accounts.read_with(&window_cx, |accounts, _| accounts.identity_editor_open()),
        "the account already has that address"
    );
    set_field(
        &accounts,
        IdentityField::Address,
        "alerts@example.test",
        &mut window_cx,
    );
    click_selector("identity-save", &mut window_cx);
    let identities = harness.store.list_identities(imap).expect("identities");
    assert_eq!(identities.len(), 2);
    let added = identities[1].id;
    assert_eq!(identities[1].email_address, "alerts@example.test");
    assert!(!identities[1].is_primary);

    // Make primary.
    click_selector(&format!("identity-make-primary-{added}"), &mut window_cx);
    let identities = harness.store.list_identities(imap).expect("identities");
    assert_eq!(identities[0].id, added);
    assert!(identities[0].is_primary);
    assert!(!drawn(
        format!("identity-make-primary-{added}"),
        &mut window_cx
    ));
    assert!(!drawn(format!("identity-delete-{added}"), &mut window_cx));

    // Remove the old one, after asking: Cancel keeps it.
    click_selector(&format!("identity-delete-{first}"), &mut window_cx);
    assert!(accounts.read_with(&window_cx, |accounts, _| accounts.identity_removal_open()));
    assert!(!drawn("identity-remove-everywhere".into(), &mut window_cx));
    click_selector("identity-remove-cancel", &mut window_cx);
    assert_eq!(
        harness
            .store
            .list_identities(imap)
            .expect("identities")
            .len(),
        2
    );
    click_selector(&format!("identity-delete-{first}"), &mut window_cx);
    click_selector("identity-remove-confirm", &mut window_cx);
    assert_eq!(
        harness
            .store
            .list_identities(imap)
            .expect("identities")
            .len(),
        1
    );
    assert!(
        root.read_with(&window_cx, |root, cx| root.toast_messages(cx))
            .contains(&"Removed me@example.test from Zmail.".to_owned())
    );

    // A JMAP account adds with the choice to create it on the server, and
    // removing an identity the server lets the user delete offers the
    // server too.
    click_selector(&format!("identity-add-{jmap}"), &mut window_cx);
    assert!(drawn("identity-save-server".into(), &mut window_cx));
    click_selector("identity-cancel", &mut window_cx);
    click_selector(&format!("identity-delete-{help}"), &mut window_cx);
    assert!(drawn("identity-remove-everywhere".into(), &mut window_cx));
    click_selector("identity-remove-confirm", &mut window_cx);
    assert_eq!(
        harness
            .store
            .list_identities(jmap)
            .expect("identities")
            .len(),
        1
    );
    // Removed from Zmail only while the server lists it: hidden, under
    // the account's identities, with Bring back.
    let hidden = harness.store.list_hidden_identities(jmap).expect("hidden");
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].email, "help@fast.test");
    assert!(drawn(format!("hidden-identities-{jmap}"), &mut window_cx));
    click_selector(
        &format!("identity-bring-back-{}", hidden[0].id),
        &mut window_cx,
    );
    assert_eq!(
        harness
            .store
            .list_identities(jmap)
            .expect("identities")
            .len(),
        2
    );
    assert!(
        harness
            .store
            .list_hidden_identities(jmap)
            .expect("hidden")
            .is_empty()
    );
    assert!(!drawn(format!("hidden-identities-{jmap}"), &mut window_cx));
    assert!(
        root.read_with(&window_cx, |root, cx| root.toast_messages(cx))
            .contains(&"Brought back help@fast.test.".to_owned())
    );
}

#[gpui::test]
fn privacy_states_what_is_protected(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Privacy),
        |_| {},
    );
    let (root, mut window_cx) = settings_window(cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Privacy
    );
    assert!(window_cx.debug_bounds("settings-nav-privacy").is_some());
}

#[gpui::test]
fn keyboard_switches_the_keymap_and_searches_the_shortcuts(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Keyboard),
        |_| {},
    );
    let (root, mut window_cx) = settings_window(cx);
    let keyboard = root.read_with(&window_cx, |root, _| root.keyboard().clone());
    let labels = |window_cx: &mut VisualTestContext| {
        keyboard.read_with(window_cx, |keyboard, cx| {
            keyboard
                .visible(cx)
                .into_iter()
                .flat_map(|(_, rows)| rows.into_iter().map(|row| row.label.clone()))
                .collect::<Vec<_>>()
        })
    };
    let gmail_keys = |window_cx: &mut VisualTestContext| {
        keyboard.read_with(window_cx, |keyboard, cx| {
            keyboard
                .visible(cx)
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .any(|row| row.label == "Archive" && row.keys.iter().any(|key| key == "e"))
        })
    };
    assert!(labels(&mut window_cx).contains(&"Archive".to_owned()));
    assert!(!gmail_keys(&mut window_cx), "no single-key shortcuts yet");

    // Gmail: bound at once, saved, and the list shows the new keys.
    let select = keyboard.read_with(&window_cx, |keyboard, _| keyboard.keymap_select().clone());
    pick(&select, 1, &mut window_cx);
    assert_eq!(
        harness.store.ui_preferences().expect("loads").keymap,
        KeymapName::Gmail
    );
    assert_eq!(
        cx.update(|_, cx| crate::keymap::active_keymap(cx)),
        KeymapName::Gmail
    );
    assert!(gmail_keys(&mut window_cx));

    // Search narrows the list by label, place or key.
    let search = keyboard.read_with(&window_cx, |keyboard, _| keyboard.search().clone());
    focus(&search, &mut window_cx);
    window_cx.simulate_input("archive");
    window_cx.run_until_parked();
    let found = labels(&mut window_cx);
    assert!(!found.is_empty());
    assert!(
        found
            .iter()
            .all(|label| label.to_lowercase().contains("archive")),
        "{found:?}"
    );
    search.update(&mut window_cx, |input, cx| input.set_text("", cx));
    window_cx.simulate_input("no such command");
    window_cx.run_until_parked();
    assert!(labels(&mut window_cx).is_empty());

    pick(&select, 0, &mut window_cx);
    assert_eq!(
        cx.update(|_, cx| crate::keymap::active_keymap(cx)),
        KeymapName::Default
    );
}

#[gpui::test]
fn an_expired_google_sign_in_explains_the_7_day_testing_limit(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let store = &harness.store;
    let account_id = store
        .insert_account(&zmail_core::NewAccount {
            display_name: "Gmail".to_owned(),
            protocol: zmail_core::ProtocolKind::Imap,
            auth: zmail_core::AuthKind::OAuth2,
            endpoint: "imaps://imap.gmail.com:993".to_owned(),
            username: "me@example.test".to_owned(),
            cache_policy: zmail_core::CachePolicySummary::performance_first(),
            outgoing: None,
            identities: vec![zmail_core::NewIdentity {
                display_name: "Me".to_owned(),
                email_address: "me@example.test".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        })
        .expect("the account is stored");
    store
        .save_account_oauth2_client(
            account_id,
            "https://oauth2.googleapis.com/token",
            "42-abc.apps.googleusercontent.com",
        )
        .expect("the client is stored");
    let expired = zmail_core::reauth::ReauthRequired::new(
        zmail_core::reauth::ReauthReason::GrantExpired,
        "invalid_grant: Token has been expired or revoked.",
    );
    store
        .record_sync_failure(account_id, "Sync failed after retry", expired.to_string())
        .expect("the failure is recorded");

    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    accounts.update(&mut window_cx, |accounts, cx| {
        accounts.focus_account(account_id, cx)
    });
    window_cx.run_until_parked();
    assert!(window_cx.debug_bounds("account-fix").is_some());
    assert!(
        window_cx.debug_bounds("account-fix-reauth").is_some(),
        "Re-authenticate is offered"
    );
    assert!(
        window_cx.debug_bounds("account-fix-guide").is_some(),
        "the guide's fix is linked"
    );
}

/// The first account, failing to sync with `error`, and Settings →
/// Accounts opened on it as Fix… opens it.
fn fix_banner_for(
    error: &str,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, VisualTestContext, i64, &'static str) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let account = harness.store.list_accounts().expect("accounts")[0].clone();
    harness
        .store
        .record_sync_failure(account.id, "Sync failed after retry", error)
        .expect("the failure is recorded");
    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    accounts.update(&mut window_cx, |accounts, cx| {
        accounts.focus_account(account.id, cx)
    });
    window_cx.run_until_parked();
    let name: &'static str = account.display_name.leak();
    (root, window_cx, account.id, name)
}

#[gpui::test]
fn a_server_that_cannot_be_reached_offers_try_again(cx: &mut TestAppContext) {
    let (root, mut window_cx, _, name) =
        fix_banner_for("timed out while connecting to the IMAP server", cx);
    assert!(window_cx.debug_bounds("account-fix").is_some());
    assert!(window_cx.debug_bounds("account-fix-retry").is_some());
    assert!(
        window_cx.debug_bounds("account-fix-reauth").is_none(),
        "signing in again would not help"
    );
    click_selector("account-fix-retry", &mut window_cx);
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert_eq!(toasts, [format!("Trying {name} again.")]);
}

#[gpui::test]
fn a_refused_sign_in_offers_only_re_authenticate(cx: &mut TestAppContext) {
    let (_root, mut window_cx, _, _) = fix_banner_for("IMAP authentication failed", cx);
    assert!(window_cx.debug_bounds("account-fix-reauth").is_some());
    assert!(window_cx.debug_bounds("account-fix-retry").is_none());
}

#[gpui::test]
fn any_other_failure_offers_both(cx: &mut TestAppContext) {
    let (_root, mut window_cx, _, _) = fix_banner_for("failed to list IMAP mailboxes", cx);
    assert!(window_cx.debug_bounds("account-fix-reauth").is_some());
    assert!(window_cx.debug_bounds("account-fix-retry").is_some());
}

#[gpui::test]
fn re_authenticate_on_a_proton_account_is_never_silent(cx: &mut TestAppContext) {
    use zmail_core::{AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind};
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Settings(SettingsSection::Accounts),
        |_| {},
    );
    let proton = harness
        .store
        .insert_account(&NewAccount {
            display_name: "rob@proton.test".to_owned(),
            protocol: ProtocolKind::Proton,
            auth: AuthKind::ProtonSession,
            endpoint: "https://mail-api.proton.me".to_owned(),
            username: "rob".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: None,
            identities: vec![NewIdentity {
                display_name: "Rob".to_owned(),
                email_address: "rob@proton.test".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        })
        .expect("stored");
    let (root, mut window_cx) = settings_window(cx);
    let accounts = root.read_with(&window_cx, |root, _| root.accounts().clone());
    accounts.update(&mut window_cx, |accounts, cx| accounts.refresh(cx));
    window_cx.run_until_parked();

    // A session Proton has not ended: nothing to do, and it says so.
    click_selector(&format!("reauth-{proton}"), &mut window_cx);
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert_eq!(toasts.len(), 1);
    assert!(
        toasts[0].starts_with("Proton hasn't ended Zmail's session"),
        "{toasts:?}"
    );

    // Once Proton ends it, Re-authenticate signs in to that account again.
    harness
        .store
        .record_sync_failure(
            proton,
            "Sync failed after retry",
            format!("the Proton session for account {proton} has ended; sign in again"),
        )
        .expect("the failure is recorded");
    accounts.update(&mut window_cx, |accounts, cx| accounts.refresh(cx));
    window_cx.run_until_parked();
    // (The first answer's toast still covers the card's button.)
    window_cx.update(|window, cx| {
        accounts.update(cx, |accounts, cx| {
            accounts.reauthenticate(proton, window, cx)
        })
    });
    window_cx.run_until_parked();
    #[cfg(feature = "proton")]
    {
        let handle = window_cx
            .update(|_, cx| crate::account_setup::open_window(cx))
            .expect("the sign-in window is open");
        let setup = handle
            .read_with(&window_cx, |root, _| root.setup().clone())
            .expect("the window is open");
        setup.read_with(&window_cx, |setup, cx| {
            assert_eq!(setup.stage, crate::account_setup::Stage::ProtonSignIn);
            assert_eq!(
                setup.signing_in_again().map(|again| again.account_id),
                Some(proton)
            );
            assert_eq!(
                setup
                    .form
                    .text(crate::account_setup::AccountField::Email, cx),
                "rob"
            );
            assert!(!setup.can_go_back(), "it is not an Add Account flow");
        });
        assert_eq!(
            harness.store.list_accounts().expect("accounts").len(),
            2,
            "nothing is added"
        );
    }
    #[cfg(not(feature = "proton"))]
    {
        let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
        assert!(
            toasts
                .iter()
                .any(|toast| toast.contains("no Proton Mail support")),
            "{toasts:?}"
        );
    }
}
