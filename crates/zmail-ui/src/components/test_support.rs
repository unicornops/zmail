//! Helpers shared by the component tests.

use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, TestAppContext, VisualTestContext};

/// Bind the component keymap, as `run()` does at startup.
pub(crate) fn init(cx: &mut TestAppContext) {
    cx.update(super::actions::init);
}

/// Press and release each space-separated keystroke. Unlike
/// `simulate_keystrokes`, this also sends the key-up events that GPUI
/// turns into keyboard clicks on Enter and Space.
pub(crate) fn press(cx: &mut VisualTestContext, keystrokes: &str) {
    for keystroke in keystrokes.split(' ') {
        let keystroke = Keystroke::parse(keystroke).expect("valid keystroke");
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }
}
