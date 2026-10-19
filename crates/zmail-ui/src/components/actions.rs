//! Keyboard actions shared by the components, and their default bindings.
//!
//! The bindings are scoped to each component's key context ("Menu",
//! "Select", "Modal"), so they only apply while that component has focus
//! and never shadow the workspace keymap.

use gpui::{App, KeyBinding, actions};

actions!(
    zmail_components,
    [
        /// Move the highlight to the next item.
        SelectNext,
        /// Move the highlight to the previous item.
        SelectPrevious,
        /// Move the highlight to the first item.
        SelectFirst,
        /// Move the highlight to the last item.
        SelectLast,
        /// Activate the highlighted item.
        Confirm,
        /// Close the focused overlay without acting.
        Cancel,
        /// Move focus to the next control inside a modal.
        FocusNext,
        /// Move focus to the previous control inside a modal.
        FocusPrevious,
    ]
);

/// Key context of a focused [`Menu`](super::Menu).
pub(crate) const MENU_CONTEXT: &str = "Menu";
/// Key context of a focused [`Select`](super::Select) trigger.
pub(crate) const SELECT_CONTEXT: &str = "Select";
/// Key context of an open [`Modal`](super::Modal).
pub(crate) const MODAL_CONTEXT: &str = "Modal";

/// Register the components' default key bindings on their own (the
/// gallery and component tests). The app binds them with the rest through
/// [`crate::keymap::bind_keymap`].
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
}

/// The components' default key bindings, each scoped to its component.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let menu = Some(MENU_CONTEXT);
    let select = Some(SELECT_CONTEXT);
    let modal = Some(MODAL_CONTEXT);
    vec![
        KeyBinding::new("down", SelectNext, menu),
        KeyBinding::new("up", SelectPrevious, menu),
        KeyBinding::new("home", SelectFirst, menu),
        KeyBinding::new("pageup", SelectFirst, menu),
        KeyBinding::new("end", SelectLast, menu),
        KeyBinding::new("pagedown", SelectLast, menu),
        KeyBinding::new("enter", Confirm, menu),
        KeyBinding::new("space", Confirm, menu),
        KeyBinding::new("escape", Cancel, menu),
        // Tab leaves a menu the way Escape does, so focus never gets lost
        // behind it.
        KeyBinding::new("tab", Cancel, menu),
        KeyBinding::new("down", SelectNext, select),
        KeyBinding::new("up", SelectPrevious, select),
        KeyBinding::new("alt-down", SelectNext, select),
        KeyBinding::new("escape", Cancel, modal),
        KeyBinding::new("tab", FocusNext, modal),
        KeyBinding::new("shift-tab", FocusPrevious, modal),
    ]
}
