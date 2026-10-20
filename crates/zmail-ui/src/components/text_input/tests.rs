//! View tests for [`TextInput`], driven through GPUI's test platform:
//! keystrokes go through the real key bindings and the input handler that
//! the element registers while focused.

use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui::{
    AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, Modifiers,
    MouseButton, ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window, div,
    point,
};

use super::{TextInput, TextInputEvent, init};

/// The platform's `secondary` modifier as a keystroke prefix.
const SECONDARY: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

fn secondary(key: &str) -> String {
    format!("{SECONDARY}-{key}")
}

/// Opens a window whose root is a focused input built by `configure`.
fn focused_input(
    cx: &mut TestAppContext,
    configure: impl FnOnce(TextInput) -> TextInput,
) -> (Entity<TextInput>, &mut VisualTestContext) {
    cx.update(init);
    let (input, cx) = cx.add_window_view(|_, cx| configure(TextInput::new(cx)));
    focus(&input, cx);
    (input, cx)
}

fn focus(input: &Entity<TextInput>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let handle = input.read(cx).focus_handle.clone();
        window.focus(&handle, cx);
    });
    cx.run_until_parked();
}

fn text(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> String {
    cx.read_entity(input, |input, _| input.text().to_owned())
}

fn cursor(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> usize {
    cx.read_entity(input, |input, _| input.cursor_offset())
}

fn selection(input: &Entity<TextInput>, cx: &mut VisualTestContext) -> std::ops::Range<usize> {
    cx.read_entity(input, |input, _| input.selected_range())
}

fn record_events(
    input: &Entity<TextInput>,
    cx: &mut VisualTestContext,
) -> Rc<RefCell<Vec<TextInputEvent>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(input, move |_, event: &TextInputEvent, _| {
            sink.borrow_mut().push(*event);
        })
        .detach();
    });
    events
}

#[gpui::test]
fn typing_and_backspace_next_to_non_ascii_never_panics(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);

    cx.simulate_input("café");
    assert_eq!(text(&input, cx), "café");
    cx.simulate_keystrokes("backspace");
    assert_eq!(text(&input, cx), "caf");

    // Arrow keys and Delete step over whole multi-byte characters.
    cx.simulate_input("é日本");
    cx.simulate_keystrokes("left left");
    assert_eq!(cursor(&input, cx), "café".len());
    cx.simulate_keystrokes("delete");
    assert_eq!(text(&input, cx), "café本");
    cx.simulate_keystrokes("right right right home");
    assert_eq!(cursor(&input, cx), 0);
    cx.simulate_keystrokes("end backspace backspace");
    assert_eq!(text(&input, cx), "caf");
}

#[gpui::test]
fn backspace_and_arrows_treat_graphemes_as_one_character(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);

    // A combining accent, a skin-toned emoji and a ZWJ family each form
    // one grapheme made of several code points.
    let combining = "e\u{301}";
    let thumbs = "\u{1F44D}\u{1F3FD}";
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    cx.simulate_input(&format!("a{combining}b{thumbs}c{family}"));
    let full = format!("a{combining}b{thumbs}c{family}");
    assert_eq!(text(&input, cx), full);

    cx.simulate_keystrokes("backspace");
    assert_eq!(text(&input, cx), format!("a{combining}b{thumbs}c"));
    cx.simulate_keystrokes("left backspace");
    assert_eq!(text(&input, cx), format!("a{combining}bc"));
    cx.simulate_keystrokes("left backspace");
    assert_eq!(text(&input, cx), "abc");

    cx.simulate_input(combining);
    cx.simulate_keystrokes("left");
    assert_eq!(
        cursor(&input, cx),
        "a".len(),
        "left skips the whole cluster"
    );
    cx.simulate_keystrokes("shift-right");
    assert_eq!(selection(&input, cx), 1..1 + combining.len());
}

#[gpui::test]
fn secondary_v_pastes_and_single_line_flattens_newlines(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    cx.write_to_clipboard(ClipboardItem::new_string("pasted\ntext".to_owned()));

    // Ctrl+V on Linux and Windows, Cmd+V on macOS.
    cx.simulate_keystrokes(&secondary("v"));
    assert_eq!(text(&input, cx), "pasted text");
}

