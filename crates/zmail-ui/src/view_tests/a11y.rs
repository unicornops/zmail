//! Screen-reader names (UI plan Phase 8): every control the main views
//! draw has a non-empty accessible label, and rows, panes and dialogs say
//! what they are.
//!
//! GPUI's test platform never turns AccessKit on, so these tests cannot
//! read the accessibility tree itself. They read the roles and labels
//! [`AccessibleExt::accessible`](crate::a11y::AccessibleExt::accessible)
//! set while the views render (see [`crate::a11y::audit`]).

use gpui::{AppContext as _, Entity, Role, TestAppContext, VisualTestContext};

use super::settings::settings_window;
use super::{Harness, Setup, focus, keys, open_workspace, open_workspace_on, search_input, toasts};
use crate::a11y::audit;
use crate::components::ComponentTheme;
use crate::settings::SettingsSection;
use crate::styles::UiColors;
use crate::styles::high_contrast::{Contrast, ContrastSetting, SystemColors};
use crate::{InitialView, WorkspaceView};

/// Fails on any empty label, naming its role.
fn assert_all_named(entries: &[(Role, String)]) {
    assert!(!entries.is_empty(), "nothing was recorded");
    let unnamed: Vec<Role> = entries
        .iter()
        .filter(|(_, label)| label.trim().is_empty())
        .map(|(role, _)| *role)
        .collect();
    assert!(unnamed.is_empty(), "unnamed controls: {unnamed:?}");
}

fn has(entries: &[(Role, String)], role: Role, label: &str) -> bool {
    audit::labels(entries, role).contains(&label)
}

fn first_thread(harness: &Harness) -> InitialView {
    InitialView::Thread(harness.threads[0])
}

#[gpui::test]
fn the_main_window_names_every_control(cx: &mut TestAppContext) {
    audit::start();
    let (view, cx, _harness) = open_workspace_on(cx, Setup::Accounts, first_thread);
    // The palette, the cheat sheet and a compose pane draw too.
    keys("secondary-k", cx);
    keys("escape", cx);
    keys("secondary-/", cx);
    keys("escape", cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.open_compose_in_pane_for_test(window, cx);
        })
    });
    cx.run_until_parked();
    let entries = audit::finish();
    assert_all_named(&entries);

    // The panes.
    assert!(has(&entries, Role::Tree, "Mailboxes"), "{entries:?}");
    assert!(has(&entries, Role::Pane, "Message list"));
    assert!(has(&entries, Role::Pane, "Reader"));
    assert!(has(&entries, Role::Toolbar, "Mail toolbar"));
    assert!(has(&entries, Role::Toolbar, "Conversation actions"));

    // Sidebar rows read their name and unread count.
    let tree_items = audit::labels(&entries, Role::TreeItem);
    assert!(
        tree_items
            .iter()
            .any(|label| label.starts_with("Unified Inbox, ") && label.ends_with(" unread")),
        "{tree_items:?}"
    );
    assert!(
        tree_items.contains(&"Home Inbox, 2 unread"),
        "{tree_items:?}"
    );
    assert!(
        tree_items
            .iter()
            .any(|label| label.starts_with("Home account, "))
    );

    // Message rows read as the plan describes.
    let rows = audit::labels(&entries, Role::ListBoxOption);
    assert!(
        rows.iter()
            .any(|label| { label.starts_with("Unread, from Kai Santos, Quarterly report ") }),
        "{rows:?}"
    );

    // The open conversation: its subject, and each message as an article.
    assert!(
        audit::labels(&entries, Role::Heading)
            .iter()
            .any(|label| label.starts_with("Quarterly report "))
    );
    assert!(
        audit::labels(&entries, Role::Article)
            .iter()
            .any(|label| label.contains("from Kai Santos")),
    );

    // Icon-only buttons are named.
    for label in ["Reply", "More actions", "Add Account"] {
        assert!(has(&entries, Role::Button, label), "no {label} button");
    }

    // The palette, the cheat sheet and the compose form.
    assert!(has(
        &entries,
        Role::ListBox,
        "Commands, folders and accounts"
    ));
    assert!(
        audit::labels(&entries, Role::ListItem)
            .iter()
            .any(|label| label.starts_with("Command Palette: ")),
    );
    assert!(has(&entries, Role::Form, "New Message"));

    // The palette's highlighted entry is announced, since focus stays in
    // its search field.
    assert!(
        audit::labels(&entries, Role::Status)
            .iter()
            .any(|label| label.contains(", 1 of ")),
        "{:?}",
        audit::labels(&entries, Role::Status)
    );
    assert!(has(&entries, Role::MultilineTextInput, "Message"));
}

