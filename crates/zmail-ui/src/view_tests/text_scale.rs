//! Text size (UI plan Phase 8): Settings → Appearance → Text size sets
//! every window's rem size, and the layout makes room for the larger text.

use gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, px, size};
use zmail_core::preferences::UiPreferences;

use super::settings::settings_window;
use super::{Setup, open_workspace, open_workspace_with};
use crate::InitialView;
use crate::components::Select;
use crate::settings::{self, SettingsSection};
use crate::styles::DensityTokens;

fn rem_size(cx: &mut VisualTestContext) -> f32 {
    f32::from(cx.update(|window, _| window.rem_size()))
}

#[gpui::test]
fn the_text_size_applies_to_every_window_live(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace(cx, Setup::Mail);
    assert_eq!(rem_size(cx), 16.0);
    cx.update(|_, cx| settings::open(Some(SettingsSection::Appearance), cx));
    cx.run_until_parked();
    let (root, mut window_cx) = settings_window(cx);
    let text_size = root.read_with(&window_cx, |root, cx| {
        root.appearance().read(cx).text_size_select()
    });
    assert_eq!(
        text_size.read_with(&window_cx, |select, _| select.selected_index()),
        Some(0),
        "100%"
    );

    text_size.update(&mut window_cx, |select: &mut Select, cx| select.pick(4, cx));
    window_cx.run_until_parked();
    let saved: UiPreferences = harness.store.ui_preferences().expect("preferences");
    assert_eq!(saved.text_size(), 200);
    assert_eq!(rem_size(cx), 32.0, "the main window");
    assert_eq!(rem_size(&mut window_cx), 32.0, "the Settings window");

    text_size.update(&mut window_cx, |select: &mut Select, cx| select.pick(1, cx));
    window_cx.run_until_parked();
    assert_eq!(rem_size(cx), 20.0);
}

/// The bounds of the element with the run-time `selector`.
fn bounds(selector: String, cx: &mut VisualTestContext) -> Bounds<Pixels> {
    let selector: &'static str = Box::leak(selector.into_boxed_str());
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
}

fn contains(outer: Bounds<Pixels>, inner: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left()
        && inner.top() >= outer.top()
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

#[gpui::test]
fn rows_grow_to_hold_200_percent_text_at_1280_by_800(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Mail,
        |_| InitialView::Inbox,
        |store| {
            store
                .update_ui_preferences(|preferences| preferences.text_size_percent = 200)
                .expect("the preferences save");
        },
    );
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.run_until_parked();
    assert_eq!(rem_size(cx), 32.0);

    let tokens = DensityTokens::COMFORTABLE.scaled(2.0);
    let row = bounds("thread-row-0".to_owned(), cx);
    assert_eq!(f32::from(row.size.height), tokens.message_row_height);
    let thread = harness.threads[0];
    for part in ["avatar", "date"] {
        let inner = bounds(format!("thread-row-{thread}-{part}"), cx);
        assert!(contains(row, inner), "the {part} fits in its row");
    }
    let drafts = bounds("sidebar-drafts".to_owned(), cx);
    assert_eq!(f32::from(drafts.size.height), tokens.sidebar_row_height);
    assert!(
        row.right() <= px(1280.),
        "the list stays in the window: {row:?}"
    );
}