#[cfg(not(target_os = "macos"))]
#[gpui::test]
fn ctrl_v_pastes_on_linux_and_windows(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    cx.write_to_clipboard(ClipboardItem::new_string("ctrl".to_owned()));
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(text(&input, cx), "ctrl");
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn cmd_v_pastes_on_macos(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    cx.write_to_clipboard(ClipboardItem::new_string("cmd".to_owned()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(text(&input, cx), "cmd");
}

#[gpui::test]
fn shift_selection_copy_cut_and_select_all(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    cx.simulate_input("hello world");
    cx.simulate_keystrokes("shift-left shift-left shift-left shift-left shift-left");
    assert_eq!(selection(&input, cx), 6..11);

    cx.simulate_keystrokes(&secondary("c"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("world".to_owned())
    );
    assert_eq!(text(&input, cx), "hello world");

    cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    cx.simulate_keystrokes(&secondary("x"));
    assert_eq!(text(&input, cx), "hello ");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("world".to_owned())
    );

    cx.simulate_keystrokes(&secondary("a"));
    assert_eq!(selection(&input, cx), 0..6);
    cx.simulate_input("x");
    assert_eq!(text(&input, cx), "x");
}

#[gpui::test]
fn word_movement_and_word_delete(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    let word = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    cx.simulate_input("one two, three");
    cx.simulate_keystrokes(&format!("{word}-left"));
    assert_eq!(cursor(&input, cx), "one two, ".len());
    cx.simulate_keystrokes(&format!("{word}-left"));
    assert_eq!(cursor(&input, cx), "one ".len());
    cx.simulate_keystrokes(&format!("{word}-right"));
    assert_eq!(cursor(&input, cx), "one two".len());
    cx.simulate_keystrokes(&format!("{word}-backspace"));
    assert_eq!(text(&input, cx), "one , three");
}

#[gpui::test]
fn undo_and_redo_group_typing_by_word(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    let undo = secondary("z");
    let redo = secondary("shift-z");

    cx.simulate_input("hello world");
    cx.simulate_keystrokes(&undo);
    assert_eq!(text(&input, cx), "hello ");
    cx.simulate_keystrokes(&undo);
    assert_eq!(text(&input, cx), "");
    cx.simulate_keystrokes(&redo);
    assert_eq!(text(&input, cx), "hello ");
    cx.simulate_keystrokes(&redo);
    assert_eq!(text(&input, cx), "hello world");
    assert_eq!(cursor(&input, cx), "hello world".len());

    // Consecutive backspaces undo together, and restore the cursor.
    cx.simulate_keystrokes("backspace backspace backspace");
    assert_eq!(text(&input, cx), "hello wo");
    cx.simulate_keystrokes(&undo);
    assert_eq!(text(&input, cx), "hello world");

    // A pause closes the group.
    cx.simulate_input("!");
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.simulate_input("?");
    cx.simulate_keystrokes(&undo);
    assert_eq!(text(&input, cx), "hello world!");

    // Cut is its own step and undo brings back the selection.
    cx.simulate_keystrokes(&secondary("a"));
    cx.simulate_keystrokes(&secondary("x"));
    assert_eq!(text(&input, cx), "");
    cx.simulate_keystrokes(&undo);
    assert_eq!(text(&input, cx), "hello world!");
    assert_eq!(selection(&input, cx), 0.."hello world!".len());

    // New edits clear the redo stack.
    cx.simulate_keystrokes(&undo);
    cx.simulate_input("x");
    cx.simulate_keystrokes(&redo);
    assert!(text(&input, cx).ends_with('x'));
}

#[gpui::test]
fn ime_composition_marks_then_commits(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    let events = record_events(&input, cx);
    cx.simulate_input("a");

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                input,
                None,
                "k",
                Some(1..1),
                window,
                cx,
            );
        })
    });
    assert_eq!(text(&input, cx), "ak");
    assert_eq!(
        cx.read_entity(&input, |input, _| input.marked_range.clone()),
        Some(1..2)
    );

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                input,
                None,
                "\u{304B}",
                Some(1..1),
                window,
                cx,
            );
        })
    });
    assert_eq!(text(&input, cx), "a\u{304B}");

    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            gpui::EntityInputHandler::replace_text_in_range(input, None, "\u{6F22}", window, cx);
        })
    });
    assert_eq!(text(&input, cx), "a\u{6F22}");
    assert_eq!(cursor(&input, cx), "a\u{6F22}".len());
    assert_eq!(
        cx.read_entity(&input, |input, _| input.marked_range.clone()),
        None
    );
    assert!(
        events
            .borrow()
            .iter()
            .all(|event| *event == TextInputEvent::Changed)
    );

    // The whole composition undoes as one step.
    cx.simulate_keystrokes(&secondary("z"));
    assert_eq!(text(&input, cx), "a");

    // The UTF-16 view the platform IME sees stays consistent.
    cx.simulate_input("\u{1F600}b");
    let utf16_len = cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            gpui::EntityInputHandler::text_length_utf16(input, window, cx)
        })
    });
    assert_eq!(utf16_len, Some(4));
}

#[gpui::test]
fn enter_submits_single_line_inputs(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    let events = record_events(&input, cx);
    cx.simulate_input("query");
    cx.simulate_keystrokes("enter");
    assert_eq!(text(&input, cx), "query");
    assert_eq!(events.borrow().last(), Some(&TextInputEvent::Submitted));
}