#[gpui::test]
fn every_settings_section_names_its_controls(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Mail);
    audit::start();
    keys("secondary-,", cx);
    let (root, mut window_cx) = settings_window(cx);
    let w: &mut VisualTestContext = &mut window_cx;
    for section in SettingsSection::ALL {
        root.update(w, |root, cx| root.show_section(section, cx));
        w.run_until_parked();
    }
    let entries = audit::finish();
    assert_all_named(&entries);
    assert!(has(&entries, Role::TabList, "Settings sections"));
    for section in SettingsSection::ALL {
        assert!(has(&entries, Role::Tab, section.label()), "{section:?}");
        assert!(
            has(&entries, Role::TabPanel, section.label()),
            "{section:?}"
        );
    }
    // Each account's card names the buttons inside it.
    assert!(audit::labels(&entries, Role::Group).contains(&"Work"));
}

#[gpui::test]
fn onboarding_names_its_controls(cx: &mut TestAppContext) {
    audit::start();
    let (_view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    cx.run_until_parked();
    let entries = audit::finish();
    assert_all_named(&entries);
    assert!(!audit::labels(&entries, Role::Heading).is_empty());
    assert!(has(&entries, Role::Button, "Add your first account"));
}

fn announcement(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.read_entity(view, |view, _| {
        view.announcer
            .current()
            .map(|announcement| announcement.text.to_string())
    })
}

fn refresh(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    cx.update(|_, cx| view.update(cx, |view, cx| view.refresh_from_store(cx)));
    cx.run_until_parked();
}

#[gpui::test]
fn a_new_search_announces_its_result_count(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("quarterly");
    keys("enter", cx);
    assert_eq!(announcement(&view, cx).as_deref(), Some("3 results"));

    // Search Mail again, typing a query nothing matches.
    keys("secondary-a", cx);
    cx.simulate_input("zebra");
    keys("enter", cx);
    assert_eq!(announcement(&view, cx).as_deref(), Some("No results"));
}

#[gpui::test]
fn sync_failures_and_recovery_are_announced(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let account_id = harness.store.list_accounts().expect("accounts load")[0].id;
    refresh(&view, cx);
    assert_eq!(announcement(&view, cx), None, "nothing at startup");

    harness
        .store
        .record_sync_failure(account_id, "IMAP sync failed", "connection timed out")
        .expect("the failure is recorded");
    refresh(&view, cx);
    assert_eq!(
        announcement(&view, cx).as_deref(),
        Some("Couldn't sync Work: connection timed out")
    );

    harness
        .store
        .record_sync_success(account_id, "IMAP sync complete", "checkpoint")
        .expect("the success is recorded");
    refresh(&view, cx);
    assert_eq!(announcement(&view, cx).as_deref(), Some("Updated just now"));
}

#[gpui::test]
fn toasts_and_errors_are_live_regions(cx: &mut TestAppContext) {
    audit::start();
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    // Archiving shows an undo toast.
    keys("secondary-e", cx);
    let toast = toasts(&view, cx).pop().expect("a toast");
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.store_error = Some("The store is locked".to_owned());
            cx.notify();
        })
    });
    cx.run_until_parked();
    let entries = audit::finish();
    assert!(has(&entries, Role::Status, &toast), "{entries:?}");
    assert!(has(&entries, Role::Alert, "The store is locked"));
    drop(harness);
}

#[gpui::test]
fn a_windows_contrast_theme_replaces_the_palette(cx: &mut TestAppContext) {
    let system = SystemColors::HIGH_CONTRAST_BLACK;
    cx.update(|cx| ContrastSetting::simulate(Contrast::System(system), cx));
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    let (colors, theme) = cx.update(|_, cx| (*cx.global::<UiColors>(), ComponentTheme::get(cx)));
    assert_eq!(colors.background, system.window);
    assert_eq!(colors.foreground, system.window_text);
    assert_eq!(colors.selection_background, system.highlight);
    assert_eq!(
        theme.colors, colors,
        "components draw with the same palette"
    );
    assert!(!theme.is_light);

    // Switching the theme keeps the system colours.
    keys("secondary-shift-t", cx);
    let after = cx.update(|_, cx| *cx.global::<UiColors>());
    assert_eq!(after, colors);
    drop(view);
}
