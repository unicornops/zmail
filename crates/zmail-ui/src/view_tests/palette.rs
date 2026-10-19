//! The command palette (UI plan Phase 7): Secondary+K, fuzzy search over
//! commands, folders and accounts, shortcuts, recently used entries.

use gpui::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext};
use zmail_core::{MailboxSelection, VirtualMailbox};

use super::{Setup, focused_pane, keys, mailbox_of, open_workspace, open_workspace_on};
use crate::command_palette::CommandPalette;
use crate::pane::Pane;
use crate::{InitialView, WorkspaceView};

fn palette(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Option<Entity<CommandPalette>> {
    cx.read_entity(view, |view, _| {
        view.command_palette
            .as_ref()
            .map(|open| open.palette().clone())
    })
}

fn open_palette(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> Entity<CommandPalette> {
    keys("secondary-k", cx);
    palette(view, cx).expect("Secondary+K opens the palette")
}

fn labels(palette: &Entity<CommandPalette>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.read_entity(palette, |palette, _| palette.matching_labels())
}

fn highlighted(palette: &Entity<CommandPalette>, cx: &mut VisualTestContext) -> String {
    cx.read_entity(palette, |palette, _| {
        palette
            .highlighted_label()
            .map(|label| label.to_string())
            .unwrap_or_default()
    })
}

fn type_text(text: &str, cx: &mut VisualTestContext) {
    cx.simulate_input(text);
    cx.run_until_parked();
}

#[gpui::test]
fn secondary_k_opens_the_palette_and_escape_gives_focus_back(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    let palette = open_palette(&view, cx);
    let field_focused = cx.update(|window, cx| palette.focus_handle(cx).is_focused(window));
    assert!(field_focused, "typing goes to the palette's search field");
    // Nothing typed: every command, folder and account, commands first.
    let all = labels(&palette, cx);
    assert_eq!(all.first().map(String::as_str), Some("New Message"));
    assert!(all.contains(&"Go to Archive".to_owned()));
    assert!(all.contains(&"Work (me@example.test)".to_owned()));
    assert!(
        !all.contains(&"Copy".to_owned()),
        "text editing commands are left out"
    );

    keys("escape", cx);
    assert!(palette_closed(&view, cx));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));

    // Secondary+K toggles it closed as well.
    open_palette(&view, cx);
    keys("secondary-k", cx);
    assert!(palette_closed(&view, cx));
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

fn palette_closed(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    palette(view, cx).is_none()
}

#[gpui::test]
fn typing_filters_and_enter_runs_the_command_where_focus_was(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let first = harness.threads[0];
    let palette = open_palette(&view, cx);

    type_text("archive", cx);
    assert_eq!(
        labels(&palette, cx),
        ["Archive", "Go to Archive"],
        "the command named by the letters ranks first"
    );
    let secondary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let shortcut = cx.read_entity(&palette, |palette, _| palette.shortcut_for("Archive"));
    assert_eq!(
        shortcut.as_deref(),
        Some(format!("{secondary}-e").as_str()),
        "each command shows its shortcut from the keymap"
    );

    keys("enter", cx);
    assert!(palette_closed(&view, cx));
    assert_eq!(
        mailbox_of(&harness, first),
        harness.archive_id,
        "Archive ran on the selected conversation"
    );
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
}

#[gpui::test]
fn arrows_move_the_highlight_and_a_folder_opens_in_the_list(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let palette = open_palette(&view, cx);
    type_text("archive", cx);
    assert_eq!(highlighted(&palette, cx), "Archive");
    keys("down", cx);
    assert_eq!(highlighted(&palette, cx), "Go to Archive");
    keys("down", cx);
    assert_eq!(highlighted(&palette, cx), "Archive", "Down wraps around");
    keys("up", cx);
    assert_eq!(highlighted(&palette, cx), "Go to Archive", "Up wraps back");

    keys("enter", cx);
    assert!(palette_closed(&view, cx));
    let selection = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(selection, harness.archive_id);
    assert_eq!(focused_pane(&view, cx), Some(Pane::MessageList));
    // Nothing was archived on the way.
    assert_eq!(mailbox_of(&harness, harness.threads[0]), harness.inbox_id);
}

#[gpui::test]
fn recently_used_entries_come_first(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    open_palette(&view, cx);
    type_text("go trash", cx);
    keys("enter", cx);
    let selection = cx.read_entity(&view, |view, _| view.selected_mailbox_id);
    assert_eq!(selection, harness.trash_id);

    let palette = open_palette(&view, cx);
    assert_eq!(
        highlighted(&palette, cx),
        "Go to Trash",
        "the last pick comes first when nothing is typed"
    );
    // And it outranks an equally good match once something is typed.
    type_text("go t", cx);
    assert_eq!(highlighted(&palette, cx), "Go to Trash");
    keys("escape", cx);

    let saved = harness
        .store
        .ui_preferences()
        .expect("the preferences load")
        .recent_commands;
    assert_eq!(
        saved,
        [format!("mailbox:{}", harness.trash_id.expect("a Trash"))]
    );
}

#[gpui::test]
fn folders_and_accounts_across_accounts_name_the_account(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Accounts);
    let palette = open_palette(&view, cx);
    type_text("inbox", cx);
    let found = labels(&palette, cx);
    for expected in [
        "Go to Unified Inbox",
        "Go to Inbox (Work)",
        "Go to Inbox (Home)",
    ] {
        assert!(
            found.contains(&expected.to_owned()),
            "{expected} in {found:?}"
        );
    }

    // An account opens its Inbox.
    keys("escape", cx);
    let palette = open_palette(&view, cx);
    type_text("home", cx);
    assert_eq!(highlighted(&palette, cx), "Home");
    keys("enter", cx);
    let selection = cx.read_entity(&view, |view, _| view.mailbox_selection());
    assert_eq!(
        selection,
        Some(MailboxSelection::Mailbox {
            account_id: harness.home_account_id.expect("a Home account"),
            mailbox_id: harness.home_inbox_id.expect("a Home inbox"),
        })
    );

    // A virtual mailbox opens too.
    open_palette(&view, cx);
    type_text("flagged", cx);
    keys("enter", cx);
    let selection = cx.read_entity(&view, |view, _| view.mailbox_selection());
    assert_eq!(
        selection,
        Some(MailboxSelection::Virtual(VirtualMailbox::Flagged))
    );
}

#[gpui::test]
fn nothing_matching_leaves_enter_harmless(cx: &mut TestAppContext) {
    let (view, cx, harness) = open_workspace(cx, Setup::Mail);
    let palette = open_palette(&view, cx);
    type_text("zzzz", cx);
    assert!(labels(&palette, cx).is_empty());
    keys("enter down up", cx);
    assert!(
        !palette_closed(&view, cx),
        "Enter with no match does nothing"
    );
    keys("escape", cx);
    assert_eq!(mailbox_of(&harness, harness.threads[0]), harness.inbox_id);
}

#[gpui::test]
fn the_window_can_open_with_the_palette_showing(cx: &mut TestAppContext) {
    let (view, cx, _harness) =
        open_workspace_on(cx, Setup::Mail, |_| InitialView::CommandPalette("archive"));
    let palette = palette(&view, cx).expect("the palette is open");
    assert_eq!(labels(&palette, cx), ["Archive", "Go to Archive"]);
    assert!(
        cx.debug_bounds("command-palette-0").is_some(),
        "its rows are drawn"
    );
}