#[gpui::test]
fn multiline_enter_inserts_newlines_and_up_down_move_by_line(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input.multiline());
    let events = record_events(&input, cx);

    cx.simulate_input("first line");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("second");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("third line long");
    assert_eq!(text(&input, cx), "first line\nsecond\nthird line long");
    assert!(!events.borrow().contains(&TextInputEvent::Submitted));

    let second_end = "first line\nsecond".len();
    cx.simulate_keystrokes("up");
    assert_eq!(
        cursor(&input, cx),
        second_end,
        "the column clamps to a short line"
    );
    cx.simulate_keystrokes("up");
    assert_eq!(
        cursor(&input, cx),
        "first line".len(),
        "the goal column is kept"
    );
    cx.simulate_keystrokes("down down");
    assert_eq!(
        cursor(&input, cx),
        "first line\nsecond\nthird line long".len()
    );
    cx.simulate_keystrokes("home");
    assert_eq!(cursor(&input, cx), "first line\nsecond\n".len());
    cx.simulate_keystrokes("shift-up");
    assert_eq!(
        selection(&input, cx),
        "first line\n".len().."first line\nsecond\n".len()
    );
    cx.simulate_keystrokes("down down down");
    assert_eq!(
        cursor(&input, cx),
        text(&input, cx).len(),
        "down on the last line goes to the end"
    );
}

#[gpui::test]
fn mouse_click_and_drag_select(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    cx.simulate_input("hello world");

    let position_of = |input: &Entity<TextInput>, offset: usize, cx: &mut VisualTestContext| {
        cx.read_entity(input, |input, _| {
            let layout = input.layout.clone().expect("the input has been painted");
            let line_height = layout.line_height;
            layout.text_origin
                + layout.position_for_offset(offset)
                + point(gpui::px(0.5), line_height / 2.)
        })
    };

    let start = position_of(&input, 2, cx);
    cx.simulate_click(start, Modifiers::none());
    assert_eq!(cursor(&input, cx), 2);

    let from = position_of(&input, 1, cx);
    let to = position_of(&input, 4, cx);
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    assert_eq!(selection(&input, cx), 1..4);
}

#[gpui::test]
fn masked_inputs_hide_text_and_refuse_copy(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input.masked());
    cx.simulate_input("s3cr\u{e9}t");
    assert_eq!(
        cx.read_entity(&input, |input, _| input.display_text()),
        "\u{2022}".repeat(6)
    );
    cx.write_to_clipboard(ClipboardItem::new_string("unchanged".to_owned()));
    cx.simulate_keystrokes(&secondary("a"));
    cx.simulate_keystrokes(&secondary("c"));
    cx.simulate_keystrokes(&secondary("x"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("unchanged".to_owned())
    );
    assert_eq!(text(&input, cx), "s3cr\u{e9}t");
    cx.simulate_keystrokes("end backspace");
    assert_eq!(text(&input, cx), "s3cr\u{e9}");
}

#[gpui::test]
fn disabled_inputs_ignore_typing(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input.with_text("fixed"));
    cx.update(|window, cx| input.update(cx, |input, cx| input.set_disabled(true, window, cx)));
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.simulate_keystrokes("backspace");
    assert_eq!(text(&input, cx), "fixed");
    assert!(cx.read_entity(&input, |input, _| input.is_disabled()));
}

#[gpui::test]
fn set_text_does_not_emit_changed(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input);
    let events = record_events(&input, cx);
    cx.update(|_, cx| input.update(cx, |input, cx| input.set_text("preset", cx)));
    assert_eq!(text(&input, cx), "preset");
    assert!(events.borrow().is_empty());
    cx.simulate_input("!");
    assert_eq!(*events.borrow(), vec![TextInputEvent::Changed]);
}

/// Two inputs side by side, for focus tests.
struct TwoInputs {
    first: Entity<TextInput>,
    second: Entity<TextInput>,
    focus_handle: FocusHandle,
}

impl Render for TwoInputs {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.first.clone())
            .child(self.second.clone())
    }
}

impl Focusable for TwoInputs {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[gpui::test]
fn keys_only_reach_the_focused_input_and_tab_moves_focus(cx: &mut TestAppContext) {
    cx.update(init);
    let (view, cx) = cx.add_window_view(|_, cx| TwoInputs {
        first: cx.new(TextInput::new),
        second: cx.new(TextInput::new),
        focus_handle: cx.focus_handle(),
    });
    let (first, second) =
        cx.read_entity(&view, |view, _| (view.first.clone(), view.second.clone()));

    focus(&first, cx);
    cx.simulate_input("one");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("two");
    assert_eq!(text(&first, cx), "one");
    assert_eq!(text(&second, cx), "two");

    cx.simulate_keystrokes("shift-tab");
    cx.simulate_input("!");
    assert_eq!(text(&first, cx), "one!");
    assert_eq!(text(&second, cx), "two");
}

#[gpui::test]
fn clear_is_an_undoable_change(cx: &mut TestAppContext) {
    let (input, cx) = focused_input(cx, |input| input.clearable("Clear"));
    cx.simulate_input("query");
    let events = record_events(&input, cx);
    cx.update(|_, cx| input.update(cx, |input, cx| input.clear(cx)));
    assert_eq!(text(&input, cx), "");
    assert_eq!(*events.borrow(), vec![TextInputEvent::Changed]);
    cx.simulate_keystrokes(&secondary("z"));
    assert_eq!(text(&input, cx), "query");
}
