//! Reduced motion (UI plan Phase 8): the Motion setting and the system's
//! reduce-motion setting turn the UI's animations off.

use gpui::{TestAppContext, VisualTestContext};
use zmail_core::{
    SyncPhase,
    preferences::{MotionMode, UiPreferences},
};
use zmail_store::LocalStore;

use super::settings::settings_window;
use super::{Setup, open_workspace, open_workspace_with};
use crate::InitialView;
use crate::components::Select;
use crate::motion::SystemMotion;
use crate::settings::{self, SettingsSection};

fn reduce_motion(cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| cx.reduce_motion())
}

/// Delivers the next frame and returns how many animation frames the
/// window had asked for.
fn next_frame(cx: &mut VisualTestContext) -> usize {
    let requested = cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
    requested
}

/// Opens an account in its first sync, so the list shows its pulsing
/// skeleton rows, with `motion` saved.
fn first_sync_with(
    motion: MotionMode,
    cx: &mut TestAppContext,
) -> (&mut VisualTestContext, super::Harness) {
    let (_view, cx, harness) = open_workspace_with(
        cx,
        Setup::Account,
        |_| InitialView::Inbox,
        move |store: &LocalStore| {
            let account = store.list_accounts().expect("accounts")[0].id;
            store
                .begin_sync_attempt(account, SyncPhase::Syncing, "first sync")
                .expect("the phase is saved");
            store
                .update_ui_preferences(|preferences: &mut UiPreferences| {
                    preferences.motion = motion;
                })
                .expect("the preferences save");
        },
    );
    assert!(cx.debug_bounds("list-skeleton").is_some());
    (cx, harness)
}

#[gpui::test]
fn the_loading_pulse_animates_with_full_motion(cx: &mut TestAppContext) {
    let (cx, _harness) = first_sync_with(MotionMode::Full, cx);
    assert!(!reduce_motion(cx));
    for _ in 0..3 {
        assert!(next_frame(cx) > 0, "the skeleton asks for every frame");
    }
}

#[gpui::test]
fn reduced_motion_draws_the_loading_rows_still(cx: &mut TestAppContext) {
    let (cx, _harness) = first_sync_with(MotionMode::Reduce, cx);
    assert!(reduce_motion(cx));
    // Whatever the first frame queued, nothing keeps asking for frames.
    next_frame(cx);
    assert_eq!(next_frame(cx), 0);
    assert_eq!(next_frame(cx), 0);
    assert!(cx.debug_bounds("list-skeleton").is_some(), "still shown");
}

#[gpui::test]
fn the_motion_setting_applies_live_and_system_follows_the_platform(cx: &mut TestAppContext) {
    let (_view, cx, harness) = open_workspace(cx, Setup::Mail);
    assert!(!reduce_motion(cx), "unknown system setting: full motion");
    cx.update(|_, cx| settings::open(Some(SettingsSection::Appearance), cx));
    cx.run_until_parked();
    let (root, mut window_cx) = settings_window(cx);
    let motion = root.read_with(&window_cx, |root, cx| {
        root.appearance().read(cx).motion_select()
    });
    let pick = |index: usize, window_cx: &mut VisualTestContext| {
        motion.update(window_cx, |select: &mut Select, cx| select.pick(index, cx));
        window_cx.run_until_parked();
    };
    let saved = || {
        harness
            .store
            .ui_preferences()
            .expect("the preferences load")
            .motion
    };

    pick(1, &mut window_cx);
    assert_eq!(saved(), MotionMode::Reduce);
    assert!(reduce_motion(cx));

    pick(2, &mut window_cx);
    assert_eq!(saved(), MotionMode::Full);
    assert!(!reduce_motion(cx));

    // Full ignores a system that asks for less motion; Match System
    // follows it.
    cx.update(|_, cx| cx.set_global(SystemMotion(Some(true))));
    cx.update(|_, cx| crate::settings::apply_to_workspace(cx));
    assert!(!reduce_motion(cx));
    pick(0, &mut window_cx);
    assert_eq!(saved(), MotionMode::System);
    assert!(reduce_motion(cx));
}
