//! Pointer target size (UI plan Phase 8, WCAG 2.2 2.5.8): every target a
//! frame draws is at least 24×24 at the default text size.
//!
//! Targets mark themselves with `HitTargetExt::hit_target`, which records
//! them in tests, so these tests walk whatever each view drew instead of a
//! hand-kept list.

use gpui::{Modifiers, TestAppContext, VisualTestContext, px};

use super::settings::settings_window;
use super::{Setup, compose_window, open_workspace};
use crate::components::MIN_TARGET_SIZE;
use crate::components::hit_target::recorded;
use crate::settings::{self, SettingsSection};

/// Targets that are smaller on purpose, with why. These are documented
/// exceptions with keyboard and wheel alternatives, not passes (see
/// `components::hit_target`).
const EXCEPTIONS: [(&str, &str); 1] = [(
    "split-handle-",
    "6px pane divider; arrow keys, Home/End and a double click resize it",
)];

/// Redraws the window and checks every target it drew. Returns how many
/// were checked.
fn check_targets(what: &str, cx: &mut VisualTestContext) -> usize {
    recorded::take();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let mut checked = 0;
    let mut too_small = Vec::new();
    for selector in recorded::take() {
        if EXCEPTIONS
            .iter()
            .any(|(prefix, _)| selector.starts_with(prefix))
        {
            continue;
        }
        let selector: &'static str = Box::leak(selector.into_boxed_str());
        // Recorded but not laid out (a list row outside the viewport).
        let Some(bounds) = cx.debug_bounds(selector) else {
            continue;
        };
        checked += 1;
        if bounds.size.width < px(MIN_TARGET_SIZE) || bounds.size.height < px(MIN_TARGET_SIZE) {
            too_small.push(format!(
                "{selector}: {}×{}",
                f32::from(bounds.size.width),
                f32::from(bounds.size.height)
            ));
        }
    }
    assert!(
        too_small.is_empty(),
        "{what}: targets under {MIN_TARGET_SIZE}px:\n{}",
        too_small.join("\n")
    );
    checked
}

#[gpui::test]
fn the_main_window_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Accounts);
    // The open thread in the reader, and a hovered row's quick actions.
    super::click("thread-row-0", cx);
    let row = cx.debug_bounds("thread-row-1").expect("a second row");
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    cx.run_until_parked();
    let checked = check_targets("main window", cx);
    assert!(checked >= 30, "only {checked} targets were drawn");
}

#[gpui::test]
fn the_onboarding_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::NoAccount);
    let checked = check_targets("onboarding", cx);
    assert!(checked >= 2, "only {checked} targets");
}

#[gpui::test]
fn the_compose_window_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (view, cx, _harness) = open_workspace(cx, Setup::Mail);
    cx.update(|window, cx| view.update(cx, |view, cx| view.open_compose(window, cx)));
    cx.run_until_parked();
    let (_compose, mut window_cx) = compose_window(&view, cx);
    window_cx.update(|window, _| window.activate_window());
    // A recipient chip, with its remove button.
    window_cx.simulate_input("alice@example.com,");
    assert!(window_cx.debug_bounds("recipient-To-0-remove").is_some());
    assert!(check_targets("compose window", &mut window_cx) >= 5);
}

#[gpui::test]
fn the_settings_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Accounts);
    cx.update(|_, cx| settings::open(Some(SettingsSection::Accounts), cx));
    cx.run_until_parked();
    let (_root, mut window_cx) = settings_window(cx);
    for section in SettingsSection::ALL {
        cx.update(|_, cx| settings::open(Some(section), cx));
        cx.run_until_parked();
        let checked = check_targets(section.label(), &mut window_cx);
        assert!(checked >= 7, "{}: only {checked} targets", section.label());
    }
}

#[gpui::test]
fn the_command_palette_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (_view, cx, _harness) = open_workspace(cx, Setup::Mail);
    super::keys("secondary-k", cx);
    assert!(check_targets("command palette", cx) >= 10);
}
