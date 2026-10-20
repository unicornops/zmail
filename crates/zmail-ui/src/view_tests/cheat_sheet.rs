//! The keyboard shortcut cheat sheet (UI plan Phase 7): Secondary+/ and
//! the Gmail keymap's `?`, listing the bound keymap.

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zmail_core::preferences::KeymapName;

use super::{
    Setup, focus, focused_pane, keys, open_workspace, open_workspace_on, open_workspace_with,
    search_input, text,
};
use crate::cheat_sheet::ShortcutGroup;
use crate::pane::Pane;
use crate::{InitialView, WorkspaceView, keymap};

fn sheet(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<Vec<ShortcutGroup>> {
    cx.read_entity(view, |view, _| {
        view.shortcuts_modal
            .is_open()
            .then(|| view.shortcut_groups.clone())
    })
}

/// The keys the open sheet lists for `label`, in every group.
fn keys_for(groups: &[ShortcutGroup], label: &str) -> Vec<String> {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.label == label)
        .flat_map(|row| row.keys.clone())
        .collect()
}

#[gpui::test]
fn secondary_slash_opens_the_sheet_and_escape_closes_it(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    keys("secondary-/", cx);
    let groups = sheet(&view, cx).expect("the sheet is open");
    assert_eq!(groups[0].name, "General");
    let secondary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    assert_eq!(
        keys_for(&groups, "Command Palette"),
        [format!("{secondary}-k")]
    );
    assert!(keys_for(&groups, "Archive").contains(&format!("{secondary}-e")));
    assert!(
        keys_for(&groups, "Archive").iter().all(|key| key != "e"),
        "no Gmail keys in the standard sheet"
    );
    assert!(cx.debug_bounds("keyboard-shortcuts-groups").is_some());

    keys("escape", cx);
    assert!(sheet(&view, cx).is_none());
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Secondary+/ toggles it closed too.
    keys("secondary-/", cx);
    assert!(sheet(&view, cx).is_some());
    keys("secondary-/", cx);
    assert!(sheet(&view, cx).is_none());
}

#[gpui::test]
fn question_mark_opens_the_gmail_sheet_but_types_in_a_field(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| preferences.keymap = KeymapName::Gmail)
                .expect("the preference saves");
        },
    );
    keys("?", cx);
    let groups = sheet(&view, cx).expect("? opens the sheet");
    assert_eq!(keys_for(&groups, "Go to the inbox"), ["g i"]);
    assert!(keys_for(&groups, "Archive").contains(&"e".to_owned()));
    assert!(keys_for(&groups, "Keyboard Shortcuts").contains(&"?".to_owned()));
    keys("escape", cx);

    let search = search_input(&view, cx);
    focus(&search, cx);
    cx.simulate_input("?");
    keys("?", cx);
    assert!(sheet(&view, cx).is_none(), "? in a text field is just text");
    assert!(text(&search, cx).starts_with('?'));
}

#[gpui::test]
fn the_sheet_follows_a_keymap_switch(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let store = harness.store.clone();
    cx.update(|_, cx| keymap::use_keymap(&store, KeymapName::Gmail, cx))
        .expect("the choice saves");
    keys("secondary-/", cx);
    let groups = sheet(&view, cx).expect("the sheet is open");
    assert!(keys_for(&groups, "Archive").contains(&"e".to_owned()));
}

#[gpui::test]
fn the_window_can_open_with_the_sheet_showing(cx: &mut TestAppContext) {
    let (view, cx, _harness) =
        open_workspace_on(cx, Setup::Mail, |_| InitialView::KeyboardShortcuts);
    assert!(sheet(&view, cx).is_some());
    assert!(cx.debug_bounds("keyboard-shortcuts-groups").is_some());
}
